<!--
Thanks for the PR! Please fill out the sections below so review goes
smoothly. Delete sections that don't apply.
-->

## What changed

<!-- One or two sentences describing the change. -->

## Why

<!-- The user-facing problem this solves, the bug this fixes, or the
     capability this adds. Link to the issue if there is one
     (e.g. "Closes #123"). -->

## How it was tested

<!--
Check the boxes that apply, then describe anything tested manually.
-->

- [ ] `cargo fmt --check` passes
- [ ] `cargo clippy --release --all-targets -- -D warnings` passes
- [ ] `cargo test --release --all-targets` passes
- [ ] `cargo audit --deny warnings` passes
- [ ] Manually tested end-to-end against an MCP client (e.g. Copilot CLI)
- [ ] N/A — docs-only / refactor-only change

Manual testing notes:

## User-visible behaviour change?

<!-- Yes / No. If yes, describe what users will see differently. This
     is what shows up in release notes — be specific. -->

## Documentation

<!-- Check what was updated. -->

- [ ] `README.md` (end-user facing)
- [ ] `CONTRIBUTING.md` (contributor-facing rules or workflows)
- [ ] `docs/ARCHITECTURE.md` (architectural change)
- [ ] `docs/ROADMAP.md` (slice marked complete or new slice added)
- [ ] `docs/RELEASING.md` (release process change)
- [ ] `.github/copilot-instructions.md` (engineering guardrails change)
- [ ] N/A
