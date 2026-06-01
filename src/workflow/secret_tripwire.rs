//! Defense-in-depth tripwire for catastrophic secret patterns.
//!
//! Runs *after* the LLM-driven sanitization step in `commit_sanitized_context`.
//! If the LLM under-redacted and a high-confidence catastrophic pattern is
//! still present, the MCP refuses to persist the draft and asks the
//! assistant to try again.
//!
//! Patterns here are deliberately MINIMAL and ZERO-FALSE-POSITIVE. We are NOT
//! a secret scanner. Anything ambiguous belongs upstream in the LLM's
//! semantic redaction, not here. The four patterns below cover the cases
//! where a single ship-it would constitute a permanent, externally-visible
//! credential disclosure.
//!
//! If you add a pattern: it MUST have zero realistic false positives AND
//! represent a credential whose leak is materially worse than typical PII.

use std::sync::OnceLock;

use regex::Regex;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TripwireMatch {
    /// Stable identifier for the pattern (e.g. `AZURE_STORAGE_CONN_STR`).
    pub kind: String,
    /// Human-readable description for the assistant to act on.
    pub description: String,
    /// Byte offset where the catastrophic pattern starts.
    pub byte_offset: usize,
}

/// Scan `text` for catastrophic secret patterns. Returns at most one match
/// per kind (we don't need exhaustive enumeration — one hit is enough to
/// reject the commit and ask for a retry).
pub fn scan(text: &str) -> Vec<TripwireMatch> {
    let mut out = Vec::new();

    if let Some(m) = azure_storage_conn_str(text) {
        out.push(m);
    }
    if let Some(m) = pem_private_key_block(text) {
        out.push(m);
    }
    if let Some(m) = azure_account_key(text) {
        out.push(m);
    }
    if let Some(m) = bearer_jwt(text) {
        out.push(m);
    }
    out
}

fn azure_storage_conn_str(text: &str) -> Option<TripwireMatch> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        // Azure storage connection strings always contain
        // DefaultEndpointsProtocol AND AccountKey. Between them may be
        // multiple `Key=Value` pairs (AccountName=..., EndpointSuffix=...).
        // We use a lazy `.*?` instead of trying to enumerate the segments.
        Regex::new(r"(?s)DefaultEndpointsProtocol\s*=.*?AccountKey\s*=\s*[A-Za-z0-9+/=]{40,}")
            .unwrap()
    });
    re.find(text).map(|m| TripwireMatch {
        kind: "AZURE_STORAGE_CONN_STR".into(),
        description: "Azure storage account connection string (contains an account key).".into(),
        byte_offset: m.start(),
    })
}

fn pem_private_key_block(text: &str) -> Option<TripwireMatch> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        // The only thing this literal phrase matches is a private key block.
        Regex::new(r"-----BEGIN [A-Z ]*PRIVATE KEY-----").unwrap()
    });
    re.find(text).map(|m| TripwireMatch {
        kind: "PRIVATE_KEY_BLOCK".into(),
        description: "PEM private key block (RSA / EC / OPENSSH / etc).".into(),
        byte_offset: m.start(),
    })
}

fn azure_account_key(text: &str) -> Option<TripwireMatch> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        // Azure storage keys: exactly 88 chars base64, ending with `==`.
        // We anchor on `AccountKey=` to avoid catching unrelated 88-char
        // base64 blobs (which are rare but possible in legitimate logs).
        Regex::new(r"AccountKey\s*=\s*[A-Za-z0-9+/]{86}==").unwrap()
    });
    re.find(text).map(|m| TripwireMatch {
        kind: "AZURE_ACCOUNT_KEY".into(),
        description: "Azure storage account key (88-char base64 after AccountKey=).".into(),
        byte_offset: m.start(),
    })
}

fn bearer_jwt(text: &str) -> Option<TripwireMatch> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        // JWTs are 3 base64url-encoded segments separated by `.`. The first
        // segment always starts with `eyJ` because the JOSE header is JSON
        // that starts with `{"`. We require Bearer prefix to avoid matching
        // random base64 blobs.
        Regex::new(r"Bearer\s+eyJ[A-Za-z0-9_\-]+\.[A-Za-z0-9_\-]+\.[A-Za-z0-9_\-]+").unwrap()
    });
    re.find(text).map(|m| TripwireMatch {
        kind: "BEARER_JWT".into(),
        description: "Bearer JWT token in an Authorization header or similar.".into(),
        byte_offset: m.start(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- positive cases ----

    #[test]
    fn detects_azure_storage_connection_string() {
        let key = format!("{}==", "A".repeat(86));
        let s = format!(
            "DefaultEndpointsProtocol=https;AccountName=foo;AccountKey={key};EndpointSuffix=core.windows.net"
        );
        let r = scan(&s);
        assert_eq!(r.len(), 2, "got: {r:?}");
        assert!(r.iter().any(|m| m.kind == "AZURE_STORAGE_CONN_STR"));
        assert!(r.iter().any(|m| m.kind == "AZURE_ACCOUNT_KEY"));
    }

    #[test]
    fn detects_pem_private_key_block() {
        let s = "Here is the cert:\n-----BEGIN RSA PRIVATE KEY-----\nMIIEpAIBAA...";
        let r = scan(s);
        assert!(r.iter().any(|m| m.kind == "PRIVATE_KEY_BLOCK"));
    }

    #[test]
    fn detects_openssh_private_key_block() {
        let s = "-----BEGIN OPENSSH PRIVATE KEY-----";
        let r = scan(s);
        assert!(r.iter().any(|m| m.kind == "PRIVATE_KEY_BLOCK"));
    }

    #[test]
    fn detects_account_key_alone() {
        // exactly 86 + "==" = 88 char base64
        let key = "A".repeat(86) + "==";
        let s = format!("AccountKey={key}");
        let r = scan(&s);
        assert!(r.iter().any(|m| m.kind == "AZURE_ACCOUNT_KEY"));
    }

    #[test]
    fn detects_bearer_jwt() {
        let s = "Authorization: Bearer eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTYifQ.SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c";
        let r = scan(s);
        assert!(r.iter().any(|m| m.kind == "BEARER_JWT"));
    }

    // ---- negative cases (zero false positives) ----

    #[test]
    fn does_not_match_arm_resource_id() {
        let rid = "/subscriptions/00000000-0000-0000-0000-000000000001/resourceGroups/test-genai/providers/Microsoft.Storage/storageAccounts/aistudiohub";
        assert!(scan(rid).is_empty());
    }

    #[test]
    fn does_not_match_random_base64_blob() {
        // 88 chars but not labelled AccountKey=
        let s = format!("checksum={}{}", "A".repeat(86), "==");
        assert!(scan(&s).is_empty());
    }

    #[test]
    fn does_not_match_bare_bearer_word() {
        assert!(scan("the bearer of bad news").is_empty());
    }

    #[test]
    fn does_not_match_normal_error_text() {
        let s = "HTTP/1.1 404 Not Found - resource /subscriptions/abc/foo not found";
        assert!(scan(s).is_empty());
    }

    #[test]
    fn does_not_match_pem_public_key_block() {
        // We deliberately only match PRIVATE, not PUBLIC.
        assert!(scan("-----BEGIN PUBLIC KEY-----").is_empty());
        assert!(scan("-----BEGIN CERTIFICATE-----").is_empty());
    }

    #[test]
    fn empty_input_returns_empty() {
        assert!(scan("").is_empty());
    }
}
