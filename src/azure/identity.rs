//! Best-effort discovery of the *signed-in user* behind the current Azure
//! token. We never call Microsoft Graph (extra permission surface); instead
//! we decode the JWT payload of the ARM access token we already obtained.
//!
//! Works for:
//! * `az login` user tokens (v1 or v2): pulls `upn` / `preferred_username` +
//!   `name` / `given_name` / `family_name`.
//! * Service principal client-credentials tokens: detected via `appid` /
//!   `idtyp == "app"` and `is_service_principal=true` is reported.
//!
//! The decoded payload is **not** signature-verified — we already trust the
//! source (we just obtained it from Entra) and only read self-describing
//! identity claims. We never re-emit the token.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde::Serialize;
use serde_json::Value;

use crate::azure::auth::TokenScope;
use crate::azure::AuthProvider;
use crate::error::AppResult;

#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct SignedInUser {
    /// User Principal Name (e.g. `alice@contoso.com`) when present.
    pub user_principal_name: Option<String>,
    /// Best-effort display name (e.g. `Alice Example`).
    pub display_name: Option<String>,
    pub given_name: Option<String>,
    pub family_name: Option<String>,
    /// Object ID (`oid` claim).
    pub object_id: Option<String>,
    /// Tenant ID (`tid` claim).
    pub tenant_id: Option<String>,
    /// True when the token was issued to a service principal / app and
    /// therefore has no human UPN.
    pub is_service_principal: bool,
    /// Where the identity came from: `"token_claims"` today.
    pub source: String,
}

impl SignedInUser {
    pub fn empty(reason: &str) -> Self {
        Self {
            user_principal_name: None,
            display_name: None,
            given_name: None,
            family_name: None,
            object_id: None,
            tenant_id: None,
            is_service_principal: false,
            source: reason.to_string(),
        }
    }
}

/// Acquire an ARM token from the chain and decode self-describing claims.
pub async fn discover(provider: &dyn AuthProvider) -> AppResult<SignedInUser> {
    let token = provider.get_token(TokenScope::Arm).await?;
    Ok(parse_jwt_identity(&token.value))
}

/// Pure: decode the middle segment of `aaa.bbb.ccc` and pull identity claims.
/// Returns an `empty("malformed_token")` placeholder rather than failing — the
/// caller (a UX-affordance feature, not auth) should never break the flow.
pub fn parse_jwt_identity(jwt: &str) -> SignedInUser {
    let Some(payload_b64) = jwt.split('.').nth(1) else {
        return SignedInUser::empty("malformed_token");
    };
    let Ok(bytes) = URL_SAFE_NO_PAD.decode(payload_b64) else {
        return SignedInUser::empty("malformed_token");
    };
    let claims: Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(_) => return SignedInUser::empty("malformed_token"),
    };

    let s =
        |k: &str| -> Option<String> { claims.get(k).and_then(|v| v.as_str()).map(str::to_string) };

    let upn = s("upn")
        .or_else(|| s("preferred_username"))
        .or_else(|| s("unique_name"));
    let upn_is_email = upn.as_deref().is_some_and(|u| u.contains('@'));
    let upn_email = if upn_is_email { upn.clone() } else { None };

    let given_name = s("given_name");
    let family_name = s("family_name");
    let display_name = s("name").or_else(|| match (&given_name, &family_name) {
        (Some(g), Some(f)) => Some(format!("{g} {f}")),
        _ => None,
    });

    let is_sp = s("idtyp").as_deref() == Some("app")
        || (s("appid").is_some() && upn_email.is_none() && given_name.is_none());

    SignedInUser {
        user_principal_name: upn_email,
        display_name,
        given_name,
        family_name,
        object_id: s("oid"),
        tenant_id: s("tid"),
        is_service_principal: is_sp,
        source: "token_claims".into(),
    }
}

/// Derive `(first_name, last_name)` from a UPN local part like `alice.example@...`.
/// Returns `None` if heuristic can't infer both parts confidently.
pub fn names_from_upn(upn: &str) -> Option<(String, String)> {
    let local = upn.split('@').next()?;
    let parts: Vec<&str> = local
        .split(['.', '_', '-'])
        .filter(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_alphabetic()))
        .collect();
    if parts.len() < 2 {
        return None;
    }
    Some((capitalize(parts[0]), capitalize(parts[parts.len() - 1])))
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_ascii_uppercase().to_string() + chars.as_str(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_jwt(payload: &Value) -> String {
        let header = URL_SAFE_NO_PAD.encode(b"{\"alg\":\"none\"}");
        let body = URL_SAFE_NO_PAD.encode(serde_json::to_vec(payload).unwrap());
        format!("{header}.{body}.sig")
    }

    #[test]
    fn parses_user_token_with_upn() {
        let jwt = make_jwt(&serde_json::json!({
            "upn": "alice.example@contoso.com",
            "given_name": "Alice",
            "family_name": "Example",
            "name": "Alice Example",
            "oid": "11111111-1111-1111-1111-111111111111",
            "tid": "22222222-2222-2222-2222-222222222222"
        }));
        let id = parse_jwt_identity(&jwt);
        assert_eq!(
            id.user_principal_name.as_deref(),
            Some("alice.example@contoso.com")
        );
        assert_eq!(id.given_name.as_deref(), Some("Alice"));
        assert_eq!(id.family_name.as_deref(), Some("Example"));
        assert_eq!(id.display_name.as_deref(), Some("Alice Example"));
        assert!(!id.is_service_principal);
    }

    #[test]
    fn parses_v2_token_preferred_username() {
        let jwt = make_jwt(&serde_json::json!({
            "preferred_username": "bob@contoso.com",
            "name": "Bob Q"
        }));
        let id = parse_jwt_identity(&jwt);
        assert_eq!(id.user_principal_name.as_deref(), Some("bob@contoso.com"));
        assert!(!id.is_service_principal);
    }

    #[test]
    fn detects_service_principal() {
        let jwt = make_jwt(&serde_json::json!({
            "appid": "33333333-3333-3333-3333-333333333333",
            "idtyp": "app",
            "oid": "44444444-4444-4444-4444-444444444444",
            "tid": "22222222-2222-2222-2222-222222222222"
        }));
        let id = parse_jwt_identity(&jwt);
        assert!(id.is_service_principal);
        assert!(id.user_principal_name.is_none());
    }

    #[test]
    fn ignores_non_email_upn() {
        let jwt = make_jwt(&serde_json::json!({"upn": "not-an-email"}));
        let id = parse_jwt_identity(&jwt);
        assert!(id.user_principal_name.is_none());
    }

    #[test]
    fn malformed_returns_empty() {
        let id = parse_jwt_identity("not-a-jwt");
        assert_eq!(id.source, "malformed_token");
    }

    #[test]
    fn names_from_upn_dotted() {
        assert_eq!(
            names_from_upn("alice.example@contoso.com"),
            Some(("Alice".into(), "Example".into()))
        );
        assert_eq!(
            names_from_upn("a.b.c@x.com"),
            Some(("A".into(), "C".into()))
        );
    }

    #[test]
    fn names_from_upn_single_no_guess() {
        assert_eq!(names_from_upn("alice@contoso.com"), None);
    }
}
