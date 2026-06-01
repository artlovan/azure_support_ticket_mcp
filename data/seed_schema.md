# Support Services Seed Schema

This file documents the schema for `data/support_services_seed.json`, which is the
embedded fallback catalog of Azure Support services and their associated resource
types. The binary loads this seed into the SQLite cache (`support_services` table)
on first run and whenever the embedded version is newer than what's in the cache.

## Top-level object

```json
{
  "version": "2024-04-01-1",
  "generated_at": "ISO-8601 UTC timestamp",
  "source": "human-readable provenance",
  "services": [ ... ]
}
```

- `version` — opaque string. Compared against `seed_meta.version` in cache to
  decide whether to reload. Increment when the dataset changes.
- `generated_at` — ISO-8601 UTC timestamp of normalization.
- `source` — provenance string (e.g. dataset filename).
- `services` — array of service entries.

## Service entry

```json
{
  "service_id": "/providers/Microsoft.Support/services/<sid>",
  "name": "<sid>",
  "display_name": "Azure Kubernetes Service",
  "group": "Compute",
  "resource_types": ["Microsoft.ContainerService/managedClusters", "..."],
  "metadata": {}
}
```

- `service_id` — full Azure Support service ARN. Used as the primary key segment
  in `support_services` (along with `cloud`).
- `name` — bare service GUID/slug (last segment of `service_id`).
- `display_name` — human-readable label.
- `group` — high-level grouping (e.g. `Compute`, `Networking`).
- `resource_types` — list of ARM provider/type strings this service supports.
  Used by the resolver to map a resource to a candidate support service.
- `metadata` — reserved object for future use (e.g. severity hints, plan
  requirements). Always present, may be empty.

## Source

The seed is sourced from Azure's live Microsoft.Support REST API:

```
GET https://management.azure.com/providers/Microsoft.Support/services?api-version=2024-04-01
```

`display_name`, `name`, `service_id`, and `resource_types` come directly from
the API. `group` is a curated value (not returned by the API); it is
preserved across refreshes via merge — see `data/README.md` for the refresh
procedure.

### Historical source

The initial seed (v `2024-04-01-1`) was normalized from the
`azure-support-slack-bot` dataset (`dataset_services_mapped.json`). That
dataset itself was a snapshot of the same Microsoft.Support API, taken
manually. From v `2024-04-01-2` onward, the seed is refreshed directly
from the live API via `scripts/refresh_support_services_seed.py`.
