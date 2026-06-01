//! `GET /providers/Microsoft.Support/services`

use serde::{Deserialize, Serialize};

use crate::azure::client::ArmClient;
use crate::error::AppResult;

pub const SUPPORT_API_VERSION: &str = "2024-04-01";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SupportService {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub properties: SupportServiceProps,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SupportServiceProps {
    #[serde(rename = "displayName", default)]
    pub display_name: Option<String>,
    #[serde(rename = "resourceTypes", default)]
    pub resource_types: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct ListEnvelope<T> {
    value: Vec<T>,
}

pub async fn list_services(arm: &ArmClient) -> AppResult<Vec<SupportService>> {
    let path = format!("/providers/Microsoft.Support/services?api-version={SUPPORT_API_VERSION}");
    let env: ListEnvelope<SupportService> = arm.get_json(&path).await?;
    Ok(env.value)
}
