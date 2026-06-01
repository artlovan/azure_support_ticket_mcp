//! SQLite-backed cache. Uses `sqlx` runtime-tokio with bundled-rustls.

use std::path::Path;
use std::str::FromStr;

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::SqlitePool;

use crate::error::{AppError, AppResult};

use super::models::{ProblemClassificationRow, SeedMeta, SupportServiceRow};
use super::refresh::SingleFlight;
use super::ttl::now_unix;

#[derive(Clone)]
pub struct Cache {
    pool: SqlitePool,
    flights: SingleFlight,
    cloud: String,
}

impl Cache {
    /// Open (creating if needed) the cache at `path`, run migrations,
    /// and bind operations to the given cloud namespace.
    pub async fn open(path: &Path, cloud: impl Into<String>) -> AppResult<Self> {
        if let Some(parent) = path.parent() {
            if !parent.exists() {
                std::fs::create_dir_all(parent).map_err(|e| AppError::io(parent, e))?;
            }
        }

        let url = format!("sqlite://{}", path.to_string_lossy());
        let opts = SqliteConnectOptions::from_str(&url)
            .map_err(|e| AppError::Internal(format!("sqlite options: {e}")))?
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal)
            .foreign_keys(true);

        let pool = SqlitePoolOptions::new()
            .max_connections(8)
            .connect_with(opts)
            .await?;

        let cache = Self {
            pool,
            flights: SingleFlight::new(),
            cloud: cloud.into(),
        };
        cache.migrate().await?;
        Ok(cache)
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    pub fn cloud(&self) -> &str {
        &self.cloud
    }

    pub fn flights(&self) -> SingleFlight {
        self.flights.clone()
    }

    async fn migrate(&self) -> AppResult<()> {
        // We embed migrations from the workspace `migrations/` folder at build time.
        sqlx::migrate!("./migrations").run(&self.pool).await?;
        Ok(())
    }

    // ---- support services ----

    pub async fn upsert_support_service(&self, row: &SupportServiceRow) -> AppResult<()> {
        sqlx::query(
            "INSERT INTO support_services
                (cloud, service_id, name, display_name, service_group,
                 resource_types_json, metadata_json, source, updated_at, etag)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(cloud, service_id) DO UPDATE SET
                name = excluded.name,
                display_name = excluded.display_name,
                service_group = excluded.service_group,
                resource_types_json = excluded.resource_types_json,
                metadata_json = excluded.metadata_json,
                source = excluded.source,
                updated_at = excluded.updated_at,
                etag = excluded.etag",
        )
        .bind(&row.cloud)
        .bind(&row.service_id)
        .bind(&row.name)
        .bind(&row.display_name)
        .bind(&row.service_group)
        .bind(&row.resource_types_json)
        .bind(&row.metadata_json)
        .bind(&row.source)
        .bind(row.updated_at)
        .bind(&row.etag)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Delete `support_services` rows that the embedded seed no longer
    /// contains.
    ///
    /// The seed loader calls this BEFORE upserting the new set, so an
    /// upgrade from a larger seed (e.g. 437 services with deprecated
    /// entries) to a smaller one (e.g. 349 after Microsoft pruned) actually
    /// shrinks the cache. Without this, `upsert_support_service` would
    /// leave deprecated rows in place forever and the resolver could
    /// surface dead service IDs to the user.
    ///
    /// Only deletes rows where `source = 'seed'` — never touches rows
    /// inserted from other sources (e.g. live ARM fetches by other tools).
    pub async fn delete_seed_services_not_in(&self, keep_ids: &[String]) -> AppResult<usize> {
        if keep_ids.is_empty() {
            // Refuse: never wipe the whole cache when given no IDs. This
            // would happen if the seed file accidentally landed empty.
            // Caller (seed loader) already validates non-empty before
            // calling; this is belt-and-suspenders.
            return Ok(0);
        }
        // SQLite supports up to SQLITE_MAX_VARIABLE_NUMBER (32766 default)
        // bound parameters per statement. Our seed has hundreds at most.
        let placeholders = std::iter::repeat_n("?", keep_ids.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "DELETE FROM support_services
             WHERE cloud = ?
               AND source = 'seed'
               AND service_id NOT IN ({placeholders})"
        );
        let mut q = sqlx::query(&sql).bind(&self.cloud);
        for id in keep_ids {
            q = q.bind(id);
        }
        let result = q.execute(&self.pool).await?;
        Ok(result.rows_affected() as usize)
    }

    pub async fn list_support_services(&self) -> AppResult<Vec<SupportServiceRow>> {
        let rows = sqlx::query_as::<_, SupportServiceRowDb>(
            "SELECT cloud, service_id, name, display_name, service_group,
                    resource_types_json, metadata_json, source, updated_at, etag
             FROM support_services WHERE cloud = ?
             ORDER BY service_group, display_name",
        )
        .bind(&self.cloud)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    pub async fn support_services_count(&self) -> AppResult<i64> {
        let n: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM support_services WHERE cloud = ?")
            .bind(&self.cloud)
            .fetch_one(&self.pool)
            .await?;
        Ok(n.0)
    }

    pub async fn list_classifications(
        &self,
        service_id: &str,
    ) -> AppResult<Vec<ProblemClassificationRow>> {
        let rows = sqlx::query_as::<_, ProblemClassificationRowDb>(
            "SELECT cloud, service_id, classification_id, display_name, parent_id,
                    metadata_json, updated_at, etag
             FROM problem_classifications WHERE cloud = ? AND service_id = ?
             ORDER BY display_name",
        )
        .bind(&self.cloud)
        .bind(service_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    pub async fn upsert_classification(&self, row: &ProblemClassificationRow) -> AppResult<()> {
        sqlx::query(
            "INSERT INTO problem_classifications
                (cloud, service_id, classification_id, display_name, parent_id,
                 metadata_json, updated_at, etag)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(cloud, service_id, classification_id) DO UPDATE SET
                display_name = excluded.display_name,
                parent_id = excluded.parent_id,
                metadata_json = excluded.metadata_json,
                updated_at = excluded.updated_at,
                etag = excluded.etag",
        )
        .bind(&row.cloud)
        .bind(&row.service_id)
        .bind(&row.classification_id)
        .bind(&row.display_name)
        .bind(&row.parent_id)
        .bind(&row.metadata_json)
        .bind(row.updated_at)
        .bind(&row.etag)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    // ---- ticket cache (write-through; opt-in read-through) ----

    #[allow(clippy::too_many_arguments)]
    pub async fn upsert_ticket_cache(&self, row: TicketCacheRow<'_>) -> AppResult<()> {
        sqlx::query(
            "INSERT INTO tickets_cache (subscription_id, ticket_name, support_ticket_id,
                tenant_id, title, severity, status, service_id, service_display_name,
                problem_classification_id, resource_id, created_date, modified_date,
                raw_json, cached_at, source)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(subscription_id, ticket_name) DO UPDATE SET
                support_ticket_id = COALESCE(excluded.support_ticket_id, tickets_cache.support_ticket_id),
                tenant_id = COALESCE(excluded.tenant_id, tickets_cache.tenant_id),
                title = COALESCE(excluded.title, tickets_cache.title),
                severity = COALESCE(excluded.severity, tickets_cache.severity),
                status = COALESCE(excluded.status, tickets_cache.status),
                service_id = COALESCE(excluded.service_id, tickets_cache.service_id),
                service_display_name = COALESCE(excluded.service_display_name, tickets_cache.service_display_name),
                problem_classification_id = COALESCE(excluded.problem_classification_id, tickets_cache.problem_classification_id),
                resource_id = COALESCE(excluded.resource_id, tickets_cache.resource_id),
                created_date = COALESCE(excluded.created_date, tickets_cache.created_date),
                modified_date = COALESCE(excluded.modified_date, tickets_cache.modified_date),
                raw_json = excluded.raw_json,
                cached_at = excluded.cached_at,
                source = excluded.source",
        )
        .bind(row.subscription_id)
        .bind(row.ticket_name)
        .bind(row.support_ticket_id)
        .bind(row.tenant_id)
        .bind(row.title)
        .bind(row.severity)
        .bind(row.status)
        .bind(row.service_id)
        .bind(row.service_display_name)
        .bind(row.problem_classification_id)
        .bind(row.resource_id)
        .bind(row.created_date)
        .bind(row.modified_date)
        .bind(row.raw_json)
        .bind(now_unix())
        .bind(row.source)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn get_ticket_cache(
        &self,
        subscription_id: &str,
        ticket_name: &str,
    ) -> AppResult<Option<TicketCacheEntry>> {
        let row: Option<TicketCacheRowDb> = sqlx::query_as(
            "SELECT subscription_id, ticket_name, support_ticket_id, tenant_id, title, severity,
                    status, service_id, service_display_name, problem_classification_id,
                    resource_id, created_date, modified_date, raw_json, cached_at, source
             FROM tickets_cache WHERE subscription_id = ? AND ticket_name = ?",
        )
        .bind(subscription_id)
        .bind(ticket_name)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(Into::into))
    }

    pub async fn list_recent_tickets_cache(
        &self,
        subscription_id: &str,
        limit: i64,
    ) -> AppResult<Vec<TicketCacheEntry>> {
        let rows: Vec<TicketCacheRowDb> = sqlx::query_as(
            "SELECT subscription_id, ticket_name, support_ticket_id, tenant_id, title, severity,
                    status, service_id, service_display_name, problem_classification_id,
                    resource_id, created_date, modified_date, raw_json, cached_at, source
             FROM tickets_cache WHERE subscription_id = ?
             ORDER BY COALESCE(modified_date, created_date, '') DESC, cached_at DESC
             LIMIT ?",
        )
        .bind(subscription_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    // ---- refresh state ----

    pub async fn refresh_state_last_success(&self, key: &str) -> AppResult<Option<i64>> {
        let row: Option<(Option<i64>,)> =
            sqlx::query_as("SELECT last_success_at FROM cache_refresh_state WHERE cache_key = ?")
                .bind(key)
                .fetch_optional(&self.pool)
                .await?;
        Ok(row.and_then(|r| r.0))
    }

    pub async fn record_refresh_success(&self, key: &str) -> AppResult<()> {
        let now = now_unix();
        sqlx::query(
            "INSERT INTO cache_refresh_state
                (cache_key, last_attempt_at, last_success_at, last_error, refresh_in_progress)
             VALUES (?, ?, ?, NULL, 0)
             ON CONFLICT(cache_key) DO UPDATE SET
                last_attempt_at = excluded.last_attempt_at,
                last_success_at = excluded.last_success_at,
                last_error = NULL,
                refresh_in_progress = 0",
        )
        .bind(key)
        .bind(now)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn record_refresh_error(&self, key: &str, err: &str) -> AppResult<()> {
        let now = now_unix();
        sqlx::query(
            "INSERT INTO cache_refresh_state
                (cache_key, last_attempt_at, last_success_at, last_error, refresh_in_progress)
             VALUES (?, ?, NULL, ?, 0)
             ON CONFLICT(cache_key) DO UPDATE SET
                last_attempt_at = excluded.last_attempt_at,
                last_error = excluded.last_error,
                refresh_in_progress = 0",
        )
        .bind(key)
        .bind(now)
        .bind(err)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    // ---- seed meta ----

    pub async fn put_seed_meta(&self, key: &str, value: &str) -> AppResult<()> {
        sqlx::query(
            "INSERT INTO seed_meta(key, value) VALUES(?, ?)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        )
        .bind(key)
        .bind(value)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn get_seed_meta(&self, key: &str) -> AppResult<Option<String>> {
        let row: Option<(String,)> = sqlx::query_as("SELECT value FROM seed_meta WHERE key = ?")
            .bind(key)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|r| r.0))
    }

    pub async fn seed_meta(&self) -> AppResult<SeedMeta> {
        Ok(SeedMeta {
            version: self.get_seed_meta("version").await?,
            sha256: self.get_seed_meta("sha256").await?,
            source: self.get_seed_meta("source").await?,
            loaded_at: self
                .get_seed_meta("loaded_at")
                .await?
                .and_then(|s| s.parse().ok()),
        })
    }
}

#[derive(sqlx::FromRow)]
struct SupportServiceRowDb {
    cloud: String,
    service_id: String,
    name: String,
    display_name: String,
    service_group: Option<String>,
    resource_types_json: Option<String>,
    metadata_json: Option<String>,
    source: String,
    updated_at: i64,
    etag: Option<String>,
}

impl From<SupportServiceRowDb> for SupportServiceRow {
    fn from(r: SupportServiceRowDb) -> Self {
        Self {
            cloud: r.cloud,
            service_id: r.service_id,
            name: r.name,
            display_name: r.display_name,
            service_group: r.service_group,
            resource_types_json: r.resource_types_json,
            metadata_json: r.metadata_json,
            source: r.source,
            updated_at: r.updated_at,
            etag: r.etag,
        }
    }
}

#[derive(sqlx::FromRow)]
struct ProblemClassificationRowDb {
    cloud: String,
    service_id: String,
    classification_id: String,
    display_name: String,
    parent_id: Option<String>,
    metadata_json: Option<String>,
    updated_at: i64,
    etag: Option<String>,
}

impl From<ProblemClassificationRowDb> for ProblemClassificationRow {
    fn from(r: ProblemClassificationRowDb) -> Self {
        Self {
            cloud: r.cloud,
            service_id: r.service_id,
            classification_id: r.classification_id,
            display_name: r.display_name,
            parent_id: r.parent_id,
            metadata_json: r.metadata_json,
            updated_at: r.updated_at,
            etag: r.etag,
        }
    }
}

/// Convenience for tests / doctor.
#[cfg(test)]
pub async fn open_in_temp_dir(
    cloud: &str,
) -> AppResult<(Cache, std::path::PathBuf, tempfile::TempDir)> {
    let dir = tempfile::tempdir().map_err(AppError::io_no_path)?;
    let path = dir.path().join("cache.sqlite");
    let cache = Cache::open(&path, cloud).await?;
    Ok((cache, path, dir))
}

// ---- ticket cache row types ----

/// Borrowed insert/update payload for `tickets_cache`.
#[derive(Debug, Clone, Copy)]
pub struct TicketCacheRow<'a> {
    pub subscription_id: &'a str,
    pub ticket_name: &'a str,
    pub support_ticket_id: Option<&'a str>,
    pub tenant_id: Option<&'a str>,
    pub title: Option<&'a str>,
    pub severity: Option<&'a str>,
    pub status: Option<&'a str>,
    pub service_id: Option<&'a str>,
    pub service_display_name: Option<&'a str>,
    pub problem_classification_id: Option<&'a str>,
    pub resource_id: Option<&'a str>,
    pub created_date: Option<&'a str>,
    pub modified_date: Option<&'a str>,
    pub raw_json: &'a str,
    pub source: &'a str,
}

/// Owned read-back of a `tickets_cache` row.
#[derive(Debug, Clone)]
pub struct TicketCacheEntry {
    pub subscription_id: String,
    pub ticket_name: String,
    pub support_ticket_id: Option<String>,
    pub tenant_id: Option<String>,
    pub title: Option<String>,
    pub severity: Option<String>,
    pub status: Option<String>,
    pub service_id: Option<String>,
    pub service_display_name: Option<String>,
    pub problem_classification_id: Option<String>,
    pub resource_id: Option<String>,
    pub created_date: Option<String>,
    pub modified_date: Option<String>,
    pub raw_json: String,
    pub cached_at: i64,
    pub source: String,
}

#[derive(sqlx::FromRow)]
struct TicketCacheRowDb {
    subscription_id: String,
    ticket_name: String,
    support_ticket_id: Option<String>,
    tenant_id: Option<String>,
    title: Option<String>,
    severity: Option<String>,
    status: Option<String>,
    service_id: Option<String>,
    service_display_name: Option<String>,
    problem_classification_id: Option<String>,
    resource_id: Option<String>,
    created_date: Option<String>,
    modified_date: Option<String>,
    raw_json: String,
    cached_at: i64,
    source: String,
}

impl From<TicketCacheRowDb> for TicketCacheEntry {
    fn from(r: TicketCacheRowDb) -> Self {
        Self {
            subscription_id: r.subscription_id,
            ticket_name: r.ticket_name,
            support_ticket_id: r.support_ticket_id,
            tenant_id: r.tenant_id,
            title: r.title,
            severity: r.severity,
            status: r.status,
            service_id: r.service_id,
            service_display_name: r.service_display_name,
            problem_classification_id: r.problem_classification_id,
            resource_id: r.resource_id,
            created_date: r.created_date,
            modified_date: r.modified_date,
            raw_json: r.raw_json,
            cached_at: r.cached_at,
            source: r.source,
        }
    }
}
