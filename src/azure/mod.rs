//! Azure clients and auth.

pub mod arm;
pub mod auth;
pub mod client;
pub mod identity;
pub mod resource_graph;
pub mod subscriptions;
pub mod support;
pub mod tenants;

pub use auth::{AuthProvider, AuthSource, ChainedAuthProvider, TokenScope};
pub use client::{ArmClient, ArmEndpoints, ArmResponse, AzureErrorBody};
