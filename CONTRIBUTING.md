# Contributing

Thanks for your interest in contributing to `azure-support-ticket-mcp`.

If you're planning a non-trivial change, please **read the Scope section
first** and open an issue to discuss the approach. For larger changes,
this saves everyone time.

---

## Scope

The project's scope is deliberately narrow. Features that fall outside
this list will generally be declined.

### What this project is

A Rust-based MCP server (`azure-support-ticket-mcp`) that helps a Copilot
CLI user (and any other MCP client over stdio) manage Azure support
tickets end-to-end through a guided, fast, conversational workflow.

It discovers Azure context, narrows choices using deterministic inference
plus a curated catalog, caches stable metadata locally, validates ticket
drafts, and only mutates Azure state after explicit user confirmation. It
should feel like a guided assistant, not a thin Azure REST wrapper.

### Product goals

- Help users **create** Azure support tickets with minimal manual field
  entry.
- Help users **read, update, reply to, and attach files to** existing
  tickets.
- Start every flow from tenant and subscription context, discovered
  across **all** accessible tenants for the signed-in user (multi-tenant
  from day one).
- Infer the affected Azure resource and service from resource IDs, portal
  URLs, error text, CLI output, or natural language **before** asking
  broad questions.
- Show only **relevant** Azure Support services and problem
  classifications, ranked with explanations.
- Use local caching so common interactions are fast.
- Require explicit user confirmation (review token + `confirmed: true`)
  before any state-changing call.
- Return ticket ID, status, portal link, and a shareable summary after
  creation/update.

### Non-goals

- No UI outside MCP-compatible clients.
- No Node.js or Python implementations.
- No hard dependency on Azure CLI (`az` is an optional fallback).
- No eager global download of all problem classifications.
- No Teams/Slack/email/GitHub posting as hard dependencies in the MVP.
- No silent ticket mutations without a reviewed draft.
- No transports other than stdio in MVP (core stays transport-agnostic).

---

## Quick start

### Prerequisites

- **Rust** stable (matches `edition = "2021"` in `Cargo.toml`).
- **Azure CLI** (`az`) for local testing of live tools. Sign in with `az login`.
- An MCP-capable client to test end-to-end (Copilot CLI, Claude Desktop, etc.).

### Build, run, test

```bash
cargo build --release
./target/release/azure-support-ticket-mcp doctor        # sanity check, no Azure call
cargo test --release --all-targets
```

`cargo install --path .` puts the binary in `~/.cargo/bin/` if you want
it on your `PATH` during development.

### Register your local build with Copilot CLI

```bash
copilot mcp add azure-support-ticket-mcp -- \
  "$(pwd)/target/release/azure-support-ticket-mcp" serve
```

End-users install via the Copilot CLI plugin (`copilot plugin install
artlovan/azure_support_ticket_mcp:plugin`) — that path is for releases,
not local dev.

### Reset all local state

```bash
rm -rf ~/.azure-support-ticket-mcp/
```

Wipes the SQLite cache and saved templates so you can verify
fresh-install behaviour.

---

## Common commands

Day-to-day commands you'll use while developing.

```bash
# Build
cargo build                       # debug build (fast)
cargo build --release             # release build (matches CI / publishing)

# Run the server directly (stdio MCP loop — Ctrl-C to stop)
cargo run --release -- serve

# Diagnostics (no Azure call, no auth required)
cargo run --release -- doctor

# Tests
cargo test                                    # all tests, debug
cargo test --release --all-targets            # release profile (matches CI)
cargo test <pattern>                          # run only matching tests
cargo test -- --nocapture                     # show println!/dbg! output

# Format
cargo fmt                         # format all sources
cargo fmt --check                 # verify formatting (what CI runs)

# Lint
cargo clippy --release --all-targets -- -D warnings   # matches CI

# Dependency hygiene
cargo update                      # bump Cargo.lock within semver constraints
cargo tree                        # inspect dependency graph
cargo audit --deny warnings       # check for CVEs (matches CI; install: cargo install cargo-audit)

# Clean rebuild
cargo clean && cargo build --release

# Refresh the embedded support-services catalog from live Azure (requires az login)
./scripts/refresh_support_services_seed.py --dry-run   # preview the diff
./scripts/refresh_support_services_seed.py             # write the new seed
# See data/README.md for the full workflow + hand-classification of new entries.

# Full pre-PR check (everything CI will run)
cargo fmt --check && \
  cargo clippy --release --all-targets -- -D warnings && \
  cargo test --release --all-targets && \
  cargo audit --deny warnings
```

---

## Before opening a PR

All four must pass locally:

```bash
cargo fmt --check
cargo clippy --release --all-targets -- -D warnings
cargo test --release --all-targets
cargo audit --deny warnings
```

If you've added a new MCP tool or changed schemas, also test the server
end-to-end against at least one MCP client.

The PR template (loaded automatically when you open a PR on GitHub) will
walk you through what else to fill in.

---

## Where things live

| Topic                                                 | File / location                                    |
| ----------------------------------------------------- | -------------------------------------------------- |
| System design (layers, contracts, security model)     | [`docs/ARCHITECTURE.md`](./docs/ARCHITECTURE.md)   |
| What's shipped and what's planned                     | [`docs/ROADMAP.md`](./docs/ROADMAP.md)             |
| Release process (versioning, pipeline, troubleshooting) | [`docs/RELEASING.md`](./docs/RELEASING.md)       |
| Engineering guardrails (Rust style, MCP tool contract, Azure rules, caching, naming, no-emoji) | [`.github/copilot-instructions.md`](./.github/copilot-instructions.md) |
| Refreshing the embedded Azure support-services catalog | [`data/README.md`](./data/README.md)               |
| Reporting a security vulnerability                    | Use [GitHub Security Advisories](https://github.com/artlovan/azure_support_ticket_mcp/security/advisories/new) (private) — do not file a public issue |
| Adding a new MCP tool — where the file goes           | [`docs/ARCHITECTURE.md`](./docs/ARCHITECTURE.md) → "Naming and filesystem layout" |
| End-user install + usage                              | [`README.md`](./README.md)                         |

`.github/copilot-instructions.md` is the authoritative source for
coding-style and Azure-integration rules — it's both read by AI agents
and the canonical reference for humans. Don't duplicate those rules here.

---

## Documentation rule of thumb

When your change requires a doc update, route it to the right file:

- **User-visible behaviour** (new feature, new install step, new tool, breaking change) → `README.md`
- **Architectural change** (new layer, new contract, security-model change) → `docs/ARCHITECTURE.md`
- **Slice marked complete, or a new slice added** → `docs/ROADMAP.md`
- **Release process change** (pipeline, version-bump steps, distribution channels) → `docs/RELEASING.md`
- **Engineering guardrail change** (style, naming, tool contract, caching rules) → `.github/copilot-instructions.md`
- **Contributor workflow change** (commands, pre-PR checks, scope) → this file

---

## Releasing

Releases are cut by pushing a `v*` git tag to `main`. The pipeline is
fully automated — see [`docs/RELEASING.md`](./docs/RELEASING.md) for the
6-step "Cutting a release" checklist.

---

## Be kind

Disagreements about technical direction are welcome; personal attacks
are not. Assume good faith, be specific, keep feedback actionable.
