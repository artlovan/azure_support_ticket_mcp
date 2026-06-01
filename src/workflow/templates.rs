//! Reusable contact-info templates.
//!
//! Users open multiple tickets with the same contact details. We persist
//! those stable fields to `~/.azure-support-ticket-mcp/templates/<name>.json`
//! so subsequent `start_support_ticket_flow` calls don't re-ask the same
//! questions.
//!
//! Precedence for filling a new draft (high → low):
//!   1. Caller-supplied initial patch.
//!   2. Named template (or `default` if no name was supplied and a default exists).
//!   3. Identity autofill (from token claims).
//!   4. Locale autofill (from OS).
//!
//! Each step only fills fields that are still empty.
//!
//! After a successful `create_support_ticket`, the contact slice of the
//! submitted draft is written back to `default.json` (best-effort,
//! non-fatal). Users can override with the `save_ticket_template` tool to
//! create named templates (e.g. "personal", "team-x").

use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use tracing::warn;

use crate::error::{AppError, AppResult};
use crate::workflow::draft::{ContactDetails, TicketDraft};

/// Persisted on disk. Only carries fields that are typically *stable*
/// across tickets — issue-specific things (title, severity, service,
/// classification, resource_id) are intentionally excluded.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct TicketTemplate {
    /// Identifier used in filenames and `template_name` arguments.
    pub name: String,
    /// Short human description (optional).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub contact_details: ContactDetails,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub advanced_diagnostic_consent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub support_plan_id: Option<String>,
    /// Unix epoch seconds when this template was last written.
    pub updated_at: i64,
}

impl TicketTemplate {
    pub fn from_draft(name: &str, draft: &TicketDraft) -> Self {
        Self {
            name: name.to_string(),
            description: None,
            contact_details: draft.contact_details.clone(),
            advanced_diagnostic_consent: draft.advanced_diagnostic_consent.clone(),
            tenant_id: draft.tenant_id.clone(),
            support_plan_id: draft.support_plan_id.clone(),
            updated_at: OffsetDateTime::now_utc().unix_timestamp(),
        }
    }

    /// Apply non-null fields onto `draft` *only where draft slots are empty*.
    /// Returns the list of fields actually written.
    pub fn apply_fill_empty(&self, draft: &mut TicketDraft) -> Vec<String> {
        let mut filled = Vec::new();
        let c = &self.contact_details;
        macro_rules! fill {
            ($field:ident) => {
                if draft.contact_details.$field.is_none() && c.$field.is_some() {
                    draft.contact_details.$field = c.$field.clone();
                    filled.push(concat!("contact_details.", stringify!($field)).to_string());
                }
            };
        }
        fill!(first_name);
        fill!(last_name);
        fill!(country);
        fill!(preferred_contact_method);
        fill!(preferred_support_language);
        fill!(preferred_time_zone);
        fill!(primary_email_address);
        fill!(phone_number);
        if draft.contact_details.additional_email_addresses.is_empty()
            && !c.additional_email_addresses.is_empty()
        {
            draft.contact_details.additional_email_addresses = c.additional_email_addresses.clone();
            filled.push("contact_details.additional_email_addresses".into());
        }
        if draft.advanced_diagnostic_consent.is_none() && self.advanced_diagnostic_consent.is_some()
        {
            draft.advanced_diagnostic_consent = self.advanced_diagnostic_consent.clone();
            filled.push("advanced_diagnostic_consent".into());
        }
        if draft.tenant_id.is_none() && self.tenant_id.is_some() {
            draft.tenant_id = self.tenant_id.clone();
            filled.push("tenant_id".into());
        }
        if draft.support_plan_id.is_none() && self.support_plan_id.is_some() {
            draft.support_plan_id = self.support_plan_id.clone();
            filled.push("support_plan_id".into());
        }
        filled
    }
}

/// Lightweight info for listings.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct TemplateSummary {
    pub name: String,
    pub description: Option<String>,
    pub primary_email_address: Option<String>,
    pub country: Option<String>,
    pub updated_at: i64,
}

/// On-disk template store. Cheap to clone (`Arc` inside is unnecessary —
/// it's just a path; all I/O is per-call).
#[derive(Clone)]
pub struct TemplateStore {
    dir: PathBuf,
}

pub const DEFAULT_TEMPLATE_NAME: &str = "default";

impl TemplateStore {
    pub fn new(app_dir: &Path) -> Self {
        Self {
            dir: app_dir.join("templates"),
        }
    }

    fn ensure_dir(&self) -> AppResult<()> {
        if !self.dir.exists() {
            std::fs::create_dir_all(&self.dir).map_err(|e| AppError::io(&self.dir, e))?;
        }
        Ok(())
    }

    fn validate_name(name: &str) -> AppResult<()> {
        if name.is_empty()
            || name.len() > 64
            || !name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err(AppError::Validation(format!(
                "invalid template name `{name}`: use 1–64 chars of [A-Za-z0-9_-]"
            )));
        }
        Ok(())
    }

    fn path_for(&self, name: &str) -> PathBuf {
        self.dir.join(format!("{name}.json"))
    }

    pub fn list(&self) -> Vec<TemplateSummary> {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for e in entries.flatten() {
            let p = e.path();
            if p.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            let Ok(bytes) = std::fs::read(&p) else {
                continue;
            };
            let Ok(t) = serde_json::from_slice::<TicketTemplate>(&bytes) else {
                continue;
            };
            out.push(TemplateSummary {
                name: t.name,
                description: t.description,
                primary_email_address: t.contact_details.primary_email_address.clone(),
                country: t.contact_details.country.clone(),
                updated_at: t.updated_at,
            });
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    pub fn load(&self, name: &str) -> AppResult<Option<TicketTemplate>> {
        Self::validate_name(name)?;
        let p = self.path_for(name);
        if !p.exists() {
            return Ok(None);
        }
        let bytes = std::fs::read(&p).map_err(|e| AppError::io(&p, e))?;
        let t: TicketTemplate = serde_json::from_slice(&bytes)
            .map_err(|e| AppError::Validation(format!("template `{name}` is malformed: {e}")))?;
        Ok(Some(t))
    }

    pub fn save(&self, template: &TicketTemplate) -> AppResult<()> {
        Self::validate_name(&template.name)?;
        self.ensure_dir()?;
        let p = self.path_for(&template.name);
        let bytes = serde_json::to_vec_pretty(template).map_err(|e| {
            AppError::Validation(format!("template `{}` serialize: {e}", template.name))
        })?;
        // Atomic-ish write: temp + rename.
        let tmp = self.dir.join(format!(".{}.tmp", template.name));
        std::fs::write(&tmp, &bytes).map_err(|e| AppError::io(&tmp, e))?;
        std::fs::rename(&tmp, &p).map_err(|e| AppError::io(&p, e))?;
        Ok(())
    }

    pub fn delete(&self, name: &str) -> AppResult<bool> {
        Self::validate_name(name)?;
        let p = self.path_for(name);
        if !p.exists() {
            return Ok(false);
        }
        std::fs::remove_file(&p).map_err(|e| AppError::io(&p, e))?;
        Ok(true)
    }

    /// Best-effort save: logs on failure, never returns Err. Used by
    /// `create_support_ticket` post-success so a template-write hiccup never
    /// blocks reporting a successfully-created ticket.
    pub fn save_best_effort(&self, template: &TicketTemplate) {
        if let Err(e) = self.save(template) {
            warn!(error = %e, template = %template.name, "template save failed (non-fatal)");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn sample_template(name: &str) -> TicketTemplate {
        TicketTemplate {
            name: name.into(),
            description: Some("for unit test".into()),
            contact_details: ContactDetails {
                first_name: Some("Alice".into()),
                last_name: Some("Example".into()),
                country: Some("USA".into()),
                preferred_contact_method: Some("email".into()),
                preferred_support_language: Some("en-us".into()),
                preferred_time_zone: Some("Pacific Standard Time".into()),
                primary_email_address: Some("alice@contoso.com".into()),
                phone_number: None,
                additional_email_addresses: vec!["alice.alt@contoso.com".into()],
            },
            advanced_diagnostic_consent: Some("Yes".into()),
            tenant_id: None,
            support_plan_id: None,
            updated_at: 1_716_929_400,
        }
    }

    #[test]
    fn save_load_roundtrip() {
        let dir = tempdir().unwrap();
        let store = TemplateStore::new(dir.path());
        let t = sample_template("default");
        store.save(&t).unwrap();
        let loaded = store.load("default").unwrap().unwrap();
        assert_eq!(
            loaded.contact_details.primary_email_address.as_deref(),
            Some("alice@contoso.com")
        );
        assert_eq!(loaded.contact_details.additional_email_addresses.len(), 1);
    }

    #[test]
    fn list_returns_summaries_sorted() {
        let dir = tempdir().unwrap();
        let store = TemplateStore::new(dir.path());
        store.save(&sample_template("zeta")).unwrap();
        store.save(&sample_template("alpha")).unwrap();
        let list = store.list();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].name, "alpha");
        assert_eq!(list[1].name, "zeta");
    }

    #[test]
    fn delete_returns_false_if_missing() {
        let dir = tempdir().unwrap();
        let store = TemplateStore::new(dir.path());
        assert!(!store.delete("nope").unwrap());
        store.save(&sample_template("zap")).unwrap();
        assert!(store.delete("zap").unwrap());
        assert!(store.load("zap").unwrap().is_none());
    }

    #[test]
    fn rejects_bad_names() {
        let dir = tempdir().unwrap();
        let store = TemplateStore::new(dir.path());
        assert!(store.load("../etc/passwd").is_err());
        assert!(store.load("a b").is_err());
        assert!(store.load("").is_err());
        assert!(store.delete("with/slash").is_err());
    }

    #[test]
    fn malformed_load_errors_cleanly() {
        let dir = tempdir().unwrap();
        let store = TemplateStore::new(dir.path());
        store.ensure_dir().unwrap();
        std::fs::write(dir.path().join("templates/broken.json"), "{not json").unwrap();
        let err = store.load("broken").unwrap_err();
        assert!(format!("{err}").contains("malformed"));
    }

    #[test]
    fn apply_fill_empty_does_not_overwrite() {
        let dir = tempdir().unwrap();
        let _store = TemplateStore::new(dir.path());
        let t = sample_template("default");
        let mut draft = TicketDraft::new();
        draft.contact_details.first_name = Some("Override".into());
        let filled = t.apply_fill_empty(&mut draft);
        // first_name was set → not in filled
        assert!(!filled.iter().any(|f| f == "contact_details.first_name"));
        assert_eq!(
            draft.contact_details.first_name.as_deref(),
            Some("Override")
        );
        // empty fields should be filled
        assert_eq!(draft.contact_details.last_name.as_deref(), Some("Example"));
        assert!(filled.iter().any(|f| f == "contact_details.last_name"));
        assert!(filled.iter().any(|f| f == "advanced_diagnostic_consent"));
    }

    #[test]
    fn from_draft_captures_contact() {
        let mut d = TicketDraft::new();
        d.contact_details.primary_email_address = Some("bob@x.com".into());
        d.advanced_diagnostic_consent = Some("No".into());
        let t = TicketTemplate::from_draft("captured", &d);
        assert_eq!(t.name, "captured");
        assert_eq!(
            t.contact_details.primary_email_address.as_deref(),
            Some("bob@x.com")
        );
        assert_eq!(t.advanced_diagnostic_consent.as_deref(), Some("No"));
        assert!(t.updated_at > 0);
    }
}
