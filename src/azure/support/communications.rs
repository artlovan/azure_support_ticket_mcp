//! Support ticket communications (thread replies).
//!
//! Endpoints used:
//! - `GET .../supportTickets/{name}/communications?api-version={V}&$top=...`
//! - `GET .../supportTickets/{name}/communications/{comm}?api-version={V}`
//! - `PUT .../supportTickets/{name}/communications/{comm}?api-version={V}`  (new reply)

use serde_json::Value;
use uuid::Uuid;

use crate::azure::client::{ArmClient, ArmResponse};
use crate::error::AppResult;

use super::services::SUPPORT_API_VERSION;

#[derive(Debug, Clone)]
pub struct CommunicationPage {
    pub items: Vec<Value>,
    pub next_link: Option<String>,
}

pub async fn list_communications(
    arm: &ArmClient,
    sub_id: &str,
    ticket_name: &str,
    top: Option<u32>,
    next_link: Option<&str>,
) -> AppResult<CommunicationPage> {
    let value: Value = if let Some(link) = next_link {
        arm.get_json_absolute(link).await?
    } else {
        let mut path = format!(
            "/subscriptions/{sub_id}/providers/Microsoft.Support/supportTickets/{ticket_name}/communications?api-version={SUPPORT_API_VERSION}"
        );
        if let Some(t) = top {
            // Azure max is 10 per page.
            let capped = t.min(10);
            path.push_str(&format!("&$top={capped}"));
        }
        arm.get_json(&path).await?
    };
    let items = value
        .get("value")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let next_link = value
        .get("nextLink")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    Ok(CommunicationPage { items, next_link })
}

pub async fn get_communication(
    arm: &ArmClient,
    sub_id: &str,
    ticket_name: &str,
    comm_name: &str,
) -> AppResult<Value> {
    let path = format!(
        "/subscriptions/{sub_id}/providers/Microsoft.Support/supportTickets/{ticket_name}/communications/{comm_name}?api-version={SUPPORT_API_VERSION}"
    );
    arm.get_json::<Value>(&path).await
}

/// PUT a new communication (customer reply). Communication type is always
/// "Web" for messages authored from this MCP.
pub async fn create_communication(
    arm: &ArmClient,
    sub_id: &str,
    ticket_name: &str,
    comm_name: &str,
    subject: &str,
    body: &str,
    sender_email: Option<&str>,
) -> AppResult<Value> {
    let path = format!(
        "/subscriptions/{sub_id}/providers/Microsoft.Support/supportTickets/{ticket_name}/communications/{comm_name}?api-version={SUPPORT_API_VERSION}"
    );
    let mut props = serde_json::json!({
        "communicationType": "Web",
        "communicationDirection": "Inbound",
        "subject": subject,
        "body": body,
    });
    if let Some(s) = sender_email {
        props["sender"] = serde_json::json!(s);
    }
    let req = serde_json::json!({ "properties": props });
    match arm.put_json_raw(&path, &req).await? {
        ArmResponse::Sync(v) => Ok(v),
        ArmResponse::Async { initial_body, .. } => Ok(initial_body),
    }
}

/// Generate a random communication name. Use when there's no
/// user-meaningful input to derive a name from (e.g. internal-only paths).
/// For reply tools that already have stable user intent (ticket name +
/// subject + body), prefer [`deterministic_communication_name`] so the
/// preview-then-confirm flow produces the same name on both calls.
pub fn generate_communication_name() -> String {
    format!("comm-{}", Uuid::new_v4())
}

/// Derive a stable `comm-<hex>` name from the user-meaningful parts of a
/// reply intent. Two calls with the same inputs produce the same name —
/// critical for the two-call preview-then-confirm flow, where the
/// preview-time name MUST match the confirm-time name (otherwise Azure
/// would create two communications, or the second call would have a hash
/// that depends on a different generated name and never match).
///
/// Uses SHA-256 truncated to 16 hex chars (8 bytes) — short enough for
/// the URL path, long enough that collisions across distinct intents are
/// not a practical concern.
pub fn deterministic_communication_name(
    ticket_name: &str,
    subject: &str,
    body: &str,
    sender_email: Option<&str>,
) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(b"v1\0");
    h.update(ticket_name.as_bytes());
    h.update(b"\0");
    h.update(subject.as_bytes());
    h.update(b"\0");
    h.update(body.as_bytes());
    h.update(b"\0");
    h.update(sender_email.unwrap_or("").as_bytes());
    let digest = h.finalize();
    let mut hex = String::with_capacity(16);
    for b in &digest[..8] {
        use std::fmt::Write;
        let _ = write!(hex, "{b:02x}");
    }
    format!("comm-{hex}")
}
