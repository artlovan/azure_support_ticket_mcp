# azure-support-ticket-mcp

Opening an Azure support ticket should take seconds, not minutes. **azure-support-ticket-mcp** is a fast, local MCP server that turns the full ticket lifecycle into a guided conversation — no portal, no context switch, no re-typing the resource ID Azure already knows about.

---

<!--
  Demo asset — captured with asciinema, sanitized, rendered with agg/ffmpeg.
  See docs/media/README.md for the workflow.
-->

![Demo: opening an Azure support ticket from Copilot CLI](./docs/media/demo.gif)

## Quick start (5 minutes)

### 1. Install

**macOS / Linux:**

```bash
curl -sSL https://github.com/artlovan/azure_support_ticket_mcp/releases/latest/download/install.sh | sh
```

**Windows (PowerShell):**

```powershell
irm https://github.com/artlovan/azure_support_ticket_mcp/releases/latest/download/install.ps1 | iex
```

The install script detects your OS and CPU architecture, downloads the
matching binary from the latest GitHub release, verifies its SHA256
checksum, and installs it under your home directory:

- **macOS / Linux:** `$HOME/.local/bin/azure-support-ticket-mcp`
  (override with `--prefix=/usr/local` for a system-wide install).
- **Windows:** `%LOCALAPPDATA%\Programs\azure-support-ticket-mcp\`.

The script prints a one-line `PATH` hint if the install directory isn't
already on your `PATH`. Open a new shell after installing so the change
takes effect.

> **Prefer to download the binary yourself?** Every release also publishes
> the raw per-platform binaries and their `.sha256` sidecars at
> `https://github.com/artlovan/azure_support_ticket_mcp/releases/latest/download/azure-support-ticket-mcp-<os>-<arch>`.
> Pick the asset matching your platform, verify the checksum, `chmod +x`,
> and move it onto your `PATH`.

Verify it's installed and the embedded support-service catalog loaded
correctly (no Azure call yet):

```bash
azure-support-ticket-mcp doctor
```

You should see something like:

```text
app dir: /Users/you/.azure-support-ticket-mcp
cache path: /Users/you/.azure-support-ticket-mcp/cache.sqlite
cloud: AzurePublicCloud
drafts.store: memory
seed download: false
cache: OK (425 services, seed version "2024-04-01-1")
az cli: FOUND (/opt/homebrew/bin/az)
arm reachable: OK (HTTP 400 Bad Request)
```

* **`cache: OK (425 services …)`** — the embedded support-service catalog
 loaded; the server is usable offline.
* **`az cli: FOUND`** — if present, the server can fall back to the `az` CLI
 for auth (so you don't have to set service-principal env vars).
* **`arm reachable: OK (HTTP 400 …)`** — `400` on a HEAD probe is expected and
 proves we can talk to `management.azure.com`.

> Building from source instead? See
> [`CONTRIBUTING.md`](./CONTRIBUTING.md) → Development environment.

### 2. Authenticate to Azure

Either:

```bash
# Easiest: use the Azure CLI you already have.
az login
```

…or set environment variables for a service principal:

```bash
export AZURE_TENANT_ID=...
export AZURE_CLIENT_ID=...
export AZURE_CLIENT_SECRET=...
```

The server prefers env vars and falls back to `az` unless you set
`AZURE_SUPPORT_TICKET_MCP_AUTH_ALLOW_AZ_CLI_FALLBACK=false`.

### 3. Register with Copilot CLI

```bash
copilot plugin install artlovan/azure_support_ticket_mcp:plugin
```

This installs a small [Copilot CLI plugin](./plugin/) that tells Copilot
how to spawn the binary you installed in step 1 (`azure-support-ticket-mcp serve`).
The plugin itself is just a launcher config — it does **not** ship the
binary, so step 1 must be done first.

Verify with:

```bash
copilot plugin list                 # should show azure-support-ticket-mcp
copilot mcp list                    # should show azure-support-ticket-mcp registered
```

> Prefer to register manually without the plugin? You can still do it the
> old-fashioned way — the plugin is purely a convenience wrapper:
>
> ```bash
> copilot mcp add azure-support-ticket-mcp -- \
>   "$(command -v azure-support-ticket-mcp)" serve
> ```
>
> `copilot mcp add` only **registers** the launcher in Copilot's config —
> it does not start anything. The MCP process is spawned by Copilot CLI
> each time you open an interactive session and is shut down when the
> session ends. Inspect with `copilot mcp list` · remove with
> `copilot mcp remove azure-support-ticket-mcp`.

### 4. Try it

> **Tip:** add `--allow-tool=azure-support-ticket-mcp` to any `copilot` command
> below to skip the per-tool approval prompts. The MCP's own preview-then-confirm
> gate still requires you to actively approve every state-changing call —
> see [Security](#security).

In a new Copilot CLI session, try a prompt like any of these:

```bash
# AKS cluster scale-out problem
copilot -i "open a support ticket — my AKS cluster prod-aks can't scale out"

# Azure OpenAI rate limiting
copilot -i "my Azure OpenAI deployment gpt-4o-prod is returning 429s every few minutes — please open a ticket"

# GPU VM quota error
copilot -i "file a support request — I can't deploy more GPU VMs in eastus, hitting a quota error"

# Networking / private endpoint issue
copilot -i "open a ticket — VMs in app-vnet can't reach Cosmos DB prod-cosmos through its private endpoint"
```

Or pipe an error log straight in from your terminal:

```bash
# Any log file — generic catch-all
copilot -i "ticket this: $(cat err.log)"

# GitHub Actions / CI failure
copilot -i "ticket this: $(gh run view <run-id> --log-failed)"

# Azure Developer CLI: provision + deploy
copilot -i "ticket this: $(azd up 2>&1)"

# Azure Functions deploy failure
copilot -i "ticket this: $(func azure functionapp publish my-app 2>&1)"

# Live app logs from Azure Container Apps
copilot -i "ticket this: $(az containerapp logs show -n my-app -g rg --tail 200 2>&1)"

# ARM deployment failure
copilot -i "ticket this: $(az deployment operation list --resource-group rg --name dep -o json 2>&1)"

# Pod scheduling / events / image-pull issues
copilot -i "ticket this: $(kubectl describe pod my-pod 2>&1)"

# Runtime app logs from a pod
copilot -i "ticket this: $(kubectl logs my-pod --tail=200 2>&1)"

# Terraform IaC apply failure
copilot -i "ticket this: $(terraform apply 2>&1)"
```

Copilot will walk you through: pick the right support service → pick a problem
classification → fill contact details → **preview** → confirm → ticket opens.
When you pipe error logs, the MCP auto-extracts the safe context (resource
IDs, error codes, severity hints) and runs the LLM sanitization step
described in [Security](#security) before anything is persisted or sent to
Azure.

> Every side-effecting tool (`create_support_ticket`, `update_support_ticket`,
> `reply_to_ticket`, `add_attachments_to_ticket`) is **gated**: nothing hits
> Azure until you confirm a one-time `review_token` + `draft_hash` returned by
> the preview call. Stale or tampered confirmations are rejected.

---

## Security

The intent is to make piping raw error output into a support ticket safer
than copy-pasting it through the portal, not to make it risk-free. A few
deliberate choices help here:

### Sanitization happens in the LLM, not the MCP

When you paste or pipe error text, the LLM in your host (Copilot CLI,
Claude, etc.) reads it first and is instructed to scrub obvious secrets
(connection strings, keys, tokens, private keys, passwords) before passing
the result to the MCP. The MCP itself never receives the raw text directly
as the draft body — it gets the sanitized version plus a summary of what
was redacted.

This means the LLM does see your raw text on the way through. Treat the LLM
as part of your trust boundary (which it already is for any other Copilot
interaction); the MCP doesn't change that.

### A last-resort pattern check before persisting

Before saving a sanitized draft, the MCP runs a small unambiguous-pattern
check (e.g. PEM private key blocks, full Azure storage connection strings)
and refuses to persist the draft if any matches. This is a safety net, not
a guarantee — it covers a deliberately narrow set of patterns that should
never appear in a support ticket regardless of context. Anything subtler
(API keys without distinguishing prefixes, custom-format tokens, business
data) relies on the LLM step above.

### Preview before submit

Every state-changing tool — create, update, reply, attach — is two-call.
The first call returns a preview showing exactly what will hit Azure. The
second call submits it, and only goes through if the draft hasn't changed
in between.

### What lives where

- **In memory only:** raw error text (held just long enough for the LLM
  step), in-progress drafts, review tokens.
- **On disk** (`~/.azure-support-ticket-mcp/`): cache of tenant /
  subscription / service / classification metadata and ticket history (no
  secrets); your contact templates (name, email, phone, locale — whatever
  you put in them).
- **Never logged:** access tokens, Authorization headers, contact details,
  ticket bodies. Logs go to stderr at structured-field granularity only.

### Reset

To wipe all local state (cache, templates, drafts), delete the app
directory. The MCP will re-create what it needs on next startup; you'll
lose your saved contact templates and need to re-seed them.

```bash
rm -rf ~/.azure-support-ticket-mcp/
```

---

## Features

### Error ingestion
- **Pipe stdout/stderr straight into a ticket** — `copilot -i "ticket this: $(cat err.log)"` walks all the way to a draft
- **Auto-extracts safe context** — ARM resource IDs, error codes, correlation IDs, HTTP status (5xx -> critical, 429 -> moderate), kubectl event reasons
- **LLM-sanitized before persisting** — the LLM scrubs obvious secrets; the MCP runs a last-resort pattern check before saving the draft (see [Security](#security))
- **Auto-approve mode** for scripts/CI via `copilot -p "..." --allow-all-tools` (the safety contract still holds; the assistant just runs preview -> submit back-to-back)

### Drafts
- **Start** a guided draft (with prefilled fields from your default template)
- **Build / patch** an existing draft incrementally (the assistant adds info turn-by-turn)
- **Preview** the draft as a pretty markdown table before committing
- **List** all in-progress drafts
- **Discard** a draft you no longer want
- **Resume** any draft by ID

### Tickets

The full ticket lifecycle — open, manage, converse, attach, close — through one set of tools.

**Lifecycle:**
- **Create** a new support ticket from a guided draft (with confirmation preview)
- **List** your existing tickets (with `top` / `state` filters; cached locally)
- **Get** a single ticket's full details
- **Update** ticket fields — severity, contact info, CC recipients
- **Close** tickets (status update) when an issue is resolved

**Conversation with support engineers:**
- **Read the full thread** — list every customer + Microsoft message on a ticket
- **Summarize the thread** locally (no LLM round-trip) for a quick "what's the current state"
- **Reply to the support engineer** with a confirmation preview before posting
- **Newest-only** mode (`top=1`) for "what did support say last?"

**Attachments (images, logs, anything):**
- **Attach files** to a ticket (screenshots, traces, har files, etc.)
- **Pre-create** workspace at draft time so the first message can carry attachments
- **List** existing attachments on a ticket
- **Smart limits enforced**: 5 files per upload call, 25 files total per ticket, 5 MB per file (matches the Azure portal cap)

### Reusable contact templates
- **Init** a starter template (your name, email, phone, contact prefs, locale, etc.)
- **List / get** all saved templates
- **Save** the current contact info as a new template (or overwrite an existing one)
- **Delete** templates you don't need
- **Auto-save** contacts to your `default` template every time you create a ticket (opt-out per call)

### Smart scoping (tenant → subscription → service → classification)
- **Resolve issue context** from a pasted portal URL, ARM resource ID, or free-form description (e.g. "my storage account named foo is throwing 403s")
- **Disambiguation prompts** when your wording could match multiple services (with an always-present "Other — describe differently" escape hatch)
- **Auto-narrow** problem classifications to the specific service you picked (no scrolling through thousands of irrelevant ones)
- **Tenant auto-resolution** from a subscription ID (cache → ARM single-GET → ARM list fallback)
- **List tenants / subscriptions** with caching and filter by tenant

### Authentication & identity
- **`az login` credential chain** — zero new credentials to manage
- **`whoami`** — shows the signed-in identity (email/UPN) the MCP will use
- **`azure_auth_status`** — explicit check of token validity / which credential type is active
- **Tenant + subscription confirmation** before any destructive operation — never silently picks one

### Performance & freshness
- **Local SQLite cache** for tenants, subscriptions, services, problem classifications, tickets, and communications
- **TTL-based smart refresh** — stale-while-revalidate on read paths
- **`refresh_support_cache`** for explicit warm-up after a long idle
- **Write-through** — every ARM lookup populates the cache for next time

### Health & diagnostics
- **`doctor`** — one-shot self-check (auth, cache, ARM reachability, file workspace API)
- **Display-quality warnings** in every preview (missing tenant, missing resource, missing classification, short description, critical severity) — informational, never blocks submission
- **Helpful error mapping** — Azure error codes translated to actionable guidance

### Safety: human-in-the-loop confirmation
- **Two-step confirmation** on every destructive op (create, update, reply, upload) — see the rendered ticket as a markdown table before anything is submitted
- **Draft integrity check** — if the draft changes between preview and submit, the submit is rejected

---


## Setting up your contact template

> See the [Reusable contact templates](#reusable-contact-templates) feature group for the full capability list — this section is the setup walkthrough.

The MCP can remember your contact details (name, email, country, language,
timezone, etc.) so you're not asked the same questions every time you open a
ticket. Templates live in `~/.azure-support-ticket-mcp/templates/<name>.json`.

### The `default` template (auto-created)

The MCP creates a `default` template automatically the first time it runs.
It seeds the template from:

* Your ARM token claims → `primary_email_address`, `first_name`, `last_name`, `tenant_id`;
* OS locale (`LANG`, `/etc/localtime`) → `country`, `preferred_support_language`, `preferred_time_zone`.

Inspect what got seeded:

```bash
cat ~/.azure-support-ticket-mcp/templates/default.json
```

Update it through Copilot — *"set my preferred contact method to phone and
add `+1-555-0100`"* calls `save_ticket_template` and patches the JSON. Or
edit the file directly. To re-seed from scratch (e.g. after switching
identities), use *"reset my default template"* (`init_ticket_template` with
`overwrite=true`).

### Named templates (per-team / per-project)

When you want a separate template — say, on-call contact for a specific
service — save one:

> *"Save a template called `team-aks` with on-call email `oncall-aks@example.com`
> and description 'On-call for AKS prod'."*

Copilot calls `save_ticket_template`. Then later:

> *"Open a critical ticket using the `team-aks` template — pod crash loop on
> `prod-aks-east`."*

Copilot passes `template_name: "team-aks"` to `start_support_ticket_flow`. Any
fields the template provides won't be re-asked.

### What gets auto-saved

After a successful `create_support_ticket`, the contact slice of the submitted
draft is written back to `default.json` (best-effort, non-fatal). To opt out
for one ticket: *"don't update my default template this time"*
(`save_as_default_template: false`). To capture under a named template:
*"save these contacts as `team-aks`"* (`save_as_template_name: "team-aks"`).

Template names must match `[A-Za-z0-9_-]{1,64}` (blocks path traversal).
Writes are atomic (temp file + rename). For the full list of template tools see
[Tool reference](#tool-reference).

---

## End-to-end walkthroughs

You can paste these requests into Copilot CLI — the model picks the right tools
under the hood. The tool names below let you trace what's happening if you want
to follow along in logs.

### A. Open a brand-new ticket

> *"Open a moderate severity ticket about my prod AKS cluster failing to
> autoscale. Use my default subscription."*

Behind the scenes:

1. `resolve_issue_context` ranks support services from the resource id / phrase.
2. `list_problem_classifications` lists problem buckets for the chosen service.
3. `start_support_ticket_flow` → `build_ticket_draft` fills the form.
4. `preview_ticket_draft` shows you the body — **confirm before submission**.
5. `create_support_ticket` PUTs it; returns ticket name + portal URL +
 a copy-paste-friendly share message.

### B. Open a ticket *with* attachments

> *"Same as above, but also attach `~/logs/aks-events.log` and
> `~/logs/kubectl-describe.txt`."*

Adds one extra step before submission:

* `prepare_attachments` — creates the file workspace, uploads each file (chunked
 to ≤2.5 MB base64 per chunk; files capped at 5 MB), and pins the workspace
 name to the draft. `create_support_ticket` then reuses that name as the
 ticket name (Azure convention).

### C. Triage an existing ticket

> *"What's the latest on ticket `xxxxxxxxxxxxxx-xxxx`?"*

* `summarize_ticket_thread` — pulls ticket body + recent communications,
 produces a deterministic local summary (counts inbound/outbound replies,
 truncates the latest message body to ~500 chars). **No LLM is invoked from
 inside the MCP.**

For finer detail:

* `list_ticket_communications` — paged thread (max 10 per page; pass `top=1`
 to get just the newest reply).
* `get_support_ticket` — full raw body.

### D. Reply to a ticket

> *"Reply to ticket `xxxxxxxxxxxxxx-xxxx`: subject 'Update', body 'Issue still
> reproducing in eastus2, attaching new logs'."*

* `reply_to_ticket` — first call returns a preview. Copilot shows you exactly
 what will be posted; confirming sends it.

### E. Add more attachments to an existing ticket

> *"Add `~/logs/new-trace.txt` to ticket `xxxxxxxxxxxxxx-xxxx`."*

* `add_attachments_to_ticket` — same two-call confirmation pattern. Note that
 attachments live on the **ticket workspace**, not on any individual reply.

### F. Update severity / status / contact

> *"Bump ticket `xxxxxxxxxxxxxx-xxxx` to critical and set primary email to
> `oncall@example.com`."*

* `update_support_ticket` — two-call. Allowed fields: severity, status,
 `advancedDiagnosticConsent`, contact details.

### G. Browse your tickets

> *"List my open support tickets in the prod subscription."*

* `list_support_tickets` — paged via `$top` + `next_link`, optional OData
 `$filter` like `Status eq 'Open'`.

---

## Configuration

All settings live in `~/.azure-support-ticket-mcp/`:

| File | Purpose |
| --- | --- |
| `config.toml` | Overrideable settings (`[general]`, `[auth]`, `[cache]`, `[drafts]`, `[seed]`). |
| `cache.sqlite` | Services + classifications cache (auto-created). |
| `drafts.sqlite` | Persistent draft store, only when `drafts.store = "sqlite"`. |

Env overrides (prefix `AZURE_SUPPORT_TICKET_MCP_`):

| Variable | Purpose |
| --- | --- |
| `…_HOME` | Override the entire app dir (used by tests). |
| `…_CLOUD` | e.g. `AzurePublicCloud`, `AzureUSGovernment`. |
| `…_LOG_LEVEL` | `trace` / `debug` / `info` / `warn` / `error`. |
| `…_AUTH_PREFER` | `env` (default) or `az_cli`. |
| `…_AUTH_ALLOW_AZ_CLI_FALLBACK` | `true` / `false`. |
| `…_CACHE_PATH` | Move the SQLite cache. |
| `…_DRAFTS_STORE` | `memory` (default) or `sqlite`. |

Azure auth chain: env vars (`AZURE_TENANT_ID` + `AZURE_CLIENT_ID` +
`AZURE_CLIENT_SECRET`) → `az` CLI fallback (when enabled).

---

## Tool reference

The MCP advertises all tools and their JSON schemas over the protocol itself — your client (Copilot CLI, Claude Desktop, etc.) lists them on connect. For engineering-level docs see [`docs/ARCHITECTURE.md`](./docs/ARCHITECTURE.md); for the canonical tool descriptions read the `#[tool]` annotations in [`src/mcp/server.rs`](./src/mcp/server.rs).

---

## Troubleshooting

| Symptom | Try this |
| --- | --- |
| `doctor` says `az cli: NOT FOUND` | Install Azure CLI or set env-var credentials. |
| `doctor` says `arm reachable: FAIL` | Check proxy / corp firewall; verify `https://management.azure.com` is reachable. |
| Copilot says "no auth provider succeeded" | Run `az login` or export `AZURE_TENANT_ID`/`AZURE_CLIENT_ID`/`AZURE_CLIENT_SECRET`. |
| `create_support_ticket` returns `403` / `AuthorizationFailed` | Your identity needs `Microsoft.Support/supportTickets/write` (often via `Support Request Contributor`). |
| `review_token expired` | Tokens have a 30-minute idle TTL. Re-run the build/preview step to get a fresh one. |
| `draft_hash mismatch` | You changed the draft (or the patch) since the token was issued. Re-preview to get a new token + hash. |
| `severity` rejected | Valid values: `minimal`, `moderate`, `critical`, `highestcriticalimpact` (last is Premium-only). For `critical`+, phone number is required in contact details. |
| MCP tools unavailable after resuming a Copilot CLI session | Copilot CLI doesn't always re-handshake MCP servers when you resume a session. Run `/restart` to force re-discovery. (Tracking upstream; not something the MCP can fix from its side.) |
| Verbose logs | `export AZURE_SUPPORT_TICKET_MCP_LOG_LEVEL=debug` (logs go to **stderr** so they never pollute MCP stdio). |

---

## Contributing

Contributions are welcome. See [`CONTRIBUTING.md`](./CONTRIBUTING.md) for the
development environment setup, Rust style, MCP tool contract, testing
expectations, and PR workflow. For larger changes, please open an issue
first to discuss the approach.

For engineering guardrails see
[`.github/copilot-instructions.md`](./.github/copilot-instructions.md);
for the full design see [`docs/ARCHITECTURE.md`](./docs/ARCHITECTURE.md);
for shipped vs planned work see [`docs/ROADMAP.md`](./docs/ROADMAP.md).
