-- Initial schema for azure-support-ticket-mcp local cache.
CREATE TABLE IF NOT EXISTS support_services (
  cloud TEXT NOT NULL,
  service_id TEXT NOT NULL,
  name TEXT NOT NULL,
  display_name TEXT NOT NULL,
  service_group TEXT,
  resource_types_json TEXT,
  metadata_json TEXT,
  source TEXT NOT NULL,
  updated_at INTEGER NOT NULL,
  etag TEXT,
  PRIMARY KEY (cloud, service_id)
);
CREATE INDEX IF NOT EXISTS idx_support_services_group ON support_services(cloud, service_group);
CREATE INDEX IF NOT EXISTS idx_support_services_name ON support_services(cloud, display_name);

CREATE TABLE IF NOT EXISTS problem_classifications (
  cloud TEXT NOT NULL,
  service_id TEXT NOT NULL,
  classification_id TEXT NOT NULL,
  display_name TEXT NOT NULL,
  parent_id TEXT,
  metadata_json TEXT,
  updated_at INTEGER NOT NULL,
  etag TEXT,
  PRIMARY KEY (cloud, service_id, classification_id)
);

CREATE TABLE IF NOT EXISTS tenants (
  account_id TEXT NOT NULL,
  tenant_id TEXT NOT NULL,
  display_name TEXT,
  updated_at INTEGER NOT NULL,
  PRIMARY KEY (account_id, tenant_id)
);

CREATE TABLE IF NOT EXISTS subscriptions (
  tenant_id TEXT NOT NULL,
  subscription_id TEXT NOT NULL,
  display_name TEXT NOT NULL,
  state TEXT,
  updated_at INTEGER NOT NULL,
  PRIMARY KEY (tenant_id, subscription_id)
);

CREATE TABLE IF NOT EXISTS resource_inventory (
  subscription_id TEXT NOT NULL,
  resource_id TEXT NOT NULL,
  name TEXT NOT NULL,
  type TEXT NOT NULL,
  location TEXT,
  resource_group TEXT,
  updated_at INTEGER NOT NULL,
  PRIMARY KEY (subscription_id, resource_id)
);

CREATE TABLE IF NOT EXISTS cache_refresh_state (
  cache_key TEXT PRIMARY KEY,
  last_attempt_at INTEGER,
  last_success_at INTEGER,
  last_error TEXT,
  refresh_in_progress INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS seed_meta (
  key TEXT PRIMARY KEY,
  value TEXT NOT NULL
);
