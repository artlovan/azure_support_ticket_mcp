//! `init_ticket_template`: bootstrap a template file on disk, seeded with
//! identity (email, names, tenant) and OS locale (country, language,
//! timezone) so the user can confirm/edit values once instead of starting
//! from a blank file.
//!
//! Refuses to overwrite an existing template unless `overwrite: true`. The
//! `default` template is the one auto-loaded by `start_support_ticket_flow`.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::azure::identity::{self, names_from_upn, SignedInUser};
use crate::bootstrap::locale::{self, sanitize_contact_name, split_display_name, LocaleHints};
use crate::bootstrap::AppState;
use crate::error::{AppError, AppResult};
use crate::workflow::draft::ContactDetails;
use crate::workflow::templates::{TicketTemplate, DEFAULT_TEMPLATE_NAME};

#[derive(Debug, Deserialize, JsonSchema)]
pub struct Input {
    /// Template name. Default `default`. 1-64 chars of [A-Za-z0-9_-].
    #[serde(default = "default_name")]
    pub name: String,
    /// Optional description (e.g. "team-aks").
    #[serde(default)]
    pub description: Option<String>,
    /// Replace existing template. Default false.
    #[serde(default)]
    pub overwrite: bool,
    /// Seed email/names/tenant from identity. Default true.
    #[serde(default = "default_true")]
    pub seed_from_identity: bool,
    /// Seed country/language/timezone from OS locale. Default true.
    #[serde(default = "default_true")]
    pub seed_from_locale: bool,
}

fn default_name() -> String {
    DEFAULT_TEMPLATE_NAME.to_string()
}
fn default_true() -> bool {
    true
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Output {
    pub name: String,
    pub saved_path: String,
    /// `true` if a file already existed and was replaced; `false` for a
    /// fresh create.
    pub overwritten: bool,
    /// The template that was just written (so the client can show it).
    pub template: TicketTemplate,
    /// Fields that were auto-seeded from identity/locale. The client should
    /// confirm these with the user and offer to edit via
    /// `save_ticket_template` (or by editing the JSON file directly).
    pub seeded_fields: Vec<String>,
    /// Fields the user still needs to provide (phone, etc.) — empty if
    /// everything was seeded.
    pub blank_fields: Vec<String>,
    /// Echo back the identity hit (so the UI can say "I used your sign-in
    /// email …").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub identity: Option<SignedInUser>,
    /// Echo back the locale hits.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub locale: Option<LocaleHints>,
    pub message: String,
}

pub async fn run(state: &AppState, input: Input) -> AppResult<Output> {
    // Existence check (before any work).
    let existing = state.templates.load(&input.name)?;
    if existing.is_some() && !input.overwrite {
        return Err(AppError::Validation(format!(
            "template `{}` already exists. Pass overwrite=true to replace it, or use save_ticket_template to update individual fields.",
            input.name
        )));
    }

    let mut contact = ContactDetails::default();
    let mut tenant_id: Option<String> = None;
    let mut seeded: Vec<String> = Vec::new();
    let mut id_hit: Option<SignedInUser> = None;
    let mut locale_hit: Option<LocaleHints> = None;

    if input.seed_from_identity {
        if let Ok((_arm, chain)) = super::arm_for(state) {
            if let Ok(id) = identity::discover(chain.as_ref()).await {
                if let Some(upn) = id.user_principal_name.clone() {
                    contact.primary_email_address = Some(upn.clone());
                    seeded.push("contact_details.primary_email_address".into());

                    // First/last: prefer JWT given/family, fall back to split
                    // of display_name, then UPN local part. Only commit BOTH
                    // when both survive sanitization (Azure rejects accented
                    // chars; partial names are worse than blanks).
                    let raw = id
                        .given_name
                        .clone()
                        .zip(id.family_name.clone())
                        .or_else(|| id.display_name.as_deref().and_then(split_display_name))
                        .or_else(|| names_from_upn(&upn));
                    if let Some((f, l)) = raw.and_then(|(f, l)| {
                        Some((sanitize_contact_name(&f)?, sanitize_contact_name(&l)?))
                    }) {
                        contact.first_name = Some(f);
                        contact.last_name = Some(l);
                        seeded.push("contact_details.first_name".into());
                        seeded.push("contact_details.last_name".into());
                    }
                }
                if let Some(tid) = id.tenant_id.clone() {
                    tenant_id = Some(tid);
                    seeded.push("tenant_id".into());
                }
                id_hit = Some(id);
            }
        }
    }

    if input.seed_from_locale {
        let hints = locale::detect();
        if let Some(c) = &hints.country {
            contact.country = Some(c.clone());
            seeded.push("contact_details.country".into());
        }
        if let Some(l) = &hints.preferred_support_language {
            contact.preferred_support_language = Some(l.clone());
            seeded.push("contact_details.preferred_support_language".into());
        }
        if let Some(tz) = &hints.preferred_time_zone {
            contact.preferred_time_zone = Some(tz.clone());
            seeded.push("contact_details.preferred_time_zone".into());
        }
        if contact.primary_email_address.is_some() {
            contact.preferred_contact_method = Some("email".into());
            seeded.push("contact_details.preferred_contact_method".into());
        }
        if hints != LocaleHints::default() {
            locale_hit = Some(hints);
        }
    }

    // Compute what's still blank — useful guidance for the client.
    let mut blank: Vec<String> = Vec::new();
    macro_rules! check {
        ($field:ident) => {
            if contact.$field.is_none() {
                blank.push(concat!("contact_details.", stringify!($field)).to_string());
            }
        };
    }
    check!(first_name);
    check!(last_name);
    check!(country);
    check!(preferred_contact_method);
    check!(preferred_support_language);
    check!(preferred_time_zone);
    check!(primary_email_address);
    check!(phone_number);

    let template = TicketTemplate {
        name: input.name.clone(),
        description: input.description,
        contact_details: contact,
        advanced_diagnostic_consent: None,
        tenant_id,
        support_plan_id: None,
        updated_at: OffsetDateTime::now_utc().unix_timestamp(),
    };

    state.templates.save(&template)?;
    let saved_path = state
        .config
        .app_dir()
        .join("templates")
        .join(format!("{}.json", input.name));

    let overwritten = existing.is_some();
    let message = match (overwritten, seeded.len()) {
        (true, n) => format!(
            "Replaced template `{}` ({n} fields seeded). Edit further with save_ticket_template or by editing the JSON file directly.",
            input.name
        ),
        (false, 0) => format!(
            "Created empty template `{}` at {}. No identity/locale hints were available — fill it in via save_ticket_template or by editing the file.",
            input.name,
            saved_path.display()
        ),
        (false, n) => format!(
            "Created template `{}` ({n} fields seeded from identity/locale). Saved to {}.",
            input.name,
            saved_path.display()
        ),
    };

    Ok(Output {
        name: input.name,
        saved_path: saved_path.display().to_string(),
        overwritten,
        template,
        seeded_fields: seeded,
        blank_fields: blank,
        identity: id_hit,
        locale: locale_hit,
        message,
    })
}
