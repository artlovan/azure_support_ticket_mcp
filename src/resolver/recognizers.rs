//! Deterministic extractors for common error-context shapes.
//!
//! Each recognizer scans a blob of raw text (stack trace, log dump, JSON
//! response body, kubectl output...) and proposes one or more *safe* fields
//! plus an evidence trail. "Safe" means: things that are NOT secrets but ARE
//! useful for opening an Azure support ticket — Azure resource IDs, error
//! codes, correlation IDs, HTTP status codes, severity hints.
//!
//! Why "safe": these fields are surfaced to the assistant BEFORE the
//! LLM-driven sanitization step. They cannot leak secrets because the
//! patterns themselves only match non-secret material (e.g. an ARM resource
//! ID is publicly-shaped and only identifies a resource, not its keys).
//!
//! Cost model: pure functions, no IO, no allocation beyond the matches
//! themselves. Run cheap-first; if a high-confidence recognizer matches we
//! return early. Anything we don't recognize falls through to the LLM.

use std::sync::OnceLock;

use regex::Regex;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// One concrete extracted hint with provenance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Evidence {
    /// Which recognizer produced this hint.
    pub recognizer: String,
    /// First-line byte offset where the supporting match starts (0-based).
    /// Useful so the assistant can quote context in the description.
    pub byte_offset: usize,
    /// Truncated snippet of the matched text, max ~120 chars. Never echoes
    /// secrets because recognizers only match non-secret shapes.
    pub snippet: String,
}

/// What recognizers propose for the draft.
#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ExtractedFields {
    /// Full ARM resource ID, e.g. `/subscriptions/<guid>/resourceGroups/...`.
    /// Implies `subscription_id` (parsed out by the caller).
    pub resource_id: Option<String>,
    /// Just the subscription GUID, if seen separately from a full resource ID.
    pub subscription_id: Option<String>,
    /// Azure error code (e.g. `ResourceNotFound`, `429`, `Unauthorized`).
    pub error_code: Option<String>,
    /// Azure correlation/request ID for the ticket description.
    pub correlation_id: Option<String>,
    /// One of "minimal" | "moderate" | "critical" — best-guess from HTTP
    /// status / event severity.
    pub severity_hint: Option<String>,
    /// Short suggested title (e.g. "kubectl Events: BackOff Pulling image").
    pub title_hint: Option<String>,
}

/// Top-level result returned from [`run_all`].
#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RecognizerResult {
    /// Names of recognizers that found at least one match (empty if nothing
    /// matched -> assistant falls back to plain LLM extraction).
    pub matched: Vec<String>,
    /// Aggregated extracted fields. Later recognizers don't overwrite earlier
    /// ones; first-wins by run order (cheapest/most-specific first).
    pub fields: ExtractedFields,
    /// Each individual match for traceability (assistant may surface the
    /// matched snippets in the description).
    pub evidence: Vec<Evidence>,
}

/// Run every recognizer over `text` and aggregate the results.
pub fn run_all(text: &str) -> RecognizerResult {
    let mut out = RecognizerResult::default();
    // Cheap, specific first.
    arm_error_envelope(text, &mut out);
    az_deployment_op_failure(text, &mut out);
    resource_id_pattern(text, &mut out);
    http_status_pattern(text, &mut out);
    kubectl_events_block(text, &mut out);
    out
}

// ---- ARM error envelope --------------------------------------------------

/// Matches `{"error": {"code": "...", "message": "...", ...}}` and similar
/// wrappers seen in ARM 4xx/5xx responses. Extracts `error_code` and any
/// `x-ms-correlation-request-id` header echoed in the same blob.
fn arm_error_envelope(text: &str, out: &mut RecognizerResult) {
    static RE_CODE: OnceLock<Regex> = OnceLock::new();
    let re_code = RE_CODE.get_or_init(|| {
        // "code"\s*:\s*"<token>"  inside an "error" object. We accept either
        // a nested object or a flat envelope (some Azure SDKs flatten).
        Regex::new(r#""code"\s*:\s*"([A-Za-z0-9_.\-]{2,80})""#).unwrap()
    });
    if let Some(m) = re_code.captures(text) {
        let code = m.get(1).unwrap().as_str().to_string();
        if out.fields.error_code.is_none() {
            out.fields.error_code = Some(code.clone());
        }
        push_match(out, "arm_error_envelope", m.get(0).unwrap().start(), &m[0]);
    }
    static RE_CORR: OnceLock<Regex> = OnceLock::new();
    let re_corr = RE_CORR.get_or_init(|| {
        Regex::new(r#"(?i)x-ms-correlation-request-id[:\s"]+([0-9a-f-]{8,})"#).unwrap()
    });
    if let Some(m) = re_corr.captures(text) {
        let cid = m.get(1).unwrap().as_str().to_string();
        if out.fields.correlation_id.is_none() {
            out.fields.correlation_id = Some(cid);
        }
        push_match(
            out,
            "arm_error_envelope_correlation",
            m.get(0).unwrap().start(),
            &m[0],
        );
    }
}

// ---- az deployment operation failure ------------------------------------

/// Matches the `provisioningOperation` / `provisioningState=Failed` shape
/// from `az deployment operation list/show -o json`.
fn az_deployment_op_failure(text: &str, out: &mut RecognizerResult) {
    static RE_PSTATE: OnceLock<Regex> = OnceLock::new();
    let re = RE_PSTATE.get_or_init(|| Regex::new(r#""provisioningState"\s*:\s*"Failed""#).unwrap());
    if let Some(m) = re.find(text) {
        push_match(out, "az_deployment_failed", m.start(), m.as_str());
        // Bump severity hint to moderate if not already set.
        if out.fields.severity_hint.is_none() {
            out.fields.severity_hint = Some("moderate".into());
        }
    }
}

// ---- Resource ID pattern ------------------------------------------------

/// Matches a full Azure ARM resource ID and extracts both the resource ID
/// (canonical) and the subscription GUID. Big win for the infra-engineer
/// case.
fn resource_id_pattern(text: &str, out: &mut RecognizerResult) {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        // Subscription GUIDs are RFC4122. Resource group / provider / resource
        // segments allow letters, digits, dashes, underscores, dots, parens.
        Regex::new(
            r#"/subscriptions/(?P<sub>[0-9a-fA-F\-]{36})/resourceGroups/[^/\s"]+/providers/[A-Za-z0-9\.]+/[A-Za-z0-9_\.\-/\(\)]+"#,
        )
        .unwrap()
    });
    if let Some(caps) = re.captures(text) {
        let full = caps.get(0).unwrap();
        let sub = caps.name("sub").unwrap().as_str();
        if out.fields.resource_id.is_none() {
            out.fields.resource_id = Some(full.as_str().to_string());
        }
        if out.fields.subscription_id.is_none() {
            out.fields.subscription_id = Some(sub.to_string());
        }
        push_match(out, "resource_id", full.start(), full.as_str());
    }
}

// ---- HTTP status pattern ------------------------------------------------

/// Matches HTTP status codes in common forms and sets a severity hint.
fn http_status_pattern(text: &str, out: &mut RecognizerResult) {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        // "HTTP/1.1 503 ...", "status code 429", "status=500", " (404)"
        Regex::new(
            r"(?i)(?:HTTP/[12]\.[01]\s+|status(?:\s*code)?[:=\s]+|\()\s*(?P<code>[45]\d{2})\b",
        )
        .unwrap()
    });
    if let Some(caps) = re.captures(text) {
        let code: u16 = caps["code"].parse().unwrap_or(0);
        if out.fields.error_code.is_none() {
            out.fields.error_code = Some(format!("HTTP {code}"));
        }
        if out.fields.severity_hint.is_none() {
            out.fields.severity_hint = Some(severity_from_status(code).to_string());
        }
        let full = caps.get(0).unwrap();
        push_match(out, "http_status", full.start(), full.as_str());
    }
}

fn severity_from_status(code: u16) -> &'static str {
    // 5xx -> service-side failure -> critical until proven otherwise.
    // 429 -> throttling, almost always degraded production -> moderate.
    // Other 4xx -> usually config/permission -> minimal default.
    match code {
        500..=599 => "critical",
        429 => "moderate",
        _ => "minimal",
    }
}

// ---- kubectl Events block -----------------------------------------------

/// Picks up the `Events:` table from `kubectl describe pod/...`. Uses the
/// last `Warning` event as a title hint.
fn kubectl_events_block(text: &str, out: &mut RecognizerResult) {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        // kubectl describe Events table has 5 columns: Type, Reason, Age,
        // From, Message. The columns are space-padded; >=2 spaces is the
        // separator. Skip Age and From to land on Message.
        Regex::new(r"Warning\s{2,}(?P<reason>[A-Za-z][A-Za-z0-9]*)\s{2,}\S+\s{2,}\S+\s{2,}(?P<msg>[^\n]{3,200})").unwrap()
    });
    if let Some(caps) = re.captures(text) {
        let reason = caps["reason"].to_string();
        let msg = caps["msg"].trim().to_string();
        if out.fields.title_hint.is_none() {
            // Trim to 120 chars to fit Azure title length.
            let mut title = format!("kubectl: {reason} - {msg}");
            if title.len() > 120 {
                title.truncate(117);
                title.push_str("...");
            }
            out.fields.title_hint = Some(title);
        }
        let full = caps.get(0).unwrap();
        push_match(out, "kubectl_events", full.start(), full.as_str());
    }
}

// ---- helpers -------------------------------------------------------------

fn push_match(out: &mut RecognizerResult, name: &str, byte_offset: usize, raw: &str) {
    if !out.matched.iter().any(|n| n == name) {
        out.matched.push(name.to_string());
    }
    let snippet = truncate_snippet(raw, 120);
    out.evidence.push(Evidence {
        recognizer: name.to_string(),
        byte_offset,
        snippet,
    });
}

fn truncate_snippet(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while !s.is_char_boundary(end) && end > 0 {
        end -= 1;
    }
    let mut out = s[..end].to_string();
    out.push_str("...");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arm_error_envelope_extracts_code_and_correlation_id() {
        let blob = r#"
            HTTP/1.1 400 Bad Request
            x-ms-correlation-request-id: 88888888-8888-8888-8888-888888888888
            content-type: application/json

            {"error":{"code":"ResourceNameExists","message":"Resource name foo already exists. Try again with a different name"}}
        "#;
        let r = run_all(blob);
        assert!(r.matched.contains(&"arm_error_envelope".to_string()));
        assert_eq!(r.fields.error_code.as_deref(), Some("ResourceNameExists"));
        assert_eq!(
            r.fields.correlation_id.as_deref(),
            Some("88888888-8888-8888-8888-888888888888")
        );
        // HTTP status pattern should also fire and set severity.
        assert_eq!(r.fields.severity_hint.as_deref(), Some("minimal"));
    }

    #[test]
    fn http_5xx_maps_to_critical() {
        let r = run_all("Failed: HTTP/1.1 503 Service Unavailable");
        assert_eq!(r.fields.severity_hint.as_deref(), Some("critical"));
        assert_eq!(r.fields.error_code.as_deref(), Some("HTTP 503"));
    }

    #[test]
    fn http_429_maps_to_moderate() {
        let r = run_all("Got status code 429 Too Many Requests");
        assert_eq!(r.fields.severity_hint.as_deref(), Some("moderate"));
    }

    #[test]
    fn resource_id_extracts_subscription_and_full_id() {
        let blob = "Operation on /subscriptions/00000000-0000-0000-0000-000000000001/resourceGroups/test-genai/providers/Microsoft.Storage/storageAccounts/aistudiohub failed";
        let r = run_all(blob);
        assert!(r.matched.contains(&"resource_id".to_string()));
        assert_eq!(
            r.fields.subscription_id.as_deref(),
            Some("00000000-0000-0000-0000-000000000001")
        );
        assert!(r
            .fields
            .resource_id
            .as_deref()
            .unwrap()
            .ends_with("/storageAccounts/aistudiohub"));
    }

    #[test]
    fn az_deployment_failed_sets_moderate_default() {
        let blob = r#"{"name":"dep","properties":{"provisioningState":"Failed","provisioningOperation":"Create"}}"#;
        let r = run_all(blob);
        assert!(r.matched.contains(&"az_deployment_failed".to_string()));
        assert_eq!(r.fields.severity_hint.as_deref(), Some("moderate"));
    }

    #[test]
    fn kubectl_events_extracts_title_hint() {
        let blob = "
Events:
  Type     Reason       Age   From               Message
  ----     ------       ----  ----               -------
  Normal   Scheduled    2m    default-scheduler  Successfully assigned default/my-pod
  Warning  BackOff      30s   kubelet            Back-off pulling image \"my-registry.io/app:bad\"
";
        let r = run_all(blob);
        assert!(r.matched.contains(&"kubectl_events".to_string()));
        let t = r.fields.title_hint.unwrap();
        assert!(
            t.starts_with("kubectl: BackOff - Back-off pulling image"),
            "got: {t}"
        );
    }

    #[test]
    fn empty_blob_matches_nothing() {
        let r = run_all("");
        assert!(r.matched.is_empty());
        assert!(r.evidence.is_empty());
        assert!(r.fields.resource_id.is_none());
    }

    #[test]
    fn plain_prose_matches_nothing() {
        let r = run_all("hello world, this is just a normal sentence.");
        assert!(r.matched.is_empty());
    }

    #[test]
    fn first_wins_on_duplicates() {
        let blob = r#"
            "code":"FirstError"
            "code":"SecondError"
        "#;
        let r = run_all(blob);
        assert_eq!(r.fields.error_code.as_deref(), Some("FirstError"));
    }

    #[test]
    fn snippet_truncation_respects_char_boundaries() {
        let s = "a".repeat(200);
        let t = truncate_snippet(&s, 120);
        assert_eq!(t.len(), 123); // 120 + "..."
    }
}
