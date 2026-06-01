//! Exact resource ID GET against ARM (validation helper).
//! Used by the resolver to confirm a resource exists before drafting.

use crate::error::AppResult;

use super::client::ArmClient;

pub async fn get_resource_raw(
    arm: &ArmClient,
    resource_id: &str,
    api_version: &str,
) -> AppResult<serde_json::Value> {
    let path = format!("{resource_id}?api-version={api_version}");
    arm.get_json::<serde_json::Value>(&path).await
}
