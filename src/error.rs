//! Typed error model for the application.
//!
//! Recoverable / domain errors use this `AppError` enum. Azure HTTP errors
//! preserve code, HTTP status, request id, and operation id so the MCP tool
//! layer can surface actionable diagnostics without leaking secrets.

use std::path::PathBuf;

use thiserror::Error;

pub type AppResult<T> = Result<T, AppError>;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("config error: {0}")]
    Config(String),

    #[error("io error at {path:?}: {source}")]
    Io {
        path: Option<PathBuf>,
        #[source]
        source: std::io::Error,
    },

    #[error("toml parse error: {0}")]
    Toml(#[from] toml::de::Error),

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("sqlx error: {0}")]
    Sqlx(#[from] sqlx::Error),

    #[error("migration error: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),

    #[error("seed error: {0}")]
    Seed(String),

    #[error("auth error: {0}")]
    Auth(String),

    #[error(
        "azure api error: {message} (code={code:?}, status={status:?}, request_id={request_id:?})"
    )]
    Azure {
        message: String,
        code: Option<String>,
        status: Option<u16>,
        request_id: Option<String>,
        operation_id: Option<String>,
    },

    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("validation error: {0}")]
    Validation(String),

    #[error("not found: {0}")]
    NotFound(String),

    #[error("mcp error: {0}")]
    Mcp(String),

    #[error("internal error: {0}")]
    Internal(String),
}

impl AppError {
    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: Some(path.into()),
            source,
        }
    }

    pub fn io_no_path(source: std::io::Error) -> Self {
        Self::Io { path: None, source }
    }
}
