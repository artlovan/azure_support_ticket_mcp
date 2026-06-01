//! `azure-support-ticket-mcp` library crate.
//!
//! The binary is a thin entrypoint; all logic lives here so it can be
//! exercised by integration tests without spawning the process.

pub mod azure;
pub mod bootstrap;
pub mod cache;
pub mod config;
pub mod error;
pub mod mcp;
pub mod resolver;
pub mod workflow;

pub use error::{AppError, AppResult};
