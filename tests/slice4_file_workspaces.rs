//! Slice 4 — file workspaces + chunked upload against wiremock.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};
use time::OffsetDateTime;
use wiremock::matchers::{body_partial_json, header, method, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

use azure_support_ticket_mcp::azure::auth::{AccessToken, AuthProvider, AuthSource, TokenScope};
use azure_support_ticket_mcp::azure::client::{ArmClient, ArmEndpoints};
use azure_support_ticket_mcp::azure::support::file_workspaces::{
    create_workspace, encode_for_upload, list_files, upload_file, MAX_CHUNK_B64_CHARS,
    MAX_FILE_BYTES,
};
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
async fn create_workspace_puts_empty_body() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path_regex(
            r"^/subscriptions/[^/]+/providers/Microsoft\.Support/fileWorkspaces/[^/]+$",
        ))
        .and(header("authorization", "Bearer test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "name": "ws1",
            "properties": {"expirationTime": "2030-01-01T00:00:00Z"}
        })))
        .mount(&server)
        .await;
    let arm = arm_for(&server);
    let v = create_workspace(&arm, "sub", "ws1").await.unwrap();
    assert_eq!(v["name"], "ws1");
}

#[tokio::test]
async fn list_files_returns_value_array() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path_regex(
            r"^/subscriptions/[^/]+/providers/Microsoft\.Support/fileWorkspaces/[^/]+/files$",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "value": [
                {"name": "a.log", "properties": {"fileSize": 10, "numberOfChunks": 1}},
                {"name": "b.bin", "properties": {"fileSize": 20, "numberOfChunks": 1}},
            ]
        })))
        .mount(&server)
        .await;
    let arm = arm_for(&server);
    let files = list_files(&arm, "sub", "ws1").await.unwrap();
    assert_eq!(files.len(), 2);
    assert_eq!(files[0]["name"], "a.log");
}

#[tokio::test]
async fn upload_file_creates_then_uploads_each_chunk() {
    let server = MockServer::start().await;
    // File create.
    Mock::given(method("PUT"))
        .and(path_regex(
            r"^/subscriptions/[^/]+/providers/Microsoft\.Support/fileWorkspaces/[^/]+/files/[^/]+$",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "name": "x.txt",
            "properties": {"fileSize": 11, "numberOfChunks": 1, "chunkSize": 16}
        })))
        .mount(&server)
        .await;
    // Each chunk upload (POST).
    Mock::given(method("POST"))
        .and(path_regex(
            r"^/subscriptions/[^/]+/providers/Microsoft\.Support/fileWorkspaces/[^/]+/files/[^/]+/upload$",
        ))
        .and(body_partial_json(json!({"chunkIndex": 0})))
        .respond_with(ResponseTemplate::new(200).set_body_json(Value::Null))
        .mount(&server)
        .await;

    let arm = arm_for(&server);
    let v = upload_file(&arm, "sub", "ws1", "x.txt", b"hello world")
        .await
        .unwrap();
    assert_eq!(v["properties"]["fileSize"], 11);
}

#[test]
fn encoder_respects_chunk_and_file_caps() {
    let small = encode_for_upload(b"abc").unwrap();
    assert_eq!(small.chunk_b64.len(), 1);

    let mid = encode_for_upload(&vec![0u8; 3 * 1024 * 1024]).unwrap();
    assert!(mid.chunk_b64.len() >= 2);
    for c in &mid.chunk_b64 {
        assert!(c.len() <= MAX_CHUNK_B64_CHARS);
    }

    let too_big = vec![0u8; MAX_FILE_BYTES + 1];
    assert!(encode_for_upload(&too_big).is_err());
}
