//! Issue context shared across resolver passes.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IssueContext {
    pub tenant_id: Option<String>,
    pub subscription_id: Option<String>,
    pub user_input: Option<String>,
    pub resource_id: Option<String>,
    pub resource_name: Option<String>,
    pub portal_url: Option<String>,
    pub error_text: Option<String>,
}
