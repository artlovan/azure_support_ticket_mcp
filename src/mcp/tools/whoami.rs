//! `whoami`: surface the signed-in user's identity (UPN, display name, oid,
//! tenant). Helps Copilot avoid asking the user for info we can derive from
//! the token they already authenticated with.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::azure::identity::{self, SignedInUser};
use crate::bootstrap::AppState;
use crate::error::AppResult;

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct Input {}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Output {
    pub identity: SignedInUser,
    pub message: String,
}

pub async fn run(state: &AppState, _input: Input) -> AppResult<Output> {
    let (_client, chain) = super::arm_for(state)?;
    let id = identity::discover(chain.as_ref()).await?;
    let message = if id.is_service_principal {
        "Signed in as a service principal; no user email is available from the token. Ask the user for a contact email before creating tickets.".into()
    } else if let Some(upn) = &id.user_principal_name {
        format!(
            "Signed in as {upn}. Use this as the default primary_email_address unless the user overrides it.",
        )
    } else {
        "Could not determine UPN from token claims. Ask the user for their contact email.".into()
    };
    Ok(Output {
        identity: id,
        message,
    })
}
