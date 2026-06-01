//! Slice 3 — spawn server, verify the two-call confirmation flow for
//! `update_support_ticket` and `reply_to_ticket` without contacting Azure.
//! Only the *preview* phase is exercised end-to-end; the *apply* phase is
//! covered by `slice3_tickets_crud.rs` against wiremock.

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
            "clientInfo": {"name": "slice3-test", "version": "0"}
        }
    }));
    let _ = srv.next_response(0);
    srv.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
}

#[test]
fn slice3_update_and_reply_preview_round_trip() {
    let tmp = tempfile::tempdir().unwrap();
    let mut srv = Server::spawn(tmp.path());
    initialize(&mut srv);

    // ---- update_support_ticket: preview phase only ----
    let res = srv.call(
        1,
        "update_support_ticket",
        json!({
            "subscription_id": "00000000-0000-0000-0000-000000000001",
            "ticket_name": "ticket-abc",
            "severity": "critical"
        }),
    );
    let s = Server::structured(&res);
    assert_eq!(s["phase"], "preview");
    let token = s["review_token"].as_str().unwrap().to_string();
    let hash = s["draft_hash"].as_str().unwrap().to_string();
    assert!(token.starts_with("rt_"));
    assert!(hash.starts_with("sha256:"));
    assert_eq!(s["patch_properties"]["severity"], "critical");

    // Calling again with a *different* patch should issue a new token
    // (the intent_key revokes prior tokens for the same ticket).
    let res2 = srv.call(
        2,
        "update_support_ticket",
        json!({
            "subscription_id": "00000000-0000-0000-0000-000000000001",
            "ticket_name": "ticket-abc",
            "severity": "moderate"
        }),
    );
    let s2 = Server::structured(&res2);
    assert_ne!(s2["review_token"].as_str().unwrap(), token);
    let new_token = s2["review_token"].as_str().unwrap();
    let new_hash = s2["draft_hash"].as_str().unwrap();

    // Confirming with stale review_token → validation error.
    let bad = srv.call(
        3,
        "update_support_ticket",
        json!({
            "subscription_id": "00000000-0000-0000-0000-000000000001",
            "ticket_name": "ticket-abc",
            "severity": "moderate",
            "review_token": token,
            "draft_hash": hash,
            "confirmed": true
        }),
    );
    assert!(bad["error"]["message"]
        .as_str()
        .unwrap()
        .to_lowercase()
        .contains("review_token"));

    // Confirming with current token + wrong hash → mismatch error.
    let bad2 = srv.call(
        4,
        "update_support_ticket",
        json!({
            "subscription_id": "00000000-0000-0000-0000-000000000001",
            "ticket_name": "ticket-abc",
            "severity": "moderate",
            "review_token": new_token,
            "draft_hash": "sha256:deadbeef",
            "confirmed": true
        }),
    );
    assert!(bad2["error"]["message"]
        .as_str()
        .unwrap()
        .to_lowercase()
        .contains("draft_hash"));

    // Confirming current token + correct hash but confirmed=false → guard fires.
    let bad3 = srv.call(
        5,
        "update_support_ticket",
        json!({
            "subscription_id": "00000000-0000-0000-0000-000000000001",
            "ticket_name": "ticket-abc",
            "severity": "moderate",
            "review_token": new_token,
            "draft_hash": new_hash,
            "confirmed": false
        }),
    );
    let s3 = Server::structured(&bad3);
    // confirmed=false routes back to preview, not an error.
    assert_eq!(s3["phase"], "preview");

    // ---- reply_to_ticket: preview ----
    let r = srv.call(
        6,
        "reply_to_ticket",
        json!({
            "subscription_id": "00000000-0000-0000-0000-000000000001",
            "ticket_name": "ticket-abc",
            "subject": "Update",
            "body": "Trying again"
        }),
    );
    let sr = Server::structured(&r);
    assert_eq!(sr["phase"], "preview");
    assert!(sr["review_token"].as_str().unwrap().starts_with("rt_"));

    // Validation: missing required fields rejected.
    let bad_reply = srv.call(
        7,
        "reply_to_ticket",
        json!({
            "subscription_id": "00000000-0000-0000-0000-000000000001",
            "ticket_name": "ticket-abc",
            "subject": "",
            "body": ""
        }),
    );
    assert!(bad_reply["error"]["message"]
        .as_str()
        .unwrap()
        .to_lowercase()
        .contains("required"));

    // update_support_ticket without any mutable field → validation error.
    let bad_update = srv.call(
        8,
        "update_support_ticket",
        json!({
            "subscription_id": "00000000-0000-0000-0000-000000000001",
            "ticket_name": "ticket-abc"
        }),
    );
    assert!(bad_update["error"]["message"]
        .as_str()
        .unwrap()
        .to_lowercase()
        .contains("mutable field"));

    srv.shutdown();
}
