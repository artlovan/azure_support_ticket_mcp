//! `GET /tenants` listing.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::AppResult;

use super::client::ArmClient;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Tenant {
    #[serde(rename = "tenantId")]
    pub tenant_id: String,
    #[serde(rename = "displayName", default)]
    pub display_name: Option<String>,
    #[serde(rename = "defaultDomain", default)]
    pub default_domain: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ListEnvelope<T> {
    value: Vec<T>,
}

pub async fn list_tenants(arm: &ArmClient) -> AppResult<Vec<Tenant>> {
    let env: ListEnvelope<Tenant> = arm.get_json("/tenants?api-version=2022-12-01").await?;
    Ok(env.value)
}
