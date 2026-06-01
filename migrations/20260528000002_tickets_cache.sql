-- Local cache of tickets we've authored / touched (via create/update/reply).
-- Write-through: rows are inserted/updated on successful Azure write, plus
-- (opt-in) on read-through via prefer_local_cache. We never lazily fetch
-- from this table without the caller asking; status & communications are
-- always stale risk so reads must opt in.
CREATE TABLE IF NOT EXISTS tickets_cache (
  subscription_id TEXT NOT NULL,
  ticket_name TEXT NOT NULL,
  support_ticket_id TEXT,
  tenant_id TEXT,
  title TEXT,
  severity TEXT,
  status TEXT,
  service_id TEXT,
  service_display_name TEXT,
  problem_classification_id TEXT,
  resource_id TEXT,
  created_date TEXT,
  modified_date TEXT,
  -- Full ARM response (or our locally-built equivalent) so get_support_ticket
  -- can return identical shape without round-tripping Azure.
  raw_json TEXT NOT NULL,
  -- Local wall-clock when this row was last refreshed (write or read-through).
  cached_at INTEGER NOT NULL,
  -- Where the cached row came from: 'create' | 'update' | 'reply' | 'get' | 'list'.
  source TEXT NOT NULL,
  PRIMARY KEY (subscription_id, ticket_name)
);
CREATE INDEX IF NOT EXISTS idx_tickets_cache_recent
  ON tickets_cache(subscription_id, cached_at DESC);
CREATE INDEX IF NOT EXISTS idx_tickets_cache_status
  ON tickets_cache(subscription_id, status);
