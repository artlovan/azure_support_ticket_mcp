//! `get_support_ticket`: full body of a single ticket. Optional cache-first
//! read keyed on subscription + ticket name, falling back to Azure.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::azure::support::tickets::get_ticket;
use crate::bootstrap::AppState;
use crate::error::{AppError, AppResult};

#[derive(Debug, Deserialize, JsonSchema)]
pub struct Input {
    pub subscription_id: String,
    pub ticket_name: String,
    /// Serve from local SQLite cache when present (written through on
    /// create/update). Default false — Azure is source of truth. Set to
    /// `true` for fast read-back of recently-authored fields when status
    /// freshness doesn't matter; combine with `max_cache_age_seconds` for
    /// a TTL guard.
    #[serde(default)]
    pub prefer_local_cache: bool,
    /// Maximum cache age (seconds) when prefer_local_cache=true. Older entries
    /// fall through to Azure. Default 300 (5 minutes).
    #[serde(default = "default_max_age")]
    pub max_cache_age_seconds: i64,
}

fn default_max_age() -> i64 {
    300
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Output {
    pub ticket_name: String,
    pub raw: serde_json::Value,
    /// True if served from local cache rather than Azure.
    pub from_cache: bool,
    /// Age (seconds) of the cache row when served from cache; None on Azure hits.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_age_seconds: Option<i64>,
}

pub async fn run(state: &AppState, input: Input) -> AppResult<Output> {
    if input.subscription_id.trim().is_empty() || input.ticket_name.trim().is_empty() {
        return Err(AppError::Validation(
            "subscription_id and ticket_name are required".into(),
        ));
    }
    if input.prefer_local_cache {
        if let Some(entry) = state
            .cache
            .get_ticket_cache(&input.subscription_id, &input.ticket_name)
            .await?
        {
            let age = crate::cache::now_unix() - entry.cached_at;
            if age <= input.max_cache_age_seconds {
                let raw: serde_json::Value =
                    serde_json::from_str(&entry.raw_json).unwrap_or(serde_json::Value::Null);
                return Ok(Output {
                    ticket_name: input.ticket_name,
                    raw,
                    from_cache: true,
                    cache_age_seconds: Some(age),
                });
            }
        }
    }
    let (arm, _chain) = super::arm_for(state)?;
    let raw = get_ticket(&arm, &input.subscription_id, &input.ticket_name).await?;
    // Write-through on every fetch so subsequent prefer_local_cache calls are fast.
    crate::cache::tickets::upsert_from_arm(
        &state.cache,
        &input.subscription_id,
        &input.ticket_name,
        None,
        &raw,
        "get",
    )
    .await;
    Ok(Output {
        ticket_name: input.ticket_name,
        raw,
        from_cache: false,
        cache_age_seconds: None,
    })
}
