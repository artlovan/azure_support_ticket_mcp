//! Review-token + draft-hash confirmation guard.
//!
//! - `issue` is called whenever a draft is mutated; rotates the token, returns
//!   the matching content hash.
//! - `verify` is called from every side-effecting tool; rejects if the token
//!   is unknown/expired, if the recomputed hash doesn't match, or if
//!   `confirmed != true`.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use parking_lot::RwLock;
use uuid::Uuid;

use crate::error::{AppError, AppResult};

use super::draft::TicketDraft;

const TOKEN_TTL: Duration = Duration::from_secs(30 * 60); // 30 min idle

#[derive(Clone)]
pub struct ReviewIssue {
    pub review_token: String,
    pub draft_hash: String,
}

#[derive(Clone)]
struct Entry {
    /// Generic intent key (draft_id for create flows, or e.g. `update:<ticket>` for CRUD).
    intent_key: String,
    expected_hash: String,
    expires_at: SystemTime,
}

#[derive(Default)]
pub struct ReviewTokens {
    inner: RwLock<HashMap<String, Entry>>,
}

pub type SharedReviewTokens = Arc<ReviewTokens>;

impl ReviewTokens {
    pub fn new() -> Self {
        Self::default()
    }

    /// Issue a token bound to the current draft content.
    pub fn issue(&self, draft: &TicketDraft) -> ReviewIssue {
        self.issue_for_intent(draft.draft_id.clone(), draft.content_hash())
    }

    /// Issue a token bound to an arbitrary intent (e.g. a `(ticket, patch)` pair).
    /// The caller computes `intent_hash` deterministically.
    pub fn issue_for_intent(&self, intent_key: String, intent_hash: String) -> ReviewIssue {
        let token = format!("rt_{}", Uuid::new_v7(uuid::Timestamp::now(uuid::NoContext)));
        let entry = Entry {
            intent_key,
            expected_hash: intent_hash.clone(),
            expires_at: SystemTime::now() + TOKEN_TTL,
        };
        self.inner.write().insert(token.clone(), entry);
        ReviewIssue {
            review_token: token,
            draft_hash: intent_hash,
        }
    }

    /// Verify a confirmation call. Returns the `intent_key` (e.g. draft_id)
    /// bound to the token on success.
    pub fn verify(
        &self,
        review_token: &str,
        provided_hash: &str,
        confirmed: bool,
    ) -> AppResult<String> {
        if !confirmed {
            return Err(AppError::Validation(
                "confirmed must be true to perform this side-effecting action".into(),
            ));
        }
        let mut guard = self.inner.write();
        let entry = guard
            .get(review_token)
            .cloned()
            .ok_or_else(|| AppError::Validation("review_token unknown or expired".into()))?;
        if entry.expires_at <= SystemTime::now() {
            guard.remove(review_token);
            return Err(AppError::Validation("review_token expired".into()));
        }
        if entry.expected_hash != provided_hash {
            return Err(AppError::Validation(format!(
                "draft_hash mismatch: provided {provided_hash} expected {expected}; rebuild the draft and confirm again",
                expected = entry.expected_hash
            )));
        }
        Ok(entry.intent_key)
    }

    /// Verify that a freshly-recomputed hash matches what the token expects.
    /// Defensive double-check in case the draft was mutated between
    /// `verify` and the final read.
    pub fn check_hash(&self, review_token: &str, current_hash: &str) -> AppResult<()> {
        let guard = self.inner.read();
        let entry = guard
            .get(review_token)
            .ok_or_else(|| AppError::Validation("review_token unknown or expired".into()))?;
        if entry.expected_hash != current_hash {
            return Err(AppError::Validation(
                "draft was modified between confirmation and submission; rebuild".into(),
            ));
        }
        Ok(())
    }

    pub fn revoke(&self, review_token: &str) {
        self.inner.write().remove(review_token);
    }

    /// Drop all tokens bound to a given intent (called after deletion / submit).
    pub fn revoke_draft(&self, intent_key: &str) {
        let mut g = self.inner.write();
        g.retain(|_, e| e.intent_key != intent_key);
    }

    pub fn gc_expired(&self) -> usize {
        let now = SystemTime::now();
        let mut g = self.inner.write();
        let before = g.len();
        g.retain(|_, e| e.expires_at > now);
        before - g.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issue_and_verify_roundtrip() {
        let tokens = ReviewTokens::new();
        let mut d = TicketDraft::new();
        d.title = Some("hi".into());
        let issued = tokens.issue(&d);
        let did = tokens
            .verify(&issued.review_token, &issued.draft_hash, true)
            .unwrap();
        assert_eq!(did, d.draft_id);
    }

    #[test]
    fn verify_requires_confirmed_true() {
        let tokens = ReviewTokens::new();
        let d = TicketDraft::new();
        let issued = tokens.issue(&d);
        let e = tokens
            .verify(&issued.review_token, &issued.draft_hash, false)
            .unwrap_err();
        assert!(format!("{e}").contains("confirmed must be true"));
    }

    #[test]
    fn verify_rejects_hash_drift() {
        let tokens = ReviewTokens::new();
        let mut d = TicketDraft::new();
        d.title = Some("a".into());
        let issued = tokens.issue(&d);
        d.title = Some("b".into()); // mutate
        let stale_hash = d.content_hash();
        let e = tokens
            .verify(&issued.review_token, &stale_hash, true)
            .unwrap_err();
        assert!(format!("{e}").contains("draft_hash mismatch"));
    }

    #[test]
    fn verify_rejects_unknown_token() {
        let tokens = ReviewTokens::new();
        let e = tokens
            .verify("rt_does_not_exist", "sha256:0", true)
            .unwrap_err();
        assert!(format!("{e}").contains("unknown"));
    }
}
