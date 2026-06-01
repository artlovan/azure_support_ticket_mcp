# Architecture

Technical reference for `azure-support-ticket-mcp`. This describes how the
system is built, the boundaries between layers, and the contracts each layer
exposes. For *what* and *why* (product scope, goals, non-goals) see
[`CONTRIBUTING.md`](./CONTRIBUTING.md) → Scope. For the slice rollout history
and what's planned next see [`ROADMAP.md`](./ROADMAP.md).

---

## 1. High-level architecture

```text
MCP client (Copilot CLI, Claude, Cursor, ...)
  |
  | JSON-RPC over stdio (rmcp)
  v
azure-support-ticket-mcp (Rust)
  |
  +-- MCP Tool Layer (rmcp adapter)
  |     +-- workflow-oriented tools
  |     +-- typed schemas (schemars 0.8)
  |     +-- side-effect guardrails (review token + confirmed:true)
  |
  +-- Workflow Layer
  |     +-- ticket flow orchestration (create / read / update / reply / attach)
  |     +-- draft builder + validator
  |     +-- pluggable DraftStore (memory default, SQLite opt-in)
  |     +-- review token issuance + draft_hash verification
  |     +-- sanitize tokens + secret tripwire (Slice 5)
  |     +-- share message formatter
  |
  +-- Resolver Layer
  |     +-- tenant/subscription scoping
  |     +-- resource ID / portal URL / error / NL extractors
  |     +-- deterministic recognizers (ARM error envelope, http_status, etc.)
  |     +-- provider-type to support-service ranking
  |     +-- problem classification ranking (per service)
  |
  +-- Azure Client Layer (typed REST)
  |     +-- auth providers (env chain, az CLI fallback)
  |     +-- tenants / subscriptions
  |     +-- Azure Resource Graph (resource search)
  |     +-- ARM (exact resource ID validation)
  |     +-- Microsoft.Support (services, classifications,
  |         supportTickets CRUD, communications, file workspaces)
  |
  +-- Cache Layer (SQLite)
  |     +-- catalog + identity + classifications + inventory
  |     +-- TTL policies, ETags
  |     +-- stale-while-revalidate, stale-if-error
  |     +-- single-flight refresh locks
  |
  +-- Bootstrap / Init Layer
        +-- ensure_initialized() (idempotent, runs on serve)
        +-- config (TOML + env overrides)
        +-- seed data load (embedded fallback + versioned GitHub Release)
        +-- doctor command (lightweight environment checks)
```

Keep these boundaries intact. Do not mix MCP transport, Azure HTTP calls,
cache persistence, inference logic, draft storage, and ticket workflow
decisions in the same module.

---

## 2. Naming and filesystem layout

- Crate, binary, repo, and config namespace: **`azure-support-ticket-mcp`** (kebab-case).
- Rust module path: `azure_support_ticket_mcp`.

User-scoped paths (chosen explicitly over XDG):

```text
~/.azure-support-ticket-mcp/
  config.toml         # TOML config, overridable by env vars
  cache.sqlite        # local cache (catalog, identity, inventory)
  drafts.sqlite       # optional draft store (only if drafts.store = "sqlite")
  logs/               # rotated tracing logs (opt-in)
  seed/               # downloaded seed asset (versioned)
```

Repository layout:

```text
azure-support-ticket-mcp/
  Cargo.toml
  README.md
  CONTRIBUTING.md
  LICENSE
  docs/
    ARCHITECTURE.md          # this file
    ROADMAP.md
    RELEASING.md             # release cycle and pipeline
  scripts/
    install.sh               # POSIX install script (published as release asset)
    install.ps1              # PowerShell install script (published as release asset)
  plugin/
    plugin.json              # Copilot CLI plugin manifest
    .mcp.json                # MCP launcher config (spawns the binary)
    README.md                # what this directory is, how it's installed
  .github/
    copilot-instructions.md
    workflows/
      ci.yml                 # tests + lint on push/PR
      release.yml            # build + publish on `v*` tag push
  data/
    support_services_seed.json   # normalized, version-pinned seed (embedded at build)
    seed_schema.md
  src/
    main.rs
    lib.rs
    error.rs
    config.rs
    bootstrap/
      mod.rs
      init.rs                    # ensure_initialized(): idempotent
      seed.rs                    # embedded + GitHub Release loader
      doctor.rs                  # `doctor` subcommand
    mcp/
      mod.rs
      server.rs                  # rmcp stdio adapter, tool registration
      tools/                     # one file per tool
    azure/
      mod.rs
      auth.rs                    # AuthProvider trait + env / az CLI impls
      client.rs                  # typed REST client (reqwest)
      tenants.rs
      subscriptions.rs
      resource_graph.rs          # ARG search
      arm.rs                     # exact resource ID validation
      support/
        mod.rs
        services.rs
        classifications.rs
        tickets.rs               # CRUD + PATCH
        communications.rs        # list + reply
        file_workspaces.rs       # create + chunked upload
    cache/
      mod.rs
      db.rs                      # SQLite (sqlx)
      models.rs
      refresh.rs                 # stale-while-revalidate + single-flight
      ttl.rs
    resolver/
      mod.rs
      context.rs
      extractors.rs              # resource ID, portal URL, error text
      recognizers.rs             # deterministic error-shape recognizers (Slice 5)
      ranking.rs
      service_map.rs             # curated provider-type hints
    workflow/
      mod.rs
      draft.rs                   # builder + validator
      store.rs                   # DraftStore trait + Memory/SQLite impls
      confirm.rs                 # review token + draft_hash guard
      sanitize_tokens.rs         # one-shot tokens for zero-friction ingest (Slice 5)
      secret_tripwire.rs         # defense-in-depth secret detection (Slice 5)
      share.rs                   # share message formatter
  tests/                         # mocked integration tests
```

### 2.1 Recommended crates

```text
rmcp               MCP server SDK (stdio)
tokio              async runtime
reqwest            HTTP client (rustls)
serde / serde_json serialization
schemars 0.8       JSON Schema for MCP tool inputs/outputs
thiserror          typed errors
tracing            structured diagnostics
sqlx               SQLite (compile-time checked queries)
uuid               ticket name + review token generation
time               timestamps and TTL math
toml               config parsing
dirs               home directory discovery
which              optional `az` detection
url                portal URL parsing
regex              resource ID / error extraction
sha2               draft_hash
base64             file workspace chunk upload
```

---

## 3. MCP tool layer

The MCP layer is a thin rmcp adapter. All tools live in `src/mcp/tools/` as
individual files and are registered in `src/mcp/server.rs` with `#[tool]`
annotations. The `#[tool]` `description` strings and the doc-comments on
input/output fields are the **canonical tool documentation** — they are what
assistants read at runtime.

To browse the full tool inventory, run any MCP client against the binary and
list its tools, or read `src/mcp/server.rs`. The high-level groupings are:

- **Discovery (read-only):** auth status, tenants, subscriptions, services,
  classifications, doctor, cache refresh.
- **Draft + create:** start flow, resolve context, build/validate/preview
  draft, create ticket, format share message.
- **Ticket CRUD + communications:** list / get / update tickets, list /
  reply / summarize communications.
- **Attachments:** prepare, add, list — all via file workspaces.
- **Zero-friction ingest:** ingest error context (two-call sanitization
  handshake) → commit sanitized context → standard draft pipeline.

### 3.1 Confirmation guard (applies to all side-effecting tools)

`build_ticket_draft` (and other draft builders) return:

```json
{
  "review_token": "rt_01J...",
  "draft_hash": "sha256:...",
  "preview": "..."
}
```

`create_support_ticket`, `update_ticket`, `reply_to_ticket`, and
`add_attachments_to_ticket` must receive:

```json
{
  "review_token": "rt_01J...",
  "draft_hash": "sha256:...",
  "confirmed": true
}
```

The server recomputes `draft_hash` from the current draft state and refuses
the call if any of: token unknown/expired, hash mismatch, or
`confirmed != true`. This prevents stale-draft submissions and accidental
mutations.

---

## 4. Authentication

Trait-based `AuthProvider`. Concrete implementations:

1. **EnvCredentialProvider** — Azure SDK-compatible chain: env vars
   (`AZURE_TENANT_ID` / `AZURE_CLIENT_ID` / `AZURE_CLIENT_SECRET` or
   federated), managed identity, workload identity.
2. **AzureCliTokenProvider** — fallback via
   `az account get-access-token --resource https://management.azure.com/`
   when `az` is on PATH and the user is signed in.

Order: env chain first, az CLI fallback second, actionable error if both fail.

Multi-tenant user discovery from start: enumerate tenants accessible to the
signed-in identity via `GET /tenants?api-version=2022-12-01`, list
subscriptions per tenant lazily.

### 4.1 `azure_auth_status` output

```json
{
  "authenticated": true,
  "source": "env_chain",
  "user": "user@contoso.com",
  "defaultTenantId": "...",
  "defaultSubscriptionId": "...",
  "tenantsAvailable": 3,
  "azCliAvailable": true,
  "message": "Authenticated via environment credentials."
}
```

---

## 5. Azure API integration

Direct REST via `reqwest` (no Azure SDK crate dependency).

Default endpoints (configurable per cloud):

```text
ARM:               https://management.azure.com
Resource Graph:    https://management.azure.com/providers/Microsoft.ResourceGraph
Microsoft Graph:   https://graph.microsoft.com (optional, profile prefill)
```

Supported clouds: `AzurePublicCloud` (MVP), `AzureUSGovernment`,
`AzureChinaCloud`, `CustomCloud` (config-only switch).

### 5.1 Endpoints used

```text
GET    /tenants?api-version=2022-12-01
GET    /subscriptions?api-version=2022-12-01
POST   /providers/Microsoft.ResourceGraph/resources?api-version=2024-04-01   (search)
GET    /{resourceId}?api-version=...                                          (exact ID validation)

GET    /providers/Microsoft.Support/services?api-version=2024-04-01
GET    /providers/Microsoft.Support/services/{sid}/problemClassifications?api-version=2024-04-01

PUT    /subscriptions/{subId}/providers/Microsoft.Support/supportTickets/{name}?api-version=2024-04-01
GET    /subscriptions/{subId}/providers/Microsoft.Support/supportTickets/{name}?api-version=2024-04-01
GET    /subscriptions/{subId}/providers/Microsoft.Support/supportTickets?api-version=2024-04-01
PATCH  /subscriptions/{subId}/providers/Microsoft.Support/supportTickets/{name}?api-version=2024-04-01

GET    /subscriptions/{subId}/providers/Microsoft.Support/supportTickets/{name}/communications?api-version=2024-04-01
PUT    /subscriptions/{subId}/providers/Microsoft.Support/supportTickets/{name}/communications/{commName}?api-version=2024-04-01

PUT    /subscriptions/{subId}/providers/Microsoft.Support/fileWorkspaces/{wsName}?api-version=2024-04-01
PUT    /subscriptions/{subId}/providers/Microsoft.Support/fileWorkspaces/{wsName}/files/{fileName}?api-version=2024-04-01
POST   /subscriptions/{subId}/providers/Microsoft.Support/fileWorkspaces/{wsName}/files/{fileName}/upload?api-version=2024-04-01
```

By convention, `wsName == supportTicketName` so post-creation
`add_attachments_to_ticket` reuses the same workspace.

### 5.2 Severity and contact rules

- Severities: `minimal`, `moderate`, `critical`, `highestcriticalimpact`
  (last is Premium-only).
- For `critical` / `highestcriticalimpact`, phone is required in
  `contactDetails`.
- Validate before submit; return actionable error if support plan/severity
  is incompatible (preserve Azure error code + request ID).

---

## 6. Local cache design

SQLite cache at `~/.azure-support-ticket-mcp/cache.sqlite`. Cache keys
scoped by `cloud`, then `account` / `tenant` / `subscription` / `service`
as relevant.

### 6.1 Tables

```sql
CREATE TABLE support_services (
  cloud TEXT NOT NULL,
  service_id TEXT NOT NULL,            -- /providers/Microsoft.Support/services/{sid}
  name TEXT NOT NULL,                  -- sid
  display_name TEXT NOT NULL,
  service_group TEXT,                  -- from seed
  resource_types_json TEXT,            -- from seed
  metadata_json TEXT,
  source TEXT NOT NULL,                -- 'seed' | 'live'
  updated_at INTEGER NOT NULL,
  etag TEXT,
  PRIMARY KEY (cloud, service_id)
);

CREATE TABLE problem_classifications (
  cloud TEXT NOT NULL,
  service_id TEXT NOT NULL,
  classification_id TEXT NOT NULL,
  display_name TEXT NOT NULL,
  parent_id TEXT,                      -- for grouping
  metadata_json TEXT,
  updated_at INTEGER NOT NULL,
  etag TEXT,
  PRIMARY KEY (cloud, service_id, classification_id)
);

CREATE TABLE tenants (
  account_id TEXT NOT NULL,
  tenant_id TEXT NOT NULL,
  display_name TEXT,
  updated_at INTEGER NOT NULL,
  PRIMARY KEY (account_id, tenant_id)
);

CREATE TABLE subscriptions (
  tenant_id TEXT NOT NULL,
  subscription_id TEXT NOT NULL,
  display_name TEXT NOT NULL,
  state TEXT,
  updated_at INTEGER NOT NULL,
  PRIMARY KEY (tenant_id, subscription_id)
);

CREATE TABLE resource_inventory (
  subscription_id TEXT NOT NULL,
  resource_id TEXT NOT NULL,
  name TEXT NOT NULL,
  type TEXT NOT NULL,
  location TEXT,
  resource_group TEXT,
  updated_at INTEGER NOT NULL,
  PRIMARY KEY (subscription_id, resource_id)
);

CREATE TABLE cache_refresh_state (
  cache_key TEXT PRIMARY KEY,
  last_attempt_at INTEGER,
  last_success_at INTEGER,
  last_error TEXT,
  refresh_in_progress INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE seed_meta (
  key TEXT PRIMARY KEY,                -- 'version', 'sha256', 'source', 'loaded_at'
  value TEXT NOT NULL
);
```

### 6.2 Refresh strategy

Stale-while-revalidate, with single-flight locks.

| Data | Scope | TTL | Behavior |
|---|---|---:|---|
| Tenants | account | 5–15 min | Refresh when auth identity changes |
| Subscriptions | tenant | 5–15 min | Refresh on tenant switch / TTL |
| Support services | cloud | 24 h | Seed wins if live unavailable; SWR otherwise |
| Problem classifications | cloud + service | 24 h – 7 d | Lazy per service |
| Resource inventory (ARG) | subscription | 2–10 min | Only when needed |
| Contact/profile defaults | user/tenant | 1–24 h | Prefill, always confirm |

Rules:

1. Never fetch all problem classifications globally.
2. Fetch classifications only after a service is selected or strongly inferred.
3. Fresh cache → return immediately.
4. Stale cache → return immediately, refresh in background.
5. Missing required cache → fetch once; if it fails, return clear actionable error.
6. Single-flight refresh per `cache_key`.
7. Stale-if-error: keep using stale data when upstream is down.
8. Use ETags when Azure provides them.
9. Short negative-cache TTLs for empty/404 results.

---

## 7. Configuration

TOML at `~/.azure-support-ticket-mcp/config.toml`, fully overridable by env
vars (`AZURE_SUPPORT_TICKET_MCP_*`).

```toml
[general]
cloud = "AzurePublicCloud"
log_level = "info"

[auth]
prefer = "env"                     # "env" | "az_cli"
allow_az_cli_fallback = true

[cache]
path = "~/.azure-support-ticket-mcp/cache.sqlite"
services_ttl_hours = 24
classifications_ttl_hours = 168

[drafts]
store = "memory"                   # "memory" | "sqlite"
sqlite_path = "~/.azure-support-ticket-mcp/drafts.sqlite"
ttl_days = 7                       # 0 = no TTL

[seed]
auto_download = true
release_url_template = "https://github.com/artlovan/azure_support_ticket_mcp/releases/download/v{version}/support_services_seed.json"
```

Env override examples: `AZURE_SUPPORT_TICKET_MCP_CACHE_PATH`,
`AZURE_SUPPORT_TICKET_MCP_DRAFTS_STORE`.

---

## 8. Bootstrap and initialization

- Subcommands: `serve` (default), `doctor`, `init` (manual seed refresh),
  `version`.
- On `serve`, `ensure_initialized()` runs idempotently:
  1. Ensure `~/.azure-support-ticket-mcp/` exists.
  2. Open or create `cache.sqlite` and run migrations.
  3. Load seed data: embedded asset → check `seed_meta.version` against
     binary version → if `seed.auto_download` and remote is newer/missing,
     download verified release asset; else fall back to embedded.
  4. Spawn background prewarm of support services list (non-blocking;
     serve never waits).
- `doctor` performs lightweight checks: cache writable, `az` presence
  (informational), token acquisition probe, network reachability to ARM.
- Permission checks are **lazy and actionable** — never gate startup on
  Azure RBAC.

---

## 9. Seed data

The seed is sourced from Azure's live `Microsoft.Support/services` REST API
and embedded into the binary at build time. See `data/README.md` for the
refresh workflow contributors run.

- Path in repo: `data/support_services_seed.json`
- Schema (normalized, full):

```json
{
  "version": "2024-04-01-2",
  "generated_at": "ISO-8601",
  "source": "Azure Microsoft.Support/services REST API",
  "services": [
    {
      "service_id": "/providers/Microsoft.Support/services/{sid}",
      "name": "{sid}",
      "display_name": "Azure Kubernetes Service",
      "group": "Compute",
      "resource_types": ["Microsoft.ContainerService/managedClusters", "..."],
      "metadata": { "...": "..." }
    }
  ]
}
```

- Embedded in the binary via `include_bytes!` (`src/bootstrap/seed.rs`).
  Build-time correctness is guaranteed by Cargo's file-dependency tracking:
  edit the JSON → next `cargo build` re-embeds the new bytes. The
  `embedded_seed_parses` test in `src/bootstrap/seed.rs` runs on every CI
  build and fails the PR if the file is malformed.

### 9.1 Update flow (build-time and runtime)

**Repo → binary (build-time, automatic):** `include_bytes!` macro at
`src/bootstrap/seed.rs:17`. The Rust compiler reads the file on every
build and bakes the exact bytes into the binary as a `const &[u8]`. No
manual step.

**Binary → user's cache (runtime, version-keyed):**
`load_into_cache_if_needed` runs in `ensure_initialized()` at every
`serve` startup. It parses the embedded bytes, reads the user's
recorded seed version from the `seed_meta` SQLite table, and:

- If `cached_version == embedded_version` → no-op, skip the reload.
- If `cached_version != embedded_version` → upsert every embedded service
  into the cache, then record the new version + sha256 + source + load
  time in `seed_meta`.

The decision key is the `version` field at the top of the JSON
(`"2024-04-01-N"`). The refresh script
(`scripts/refresh_support_services_seed.py`) bumps this suffix on every
successful refresh; that bump is what triggers the cache reload on the
upgrading user's machine.

### 9.2 Deferred design decisions

Two seed-related decisions were intentionally deferred from v1. Both
have explicit ADRs (rationale, alternatives, triggers for revisit) in
[`docs/ROADMAP.md`](./ROADMAP.md) under "Deferred design decisions":

1. **Upstream catalog refresh is manual in v1.** The
   `scripts/refresh_support_services_seed.py` tool exists, and the
   release checklist documents running it. Automation (scheduled
   GitHub Action that opens a refresh PR weekly) is planned but
   requires Azure CI auth setup that isn't justified yet.
2. **End-users can't refresh the embedded catalog without upgrading the
   binary.** The `[seed]` config section in §7 already specifies the
   `release_url_template` placeholder that a future runtime refresh
   subcommand would use. Implementation is deferred to Slice 6+.

---

## 10. Tenant and subscription UX

1. Enumerate **all** tenants accessible to the signed-in identity
   (multi-tenant from start).
2. One tenant + one subscription → use as defaults; surface the choice in
   tool output.
3. Multiple → return ranked choices; do not silently pick.
4. Trust subscription IDs found inside resource IDs or portal URLs (after
   access check).
5. Never auto-switch tenant/subscription mid-flow without surfacing the
   change.

---

## 11. Resource and service inference

`resolve_issue_context` input:

```json
{
  "tenantId": "...",
  "subscriptionId": "...",
  "userInput": "my AKS cluster prod-aks cannot scale nodes",
  "resourceId": "...",
  "resourceName": "prod-aks",
  "portalUrl": "...",
  "errorText": "QuotaExceeded ..."
}
```

Deterministic order:

1. Full resource ID → validate via ARM GET.
2. Portal URL → decode `/subscriptions/.../resourceGroups/.../providers/...`.
3. Provider/type → curated map → candidate support services.
4. Exact resource name within selected subscription → Resource Graph search.
5. Known error code patterns → mapped classification hints.
6. Keyword/fuzzy match against cached service + classification names.

Always return **ranked** candidates with `confidence` and `reason`. Prefer a
short ranked list over an open-ended question.

---

## 12. Problem classification UX

Service-scoped, grouped, ranked top-N:

1. Resolve service first.
2. Load cached classifications for that service.
3. If missing, fetch from Azure.
4. Rank with issue text + error text + resource type.
5. Return top N (default 5) plus `"showMore": true` cursor.

---

## 13. Ticket draft workflow

### 13.1 Required fields

```text
tenantId
subscriptionId
serviceId
problemClassificationId
title
description
severity
advancedDiagnosticConsent
contactDetails.firstName
contactDetails.lastName
contactDetails.country
contactDetails.preferredContactMethod
contactDetails.preferredSupportLanguage
contactDetails.preferredTimeZone
contactDetails.primaryEmailAddress
contactDetails.phoneNumber       # required when severity is critical|highestcriticalimpact
```

### 13.2 Optional fields

```text
resourceId
problemStartTime
require24x7Response
technicalTicketDetails.resourceId
quotaTicketDetails
supportPlanId
fileWorkspaceName                # auto-set by prepare_attachments
```

### 13.3 Profile prefill

Best-effort: from cached identity / `az account show` / optional Microsoft
Graph (`/me`). Always **show prefilled values for explicit confirmation**
before issuing a review token.

### 13.4 DraftStore

Trait `DraftStore { get / put / delete / list }`. Implementations:

- `MemoryDraftStore` (default) — process-lifetime, fine for single stdio
  session.
- `SqliteDraftStore` (opt-in via config) — `drafts.sqlite`, supports
  retention.

Retention:

- Default TTL 7 days.
- Delete-after-submit on successful ticket creation.
- `ttl_days = 0` disables TTL (no-TTL override).

---

## 14. Confirmation and creation

`build_ticket_draft` returns a `review_token` (UUIDv7) and `draft_hash`
(SHA-256 of canonical draft JSON). The token has a 30-minute idle expiry.

Side-effecting tools require all three: `review_token`, `draft_hash`,
`confirmed: true`. The server:

1. Looks up draft by `review_token`.
2. Recomputes `draft_hash`; rejects on mismatch.
3. Validates `confirmed === true`.
4. Validates required fields + severity/phone rules.
5. Generates ticket name (UUID-based) if not provided.
6. Submits PUT; handles `200` or `202` with operation poll.
7. On success, deletes draft (unless retention override).
8. Returns ticket details + share summary.

---

## 15. Communications

- `list_ticket_communications` paginates via `$top` / `nextLink` (max 10
  per page from API).
- `get_latest_communication` returns the most recent of type `web` or
  `phone`.
- `summarize_ticket_thread` is a **local** condenser
  (truncation/highlight); it does not call any LLM from within the MCP. The
  MCP client may summarize further.
- `reply_to_ticket` = create_communication; gated by confirmation guard.

---

## 16. Attachments

- Workspace name == ticket name (matches portal behavior, enables
  post-create uploads).
- `prepare_attachments` (pre-create): create workspace + upload files;
  returns `fileWorkspaceName`.
- `create_support_ticket` auto-sets `fileWorkspaceName` if
  `prepare_attachments` was called for this draft.
- `add_attachments_to_ticket` (post-create): uploads to the existing
  workspace.
- `list_attachments`: enumerates files in the workspace.
- Reply tool surfaces the decoupling clearly: "attachments live on the
  ticket workspace, not on this reply."
- Chunking: ≤2.5MB base64 chunks, max 2 chunks per file, ≤5MB per file.
  Surface clear errors above limits.

---

## 17. Sharing

`format_ticket_share_message` returns a copy/paste markdown block:

```text
Azure support ticket opened: 1234567890000000
Title: AKS cluster nodes fail to scale
Severity: Moderate
Subscription: Prod Platform
Resource: /subscriptions/.../managedClusters/prod-aks-eastus
Status: Open
Portal: https://portal.azure.com/...
Summary: Node pool scale-out fails with quota errors in eastus.
```

Out-of-MVP integrations (Teams/Slack/email/GitHub) — if ever added — must
require explicit recipient/channel + message preview + `confirmed: true`.

---

## 18. Zero-friction error ingestion (Slice 5)

Two-call trust-boundary handshake that turns a piped error blob into a
draft without ever persisting raw user input directly.

### 18.1 Trust-boundary model

- The MCP is the **persistence gate**. It refuses to persist user-pasted
  raw text directly.
- The LLM in the host is the **semantic sanitizer**. It knows context —
  that an ARM resource ID is safe to keep, that a connection string is
  not.
- The MCP runs a tiny **defense-in-depth tripwire** AFTER the LLM hands
  back sanitized text. Tripwire hits are unambiguous catastrophic secret
  patterns; they reject the commit with a retry hint and keep the
  `sanitize_token` valid so the assistant can try again.

### 18.2 Flow

1. User pipes content: `copilot -i "ticket this: $(cat err.log)"`.
2. Assistant calls `ingest_error_context(raw_text=<blob>)`. The MCP runs
   deterministic recognizers over the blob and returns:
   - SAFE extracted hints: ARM resource ID, subscription ID (parsed out),
     error code, correlation ID, severity hint, title hint.
   - `sanitize_token` (one-shot, 5-min TTL, bound to content hash).
   - `raw_text_echo` plus `sanitize_instructions` for the LLM.
   - Hard cap: 1 MiB per call.
3. Assistant produces sanitized text per the instructions: keep ARM IDs,
   error codes, stack traces, correlation IDs; redact connection strings,
   Bearer tokens, account keys, PEM private key blocks, passwords. Track
   each redaction as `[REDACTED:<KIND>]`.
4. Assistant calls `commit_sanitized_context(sanitize_token,
   sanitized_text, redacted_summary)`. The MCP runs the
   catastrophic-secret tripwire:
   - `AZURE_STORAGE_CONN_STR` — `DefaultEndpointsProtocol=...AccountKey=...`
   - `AZURE_ACCOUNT_KEY` — `AccountKey=` + 88-char base64
   - `PRIVATE_KEY_BLOCK` — `-----BEGIN ... PRIVATE KEY-----`
   - `BEARER_JWT` — `Bearer eyJ...`.`...`.`...`

   On tripwire match: REJECT with retry hint, token stays valid.
   On pass: create draft with recognizer fields + sanitized description
   (prepended with `Error code:` / `Correlation ID:` header lines if
   any), stash `redacted_summary` on the draft for display, issue
   `review_token` + `draft_hash`.
5. Standard pipeline takes over: `build_ticket_draft` (assistant fills
   any remaining fields with the user) → `preview_ticket_draft` →
   `create_support_ticket`. The preview surfaces the FULL sanitized
   description (no truncation) and the redaction summary so the user
   sees ALL data before confirming.

### 18.3 Recognizers

Pure functions in `src/resolver/recognizers.rs`:

| Recognizer | What it extracts |
| --- | --- |
| `arm_error_envelope` | `error.code`, `x-ms-correlation-request-id` |
| `az_deployment_failed` | `provisioningState=Failed` → moderate severity |
| `resource_id` | Full ARM ID → resource_id + subscription_id |
| `http_status` | 4xx/5xx → error_code + severity_hint |
| `kubectl_events` | Last `Warning` row → title hint |

Each runs cheaply, first-wins on duplicates. Empty matched array = nothing
found, assistant falls back to plain LLM extraction.
`extraction_blob_only=true` opt-out disables recognizers for users who
prefer raw passthrough.

### 18.4 Auto-approve

`copilot -p "..." --allow-all-tools` (non-interactive) gets you
auto-approve. The MCP's safety contract is unchanged — the
`review_token` + `draft_hash` + `confirmed:true` gate stays. Auto-approve
just means the assistant runs preview → submit back-to-back without
pausing.

---

## 19. Error handling

Structured errors preserve Azure error code, HTTP status, request ID,
operation ID, and a user-facing actionable message.

| Situation | Response |
|---|---|
| Not authenticated | `Run az login or set Azure credentials.` |
| No accessible subscriptions | `No accessible subscriptions found for this tenant.` |
| Ambiguous resource name | `Found 3 matching resources; please choose one.` |
| Missing support plan | `This subscription's support plan does not allow this severity/type.` |
| Stale catalog served | `Using cached classifications while refreshing in background.` |
| Azure unavailable | `Azure Support API unavailable; cached data still usable.` |
| Missing confirmation | `Ticket not submitted: confirmed:true and matching review_token required.` |
| Hash mismatch | `Draft changed since review; rebuild draft and reconfirm.` |
| File too large | `File exceeds 5MB workspace limit.` |
| Tripwire hit | `Sanitization rejected: catastrophic secret pattern detected; retry with that section redacted.` |

---

## 20. Observability

`tracing` with structured fields:

```text
tool_name, tenant_id, subscription_id,
cache_status, cache_key,
azure_operation, http_status, request_id, operation_id,
duration_ms, slice, draft_id (hashed)
```

Never log tokens, secrets, full user descriptions, contact details, or
ticket bodies by default. Debug logs require
`RUST_LOG=azure_support_ticket_mcp=debug`.
