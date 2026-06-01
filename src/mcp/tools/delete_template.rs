//! `delete_ticket_template`: remove a named template.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::bootstrap::AppState;
use crate::error::AppResult;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct Input {
    pub name: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Output {
    pub deleted: bool,
    pub message: String,
}

pub async fn run(state: &AppState, input: Input) -> AppResult<Output> {
    let deleted = state.templates.delete(&input.name)?;
    let message = if deleted {
        format!("Deleted template `{}`.", input.name)
    } else {
        format!("No template named `{}` to delete.", input.name)
    };
    Ok(Output { deleted, message })
}
