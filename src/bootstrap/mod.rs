//! Bootstrap and lifecycle for the server process.

pub mod doctor;
pub mod init;
pub mod locale;
pub mod seed;

pub use init::{ensure_initialized, AppState};
