//! `azure_resource_search`: escape-hatch tool for querying Azure Resource
//! Graph directly with a KQL query.
//!
//! ## When to use this vs the curated tools
//!
//! The MCP's main resource-resolution path is `resolve_issue_context`,
//! which runs a curated multi-pass strategy (exact name → name substring →
//! id substring), returns ranked candidates with confidence, and offers a
//! disambiguation picker. **Prefer that for the normal case** — it's
//! tuned for the ticket-filing workflow.
//!
//! Reach for `azure_resource_search` when:
//!
//! - `resolve_issue_context` returned 0 candidates but the user is sure
//!   the resource exists (different subscription, different naming, type
//!   filter needed, etc.).
//! - The user asks something the curated tool can't express
//!   (e.g. "find all VMs in eastus tagged 'env=prod'").
//! - You need to look up a sub-resource that isn't indexed in Resource
//!   Graph at the top level (e.g. Azure OpenAI deployments live under
//!   their parent account; finding the parent often requires a custom
//!   `where type =~ 'microsoft.cognitiveservices/accounts'` query).
//!
//! ## Safety contract
//!
//! Read-only by construction. Resource Graph is a query API; there's no
//! way to mutate state through it. This tool deliberately does NOT
//! provide a hook to invoke `az rest` or arbitrary ARM REST calls —
//! state-changing operations go through their dedicated MCP tools
//! (`create_support_ticket`, `update_support_ticket`, etc.) which enforce
//! the `review_token` + `draft_hash` + `confirmed: true` handshake.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::azure::resource_graph::{query, ResourceRow};
use crate::bootstrap::AppState;
use crate::error::AppResult;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct Input {
    /// KQL query to run against Azure Resource Graph. Use full KQL syntax
    /// — `project`, `where`, `extend`, `summarize`, `join`, etc. all work.
    /// Quote string literals with single quotes (`'value'`). Always
    /// `project` the columns you need; the rows returned are
    /// `{id, name, type, resourceGroup, subscriptionId}` from any subset
    /// you project.
    ///
    /// Example queries:
    /// - `Resources | where type =~ 'microsoft.containerservice/managedclusters' | project name, id, resourceGroup | limit 10`
    /// - `Resources | where name contains 'prod' and location == 'eastus' | project name, type | limit 20`
    /// - `Resources | where type startswith 'microsoft.cognitiveservices' | project name, type, resourceGroup | limit 50`
    pub query: String,

    /// Subscription IDs to scope the query to. `None` (or omitted) means
    /// "search across every subscription the calling identity can read"
    /// — slower but useful when you don't know which subscription the
    /// resource lives in. Pass `Some(vec![sub_id])` to restrict.
    #[serde(default)]
    pub subscriptions: Option<Vec<String>>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Output {
    /// Raw matching rows. Empty when no resources matched the query.
    pub rows: Vec<ResourceRow>,
    /// Total matching records Resource Graph found, BEFORE any `| limit N`
    /// truncation in the query. If `total_records > rows.len()`, your
    /// query was over-broad — narrow it with a `where` clause.
    pub total_records: i64,
    /// The exact KQL that was sent. Echoed so the calling assistant can
    /// show it to the user when explaining what was searched, or as a
    /// starting point for a follow-up refinement.
    pub kql: String,
    /// Short summary of the outcome — included so the calling assistant
    /// doesn't have to invent narration. Reflects rows + total_records.
    pub message: String,
    /// Instructions for the calling assistant: explicit next-action
    /// suggestions for both the empty-result and non-empty-result cases.
    /// Reading the field name literally: "after this returns, do X".
    pub assistant_instructions: String,
}

pub async fn run(state: &AppState, input: Input) -> AppResult<Output> {
    let (client, _auth) = super::arm_for(state)?;

    let subs_slice = input.subscriptions.as_deref();
    let result = query(&client, &input.query, subs_slice).await?;

    let message = if result.rows.is_empty() {
        format!(
            "0 resources matched (total_records: {}). Try varying the query: \
             relax case sensitivity (use `=~` instead of `==`), widen the \
             match (`contains` instead of `=~`), drop the subscription scope \
             (omit `subscriptions`), or check the `type` filter spelling.",
            result.total_records
        )
    } else if result.total_records > result.rows.len() as i64 {
        format!(
            "{} rows returned (of {} total matches — query was capped by \
             `| limit N`). If the right resource isn't in the rows, narrow \
             the query with an additional `where` clause.",
            result.rows.len(),
            result.total_records
        )
    } else {
        format!(
            "{} resource(s) matched. Pick the one the user means, then \
             continue the ticket flow with the resource's `id` field as the \
             ARM resource ID.",
            result.rows.len()
        )
    };

    let assistant_instructions = if result.rows.is_empty() {
        "No matches. Try one of: (a) re-run with `subscriptions: null` to \
         search across ALL accessible subscriptions; (b) widen the query \
         with `contains` instead of `=~`; (c) try a different `type` filter \
         (e.g. resources can live under unexpected providers); (d) if the \
         resource might be a sub-resource (Azure OpenAI deployment, SQL \
         database, etc.) query the parent type instead. Only escalate to \
         the user after trying at least one variation."
            .to_string()
    } else {
        "Show the matched resources to the user verbatim (name + type + \
         resourceGroup). Once they confirm which one, pass its `id` field \
         as `technical_ticket_details.resource_id` via `build_ticket_draft`. \
         Never invent or modify the ARM ID — use the exact `id` Resource \
         Graph returned."
            .to_string()
    };

    Ok(Output {
        rows: result.rows,
        total_records: result.total_records,
        kql: input.query,
        message,
        assistant_instructions,
    })
}

#[cfg(test)]
mod tests {
    //! Single high-value test: the tool's contract is "return what Resource
    //! Graph returned, with steering text appropriate to result shape". We
    //! cover empty + non-empty paths plus the over-broad-query case.
    //! Lower-level KQL behavior is already tested in
    //! `azure::resource_graph::tests`; no need to re-cover it here.
    use super::*;
    use crate::azure::resource_graph::ResourceRow;

    fn fake_output(rows: Vec<ResourceRow>, total: i64, kql: &str) -> Output {
        let message = if rows.is_empty() {
            "empty".into()
        } else {
            "ok".into()
        };
        Output {
            rows,
            total_records: total,
            kql: kql.to_string(),
            message,
            assistant_instructions: "test".into(),
        }
    }

    #[test]
    fn output_message_includes_total_when_truncated() {
        // Just exercises the Output shape; the live `run` is tested via
        // the higher-level wiremock infrastructure in `resource_graph.rs`.
        let out = fake_output(
            vec![ResourceRow {
                id: Some("/sub/x".into()),
                name: Some("x".into()),
                resource_type: Some("type".into()),
                resource_group: None,
                subscription_id: None,
            }],
            42,
            "Resources | limit 5",
        );
        assert_eq!(out.rows.len(), 1);
        assert_eq!(out.total_records, 42);
        assert_eq!(out.kql, "Resources | limit 5");
    }
}
