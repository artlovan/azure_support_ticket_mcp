//! Slice 2 end-to-end: spawn `serve`, drive start_flow → build_draft →
//! preview → create (with confirmation guard rejects). Does not call Azure
//! (so no real `create_support_ticket` here — that path is covered by
//! `slice2_tickets.rs` against wiremock).

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

fn handshake(srv: &mut Server) {
    srv.send(&json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {"protocolVersion": "2024-11-05", "capabilities": {},
                   "clientInfo": {"name": "slice2-it", "version": "0"}}
    }));
    let _ = srv.next_response(1);
    srv.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
}

#[test]
fn slice2_draft_workflow_end_to_end() {
    let tmp = tempfile::tempdir().unwrap();
    let mut srv = Server::spawn(tmp.path());
    handshake(&mut srv);

    // 1) tools/list includes all slice-2 names.
    srv.send(&json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}));
    let tools = srv.next_response(2);
    let names: Vec<String> = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();
    for n in [
        "start_support_ticket_flow",
        "resolve_issue_context",
        "build_ticket_draft",
        "preview_ticket_draft",
        "create_support_ticket",
    ] {
        assert!(names.iter().any(|x| x == n), "missing tool {n}");
    }

    // 2) start a draft pre-filled with tenant + subscription.
    let started = srv.call(
        3,
        "start_support_ticket_flow",
        json!({"tenant_id": "tenant-1", "subscription_id": "00000000-0000-0000-0000-000000000001"}),
    );
    let s = Server::structured(&started);
    let draft_id = s["draft_id"].as_str().unwrap().to_string();
    let token_v1 = s["review_token"].as_str().unwrap().to_string();
    let hash_v1 = s["draft_hash"].as_str().unwrap().to_string();
    assert!(draft_id.starts_with("draft_"));
    assert!(token_v1.starts_with("rt_"));
    assert!(hash_v1.starts_with("sha256:"));

    // 3) resolve context — verify AKS classification.
    let resolved = srv.call(
        4,
        "resolve_issue_context",
        json!({
            "user_input": "Open ticket: AKS prod-aks fails to scale",
            "resource_id": "/subscriptions/00000000-0000-0000-0000-000000000001/resourceGroups/rg/providers/Microsoft.ContainerService/managedClusters/prod-aks"
        }),
    );
    let r = Server::structured(&resolved);
    assert_eq!(
        r["resource_type"],
        "Microsoft.ContainerService/managedClusters"
    );
    assert_eq!(r["resource_name"], "prod-aks");
    let cands = r["service_candidates"]["candidates"].as_array().unwrap();
    assert!(!cands.is_empty());
    assert!(cands[0]["display_name"]
        .as_str()
        .unwrap()
        .contains("Kubernetes"));

    // 4) build the draft incrementally — token rotates.
    let after_patch = srv.call(
        5,
        "build_ticket_draft",
        json!({
            "draft_id": draft_id,
            "service_id": "/providers/Microsoft.Support/services/aks",
            "problem_classification_id": "/providers/Microsoft.Support/services/aks/problemClassifications/scale",
            "title": "AKS scale",
            "description": "Nodes won't scale out",
            "severity": "moderate",
            "advanced_diagnostic_consent": "Yes",
            "resource_id": "/subscriptions/00000000-0000-0000-0000-000000000001/resourceGroups/rg/providers/Microsoft.ContainerService/managedClusters/prod-aks",
            "contact_details": {
                "first_name": "Ada", "last_name": "Lovelace",
                "country": "USA", "preferred_contact_method": "email",
                "preferred_support_language": "en-us",
                "preferred_time_zone": "Pacific Standard Time",
                "primary_email_address": "ada@example.com"
            }
        }),
    );
    let b = Server::structured(&after_patch);
    let token_v2 = b["review_token"].as_str().unwrap().to_string();
    let hash_v2 = b["draft_hash"].as_str().unwrap().to_string();
    assert_eq!(b["valid"], Value::Bool(true));
    assert!(b["missing"].as_array().unwrap().is_empty());
    assert_ne!(token_v1, token_v2, "token must rotate on patch");
    assert_ne!(hash_v1, hash_v2, "hash must change after patch");

    // 5) preview renders the expected fields (also includes validation warnings).
    let p = srv.call(7, "preview_ticket_draft", json!({"draft_id": draft_id}));
    let preview = Server::structured(&p)["preview"].as_str().unwrap();
    assert!(preview.contains("Title: AKS scale"));
    assert!(preview.contains("Email: ada@example.com"));
    assert!(preview.contains("Severity: B - Moderate impact"));

    // 6) create_support_ticket with WRONG hash → confirmation guard rejects.
    let bad = srv.call(
        8,
        "create_support_ticket",
        json!({
            "draft_id": draft_id,
            "review_token": token_v2,
            "draft_hash": "sha256:deadbeef",
            "confirmed": true
        }),
    );
    let err_msg = bad["error"]["message"]
        .as_str()
        .unwrap_or_else(|| panic!("expected error envelope: {bad}"));
    assert!(err_msg.contains("draft_hash mismatch"), "got: {err_msg}");

    // 7) create_support_ticket with confirmed=false → rejected too.
    let unconfirmed = srv.call(
        9,
        "create_support_ticket",
        json!({
            "draft_id": draft_id,
            "review_token": token_v2,
            "draft_hash": hash_v2,
            "confirmed": false
        }),
    );
    let err_msg = unconfirmed["error"]["message"]
        .as_str()
        .expect("expected error envelope");
    assert!(err_msg.contains("confirmed must be true"), "got: {err_msg}");

    srv.shutdown();
}
