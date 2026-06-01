//! `commit_sanitized_context` — second half of the zero-friction handshake.
//!
//! Consumes the `sanitize_token` from `ingest_error_context`, validates the
//! sanitized text against the catastrophic-pattern tripwire, then creates
//! the draft populated with the recognizer-extracted fields + the sanitized
//! description.
//!
//! Failure mode: if the tripwire matches an unambiguous secret in the
//! "sanitized" text, the MCP rejects the commit and asks the assistant to
//! re-sanitize. The token is NOT consumed on tripwire failure — the
//! assistant gets a retry with the same token.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::bootstrap::AppState;
use crate::error::{AppError, AppResult};
use crate::resolver::recognizers::ExtractedFields;
use crate::workflow::draft::{TicketDraft, TicketDraftPatch};
use crate::workflow::secret_tripwire;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct Input {
    /// Token from a prior ingest_error_context call. One-shot, 5-min TTL.
    pub sanitize_token: String,
    /// The sanitized text. This becomes the draft's description.
    /// MUST NOT contain catastrophic secret patterns; the tripwire will
    /// reject the commit if it detects any.
    pub sanitized_text: String,
    /// Human-readable summary of what was redacted, e.g.
    /// "Redacted 2 items: BEARER_TOKEN at line 142, STORAGE_KEY at line 891".
    /// Shown to the user in the final preview so they can sanity-check
    /// that no critical context was lost.
    #[serde(default)]
    pub redacted_summary: Option<String>,
    /// Optional additional title hint from the assistant (overrides the
    /// recognizer's title_hint if provided).
    #[serde(default)]
    pub title: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Output {
    /// New draft ID. Pass to build_ticket_draft / preview_ticket_draft.
    pub draft_id: String,
    /// Confirmation guard token for the new draft.
    pub draft_hash: String,
    /// Review token, paired with draft_hash.
    pub review_token: String,
    /// Names of fields the MCP wrote from recognizer hints — so the
    /// assistant knows what NOT to ask the user to re-fill.
    pub prefilled_fields: Vec<String>,
    /// Echoed back so the final preview can surface it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub redacted_summary: Option<String>,
    /// Short message for the assistant.
    pub message: String,
}

pub async fn run(state: &AppState, input: Input) -> AppResult<Output> {
    // Tripwire FIRST — before consuming the token. That way, on a tripwire
    // hit, the assistant can retry with the same token (we don't burn the
    // user's only chance to fix it).
    let tripwire = secret_tripwire::scan(&input.sanitized_text);
    if !tripwire.is_empty() {
        let kinds: Vec<&str> = tripwire.iter().map(|m| m.kind.as_str()).collect();
        return Err(AppError::Validation(format!(
            "sanitization_incomplete: the catastrophic-secret tripwire matched [{}]. \
             Re-sanitize the text to remove these patterns and call commit_sanitized_context \
             again with the SAME sanitize_token (it remains valid). Matches: {}",
            kinds.join(", "),
            serde_json::to_string(&tripwire).unwrap_or_default()
        )));
    }

    // Consume the token now that we're sure the input is safe.
    let req = state
        .sanitize_tokens
        .consume(&input.sanitize_token, &input.sanitized_text)?;

    // Build the draft from recognizer-extracted + caller-provided hints.
    let merged = merge_fields(&req.recognized.fields, &req.caller_hints);
    let mut draft = TicketDraft::new();
    let patch = patch_from_fields(&merged, &input.sanitized_text, input.title.as_deref());
    draft.apply_patch(&patch);
    draft.redacted_summary = input.redacted_summary.clone();

    // Tenant backfill so the preview shows tenant alongside subscription.
    super::tenant_lookup::backfill_tenant(state, &mut draft).await;

    state.drafts.put(draft.clone()).await?;
    let issued = state.review_tokens.issue(&draft);

    let prefilled_fields = prefilled_field_names(&merged, input.title.is_some());
    let message = if prefilled_fields.is_empty() {
        "Draft created from sanitized error context. Call preview_ticket_draft to review.".into()
    } else {
        format!(
            "Draft created with {} prefilled field(s): {}. Call preview_ticket_draft to review the full ticket before submission.",
            prefilled_fields.len(),
            prefilled_fields.join(", ")
        )
    };

    Ok(Output {
        draft_id: draft.draft_id,
        draft_hash: issued.draft_hash,
        review_token: issued.review_token,
        prefilled_fields,
        redacted_summary: input.redacted_summary,
        message,
    })
}

/// Merge recognizer-extracted fields with caller-provided hints. Caller
/// wins on conflict (the human passed them deliberately).
fn merge_fields(recognized: &ExtractedFields, caller: &ExtractedFields) -> ExtractedFields {
    ExtractedFields {
        resource_id: caller
            .resource_id
            .clone()
            .or_else(|| recognized.resource_id.clone()),
        subscription_id: caller
            .subscription_id
            .clone()
            .or_else(|| recognized.subscription_id.clone()),
        error_code: caller
            .error_code
            .clone()
            .or_else(|| recognized.error_code.clone()),
        correlation_id: caller
            .correlation_id
            .clone()
            .or_else(|| recognized.correlation_id.clone()),
        severity_hint: caller
            .severity_hint
            .clone()
            .or_else(|| recognized.severity_hint.clone()),
        title_hint: caller
            .title_hint
            .clone()
            .or_else(|| recognized.title_hint.clone()),
    }
}

/// Turn the merged fields into a TicketDraftPatch. The sanitized text
/// becomes the description; recognizer hints fill scope/severity/title.
fn patch_from_fields(
    f: &ExtractedFields,
    sanitized_text: &str,
    explicit_title: Option<&str>,
) -> TicketDraftPatch {
    // Description: sanitized text. If we have an error_code or
    // correlation_id, prepend them as a small structured header so the
    // support engineer can scan it without reading the whole body.
    let mut description = String::new();
    let mut header_lines = Vec::new();
    if let Some(code) = &f.error_code {
        header_lines.push(format!("Error code: {code}"));
    }
    if let Some(cid) = &f.correlation_id {
        header_lines.push(format!("Correlation ID: {cid}"));
    }
    if !header_lines.is_empty() {
        description.push_str(&header_lines.join("\n"));
        description.push_str("\n\n--- error context ---\n");
    }
    description.push_str(sanitized_text);

    TicketDraftPatch {
        subscription_id: f.subscription_id.clone(),
        resource_id: f.resource_id.clone(),
        severity: f.severity_hint.clone(),
        title: explicit_title
            .map(str::to_string)
            .or_else(|| f.title_hint.clone()),
        description: Some(description),
        ..Default::default()
    }
}

fn prefilled_field_names(f: &ExtractedFields, has_explicit_title: bool) -> Vec<String> {
    let mut out = Vec::new();
    if f.subscription_id.is_some() {
        out.push("subscription_id".into());
    }
    if f.resource_id.is_some() {
        out.push("resource_id".into());
    }
    if f.severity_hint.is_some() {
        out.push("severity".into());
    }
    if has_explicit_title || f.title_hint.is_some() {
        out.push("title".into());
    }
    out.push("description".into());
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::resolver::recognizers::RecognizerResult;

    async fn fresh_state() -> AppState {
        let tmp = tempfile::tempdir().unwrap();
        let mut cfg = Config::default();
        cfg.cache.path = tmp.path().join("cache.sqlite");
        cfg.drafts.sqlite_path = tmp.path().join("drafts.sqlite");
        std::mem::forget(tmp);
        crate::bootstrap::ensure_initialized(&cfg).await.unwrap()
    }

    #[tokio::test]
    async fn happy_path_creates_draft_with_prefilled_fields() {
        let s = fresh_state().await;
        // Pre-stage a sanitize_token with recognized fields.
        let mut rec = RecognizerResult::default();
        rec.fields.subscription_id = Some("00000000-0000-0000-0000-000000000001".into());
        rec.fields.error_code = Some("ResourceNotFound".into());
        rec.fields.severity_hint = Some("moderate".into());
        let token = s
            .sanitize_tokens
            .issue("raw text", rec, ExtractedFields::default());

        let out = run(
            &s,
            Input {
                sanitize_token: token,
                sanitized_text: "scrubbed body of the error".into(),
                redacted_summary: Some("Redacted 1: STORAGE_KEY".into()),
                title: None,
            },
        )
        .await
        .unwrap();

        assert!(out.draft_id.starts_with("draft_"));
        assert!(out.review_token.starts_with("rt_"));
        assert!(out
            .prefilled_fields
            .contains(&"subscription_id".to_string()));
        assert!(out.prefilled_fields.contains(&"severity".to_string()));
        assert_eq!(
            out.redacted_summary.as_deref(),
            Some("Redacted 1: STORAGE_KEY")
        );

        // Draft should be persisted and have a description containing both
        // the header and the sanitized body.
        let d = s.drafts.get(&out.draft_id).await.unwrap();
        let desc = d.description.as_deref().unwrap();
        assert!(desc.contains("Error code: ResourceNotFound"));
        assert!(desc.contains("scrubbed body of the error"));
        assert_eq!(d.severity.as_deref(), Some("moderate"));
    }

    #[tokio::test]
    async fn tripwire_rejects_storage_conn_string_and_preserves_token() {
        let s = fresh_state().await;
        let token = s.sanitize_tokens.issue(
            "raw",
            RecognizerResult::default(),
            ExtractedFields::default(),
        );

        let key = format!("{}==", "A".repeat(86));
        let dirty = format!("Connection failed: DefaultEndpointsProtocol=https;AccountName=foo;AccountKey={key};EndpointSuffix=core.windows.net");
        let err = run(
            &s,
            Input {
                sanitize_token: token.clone(),
                sanitized_text: dirty,
                redacted_summary: None,
                title: None,
            },
        )
        .await
        .unwrap_err();
        assert!(format!("{err}").contains("sanitization_incomplete"));
        assert!(format!("{err}").contains("AZURE_STORAGE_CONN_STR"));

        // Token must still be valid for retry.
        let out = run(
            &s,
            Input {
                sanitize_token: token,
                sanitized_text: "Connection failed: [REDACTED:STORAGE_CONN_STR]".into(),
                redacted_summary: Some("Redacted 1: STORAGE_CONN_STR".into()),
                title: None,
            },
        )
        .await
        .unwrap();
        assert!(out.draft_id.starts_with("draft_"));
    }

    #[tokio::test]
    async fn rejects_unknown_token() {
        let s = fresh_state().await;
        let err = run(
            &s,
            Input {
                sanitize_token: "san_does_not_exist".into(),
                sanitized_text: "anything".into(),
                redacted_summary: None,
                title: None,
            },
        )
        .await
        .unwrap_err();
        assert!(format!("{err}").contains("unknown or already consumed"));
    }

    #[tokio::test]
    async fn explicit_title_overrides_recognizer_title() {
        let s = fresh_state().await;
        let mut rec = RecognizerResult::default();
        rec.fields.title_hint = Some("from-recognizer".into());
        let token = s
            .sanitize_tokens
            .issue("raw", rec, ExtractedFields::default());
        let out = run(
            &s,
            Input {
                sanitize_token: token,
                sanitized_text: "body".into(),
                redacted_summary: None,
                title: Some("explicit-title".into()),
            },
        )
        .await
        .unwrap();
        let d = s.drafts.get(&out.draft_id).await.unwrap();
        assert_eq!(d.title.as_deref(), Some("explicit-title"));
    }
}
