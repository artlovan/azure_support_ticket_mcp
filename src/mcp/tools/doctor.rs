use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::bootstrap::AppState;
use crate::error::AppResult;

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct Input {}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Output {
    pub app_dir: String,
    pub cache_path: String,
    pub cloud: String,
    pub services_in_cache: i64,
    pub seed_version: Option<String>,
    pub az_cli_present: bool,
    pub arm_reachable: bool,
    pub message: String,
}

pub async fn run(state: &AppState) -> AppResult<Output> {
    let services_in_cache = state.cache.support_services_count().await.unwrap_or(-1);
    let seed = state.cache.seed_meta().await.ok().and_then(|m| m.version);
    let az_cli_present = which::which("az").is_ok();

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(crate::error::AppError::Http)?;
    let arm_reachable = client
        .head("https://management.azure.com/")
        .send()
        .await
        .map(|r| r.status().as_u16() < 500)
        .unwrap_or(false);

    Ok(Output {
        app_dir: state.config.app_dir().display().to_string(),
        cache_path: state.config.cache.path.display().to_string(),
        cloud: state.config.general.cloud.clone(),
        services_in_cache,
        seed_version: seed,
        az_cli_present,
        arm_reachable,
        message: "doctor OK".into(),
    })
}
