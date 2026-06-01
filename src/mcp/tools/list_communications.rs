//! `list_ticket_communications`: paged list of replies on a ticket.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::azure::support::communications::{list_communications, CommunicationPage};
use crate::bootstrap::AppState;
use crate::error::{AppError, AppResult};

#[derive(Debug, Deserialize, JsonSchema)]
pub struct Input {
    pub subscription_id: String,
    pub ticket_name: String,
    /// Page size (Azure max 10). Default 10.
    #[serde(default)]
    pub top: Option<u32>,
    /// Continuation link from a previous page.
    #[serde(default)]
    pub next_link: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct CommunicationSummary {
    pub communication_name: String,
    pub direction: Option<String>,
    pub communication_type: Option<String>,
    pub sender: Option<String>,
    pub subject: Option<String>,
    pub created_date: Option<String>,
    pub body_preview: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Output {
    pub communications: Vec<CommunicationSummary>,
    pub next_link: Option<String>,
}

pub async fn run(state: &AppState, input: Input) -> AppResult<Output> {
    if input.subscription_id.trim().is_empty() || input.ticket_name.trim().is_empty() {
        return Err(AppError::Validation(
            "subscription_id and ticket_name are required".into(),
        ));
    }
    let (arm, _chain) = super::arm_for(state)?;
    let CommunicationPage { items, next_link } = list_communications(
        &arm,
        &input.subscription_id,
        &input.ticket_name,
        input.top.or(Some(10)),
        input.next_link.as_deref(),
    )
    .await?;
    let summaries = items
        .into_iter()
        .map(|c| {
            let name = c
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let p = c
                .get("properties")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            let body = p
                .get("body")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let preview: String = body.chars().take(200).collect();
            CommunicationSummary {
                communication_name: name,
                direction: p
                    .get("communicationDirection")
                    .and_then(|v| v.as_str())
                    .map(String::from),
                communication_type: p
                    .get("communicationType")
                    .and_then(|v| v.as_str())
                    .map(String::from),
                sender: p.get("sender").and_then(|v| v.as_str()).map(String::from),
                subject: p.get("subject").and_then(|v| v.as_str()).map(String::from),
                created_date: p
                    .get("createdDate")
                    .and_then(|v| v.as_str())
                    .map(String::from),
                body_preview: if body.chars().count() > 200 {
                    format!("{preview}…")
                } else {
                    preview
                },
            }
        })
        .collect();
    Ok(Output {
        communications: summaries,
        next_link,
    })
}
