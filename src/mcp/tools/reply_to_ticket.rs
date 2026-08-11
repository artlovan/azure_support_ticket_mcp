//! `reply_to_ticket`: two-call confirmation flow (same pattern as
//! `update_support_ticket`).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tracing::info;

use crate::azure::support::communications::{
    create_communication, deterministic_communication_name,
};
use crate::bootstrap::AppState;
use crate::error::{AppError, AppResult};
use crate::workflow::draft::hash_intent;

const INTENT_PREFIX: &str = "reply_to_ticket:";

#[derive(Debug, Deserialize, JsonSchema)]
pub struct Input {
    pub subscription_id: String,
    pub ticket_name: String,
    pub subject: String,
    pub body: String,
    /// Optional override; UUID-based name is generated otherwise.
    #[serde(default)]
    pub communication_name: Option<String>,
    /// Optional sender email override; usually inferred by Azure.
    #[serde(default)]
    pub sender_email: Option<String>,

    // ---- confirmation ----
    #[serde(default)]
    pub review_token: Option<String>,
    #[serde(default)]
    pub draft_hash: Option<String>,
    #[serde(default)]
    pub confirmed: Option<bool>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Output {
    pub phase: String,
    pub ticket_name: String,
    pub communication_name: String,
    #[schemars(schema_with = "crate::mcp::schema::any_json_schema")]
    pub intent: Value,
    pub review_token: Option<String>,
    pub draft_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "crate::mcp::schema::any_json_schema")]
    pub created: Option<Value>,
    /// Preformatted markdown to render verbatim during the preview phase. Use
    /// this instead of building your own question — embedded literal `\n`
    /// sequences in hand-built strings render badly in CLI hosts.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confirmation_prompt: Option<String>,
    /// Short one-liner safe for a single-line confirmation widget. Hosts
    /// with a confirmation dialog use this as the question text; hosts that
    /// just ask in chat can ignore it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub question_prompt: Option<String>,
    pub instructions: String,
}

pub async fn run(state: &AppState, input: Input) -> AppResult<Output> {
    if input.subscription_id.trim().is_empty()
        || input.ticket_name.trim().is_empty()
        || input.subject.trim().is_empty()
        || input.body.trim().is_empty()
    {
        return Err(AppError::Validation(
            "subscription_id, ticket_name, subject, and body are required".into(),
        ));
    }
    // Derive `comm_name` deterministically from the user-meaningful intent
    // so the preview-call name and the submit-call name always match. Prior
    // bug: a random UUID was generated on each call and folded into the
    // hashed intent, so when the assistant called preview (name=A, hash=H1)
    // and then submit without re-passing communication_name (name=B,
    // hash=H2 ≠ H1), the second call failed with "reply contents changed".
    // Models eventually figured out the round-trip but only after multiple
    // retries — fix by making the name a pure function of inputs.
    //
    // Callers can still override via input.communication_name (useful for
    // tests or for resuming a draft generated server-side previously).
    let comm_name = input.communication_name.clone().unwrap_or_else(|| {
        deterministic_communication_name(
            &input.ticket_name,
            &input.subject,
            &input.body,
            input.sender_email.as_deref(),
        )
    });
    // NOTE: `communication_name` is intentionally EXCLUDED from the hashed
    // intent below. The hash covers only user-meaningful state (ticket +
    // subject + body + sender_email). Including comm_name would re-introduce
    // the original bug: any divergence in name generation would invalidate
    // the hash even when the user's intent is identical.
    let intent = json!({
        "ticket_name": input.ticket_name,
        "subject": input.subject,
        "body": input.body,
        "sender_email": input.sender_email,
    });
    let intent_key = format!("{INTENT_PREFIX}{}", input.ticket_name);
    let intent_hash = hash_intent(&intent)?;

    match (
        input.review_token.as_deref(),
        input.draft_hash.as_deref(),
        input.confirmed,
    ) {
        (None, _, _) | (_, None, _) | (_, _, None | Some(false)) => {
            state.review_tokens.revoke_draft(&intent_key);
            let issued = state
                .review_tokens
                .issue_for_intent(intent_key, intent_hash.clone());
            let prompt = render_reply_prompt(
                &input.ticket_name,
                &input.subject,
                &input.body,
                input.sender_email.as_deref(),
            );
            Ok(Output {
                phase: "preview".into(),
                ticket_name: input.ticket_name,
                communication_name: comm_name,
                intent,
                review_token: Some(issued.review_token),
                draft_hash: Some(issued.draft_hash),
                created: None,
                confirmation_prompt: Some(prompt),
                question_prompt: Some("Post this reply?".into()),
                instructions:
                    "TWO STEPS: (1) SHOW `confirmation_prompt` to the user VERBATIM (markdown — render in chat; if your environment has a separate confirmation widget that strips formatting, still print to chat first). Do NOT paraphrase. (2) THEN ask for confirmation using whatever interaction your environment supports (confirmation widget with `question_prompt` + 3 choices, or plain chat question). Reply handling: yes/1/post → re-call reply_to_ticket with review_token+draft_hash+confirmed=true; cancel/3 → stop; ANY other free-form reply → treat as edits to subject/body and re-call WITHOUT review_token."
                        .into(),
            })
        }
        (Some(token), Some(hash), Some(true)) => {
            let bound_key = state.review_tokens.verify(token, hash, true)?;
            if bound_key != intent_key {
                return Err(AppError::Validation(format!(
                    "review_token is bound to a different reply intent (`{bound_key}`)"
                )));
            }
            if hash != intent_hash {
                return Err(AppError::Validation(
                    "reply contents changed since the review_token was issued; re-run without review_token to get a fresh preview"
                        .into(),
                ));
            }
            let (arm, _chain) = super::arm_for(state)?;
            info!(
                ticket_name = %input.ticket_name,
                communication_name = %comm_name,
                "posting ticket communication"
            );
            let created = create_communication(
                &arm,
                &input.subscription_id,
                &input.ticket_name,
                &comm_name,
                &input.subject,
                &input.body,
                input.sender_email.as_deref(),
            )
            .await?;
            state.review_tokens.revoke(token);
            Ok(Output {
                phase: "applied".into(),
                ticket_name: input.ticket_name,
                communication_name: comm_name,
                intent,
                review_token: None,
                draft_hash: None,
                created: Some(created),
                confirmation_prompt: None,
                question_prompt: None,
                instructions:
                    "Reply posted. Use list_ticket_communications to see the updated thread.".into(),
            })
        }
    }
}

/// Build a multi-line markdown preview of the pending reply. Clients are
/// instructed to render verbatim so newlines survive (avoids the literal
/// `\n\n` glitch when assistants hand-build prompts).
fn render_reply_prompt(
    ticket_name: &str,
    subject: &str,
    body: &str,
    sender_email: Option<&str>,
) -> String {
    let mut s = String::new();
    s.push_str(&format!(
        "**Post this reply to ticket `{ticket_name}`?**\n\n"
    ));
    s.push_str(&format!("**Subject:** {subject}\n\n"));
    if let Some(from) = sender_email {
        s.push_str(&format!("**From:** {from}\n\n"));
    }
    s.push_str("**Message:**\n\n");
    // Quote the body so multi-line / markdown content stays visually distinct.
    for line in body.lines() {
        s.push_str("> ");
        s.push_str(line);
        s.push('\n');
    }
    if body.is_empty() {
        s.push_str("> _(empty)_\n");
    }
    s.push('\n');
    s.push_str("**Reply with one of:**\n");
    s.push_str("1. **Yes, post** — send to Azure now.\n");
    s.push_str("2. **Your edits, inline** — type changes in plain English (e.g. _'shorten the body, change subject to …'_) and I'll re-preview. No need to pick this option first — any non-yes/cancel reply is treated as edits.\n");
    s.push_str("3. **Cancel** — don't post.\n");
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_preserves_real_newlines_not_escaped() {
        let p = render_reply_prompt(
            "2605290040000074",
            "Re: HTTP 429",
            "line one\nline two",
            None,
        );
        assert!(p.contains("**Subject:** Re: HTTP 429"));
        assert!(p.contains("> line one\n> line two"));
        // critical: no literal backslash-n escapes
        assert!(!p.contains("\\n"));
        // multi-line
        assert!(p.lines().count() > 6, "expected multi-line, got:\n{p}");
    }

    #[test]
    fn prompt_includes_three_options_and_inline_edit_hint() {
        let p = render_reply_prompt("t", "s", "b", Some("a@b.com"));
        assert!(p.contains("**From:** a@b.com"));
        assert!(p.contains("Yes, post"));
        assert!(p.contains("Your edits, inline"));
        assert!(p.contains("Cancel"));
    }

    // Regression guard for the multi-retry hash-mismatch loop: when the
    // assistant calls reply_to_ticket twice with the same user inputs (once
    // to get a preview, once to confirm) and does NOT round-trip the
    // server-generated communication_name, the hash computed on the second
    // call MUST match the first. Prior bug: comm_name was random per call
    // and folded into the hash, so the second call always mismatched.
    //
    // We assert this at the hash layer (cheapest, most direct test) rather
    // than spinning up a full AppState + RPC roundtrip. The contract that
    // matters is "same user-meaningful inputs → same hash".
    #[test]
    fn intent_hash_is_stable_across_calls_without_round_tripping_comm_name() {
        // Build the SAME intent the way `run` does — minus the
        // communication_name (which must NOT be part of the hash).
        let intent = serde_json::json!({
            "ticket_name": "tkt-123",
            "subject": "Re: HTTP 429",
            "body": "Additional symptom: also seeing 404s.",
            "sender_email": "alice@example.com",
        });
        let h1 = hash_intent(&intent).unwrap();
        let h2 = hash_intent(&intent).unwrap();
        assert_eq!(h1, h2, "same user-meaningful intent must hash identically");

        // Sanity: changing any user-meaningful field DOES change the hash.
        let mut different = intent.clone();
        different["body"] = serde_json::json!("Additional symptom: also seeing 500s.");
        assert_ne!(hash_intent(&different).unwrap(), h1);
    }

    #[test]
    fn deterministic_comm_name_matches_across_calls() {
        // Direct test of the helper that drives Bug 2's fix. Two
        // independent calls with identical inputs MUST return the same
        // name — that's what makes the preview-then-confirm flow work
        // without round-tripping the name through the assistant.
        let a = deterministic_communication_name(
            "tkt-123",
            "Re: HTTP 429",
            "body text",
            Some("alice@example.com"),
        );
        let b = deterministic_communication_name(
            "tkt-123",
            "Re: HTTP 429",
            "body text",
            Some("alice@example.com"),
        );
        assert_eq!(a, b);
        assert!(a.starts_with("comm-"));
        // Different inputs → different name.
        let c = deterministic_communication_name(
            "tkt-123",
            "Re: HTTP 429",
            "different body",
            Some("alice@example.com"),
        );
        assert_ne!(a, c);
    }
}
