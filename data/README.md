# `data/` — embedded reference data

This directory holds reference data that the MCP binary embeds at build
time via `include_bytes!`.

| File                          | Purpose                                                                       |
| ----------------------------- | ----------------------------------------------------------------------------- |
| `support_services_seed.json`  | Catalog of Azure support services + their resource-type mappings.             |
| `seed_schema.md`              | Schema documentation for `support_services_seed.json`.                        |

---

## Refreshing `support_services_seed.json`

The catalog is sourced from Azure's live `Microsoft.Support/services` REST
endpoint. Microsoft adds new services, renames existing ones (e.g. "OpenAI"
→ "Azure OpenAI", "Azure AI Foundry" → "Microsoft Foundry"), and removes
deprecated/duplicate entries over time. Periodically — or whenever a user
reports a missing/wrong service — refresh the seed.

### Prerequisites

- **`az` CLI** installed and logged in (`az login`). Any Azure account
  works; the Microsoft.Support API doesn't require a special role.
- **Python 3.7+** (standard library only — no `pip install` needed).

### Run

From the repository root:

```bash
# Show the diff without writing the file:
./scripts/refresh_support_services_seed.py --dry-run

# Fetch, merge, and overwrite data/support_services_seed.json:
./scripts/refresh_support_services_seed.py
```

The script prints a summary like:

```text
==> diff summary
    added (new in live):      30
    removed (gone from live): 106
    display-name changed:     8
    resource-types changed:   6
```

### What the script does

1. Calls `GET /providers/Microsoft.Support/services?api-version=2024-04-01`
   via `az rest` (reuses your `az login` session).
2. Merges with the existing seed:
   - Services in both → updated with live `display_name` and
     `resource_types`, but **keep their existing `group`** (a curated
     value that ARM does not return).
   - Services in live only → added with `group: null` (need manual
     classification — script prints them).
   - Services in current only → removed (deprecated/duplicates pruned
     by Microsoft).
3. Re-sorts by `(group, display_name)` for a stable, diff-friendly file.
4. Bumps `version` (e.g. `2024-04-01-1` → `2024-04-01-2`) and stamps the
   current UTC time into `generated_at`.
5. Writes atomically (`.json.tmp` then rename) so a partial write can't
   corrupt the file.

### After running: hand-classify NEW entries

New services land with `group: null` because Azure doesn't return a group
field. The script prints each one. Open
`data/support_services_seed.json` and set `"group"` on each to one of the
existing group values.

To list the unique groups currently in use:

```bash
jq -r '.services[].group | select(.)' data/support_services_seed.json | sort -u
```

Most new services map obviously by domain (e.g. "Billing" → `Billing & Subscription Management`,
"Azure Cosmos DB Fleet" → `Databases`). Leaving `group: null` is also fine
— the resolver still finds the service by name; it just can't rank by
group affinity until classified.

### After running: commit, build, test

```bash
cargo build --release
cargo test --release --all-targets   # the embedded bytes change, so rebuild is required
git add data/support_services_seed.json
git commit -m "Refresh support-services seed (v2024-04-01-<N>)"
```

### Why not run this automatically in CI?

We could add a scheduled GitHub Actions job that runs the script weekly
and opens a PR with the diff. That's worth doing once the project has
external users — for now (single-maintainer, pre-release), running the
script manually before each release is sufficient. See
[`docs/RELEASING.md`](../docs/RELEASING.md) → "Cutting a release" for
where this fits in the release checklist.
