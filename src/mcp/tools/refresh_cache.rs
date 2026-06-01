use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::azure::support::services::list_services;
use crate::bootstrap::AppState;
use crate::cache::{now_unix, ProblemClassificationRow, SupportServiceRow};
use crate::error::{AppError, AppResult};

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct Input {
    /// `"services"` or `"classifications"`.
    #[serde(default = "default_target")]
    pub target: String,
    /// Required when target = `"classifications"`.
    #[serde(default)]
    pub service_id: Option<String>,
}

fn default_target() -> String {
    "services".into()
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Output {
    pub target: String,
    pub refreshed: usize,
    pub source: &'static str,
    pub message: String,
}

pub async fn run(state: &AppState, input: Input) -> AppResult<Output> {
    match input.target.as_str() {
        "services" => {
            let (arm, _chain) = super::arm_for(state)?;
            let services = list_services(&arm).await?;
            let cloud = state.cache.cloud().to_string();
            let now = now_unix();
            let count = services.len();
            for s in services {
                let display = s
                    .properties
                    .display_name
                    .clone()
                    .unwrap_or_else(|| s.name.clone());
                let resource_types_json = if s.properties.resource_types.is_empty() {
                    None
                } else {
                    Some(serde_json::to_string(&s.properties.resource_types)?)
                };
                let row = SupportServiceRow {
                    cloud: cloud.clone(),
                    service_id: s.id,
                    name: s.name,
                    display_name: display,
                    service_group: None,
                    resource_types_json,
                    metadata_json: None,
                    source: "live".into(),
                    updated_at: now,
                    etag: None,
                };
                state.cache.upsert_support_service(&row).await?;
            }
            state
                .cache
                .record_refresh_success("support_services")
                .await?;
            Ok(Output {
                target: input.target,
                refreshed: count,
                source: "live",
                message: format!("Refreshed {count} support services from Azure."),
            })
        }
        "classifications" => {
            let sid = input.service_id.clone().ok_or_else(|| {
                AppError::Validation(
                    "service_id is required when target = 'classifications'".into(),
                )
            })?;
            let (arm, _chain) = super::arm_for(state)?;
            let fetched =
                crate::azure::support::classifications::list_classifications(&arm, &sid).await?;
            let cloud = state.cache.cloud().to_string();
            let now = now_unix();
            let count = fetched.len();
            for c in fetched {
                let row = ProblemClassificationRow {
                    cloud: cloud.clone(),
                    service_id: sid.clone(),
                    classification_id: c.id,
                    display_name: c
                        .properties
                        .display_name
                        .clone()
                        .unwrap_or_else(|| c.name.clone()),
                    parent_id: None,
                    metadata_json: None,
                    updated_at: now,
                    etag: None,
                };
                state.cache.upsert_classification(&row).await?;
            }
            Ok(Output {
                target: input.target,
                refreshed: count,
                source: "live",
                message: format!("Refreshed {count} classifications for {sid}."),
            })
        }
        other => Err(AppError::Validation(format!(
            "target must be 'services' or 'classifications', got '{other}'"
        ))),
    }
}
