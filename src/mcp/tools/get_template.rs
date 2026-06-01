//! `get_ticket_template`: full contents of one template.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::bootstrap::AppState;
use crate::error::{AppError, AppResult};
use crate::workflow::templates::TicketTemplate;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct Input {
    pub name: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Output {
    pub template: TicketTemplate,
}

pub async fn run(state: &AppState, input: Input) -> AppResult<Output> {
    let template = state
        .templates
        .load(&input.name)?
        .ok_or_else(|| AppError::NotFound(format!("template `{}`", input.name)))?;
    Ok(Output { template })
}
