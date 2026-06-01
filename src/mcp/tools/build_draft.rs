//! `build_ticket_draft`: patch an existing draft, rotate review_token+hash.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::bootstrap::AppState;
use crate::error::{AppError, AppResult};
use crate::workflow::draft::{TicketDraft, TicketDraftPatch};
use crate::workflow::validator;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct Input {
    pub draft_id: String,
    /// Field-level patch. Provided fields overwrite the draft.
    #[serde(flatten)]
    pub patch: TicketDraftPatch,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Output {
    pub draft: TicketDraft,
    pub draft_hash: String,
    pub review_token: String,
    pub valid: bool,
    pub missing: Vec<String>,
    pub warnings: Vec<String>,
    pub message: String,
}

pub async fn run(state: &AppState, input: Input) -> AppResult<Output> {
    let mut draft = state
        .drafts
        .get(&input.draft_id)
        .await
        .map_err(|_| AppError::NotFound(format!("draft {} not found", input.draft_id)))?;
    draft.apply_patch(&input.patch);
    super::tenant_lookup::backfill_tenant(state, &mut draft).await;
    state.drafts.put(draft.clone()).await?;
    state.review_tokens.revoke_draft(&draft.draft_id);
    let issued = state.review_tokens.issue(&draft);

    let report = validator::validate(&draft);
    let missing: Vec<String> = report.errors.iter().map(|e| e.field.clone()).collect();
    let warnings: Vec<String> = report
        .warnings
        .iter()
        .map(|w| format!("{}: {}", w.field, w.message))
        .collect();

    let message = if report.valid {
        "Draft is valid. Call preview_ticket_draft for a human-readable summary, then create_support_ticket with the review_token + draft_hash + confirmed:true.".into()
    } else {
        format!(
            "Draft is incomplete: {} field(s) missing. Continue calling build_ticket_draft to fill them.",
            missing.len()
        )
    };

    Ok(Output {
        draft,
        draft_hash: issued.draft_hash,
        review_token: issued.review_token,
        valid: report.valid,
        missing,
        warnings,
        message,
    })
}
