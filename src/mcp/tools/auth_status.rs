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
    /// Tenant ID from `AZURE_TENANT_ID` (env client-secret path), if set.
    pub env_tenant_id: Option<String>,
    /// Active az CLI context, read locally from `az account show` (no network).
    pub active_tenant_id: Option<String>,
    pub active_subscription_id: Option<String>,
    pub active_subscription_name: Option<String>,
    pub active_user: Option<String>,
    pub probed: bool,
    pub authenticated: Option<bool>,
    pub winning_source: Option<String>,
    pub message: String,
}

/// Build the "subscription X in tenant Y" phrase from the active az CLI
/// context. Returns an empty string when neither is known. No leading space
/// or trailing punctuation, so callers can embed it in a larger sentence.
fn az_context_phrase(
    active_tenant_id: Option<&str>,
    active_subscription_id: Option<&str>,
    active_subscription_name: Option<&str>,
) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(sub_id) = active_subscription_id {
        let label = match active_subscription_name {
            Some(name) => format!("subscription {name} ({sub_id})"),
            None => format!("subscription {sub_id}"),
        };
        parts.push(label);
    }
    if let Some(tid) = active_tenant_id {
        parts.push(format!("tenant {tid}"));
    }
    parts.join(" in ")
}

/// Build a human-readable context fragment describing which identity will be
/// used and in which tenant/subscription. Returns an empty string when nothing
/// is known; otherwise always begins with a leading space so callers can
/// concatenate it directly.
///
/// When a service principal is configured (env credentials), it wins the auth
/// chain and is single-tenant by nature, so we lead with it and demote any az
/// CLI context to a parenthetical. Otherwise the az CLI fallback is what will
/// be used, so we foreground its active context.
fn describe_auth_context(
    sp_configured: bool,
    env_tenant_id: Option<&str>,
    active_tenant_id: Option<&str>,
    active_subscription_id: Option<&str>,
    active_subscription_name: Option<&str>,
) -> String {
    let az_ctx = az_context_phrase(
        active_tenant_id,
        active_subscription_id,
        active_subscription_name,
    );

    if sp_configured {
        let tenant = env_tenant_id.unwrap_or("<AZURE_TENANT_ID>");
        let mut out = format!(
            " Primary auth: service principal in tenant {tenant}; subscriptions are selected per request (no default), and a service principal acts only within this one tenant."
        );
        if !az_ctx.is_empty() {
            out.push_str(&format!(
                " (Azure CLI is also signed in to {az_ctx}, but the service principal takes precedence.)"
            ));
        }
        return out;
    }

    let mut out = String::new();
    if !az_ctx.is_empty() {
        out.push_str(&format!(" Active az CLI context: {az_ctx}."));
    }
    if let Some(env_tid) = env_tenant_id {
        // Only call out the env tenant when it differs from the active az
        // tenant, to avoid restating the same ID twice.
        if active_tenant_id != Some(env_tid) {
            out.push_str(&format!(" Env AZURE_TENANT_ID: {env_tid}."));
        }
    }
    out
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

    let env_tenant_id = std::env::var("AZURE_TENANT_ID").ok();
    // Mirror EnvCredentialProvider::from_env: a service principal is active
    // only when all three credentials are present, in which case it wins the
    // auth chain ahead of the az CLI fallback.
    let sp_configured = env_tenant_id.is_some()
        && std::env::var("AZURE_CLIENT_ID").is_ok()
        && std::env::var("AZURE_CLIENT_SECRET").is_ok();

    // Local-only disclosure of the active az CLI context. `az account show`
    // reads the on-disk profile and does not touch the network, so this is
    // safe to do even in the non-probe path.
    let active = if az_cli_available {
        crate::azure::auth::az_active_account().await
    } else {
        None
    };
    let active_tenant_id = active.as_ref().and_then(|a| a.tenant_id.clone());
    let active_subscription_id = active.as_ref().and_then(|a| a.subscription_id.clone());
    let active_subscription_name = active.as_ref().and_then(|a| a.subscription_name.clone());
    let active_user = active.as_ref().and_then(|a| a.user.clone());

    let context = describe_auth_context(
        sp_configured,
        env_tenant_id.as_deref(),
        active_tenant_id.as_deref(),
        active_subscription_id.as_deref(),
        active_subscription_name.as_deref(),
    );

    if !input.probe_token {
        let sources = if configured.is_empty() {
            "none".to_string()
        } else {
            configured.join(", ")
        };
        return Ok(Output {
            cloud,
            configured_sources: configured,
            az_cli_available,
            env_tenant_id,
            active_tenant_id,
            active_subscription_id,
            active_subscription_name,
            active_user,
            probed: false,
            authenticated: None,
            winning_source: None,
            message: format!(
                "Configured sources: {sources}.{context} Pass {{\"probe_token\": true}} to verify token acquisition."
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
            env_tenant_id,
            active_tenant_id,
            active_subscription_id,
            active_subscription_name,
            active_user,
            probed: true,
            authenticated: Some(true),
            winning_source: Some(format!("{:?}", t.source)),
            message: format!("Authenticated.{context}"),
        }),
        Err(e) => Ok(Output {
            cloud,
            configured_sources: configured,
            az_cli_available,
            env_tenant_id,
            active_tenant_id,
            active_subscription_id,
            active_subscription_name,
            active_user,
            probed: true,
            authenticated: Some(false),
            winning_source: None,
            message: format!("{e}"),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn az_phrase_with_sub_and_tenant() {
        let p = az_context_phrase(
            Some("11111111-1111-1111-1111-111111111111"),
            Some("00000000-0000-0000-0000-000000000001"),
            Some("My Sub"),
        );
        assert_eq!(
            p,
            "subscription My Sub (00000000-0000-0000-0000-000000000001) in tenant 11111111-1111-1111-1111-111111111111"
        );
    }

    #[test]
    fn az_phrase_empty_when_unknown() {
        assert_eq!(az_context_phrase(None, None, None), "");
    }

    #[test]
    fn sp_context_leads_with_service_principal() {
        let s = describe_auth_context(
            true,
            Some("aaaa-tenant"),
            Some("bbbb-az-tenant"),
            Some("sub-b"),
            Some("Sub B"),
        );
        assert!(s.contains("Primary auth: service principal in tenant aaaa-tenant"));
        assert!(s.contains("selected per request"));
        assert!(s.contains("acts only within this one tenant"));
        // az CLI context is demoted to a parenthetical, not foregrounded.
        assert!(s.contains("the service principal takes precedence"));
        assert!(s.find("service principal").unwrap() < s.find("Azure CLI is also").unwrap());
    }

    #[test]
    fn sp_context_without_az_cli_omits_secondary() {
        let s = describe_auth_context(true, Some("aaaa-tenant"), None, None, None);
        assert!(s.contains("Primary auth: service principal in tenant aaaa-tenant"));
        assert!(!s.contains("Azure CLI is also"));
    }

    #[test]
    fn az_login_context_foregrounds_az() {
        let s = describe_auth_context(
            false,
            None,
            Some("11111111-1111-1111-1111-111111111111"),
            Some("sub-id"),
            Some("My Sub"),
        );
        assert!(s.contains(
            "Active az CLI context: subscription My Sub (sub-id) in tenant 11111111-1111-1111-1111-111111111111"
        ));
        assert!(!s.contains("service principal"));
        assert!(s.starts_with(' '));
    }

    #[test]
    fn az_login_context_mentions_env_tenant_when_different() {
        let s = describe_auth_context(
            false,
            Some("22222222-2222-2222-2222-222222222222"),
            Some("11111111-1111-1111-1111-111111111111"),
            Some("sub-id"),
            None,
        );
        assert!(s.contains("Env AZURE_TENANT_ID: 22222222-2222-2222-2222-222222222222"));
    }

    #[test]
    fn az_login_context_hides_env_tenant_when_same() {
        let s = describe_auth_context(
            false,
            Some("11111111-1111-1111-1111-111111111111"),
            Some("11111111-1111-1111-1111-111111111111"),
            None,
            None,
        );
        assert!(!s.contains("Env AZURE_TENANT_ID"));
    }

    #[test]
    fn context_empty_when_nothing_known() {
        assert_eq!(describe_auth_context(false, None, None, None, None), "");
    }
}
