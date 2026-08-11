//! MCP server adapter (stdio + tool registration).

pub(crate) mod schema;
pub mod server;
pub mod tools;

pub use server::serve_stdio;
