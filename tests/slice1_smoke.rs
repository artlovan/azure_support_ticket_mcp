//! End-to-end smoke for Slice 1: spawn `serve`, complete the MCP handshake,
//! list tools, and call the offline-safe `doctor` + `list_relevant_support_services`
//! tools. Verifies the seed loads into a fresh cache and resolver scoring works.
//!
//! This test does not contact Azure — all checks are local.

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
        assert!(bin.exists(), "build the binary first: {}", bin.display());
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
        let line = serde_json::to_string(v).unwrap();
        self.stdin.write_all(line.as_bytes()).unwrap();
        self.stdin.write_all(b"\n").unwrap();
        self.stdin.flush().unwrap();
    }

    fn next_response(&mut self, id: i64) -> Value {
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            if Instant::now() > deadline {
                panic!("timed out waiting for id={id}");
            }
            let mut line = String::new();
            let n = self.reader.read_line(&mut line).expect("read");
            if n == 0 {
                panic!("server closed stdout before id={id}");
            }
            let v: Value = match serde_json::from_str(line.trim()) {
                Ok(v) => v,
                Err(_) => continue,
            };
            if v.get("id").and_then(|x| x.as_i64()) == Some(id) {
                return v;
            }
        }
    }

    fn shutdown(mut self) {
        drop(self.stdin);
        let _ = self.child.wait();
    }
}

#[test]
fn slice1_handshake_and_tools() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let mut srv = Server::spawn(tmp.path());

    srv.send(&json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {"protocolVersion": "2024-11-05", "capabilities": {},
                   "clientInfo": {"name": "slice1-it", "version": "0"}}
    }));
    let init = srv.next_response(1);
    assert!(init["result"]["serverInfo"]["name"].is_string());

    srv.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));

    srv.send(&json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}));
    let tools = srv.next_response(2);
    let names: Vec<String> = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();
    for expected in [
        "azure_auth_status",
        "doctor",
        "list_tenants",
        "list_subscriptions",
        "list_relevant_support_services",
        "list_problem_classifications",
        "refresh_support_cache",
    ] {
        assert!(
            names.iter().any(|n| n == expected),
            "missing tool {expected}; got {names:?}"
        );
    }

    srv.send(&json!({
        "jsonrpc": "2.0", "id": 3, "method": "tools/call",
        "params": {"name": "doctor", "arguments": {}}
    }));
    let doc = srv.next_response(3);
    let s = &doc["result"]["structuredContent"];
    assert_eq!(s["arm_reachable"], Value::Bool(true));
    assert!(s["services_in_cache"].as_i64().unwrap() > 100);
    assert!(s["seed_version"].is_string());

    srv.send(&json!({
        "jsonrpc": "2.0", "id": 4, "method": "tools/call",
        "params": {"name": "list_relevant_support_services", "arguments": {
            "resource_id": "/subscriptions/11111111-1111-1111-1111-111111111111/resourceGroups/rg/providers/Microsoft.ContainerService/managedClusters/x"
        }}
    }));
    let res = srv.next_response(4);
    let cands = res["result"]["structuredContent"]["candidates"]
        .as_array()
        .unwrap();
    assert!(!cands.is_empty(), "expected AKS candidates");
    let top = &cands[0];
    assert!(top["display_name"].as_str().unwrap().contains("Kubernetes"));
    assert!(top["confidence"].as_f64().unwrap() >= 0.8);

    srv.shutdown();
}
