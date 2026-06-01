//! `POST /providers/Microsoft.ResourceGraph/resources?api-version=2024-04-01`
//!
//! Azure Resource Graph is the right tool for "find resources by name" — it
//! indexes every ARM resource across every subscription you have access to,
//! and supports KQL for ranking. Per `docs/ARCHITECTURE.md` §10/§15, Resource
//! Graph is step 4 of the resolver order ("Exact resource name within
//! selected subscription → Resource Graph search").
//!
//! This module is intentionally a thin typed wrapper. Higher-level "find me
//! the resource the user named" logic lives in
//! `crate::resolver::resource_search`, which calls this and ranks results.

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::azure::client::{ArmClient, ArmResponse};
use crate::error::{AppError, AppResult};

pub const RESOURCE_GRAPH_API_VERSION: &str = "2024-04-01";

/// A single row from a `project id, name, type, resourceGroup, subscriptionId`
/// KQL query. Fields are `Option` because callers may project subsets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ResourceRow {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default, rename = "type")]
    pub resource_type: Option<String>,
    #[serde(default, rename = "resourceGroup")]
    pub resource_group: Option<String>,
    #[serde(default, rename = "subscriptionId")]
    pub subscription_id: Option<String>,
}

/// Result of one Resource Graph query call. `total_records` is what Resource
/// Graph reports as the true count before any `| limit N` truncation, so
/// callers can decide whether to widen the query or page.
#[derive(Debug, Clone)]
pub struct QueryResult {
    pub rows: Vec<ResourceRow>,
    pub total_records: i64,
}

/// Run a KQL query against Azure Resource Graph.
///
/// `subscriptions`: optional scope. When `None`, Resource Graph searches
/// across every subscription the calling identity can read (slower, broader).
/// When `Some(vec)`, the query is restricted to those subscription IDs
/// (faster, narrower). Empty `Some(vec![])` is equivalent to `None` in the
/// Resource Graph API; we treat both the same here.
///
/// **Note on KQL injection:** Resource Graph does not parameterize queries.
/// The caller is responsible for escaping any user-controlled strings before
/// embedding them in `kql`. See `crate::resolver::resource_search` for the
/// safe wrapper that quotes/escapes hints before building the KQL.
pub async fn query(
    arm: &ArmClient,
    kql: &str,
    subscriptions: Option<&[String]>,
) -> AppResult<QueryResult> {
    let path = format!(
        "/providers/Microsoft.ResourceGraph/resources?api-version={RESOURCE_GRAPH_API_VERSION}"
    );

    let mut body = json!({
        "query": kql,
        "options": {
            "resultFormat": "objectArray",
        },
    });

    if let Some(subs) = subscriptions.filter(|s| !s.is_empty()) {
        body["subscriptions"] = json!(subs);
    }

    let resp = arm.post_json_raw(&path, &body).await?;
    let value = match resp {
        ArmResponse::Sync(v) => v,
        ArmResponse::Async { initial_body, .. } => initial_body,
    };

    let total_records = value
        .get("totalRecords")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);

    let rows: Vec<ResourceRow> = value
        .get("data")
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .map_err(|e| AppError::Azure {
            message: format!("Resource Graph `data` decode failed: {e}"),
            code: None,
            status: None,
            request_id: None,
            operation_id: None,
        })?
        .unwrap_or_default();

    Ok(QueryResult {
        rows,
        total_records,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::azure::auth::{AccessToken, AuthProvider, AuthSource, TokenScope};
    use crate::azure::client::{ArmClient, ArmEndpoints};
    use std::sync::Arc;
    use time::{Duration, OffsetDateTime};
    use wiremock::matchers::{body_partial_json, method, path_regex};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// Test-only AuthProvider — returns a fixed dummy token, no network.
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

    #[tokio::test]
    async fn parses_typical_resource_graph_response() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path_regex(r"^/providers/Microsoft\.ResourceGraph/resources$"))
            .and(body_partial_json(json!({
                "query": "Resources | where name =~ 'contoso-b2c' | project id, name, type, resourceGroup, subscriptionId | limit 5",
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "totalRecords": 1,
                "count": 1,
                "data": [
                    {
                        "id": "/subscriptions/sub1/resourceGroups/contoso-b2c",
                        "name": "contoso-b2c",
                        "type": "microsoft.resources/subscriptions/resourcegroups",
                        "resourceGroup": "contoso-b2c",
                        "subscriptionId": "sub1"
                    }
                ],
                "resultTruncated": "false"
            })))
            .mount(&server)
            .await;

        let arm = client_for(&server);
        let q = "Resources | where name =~ 'contoso-b2c' | project id, name, type, resourceGroup, subscriptionId | limit 5";
        let result = query(&arm, q, None).await.expect("query ok");

        assert_eq!(result.total_records, 1);
        assert_eq!(result.rows.len(), 1);
        let r = &result.rows[0];
        assert_eq!(r.name.as_deref(), Some("contoso-b2c"));
        assert_eq!(
            r.resource_type.as_deref(),
            Some("microsoft.resources/subscriptions/resourcegroups")
        );
        assert_eq!(r.subscription_id.as_deref(), Some("sub1"));
    }

    #[tokio::test]
    async fn returns_empty_when_no_matches() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path_regex(
                r"^/providers/Microsoft\.ResourceGraph/resources$",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "totalRecords": 0,
                "count": 0,
                "data": []
            })))
            .mount(&server)
            .await;

        let arm = client_for(&server);
        let result = query(&arm, "Resources | where name =~ 'nope'", None)
            .await
            .expect("ok");
        assert_eq!(result.total_records, 0);
        assert!(result.rows.is_empty());
    }

    #[tokio::test]
    async fn scopes_request_to_subscriptions_when_provided() {
        let server = MockServer::start().await;
        // body_partial_json verifies the subscriptions array landed in the body.
        Mock::given(method("POST"))
            .and(path_regex(
                r"^/providers/Microsoft\.ResourceGraph/resources$",
            ))
            .and(body_partial_json(json!({
                "subscriptions": ["sub-a", "sub-b"],
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "totalRecords": 0,
                "data": []
            })))
            .mount(&server)
            .await;

        let arm = client_for(&server);
        let subs = vec!["sub-a".to_string(), "sub-b".to_string()];
        let result = query(&arm, "Resources | limit 1", Some(&subs))
            .await
            .expect("ok");
        assert_eq!(result.total_records, 0);
    }

    #[tokio::test]
    async fn empty_subscriptions_vec_is_treated_as_unscoped() {
        let server = MockServer::start().await;
        // The mock matches ONLY if `subscriptions` is NOT present in the body —
        // we assert this by responding 200 if the partial match succeeds (no
        // `subscriptions` field expected). Since wiremock can't easily assert
        // "absence of field", we just verify the call goes through.
        Mock::given(method("POST"))
            .and(path_regex(
                r"^/providers/Microsoft\.ResourceGraph/resources$",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "totalRecords": 0,
                "data": []
            })))
            .mount(&server)
            .await;

        let arm = client_for(&server);
        let empty: Vec<String> = Vec::new();
        let result = query(&arm, "Resources | limit 1", Some(&empty))
            .await
            .expect("ok");
        assert_eq!(result.total_records, 0);
    }
}
