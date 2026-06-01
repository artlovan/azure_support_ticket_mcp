use serde::{Deserialize, Serialize};

/// Row mirror of the `support_services` table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SupportServiceRow {
    pub cloud: String,
    pub service_id: String,
    pub name: String,
    pub display_name: String,
    pub service_group: Option<String>,
    /// JSON-encoded list of resource types.
    pub resource_types_json: Option<String>,
    pub metadata_json: Option<String>,
    pub source: String,
    pub updated_at: i64,
    pub etag: Option<String>,
}

impl SupportServiceRow {
    pub fn resource_types(&self) -> Vec<String> {
        self.resource_types_json
            .as_deref()
            .and_then(|s| serde_json::from_str::<Vec<String>>(s).ok())
            .unwrap_or_default()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProblemClassificationRow {
    pub cloud: String,
    pub service_id: String,
    pub classification_id: String,
    pub display_name: String,
    pub parent_id: Option<String>,
    pub metadata_json: Option<String>,
    pub updated_at: i64,
    pub etag: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CacheStatus {
    Fresh,
    Stale,
    Missing,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SeedMeta {
    pub version: Option<String>,
    pub sha256: Option<String>,
    pub source: Option<String>,
    pub loaded_at: Option<i64>,
}

/// Cache key for the "all support services for cloud X" refresh registry.
pub fn support_services_key(cloud: &str) -> String {
    format!("support_services::{cloud}")
}

/// Cache key for "classifications for service Y in cloud X".
pub fn classifications_key(cloud: &str, service_id: &str) -> String {
    format!("classifications::{cloud}::{service_id}")
}

pub const SUPPORT_SERVICES_KEY: &str = "support_services";
