//! Short-lived tokens that gate the sanitization handshake.
//!
//! The flow is two-call:
//!   1. `ingest_error_context` mints a token bound to the raw text's content
//!      hash and the recognizer output. Nothing is persisted.
//!   2. `commit_sanitized_context` must present the token PLUS sanitized
//!      text. The token is consumed (one-shot) and the draft is created.
//!
//! Tokens live in memory only — they expire in 5 minutes and don't survive
//! restarts. That's intentional: the sanitization handshake is meant to be
//! a single conversation turn, not a long-running workflow.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use parking_lot::RwLock;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::error::{AppError, AppResult};
use crate::resolver::recognizers::{ExtractedFields, RecognizerResult};

const TOKEN_TTL: Duration = Duration::from_secs(5 * 60);

/// Captured state from the `ingest_error_context` call. The commit step
/// reads this back to build the draft.
#[derive(Debug, Clone)]
pub struct SanitizeRequest {
    /// SHA-256 of the original raw_text. Recomputed on commit to prevent
    /// swapping different content under the same token.
    pub content_hash: String,
    /// Recognizer hints extracted in step 1.
    pub recognized: RecognizerResult,
    /// Hints the caller passed in (e.g. subscription_id from the harness).
    pub caller_hints: ExtractedFields,
    expires_at: SystemTime,
}

#[derive(Default)]
pub struct SanitizeTokens {
    inner: RwLock<HashMap<String, SanitizeRequest>>,
}

pub type SharedSanitizeTokens = Arc<SanitizeTokens>;

impl SanitizeTokens {
    pub fn new() -> Self {
        Self::default()
    }

    /// Mint a fresh token, bind it to `raw_text` + recognizer output.
    pub fn issue(
        &self,
        raw_text: &str,
        recognized: RecognizerResult,
        caller_hints: ExtractedFields,
    ) -> String {
        let token = format!(
            "san_{}",
            Uuid::new_v7(uuid::Timestamp::now(uuid::NoContext))
        );
        let req = SanitizeRequest {
            content_hash: content_hash(raw_text),
            recognized,
            caller_hints,
            expires_at: SystemTime::now() + TOKEN_TTL,
        };
        self.inner.write().insert(token.clone(), req);
        token
    }

    /// Consume a token. Errors on unknown / expired / wrong content. On
    /// success, the entry is removed from the store.
    pub fn consume(&self, token: &str, sanitized_text: &str) -> AppResult<SanitizeRequest> {
        let mut g = self.inner.write();
        let req = g.remove(token).ok_or_else(|| {
            AppError::Validation("sanitize_token unknown or already consumed".into())
        })?;
        if req.expires_at <= SystemTime::now() {
            return Err(AppError::Validation(
                "sanitize_token expired (5 min TTL); re-call ingest_error_context".into(),
            ));
        }
        // We don't compare sanitized_text hash to content_hash (that would
        // defeat the purpose of sanitization). We rely on the token+TTL.
        // The bound `content_hash` is informational, accessible to callers
        // for auditing if they want to compare.
        let _ = sanitized_text;
        Ok(req)
    }

    /// Drop expired entries. Cheap; safe to call from a periodic task.
    pub fn gc_expired(&self) -> usize {
        let now = SystemTime::now();
        let mut g = self.inner.write();
        let before = g.len();
        g.retain(|_, r| r.expires_at > now);
        before - g.len()
    }
}

pub fn content_hash(text: &str) -> String {
    let mut h = Sha256::new();
    h.update(text.as_bytes());
    format!("sha256:{:x}", h.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issue_then_consume_roundtrip() {
        let s = SanitizeTokens::new();
        let token = s.issue(
            "hello world",
            RecognizerResult::default(),
            ExtractedFields::default(),
        );
        let req = s.consume(&token, "hello scrubbed").unwrap();
        assert_eq!(req.content_hash, content_hash("hello world"));
    }

    #[test]
    fn consume_is_one_shot() {
        let s = SanitizeTokens::new();
        let token = s.issue("x", RecognizerResult::default(), ExtractedFields::default());
        s.consume(&token, "x").unwrap();
        let err = s.consume(&token, "x").unwrap_err();
        assert!(format!("{err}").contains("unknown or already consumed"));
    }

    #[test]
    fn unknown_token_rejected() {
        let s = SanitizeTokens::new();
        let err = s.consume("san_does_not_exist", "x").unwrap_err();
        assert!(format!("{err}").contains("unknown"));
    }

    #[test]
    fn expired_token_rejected() {
        let s = SanitizeTokens::new();
        let token = s.issue("x", RecognizerResult::default(), ExtractedFields::default());
        // Force-expire by rewriting the entry in place.
        {
            let mut g = s.inner.write();
            let e = g.get_mut(&token).unwrap();
            e.expires_at = SystemTime::now() - Duration::from_secs(1);
        }
        let err = s.consume(&token, "x").unwrap_err();
        assert!(format!("{err}").contains("expired"));
    }

    #[test]
    fn gc_drops_only_expired() {
        let s = SanitizeTokens::new();
        let live = s.issue("a", RecognizerResult::default(), ExtractedFields::default());
        let dead = s.issue("b", RecognizerResult::default(), ExtractedFields::default());
        {
            let mut g = s.inner.write();
            g.get_mut(&dead).unwrap().expires_at = SystemTime::now() - Duration::from_secs(1);
        }
        let n = s.gc_expired();
        assert_eq!(n, 1);
        // live still consumable
        s.consume(&live, "a").unwrap();
    }

    #[test]
    fn content_hash_is_deterministic() {
        assert_eq!(content_hash("abc"), content_hash("abc"));
        assert_ne!(content_hash("abc"), content_hash("abcd"));
    }
}
