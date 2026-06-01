use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::bootstrap::AppState;
use crate::error::AppResult;

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct Input {
    /// If true, actually attempt token acquisition. Defaults to false
    /// (configuration-only report, never touches the network).
    #[serde(default)]
    pub probe_token: bool,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Output {
    pub cloud: String,
    pub configured_sources: Vec<String>,
    pub az_cli_available: bool,
    pub probed: bool,
    pub authenticated: Option<bool>,
    pub winning_source: Option<String>,
    pub message: String,
}

pub async fn run(state: &AppState, input: Input) -> AppResult<Output> {
    let cloud = state.config.general.cloud.clone();
    let az_cli_available = which::which("az").is_ok();

    let mut configured = Vec::new();
    if state.config.auth.prefer == "env" || state.config.auth.prefer == "az_cli" {
        configured.push("env_client_secret".to_string());
    }
    if state.config.auth.allow_az_cli_fallback && az_cli_available {
        configured.push("azure_cli".to_string());
    }

    if !input.probe_token {
        return Ok(Output {
            cloud,
            configured_sources: configured.clone(),
            az_cli_available,
            probed: false,
            authenticated: None,
            winning_source: None,
            message: format!(
                "Configured sources: {}. Pass {{\"probe_token\": true}} to verify token acquisition.",
                configured.join(", ")
            ),
        });
    }

    let (_client, chain) = super::arm_for(state)?;
    let chain: std::sync::Arc<dyn crate::azure::AuthProvider> = chain;
    match chain.get_token(crate::azure::auth::TokenScope::Arm).await {
        Ok(t) => Ok(Output {
            cloud,
            configured_sources: configured,
            az_cli_available,
            probed: true,
            authenticated: Some(true),
            winning_source: Some(format!("{:?}", t.source)),
            message: "Authenticated.".into(),
        }),
        Err(e) => Ok(Output {
            cloud,
            configured_sources: configured,
            az_cli_available,
            probed: true,
            authenticated: Some(false),
            winning_source: None,
            message: format!("{e}"),
        }),
    }
}
