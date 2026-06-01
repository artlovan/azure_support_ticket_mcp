use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::azure::identity::{self, names_from_upn, SignedInUser};
use crate::bootstrap::locale::{self, sanitize_contact_name, split_display_name, LocaleHints};
use crate::bootstrap::AppState;
use crate::error::AppResult;
use crate::workflow::draft::{TicketDraft, TicketDraftPatch};
use crate::workflow::templates::DEFAULT_TEMPLATE_NAME;

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct Input {
    /// Optional initial patch (e.g. tenant/subscription from list_tenants).
    #[serde(default, flatten)]
    pub initial: TicketDraftPatch,
    /// Named template to apply before autofill. If unset and
    /// use_default_template=true, `default` is auto-applied if it exists.
    #[serde(default)]
    pub template_name: Option<String>,
    /// Auto-apply `default` template when no template_name given. Default true.
    #[serde(default = "default_true")]
    pub use_default_template: bool,
    /// Auto-fill email + first/last name from signed-in identity. Default true.
    #[serde(default = "default_true")]
    pub auto_fill_contact: bool,
    /// Auto-fill country/language/timezone from OS locale. Default true.
    #[serde(default = "default_true")]
    pub auto_fill_locale: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Output {
    pub draft_id: String,
    pub draft_hash: String,
    pub review_token: String,
    pub message: String,
    /// Name of the template that was applied (if any).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub applied_template: Option<String>,
    /// Populated when `auto_fill_contact` succeeded. Lets the client tell the
    /// user "I used your sign-in email <X> — want to change it?" instead of
    /// blank-asking.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prefilled_from_identity: Option<SignedInUser>,
    /// Populated when `auto_fill_locale` produced any guesses.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prefilled_from_locale: Option<LocaleHints>,
    /// Fields the autofill actually wrote on the draft (so the client knows
    /// what *not* to re-ask for).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub prefilled_fields: Vec<String>,
}

pub async fn run(state: &AppState, input: Input) -> AppResult<Output> {
    let mut draft = TicketDraft::new();
    draft.apply_patch(&input.initial);

    let mut applied_template: Option<String> = None;
    let mut prefilled_id: Option<SignedInUser> = None;
    let mut prefilled_locale: Option<LocaleHints> = None;
    let mut prefilled_fields: Vec<String> = Vec::new();

    // Templates: explicit name wins; else fall back to `default` if it exists.
    let template_to_load = input.template_name.clone().or_else(|| {
        input
            .use_default_template
            .then(|| DEFAULT_TEMPLATE_NAME.to_string())
    });
    if let Some(name) = template_to_load {
        match state.templates.load(&name) {
            Ok(Some(t)) => {
                let filled = t.apply_fill_empty(&mut draft);
                if !filled.is_empty() {
                    prefilled_fields.extend(filled);
                }
                applied_template = Some(t.name);
            }
            Ok(None) => {
                // Explicit name that doesn't exist is an error; missing
                // default is fine (first-time users).
                if input.template_name.is_some() {
                    return Err(crate::error::AppError::NotFound(format!(
                        "template `{name}` not found. Call list_ticket_templates to see saved templates."
                    )));
                }
            }
            Err(e) => return Err(e),
        }
    }

    if input.auto_fill_contact {
        if let Ok((_client, chain)) = super::arm_for(state) {
            if let Ok(id) = identity::discover(chain.as_ref()).await {
                if let Some(upn) = id.user_principal_name.clone() {
                    if draft.contact_details.primary_email_address.is_none() {
                        draft.contact_details.primary_email_address = Some(upn.clone());
                        prefilled_fields.push("contact_details.primary_email_address".into());
                    }
                    // Resolve first/last in priority order:
                    //  1. JWT given_name + family_name
                    //  2. split JWT name claim ("Maria del Carmen Garcia" → first="Maria", last="del Carmen Garcia")
                    //  3. split UPN local part on . _ -
                    // Then sanitize for Azure Support API constraints
                    // (ASCII letters / space / hyphen / apostrophe only;
                    // accented chars cause API 400). Only commit BOTH when
                    // both survive sanitization — half-filled contact is
                    // worse UX than asking the user.
                    let raw = id
                        .given_name
                        .clone()
                        .zip(id.family_name.clone())
                        .or_else(|| id.display_name.as_deref().and_then(split_display_name))
                        .or_else(|| names_from_upn(&upn));
                    let sanitized = raw.and_then(|(f, l)| {
                        let f = sanitize_contact_name(&f)?;
                        let l = sanitize_contact_name(&l)?;
                        Some((f, l))
                    });
                    if let Some((f, l)) = sanitized {
                        if draft.contact_details.first_name.is_none()
                            && draft.contact_details.last_name.is_none()
                        {
                            draft.contact_details.first_name = Some(f);
                            draft.contact_details.last_name = Some(l);
                            prefilled_fields.push("contact_details.first_name".into());
                            prefilled_fields.push("contact_details.last_name".into());
                        }
                    }
                    if draft.tenant_id.is_none() {
                        if let Some(tid) = id.tenant_id.clone() {
                            draft.tenant_id = Some(tid);
                            prefilled_fields.push("tenant_id".into());
                        }
                    }
                }
                prefilled_id = Some(id);
            }
        }
    }

    if input.auto_fill_locale {
        let hints = locale::detect();
        if let Some(c) = &hints.country {
            if draft.contact_details.country.is_none() {
                draft.contact_details.country = Some(c.clone());
                prefilled_fields.push("contact_details.country".into());
            }
        }
        if let Some(l) = &hints.preferred_support_language {
            if draft.contact_details.preferred_support_language.is_none() {
                draft.contact_details.preferred_support_language = Some(l.clone());
                prefilled_fields.push("contact_details.preferred_support_language".into());
            }
        }
        if let Some(tz) = &hints.preferred_time_zone {
            if draft.contact_details.preferred_time_zone.is_none() {
                draft.contact_details.preferred_time_zone = Some(tz.clone());
                prefilled_fields.push("contact_details.preferred_time_zone".into());
            }
        }
        // Default contact method to email if not set and we have one.
        if draft.contact_details.preferred_contact_method.is_none()
            && draft.contact_details.primary_email_address.is_some()
        {
            draft.contact_details.preferred_contact_method = Some("email".into());
            prefilled_fields.push("contact_details.preferred_contact_method".into());
        }
        if hints != LocaleHints::default() {
            prefilled_locale = Some(hints);
        }
    }

    super::tenant_lookup::backfill_tenant(state, &mut draft).await;
    state.drafts.put(draft.clone()).await?;
    let issued = state.review_tokens.issue(&draft);

    let message = if prefilled_fields.is_empty() {
        "Draft created. Use build_ticket_draft to fill remaining fields, then create_support_ticket to submit.".into()
    } else if let Some(tmpl) = &applied_template {
        format!(
            "Draft created from template `{tmpl}` + autofill ({} fields). Confirm with the user before submitting (they can override via build_ticket_draft).",
            prefilled_fields.len()
        )
    } else {
        format!(
            "Draft created with auto-filled fields ({}). Confirm with the user before submitting (they can override via build_ticket_draft).",
            prefilled_fields.join(", ")
        )
    };

    Ok(Output {
        draft_id: draft.draft_id,
        draft_hash: issued.draft_hash,
        review_token: issued.review_token,
        message,
        applied_template,
        prefilled_from_identity: prefilled_id,
        prefilled_from_locale: prefilled_locale,
        prefilled_fields,
    })
}
