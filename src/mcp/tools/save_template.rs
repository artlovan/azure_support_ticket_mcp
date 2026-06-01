//! `save_ticket_template`: persist a named template from either an existing
//! draft or an explicit contact block. Overwrites if the name already exists.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::bootstrap::AppState;
use crate::error::{AppError, AppResult};
use crate::workflow::draft::ContactDetails;
use crate::workflow::templates::TicketTemplate;
use time::OffsetDateTime;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct Input {
    /// Template name. 1-64 chars of `[A-Za-z0-9_-]`.
    pub name: String,
    /// Optional human description.
    #[serde(default)]
    pub description: Option<String>,
    /// Capture from the contact details of this draft (mutually exclusive
    /// with `contact_details`).
    #[serde(default)]
    pub from_draft_id: Option<String>,
    /// Inline contact details (mutually exclusive with `from_draft_id`).
    #[serde(default)]
    pub contact_details: Option<ContactDetails>,
    /// Optional preferences to persist alongside contact info.
    #[serde(default)]
    pub advanced_diagnostic_consent: Option<String>,
    #[serde(default)]
    pub tenant_id: Option<String>,
    #[serde(default)]
    pub support_plan_id: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Output {
    pub saved_path: String,
    pub template: TicketTemplate,
    pub message: String,
}

pub async fn run(state: &AppState, input: Input) -> AppResult<Output> {
    if input.from_draft_id.is_some() && input.contact_details.is_some() {
        return Err(AppError::Validation(
            "supply either `from_draft_id` or `contact_details`, not both".into(),
        ));
    }

    let mut template = if let Some(draft_id) = &input.from_draft_id {
        let draft = state.drafts.get(draft_id).await?;
        TicketTemplate::from_draft(&input.name, &draft)
    } else if let Some(c) = input.contact_details {
        TicketTemplate {
            name: input.name.clone(),
            description: None,
            contact_details: c,
            advanced_diagnostic_consent: None,
            tenant_id: None,
            support_plan_id: None,
            updated_at: OffsetDateTime::now_utc().unix_timestamp(),
        }
    } else {
        return Err(AppError::Validation(
            "must supply `from_draft_id` or `contact_details`".into(),
        ));
    };

    // Caller-supplied overrides win over the captured/inline values.
    if input.description.is_some() {
        template.description = input.description;
    }
    if input.advanced_diagnostic_consent.is_some() {
        template.advanced_diagnostic_consent = input.advanced_diagnostic_consent;
    }
    if input.tenant_id.is_some() {
        template.tenant_id = input.tenant_id;
    }
    if input.support_plan_id.is_some() {
        template.support_plan_id = input.support_plan_id;
    }
    template.updated_at = OffsetDateTime::now_utc().unix_timestamp();
    template.name = input.name.clone();

    state.templates.save(&template)?;
    let path = state
        .config
        .app_dir()
        .join("templates")
        .join(format!("{}.json", input.name));

    Ok(Output {
        saved_path: path.display().to_string(),
        message: format!(
            "Saved template `{}`. Use it next time via start_support_ticket_flow with template_name=`{}`.",
            input.name, input.name
        ),
        template,
    })
}
