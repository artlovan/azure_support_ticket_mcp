# Roadmap

What has shipped and what's planned for `azure-support-ticket-mcp`.

For scope and goals see [`CONTRIBUTING.md`](./CONTRIBUTING.md) → Scope. For
how it's built see [`ARCHITECTURE.md`](./ARCHITECTURE.md).

---

## Shipped

### Slice 1 — Discovery + cache foundation

- Rust skeleton with `rmcp` stdio.
- `serve` runs `ensure_initialized()`.
- Auth (env + az CLI fallback) and `azure_auth_status`.
- Multi-tenant discovery: `list_tenants`, `list_subscriptions`.
- SQLite cache + seed load (embedded).
- `list_relevant_support_services`, `list_problem_classifications`
  (read-only).
- `refresh_support_cache`, `doctor`.

**Acceptance:** a user with valid env credentials runs `serve`, lists
their tenants/subscriptions, and gets a ranked list of support services
for a typed resource ID — entirely offline-friendly using seed.

### Slice 2 — Draft + create ticket

- Resolver (`resolve_issue_context`) with all extractors.
- Draft builder + validator + pluggable `DraftStore`.
- Review token + `draft_hash` confirmation guard.
- `create_support_ticket` end-to-end (Technical tickets only in MVP).
- `format_ticket_share_message`.
- Live test (manual) for actual ticket submission.

**Acceptance:** the MVP user prompt below works end-to-end.

### Slice 3 — Ticket CRUD + communications

- `list_support_tickets`, `get_support_ticket`.
- `update_ticket` (PATCH severity/status/contact/diagnostic consent),
  gated.
- `list_ticket_communications`, `get_latest_communication`,
  `summarize_ticket_thread`.
- `reply_to_ticket`, gated.

### Slice 4 — Attachments via file workspaces

- `prepare_attachments` (pre-create), wires `fileWorkspaceName`.
- `add_attachments_to_ticket` (post-create).
- `list_attachments`.
- Chunked upload with clear size errors.

### Slice 5 — Zero-friction error ingestion

- `ingest_error_context` — recognizers + `sanitize_token`.
- `commit_sanitized_context` — tripwire-checked persistence.
- Deterministic recognizers in `src/resolver/recognizers.rs` (ARM error
  envelope, az deployment failed, resource ID, HTTP status, kubectl
  events).
- Defense-in-depth secret tripwire in
  `src/workflow/secret_tripwire.rs` (4 catastrophic patterns).
- Preview shows full sanitized description + redaction summary so the
  user sees ALL data before confirming.
- Compatible with `copilot -i "ticket this: $(cat err.log)"` and
  auto-approve via `copilot -p ... --allow-all-tools`.

See [`ARCHITECTURE.md`](./ARCHITECTURE.md) §18 for the trust-boundary
model.

---

## Planned

### Slice 6+ (post-MVP)

- Billing / Subscription Management / Quota ticket types.
- Centralized service-principal onboarding.
- Outbound sharing integrations (Teams/Slack/email/GitHub) — must require
  explicit recipient/channel + message preview + `confirmed: true`.
- SSE / non-stdio transports.
- Streaming ingest for content > 1 MiB.
- Additional recognizers (App Insights, Log Analytics, Defender alerts,
  terraform output) — added as users tell us what shapes they paste most.
- Per-tenant "redaction policy" config.

### Distribution channels (post-v1)

The v1 release uses `curl | sh` (Linux/macOS) + `iwr | iex` (Windows) for
binary install. Additional channels we may add once there's user demand:

- **Homebrew tap** — `brew install artlovan/tap/azure-support-ticket-mcp`.
  Requires a separate `homebrew-tap` repo with a Ruby Formula; release
  workflow bumps version + SHA256 each tag.
- **Scoop / Winget** (Windows) — once Windows usage warrants it.
- **`cargo install azure-support-ticket-mcp`** — publish to crates.io so
  Rust users can install from source without cloning. Also enables
  reproducible-from-source verification.

---

## Deferred design decisions

Decisions we made consciously NOT to implement in v1, with the
rationale, alternatives considered, and the concrete trigger that
should make us revisit. These are the seed-data ADRs; future ADRs land
here too.

### ADR-1: Upstream catalog refresh is manual in v1

**Status:** Accepted. Defer automation to post-v1.

**Context.** The seed in the repo can drift from what Azure actually
serves at `Microsoft.Support/services`. Microsoft adds new services,
renames existing ones (e.g. "OpenAI" → "Azure OpenAI", "Azure AI Foundry"
→ "Microsoft Foundry"), and prunes deprecated/duplicate entries.

**Options considered.**

1. **Manual refresh per release** (chosen for v1). Contributor runs
   `scripts/refresh_support_services_seed.py` before cutting a release.
2. **CI age-check.** Warn if the seed's `generated_at` is more than N
   days old. Cheap, but a proxy — doesn't actually verify against the
   live API. A seed that's "5 days old" by timestamp can still be stale
   if Microsoft renamed 10 services yesterday.
3. **Scheduled auto-PR.** Weekly GitHub Action runs the refresh script
   and opens a PR with the diff for human review + hand-classification
   of any new entries. Requires an Azure service principal + GitHub
   secrets (the `Microsoft.Support/services` endpoint requires ARM auth
   — verified: an unauthenticated request returns
   `HTTP 401 AuthenticationFailed`).

**Decision.** Option 1 for v1. Single maintainer, pre-release,
infrequent catalog changes — the manual checklist step
(`docs/RELEASING.md` "Cutting a release" §2) is sufficient.

**Trigger for revisit.** Move to Option 3 when EITHER:
- The project has external users actively reporting catalog staleness, OR
- The maintainer is shipping less than monthly AND the seed drift is
  meaningfully hurting users.

Implementation cost is ~30 min (one workflow + one-time SP setup);
deferred only because the value isn't there yet.

### ADR-2: End-users on older binaries cannot refresh the catalog without upgrading

**Status:** Accepted. Decoupled runtime refresh is planned for Slice 6+.

**Context.** Embedding the seed in the binary (`include_bytes!`) means
catalog freshness is tied to binary releases. A user on a binary that's
6 months old has a 6-month-old catalog. Worst-case observable impact:
the model could surface a deprecated service ID; submitting a ticket
against it would be rejected by Azure.

**Options considered.**

1. **Embedded-only, upgrade-to-refresh** (chosen for v1). User installs
   a newer binary to get a newer catalog.
2. **Optional download from GitHub Release.** A CLI subcommand
   (`azure-support-ticket-mcp refresh-seed`) downloads
   `releases/latest/download/support_services_seed.json` to
   `~/.azure-support-ticket-mcp/seed/`. On startup the binary prefers
   the downloaded file when its `version` is newer than embedded; falls
   back to embedded otherwise. Embedded remains the bootstrap path so
   fresh installs and `rm -rf` recovery still work offline.
3. **Always-fetch on startup.** Network call on every `serve` spawn.
   Rejected — adds startup latency, breaks offline use, requires
   caching anyway.

**Decision.** Option 1 for v1. Forcing a binary upgrade for catalog
freshness is the simplest path and acceptable while releases are
frequent. The `[seed]` config section in `docs/ARCHITECTURE.md` §7
already specifies the `release_url_template` placeholder that Option 2
would consume — the architecture slot exists, the implementation is
deferred.

**Non-decision.** Forcing binary upgrades for catalog freshness is
**not** acceptable as a long-term policy. If/when the catalog-update
cadence outpaces the binary-release cadence, Option 2 ships. Until
then, users have a usable tool with whatever catalog their binary
embeds, and the worst case (rejected ticket against a dead service ID)
is recoverable in seconds.

**Trigger for revisit.** Move to Option 2 when EITHER:
- A user reports being stuck on a stale catalog, OR
- The project ships less than monthly AND the manual refresh script has
  accumulated meaningful drift since the last binary release.

**Constraint maintained until then.** Continue uploading
`support_services_seed.json` as a release asset (in
`.github/workflows/release.yml`) so Option 2 is unblocked whenever we
choose to build it — without that, the runtime download path would
need a separate asset-publishing change first.

---

## MVP acceptance (end of Slice 2)

Handle this prompt end-to-end:

```text
Open an Azure support ticket for my AKS cluster prod-aks. Nodes are
failing to scale out with quota errors.
```

Required flow:

1. Identify or ask for tenant.
2. Identify or ask for subscription.
3. Resolve the AKS resource (ranked candidates if ambiguous).
4. Resolve "Azure Kubernetes Service" support service.
5. Fetch / load only AKS-relevant problem classifications.
6. Rank and show only top relevant classifications.
7. Ask only for missing ticket fields; prefill safe defaults.
8. Show final draft + return `review_token` + `draft_hash`.
9. Create the ticket only after `confirmed:true` + matching token/hash.
10. Return ticket ID, status, portal URL, and share summary.

---

## Build order (historical, for context)

The order Slices 1–5 were built in:

1. Cargo project + `rmcp` stdio skeleton + `serve` subcommand.
2. Config (TOML + env) and paths under `~/.azure-support-ticket-mcp/`.
3. SQLite cache + migrations + seed loader (embedded only).
4. `AuthProvider` trait + env chain + az CLI fallback +
   `azure_auth_status`.
5. ARM REST client + `list_tenants` + `list_subscriptions`.
6. Support services + classifications fetch with SWR + single-flight.
7. Resolver (resource ID, portal URL, provider-type map, ARG search) +
   ranking.
8. Draft store (Memory + SQLite) + validator.
9. Confirmation guard (review_token + draft_hash).
10. `create_support_ticket` + share summary.
11. Slice 3 tools (CRUD + communications).
12. Slice 4 tools (attachments).
13. Slice 5 (zero-friction ingest + tripwire).
14. Doctor + GitHub Release seed download.
15. Tests, README, MCP registration docs.
