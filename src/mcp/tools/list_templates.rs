//! `list_ticket_templates`: enumerate saved contact templates.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::bootstrap::AppState;
use crate::error::AppResult;
use crate::workflow::templates::TemplateSummary;

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct Input {}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Output {
    pub templates: Vec<TemplateSummary>,
    pub message: String,
}

pub async fn run(state: &AppState, _input: Input) -> AppResult<Output> {
    let templates = state.templates.list();
    let message = match templates.len() {
        0 => "No saved templates. After your first successful ticket, contact info is auto-saved as `default`. Use `save_ticket_template` to capture additional named templates.".into(),
        1 => format!("1 template available: `{}`.", templates[0].name),
        n => format!("{n} templates available: {}.",
                     templates.iter().map(|t| format!("`{}`", t.name)).collect::<Vec<_>>().join(", ")),
    };
    Ok(Output { templates, message })
}
