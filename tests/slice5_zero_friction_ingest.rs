//! Slice 5 — end-to-end: spawn the server, drive the zero-friction
//! handshake (`ingest_error_context` -> `commit_sanitized_context`) over
//! stdio, then preview the resulting draft. No Azure calls.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

fn binary_path() -> std::path::PathBuf {
    let mut p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("target");
    p.push(if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    });
    p.push("azure-support-ticket-mcp");
    p
}

struct Server {
    child: std::process::Child,
    stdin: std::process::ChildStdin,
    reader: BufReader<std::process::ChildStdout>,
}

impl Server {
    fn spawn(tmpdir: &std::path::Path) -> Self {
        let bin = binary_path();
        assert!(bin.exists(), "build first: {}", bin.display());
        let mut child = Command::new(&bin)
            .arg("serve")
            .env("AZURE_SUPPORT_TICKET_MCP_HOME", tmpdir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn server");
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        Self {
            child,
            stdin,
            reader: BufReader::new(stdout),
        }
    }
    fn send(&mut self, v: &Value) {
        self.stdin
            .write_all(serde_json::to_string(v).unwrap().as_bytes())
            .unwrap();
        self.stdin.write_all(b"\n").unwrap();
        self.stdin.flush().unwrap();
    }
    fn next_response(&mut self, id: i64) -> Value {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            assert!(Instant::now() <= deadline, "timeout id={id}");
            let mut line = String::new();
            assert!(self.reader.read_line(&mut line).unwrap() > 0, "eof id={id}");
            if let Ok(v) = serde_json::from_str::<Value>(line.trim()) {
                if v.get("id").and_then(|x| x.as_i64()) == Some(id) {
                    return v;
                }
            }
        }
    }
    fn call(&mut self, id: i64, name: &str, args: Value) -> Value {
        self.send(&json!({
            "jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": {"name": name, "arguments": args}
        }));
        self.next_response(id)
    }
    fn structured(v: &Value) -> &Value {
        &v["result"]["structuredContent"]
    }
    fn shutdown(mut self) {
        drop(self.stdin);
        let _ = self.child.wait();
    }
}

fn initialize(srv: &mut Server) {
    srv.send(&json!({
        "jsonrpc": "2.0", "id": 0, "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {"name": "slice5-test", "version": "0"}
        }
    }));
    let _ = srv.next_response(0);
    srv.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
}

#[test]
fn slice5_ingest_then_commit_then_preview() {
    let tmp = tempfile::tempdir().unwrap();
    let mut srv = Server::spawn(tmp.path());
    initialize(&mut srv);

    // ---- 1. ingest_error_context with a representative ARM 5xx blob ----
    let raw = r#"Failed: HTTP/1.1 503 Service Unavailable
x-ms-correlation-request-id: 88888888-8888-8888-8888-888888888888
{"error":{"code":"InternalServerError","message":"transient failure"}}
Operation on /subscriptions/00000000-0000-0000-0000-000000000001/resourceGroups/test-genai/providers/Microsoft.Storage/storageAccounts/aistudiohub failed"#;

    let r1 = srv.call(1, "ingest_error_context", json!({ "raw_text": raw }));
    let s1 = Server::structured(&r1);
    let token = s1["sanitize_token"].as_str().unwrap().to_string();
    assert!(token.starts_with("san_"), "got token {token}");
    let matched = s1["recognized"]["matched"].as_array().unwrap();
    let matched: Vec<String> = matched
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert!(matched.contains(&"resource_id".to_string()));
    assert!(matched.contains(&"http_status".to_string()));
    assert!(matched.contains(&"arm_error_envelope".to_string()));
    assert_eq!(s1["recognized"]["fields"]["severity_hint"], "critical");
    assert_eq!(
        s1["recognized"]["fields"]["subscription_id"],
        "00000000-0000-0000-0000-000000000001"
    );
    assert!(s1["sanitize_instructions"]
        .as_str()
        .unwrap()
        .contains("commit_sanitized_context"));

    // ---- 2. commit_sanitized_context — tripwire rejects raw paste-back ----
    let dirty_sanitized = format!(
        "Connection: DefaultEndpointsProtocol=https;AccountName=foo;AccountKey={key};EndpointSuffix=core.windows.net",
        key = "A".repeat(86) + "=="
    );
    let r2 = srv.call(
        2,
        "commit_sanitized_context",
        json!({
            "sanitize_token": token.clone(),
            "sanitized_text": dirty_sanitized,
            "redacted_summary": "nothing"
        }),
    );
    let err_msg = r2["error"]["message"].as_str().unwrap();
    assert!(
        err_msg.contains("sanitization_incomplete") && err_msg.contains("AZURE_STORAGE_CONN_STR"),
        "got: {err_msg}"
    );

    // ---- 3. commit_sanitized_context — proper sanitization ----
    let clean = "503 Service Unavailable hitting storage account aistudiohub. \
                 ARM resource ID: /subscriptions/00000000-0000-0000-0000-000000000001/resourceGroups/test-genai/providers/Microsoft.Storage/storageAccounts/aistudiohub. \
                 Connection string with key was: [REDACTED:STORAGE_CONN_STR].";
    let r3 = srv.call(
        3,
        "commit_sanitized_context",
        json!({
            "sanitize_token": token,
            "sanitized_text": clean,
            "redacted_summary": "Redacted 1: STORAGE_CONN_STR"
        }),
    );
    let s3 = Server::structured(&r3);
    let draft_id = s3["draft_id"].as_str().unwrap().to_string();
    let prefilled: Vec<String> = s3["prefilled_fields"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert!(prefilled.contains(&"subscription_id".to_string()));
    assert!(prefilled.contains(&"resource_id".to_string()));
    assert!(prefilled.contains(&"severity".to_string()));
    assert!(prefilled.contains(&"description".to_string()));
    assert_eq!(s3["redacted_summary"], "Redacted 1: STORAGE_CONN_STR");

    // ---- 4. preview_ticket_draft shows the FULL sanitized description ----
    let r4 = srv.call(4, "preview_ticket_draft", json!({ "draft_id": draft_id }));
    let s4 = Server::structured(&r4);
    let prompt = s4["confirmation_prompt"].as_str().unwrap();
    assert!(
        prompt.contains("**Description (full text"),
        "preview is missing description block:\n{prompt}"
    );
    assert!(
        prompt.contains("503 Service Unavailable"),
        "description not echoed:\n{prompt}"
    );
    assert!(
        prompt.contains("Sanitization summary"),
        "redaction summary missing:\n{prompt}"
    );
    assert!(
        prompt.contains("Redacted 1: STORAGE_CONN_STR"),
        "redaction summary text missing:\n{prompt}"
    );

    srv.shutdown();
}

#[test]
fn slice5_oversized_paste_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let mut srv = Server::spawn(tmp.path());
    initialize(&mut srv);
    // 1 MiB + 1
    let huge: String = "x".repeat(1024 * 1024 + 1);
    let r = srv.call(1, "ingest_error_context", json!({ "raw_text": huge }));
    let err = r["error"]["message"].as_str().unwrap();
    assert!(err.contains("exceeds"), "got: {err}");
    srv.shutdown();
}
