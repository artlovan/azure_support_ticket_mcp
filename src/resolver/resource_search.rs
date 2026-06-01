//! Resource search by name/hint.
//!
//! Closes the long-standing gap between `docs/ARCHITECTURE.md` §15 step 4
//! ("Exact resource name within selected subscription → Resource Graph
//! search") and the previous code, which only handled steps 1 (full ARM ID)
//! and 2 (portal URL).
//!
//! ## Multi-pass strategy
//!
//! A single KQL query with an `or` clause would work, but ranks badly: an
//! exact match and a substring match would tie. Instead we run up to three
//! passes and tag each candidate with the pass that found it, so the caller
//! can present results as "best match (exact name), then alternatives
//! (substring), then fallback (id contains)":
//!
//!   1. **Name exact** — `where name =~ "<hint>"` (case-insensitive equality).
//!      Highest confidence. Wins almost always when present.
//!   2. **Name substring** — `where name contains "<hint>"` (case-insensitive).
//!      Catches resources whose canonical name embeds the user's hint plus a
//!      suffix or prefix. **This is the pass that finds Azure AD B2C
//!      directories**, which have `name = "<friendly>.onmicrosoft.com"`
//!      while users naturally type just `<friendly>`. Also catches DNS
//!      zones, KeyVault URIs, Front Door endpoints — anywhere a friendly
//!      prefix is part of a longer canonical name.
//!   3. **ID substring** — `where id contains "<hint>"` (case-insensitive).
//!      Last-resort pass for cases where the hint appears in the resource
//!      group name or another ARM path component but not in the resource's
//!      own `name`. Lowest confidence.
//!
//! Each pass excludes matches already found by an earlier pass (via KQL
//! `not(...)` guards), so deduplication is server-side.
//!
//! ## KQL injection
//!
//! User-supplied hints land directly inside a KQL string literal. We sanitize
//! by stripping characters that could break out of the literal or pivot the
//! query — single quotes, double quotes, backslash, newlines, KQL pipe.
//! Anything left is alphanumeric, dashes, dots, underscores — exactly the
//! character set Azure resource names use. See [`sanitize_hint`].

use serde::Serialize;

use crate::azure::client::ArmClient;
use crate::azure::resource_graph::{query, ResourceRow};
use crate::error::AppResult;

/// Per-candidate cap on rows returned by each pass. We dedupe across passes,
/// so the final candidate list is at most `MAX_PER_PASS * 3` long before
/// deduplication.
const MAX_PER_PASS: usize = 5;

/// Final cap on candidates returned to the caller after dedup. Keeps the
/// picker UI tight even if all three passes return their full quota.
const MAX_TOTAL: usize = 8;

/// A ranked resource match.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResourceCandidate {
    /// Full ARM resource ID (e.g. `/subscriptions/.../resourceGroups/foo`).
    pub id: String,
    /// Canonical resource name as Azure stores it (may differ from the
    /// user-friendly hint — e.g. `oncontoso-b2c.onmicrosoft.com` for a B2C
    /// directory the user called `contoso-b2c`).
    pub name: String,
    /// Provider/type, e.g. `microsoft.containerservice/managedclusters`.
    pub resource_type: String,
    /// Resource group, if applicable (None for subscription-scoped resources).
    pub resource_group: Option<String>,
    /// Subscription this resource lives in.
    pub subscription_id: Option<String>,
    /// Which pass produced this match. Used to rank + explain.
    pub match_reason: MatchReason,
}

/// Why a candidate matched — surfaced to the user so they can judge
/// confidence at a glance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchReason {
    /// `name =~ "<hint>"` — strongest signal.
    NameExact,
    /// `name contains "<hint>"` — strong; B2C / DNS / etc.
    NameContains,
    /// `id contains "<hint>"` — weakest; hint appears in a path component.
    IdContains,
}

impl MatchReason {
    pub fn label(self) -> &'static str {
        match self {
            Self::NameExact => "name exact",
            Self::NameContains => "name contains",
            Self::IdContains => "id contains",
        }
    }
}

/// Search Azure Resource Graph for resources matching `hint`, scoped to
/// `subscriptions` if provided.
///
/// Returns an empty `Vec` if the sanitized hint is too short to be useful
/// (< 2 chars) — no point hitting Resource Graph for a single character.
///
/// Errors propagate from the underlying `query` call (auth, network, RG
/// quota, etc.) — the caller decides how to surface them.
pub async fn search_resources_by_hint(
    arm: &ArmClient,
    hint: &str,
    subscriptions: Option<&[String]>,
) -> AppResult<Vec<ResourceCandidate>> {
    let safe_hint = sanitize_hint(hint);
    if safe_hint.chars().count() < 2 {
        return Ok(Vec::new());
    }

    let mut candidates: Vec<ResourceCandidate> = Vec::new();
    let mut seen_ids: std::collections::HashSet<String> = std::collections::HashSet::new();

    // Pass 1: exact name match.
    let pass1 = format!(
        "Resources | where name =~ '{safe_hint}' \
         | project id, name, type, resourceGroup, subscriptionId \
         | limit {MAX_PER_PASS}"
    );
    let result = query(arm, &pass1, subscriptions).await?;
    extend_dedup(
        &mut candidates,
        &mut seen_ids,
        result.rows,
        MatchReason::NameExact,
    );

    // Pass 2: name substring (excluding exact already captured).
    if candidates.len() < MAX_TOTAL {
        let pass2 = format!(
            "Resources \
             | where name contains '{safe_hint}' and not(name =~ '{safe_hint}') \
             | project id, name, type, resourceGroup, subscriptionId \
             | limit {MAX_PER_PASS}"
        );
        let result = query(arm, &pass2, subscriptions).await?;
        extend_dedup(
            &mut candidates,
            &mut seen_ids,
            result.rows,
            MatchReason::NameContains,
        );
    }

    // Pass 3: id substring (excluding anything already matched by name).
    if candidates.len() < MAX_TOTAL {
        let pass3 = format!(
            "Resources \
             | where id contains '{safe_hint}' and not(name contains '{safe_hint}') \
             | project id, name, type, resourceGroup, subscriptionId \
             | limit {MAX_PER_PASS}"
        );
        let result = query(arm, &pass3, subscriptions).await?;
        extend_dedup(
            &mut candidates,
            &mut seen_ids,
            result.rows,
            MatchReason::IdContains,
        );
    }

    candidates.truncate(MAX_TOTAL);
    Ok(candidates)
}

fn extend_dedup(
    out: &mut Vec<ResourceCandidate>,
    seen: &mut std::collections::HashSet<String>,
    rows: Vec<ResourceRow>,
    reason: MatchReason,
) {
    for row in rows {
        let Some(id) = row.id.clone() else { continue };
        if !seen.insert(id.clone()) {
            continue;
        }
        out.push(ResourceCandidate {
            id,
            name: row.name.unwrap_or_default(),
            resource_type: row.resource_type.unwrap_or_default(),
            resource_group: row.resource_group,
            subscription_id: row.subscription_id,
            match_reason: reason,
        });
        if out.len() >= MAX_TOTAL {
            break;
        }
    }
}

/// Strip characters that could break out of a KQL string literal or pivot
/// the query. We keep only alphanumeric, `-`, `_`, `.`, which is exactly
/// the character set Azure resource names use.
///
/// Returns an empty string for hints that would entirely sanitize away
/// (callers should treat empty as "no search").
fn sanitize_hint(hint: &str) -> String {
    hint.chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::azure::auth::{AccessToken, AuthProvider, AuthSource, TokenScope};
    use crate::azure::client::{ArmClient, ArmEndpoints};
    use serde_json::json;
    use std::sync::Arc;
    use time::{Duration, OffsetDateTime};
    use wiremock::matchers::{body_partial_json, method, path_regex};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    struct DummyAuth;
    #[async_trait::async_trait]
    impl AuthProvider for DummyAuth {
        fn source(&self) -> AuthSource {
            AuthSource::EnvClientSecret
        }
        async fn get_token(&self, _scope: TokenScope) -> AppResult<AccessToken> {
            Ok(AccessToken {
                value: "dummy".into(),
                expires_on: OffsetDateTime::now_utc() + Duration::seconds(3600),
                source: AuthSource::EnvClientSecret,
            })
        }
    }

    fn client_for(server: &MockServer) -> ArmClient {
        ArmClient::new(ArmEndpoints { arm: server.uri() }, Arc::new(DummyAuth))
            .expect("client build")
    }

    // --- sanitize_hint -----------------------------------------------------

    #[test]
    fn sanitize_strips_kql_breakers() {
        assert_eq!(sanitize_hint("contoso-b2c"), "contoso-b2c");
        assert_eq!(sanitize_hint("b2c-apps_1.0"), "b2c-apps_1.0");
        assert_eq!(sanitize_hint("'; drop table --"), "droptable--");
        assert_eq!(
            sanitize_hint("a' or 'x'='x"),
            "aorxx",
            "no quotes, spaces, or KQL operators survive"
        );
        assert_eq!(sanitize_hint("name | project *"), "nameproject");
    }

    #[test]
    fn sanitize_returns_empty_for_pure_punctuation() {
        assert_eq!(sanitize_hint("!@#$%^&*()"), "");
        assert_eq!(sanitize_hint("   \n\t  "), "");
    }

    #[tokio::test]
    async fn empty_hint_returns_empty_without_calling_arm() {
        // No mock set up — if we hit ARM, the test would fail with a body decode error.
        let server = MockServer::start().await;
        let arm = client_for(&server);
        let result = search_resources_by_hint(&arm, "", None)
            .await
            .expect("empty hint must be Ok(empty)");
        assert!(result.is_empty());
    }

    #[tokio::test]
    async fn single_char_hint_returns_empty_without_calling_arm() {
        let server = MockServer::start().await;
        let arm = client_for(&server);
        let result = search_resources_by_hint(&arm, "x", None).await.expect("ok");
        assert!(result.is_empty(), "1-char hint too noisy; must not query");
    }

    // --- multi-pass behavior ----------------------------------------------

    #[tokio::test]
    async fn exact_match_wins_first_pass() {
        let server = MockServer::start().await;
        // Pass 1: returns the RG.
        Mock::given(method("POST"))
            .and(path_regex(
                r"^/providers/Microsoft\.ResourceGraph/resources$",
            ))
            .and(body_partial_json(json!({
                "query": "Resources | where name =~ 'contoso-b2c' \
                          | project id, name, type, resourceGroup, subscriptionId \
                          | limit 5",
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "totalRecords": 1,
                "data": [{
                    "id": "/subscriptions/sub1/resourceGroups/contoso-b2c",
                    "name": "contoso-b2c",
                    "type": "microsoft.resources/subscriptions/resourcegroups",
                    "resourceGroup": "contoso-b2c",
                    "subscriptionId": "sub1",
                }]
            })))
            .mount(&server)
            .await;
        // Pass 2: substring catches the B2C directory.
        Mock::given(method("POST"))
            .and(path_regex(r"^/providers/Microsoft\.ResourceGraph/resources$"))
            .and(body_partial_json(json!({
                "query": "Resources \
                          | where name contains 'contoso-b2c' and not(name =~ 'contoso-b2c') \
                          | project id, name, type, resourceGroup, subscriptionId \
                          | limit 5",
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "totalRecords": 1,
                "data": [{
                    "id": "/subscriptions/sub1/resourceGroups/contoso-b2c/providers/Microsoft.AzureActiveDirectory/b2cDirectories/oncontoso-b2c.onmicrosoft.com",
                    "name": "oncontoso-b2c.onmicrosoft.com",
                    "type": "microsoft.azureactivedirectory/b2cdirectories",
                    "resourceGroup": "contoso-b2c",
                    "subscriptionId": "sub1",
                }]
            })))
            .mount(&server)
            .await;
        // Pass 3 mock — even if called, returns nothing.
        Mock::given(method("POST"))
            .and(path_regex(
                r"^/providers/Microsoft\.ResourceGraph/resources$",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "totalRecords": 0, "data": []
            })))
            .mount(&server)
            .await;

        let arm = client_for(&server);
        let result = search_resources_by_hint(&arm, "contoso-b2c", None)
            .await
            .expect("ok");

        // Exact match first, substring match second.
        assert_eq!(result.len(), 2, "got: {result:#?}");
        assert_eq!(result[0].name, "contoso-b2c");
        assert_eq!(result[0].match_reason, MatchReason::NameExact);
        assert_eq!(result[1].name, "oncontoso-b2c.onmicrosoft.com");
        assert_eq!(result[1].match_reason, MatchReason::NameContains);
    }

    #[tokio::test]
    async fn b2c_friendly_name_resolves_via_substring_when_no_exact_match() {
        // Real-world B2C scenario: user types "contoso-b2c", the actual ARM name is
        // "oncontoso-b2c.onmicrosoft.com". Exact match returns nothing; substring
        // catches it.
        let server = MockServer::start().await;
        // Pass 1: empty.
        Mock::given(method("POST"))
            .and(path_regex(
                r"^/providers/Microsoft\.ResourceGraph/resources$",
            ))
            .and(body_partial_json(json!({
                "query": "Resources | where name =~ 'contoso-b2c' \
                          | project id, name, type, resourceGroup, subscriptionId \
                          | limit 5",
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "totalRecords": 0, "data": []
            })))
            .mount(&server)
            .await;
        // Pass 2: the actual B2C directory.
        Mock::given(method("POST"))
            .and(path_regex(r"^/providers/Microsoft\.ResourceGraph/resources$"))
            .and(body_partial_json(json!({
                "query": "Resources \
                          | where name contains 'contoso-b2c' and not(name =~ 'contoso-b2c') \
                          | project id, name, type, resourceGroup, subscriptionId \
                          | limit 5",
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "totalRecords": 1,
                "data": [{
                    "id": "/subscriptions/sub1/resourceGroups/auth/providers/Microsoft.AzureActiveDirectory/b2cDirectories/oncontoso-b2c.onmicrosoft.com",
                    "name": "oncontoso-b2c.onmicrosoft.com",
                    "type": "microsoft.azureactivedirectory/b2cdirectories",
                    "resourceGroup": "auth",
                    "subscriptionId": "sub1",
                }]
            })))
            .mount(&server)
            .await;
        // Pass 3: empty fallback.
        Mock::given(method("POST"))
            .and(path_regex(
                r"^/providers/Microsoft\.ResourceGraph/resources$",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "totalRecords": 0, "data": []
            })))
            .mount(&server)
            .await;

        let arm = client_for(&server);
        let result = search_resources_by_hint(&arm, "contoso-b2c", None)
            .await
            .expect("ok");
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "oncontoso-b2c.onmicrosoft.com");
        assert_eq!(result[0].match_reason, MatchReason::NameContains);
        assert_eq!(
            result[0].resource_type,
            "microsoft.azureactivedirectory/b2cdirectories"
        );
    }

    #[tokio::test]
    async fn no_results_returns_empty_vec_not_error() {
        let server = MockServer::start().await;
        // All three passes return empty.
        Mock::given(method("POST"))
            .and(path_regex(
                r"^/providers/Microsoft\.ResourceGraph/resources$",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "totalRecords": 0, "data": []
            })))
            .mount(&server)
            .await;

        let arm = client_for(&server);
        let result = search_resources_by_hint(&arm, "totally-unknown-resource", None)
            .await
            .expect("ok");
        assert!(result.is_empty(), "no matches must be Ok(empty), not Err");
    }
}
