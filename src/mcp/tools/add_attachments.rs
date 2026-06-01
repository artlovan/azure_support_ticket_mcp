//! `add_attachments_to_ticket`: two-call gated upload to an existing
//! ticket's workspace.

use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tracing::info;

use crate::azure::support::file_workspaces::{
    create_workspace, list_files, upload_file, MAX_FILES_PER_CALL, MAX_FILES_PER_TICKET,
};
use crate::bootstrap::AppState;
use crate::error::{AppError, AppResult};
use crate::workflow::draft::hash_intent;

use super::prepare_attachments::FileInput;

const INTENT_PREFIX: &str = "add_attachments:";

#[derive(Debug, Deserialize, JsonSchema)]
pub struct Input {
    pub subscription_id: String,
    /// Ticket name (also the workspace name, by convention).
    pub ticket_name: String,
    pub files: Vec<FileInput>,
    // ---- confirmation ----
    #[serde(default)]
    pub review_token: Option<String>,
    #[serde(default)]
    pub draft_hash: Option<String>,
    #[serde(default)]
    pub confirmed: Option<bool>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct UploadedFile {
    pub file_name: String,
    pub size_bytes: usize,
    pub chunks: usize,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Output {
    pub phase: String,
    pub ticket_name: String,
    pub file_workspace_name: String,
    pub planned: Vec<UploadedFile>,
    /// Total file count this ticket will have after upload (existing + new).
    /// Useful for the assistant to show "5 / 25 attachments" style hints.
    pub total_after_upload: usize,
    /// Existing attachment count on the ticket before this call.
    pub existing_count: usize,
    pub review_token: Option<String>,
    pub draft_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uploaded: Option<Vec<UploadedFile>>,
    /// Preformatted markdown to render verbatim during the preview phase.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confirmation_prompt: Option<String>,
    /// Short one-liner safe for a single-line confirmation widget. Hosts
    /// with a confirmation dialog use this as the question text; hosts that
    /// just ask in chat can ignore it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub question_prompt: Option<String>,
    pub instructions: String,
}

pub async fn run(state: &AppState, input: Input) -> AppResult<Output> {
    if input.subscription_id.trim().is_empty() || input.ticket_name.trim().is_empty() {
        return Err(AppError::Validation(
            "subscription_id and ticket_name are required".into(),
        ));
    }
    if input.files.is_empty() {
        return Err(AppError::Validation("at least one file is required".into()));
    }
    if input.files.len() > MAX_FILES_PER_CALL {
        return Err(AppError::Validation(format!(
            "Azure Support accepts at most {MAX_FILES_PER_CALL} files per upload call; got {}. Split into multiple calls.",
            input.files.len()
        )));
    }

    // Decode + size-check up front so the preview reports the same plan we'd apply.
    let prepared = decode_files(&input.files)?;
    let planned: Vec<UploadedFile> = prepared
        .iter()
        .map(|(name, bytes)| {
            let encoded = crate::azure::support::file_workspaces::encode_for_upload(bytes).unwrap();
            UploadedFile {
                file_name: name.clone(),
                size_bytes: bytes.len(),
                chunks: encoded.chunk_b64.len(),
            }
        })
        .collect();

    // Pre-flight: how many attachments already on the workspace?
    let (arm, _chain) = super::arm_for(state)?;
    let existing_count = list_files(&arm, &input.subscription_id, &input.ticket_name)
        .await
        .map(|v| v.len())
        .unwrap_or(0);
    let total_after_upload = existing_count + planned.len();
    if total_after_upload > MAX_FILES_PER_TICKET {
        return Err(AppError::Validation(format!(
            "ticket `{}` already has {} attachment(s); uploading {} more would exceed the per-ticket limit of {MAX_FILES_PER_TICKET}. Remove some via the Azure portal first, or skip {} of the new files.",
            input.ticket_name,
            existing_count,
            planned.len(),
            total_after_upload - MAX_FILES_PER_TICKET
        )));
    }

    let intent_key = format!("{INTENT_PREFIX}{}", input.ticket_name);
    // Hash by ticket + ordered list of (name, size_bytes, chunks) — content
    // bytes themselves are not in the hash to keep the JSON readable. The
    // tuple (name, size_bytes, chunks) is enough to detect changed payloads.
    let intent_hash = hash_intent(&json!({
        "ticket_name": input.ticket_name,
        "files": planned.iter().map(|u| json!({
            "file_name": u.file_name,
            "size_bytes": u.size_bytes,
            "chunks": u.chunks,
        })).collect::<Vec<_>>()
    }))?;

    match (
        input.review_token.as_deref(),
        input.draft_hash.as_deref(),
        input.confirmed,
    ) {
        (None, _, _) | (_, None, _) | (_, _, None | Some(false)) => {
            state.review_tokens.revoke_draft(&intent_key);
            let issued = state
                .review_tokens
                .issue_for_intent(intent_key, intent_hash.clone());
            let prompt = render_attach_prompt(
                &input.ticket_name,
                &planned,
                existing_count,
                total_after_upload,
            );
            let planned_count = planned.len();
            Ok(Output {
                phase: "preview".into(),
                ticket_name: input.ticket_name.clone(),
                file_workspace_name: input.ticket_name,
                planned,
                total_after_upload,
                existing_count,
                review_token: Some(issued.review_token),
                draft_hash: Some(issued.draft_hash),
                uploaded: None,
                confirmation_prompt: Some(prompt),
                question_prompt: Some(format!("Upload {planned_count} file(s)?")),
                instructions:
                    "TWO STEPS: (1) SHOW `confirmation_prompt` to the user VERBATIM (markdown table — render in chat; if your environment has a separate confirmation widget that strips formatting, still print to chat first). (2) THEN ask for confirmation using whatever interaction your environment supports (confirmation widget with `question_prompt` + 3 choices, or plain chat question). Reply handling: yes/1 → re-call with review_token+draft_hash+confirmed=true; cancel/3 → stop; ANY other free-form reply → treat as edits (e.g. remove a file, rename one) and re-call WITHOUT review_token."
                        .into(),
            })
        }
        (Some(token), Some(hash), Some(true)) => {
            let bound_key = state.review_tokens.verify(token, hash, true)?;
            if bound_key != intent_key {
                return Err(AppError::Validation(format!(
                    "review_token is bound to a different ticket (`{bound_key}`)"
                )));
            }
            if hash != intent_hash {
                return Err(AppError::Validation(
                    "file set changed since the review_token was issued; re-run without review_token to get a fresh preview"
                        .into(),
                ));
            }
            info!(
                ticket_name = %input.ticket_name,
                file_workspace = %input.ticket_name,
                files = prepared.len(),
                "uploading attachments to existing ticket workspace"
            );
            // Workspace should already exist (created by ticket submission); PUT
            // is idempotent so this is safe regardless.
            let _ = create_workspace(&arm, &input.subscription_id, &input.ticket_name).await?;
            let mut uploaded = Vec::new();
            for (name, bytes) in &prepared {
                let _ = upload_file(
                    &arm,
                    &input.subscription_id,
                    &input.ticket_name,
                    name,
                    bytes,
                )
                .await?;
                let encoded = crate::azure::support::file_workspaces::encode_for_upload(bytes)?;
                uploaded.push(UploadedFile {
                    file_name: name.clone(),
                    size_bytes: bytes.len(),
                    chunks: encoded.chunk_b64.len(),
                });
            }
            state.review_tokens.revoke(token);
            Ok(Output {
                phase: "applied".into(),
                ticket_name: input.ticket_name.clone(),
                file_workspace_name: input.ticket_name,
                planned,
                total_after_upload,
                existing_count,
                review_token: None,
                draft_hash: None,
                uploaded: Some(uploaded),
                confirmation_prompt: None,
                question_prompt: None,
                instructions:
                    "Attachments uploaded. They live on the ticket workspace, not on any individual reply."
                        .into(),
            })
        }
    }
}

/// Friendly markdown preview of pending uploads. Renders a table with
/// readable byte sizes and the cap usage so the user can sanity-check.
fn render_attach_prompt(
    ticket_name: &str,
    planned: &[UploadedFile],
    existing_count: usize,
    total_after_upload: usize,
) -> String {
    let mut s = String::new();
    s.push_str(&format!(
        "**Upload {} file(s) to ticket `{}`?**\n\n",
        planned.len(),
        ticket_name
    ));
    s.push_str("| # | File | Size |\n");
    s.push_str("|---|---|---|\n");
    for (i, f) in planned.iter().enumerate() {
        s.push_str(&format!(
            "| {} | {} | {} |\n",
            i + 1,
            f.file_name,
            human_bytes(f.size_bytes)
        ));
    }
    s.push('\n');
    s.push_str(&format!(
        "_Attachments on this ticket after upload: **{} / {}** ({} existing + {} new). Per-call cap: {}._\n\n",
        total_after_upload,
        MAX_FILES_PER_TICKET,
        existing_count,
        planned.len(),
        MAX_FILES_PER_CALL,
    ));
    s.push_str("**Reply with one of:**\n");
    s.push_str("1. **Yes, upload** — send to Azure now.\n");
    s.push_str("2. **Your edits, inline** — e.g. _'drop file 2'_ or _'rename to logs.txt'_; any non-yes/cancel reply is treated as edits and re-previewed.\n");
    s.push_str("3. **Cancel** — don't upload.\n");
    s
}

fn human_bytes(n: usize) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    let f = n as f64;
    if f >= MB {
        format!("{:.2} MB", f / MB)
    } else if f >= KB {
        format!("{:.1} KB", f / KB)
    } else {
        format!("{n} B")
    }
}

fn decode_files(files: &[FileInput]) -> AppResult<Vec<(String, Vec<u8>)>> {
    let mut out = Vec::new();
    for (i, f) in files.iter().enumerate() {
        let bytes = match (&f.path, &f.content_base64) {
            (Some(p), None) => super::file_resolve::read_user_file(p).map_err(|e| match e {
                AppError::Validation(m) => AppError::Validation(format!("file #{i}: {m}")),
                other => other,
            })?,
            (None, Some(b64)) => B64.decode(b64.as_bytes()).map_err(|e| {
                AppError::Validation(format!("file #{i}: invalid content_base64: {e}"))
            })?,
            (Some(_), Some(_)) | (None, None) => {
                return Err(AppError::Validation(format!(
                    "file #{i}: provide exactly one of `path` or `content_base64`"
                )))
            }
        };
        let file_name = match (&f.file_name, &f.path) {
            (Some(n), _) => n.clone(),
            (None, Some(p)) => std::path::Path::new(p)
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("attachment.bin")
                .to_string(),
            (None, None) => format!("attachment-{i}.bin"),
        };
        // Validate file size eagerly (encode_for_upload will catch it too).
        if bytes.len() > crate::azure::support::file_workspaces::MAX_FILE_BYTES {
            return Err(AppError::Validation(format!(
                "file #{i} (`{file_name}`) is {} bytes; max {}",
                bytes.len(),
                crate::azure::support::file_workspaces::MAX_FILE_BYTES
            )));
        }
        out.push((file_name, bytes));
    }
    Ok(out)
}
