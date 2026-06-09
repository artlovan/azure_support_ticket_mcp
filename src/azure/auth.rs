//! Authentication providers.
//!
//! All providers return an opaque bearer token plus an expiry. The
//! `ChainedAuthProvider` tries each inner provider in order and surfaces an
//! actionable error if all fail.
//!
//! Chain order:
//!   1. Environment credentials (`AZURE_TENANT_ID`/`AZURE_CLIENT_ID`/`AZURE_CLIENT_SECRET`).
//!   2. Azure CLI fallback (`az account get-access-token`).
//!
//! Managed Identity and Workload Identity are scaffolded as future providers.

use std::process::Stdio;
use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;
use serde::Deserialize;
use time::OffsetDateTime;
use tokio::process::Command;
use tracing::{debug, warn};

use crate::error::{AppError, AppResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenScope {
    Arm,
}

impl TokenScope {
    pub fn resource(&self) -> &'static str {
        match self {
            Self::Arm => "https://management.azure.com/",
        }
    }
}

#[derive(Debug, Clone)]
pub struct AccessToken {
    pub value: String,
    pub expires_on: OffsetDateTime,
    pub source: AuthSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthSource {
    EnvClientSecret,
    AzureCli,
}

#[async_trait]
pub trait AuthProvider: Send + Sync {
    fn source(&self) -> AuthSource;
    async fn get_token(&self, scope: TokenScope) -> AppResult<AccessToken>;
}

// -----------------------------------------------------------------------
// Chained provider
// -----------------------------------------------------------------------

pub struct ChainedAuthProvider {
    providers: Vec<Arc<dyn AuthProvider>>,
}

impl ChainedAuthProvider {
    pub fn new(providers: Vec<Arc<dyn AuthProvider>>) -> Self {
        Self { providers }
    }

    pub fn sources(&self) -> Vec<AuthSource> {
        self.providers.iter().map(|p| p.source()).collect()
    }
}

#[async_trait]
impl AuthProvider for ChainedAuthProvider {
    fn source(&self) -> AuthSource {
        // Caller uses returned token's .source for the actual winner.
        self.providers
            .first()
            .map(|p| p.source())
            .unwrap_or(AuthSource::EnvClientSecret)
    }

    async fn get_token(&self, scope: TokenScope) -> AppResult<AccessToken> {
        let mut last_err: Option<AppError> = None;
        for p in &self.providers {
            match p.get_token(scope).await {
                Ok(t) => return Ok(t),
                Err(e) => {
                    debug!(provider = ?p.source(), error = %e, "auth provider failed");
                    last_err = Some(e);
                }
            }
        }
        Err(last_err.unwrap_or_else(|| {
            AppError::Auth(
                "no auth provider configured. Set Azure env credentials or run `az login`.".into(),
            )
        }))
    }
}

// -----------------------------------------------------------------------
// Env credential provider (client_secret)
// -----------------------------------------------------------------------

pub struct EnvCredentialProvider {
    tenant_id: String,
    client_id: String,
    client_secret: String,
    http: reqwest::Client,
    cache: Mutex<Option<AccessToken>>,
}

impl EnvCredentialProvider {
    pub fn from_env() -> Option<Self> {
        let tenant_id = std::env::var("AZURE_TENANT_ID").ok()?;
        let client_id = std::env::var("AZURE_CLIENT_ID").ok()?;
        let client_secret = std::env::var("AZURE_CLIENT_SECRET").ok()?;
        Some(Self {
            tenant_id,
            client_id,
            client_secret,
            http: reqwest::Client::new(),
            cache: Mutex::new(None),
        })
    }
}

#[derive(Debug, Deserialize)]
struct EntraTokenResponse {
    access_token: String,
    expires_in: i64,
}

#[async_trait]
impl AuthProvider for EnvCredentialProvider {
    fn source(&self) -> AuthSource {
        AuthSource::EnvClientSecret
    }

    async fn get_token(&self, scope: TokenScope) -> AppResult<AccessToken> {
        if let Some(t) = cached_token(&self.cache) {
            return Ok(t);
        }

        let url = format!(
            "https://login.microsoftonline.com/{}/oauth2/v2.0/token",
            self.tenant_id
        );
        let scope_value = format!("{}.default", scope.resource());
        let form = [
            ("grant_type", "client_credentials"),
            ("client_id", &self.client_id),
            ("client_secret", &self.client_secret),
            ("scope", &scope_value),
        ];

        let resp = self.http.post(&url).form(&form).send().await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(AppError::Auth(format!(
                "Entra token request failed: HTTP {status}: {body}"
            )));
        }
        let parsed: EntraTokenResponse = resp.json().await?;
        let token = AccessToken {
            value: parsed.access_token,
            expires_on: OffsetDateTime::now_utc() + time::Duration::seconds(parsed.expires_in - 60),
            source: AuthSource::EnvClientSecret,
        };
        *self.cache.lock() = Some(token.clone());
        Ok(token)
    }
}

// -----------------------------------------------------------------------
// Azure CLI fallback
// -----------------------------------------------------------------------

pub struct AzureCliTokenProvider {
    cache: Mutex<Option<AccessToken>>,
}

impl AzureCliTokenProvider {
    pub fn new_if_available() -> Option<Self> {
        if which::which("az").is_ok() {
            Some(Self {
                cache: Mutex::new(None),
            })
        } else {
            None
        }
    }
}

#[derive(Debug, Deserialize)]
struct AzCliToken {
    #[serde(rename = "accessToken")]
    access_token: String,
    #[serde(rename = "expiresOn")]
    expires_on: String, // "YYYY-MM-DD HH:MM:SS.ffffff" local
}

#[async_trait]
impl AuthProvider for AzureCliTokenProvider {
    fn source(&self) -> AuthSource {
        AuthSource::AzureCli
    }

    async fn get_token(&self, scope: TokenScope) -> AppResult<AccessToken> {
        if let Some(t) = cached_token(&self.cache) {
            return Ok(t);
        }

        let out = Command::new("az")
            .args([
                "account",
                "get-access-token",
                "--resource",
                scope.resource(),
                "--output",
                "json",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
            .map_err(|e| AppError::Auth(format!("failed to run az: {e}")))?;

        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            warn!(stderr = %stderr, "az get-access-token failed");
            return Err(AppError::Auth(az_token_error_message(&stderr)));
        }

        let parsed: AzCliToken = serde_json::from_slice(&out.stdout)
            .map_err(|e| AppError::Auth(format!("az output parse failed: {e}")))?;

        // Best-effort expiry parse; fall back to 30 min if local parsing fails.
        let expires_on = parse_az_cli_expiry(&parsed.expires_on)
            .unwrap_or_else(|| OffsetDateTime::now_utc() + time::Duration::minutes(30));
        let token = AccessToken {
            value: parsed.access_token,
            expires_on: expires_on - time::Duration::seconds(60),
            source: AuthSource::AzureCli,
        };
        *self.cache.lock() = Some(token.clone());
        Ok(token)
    }
}

fn parse_az_cli_expiry(s: &str) -> Option<OffsetDateTime> {
    // `az` prints either ISO-8601 or naive local "YYYY-MM-DD HH:MM:SS.ffffff".
    // We avoid depending on `time` local-offset (requires unsafe at runtime on
    // many platforms). If the value is naive, treat it as UTC — tokens are
    // short-lived (~1h) so a few hours of skew still expires sooner than the
    // real token, which is the safe direction.
    use time::format_description::well_known::Iso8601;
    if let Ok(dt) = OffsetDateTime::parse(s, &Iso8601::DEFAULT) {
        return Some(dt);
    }
    let fmt = time::format_description::parse(
        "[year]-[month]-[day] [hour]:[minute]:[second].[subsecond]",
    )
    .ok()?;
    let primitive = time::PrimitiveDateTime::parse(s, &fmt).ok()?;
    Some(primitive.assume_utc())
}

fn cached_token(cache: &Mutex<Option<AccessToken>>) -> Option<AccessToken> {
    let g = cache.lock();
    if let Some(t) = g.as_ref() {
        if t.expires_on > OffsetDateTime::now_utc() {
            return Some(t.clone());
        }
    }
    None
}

/// Build the default chained provider based on config.
pub fn build_default_chain(allow_az_cli_fallback: bool) -> AppResult<ChainedAuthProvider> {
    let mut chain: Vec<Arc<dyn AuthProvider>> = Vec::new();

    if let Some(env) = EnvCredentialProvider::from_env() {
        chain.push(Arc::new(env));
    }
    if allow_az_cli_fallback {
        if let Some(cli) = AzureCliTokenProvider::new_if_available() {
            chain.push(Arc::new(cli));
        }
    }
    if chain.is_empty() {
        return Err(AppError::Auth(
            "no Azure credentials available. Set AZURE_TENANT_ID/AZURE_CLIENT_ID/AZURE_CLIENT_SECRET, or run `az login`.".into(),
        ));
    }
    Ok(ChainedAuthProvider::new(chain))
}

// -----------------------------------------------------------------------
// Actionable error messages
// -----------------------------------------------------------------------

/// Build an actionable error message from a failed `az account get-access-token`
/// invocation. Any such failure (not logged in, MSAL token-cache miss, wrong
/// active subscription) is fixable by re-authenticating and/or selecting the
/// right subscription, so we always append concrete remediation steps rather
/// than echoing the raw (often cryptic) CLI output on its own.
fn az_token_error_message(stderr: &str) -> String {
    let first = stderr
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("az get-access-token failed");
    format!(
        "Azure CLI could not provide a token ({first}). Run `az login` to sign in, \
         then `az account set --subscription <id>` to select the correct \
         tenant/subscription, and retry."
    )
}

// -----------------------------------------------------------------------
// Local az CLI context (no network): `az account show`
// -----------------------------------------------------------------------

/// The Azure CLI's currently-active account context, read from the local
/// on-disk profile via `az account show`. Used to disclose which tenant and
/// subscription the CLI fallback would act in, without acquiring a token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AzActiveAccount {
    pub tenant_id: Option<String>,
    pub subscription_id: Option<String>,
    pub subscription_name: Option<String>,
    pub user: Option<String>,
}

/// Pure parse of `az account show --output json` output. Returns `None` when
/// the JSON is unparseable or carries neither a tenant nor a subscription.
pub fn parse_az_account_show(json: &[u8]) -> Option<AzActiveAccount> {
    let v: serde_json::Value = serde_json::from_slice(json).ok()?;
    let s = |k: &str| v.get(k).and_then(|x| x.as_str()).map(str::to_string);
    let acct = AzActiveAccount {
        tenant_id: s("tenantId"),
        subscription_id: s("id"),
        subscription_name: s("name"),
        user: v
            .get("user")
            .and_then(|u| u.get("name"))
            .and_then(|x| x.as_str())
            .map(str::to_string),
    };
    if acct.tenant_id.is_none() && acct.subscription_id.is_none() {
        return None;
    }
    Some(acct)
}

/// Read the active az CLI account context. Local-only: reads the on-disk az
/// profile and does not touch the network. Returns `None` when `az` is absent,
/// not logged in, or the output cannot be parsed.
pub async fn az_active_account() -> Option<AzActiveAccount> {
    if which::which("az").is_err() {
        return None;
    }
    let out = Command::new("az")
        .args(["account", "show", "--output", "json"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .ok()?;
    if !out.status.success() {
        return None;
    }
    parse_az_account_show(&out.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn az_token_error_is_actionable() {
        let msg = az_token_error_message(
            "ERROR: Please run 'az login' to setup account.\ntrailing noise",
        );
        assert!(msg.contains("az login"));
        assert!(msg.contains("az account set --subscription"));
        // Surfaces the original first line for context.
        assert!(msg.contains("Please run 'az login' to setup account."));
    }

    #[test]
    fn az_token_error_handles_empty_stderr() {
        let msg = az_token_error_message("   \n\n");
        assert!(msg.contains("az get-access-token failed"));
        assert!(msg.contains("az login"));
    }

    #[test]
    fn parse_az_account_show_extracts_context() {
        let json = br#"{
            "environmentName": "AzureCloud",
            "id": "00000000-0000-0000-0000-000000000001",
            "isDefault": true,
            "name": "My Subscription",
            "state": "Enabled",
            "tenantId": "11111111-1111-1111-1111-111111111111",
            "user": { "name": "tester@contoso.com", "type": "user" }
        }"#;
        let acct = parse_az_account_show(json).expect("should parse");
        assert_eq!(
            acct.tenant_id.as_deref(),
            Some("11111111-1111-1111-1111-111111111111")
        );
        assert_eq!(
            acct.subscription_id.as_deref(),
            Some("00000000-0000-0000-0000-000000000001")
        );
        assert_eq!(acct.subscription_name.as_deref(), Some("My Subscription"));
        assert_eq!(acct.user.as_deref(), Some("tester@contoso.com"));
    }

    #[test]
    fn parse_az_account_show_rejects_empty_or_garbage() {
        assert!(parse_az_account_show(b"{}").is_none());
        assert!(parse_az_account_show(b"not json").is_none());
    }
}
