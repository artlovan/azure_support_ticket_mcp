//! Tests for `azure::support::tickets` against a mocked ARM.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::json;
use time::OffsetDateTime;
use wiremock::matchers::{header, method, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

use azure_support_ticket_mcp::azure::auth::{AccessToken, AuthProvider, AuthSource, TokenScope};
use azure_support_ticket_mcp::azure::client::{ArmClient, ArmEndpoints};
use azure_support_ticket_mcp::azure::support::tickets::{
    build_ticket_body, create_ticket, generate_ticket_name,
};
use azure_support_ticket_mcp::error::AppResult;
use azure_support_ticket_mcp::workflow::draft::TicketDraft;

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

fn good_draft() -> TicketDraft {
    let mut d = TicketDraft::new();
    d.tenant_id = Some("t".into());
    d.subscription_id = Some("00000000-0000-0000-0000-000000000001".into());
    d.service_id = Some("/providers/Microsoft.Support/services/aks".into());
    d.problem_classification_id =
        Some("/providers/Microsoft.Support/services/aks/problemClassifications/scale".into());
    d.title = Some("AKS nodes won't scale".into());
    d.description = Some("Scale-out failing with quota errors.".into());
    d.severity = Some("moderate".into());
    d.advanced_diagnostic_consent = Some("Yes".into());
    d.contact_details.first_name = Some("Ada".into());
    d.contact_details.last_name = Some("Lovelace".into());
    d.contact_details.country = Some("USA".into());
    d.contact_details.preferred_contact_method = Some("email".into());
    d.contact_details.preferred_support_language = Some("en-us".into());
    d.contact_details.preferred_time_zone = Some("Pacific Standard Time".into());
    d.contact_details.primary_email_address = Some("ada@example.com".into());
    d.resource_id =
        Some("/subscriptions/00000000-0000-0000-0000-000000000001/resourceGroups/rg/providers/Microsoft.ContainerService/managedClusters/prod-aks".into());
    d
}

fn arm_for(server: &MockServer) -> ArmClient {
    let endpoints = ArmEndpoints { arm: server.uri() };
    ArmClient::new(endpoints, Arc::new(FakeAuth)).unwrap()
}

#[tokio::test]
async fn create_ticket_sync_200() {
    let server = MockServer::start().await;
    let ticket_name = generate_ticket_name();

    Mock::given(method("PUT"))
        .and(path_regex(r"^/subscriptions/[^/]+/providers/Microsoft\.Support/supportTickets/.+"))
        .and(header("authorization", "Bearer test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": format!("/subscriptions/sub/providers/Microsoft.Support/supportTickets/{ticket_name}"),
            "name": ticket_name,
            "properties": {
                "title": "AKS nodes won't scale",
                "severity": "moderate",
                "status": "Open",
                "supportTicketId": "1234567890000000"
            }
        })))
        .mount(&server)
        .await;

    let arm = arm_for(&server);
    let body = build_ticket_body(&good_draft());
    let created = create_ticket(
        &arm,
        "00000000-0000-0000-0000-000000000001",
        &ticket_name,
        &body,
        3,
        Duration::from_millis(10),
    )
    .await
    .expect("create_ticket sync");

    assert_eq!(created.status, "Open");
    assert_eq!(created.title, "AKS nodes won't scale");
    assert_eq!(
        created.support_ticket_id.as_deref(),
        Some("1234567890000000")
    );
}

#[tokio::test]
async fn create_ticket_async_202_then_poll_succeeded() {
    let server = MockServer::start().await;
    let ticket_name = generate_ticket_name();
    let async_path = "/asyncops/op-1";
    let async_url = format!("{}{}", server.uri(), async_path);

    Mock::given(method("PUT"))
        .and(path_regex(r"^/subscriptions/.+/supportTickets/.+"))
        .respond_with(
            ResponseTemplate::new(202)
                .insert_header("Azure-AsyncOperation", async_url.as_str())
                .set_body_json(json!({
                    "properties": {
                        "title": "AKS nodes won't scale",
                        "severity": "moderate"
                    }
                })),
        )
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path_regex(r"^/asyncops/op-1$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"status": "Succeeded"})))
        .mount(&server)
        .await;

    // Final read of the ticket after success.
    Mock::given(method("GET"))
        .and(path_regex(r"^/subscriptions/.+/supportTickets/.+"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "name": ticket_name,
            "properties": {
                "title": "AKS nodes won't scale",
                "severity": "moderate",
                "status": "Open",
                "supportTicketId": "9876543210000000"
            }
        })))
        .mount(&server)
        .await;

    let arm = arm_for(&server);
    let body = build_ticket_body(&good_draft());
    let created = create_ticket(
        &arm,
        "00000000-0000-0000-0000-000000000001",
        &ticket_name,
        &body,
        5,
        Duration::from_millis(10),
    )
    .await
    .expect("create_ticket async");

    assert_eq!(created.status, "Open");
    assert_eq!(
        created.support_ticket_id.as_deref(),
        Some("9876543210000000")
    );
}
