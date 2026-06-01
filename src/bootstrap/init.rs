//! `ensure_initialized()` — idempotent startup.
//!
//! Sets up the app directory, opens the SQLite cache, runs migrations,
//! loads the embedded seed if needed, and assembles a shared `AppState`
//! used by all MCP tools.

use std::sync::Arc;

use tracing::{debug, info};

use crate::cache::Cache;
use crate::config::Config;
use crate::error::{AppError, AppResult};
use crate::workflow::confirm::{ReviewTokens, SharedReviewTokens};
use crate::workflow::sanitize_tokens::{SanitizeTokens, SharedSanitizeTokens};
use crate::workflow::store::{MemoryDraftStore, SharedDraftStore};
use crate::workflow::templates::TemplateStore;

use super::seed;

/// Shared, cheaply-cloneable application state passed to tools.
#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub cache: Cache,
    pub seed_version: String,
    pub services_loaded: usize,
    pub drafts: SharedDraftStore,
    pub review_tokens: SharedReviewTokens,
    pub sanitize_tokens: SharedSanitizeTokens,
    pub templates: TemplateStore,
}

pub async fn ensure_initialized(config: &Config) -> AppResult<AppState> {
    let app_dir = config.app_dir();
    if !app_dir.exists() {
        std::fs::create_dir_all(&app_dir).map_err(|e| AppError::io(&app_dir, e))?;
        info!(path = %app_dir.display(), "created app directory");
    } else {
        debug!(path = %app_dir.display(), "app directory exists");
    }

    let cache = Cache::open(&config.cache.path, &config.general.cloud).await?;
    debug!(path = %config.cache.path.display(), "cache opened");

    let outcome = seed::load_into_cache_if_needed(&cache).await?;
    if outcome.reloaded {
        info!(version = %outcome.version, services = outcome.services_count, "seed loaded");
    }

    Ok(AppState {
        config: Arc::new(config.clone()),
        cache,
        seed_version: outcome.version,
        services_loaded: outcome.services_count,
        drafts: Arc::new(MemoryDraftStore::new()),
        review_tokens: Arc::new(ReviewTokens::new()),
        sanitize_tokens: Arc::new(SanitizeTokens::new()),
        templates: TemplateStore::new(&app_dir),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn ensure_initialized_creates_state() {
        let tmp = tempfile::tempdir().unwrap();
        let mut cfg = Config::default();
        cfg.cache.path = tmp.path().join("cache.sqlite");
        cfg.drafts.sqlite_path = tmp.path().join("drafts.sqlite");

        let state = ensure_initialized(&cfg).await.unwrap();
        assert!(state.services_loaded > 0);
        assert!(!state.seed_version.is_empty());

        // Second call is idempotent.
        let state2 = ensure_initialized(&cfg).await.unwrap();
        assert_eq!(state.services_loaded, state2.services_loaded);
    }
}
