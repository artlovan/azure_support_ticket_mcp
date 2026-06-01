//! Slice 4 — spawn server, exercise prepare_attachments + the two-call flow
//! for add_attachments_to_ticket without contacting Azure (preview phase only).

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
            "clientInfo": {"name": "slice4-test", "version": "0"}
        }
    }));
    let _ = srv.next_response(0);
    srv.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
}

#[test]
fn slice4_add_attachments_preview_round_trip() {
    let tmp = tempfile::tempdir().unwrap();
    let mut srv = Server::spawn(tmp.path());
    initialize(&mut srv);

    // ---- add_attachments_to_ticket: preview ----
    let body_b64 = {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD.encode(b"log contents")
    };
    let res = srv.call(
        1,
        "add_attachments_to_ticket",
        json!({
            "subscription_id": "00000000-0000-0000-0000-000000000001",
            "ticket_name": "ticket-xyz",
            "files": [
                {"file_name": "diag.log", "content_base64": body_b64.clone()}
            ]
        }),
    );
    let s = Server::structured(&res);
    assert_eq!(s["phase"], "preview");
    assert_eq!(s["ticket_name"], "ticket-xyz");
    assert_eq!(s["file_workspace_name"], "ticket-xyz");
    assert_eq!(s["planned"][0]["file_name"], "diag.log");
    assert_eq!(s["planned"][0]["size_bytes"], 12);
    assert!(s["review_token"].as_str().unwrap().starts_with("rt_"));

    let token = s["review_token"].as_str().unwrap().to_string();
    let hash = s["draft_hash"].as_str().unwrap().to_string();

    // Adding a different file set → token rotates, old token is stale.
    let res2 = srv.call(
        2,
        "add_attachments_to_ticket",
        json!({
            "subscription_id": "00000000-0000-0000-0000-000000000001",
            "ticket_name": "ticket-xyz",
            "files": [
                {"file_name": "diag.log", "content_base64": body_b64},
                {"file_name": "extra.txt", "content_base64": "aGk="}
            ]
        }),
    );
    let s2 = Server::structured(&res2);
    assert_ne!(s2["review_token"].as_str().unwrap(), token);
    assert_eq!(s2["planned"].as_array().unwrap().len(), 2);

    // Confirming with stale token → error.
    let bad = srv.call(
        3,
        "add_attachments_to_ticket",
        json!({
            "subscription_id": "00000000-0000-0000-0000-000000000001",
            "ticket_name": "ticket-xyz",
            "files": [
                {"file_name": "diag.log", "content_base64": "aGk="}
            ],
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

    // Missing files → validation error.
    let bad2 = srv.call(
        4,
        "add_attachments_to_ticket",
        json!({
            "subscription_id": "00000000-0000-0000-0000-000000000001",
            "ticket_name": "ticket-xyz",
            "files": []
        }),
    );
    assert!(bad2["error"]["message"]
        .as_str()
        .unwrap()
        .to_lowercase()
        .contains("at least one file"));

    // Oversized file → validation error.
    let big_b64 = {
        use base64::Engine as _;
        // 5MB + 1 raw bytes
        let bytes = vec![0u8; 5 * 1024 * 1024 + 1];
        base64::engine::general_purpose::STANDARD.encode(&bytes)
    };
    let bad3 = srv.call(
        5,
        "add_attachments_to_ticket",
        json!({
            "subscription_id": "00000000-0000-0000-0000-000000000001",
            "ticket_name": "ticket-xyz",
            "files": [
                {"file_name": "huge.bin", "content_base64": big_b64}
            ]
        }),
    );
    assert!(bad3["error"]["message"]
        .as_str()
        .unwrap()
        .to_lowercase()
        .contains("max"));

    srv.shutdown();
}
