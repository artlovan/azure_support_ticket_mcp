//! Local-only ticket thread summarizer.
//!
//! **Important:** never invokes an LLM. All work is deterministic string
//! analysis so the MCP can be embedded in trust-sensitive environments and
//! produce the same output regardless of which client is calling.
//!
//! Output is designed to be consumed by an LLM client (Copilot CLI etc.) that
//! can further compress for the human.

use serde::Serialize;
use serde_json::Value;

const MAX_SNIPPET_CHARS: usize = 500;

#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct ThreadSummary {
    pub ticket_name: String,
    pub title: Option<String>,
    pub status: Option<String>,
    pub severity: Option<String>,
    pub created_date: Option<String>,
    pub modified_date: Option<String>,
    pub total_communications: usize,
    pub inbound_count: usize,
    pub outbound_count: usize,
    pub latest: Option<LatestSnippet>,
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct LatestSnippet {
    pub direction: Option<String>,
    pub sender: Option<String>,
    pub created_date: Option<String>,
    pub subject: Option<String>,
    pub body_snippet: String,
}

pub fn summarize(ticket: &Value, communications: &[Value]) -> ThreadSummary {
    let ticket_name = ticket
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let props = ticket.get("properties").cloned().unwrap_or(Value::Null);
    let mut inbound = 0usize;
    let mut outbound = 0usize;
    for c in communications {
        let dir = c
            .get("properties")
            .and_then(|p| p.get("communicationDirection"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        match dir {
            "Inbound" => inbound += 1,
            "Outbound" => outbound += 1,
            _ => {}
        }
    }
    let latest = communications.last().map(|c| {
        let p = c.get("properties").cloned().unwrap_or(Value::Null);
        let body = p
            .get("body")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let (snippet, truncated) = truncate(&body, MAX_SNIPPET_CHARS);
        LatestSnippet {
            direction: p
                .get("communicationDirection")
                .and_then(|v| v.as_str())
                .map(String::from),
            sender: p.get("sender").and_then(|v| v.as_str()).map(String::from),
            created_date: p
                .get("createdDate")
                .and_then(|v| v.as_str())
                .map(String::from),
            subject: p.get("subject").and_then(|v| v.as_str()).map(String::from),
            body_snippet: if truncated {
                format!("{snippet}…")
            } else {
                snippet
            },
        }
    });
    ThreadSummary {
        ticket_name,
        title: props
            .get("title")
            .and_then(|v| v.as_str())
            .map(String::from),
        status: props
            .get("status")
            .and_then(|v| v.as_str())
            .map(String::from),
        severity: props
            .get("severity")
            .and_then(|v| v.as_str())
            .map(String::from),
        created_date: props
            .get("createdDate")
            .and_then(|v| v.as_str())
            .map(String::from),
        modified_date: props
            .get("modifiedDate")
            .and_then(|v| v.as_str())
            .map(String::from),
        total_communications: communications.len(),
        inbound_count: inbound,
        outbound_count: outbound,
        latest,
        truncated: communications.len() > 25,
    }
}

fn truncate(s: &str, max: usize) -> (String, bool) {
    if s.chars().count() <= max {
        return (s.to_string(), false);
    }
    let cut: String = s.chars().take(max).collect();
    (cut, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn counts_directions_and_truncates() {
        let ticket = json!({
            "name": "t1",
            "properties": { "title": "T", "status": "Open", "severity": "moderate" }
        });
        let long = "x".repeat(1000);
        let comms = vec![
            json!({"properties": {"communicationDirection": "Inbound", "body": "hi"}}),
            json!({"properties": {"communicationDirection": "Outbound", "sender": "eng@ms", "body": long, "subject": "re"}}),
        ];
        let s = summarize(&ticket, &comms);
        assert_eq!(s.total_communications, 2);
        assert_eq!(s.inbound_count, 1);
        assert_eq!(s.outbound_count, 1);
        let latest = s.latest.unwrap();
        assert!(latest.body_snippet.ends_with('…'));
        assert!(latest.body_snippet.chars().count() <= MAX_SNIPPET_CHARS + 1);
    }
}
