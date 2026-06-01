//! `list_drafts` + `discard_draft`: enumerate / remove in-progress ticket
//! drafts. Drafts live in `state.drafts` (memory by default, ttl_days in
//! sqlite mode) — this surfaces them so the user can resume or clean up
//! abandoned ones.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::bootstrap::AppState;
use crate::error::AppResult;
use crate::workflow::validator;

#[derive(Debug, Deserialize, JsonSchema, Default)]
pub struct ListInput {
    /// If true (default), also include drafts that pass validation. Set to
    /// `false` to see only incomplete drafts you still need to fill in.
    #[serde(default = "default_true")]
    pub include_valid: bool,
    /// Optional substring match on title/service/subscription_id (case-insensitive).
    #[serde(default)]
    pub filter: Option<String>,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct DraftSummary {
    pub draft_id: String,
    pub title: Option<String>,
    pub service_id: Option<String>,
    pub problem_classification_id: Option<String>,
    pub severity: Option<String>,
    pub subscription_id: Option<String>,
    pub tenant_id: Option<String>,
    pub resource_id: Option<String>,
    pub valid: bool,
    pub missing_field_count: usize,
    pub missing_fields: Vec<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ListOutput {
    pub drafts: Vec<DraftSummary>,
    pub count: usize,
    pub message: String,
}

pub async fn list(state: &AppState, input: ListInput) -> AppResult<ListOutput> {
    let all = state.drafts.list().await?;
    let needle = input.filter.as_deref().map(str::to_lowercase);

    let mut out: Vec<DraftSummary> = all
        .into_iter()
        .filter_map(|d| {
            let report = validator::validate(&d);
            if !input.include_valid && report.valid {
                return None;
            }
            if let Some(n) = &needle {
                let hay = format!(
                    "{} {} {}",
                    d.title.as_deref().unwrap_or(""),
                    d.service_id.as_deref().unwrap_or(""),
                    d.subscription_id.as_deref().unwrap_or(""),
                )
                .to_lowercase();
                if !hay.contains(n) {
                    return None;
                }
            }
            let missing: Vec<String> = report.errors.iter().map(|e| e.field.clone()).collect();
            Some(DraftSummary {
                draft_id: d.draft_id.clone(),
                title: d.title.clone(),
                service_id: d.service_id.clone(),
                problem_classification_id: d.problem_classification_id.clone(),
                severity: d.severity.clone(),
                subscription_id: d.subscription_id.clone(),
                tenant_id: d.tenant_id.clone(),
                resource_id: d
                    .resource_id
                    .clone()
                    .or_else(|| d.technical_ticket_details.resource_id.clone()),
                valid: report.valid,
                missing_field_count: missing.len(),
                missing_fields: missing,
            })
        })
        .collect();

    // Most-actionable first: valid (ready to submit) before incomplete, then
    // by title for stability.
    out.sort_by(|a, b| b.valid.cmp(&a.valid).then(a.title.cmp(&b.title)));

    let count = out.len();
    let message = if count == 0 {
        "No drafts found. Start one with start_support_ticket_flow.".into()
    } else {
        format!(
            "{count} draft(s). Resume any with build_ticket_draft (use draft_id), or call preview_ticket_draft. Discard with discard_draft."
        )
    };
    Ok(ListOutput {
        drafts: out,
        count,
        message,
    })
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DiscardInput {
    pub draft_id: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct DiscardOutput {
    pub draft_id: String,
    pub discarded: bool,
    pub message: String,
}

pub async fn discard(state: &AppState, input: DiscardInput) -> AppResult<DiscardOutput> {
    // Best-effort: revoke any review_token bound to this draft so any
    // outstanding preview can't accidentally submit.
    state.review_tokens.revoke_draft(&input.draft_id);
    state.drafts.delete(&input.draft_id).await?;
    Ok(DiscardOutput {
        draft_id: input.draft_id.clone(),
        discarded: true,
        message: format!("Draft `{}` discarded.", input.draft_id),
    })
}

#[cfg(test)]
mod tests {
    use crate::workflow::draft::TicketDraft;
    use crate::workflow::store::{DraftStore, MemoryDraftStore};

    #[tokio::test]
    async fn list_filters_and_orders() {
        let store = MemoryDraftStore::new();
        let mut a = TicketDraft::new();
        a.title = Some("Alpha bug".into());
        let mut b = TicketDraft::new();
        b.title = Some("Beta issue".into());
        store.put(a).await.unwrap();
        store.put(b).await.unwrap();
        let drafts = store.list().await.unwrap();
        assert_eq!(drafts.len(), 2);
    }
}
