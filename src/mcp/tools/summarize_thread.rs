//! `summarize_ticket_thread`: local-only summary of a ticket + its replies.
//! Never invokes an LLM.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::azure::support::{
    communications::{list_communications, CommunicationPage},
    tickets::get_ticket,
};
use crate::bootstrap::AppState;
use crate::error::{AppError, AppResult};
use crate::workflow::summarize::{summarize, ThreadSummary};

#[derive(Debug, Deserialize, JsonSchema)]
pub struct Input {
    pub subscription_id: String,
    pub ticket_name: String,
    /// Max communications to pull. Default 25 (caps at 50).
    #[serde(default)]
    pub max_communications: Option<u32>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Output {
    pub summary: ThreadSummary,
}

pub async fn run(state: &AppState, input: Input) -> AppResult<Output> {
    if input.subscription_id.trim().is_empty() || input.ticket_name.trim().is_empty() {
        return Err(AppError::Validation(
            "subscription_id and ticket_name are required".into(),
        ));
    }
    let cap = input.max_communications.unwrap_or(25).min(50);
    let (arm, _chain) = super::arm_for(state)?;
    let ticket = get_ticket(&arm, &input.subscription_id, &input.ticket_name).await?;

    // Pull up to `cap` items, following nextLink as needed (10/page max).
    let mut collected: Vec<serde_json::Value> = Vec::new();
    let mut next: Option<String> = None;
    loop {
        let CommunicationPage { items, next_link } = list_communications(
            &arm,
            &input.subscription_id,
            &input.ticket_name,
            Some(10),
            next.as_deref(),
        )
        .await?;
        collected.extend(items);
        if collected.len() as u32 >= cap {
            collected.truncate(cap as usize);
            break;
        }
        match next_link {
            Some(l) => next = Some(l),
            None => break,
        }
    }
    let summary = summarize(&ticket, &collected);
    Ok(Output { summary })
}
