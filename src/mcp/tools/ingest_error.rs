//! `ingest_error_context` — first half of the zero-friction handshake.
//!
//! Accepts a blob of error text (typically piped or pasted by the user:
//! `copilot -i "ticket this: $(cat err.log)"`). The MCP:
//!
//!   1. Runs deterministic recognizers to extract SAFE hints (ARM resource
//!      IDs, error codes, correlation IDs, severity hints, title hints).
//!      These shapes are not secrets.
//!   2. Mints a short-lived `sanitize_token` bound to the raw text's
//!      content hash AND to the recognizer output.
//!   3. Returns the safe hints, the raw text echo, the token, and
//!      machine-readable sanitization instructions for the assistant LLM.
//!
//! **This tool does NOT persist a draft.** The draft is only created in
//! step 2 of the handshake (`commit_sanitized_context`) after the assistant
//! returns LLM-sanitized text. The MCP is the trust boundary; it refuses to
//! persist raw user-pasted content.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::bootstrap::AppState;
use crate::error::{AppError, AppResult};
use crate::resolver::recognizers::{self, ExtractedFields, RecognizerResult};

const MAX_RAW_BYTES: usize = 1024 * 1024; // 1 MiB hard cap on a single paste

#[derive(Debug, Deserialize, JsonSchema)]
pub struct Input {
    /// The raw error text the user wants to ticket. Typically piped from
    /// stdout/stderr (`cmd 2>&1 | copilot -i "ticket this: $(cat -)"`) or
    /// pasted into chat. Hard cap: 1 MiB. The MCP does NOT persist this
    /// directly — it returns a sanitize_token and waits for the assistant
    /// to commit the sanitized version via commit_sanitized_context.
    pub raw_text: String,
    /// Optional caller-side hints (e.g. the harness already knows the
    /// subscription from a prior tool call). These are merged with the
    /// recognizer output; caller hints win on conflict because the human
    /// provided them deliberately.
    #[serde(default)]
    pub caller_hints: Option<ExtractedFields>,
    /// Disable recognizer extraction and treat raw_text as opaque. Use this
    /// when you know your content doesn't match standard Azure shapes and
    /// you want to avoid false-positive field assignments. Default false.
    #[serde(default)]
    pub extraction_blob_only: bool,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Output {
    /// Opaque one-shot token. Pass this verbatim to commit_sanitized_context
    /// after the assistant scrubs secrets. Expires in 5 minutes.
    pub sanitize_token: String,
    /// What the deterministic recognizers extracted from the raw text.
    /// Empty `matched` array means nothing matched — assistant should fall
    /// back to LLM extraction over the raw_text echo.
    pub recognized: RecognizerResult,
    /// Echo of the raw text. The assistant feeds this into its LLM
    /// sanitization step, then calls commit_sanitized_context with the
    /// cleaned version.
    pub raw_text_echo: String,
    /// Byte length of raw_text (post-truncation if it was capped).
    pub raw_text_bytes: usize,
    /// Machine-readable instructions for the assistant on how to sanitize
    /// and commit. Includes the secret patterns to look for and the rule
    /// that ARM IDs / error codes / correlation IDs MUST be preserved.
    pub sanitize_instructions: String,
}

pub async fn run(state: &AppState, input: Input) -> AppResult<Output> {
    if input.raw_text.is_empty() {
        return Err(AppError::Validation(
            "raw_text is empty; nothing to ingest".into(),
        ));
    }
    if input.raw_text.len() > MAX_RAW_BYTES {
        return Err(AppError::Validation(format!(
            "raw_text exceeds {MAX_RAW_BYTES} bytes; trim before piping (use head/tail/grep)",
        )));
    }

    let recognized = if input.extraction_blob_only {
        RecognizerResult::default()
    } else {
        recognizers::run_all(&input.raw_text)
    };

    let caller_hints = input.caller_hints.unwrap_or_default();
    let token = state
        .sanitize_tokens
        .issue(&input.raw_text, recognized.clone(), caller_hints);

    let sanitize_instructions = build_instructions();

    Ok(Output {
        sanitize_token: token,
        recognized,
        raw_text_bytes: input.raw_text.len(),
        raw_text_echo: input.raw_text,
        sanitize_instructions,
    })
}

fn build_instructions() -> String {
    r#"NEXT STEP (mandatory): produce a sanitized version of raw_text_echo and call commit_sanitized_context.

Rules for sanitization:
- REMOVE / REPLACE with placeholder these patterns:
  * Connection strings (DefaultEndpointsProtocol=...AccountKey=...)
  * AccountKey= values, SAS tokens (sv=...&sig=...)
  * `Authorization: Bearer <jwt>` tokens
  * PEM blocks (-----BEGIN ... PRIVATE KEY-----)
  * AWS access keys (AKIA...), GitHub PATs (ghp_...), generic API keys
  * Passwords, account passwords, OAuth client secrets
  * Personal email addresses unrelated to the ticket
  * IP addresses of internal hosts when not relevant to the error
- KEEP (these are not secrets and ARE important for Microsoft Support):
  * Azure resource IDs (/subscriptions/.../resourceGroups/.../providers/...)
  * Subscription / tenant GUIDs
  * Error codes, HTTP status codes, exception types
  * Stack traces (with secrets scrubbed)
  * Timestamps, region/zone info, correlation/request IDs
  * Severity indicators ("FATAL", "ERROR", "503")
- Replace removed values with `[REDACTED:<KIND>]` so the support engineer
  knows what type of value was there (e.g. `[REDACTED:STORAGE_KEY]`).
- Track each redaction so you can produce a redacted_summary like:
  "Redacted 2 items: BEARER_TOKEN at line 142, STORAGE_KEY at line 891"

Then call:
  commit_sanitized_context(
    sanitize_token: <token from this response>,
    sanitized_text: <cleaned text>,
    redacted_summary: <human-readable summary>,
  )

The MCP will refuse the commit if it detects an unambiguous catastrophic
secret pattern still present (storage conn string, account key, private key
block, Bearer JWT). On rejection, re-sanitize and retry."#
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    async fn fresh_state() -> AppState {
        let tmp = tempfile::tempdir().unwrap();
        let mut cfg = Config::default();
        cfg.cache.path = tmp.path().join("cache.sqlite");
        cfg.drafts.sqlite_path = tmp.path().join("drafts.sqlite");
        // Leak the tmpdir so paths stay valid for the test (cheap; we drop
        // the whole process at test end).
        std::mem::forget(tmp);
        crate::bootstrap::ensure_initialized(&cfg).await.unwrap()
    }

    #[tokio::test]
    async fn rejects_empty_text() {
        let s = fresh_state().await;
        let err = run(
            &s,
            Input {
                raw_text: String::new(),
                caller_hints: None,
                extraction_blob_only: false,
            },
        )
        .await
        .unwrap_err();
        assert!(format!("{err}").contains("empty"));
    }

    #[tokio::test]
    async fn rejects_oversized_text() {
        let s = fresh_state().await;
        let huge = "x".repeat(MAX_RAW_BYTES + 1);
        let err = run(
            &s,
            Input {
                raw_text: huge,
                caller_hints: None,
                extraction_blob_only: false,
            },
        )
        .await
        .unwrap_err();
        assert!(format!("{err}").contains("exceeds"));
    }

    #[tokio::test]
    async fn happy_path_returns_token_and_recognizes_arm_id() {
        let s = fresh_state().await;
        let raw = r#"Operation on /subscriptions/00000000-0000-0000-0000-000000000001/resourceGroups/test-genai/providers/Microsoft.Storage/storageAccounts/foo failed with HTTP/1.1 503"#;
        let out = run(
            &s,
            Input {
                raw_text: raw.into(),
                caller_hints: None,
                extraction_blob_only: false,
            },
        )
        .await
        .unwrap();
        assert!(out.sanitize_token.starts_with("san_"));
        assert!(out.recognized.matched.contains(&"resource_id".to_string()));
        assert!(out.recognized.matched.contains(&"http_status".to_string()));
        assert_eq!(
            out.recognized.fields.severity_hint.as_deref(),
            Some("critical")
        );
        assert_eq!(out.raw_text_bytes, raw.len());
        assert_eq!(out.raw_text_echo, raw);
        assert!(out
            .sanitize_instructions
            .contains("commit_sanitized_context"));
    }

    #[tokio::test]
    async fn blob_only_skips_recognizers() {
        let s = fresh_state().await;
        let raw = "Operation on /subscriptions/00000000-0000-0000-0000-000000000001/resourceGroups/a/providers/Microsoft.Storage/storageAccounts/b failed";
        let out = run(
            &s,
            Input {
                raw_text: raw.into(),
                caller_hints: None,
                extraction_blob_only: true,
            },
        )
        .await
        .unwrap();
        assert!(out.recognized.matched.is_empty());
        assert!(out.recognized.fields.resource_id.is_none());
    }
}
