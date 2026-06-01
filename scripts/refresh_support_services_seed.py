#!/usr/bin/env python3
"""
refresh_support_services_seed.py — fetch the latest Azure support-services
catalog from the live ARM REST API and merge it into
`data/support_services_seed.json`.

WHAT IT DOES
------------
1. Calls `GET /providers/Microsoft.Support/services?api-version=2024-04-01`
   via `az rest` (so it reuses your current `az login` session — no separate
   auth setup).
2. Normalizes each entry into the seed schema (see `data/seed_schema.md`).
3. Merges with the existing seed:
   - Services present in BOTH → updated with live `display_name` and
     `resource_types`, but keep their existing `group` (a curated value that
     ARM does not return).
   - Services in LIVE only → added with `group: null` (will need manual
     classification — the script prints them so you can edit afterward).
   - Services in CURRENT only → removed (these are deprecated/duplicates
     that Microsoft has pruned from the catalog).
4. Re-sorts by `(group, display_name)` to keep the file diff-friendly.
5. Bumps `version` (suffix `-N+1` of the current version) and stamps the
   current UTC time into `generated_at`.
6. Writes the new file in place over `data/support_services_seed.json`.
7. Prints a diff summary so you can see what changed.

PREREQUISITES
-------------
- `az` CLI installed and logged in (`az login`). The Microsoft.Support API
  doesn't require any special role — any account with access to ANY Azure
  subscription can read it.
- Python 3.7+ (standard library only — no `pip install` needed).

USAGE
-----
    # From the repository root:
    ./scripts/refresh_support_services_seed.py

    # Or with an explicit project root (e.g. from elsewhere):
    ./scripts/refresh_support_services_seed.py --root /path/to/repo

    # Dry-run — print the diff but DON'T overwrite the file:
    ./scripts/refresh_support_services_seed.py --dry-run

EXIT CODES
----------
- 0 — success (file written, or dry-run completed cleanly).
- 1 — operational error (az not found, az not logged in, API call failed,
      file write failed). Existing seed is left untouched on any error.

NEW ENTRIES WITHOUT A GROUP
---------------------------
Services that ARM returns but didn't exist in the previous seed land with
`group: null`. The script prints them at the end so you can hand-classify.
Open `data/support_services_seed.json` and set the `"group"` field on each
new entry to one of the existing group strings (grep for unique groups:
`jq -r '.services[].group | select(.)' data/support_services_seed.json | \\
 sort -u`).

Leaving `group: null` is also fine — the resolver just won't be able to
rank that service by group affinity until you classify it.
"""

from __future__ import annotations

import argparse
import json
import re
import shutil
import subprocess
import sys
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

API_VERSION = "2024-04-01"
ARM_URI = (
    f"https://management.azure.com/providers/Microsoft.Support/services"
    f"?api-version={API_VERSION}"
)
SEED_REL_PATH = Path("data") / "support_services_seed.json"


def die(msg: str, code: int = 1) -> None:
    """Print to stderr and exit with the given code. The existing seed file
    is never modified on this path."""
    print(f"error: {msg}", file=sys.stderr)
    sys.exit(code)


def ensure_az_available() -> None:
    """Refuse to proceed if `az` is not on PATH. The user might have a
    service-principal env-var setup, but this script intentionally only
    supports `az` for simplicity — matches how a contributor would normally
    have local Azure access set up."""
    if shutil.which("az") is None:
        die(
            "`az` CLI not found on PATH. Install it from "
            "https://learn.microsoft.com/cli/azure/install-azure-cli "
            "and run `az login` before re-running this script."
        )


def fetch_live_services() -> list[dict[str, Any]]:
    """Hit ARM via `az rest`. Returns the parsed `value` array from the
    response, or exits with a clear error if the call fails."""
    try:
        result = subprocess.run(
            ["az", "rest", "--method", "get", "--uri", ARM_URI],
            capture_output=True,
            text=True,
            check=False,
        )
    except FileNotFoundError:
        die("`az` not executable; ensure it's installed and on PATH.")

    if result.returncode != 0:
        die(
            f"`az rest` failed (exit {result.returncode}).\n"
            f"stderr: {result.stderr.strip()}\n"
            f"Most often this means you need to run `az login` first."
        )

    try:
        body = json.loads(result.stdout)
    except json.JSONDecodeError as e:
        die(f"`az rest` returned non-JSON output: {e}")

    services = body.get("value")
    if not isinstance(services, list):
        die(f"Unexpected response shape: missing `value` array. Got keys: {list(body.keys())}")

    if not services:
        die("ARM returned zero services. Refusing to overwrite the seed with nothing.")

    return services


def normalize(live: dict[str, Any]) -> dict[str, Any]:
    """Convert one ARM response entry into the seed schema. `group` is
    left as None — callers merge it in from the existing seed when known."""
    props = live.get("properties") or {}
    return {
        "service_id": live["id"],
        "name": live["name"],
        "display_name": props.get("displayName"),
        "group": None,
        "resource_types": props.get("resourceTypes", []),
        "metadata": {},
    }


def bump_version(current: str) -> str:
    """Bump the integer suffix after the last `-`. `2024-04-01-1` → `2024-04-01-2`.
    If the current version doesn't end with `-<int>`, append `-1`. Date prefix
    is preserved verbatim so the version sorts in publication order."""
    m = re.match(r"^(.*)-(\d+)$", current)
    if m:
        base, n = m.group(1), int(m.group(2))
        return f"{base}-{n + 1}"
    return f"{current}-1"


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Refresh data/support_services_seed.json from live Azure ARM."
    )
    parser.add_argument(
        "--root",
        type=Path,
        default=Path(__file__).resolve().parent.parent,
        help="Repository root (defaults to the parent of this script's directory).",
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="Print the diff but DON'T overwrite the seed file.",
    )
    args = parser.parse_args()

    seed_path: Path = args.root / SEED_REL_PATH
    if not seed_path.exists():
        die(f"seed file not found at {seed_path} — wrong --root?")

    ensure_az_available()

    print(f"==> reading current seed:  {seed_path}")
    with seed_path.open() as f:
        current = json.load(f)
    current_by_id: dict[str, dict[str, Any]] = {
        s["service_id"]: s for s in current["services"]
    }
    print(f"    current version:       {current['version']}")
    print(f"    current service count: {len(current_by_id)}")

    print(f"==> fetching live from:    {ARM_URI}")
    live_services = fetch_live_services()
    print(f"    live service count:    {len(live_services)}")

    # --- Merge --------------------------------------------------------------

    merged: list[dict[str, Any]] = []
    added_ids: list[str] = []
    display_changes: list[tuple[str, str, str]] = []
    restype_changes: list[tuple[str, list[str], list[str]]] = []

    for live in live_services:
        normalized = normalize(live)
        sid = normalized["service_id"]
        existing = current_by_id.get(sid)
        if existing:
            # Preserve group from current (ARM doesn't return it).
            normalized["group"] = existing.get("group")
            # Detect drift worth reporting.
            if normalized["display_name"] != existing.get("display_name"):
                display_changes.append(
                    (sid, existing.get("display_name") or "", normalized["display_name"] or "")
                )
            if sorted(normalized["resource_types"]) != sorted(existing.get("resource_types", [])):
                restype_changes.append(
                    (sid, existing.get("resource_types", []), normalized["resource_types"])
                )
        else:
            added_ids.append(sid)
        merged.append(normalized)

    live_ids = {s["id"] for s in live_services}
    removed_ids = sorted(set(current_by_id) - live_ids)

    # Keep file diff-friendly: sort by (group, display_name). Entries with
    # no group sort last (zzz pseudo-group).
    merged.sort(key=lambda x: (x["group"] or "zzz", x["display_name"] or ""))

    new_seed = {
        "version": bump_version(current["version"]),
        "generated_at": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "source": (
            "Azure Microsoft.Support/services REST API "
            f"(api-version {API_VERSION}); groups merged from previous seed where known"
        ),
        "services": merged,
    }

    # --- Report ------------------------------------------------------------

    print()
    print("==> diff summary")
    print(f"    added (new in live):      {len(added_ids)}")
    print(f"    removed (gone from live): {len(removed_ids)}")
    print(f"    display-name changed:     {len(display_changes)}")
    print(f"    resource-types changed:   {len(restype_changes)}")
    print()

    if added_ids:
        added_names = [
            (sid, normalize(next(s for s in live_services if s["id"] == sid))["display_name"])
            for sid in added_ids
        ]
        print(f"==> NEW services (group: null — please hand-classify in the file):")
        for _, name in added_names:
            print(f"    + {name}")
        print()

    if removed_ids:
        print(f"==> REMOVED services (deprecated/pruned by Microsoft):")
        for sid in removed_ids[:25]:
            print(f"    - {current_by_id[sid].get('display_name', sid)}")
        if len(removed_ids) > 25:
            print(f"    ... and {len(removed_ids) - 25} more")
        print()

    if display_changes:
        print("==> RENAMED services:")
        for _, old, new in display_changes:
            print(f"    {old!r}")
            print(f"      -> {new!r}")
        print()

    # --- Write or dry-run --------------------------------------------------

    if args.dry_run:
        print("==> --dry-run: NOT writing the file.")
        return 0

    if not (added_ids or removed_ids or display_changes or restype_changes):
        print("==> no changes detected; seed file is already current.")
        return 0

    # Atomic write: stage to .tmp then rename, so a partial write can't
    # corrupt the file if the process is killed mid-flight.
    tmp_path = seed_path.with_suffix(".json.tmp")
    with tmp_path.open("w", encoding="utf-8") as f:
        json.dump(new_seed, f, indent=2, ensure_ascii=False)
        f.write("\n")
    tmp_path.replace(seed_path)

    print(f"==> wrote: {seed_path}")
    print(f"    new version:           {new_seed['version']}")
    print(f"    new service count:     {len(merged)}")
    if added_ids:
        print(
            f"    ACTION REQUIRED: {len(added_ids)} new entries have "
            f"group: null. Hand-classify and commit."
        )
    return 0


if __name__ == "__main__":
    sys.exit(main())
