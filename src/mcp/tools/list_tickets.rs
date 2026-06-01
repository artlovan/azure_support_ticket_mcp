//! `list_support_tickets`: paged list of support tickets in a subscription.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::azure::support::tickets::{list_tickets, TicketPage};
use crate::bootstrap::AppState;
use crate::error::{AppError, AppResult};

#[derive(Debug, Deserialize, JsonSchema)]
pub struct Input {
    pub subscription_id: String,
    /// Page size (Azure caps at server side). Default 25.
    #[serde(default)]
    pub top: Option<u32>,
    /// OData $filter expression (e.g. `Status eq 'Open'`).
    #[serde(default)]
    pub filter: Option<String>,
    /// Continuation link from a previous page.
    #[serde(default)]
    pub next_link: Option<String>,
    /// Serve recent rows from local SQLite cache when present (populated on
    /// create/update/get). Default false. Ignored when `filter` or `next_link`
    /// is set since the cache can't honor server-side OData expressions.
    #[serde(default)]
    pub prefer_local_cache: bool,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct TicketSummary {
    pub ticket_name: String,
    pub support_ticket_id: Option<String>,
    pub title: Option<String>,
    pub status: Option<String>,
    pub severity: Option<String>,
    pub service_display_name: Option<String>,
    pub created_date: Option<String>,
    pub modified_date: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Output {
    pub tickets: Vec<TicketSummary>,
    pub next_link: Option<String>,
    pub from_cache: bool,
    /// Age (seconds) of the newest cache row used; None on Azure hits.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub newest_cache_age_seconds: Option<i64>,
}

pub async fn run(state: &AppState, input: Input) -> AppResult<Output> {
    if input.subscription_id.trim().is_empty() {
        return Err(AppError::Validation("subscription_id is required".into()));
    }
    if input.prefer_local_cache && input.filter.is_none() && input.next_link.is_none() {
        let limit = input.top.unwrap_or(25) as i64;
        let rows = state
            .cache
            .list_recent_tickets_cache(&input.subscription_id, limit)
            .await?;
        if !rows.is_empty() {
            let now = crate::cache::now_unix();
            let newest_age = rows.iter().map(|r| now - r.cached_at).min();
            let summaries = rows
                .into_iter()
                .map(|r| TicketSummary {
                    ticket_name: r.ticket_name,
                    support_ticket_id: r.support_ticket_id,
                    title: r.title,
                    status: r.status,
                    severity: r.severity,
                    service_display_name: r.service_display_name,
                    created_date: r.created_date,
                    modified_date: r.modified_date,
                })
                .collect();
            return Ok(Output {
                tickets: summaries,
                next_link: None,
                from_cache: true,
                newest_cache_age_seconds: newest_age,
            });
        }
    }
    let (arm, _chain) = super::arm_for(state)?;
    let TicketPage { tickets, next_link } = list_tickets(
        &arm,
        &input.subscription_id,
        input.top.or(Some(25)),
        input.filter.as_deref(),
        input.next_link.as_deref(),
    )
    .await?;
    let summaries: Vec<TicketSummary> = tickets
        .into_iter()
        .map(|t| {
            let name = t
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            // Write-through each row so prefer_local_cache hits are warm.
            let sub = input.subscription_id.clone();
            let raw = t.clone();
            let cache = state.cache.clone();
            let nm = name.clone();
            tokio::spawn(async move {
                crate::cache::tickets::upsert_from_arm(&cache, &sub, &nm, None, &raw, "list").await;
            });
            let p = t
                .get("properties")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            TicketSummary {
                ticket_name: name,
                support_ticket_id: p
                    .get("supportTicketId")
                    .and_then(|v| v.as_str())
                    .map(String::from),
                title: p.get("title").and_then(|v| v.as_str()).map(String::from),
                status: p.get("status").and_then(|v| v.as_str()).map(String::from),
                severity: p.get("severity").and_then(|v| v.as_str()).map(String::from),
                service_display_name: p
                    .get("serviceDisplayName")
                    .and_then(|v| v.as_str())
                    .map(String::from),
                created_date: p
                    .get("createdDate")
                    .and_then(|v| v.as_str())
                    .map(String::from),
                modified_date: p
                    .get("modifiedDate")
                    .and_then(|v| v.as_str())
                    .map(String::from),
            }
        })
        .collect();
    Ok(Output {
        tickets: summaries,
        next_link,
        from_cache: false,
        newest_cache_age_seconds: None,
    })
}
