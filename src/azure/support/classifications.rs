//! `GET /providers/Microsoft.Support/services/{sid}/problemClassifications`

use serde::{Deserialize, Serialize};

use crate::azure::client::ArmClient;
use crate::error::AppResult;

use super::services::SUPPORT_API_VERSION;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProblemClassification {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub properties: ProblemClassificationProps,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProblemClassificationProps {
    #[serde(rename = "displayName", default)]
    pub display_name: Option<String>,
    #[serde(rename = "parentProblemClassification", default)]
    pub parent: Option<serde_json::Value>,
    #[serde(rename = "secondaryConsentEnabled", default)]
    pub secondary_consent_enabled: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct ListEnvelope<T> {
    value: Vec<T>,
}

pub async fn list_classifications(
    arm: &ArmClient,
    service_id: &str,
) -> AppResult<Vec<ProblemClassification>> {
    let path = format!("{service_id}/problemClassifications?api-version={SUPPORT_API_VERSION}");
    let env: ListEnvelope<ProblemClassification> = arm.get_json(&path).await?;
    Ok(env.value)
}
