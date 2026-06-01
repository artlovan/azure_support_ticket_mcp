//! In-memory ticket draft model.
//!
//! The draft is the single source of truth between `start_support_ticket_flow`
//! and `create_support_ticket`. It's hashed canonically so the confirmation
//! guard can detect drift between preview and submission.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::error::{AppError, AppResult};

/// Allowed severity tier names (Azure REST values).
pub const SEVERITY_VALUES: &[&str] = &["minimal", "moderate", "critical", "highestcriticalimpact"];

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default)]
pub struct ContactDetails {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub country: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preferred_contact_method: Option<String>, // "email" | "phone"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preferred_support_language: Option<String>, // e.g. "en-us"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preferred_time_zone: Option<String>, // IANA / Windows tz
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary_email_address: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phone_number: Option<String>,
    /// Extra email addresses CC'd on every Azure Support update for this
    /// ticket (the portal's "Who else should we email?" field). Optional.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub additional_email_addresses: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default)]
pub struct TechnicalDetails {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_id: Option<String>,
}

/// The local draft. `draft_id` is the addressable handle returned to clients
/// (different from `review_token`, which is rotated on every mutation).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default)]
pub struct TicketDraft {
    pub draft_id: String,

    // Scope
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subscription_id: Option<String>,

    // Classification
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub problem_classification_id: Option<String>,

    // Issue
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub severity: Option<String>,

    // Resource
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub problem_start_time: Option<String>, // ISO-8601

    // Required consent + contact
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub advanced_diagnostic_consent: Option<String>, // "Yes" | "No"
    #[serde(default)]
    pub contact_details: ContactDetails,

    // Optional
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub require_24x7_response: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub support_plan_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_workspace_name: Option<String>,
    #[serde(default)]
    pub technical_ticket_details: TechnicalDetails,

    /// Display-only metadata: short summary of what was redacted during the
    /// LLM sanitization step (set by `commit_sanitized_context`). Shown in
    /// the preview so the user can sanity-check the redactions before
    /// submission. NOT sent to Azure — purely a display aid.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redacted_summary: Option<String>,

    // Audit
    pub created_at: i64,
    pub updated_at: i64,
}

impl TicketDraft {
    pub fn new() -> Self {
        let now = crate::cache::now_unix();
        Self {
            draft_id: format!(
                "draft_{}",
                Uuid::new_v7(uuid::Timestamp::now(uuid::NoContext))
            ),
            created_at: now,
            updated_at: now,
            ..Default::default()
        }
    }

    /// Deterministic hash over the draft's canonical JSON. `draft_id`,
    /// `created_at`, and `updated_at` are excluded so the hash reflects only
    /// the user-visible content (the parts the user is being asked to confirm).
    pub fn content_hash(&self) -> String {
        let v = canonicalize(self).expect("draft serializes");
        let mut h = Sha256::new();
        h.update(v.as_bytes());
        format!("sha256:{:x}", h.finalize())
    }

    /// Apply a partial patch (other non-null fields overwrite this draft).
    ///
    /// Field-level normalization happens here, NOT at individual tool
    /// callers, so every input path through this function gets consistent
    /// treatment. Specifically:
    /// - `advanced_diagnostic_consent` is normalized to PascalCase
    ///   (`"yes"` / `"YES"` / `"Yes"` → `"Yes"`; same for `"No"`).
    /// - `severity` is lowercased (Azure expects `minimal` / `moderate` /
    ///   `critical` / `highestcriticalimpact`).
    ///
    /// Values that don't match the normalization rule are passed through
    /// unchanged so the validator can produce a consistent error message.
    pub fn apply_patch(&mut self, patch: &TicketDraftPatch) {
        macro_rules! set {
            ($field:ident) => {
                if let Some(v) = patch.$field.clone() {
                    self.$field = Some(v);
                }
            };
        }
        set!(tenant_id);
        set!(subscription_id);
        set!(service_id);
        set!(problem_classification_id);
        set!(title);
        set!(description);
        set!(resource_id);
        set!(problem_start_time);
        set!(support_plan_id);
        set!(file_workspace_name);

        // Normalize severity to lowercase (Azure's API form).
        if let Some(v) = patch.severity.clone() {
            self.severity = Some(normalize_severity(&v));
        }

        // Normalize consent to PascalCase ("Yes" / "No"). Falls back to
        // the raw value when input is unrecognized so the validator's
        // standard error message fires instead of a different one here.
        if let Some(v) = patch.advanced_diagnostic_consent.clone() {
            self.advanced_diagnostic_consent = Some(normalize_consent_lenient(&v));
        }

        if let Some(v) = patch.require_24x7_response {
            self.require_24x7_response = Some(v);
        }
        if let Some(c) = &patch.contact_details {
            self.contact_details.merge(c);
        }
        if let Some(t) = &patch.technical_ticket_details {
            if let Some(rid) = &t.resource_id {
                self.technical_ticket_details.resource_id = Some(rid.clone());
            }
        }
        self.updated_at = crate::cache::now_unix();
    }
}

impl ContactDetails {
    pub fn merge(&mut self, other: &ContactDetails) {
        macro_rules! set {
            ($field:ident) => {
                if let Some(v) = other.$field.clone() {
                    self.$field = Some(v);
                }
            };
        }
        set!(first_name);
        set!(last_name);
        set!(country);
        set!(preferred_contact_method);
        set!(preferred_support_language);
        set!(preferred_time_zone);
        set!(primary_email_address);
        set!(phone_number);
        if !other.additional_email_addresses.is_empty() {
            self.additional_email_addresses = other.additional_email_addresses.clone();
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
pub struct TicketDraftPatch {
    pub tenant_id: Option<String>,
    pub subscription_id: Option<String>,
    pub service_id: Option<String>,
    pub problem_classification_id: Option<String>,
    pub title: Option<String>,
    pub description: Option<String>,
    pub severity: Option<String>,
    pub resource_id: Option<String>,
    pub problem_start_time: Option<String>,
    pub advanced_diagnostic_consent: Option<String>,
    pub require_24x7_response: Option<bool>,
    pub support_plan_id: Option<String>,
    pub file_workspace_name: Option<String>,
    pub contact_details: Option<ContactDetails>,
    pub technical_ticket_details: Option<TechnicalDetails>,
}

/// Stable JSON serialization for hashing: alphabetically sorted object keys.
/// Excludes `draft_id`, `created_at`, `updated_at`.
fn canonicalize(d: &TicketDraft) -> AppResult<String> {
    let mut v = serde_json::to_value(d).map_err(AppError::from)?;
    if let Some(obj) = v.as_object_mut() {
        obj.remove("draft_id");
        obj.remove("created_at");
        obj.remove("updated_at");
    }
    Ok(canonical_string(&v))
}

fn canonical_string(v: &serde_json::Value) -> String {
    let mut out = String::new();
    write_canonical(v, &mut out);
    out
}

/// Public helper: deterministic hash for any serializable intent (used by the
/// confirmation pattern for update / reply tools).
pub fn hash_intent<T: serde::Serialize>(intent: &T) -> AppResult<String> {
    let v = serde_json::to_value(intent).map_err(AppError::from)?;
    let s = canonical_string(&v);
    let mut h = Sha256::new();
    h.update(s.as_bytes());
    Ok(format!("sha256:{:x}", h.finalize()))
}

/// Normalize `advancedDiagnosticConsent` to Azure's required PascalCase
/// form. Returns the canonical `"Yes"` / `"No"` for recognized input;
/// errors with a clear message for anything else.
///
/// Use this from tool input handlers that need to reject unrecognized
/// input up-front (e.g. `update_ticket`). The data-model layer prefers
/// [`normalize_consent_lenient`] which falls through instead so the
/// shared validator emits the standard error.
pub fn normalize_consent(s: &str) -> AppResult<String> {
    match s.trim().to_ascii_lowercase().as_str() {
        "yes" => Ok("Yes".into()),
        "no" => Ok("No".into()),
        other => Err(AppError::Validation(format!(
            "advanced_diagnostic_consent must be yes|no (case-insensitive), got `{other}`"
        ))),
    }
}

/// Lenient version of [`normalize_consent`] for use inside `apply_patch`:
/// returns the canonical PascalCase form when input is recognized;
/// returns the raw input unchanged when not, so the standard validator
/// can produce the canonical error message ("must be 'Yes' or 'No'")
/// instead of two different errors from two different layers.
pub(crate) fn normalize_consent_lenient(s: &str) -> String {
    match s.trim().to_ascii_lowercase().as_str() {
        "yes" => "Yes".into(),
        "no" => "No".into(),
        _ => s.to_string(),
    }
}

/// Normalize severity to Azure's lowercase API form. Unrecognized input
/// passes through unchanged so the validator emits its standard message.
pub(crate) fn normalize_severity(s: &str) -> String {
    let lower = s.trim().to_ascii_lowercase();
    match lower.as_str() {
        "minimal" | "moderate" | "critical" | "highestcriticalimpact" => lower,
        _ => s.to_string(),
    }
}

fn write_canonical(v: &serde_json::Value, out: &mut String) {
    use serde_json::Value::*;
    match v {
        Null => out.push_str("null"),
        Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Number(n) => out.push_str(&n.to_string()),
        String(s) => {
            out.push_str(&serde_json::to_string(s).unwrap());
        }
        Array(a) => {
            out.push('[');
            for (i, x) in a.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_canonical(x, out);
            }
            out.push(']');
        }
        Object(map) => {
            let mut keys: Vec<&std::string::String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (i, k) in keys.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&serde_json::to_string(*k).unwrap());
                out.push(':');
                write_canonical(&map[*k], out);
            }
            out.push('}');
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_is_stable_across_serialization_order() {
        let mut a = TicketDraft::new();
        a.title = Some("AKS scale".into());
        a.severity = Some("moderate".into());
        a.contact_details.first_name = Some("Ada".into());
        a.contact_details.primary_email_address = Some("ada@example.com".into());

        let mut b = TicketDraft::new();
        b.contact_details.primary_email_address = Some("ada@example.com".into());
        b.contact_details.first_name = Some("Ada".into());
        b.severity = Some("moderate".into());
        b.title = Some("AKS scale".into());

        assert_eq!(a.content_hash(), b.content_hash());
    }

    #[test]
    fn hash_changes_on_content_edit() {
        let mut d = TicketDraft::new();
        d.title = Some("v1".into());
        let h1 = d.content_hash();
        d.title = Some("v2".into());
        assert_ne!(h1, d.content_hash());
    }

    #[test]
    fn patch_applies_only_provided_fields() {
        let mut d = TicketDraft::new();
        d.title = Some("orig".into());
        d.severity = Some("moderate".into());
        let p = TicketDraftPatch {
            severity: Some("critical".into()),
            ..Default::default()
        };
        d.apply_patch(&p);
        assert_eq!(d.title.as_deref(), Some("orig"));
        assert_eq!(d.severity.as_deref(), Some("critical"));
    }

    // --- Consent + severity normalization (regression guard) --------------
    //
    // Regression: build_ticket_draft used to pass through the raw patch
    // value, so models writing `"yes"` got rejected later by the validator
    // with "must be 'Yes' or 'No'". The fix is data-model-layer
    // normalization in apply_patch so EVERY input path (build_ticket_draft,
    // update_ticket, templates) gets the same treatment. These tests guard
    // the data-model behavior so the fix can't drift to one tool only again.
    //
    // Coverage rationale: one parametrized test per field covers the whole
    // case-variant space — separate tests for "lowercase", "uppercase",
    // etc. would just re-cover individual rows from the loop. Plus a
    // pass-through test per field (proves unrecognized input flows to the
    // validator for the canonical error), plus the strict-helper test.

    #[test]
    fn apply_patch_normalizes_consent_across_case_variants() {
        for (raw, expected) in [
            ("yes", "Yes"),
            ("YES", "Yes"),
            ("Yes", "Yes"),
            ("yEs", "Yes"),
            (" yes ", "Yes"),
            ("no", "No"),
            ("No", "No"),
            ("NO", "No"),
            ("  No  ", "No"),
        ] {
            let mut d = TicketDraft::new();
            let p = TicketDraftPatch {
                advanced_diagnostic_consent: Some(raw.into()),
                ..Default::default()
            };
            d.apply_patch(&p);
            assert_eq!(
                d.advanced_diagnostic_consent.as_deref(),
                Some(expected),
                "input {raw:?} should normalize to {expected:?}"
            );
        }
    }

    #[test]
    fn apply_patch_passes_through_unknown_consent_for_validator_to_reject() {
        // Unrecognized input stays as-is so the validator's standard error
        // message ("must be 'Yes' or 'No'") fires — instead of two different
        // errors from two different layers.
        let mut d = TicketDraft::new();
        let p = TicketDraftPatch {
            advanced_diagnostic_consent: Some("maybe".into()),
            ..Default::default()
        };
        d.apply_patch(&p);
        assert_eq!(d.advanced_diagnostic_consent.as_deref(), Some("maybe"));
    }

    #[test]
    fn apply_patch_normalizes_severity_to_lowercase() {
        for (raw, expected) in [
            ("CRITICAL", "critical"),
            ("Moderate", "moderate"),
            ("Minimal", "minimal"),
            ("HighestCriticalImpact", "highestcriticalimpact"),
        ] {
            let mut d = TicketDraft::new();
            let p = TicketDraftPatch {
                severity: Some(raw.into()),
                ..Default::default()
            };
            d.apply_patch(&p);
            assert_eq!(
                d.severity.as_deref(),
                Some(expected),
                "input {raw:?} should normalize to {expected:?}"
            );
        }
    }

    #[test]
    fn apply_patch_passes_through_unknown_severity_for_validator_to_reject() {
        let mut d = TicketDraft::new();
        let p = TicketDraftPatch {
            severity: Some("Catastrophic".into()),
            ..Default::default()
        };
        d.apply_patch(&p);
        assert_eq!(d.severity.as_deref(), Some("Catastrophic"));
    }

    #[test]
    fn normalize_consent_strict_returns_canonical_form() {
        assert_eq!(normalize_consent("yes").unwrap(), "Yes");
        assert_eq!(normalize_consent("YES").unwrap(), "Yes");
        assert_eq!(normalize_consent(" no ").unwrap(), "No");
        assert!(normalize_consent("maybe").is_err());
    }
}
