//! `update_support_ticket`: PATCH severity / status / contact / consent.
//! Two-call flow: first call (no review_token) returns a preview + freshly
//! issued review_token + draft_hash; second call (with token + hash + confirmed)
//! performs the PATCH.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tracing::info;

use crate::azure::support::tickets::patch_ticket;
use crate::bootstrap::AppState;
use crate::error::{AppError, AppResult};
use crate::workflow::draft::{hash_intent, normalize_consent};

const INTENT_PREFIX: &str = "update_ticket:";

#[derive(Debug, Deserialize, JsonSchema)]
pub struct Input {
    pub subscription_id: String,
    pub ticket_name: String,

    /// Allowed: minimal|moderate|critical|highestcriticalimpact (case-insensitive).
    #[serde(default)]
    pub severity: Option<String>,
    /// Allowed: open|closed (case-insensitive). Normalised to lowercase
    /// before sending — Azure rejects PascalCase here with
    /// InvalidParameterValue.
    #[serde(default)]
    pub status: Option<String>,
    /// Allowed: yes|no (case-insensitive). Normalised to PascalCase.
    #[serde(default)]
    pub advanced_diagnostic_consent: Option<String>,
    /// Partial contact details. Only present fields are sent.
    #[serde(default)]
    pub contact: Option<ContactPatch>,

    // ---- confirmation ----
    #[serde(default)]
    pub review_token: Option<String>,
    #[serde(default)]
    pub draft_hash: Option<String>,
    #[serde(default)]
    pub confirmed: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema, Serialize, Clone)]
pub struct ContactPatch {
    #[serde(default)]
    pub first_name: Option<String>,
    #[serde(default)]
    pub last_name: Option<String>,
    #[serde(default)]
    pub primary_email_address: Option<String>,
    /// Replace the CC recipient list (Azure's "Who else should we email?").
    /// Pass `[]` to clear all CCs, or a full new list to overwrite.
    #[serde(default)]
    pub additional_email_addresses: Option<Vec<String>>,
    #[serde(default)]
    pub phone_number: Option<String>,
    #[serde(default)]
    pub preferred_contact_method: Option<String>,
    #[serde(default)]
    pub preferred_support_language: Option<String>,
    #[serde(default)]
    pub preferred_time_zone: Option<String>,
    #[serde(default)]
    pub country: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Output {
    /// "preview" on the first call, "applied" after a successful PATCH.
    pub phase: String,
    pub ticket_name: String,
    #[schemars(schema_with = "crate::mcp::schema::any_json_schema")]
    pub patch_properties: Value,
    pub review_token: Option<String>,
    pub draft_hash: Option<String>,
    /// Updated ticket body (only when phase == "applied").
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "crate::mcp::schema::any_json_schema")]
    pub updated: Option<Value>,
    /// Preformatted markdown for the preview phase — render verbatim. Avoids
    /// the literal `\n` glitch from assistants hand-building prompts.
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
    if input.subscription_id.trim().is_empty() || input.ticket_name.trim().is_empty() {
        return Err(AppError::Validation(
            "subscription_id and ticket_name are required".into(),
        ));
    }
    let patch = build_patch(&input)?;
    if patch.as_object().map(|o| o.is_empty()).unwrap_or(true) {
        return Err(AppError::Validation(
            "at least one mutable field (severity, status, advanced_diagnostic_consent, contact) must be provided".into(),
        ));
    }
    let intent_key = format!("{INTENT_PREFIX}{}", input.ticket_name);
    let intent_hash = hash_intent(&patch)?;

    match (
        input.review_token.as_deref(),
        input.draft_hash.as_deref(),
        input.confirmed,
    ) {
        (None, _, _) | (_, None, _) | (_, _, None | Some(false)) => {
            // PHASE 1 — issue a token bound to (ticket, patch).
            state.review_tokens.revoke_draft(&intent_key);
            let issued = state
                .review_tokens
                .issue_for_intent(intent_key, intent_hash.clone());
            let prompt = render_update_prompt(&input.ticket_name, &patch);
            Ok(Output {
                phase: "preview".into(),
                ticket_name: input.ticket_name,
                patch_properties: patch,
                review_token: Some(issued.review_token),
                draft_hash: Some(issued.draft_hash),
                updated: None,
                confirmation_prompt: Some(prompt),
                question_prompt: Some("Apply this update?".into()),
                instructions:
                    "TWO STEPS: (1) SHOW `confirmation_prompt` to the user VERBATIM (markdown — render in chat; if your environment has a separate confirmation widget that strips formatting, still print to chat first). (2) THEN ask for confirmation using whatever interaction your environment supports (confirmation widget with `question_prompt` + 3 choices, or plain chat question). Reply handling: yes/1 → re-call with review_token+draft_hash+confirmed=true; cancel/3 → stop; ANY other free-form reply → treat as edits, re-call WITHOUT review_token."
                        .into(),
            })
        }
        (Some(token), Some(hash), Some(true)) => {
            // PHASE 2 — verify + apply.
            let bound_key = state.review_tokens.verify(token, hash, true)?;
            if bound_key != intent_key {
                return Err(AppError::Validation(format!(
                    "review_token is bound to a different ticket/patch intent (`{bound_key}`)"
                )));
            }
            if hash != intent_hash {
                return Err(AppError::Validation(
                    "patch contents changed since the review_token was issued; re-run without review_token to get a fresh preview"
                        .into(),
                ));
            }
            let (arm, _chain) = super::arm_for(state)?;
            info!(
                ticket_name = %input.ticket_name,
                subscription_id = %input.subscription_id,
                "patching support ticket"
            );
            let updated =
                patch_ticket(&arm, &input.subscription_id, &input.ticket_name, &patch).await?;
            state.review_tokens.revoke(token);

            // Write-through to local cache (best-effort).
            crate::cache::tickets::upsert_from_arm(
                &state.cache,
                &input.subscription_id,
                &input.ticket_name,
                None,
                &updated,
                "update",
            )
            .await;

            Ok(Output {
                phase: "applied".into(),
                ticket_name: input.ticket_name,
                patch_properties: patch,
                review_token: None,
                draft_hash: None,
                updated: Some(updated),
                confirmation_prompt: None,
                question_prompt: None,
                instructions: "Patch applied. Use get_support_ticket to re-read.".into(),
            })
        }
    }
}

/// Render a friendly markdown preview of the pending PATCH so clients can
/// render it verbatim instead of constructing literal-`\n` prompts.
fn render_update_prompt(ticket_name: &str, patch: &Value) -> String {
    let mut s = String::new();
    s.push_str(&format!(
        "**Apply this update to ticket `{ticket_name}`?**\n\n"
    ));
    s.push_str("| Field | New value |\n");
    s.push_str("|---|---|\n");
    if let Some(obj) = patch.as_object() {
        let mut keys: Vec<_> = obj.keys().collect();
        keys.sort();
        for k in keys {
            let v = obj.get(k).cloned().unwrap_or(Value::Null);
            // Friendly labels for the most common fields.
            let label = match k.as_str() {
                "severity" => "Severity",
                "status" => "Status",
                "advancedDiagnosticConsent" => "Diagnostic consent",
                "contactDetails" => "Contact details",
                other => other,
            };
            let value_str = match (&v, k.as_str()) {
                (Value::String(s), "severity") => {
                    crate::workflow::share::severity_label(s).to_string()
                }
                (Value::Object(map), "contactDetails") => {
                    let mut parts: Vec<String> = map
                        .iter()
                        .map(|(fk, fv)| {
                            let pretty_label = match fk.as_str() {
                                "primaryEmailAddress" => "email".to_string(),
                                "additionalEmailAddresses" => "CC".to_string(),
                                "phoneNumber" => "phone".to_string(),
                                "preferredContactMethod" => "method".to_string(),
                                "preferredSupportLanguage" => "language".to_string(),
                                "preferredTimeZone" => "time zone".to_string(),
                                other => other.to_string(),
                            };
                            let pretty_value = match fv {
                                Value::Array(arr) => arr
                                    .iter()
                                    .filter_map(|x| x.as_str().map(str::to_string))
                                    .collect::<Vec<_>>()
                                    .join(", "),
                                Value::String(s) => s.clone(),
                                other => other.to_string(),
                            };
                            format!("{pretty_label}: {pretty_value}")
                        })
                        .collect();
                    parts.sort();
                    parts.join("; ")
                }
                (Value::String(s), _) => s.clone(),
                (other, _) => other.to_string(),
            };
            s.push_str(&format!("| {label} | {value_str} |\n"));
        }
    }
    s.push('\n');
    s.push_str("**Reply with one of:**\n");
    s.push_str("1. **Yes, apply** — send the PATCH to Azure now.\n");
    s.push_str("2. **Your edits, inline** — type changes in plain English (e.g. _'also set severity to B'_) and I'll re-preview. No need to pick this option first — any non-yes/cancel reply is treated as edits.\n");
    s.push_str("3. **Cancel** — don't apply.\n");
    s
}

fn build_patch(input: &Input) -> AppResult<Value> {
    let mut obj = serde_json::Map::new();
    if let Some(s) = &input.severity {
        let norm = normalize_severity(s)?;
        obj.insert("severity".into(), Value::String(norm));
    }
    if let Some(s) = &input.status {
        let norm = normalize_status(s)?;
        obj.insert("status".into(), Value::String(norm));
    }
    if let Some(s) = &input.advanced_diagnostic_consent {
        let norm = normalize_consent(s)?;
        obj.insert("advancedDiagnosticConsent".into(), Value::String(norm));
    }
    if let Some(c) = &input.contact {
        let mut cm = serde_json::Map::new();
        if let Some(v) = &c.first_name {
            cm.insert("firstName".into(), json!(v));
        }
        if let Some(v) = &c.last_name {
            cm.insert("lastName".into(), json!(v));
        }
        if let Some(v) = &c.primary_email_address {
            cm.insert("primaryEmailAddress".into(), json!(v));
        }
        if let Some(v) = &c.additional_email_addresses {
            cm.insert("additionalEmailAddresses".into(), json!(v));
        }
        if let Some(v) = &c.phone_number {
            cm.insert("phoneNumber".into(), json!(v));
        }
        if let Some(v) = &c.preferred_contact_method {
            cm.insert("preferredContactMethod".into(), json!(v));
        }
        if let Some(v) = &c.preferred_support_language {
            cm.insert("preferredSupportLanguage".into(), json!(v));
        }
        if let Some(v) = &c.preferred_time_zone {
            cm.insert("preferredTimeZone".into(), json!(v));
        }
        if let Some(v) = &c.country {
            cm.insert("country".into(), json!(v));
        }
        if !cm.is_empty() {
            obj.insert("contactDetails".into(), Value::Object(cm));
        }
    }
    Ok(Value::Object(obj))
}

/// Normalize severity to the lowercase form Azure Support REST expects.
/// Accepts any case so the calling agent isn't tripped up by display vs API
/// casing.
fn normalize_severity(s: &str) -> AppResult<String> {
    let lower = s.trim().to_ascii_lowercase();
    match lower.as_str() {
        "minimal" | "moderate" | "critical" | "highestcriticalimpact" => Ok(lower),
        other => Err(AppError::Validation(format!(
            "severity must be minimal|moderate|critical|highestcriticalimpact (case-insensitive), got `{other}`"
        ))),
    }
}

/// Normalize status to the lowercase form Azure Support REST expects on
/// PATCH. Sending PascalCase here yields `InvalidParameterValue` 400 even
/// though GET responses return PascalCase (`Open`/`Closed`).
fn normalize_status(s: &str) -> AppResult<String> {
    let lower = s.trim().to_ascii_lowercase();
    match lower.as_str() {
        "open" | "closed" => Ok(lower),
        other => Err(AppError::Validation(format!(
            "status must be open|closed (case-insensitive), got `{other}`"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input_with_status(s: &str) -> Input {
        Input {
            subscription_id: "sub".into(),
            ticket_name: "tkt".into(),
            severity: None,
            status: Some(s.into()),
            advanced_diagnostic_consent: None,
            contact: None,
            review_token: None,
            draft_hash: None,
            confirmed: None,
        }
    }

    #[test]
    fn status_normalizes_to_lowercase_for_azure() {
        for variant in ["Closed", "closed", "CLOSED", "  Closed  "] {
            let p = build_patch(&input_with_status(variant)).unwrap();
            assert_eq!(p["status"], "closed", "input was {variant:?}");
        }
        let p = build_patch(&input_with_status("Open")).unwrap();
        assert_eq!(p["status"], "open");
    }

    #[test]
    fn status_rejects_bogus_values() {
        let err = build_patch(&input_with_status("reopened")).unwrap_err();
        assert!(format!("{err}").contains("open|closed"));
    }

    #[test]
    fn severity_normalizes_case() {
        let mut i = input_with_status("Open");
        i.severity = Some("Critical".into());
        let p = build_patch(&i).unwrap();
        assert_eq!(p["severity"], "critical");
    }

    #[test]
    fn consent_pascals() {
        let mut i = input_with_status("Open");
        i.advanced_diagnostic_consent = Some("yes".into());
        let p = build_patch(&i).unwrap();
        assert_eq!(p["advancedDiagnosticConsent"], "Yes");
    }

    #[test]
    fn hash_stable_across_casing() {
        // Preview phase issues a hash on "Closed"; apply phase with "closed"
        // must produce the same hash so the review_token still binds.
        let a = build_patch(&input_with_status("Closed")).unwrap();
        let b = build_patch(&input_with_status("closed")).unwrap();
        let ha = crate::workflow::draft::hash_intent(&a).unwrap();
        let hb = crate::workflow::draft::hash_intent(&b).unwrap();
        assert_eq!(ha, hb);
    }
}
