//! `GET /subscriptions` listing.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::AppResult;

use super::client::ArmClient;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Subscription {
    #[serde(rename = "subscriptionId")]
    pub subscription_id: String,
    #[serde(rename = "displayName")]
    pub display_name: String,
    #[serde(rename = "tenantId", default)]
    pub tenant_id: Option<String>,
    #[serde(default)]
    pub state: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ListEnvelope<T> {
    value: Vec<T>,
}

pub async fn list_subscriptions(arm: &ArmClient) -> AppResult<Vec<Subscription>> {
    let env: ListEnvelope<Subscription> = arm
        .get_json("/subscriptions?api-version=2022-12-01")
        .await?;
    Ok(env.value)
}

/// Fetch a single subscription by ID. Used to backfill `tenant_id` on drafts
/// when the user names a subscription without first calling list_subscriptions.
pub async fn get_subscription(arm: &ArmClient, subscription_id: &str) -> AppResult<Subscription> {
    arm.get_json(&format!(
        "/subscriptions/{subscription_id}?api-version=2022-12-01"
    ))
    .await
}
