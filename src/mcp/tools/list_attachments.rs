//! `list_attachments`: enumerate files in a ticket's workspace.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::azure::support::file_workspaces::list_files;
use crate::bootstrap::AppState;
use crate::error::{AppError, AppResult};

#[derive(Debug, Deserialize, JsonSchema)]
pub struct Input {
    pub subscription_id: String,
    /// Workspace name. By convention equal to the ticket name.
    pub file_workspace_name: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct AttachmentSummary {
    pub file_name: String,
    pub size_bytes: Option<u64>,
    pub chunk_size: Option<u64>,
    pub number_of_chunks: Option<u64>,
    pub created_on: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Output {
    pub file_workspace_name: String,
    pub attachments: Vec<AttachmentSummary>,
}

pub async fn run(state: &AppState, input: Input) -> AppResult<Output> {
    if input.subscription_id.trim().is_empty() || input.file_workspace_name.trim().is_empty() {
        return Err(AppError::Validation(
            "subscription_id and file_workspace_name are required".into(),
        ));
    }
    let (arm, _chain) = super::arm_for(state)?;
    let files = list_files(&arm, &input.subscription_id, &input.file_workspace_name).await?;
    let attachments = files
        .into_iter()
        .map(|f| {
            let name = f
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let p = f
                .get("properties")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            AttachmentSummary {
                file_name: name,
                size_bytes: p.get("fileSize").and_then(|v| v.as_u64()),
                chunk_size: p.get("chunkSize").and_then(|v| v.as_u64()),
                number_of_chunks: p.get("numberOfChunks").and_then(|v| v.as_u64()),
                created_on: p
                    .get("createdOn")
                    .and_then(|v| v.as_str())
                    .map(String::from),
            }
        })
        .collect();
    Ok(Output {
        file_workspace_name: input.file_workspace_name,
        attachments,
    })
}
