# Copilot Instructions for Azure Support Ticket MCP

## Source of Truth

- `docs/ARCHITECTURE.md` — system design (layers, contracts, cache, draft workflow, confirmation guard, security model). Source of truth for *how* the system is built.
- `docs/ROADMAP.md` — slice rollout, shipped and planned.
- `docs/RELEASING.md` — release cycle, GitHub Actions pipeline, versioning, troubleshooting.
- `CONTRIBUTING.md` → Scope — product goals and non-goals (the *what* and *why*).
- This file — day-to-day engineering guardrails for implementing all of the above.

This repository is for a Rust-based MCP server that helps Copilot CLI users open Azure support tickets through a guided workflow. Optimize for correctness, speed, safety, and a polished conversational experience.

## Repo Shape

Target the structure described in `docs/ARCHITECTURE.md`:

```text
data/                # support_services_seed.json (embedded at build), seed schema docs
plugin/              # Copilot CLI plugin manifest (plugin.json + .mcp.json)
scripts/             # install.sh + install.ps1 (published as release assets)
src/
  main.rs
  lib.rs
  error.rs
  config.rs
  bootstrap/       # ensure_initialized(), seed loader, doctor
  mcp/             # rmcp stdio adapter, tool registration, tool schemas
  azure/           # auth, ARM, Resource Graph, support (services, classifications,
                   #   tickets CRUD, communications, file workspaces)
  cache/           # SQLite cache, TTLs, refresh state, stale behavior
  resolver/        # context resolution, extractors, ranking, provider mappings
  workflow/        # draft builder/validator, DraftStore trait, confirm guard, share
tests/             # mocked integration tests and unit tests
```

User-scoped runtime paths (not XDG):

```text
~/.azure-support-ticket-mcp/
  config.toml
  cache.sqlite
  drafts.sqlite     # only when drafts.store = "sqlite"
  seed/             # optional downloaded seed asset
```

Keep these boundaries intact. Do not mix MCP transport, Azure HTTP calls, cache persistence, inference logic, draft storage, and ticket workflow decisions in the same module.

## Non-Negotiables

- The project is Rust. Do not introduce Node.js or Python implementations.
- Crate / binary / config namespace is `azure-support-ticket-mcp` (kebab-case); module path is `azure_support_ticket_mcp`.
- MCP SDK is `rmcp` behind a thin adapter. Core stays transport-agnostic; MVP transport is stdio only.
- Do not make Azure CLI a hard dependency. It may be an optional auth/context fallback.
- Multi-tenant user discovery is supported from day one.
- Start ticket flows with tenant and subscription context.
- Infer resource/service context before asking broad service or classification questions.
- Return ranked, explained choices instead of large unfiltered lists.
- Fetch problem classifications only for the selected or strongly inferred service.
- Do not eagerly fetch all classifications for all Azure services.
- Use local SQLite caching at `~/.azure-support-ticket-mcp/cache.sqlite`. Disclose cache freshness in tool responses.
- Use stale-while-revalidate and stale-if-error where cached data is usable.
- All state-changing tools (`create_support_ticket`, `update_ticket`, `reply_to_ticket`, `add_attachments_to_ticket`) require `review_token` + matching `draft_hash` + `confirmed: true`.
- `serve` runs an idempotent `ensure_initialized()`. Startup must never block on Azure RBAC; permission checks are lazy and actionable.
- Keep outbound sharing integrations (Teams/Slack/email/GitHub) out of the MVP. The MVP only formats a copy/paste share summary.
- Never log access tokens, secrets, authorization headers, contact details, or full ticket bodies.
- Normal tests must not require live Azure credentials. Live tests are env-gated; ticket-create live tests are manual-only.
- **No emoji in source files, docs, comments, log messages, or user-facing strings unless the user explicitly asks for one.** This applies to README.md, copilot-instructions.md, docs/ARCHITECTURE.md, docs/ROADMAP.md, docs/RELEASING.md, CONTRIBUTING.md, code comments, `tracing` messages, MCP tool output strings, and any other file in the repo. Use plain ASCII markers like `[warn]`, `[note]`, `**Note:**`, `WARNING:` etc. when emphasis is needed.

## MCP Tool Contract

Prefer workflow-oriented MCP tools over thin API wrappers. Tools are delivered in slices (see `docs/ROADMAP.md`).

Slice 1 (discovery, read-only):

```text
azure_auth_status
azure_resource_search           # ESCAPE HATCH: arbitrary KQL over Resource Graph
list_tenants
list_subscriptions
list_relevant_support_services
list_problem_classifications
refresh_support_cache
doctor
```

Slice 2 (draft + create — side effects gated):

```text
start_support_ticket_flow
resolve_issue_context
build_ticket_draft           # returns review_token + draft_hash
validate_ticket_draft
preview_ticket_draft
create_support_ticket        # SIDE EFFECT
format_ticket_share_message
```

Slice 3 (ticket CRUD + communications — side effects gated):

```text
list_support_tickets
get_support_ticket
update_ticket                # SIDE EFFECT (PATCH severity/status/contact/diagnostic consent)
list_ticket_communications
get_latest_communication
summarize_ticket_thread      # local condense, no internal LLM call
reply_to_ticket              # SIDE EFFECT
```

Slice 4 (attachments via file workspaces — side effects gated):

```text
prepare_attachments          # workspace name = ticket name
add_attachments_to_ticket    # SIDE EFFECT
list_attachments
```

All side-effecting tools require `review_token` + `draft_hash` + `confirmed: true`. The server recomputes `draft_hash` and refuses on mismatch, missing/expired token, or `confirmed != true`.

All tools must return structured, actionable output. For ambiguous choices, return IDs, labels, confidence, and reasons.

**Tool responses must invite iteration, not foreclose it.** When a tool returns empty / no-match / ambiguous results, the response (`next_steps`, `assistant_instructions`, `message`) MUST include concrete alternatives the calling assistant can try next — different scope, different query, different parent resource, the escape-hatch `azure_resource_search` tool. NEVER write directives like "ask the user" as the first option when the assistant can try at least one variation itself; raw shell-equipped agents iterate by varying queries, and our MCP must enable the same behavior or it loses to `az` + shell. Escalation to the user is a fallback after iteration, not a substitute for it.

## Azure Integration Rules

- Use Azure REST APIs directly via `reqwest` (no Azure SDK crate dependency). Behavioral reference: the `azure-support-slack-bot` Python implementation.
- Keep cloud endpoints configurable; default to Azure public cloud. MVP runs against Azure Public only.
- `AuthProvider` is a trait. Order: env credential chain first, `az` CLI fallback second, actionable error otherwise.
- Use **Azure Resource Graph** for resource search and **ARM GET** for exact resource ID validation.
- Required support ticket create API (api-version `2024-04-01`):

```text
PUT /subscriptions/{subscriptionId}/providers/Microsoft.Support/supportTickets/{supportTicketName}?api-version=2024-04-01
```

- Severity rules: `minimal`, `moderate`, `critical`, `highestcriticalimpact` (Premium-only). Phone is required in `contactDetails` for `critical` and `highestcriticalimpact`.
- File workspace name MUST equal the support ticket name. This enables post-create attachment uploads via the same workspace (mirrors portal behavior).
- File workspace chunking: ≤5MB per file, base64 chunks ≤2.5MB, max 2 chunks per file. Surface clear errors above limits.
- Communications API caps at 10 results per page; use paging.
- Preserve Azure error code, HTTP status, operation ID, and request ID in structured errors.
- Do not hide Azure auth, permission, support-plan, throttling, or API errors behind success-shaped fallbacks.

## Resolver and UX Rules

Use deterministic inference before fuzzy matching:

1. Full Azure resource ID.
2. Azure portal URL.
3. Provider/type.
4. Exact resource name within selected subscription.
5. Known Azure error codes or operation names.
6. Keyword/fuzzy match against cached service and classification names.

The resolver should return ranked candidates with confidence and explanation. Prefer asking the user to choose from a short ranked list over asking open-ended questions.

Problem classification UX must be scoped by service:

- Resolve or confirm service first.
- Load/fetch classifications for that service only.
- Rank classifications using issue text, resource type, and error text.
- Show top relevant choices first.

## Cache Rules

Use SQLite at `~/.azure-support-ticket-mcp/cache.sqlite`, scoped by cloud, tenant, subscription, service, or account as appropriate.

Expected cached data:

```text
tenants
subscriptions
support_services         # seeded from data/support_services_seed.json; live refresh later
problem_classifications  # lazy per service
resource_inventory       # via Azure Resource Graph
cache_refresh_state
seed_meta
```

Seed data:

- Source: `data/support_services_seed.json`, normalized from the prior `azure-support-slack-bot` dataset.
- Embedded in the binary via `include_bytes!` for offline fallback.
- Optionally upgraded via a versioned GitHub Release asset matching the binary version.
- `ensure_initialized()` loads the seed idempotently on `serve`.

Refresh behavior:

- Tenants and subscriptions: short TTL.
- Support services: longer TTL.
- Problem classifications: lazy per service with longer TTL.
- Resource inventory: short TTL and only when needed.
- If cache is fresh, return immediately.
- If cache is stale but usable, return stale data immediately and refresh in the background.
- If cache is missing and required, fetch once and return a clear error if fetch fails.
- Use single-flight locking to avoid duplicate refreshes.

## Rust Design Standards

Follow idiomatic Rust and the Rust Design Patterns guidance:

- https://rust-unofficial.github.io/patterns/
- https://rust-unofficial.github.io/patterns/intro.html
- https://rust-unofficial.github.io/patterns/idioms/index.html
- https://rust-unofficial.github.io/patterns/patterns/index.html
- https://rust-unofficial.github.io/patterns/anti_patterns/index.html

Apply SOLID in a Rust-native way:

- Single responsibility: small modules and focused functions.
- Open/closed: traits, enums, builders, and table-driven mappings for extension points.
- Liskov substitution: trait implementations must have consistent behavior and error semantics.
- Interface segregation: prefer small capability traits over a single large client trait.
- Dependency inversion: workflow code depends on traits, concrete clients are wired at the boundary.

Prefer:

- Strong domain types for tenant IDs, subscription IDs, resource IDs, service IDs, classification IDs, severity, cache keys, and ticket names.
- `Result` with typed errors for recoverable failures.
- Builders for complex ticket drafts and client/cache configuration.
- Enums for workflow state and known variants.
- Traits for auth providers, Azure clients, cache stores, ranking strategies, clocks, and share formatters.
- `tracing` for diagnostics.

Avoid:

- Stringly typed internal APIs.
- Broad catch-all error handling.
- Unnecessary `clone`, `Arc`, `Mutex`, dynamic dispatch, or heap allocation.
- Blocking work inside async contexts.
- Singleton-style global mutable state.
- Object-oriented pattern imitation that fights Rust ownership and type safety.

## Draft and Confirmation Rules

- `DraftStore` is a trait. Implementations: `MemoryDraftStore` (default) and `SqliteDraftStore` (opt-in via `drafts.store = "sqlite"`).
- Default draft TTL: 7 days. `ttl_days = 0` disables TTL. Drafts are deleted on successful submit unless retention override is set.
- `build_ticket_draft` returns `review_token` (UUIDv7, 30-min idle expiry) and `draft_hash` (SHA-256 of canonical draft JSON).
- Side-effecting tools must validate: token exists & not expired, recomputed `draft_hash` matches input, `confirmed === true`. Any mismatch → refuse with actionable error.

## Configuration Rules

- Config file: `~/.azure-support-ticket-mcp/config.toml`.
- All keys overridable by `AZURE_SUPPORT_TICKET_MCP_*` env vars.
- Sections: `[general]`, `[auth]`, `[cache]`, `[drafts]`, `[seed]` (see `docs/ARCHITECTURE.md` §7).

## Testing and Validation

Add or update tests for behavior changes. Prefer mocked Azure HTTP tests (`wiremock`).

Test tiers:

- **Mocked** — default, runs without network or credentials.
- **Live read-only** — gated by `AZ_SUPPORT_MCP_LIVE=1`.
- **Live ticket-create** — manual only, gated by `AZ_SUPPORT_MCP_LIVE_CREATE=1`; never in CI.

Prioritize coverage for:

- Azure resource ID parsing.
- Azure portal URL parsing.
- Provider/type to support-service mapping.
- Tenant and subscription selection behavior (multi-tenant).
- Cache TTL, stale-while-revalidate, stale-if-error, and single-flight refresh behavior.
- Lazy problem classification fetching.
- Ranking output with confidence and reasons.
- Ticket draft validation (incl. severity → phone requirement).
- `review_token` + `draft_hash` + `confirmed: true` guard.
- Seed loader (embedded + downloaded + version checks).
- Attachment chunking + size limits.
- Communications paging.
- Share-summary formatting.

Use existing Rust tooling only:

```text
cargo fmt
cargo clippy --all-targets --all-features -D warnings
cargo test
```

Do not add new build, lint, or test systems unless they are necessary and documented.

## Security and Privacy

- Treat ticket descriptions, contact details, subscription IDs, resource IDs, and tenant IDs as potentially sensitive.
- Minimize logging of user-provided issue text.
- Redact secrets and tokens from errors and diagnostics.
- Keep ticket creation and outbound sharing separate.
- Any future outbound sharing tool must require explicit recipient/channel, message preview, and `confirmed: true`.

## Git Restrictions

Never use Git command-line operations in this project.

- Do not run `git status`, `git diff`, `git add`, `git commit`, `git push`, `git pull`, `git fetch`, `git checkout`, `git reset`, `git branch`, `git merge`, `git rebase`, `git stash`, `git log`, or any other `git` command.
- Do not use GitHub CLI commands to inspect or mutate repository state on behalf of the user.
- The user exclusively manages Git state, commits, branches, pushes, pulls, and repository history.
