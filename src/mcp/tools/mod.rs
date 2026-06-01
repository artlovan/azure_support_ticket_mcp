//! MCP tools.
//!
//! Each submodule defines `Input`, `Output`, and `run(state, input)`. The
//! `server.rs` adapter just wires them to the rmcp macros.

use rmcp::ErrorData;

use crate::error::AppError;

pub mod add_attachments;
pub mod auth_status;
pub mod azure_search;
pub mod build_draft;
pub mod commit_sanitized;
pub mod create_ticket;
pub mod delete_template;
pub mod doctor;
pub mod file_resolve;
pub mod get_template;
pub mod get_ticket;
pub mod ingest_error;
pub mod init_template;
pub mod list_attachments;
pub mod list_classifications;
pub mod list_communications;
pub mod list_drafts;
pub mod list_services;
pub mod list_subscriptions;
pub mod list_templates;
pub mod list_tenants;
pub mod list_tickets;
pub mod prepare_attachments;
pub mod preview_draft;
pub mod refresh_cache;
pub mod reply_to_ticket;
pub mod resolve_context;
pub mod save_template;
pub mod start_flow;
pub mod summarize_thread;
pub mod tenant_lookup;
pub mod update_ticket;
pub mod whoami;

/// Convert internal errors to MCP error data. We never leak tokens or
/// stack traces; the structured fields go to logs.
pub fn to_mcp_error(err: AppError) -> ErrorData {
    tracing::warn!(error = %err, "tool failed");
    let msg = match &err {
        AppError::Azure {
            message,
            code,
            status,
            request_id,
            ..
        } => format!(
            "Azure error: {message} (code={:?}, status={:?}, request_id={:?})",
            code, status, request_id
        ),
        AppError::Auth(m) => format!("Auth: {m}"),
        AppError::Validation(m) => format!("Validation: {m}"),
        AppError::NotFound(m) => format!("Not found: {m}"),
        other => other.to_string(),
    };
    ErrorData::internal_error(msg, None)
}

// ---- shared helpers for tools ----

use std::sync::Arc;

use crate::azure::{
    auth::{build_default_chain, ChainedAuthProvider},
    ArmClient, ArmEndpoints,
};
use crate::bootstrap::AppState;
use crate::error::AppResult;

/// Build an ARM client wired with the configured auth chain. Cheap enough
/// to construct per-call; may be cached on `AppState` later if it shows up
/// in profiles.
pub fn arm_for(state: &AppState) -> AppResult<(ArmClient, Arc<ChainedAuthProvider>)> {
    let chain = Arc::new(build_default_chain(
        state.config.auth.allow_az_cli_fallback,
    )?);
    let endpoints = ArmEndpoints::for_cloud(&state.config.general.cloud);
    let client = ArmClient::new(endpoints, chain.clone())?;
    Ok((client, chain))
}
