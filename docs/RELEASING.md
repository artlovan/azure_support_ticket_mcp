# Releasing

How releases of `azure-support-ticket-mcp` are built, published, and
distributed. This document is the authoritative source for the release
process.

---

## Overview

A release of `azure-support-ticket-mcp` is a **GitHub Release** that
publishes:

1. **Per-platform binaries** for the five supported targets (one file each).
2. **SHA256 checksum sidecars** (`.sha256`) for each binary.
3. The **install scripts** (`install.sh` and `install.ps1`) that end-users
   pipe into `sh` / `iex`.

Everything is built by GitHub Actions when a `v*` git tag is pushed to
`main`. There are **no manual steps** between pushing the tag and the
release being live — no local builds, no manual asset uploads.

End-users install via:

```bash
curl -sSL https://github.com/artlovan/azure_support_ticket_mcp/releases/latest/download/install.sh | sh
```

…then register the MCP with Copilot CLI (see `README.md` → Quick start).

---

## Supported target platforms

Each release builds and publishes binaries for all five targets:

| Target triple                  | Asset name                                | Runner       |
| ------------------------------ | ----------------------------------------- | ------------ |
| `x86_64-unknown-linux-gnu`     | `azure-support-ticket-mcp-linux-x86_64`   | `ubuntu-latest` |
| `aarch64-unknown-linux-gnu`    | `azure-support-ticket-mcp-linux-aarch64`  | `ubuntu-latest` (via `cross`) |
| `x86_64-apple-darwin`          | `azure-support-ticket-mcp-darwin-x86_64`  | `macos-latest` |
| `aarch64-apple-darwin`         | `azure-support-ticket-mcp-darwin-aarch64` | `macos-latest` |
| `x86_64-pc-windows-msvc`       | `azure-support-ticket-mcp-windows-x86_64.exe` | `windows-latest` |

Linux builds target **glibc** (not musl). musl/Alpine is not supported in
v1; see `docs/ROADMAP.md` if you need it.

---

## Versioning

The project follows **Semantic Versioning 2.0.0** (`MAJOR.MINOR.PATCH`):

- **MAJOR** — breaking changes to MCP tool names, schemas, config keys, or
  required environment variables. Any change that breaks an existing
  end-user's setup is MAJOR.
- **MINOR** — new MCP tools, new optional fields, new config options,
  backwards-compatible behaviour changes.
- **PATCH** — bug fixes, documentation, performance improvements,
  dependency bumps with no behaviour change.

`0.x` versions: anything goes — breaking changes allowed in MINOR bumps
until the first `1.0.0`.

> Pre-release tags (`v1.2.0-rc.1`, `v1.2.0-beta.1`, etc.) are supported by
> the release workflow — any tag containing `-` is auto-marked as a
> "Pre-release" GitHub Release and is not picked up by
> `releases/latest/download/...` URLs. They are not part of the regular
> release cadence; use them only when there's a concrete reason (e.g.
> external beta testers).

---

## Cutting a release

> **The version contract.** Every release is identified by exactly one version
> string that must appear identically in **three places**:
>
> 1. `Cargo.toml` → `version = "X.Y.Z"`
> 2. `plugin/plugin.json` → `"version": "X.Y.Z"`
> 3. The git tag pushed to GitHub → `vX.Y.Z` (with the `v` prefix)
>
> CI enforces this two ways:
> - `ci.yml` fails any PR where (1) and (2) disagree.
> - `release.yml` fails fast if the pushed tag doesn't match (1) and (2) —
>   the build matrix won't even start. You cannot ship a mismatched release.

1. **Land all work in `main`.** Every release ships exclusively from `main`
   — no release branches in v1.

2. **Refresh the embedded support-services catalog (optional but recommended).**

   ```bash
   ./scripts/refresh_support_services_seed.py --dry-run   # preview
   ./scripts/refresh_support_services_seed.py             # apply
   ```

   If the diff is non-empty, hand-classify any new entries (the script
   prints them), then land that change as its own PR before the release
   PR. See [`data/README.md`](../data/README.md) for the full procedure.

3. **Bump `version` in `Cargo.toml` AND in `plugin/plugin.json`** to the
   same value (see the version contract above). The plugin version tracks
   the binary version 1:1 so users can see at a glance which release they
   have installed (`copilot plugin list`).

4. **Open a PR titled `Release v1.2.3`** containing only the `Cargo.toml`
   + `plugin/plugin.json` version bumps. Merge once CI is green.

5. **Tag the merge commit:**

   ```bash
   git checkout main
   git pull
   git tag -a v1.2.3 -m "v1.2.3"
   git push origin v1.2.3
   ```

6. **Wait ~10 minutes** for the release workflow to finish. Watch it at
   `https://github.com/artlovan/azure_support_ticket_mcp/actions`. On
   success the GitHub Release will be published with all assets attached
   and release notes auto-generated from merged-PR titles since the
   previous tag.

7. **Verify the install:**

   ```bash
   curl -sSL https://github.com/artlovan/azure_support_ticket_mcp/releases/download/v1.2.3/install.sh | sh
   azure-support-ticket-mcp doctor
   ```

   `doctor` should report the embedded seed loaded and ARM reachable.

---

## Pipeline anatomy

Two workflows live under `.github/workflows/`:

### `ci.yml` — pull-request gate

- **Trigger:** push to `main`, pull requests to `main`.
- **Runs on:** `ubuntu-latest`.
- **Does:**
  - `test` job — `cargo fmt --check`, `cargo clippy --release --all-targets -- -D warnings`, `cargo test --release --all-targets`.
  - `audit` job — `cargo audit --deny warnings` (RustSec advisory check). Marked `continue-on-error: true` so a transient advisory-DB hiccup doesn't block merges; real CVEs are visible in the Checks tab.
- **Does not:** build release binaries, publish anything.

This is what gates PR merges. Fast feedback loop (~3–5 minutes).

Dependency hygiene is automated separately by Dependabot (see
`.github/dependabot.yml`) — weekly Cargo + GitHub-Actions update PRs,
minor/patch bumps grouped to keep PR noise low.

### `release.yml` — tag publisher

- **Trigger:** push of a tag matching `v*`.
- **Permissions:** `contents: write` (needed to create the GitHub Release).
- **Job 1 (`build`):** matrix across five targets. Each job:
  1. Checks out the source.
  2. Installs the Rust toolchain and the matching target triple.
  3. Restores the Cargo build cache.
  4. Builds with `cargo build --release --target <triple>` (or `cross
     build` for `linux-aarch64-gnu`).
  5. Renames the produced binary to its release-asset name.
  6. Uploads it as a workflow artifact.
- **Job 2 (`release`):** runs after all matrix jobs succeed.
  1. Downloads every workflow artifact.
  2. Computes a `.sha256` sidecar for each binary.
  3. Adds `scripts/install.sh` and `scripts/install.ps1` from the checkout
     so they ship as part of the release.
  4. Creates (or updates) the GitHub Release for the pushed tag using
     `softprops/action-gh-release`, attaching every binary, every
     checksum, and both install scripts. Release notes are auto-generated
     from the merged-PR titles since the previous tag (no hand-curated
     `CHANGELOG.md` is maintained in v1 — keep PR titles descriptive).

---

## Distribution channels (current and planned)

### Current (v1)

- **`install.sh` / `install.ps1`** — pipe-to-shell install. Detects OS and
  arch, downloads the matching binary, verifies the SHA256, installs it
  under the user's home directory.
- **Raw release assets** — every per-platform binary is published as a
  GitHub Release asset for users who prefer not to pipe a shell script.
- **Copilot CLI plugin** — `copilot plugin install artlovan/azure_support_ticket_mcp:plugin`
  registers the MCP launcher in one command. The plugin lives in
  `plugin/` (manifest + `.mcp.json`); it references the binary installed
  by `install.sh` / `install.ps1` and assumes `azure-support-ticket-mcp`
  is on `PATH`. The plugin's `version` in `plugin/plugin.json` is bumped
  alongside `Cargo.toml` (see Cutting a release, step 2).

### Planned (post-v1)

Tracked in `docs/ROADMAP.md` → "Distribution channels (post-v1)":

- Homebrew tap
- Scoop / Winget (Windows)
- `cargo install azure-support-ticket-mcp` (publish to crates.io)

---

## Troubleshooting

### A matrix job fails

The release job depends on **all** matrix jobs succeeding. If one target
fails:

1. The release is not published.
2. The pushed tag is still in git. **Do not push the same tag again** —
   delete it and start over:

   ```bash
   git tag -d v1.2.3                 # delete locally
   git push --delete origin v1.2.3   # delete on remote
   ```

3. Fix the issue, bump to `v1.2.4` (don't reuse the broken version), and
   re-tag from `main`.

### `linux-aarch64-gnu` build is slow or fails

The aarch64 Linux build uses `cross` (Docker-based cross-compilation). If
Docker Hub is rate-limiting or the cross image is stale, the job can fail
intermittently. Re-run the failed job once before debugging deeper.

### Release notes are wrong / empty

The release-notes step generates notes from merged-PR titles since the
previous tag. If the notes look wrong:

1. Edit the GitHub Release manually (the assets are independent).
2. Fix the underlying cause for next time — usually a non-descriptive PR
   title. Keep PR titles imperative and specific
   ("Add tenant backfill warning" beats "fixes").

---

## Rust toolchain pinning

The project does **not** pin a `rust-toolchain.toml` in v1 — CI and the
release pipeline both use latest stable. If a regression in stable breaks
the build, pin temporarily by adding `rust-toolchain.toml`:

```toml
[toolchain]
channel = "1.83.0"
```

…and remove it once the regression is resolved upstream.
