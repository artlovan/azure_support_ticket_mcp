//! `prepare_attachments`: stage files for a draft before the ticket is created.
//!
//! Creates a fileWorkspace using the draft's pinned ticket name (generated on
//! first call), uploads each file, and stamps `file_workspace_name` on the
//! draft so `create_support_ticket` automatically picks it up. Rotates the
//! draft's `review_token` + `draft_hash`.
//!
//! Not gated by the confirmation guard: files staged in a workspace are not
//! visible to anyone until the ticket itself is created.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracing::info;

use crate::azure::support::file_workspaces::{create_workspace, upload_file};
use crate::azure::support::tickets::generate_ticket_name;
use crate::bootstrap::AppState;
use crate::error::{AppError, AppResult};
use crate::workflow::draft::TicketDraftPatch;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct Input {
    pub draft_id: String,
    /// At least one file. Each may provide either `path` (read by the MCP)
    /// or inline `content_base64`.
    pub files: Vec<FileInput>,
}

#[derive(Debug, Deserialize, JsonSchema, Clone)]
pub struct FileInput {
    /// Local filesystem path to read. Supports `~/…` and `~\…`, `$VAR`,
    /// `${VAR}`, and `%VAR%` expansion across platforms. If the path doesn't
    /// exist, the MCP searches common dirs (Desktop, Downloads, Pictures,
    /// CWD, system temp, $COPILOT_ATTACHMENTS_DIR) for files matching the
    /// basename and returns candidates in the error — never silently
    /// substitutes. Mutually exclusive with `content_base64`.
    #[serde(default)]
    pub path: Option<String>,
    /// Inline base64 content. Mutually exclusive with `path`.
    #[serde(default)]
    pub content_base64: Option<String>,
    /// File name as it should appear on the ticket. Defaults to basename of `path`.
    #[serde(default)]
    pub file_name: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct UploadedFile {
    pub file_name: String,
    pub size_bytes: usize,
    pub chunks: usize,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Output {
    pub draft_id: String,
    pub file_workspace_name: String,
    pub uploaded: Vec<UploadedFile>,
    pub review_token: String,
    pub draft_hash: String,
    pub instructions: String,
}

pub async fn run(state: &AppState, input: Input) -> AppResult<Output> {
    use crate::azure::support::file_workspaces::{MAX_FILES_PER_CALL, MAX_FILES_PER_TICKET};
    if input.files.is_empty() {
        return Err(AppError::Validation("at least one file is required".into()));
    }
    if input.files.len() > MAX_FILES_PER_CALL {
        return Err(AppError::Validation(format!(
            "Azure Support accepts at most {MAX_FILES_PER_CALL} files per upload call; got {}. Split into multiple calls (subsequent ones via add_attachments_to_ticket after the ticket is created).",
            input.files.len()
        )));
    }
    if input.files.len() > MAX_FILES_PER_TICKET {
        return Err(AppError::Validation(format!(
            "exceeds per-ticket attachment cap of {MAX_FILES_PER_TICKET}"
        )));
    }
    let draft = state.drafts.get(&input.draft_id).await?;
    let sub_id = draft
        .subscription_id
        .clone()
        .ok_or_else(|| AppError::Validation("draft.subscription_id is required".into()))?;

    // Pin ticket name on the draft (workspace name == ticket name by convention).
    let ws_name = match &draft.file_workspace_name {
        Some(existing) => existing.clone(),
        None => generate_ticket_name(),
    };

    // Decode + size-check up front.
    let mut prepared: Vec<(String, Vec<u8>)> = Vec::new();
    for (i, f) in input.files.iter().enumerate() {
        let bytes = match (&f.path, &f.content_base64) {
            (Some(p), None) => super::file_resolve::read_user_file(p).map_err(|e| match e {
                AppError::Validation(m) => AppError::Validation(format!("file #{i}: {m}")),
                other => other,
            })?,
            (None, Some(b64)) => {
                use base64::engine::general_purpose::STANDARD as B64;
                use base64::Engine as _;
                B64.decode(b64.as_bytes()).map_err(|e| {
                    AppError::Validation(format!("file #{i}: invalid content_base64: {e}"))
                })?
            }
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
        prepared.push((file_name, bytes));
    }

    // Create workspace (idempotent), then upload.
    let (arm, _chain) = super::arm_for(state)?;
    info!(
        draft_id = %input.draft_id,
        subscription_id = %sub_id,
        file_workspace = %ws_name,
        files = prepared.len(),
        "preparing attachments"
    );
    let _ = create_workspace(&arm, &sub_id, &ws_name).await?;

    let mut uploaded = Vec::new();
    for (name, bytes) in &prepared {
        let _meta = upload_file(&arm, &sub_id, &ws_name, name, bytes).await?;
        // Recompute chunk count to report it back.
        let encoded = crate::azure::support::file_workspaces::encode_for_upload(bytes)?;
        uploaded.push(UploadedFile {
            file_name: name.clone(),
            size_bytes: bytes.len(),
            chunks: encoded.chunk_b64.len(),
        });
    }

    // Stamp file_workspace_name on the draft. Rotates token + hash.
    let mut updated = state.drafts.get(&input.draft_id).await?;
    let patch = TicketDraftPatch {
        file_workspace_name: Some(ws_name.clone()),
        ..Default::default()
    };
    updated.apply_patch(&patch);
    state.drafts.put(updated.clone()).await?;
    state.review_tokens.revoke_draft(&updated.draft_id);
    let issued = state.review_tokens.issue(&updated);

    Ok(Output {
        draft_id: input.draft_id,
        file_workspace_name: ws_name,
        uploaded,
        review_token: issued.review_token,
        draft_hash: issued.draft_hash,
        instructions:
            "Files staged. Call create_support_ticket with the returned review_token + draft_hash + confirmed:true. The workspace name will be reused as the ticket name."
                .into(),
    })
}
