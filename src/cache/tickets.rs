//! Write-through cache for support tickets we author/touch.
//!
//! Tradeoff: status & communications can drift between local writes and Azure,
//! so reads are opt-in via `prefer_local_cache` on get/list tools. Writes
//! (create / update / reply) always populate the cache because we know the
//! latest state at write time.

use crate::cache::{Cache, TicketCacheRow};
use serde_json::Value;

/// Pull the common scalar fields off an Azure Support ticket JSON body.
/// Returns the canonical raw shape `{ id, name, properties }` re-serialized.
pub struct ExtractedTicket {
    pub support_ticket_id: Option<String>,
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
}

pub fn extract_from_arm(raw: &Value) -> ExtractedTicket {
    let p = raw.get("properties").cloned().unwrap_or(Value::Null);
    let s = |k: &str| p.get(k).and_then(|v| v.as_str()).map(String::from);
    let resource_id = p
        .get("technicalTicketDetails")
        .and_then(|t| t.get("resourceId"))
        .and_then(|v| v.as_str())
        .map(String::from)
        .or_else(|| s("resourceId"));
    ExtractedTicket {
        support_ticket_id: s("supportTicketId"),
        title: s("title"),
        severity: s("severity"),
        status: s("status"),
        service_id: p
            .get("serviceId")
            .and_then(|v| v.as_str())
            .map(String::from)
            .or_else(|| s("serviceId")),
        service_display_name: s("serviceDisplayName"),
        problem_classification_id: s("problemClassificationId"),
        resource_id,
        created_date: s("createdDate"),
        modified_date: s("modifiedDate"),
        raw_json: serde_json::to_string(raw).unwrap_or_else(|_| "{}".into()),
    }
}

/// Upsert the cache row. Best-effort: logs but never fails the caller.
pub async fn upsert_from_arm(
    cache: &Cache,
    subscription_id: &str,
    ticket_name: &str,
    tenant_id: Option<&str>,
    raw: &Value,
    source: &str,
) {
    let e = extract_from_arm(raw);
    let row = TicketCacheRow {
        subscription_id,
        ticket_name,
        support_ticket_id: e.support_ticket_id.as_deref(),
        tenant_id,
        title: e.title.as_deref(),
        severity: e.severity.as_deref(),
        status: e.status.as_deref(),
        service_id: e.service_id.as_deref(),
        service_display_name: e.service_display_name.as_deref(),
        problem_classification_id: e.problem_classification_id.as_deref(),
        resource_id: e.resource_id.as_deref(),
        created_date: e.created_date.as_deref(),
        modified_date: e.modified_date.as_deref(),
        raw_json: &e.raw_json,
        source,
    };
    if let Err(err) = cache.upsert_ticket_cache(row).await {
        tracing::warn!(error = %err, ticket_name, "tickets_cache upsert failed");
    }
}
