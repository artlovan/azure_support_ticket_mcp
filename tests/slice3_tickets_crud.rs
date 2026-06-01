//! Slice 3 — list / get / patch tickets and communications against wiremock ARM.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;
use time::OffsetDateTime;
use wiremock::matchers::{header, method, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

use azure_support_ticket_mcp::azure::auth::{AccessToken, AuthProvider, AuthSource, TokenScope};
use azure_support_ticket_mcp::azure::client::{ArmClient, ArmEndpoints};
use azure_support_ticket_mcp::azure::support::communications::{
    create_communication, generate_communication_name, list_communications,
};
use azure_support_ticket_mcp::azure::support::tickets::{get_ticket, list_tickets, patch_ticket};
use azure_support_ticket_mcp::error::AppResult;

struct FakeAuth;

#[async_trait]
impl AuthProvider for FakeAuth {
    fn source(&self) -> AuthSource {
        AuthSource::EnvClientSecret
    }
    async fn get_token(&self, _scope: TokenScope) -> AppResult<AccessToken> {
        Ok(AccessToken {
            value: "test-token".into(),
            expires_on: OffsetDateTime::now_utc() + time::Duration::hours(1),
            source: AuthSource::EnvClientSecret,
        })
    }
}

fn arm_for(server: &MockServer) -> ArmClient {
    let endpoints = ArmEndpoints { arm: server.uri() };
    ArmClient::new(endpoints, Arc::new(FakeAuth)).unwrap()
}

#[tokio::test]
async fn list_tickets_returns_summaries_and_next_link() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path_regex(
            r"^/subscriptions/[^/]+/providers/Microsoft\.Support/supportTickets$",
        ))
        .and(header("authorization", "Bearer test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "value": [
                {"name": "t1", "properties": {"title": "T1", "status": "Open", "severity": "moderate"}},
                {"name": "t2", "properties": {"title": "T2", "status": "Closed", "severity": "minimal"}},
            ],
            "nextLink": "https://example/next-page"
        })))
        .mount(&server)
        .await;

    let arm = arm_for(&server);
    let page = list_tickets(&arm, "sub", Some(25), None, None)
        .await
        .unwrap();
    assert_eq!(page.tickets.len(), 2);
    assert_eq!(page.next_link.as_deref(), Some("https://example/next-page"));
}

#[tokio::test]
async fn get_ticket_returns_full_body() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path_regex(
            r"^/subscriptions/[^/]+/providers/Microsoft\.Support/supportTickets/[^/]+$",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "name": "t1",
            "properties": {"title": "T1", "status": "Open", "severity": "moderate"}
        })))
        .mount(&server)
        .await;
    let arm = arm_for(&server);
    let body = get_ticket(&arm, "sub", "t1").await.unwrap();
    assert_eq!(body["name"], "t1");
}

#[tokio::test]
async fn patch_ticket_updates_severity() {
    let server = MockServer::start().await;
    Mock::given(method("PATCH"))
        .and(path_regex(
            r"^/subscriptions/[^/]+/providers/Microsoft\.Support/supportTickets/[^/]+$",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "name": "t1",
            "properties": {"severity": "critical", "status": "Open"}
        })))
        .mount(&server)
        .await;
    let arm = arm_for(&server);
    let updated = patch_ticket(&arm, "sub", "t1", &json!({"severity": "critical"}))
        .await
        .unwrap();
    assert_eq!(updated["properties"]["severity"], "critical");
}

#[tokio::test]
async fn list_communications_pages_and_summarizes() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path_regex(
            r"^/subscriptions/[^/]+/providers/Microsoft\.Support/supportTickets/[^/]+/communications$",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "value": [
                {"name": "c1", "properties": {"communicationDirection": "Inbound", "body": "hello", "subject": "hi"}},
                {"name": "c2", "properties": {"communicationDirection": "Outbound", "sender": "eng@ms", "body": "ack", "subject": "re"}}
            ]
        })))
        .mount(&server)
        .await;
    let arm = arm_for(&server);
    let page = list_communications(&arm, "sub", "t1", Some(10), None)
        .await
        .unwrap();
    assert_eq!(page.items.len(), 2);
}

#[tokio::test]
async fn create_communication_puts_with_body() {
    let server = MockServer::start().await;
    let comm = generate_communication_name();
    Mock::given(method("PUT"))
        .and(path_regex(
            r"^/subscriptions/[^/]+/providers/Microsoft\.Support/supportTickets/[^/]+/communications/.+$",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "name": comm,
            "properties": {"subject": "Update", "body": "details", "communicationDirection": "Inbound"}
        })))
        .mount(&server)
        .await;
    let arm = arm_for(&server);
    let created = create_communication(&arm, "sub", "t1", &comm, "Update", "details", None)
        .await
        .unwrap();
    assert_eq!(created["properties"]["subject"], "Update");
}
