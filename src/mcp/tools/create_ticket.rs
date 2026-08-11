//! `create_support_ticket`: gated by the confirmation guard.

use std::time::Duration;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracing::info;

use crate::azure::support::tickets::{build_ticket_body, create_ticket, generate_ticket_name};
use crate::bootstrap::AppState;
use crate::error::{AppError, AppResult};
use crate::workflow::share::{format_share_markdown, portal_url_for_ticket, ShareInputs};
use crate::workflow::validator;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct Input {
    pub draft_id: String,
    pub review_token: String,
    pub draft_hash: String,
    pub confirmed: bool,
    /// Override generated ticket name (UUID by default).
    #[serde(default)]
    pub ticket_name: Option<String>,
    /// Max async-op polls. Default 10.
    #[serde(default = "default_polls")]
    pub max_polls: u32,
    /// Seconds between polls. Default 3.
    #[serde(default = "default_interval")]
    pub poll_interval_seconds: u64,
    /// Keep draft after submit. Default false (deleted on success).
    #[serde(default)]
    pub retain_draft: bool,
    /// Auto-save contact slice to `default` template on success. Default true.
    #[serde(default = "default_true")]
    pub save_as_default_template: bool,
    /// Also save contact slice under this named template.
    #[serde(default)]
    pub save_as_template_name: Option<String>,
}

fn default_polls() -> u32 {
    10
}
fn default_interval() -> u64 {
    3
}
fn default_true() -> bool {
    true
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Output {
    pub ticket_name: String,
    pub status: String,
    pub title: String,
    pub severity: String,
    pub severity_label: String,
    pub tenant_id: Option<String>,
    pub subscription_id: String,
    pub support_ticket_id: Option<String>,
    pub portal_url: String,
    pub share_markdown: String,
    #[schemars(schema_with = "crate::mcp::schema::any_json_schema")]
    pub raw: serde_json::Value,
}

pub async fn run(state: &AppState, input: Input) -> AppResult<Output> {
    // 1. Confirmation guard.
    let bound_draft_id =
        state
            .review_tokens
            .verify(&input.review_token, &input.draft_hash, input.confirmed)?;
    if bound_draft_id != input.draft_id {
        return Err(AppError::Validation(format!(
            "review_token is bound to draft `{bound_draft_id}`, not `{}`",
            input.draft_id
        )));
    }
    let draft = state.drafts.get(&input.draft_id).await?;

    // 2. Re-validate (refuse if not valid).
    let report = validator::validate(&draft);
    if !report.valid {
        let msg = report
            .errors
            .iter()
            .map(|e| format!("{}: {}", e.field, e.message))
            .collect::<Vec<_>>()
            .join("; ");
        return Err(AppError::Validation(format!(
            "draft fails validation: {msg}"
        )));
    }

    // 3. Defensive hash recheck after the read.
    state
        .review_tokens
        .check_hash(&input.review_token, &draft.content_hash())?;

    let sub_id = draft.subscription_id.clone().ok_or_else(|| {
        AppError::Validation("subscription_id is required to submit a ticket".into())
    })?;

    // 4. Build body + submit. If attachments were prepared, the workspace
    //    name is the ticket name by convention; honor that unless caller
    //    explicitly overrides.
    let ticket_name = match (
        input.ticket_name.as_deref(),
        draft.file_workspace_name.as_deref(),
    ) {
        (Some(explicit), Some(ws)) if explicit != ws => {
            return Err(AppError::Validation(format!(
                "ticket_name `{explicit}` conflicts with the prepared file workspace `{ws}`; omit ticket_name to reuse the workspace, or call prepare_attachments again with a new draft"
            )));
        }
        (Some(explicit), _) => explicit.to_string(),
        (None, Some(ws)) => ws.to_string(),
        (None, None) => generate_ticket_name(),
    };
    let body = build_ticket_body(&draft);
    let (arm, _chain) = super::arm_for(state)?;

    info!(
        ticket_name = %ticket_name,
        subscription_id = %sub_id,
        "submitting support ticket"
    );

    let created = create_ticket(
        &arm,
        &sub_id,
        &ticket_name,
        &body,
        input.max_polls,
        Duration::from_secs(input.poll_interval_seconds),
    )
    .await?;

    // 5. Post-submit bookkeeping.
    state.review_tokens.revoke(&input.review_token);

    // Auto-save contact details to template(s) before draft deletion.
    if input.save_as_default_template {
        let t = crate::workflow::templates::TicketTemplate::from_draft(
            crate::workflow::templates::DEFAULT_TEMPLATE_NAME,
            &draft,
        );
        state.templates.save_best_effort(&t);
    }
    if let Some(named) = &input.save_as_template_name {
        let t = crate::workflow::templates::TicketTemplate::from_draft(named, &draft);
        state.templates.save_best_effort(&t);
    }

    if !input.retain_draft {
        let _ = state.drafts.delete(&input.draft_id).await;
    }

    let portal_url = portal_url_for_ticket(&sub_id, &created.ticket_name);

    // Write-through to local cache (best-effort, never fails the create).
    crate::cache::tickets::upsert_from_arm(
        &state.cache,
        &sub_id,
        &created.ticket_name,
        draft.tenant_id.as_deref(),
        &created.raw,
        "create",
    )
    .await;

    let share_markdown = format_share_markdown(&ShareInputs {
        ticket_name: &created.ticket_name,
        title: &created.title,
        severity: &created.severity,
        tenant_id: draft.tenant_id.as_deref(),
        subscription_id: &sub_id,
        subscription_display_name: None,
        resource_id: draft
            .resource_id
            .as_deref()
            .or(draft.technical_ticket_details.resource_id.as_deref()),
        status: &created.status,
        portal_url: Some(&portal_url),
        summary: draft.description.as_deref(),
    });

    Ok(Output {
        ticket_name: created.ticket_name,
        status: created.status,
        title: created.title,
        severity_label: crate::workflow::share::severity_label(&created.severity),
        severity: created.severity,
        tenant_id: draft.tenant_id.clone(),
        subscription_id: sub_id,
        support_ticket_id: created.support_ticket_id,
        portal_url,
        share_markdown,
        raw: created.raw,
    })
}
