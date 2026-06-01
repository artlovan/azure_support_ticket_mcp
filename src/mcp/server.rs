//! rmcp stdio adapter. The `ToolsServer` here is a thin shell whose only
//! job is to expose tools to the MCP transport. Workflow logic
//! lives in the other modules and is invoked through this layer.

use std::sync::Arc;

use rmcp::{
    handler::server::{
        router::tool::ToolRouter,
        wrapper::{Json, Parameters},
    },
    model::{ServerCapabilities, ServerInfo},
    tool, tool_handler, tool_router,
    transport::io::stdio,
    ErrorData, ServerHandler, ServiceExt,
};

use crate::bootstrap::AppState;
use crate::error::AppResult;

use super::tools;

/// Entry point used by `main`. Wires the tool router to stdio and serves
/// until the client disconnects.
pub async fn serve_stdio(state: AppState) -> AppResult<()> {
    let server = ToolsServer::new(Arc::new(state));
    let service = server
        .serve(stdio())
        .await
        .map_err(|e| crate::error::AppError::Mcp(format!("mcp serve init: {e}")))?;
    service
        .waiting()
        .await
        .map_err(|e| crate::error::AppError::Mcp(format!("mcp serve loop: {e}")))?;
    Ok(())
}

/// MCP server handle. Cheap to clone (`Arc<AppState>` inside).
#[derive(Clone)]
pub struct ToolsServer {
    state: Arc<AppState>,
    #[allow(dead_code)] // populated by the `#[tool_router]` macro
    tool_router: ToolRouter<Self>,
}

impl ToolsServer {
    pub fn new(state: Arc<AppState>) -> Self {
        Self {
            state,
            tool_router: Self::tool_router(),
        }
    }

    pub fn state(&self) -> &AppState {
        &self.state
    }
}

#[tool_router]
impl ToolsServer {
    /// Report auth + cloud + seed + cache freshness.
    #[tool(
        name = "azure_auth_status",
        description = "Report Azure auth configuration, cloud, seed version, and cache freshness. Does not call Azure."
    )]
    async fn azure_auth_status(
        &self,
        Parameters(input): Parameters<tools::auth_status::Input>,
    ) -> Result<Json<tools::auth_status::Output>, ErrorData> {
        tools::auth_status::run(&self.state, input)
            .await
            .map(Json)
            .map_err(tools::to_mcp_error)
    }

    /// List tenants accessible to the signed-in identity.
    #[tool(
        name = "list_tenants",
        description = "List all Azure tenants accessible to the current credentials. Multi-tenant from day one."
    )]
    async fn list_tenants(
        &self,
        Parameters(input): Parameters<tools::list_tenants::Input>,
    ) -> Result<Json<tools::list_tenants::Output>, ErrorData> {
        tools::list_tenants::run(&self.state, input)
            .await
            .map(Json)
            .map_err(tools::to_mcp_error)
    }

    /// List subscriptions in the current/selected tenant.
    #[tool(
        name = "list_subscriptions",
        description = "List subscriptions accessible to the signed-in identity (optionally filtered to one tenant)."
    )]
    async fn list_subscriptions(
        &self,
        Parameters(input): Parameters<tools::list_subscriptions::Input>,
    ) -> Result<Json<tools::list_subscriptions::Output>, ErrorData> {
        tools::list_subscriptions::run(&self.state, input)
            .await
            .map(Json)
            .map_err(tools::to_mcp_error)
    }

    /// Ranked support services for a given resource type / keyword.
    #[tool(
        name = "list_relevant_support_services",
        description = "Return Azure support services ranked for the given resource type, keyword, or both. Seed-first; live fetch later."
    )]
    async fn list_relevant_support_services(
        &self,
        Parameters(input): Parameters<tools::list_services::Input>,
    ) -> Result<Json<tools::list_services::Output>, ErrorData> {
        tools::list_services::run(&self.state, input)
            .await
            .map(Json)
            .map_err(tools::to_mcp_error)
    }

    /// List problem classifications for a selected support service (lazy).
    #[tool(
        name = "list_problem_classifications",
        description = "List problem classifications for the given support service. Cached after first fetch."
    )]
    async fn list_problem_classifications(
        &self,
        Parameters(input): Parameters<tools::list_classifications::Input>,
    ) -> Result<Json<tools::list_classifications::Output>, ErrorData> {
        tools::list_classifications::run(&self.state, input)
            .await
            .map(Json)
            .map_err(tools::to_mcp_error)
    }

    /// Clear/refresh a slice of the local cache.
    #[tool(
        name = "refresh_support_cache",
        description = "Force-refresh part of the local cache (services or a specific service's classifications)."
    )]
    async fn refresh_support_cache(
        &self,
        Parameters(input): Parameters<tools::refresh_cache::Input>,
    ) -> Result<Json<tools::refresh_cache::Output>, ErrorData> {
        tools::refresh_cache::run(&self.state, input)
            .await
            .map(Json)
            .map_err(tools::to_mcp_error)
    }

    /// Lightweight environment check (without spawning the standalone subcommand).
    #[tool(
        name = "doctor",
        description = "Quick environment check: cache status, seed version, az CLI presence, ARM reachability."
    )]
    async fn doctor(
        &self,
        Parameters(_input): Parameters<tools::doctor::Input>,
    ) -> Result<Json<tools::doctor::Output>, ErrorData> {
        tools::doctor::run(&self.state)
            .await
            .map(Json)
            .map_err(tools::to_mcp_error)
    }

    /// Identity: who is currently signed in (decoded from ARM token claims).
    #[tool(
        name = "whoami",
        description = "Signed-in identity (UPN/email/display_name/tenant) from ARM token. Use to avoid blank-asking for contact info."
    )]
    async fn whoami(
        &self,
        Parameters(input): Parameters<tools::whoami::Input>,
    ) -> Result<Json<tools::whoami::Output>, ErrorData> {
        tools::whoami::run(&self.state, input)
            .await
            .map(Json)
            .map_err(tools::to_mcp_error)
    }

    /// Templates: list saved contact templates.
    #[tool(
        name = "list_ticket_templates",
        description = "List saved templates. Call FIRST in any ticket flow — if any exist, ask user whether to reuse before collecting contact info."
    )]
    async fn list_ticket_templates(
        &self,
        Parameters(input): Parameters<tools::list_templates::Input>,
    ) -> Result<Json<tools::list_templates::Output>, ErrorData> {
        tools::list_templates::run(&self.state, input)
            .await
            .map(Json)
            .map_err(tools::to_mcp_error)
    }

    /// Templates: full content of one template.
    #[tool(
        name = "get_ticket_template",
        description = "Return the full content of a saved ticket template by name."
    )]
    async fn get_ticket_template(
        &self,
        Parameters(input): Parameters<tools::get_template::Input>,
    ) -> Result<Json<tools::get_template::Output>, ErrorData> {
        tools::get_template::run(&self.state, input)
            .await
            .map(Json)
            .map_err(tools::to_mcp_error)
    }

    /// Templates: save a named template from a draft or inline contact info.
    #[tool(
        name = "init_ticket_template",
        description = "First-run scaffold: writes a template seeded from identity (email/names/tenant) + OS locale (country/language/timezone). Default name `default`. Set overwrite=true to replace."
    )]
    async fn init_ticket_template(
        &self,
        Parameters(input): Parameters<tools::init_template::Input>,
    ) -> Result<Json<tools::init_template::Output>, ErrorData> {
        tools::init_template::run(&self.state, input)
            .await
            .map(Json)
            .map_err(tools::to_mcp_error)
    }

    /// Templates: save a named template from a draft or inline contact info.
    #[tool(
        name = "save_ticket_template",
        description = "Save/update a named template. Provide either from_draft_id or inline contact_details."
    )]
    async fn save_ticket_template(
        &self,
        Parameters(input): Parameters<tools::save_template::Input>,
    ) -> Result<Json<tools::save_template::Output>, ErrorData> {
        tools::save_template::run(&self.state, input)
            .await
            .map(Json)
            .map_err(tools::to_mcp_error)
    }

    /// Templates: remove a named template.
    #[tool(
        name = "delete_ticket_template",
        description = "Delete a saved ticket template by name."
    )]
    async fn delete_ticket_template(
        &self,
        Parameters(input): Parameters<tools::delete_template::Input>,
    ) -> Result<Json<tools::delete_template::Output>, ErrorData> {
        tools::delete_template::run(&self.state, input)
            .await
            .map(Json)
            .map_err(tools::to_mcp_error)
    }

    /// Turn user input + resource id / portal URL into ranked candidates.
    #[tool(
        name = "resolve_issue_context",
        description = "Resolve the user's input (free text, resource id, portal URL) into structured context, ranked support-service candidates, AND — when the user typed a quoted resource name like \"contoso-b2c\" / 'prod-aks' / `gpt-4o-prod` without a full ARM ID or portal URL — ranked Azure resource candidates via Azure Resource Graph. The resource search runs a 3-pass query (exact name → name substring → id substring) so weird-naming cases like B2C directories (`oncontoso-b2c.onmicrosoft.com` when user typed `contoso-b2c`), DNS zones, KeyVault URIs etc. still surface. When `resource_disambiguation` is returned, render its `question` and `options` to the user VERBATIM — they include sentinels `None of these — the issue isn't tied to a specific resource` and `Other — let me paste a different ARM ID`, so the user is never stuck. NEVER skip this step with claims like \"X resource type isn't discoverable via Resource Graph\" — that is false; surface the actual empty result and let the user pick the `Other` sentinel."
    )]
    async fn resolve_issue_context(
        &self,
        Parameters(input): Parameters<tools::resolve_context::Input>,
    ) -> Result<Json<tools::resolve_context::Output>, ErrorData> {
        tools::resolve_context::run(&self.state, input)
            .await
            .map(Json)
            .map_err(tools::to_mcp_error)
    }

    /// Escape-hatch tool: arbitrary KQL against Resource Graph. Read-only.
    #[tool(
        name = "azure_resource_search",
        description = "ESCAPE-HATCH tool for querying Azure Resource Graph with an arbitrary KQL query. Read-only — Resource Graph cannot mutate state. PREFER `resolve_issue_context` for the normal resource-resolution flow; it's tuned for ticket-filing and returns a ready-made picker UX. Reach for THIS tool when (a) `resolve_issue_context` returned 0 candidates but the user is sure the resource exists (try different scope, type filter, or matching strategy here); (b) you need something the curated tool can't express (find all VMs in a region tagged X, etc.); (c) the resource is a sub-resource not indexed at the top level (Azure OpenAI deployments, SQL databases) — use this to find the parent. When the result is empty, the response includes explicit suggestions for varying the query — try at least one variation before escalating to the user. State-changing operations are NOT exposed through this tool; for those use `create_support_ticket` / `update_support_ticket` / etc. with their confirmation handshake."
    )]
    async fn azure_resource_search(
        &self,
        Parameters(input): Parameters<tools::azure_search::Input>,
    ) -> Result<Json<tools::azure_search::Output>, ErrorData> {
        tools::azure_search::run(&self.state, input)
            .await
            .map(Json)
            .map_err(tools::to_mcp_error)
    }

    /// Create a new draft (optionally pre-filled).
    #[tool(
        name = "start_support_ticket_flow",
        description = "Create a new ticket draft. Returns draft_id, review_token, and draft_hash."
    )]
    async fn start_support_ticket_flow(
        &self,
        Parameters(input): Parameters<tools::start_flow::Input>,
    ) -> Result<Json<tools::start_flow::Output>, ErrorData> {
        tools::start_flow::run(&self.state, input)
            .await
            .map(Json)
            .map_err(tools::to_mcp_error)
    }

    /// Patch an existing draft. Rotates review_token + hash.
    #[tool(
        name = "build_ticket_draft",
        description = "Update fields on an existing draft. Returns the full draft, validation status, and a fresh review_token + draft_hash. PREREQUISITE: if the user mentions any specific Azure resource by name, resource group, portal URL, error code, or service hint that has not yet been resolved, call `resolve_issue_context` with that text FIRST. Its output now includes ranked `resource_candidates` from Azure Resource Graph plus a `resource_disambiguation` picker (with `None of these` and `Other — paste ARM ID` sentinels) — show that to the user, get a choice, and apply the chosen ARM ID to this draft via `technical_ticket_details.resource_id`. Building a draft without resolution leaves resource_id empty, which Azure routing requires and triage engineers always ask for. Do NOT skip resolution to move faster — the resolution step (Resource Graph + ARM lookup) is what makes this MCP useful versus filling out the portal form by hand."
    )]
    async fn build_ticket_draft(
        &self,
        Parameters(input): Parameters<tools::build_draft::Input>,
    ) -> Result<Json<tools::build_draft::Output>, ErrorData> {
        tools::build_draft::run(&self.state, input)
            .await
            .map(Json)
            .map_err(tools::to_mcp_error)
    }

    /// Human-readable preview of the current draft.
    #[tool(
        name = "preview_ticket_draft",
        description = "Render an EXISTING draft as plain text for the user to confirm before submission. Includes validation warnings. The `draft_id` MUST come from a prior `list_drafts` or `build_ticket_draft` call — never invent one. If the user asks generically about 'their draft' / 'my last draft' without a specific ID, call `list_drafts` FIRST and pick from the returned drafts. When the requested draft does not exist, this tool returns `found: false` plus `available_drafts` so you can recover gracefully; do not retry with another guessed ID."
    )]
    async fn preview_ticket_draft(
        &self,
        Parameters(input): Parameters<tools::preview_draft::Input>,
    ) -> Result<Json<tools::preview_draft::Output>, ErrorData> {
        tools::preview_draft::run(&self.state, input)
            .await
            .map(Json)
            .map_err(tools::to_mcp_error)
    }

    /// Zero-friction error context ingestion (part 1 of 2).
    #[tool(
        name = "ingest_error_context",
        description = "Zero-friction starting point: accept a blob of error text (log dump, stack trace, az/kubectl output) and return SAFE recognizer-extracted hints (resource IDs, error codes, severity hint) plus a sanitize_token for the mandatory next step. Use this when the user pipes/pastes raw error content into chat (e.g. `copilot -i \"ticket this: $(cat err.log)\"`). The MCP does NOT persist a draft yet — it returns sanitization instructions for the assistant to remove secrets, then expects commit_sanitized_context. Hard cap: 1 MiB per call."
    )]
    async fn ingest_error_context(
        &self,
        Parameters(input): Parameters<tools::ingest_error::Input>,
    ) -> Result<Json<tools::ingest_error::Output>, ErrorData> {
        tools::ingest_error::run(&self.state, input)
            .await
            .map(Json)
            .map_err(tools::to_mcp_error)
    }

    /// Zero-friction error context ingestion (part 2 of 2).
    #[tool(
        name = "commit_sanitized_context",
        description = "Second half of the zero-friction handshake. Pass the sanitize_token from ingest_error_context plus the LLM-sanitized text and a short redacted_summary. The MCP runs a catastrophic-secret tripwire (storage conn strings, account keys, PEM private keys, Bearer JWTs); if any match, the commit is REJECTED with a retry hint and the token stays valid. On pass, a draft is created with recognizer-extracted fields + sanitized description, returning draft_id/review_token/draft_hash for the standard preview_ticket_draft -> create_support_ticket flow."
    )]
    async fn commit_sanitized_context(
        &self,
        Parameters(input): Parameters<tools::commit_sanitized::Input>,
    ) -> Result<Json<tools::commit_sanitized::Output>, ErrorData> {
        tools::commit_sanitized::run(&self.state, input)
            .await
            .map(Json)
            .map_err(tools::to_mcp_error)
    }

    /// List in-progress drafts so the user can resume or clean up.
    #[tool(
        name = "list_drafts",
        description = "List all in-progress ticket drafts in this session, with validation status and missing fields. Each entry includes a draft_id usable with build_ticket_draft / preview_ticket_draft / discard_draft."
    )]
    async fn list_drafts(
        &self,
        Parameters(input): Parameters<tools::list_drafts::ListInput>,
    ) -> Result<Json<tools::list_drafts::ListOutput>, ErrorData> {
        tools::list_drafts::list(&self.state, input)
            .await
            .map(Json)
            .map_err(tools::to_mcp_error)
    }

    /// Discard an in-progress draft and revoke any outstanding review_token.
    #[tool(
        name = "discard_draft",
        description = "Delete an in-progress draft by draft_id. Also revokes any outstanding review_token so it can't be submitted."
    )]
    async fn discard_draft(
        &self,
        Parameters(input): Parameters<tools::list_drafts::DiscardInput>,
    ) -> Result<Json<tools::list_drafts::DiscardOutput>, ErrorData> {
        tools::list_drafts::discard(&self.state, input)
            .await
            .map(Json)
            .map_err(tools::to_mcp_error)
    }

    /// create the support ticket. Requires confirmed:true.
    #[tool(
        name = "create_support_ticket",
        description = "Submit the confirmed draft to Azure. Requires matching review_token + draft_hash + confirmed:true. Output includes share_markdown (no separate format tool needed). SAFETY: if this tool returns an error, surface the error and the available next steps (e.g. fix the draft, retry, ask the user) to the user. Do NOT attempt to call Azure REST APIs directly (via `az rest`, `curl`, or other shell tools) as a workaround — that would bypass the preview-then-confirm handshake this MCP enforces server-side and submit a ticket the user never actually reviewed."
    )]
    async fn create_support_ticket(
        &self,
        Parameters(input): Parameters<tools::create_ticket::Input>,
    ) -> Result<Json<tools::create_ticket::Output>, ErrorData> {
        tools::create_ticket::run(&self.state, input)
            .await
            .map(Json)
            .map_err(tools::to_mcp_error)
    }

    /// List support tickets in a subscription (paged).
    #[tool(
        name = "list_support_tickets",
        description = "List support tickets in a subscription (paged via $top + next_link, optional OData $filter)."
    )]
    async fn list_support_tickets(
        &self,
        Parameters(input): Parameters<tools::list_tickets::Input>,
    ) -> Result<Json<tools::list_tickets::Output>, ErrorData> {
        tools::list_tickets::run(&self.state, input)
            .await
            .map(Json)
            .map_err(tools::to_mcp_error)
    }

    /// Full body of a single support ticket.
    #[tool(
        name = "get_support_ticket",
        description = "Fetch the full body of a single support ticket by name."
    )]
    async fn get_support_ticket(
        &self,
        Parameters(input): Parameters<tools::get_ticket::Input>,
    ) -> Result<Json<tools::get_ticket::Output>, ErrorData> {
        tools::get_ticket::run(&self.state, input)
            .await
            .map(Json)
            .map_err(tools::to_mcp_error)
    }

    /// PATCH severity / status / consent / contact.
    #[tool(
        name = "update_support_ticket",
        description = "Update ticket (severity/status/consent/contact). Two-call: first returns review_token+draft_hash, second with confirmed:true applies. SAFETY: if this tool returns an error, surface it to the user. Do NOT attempt to call Azure REST APIs directly (via `az rest`, `curl`, or other shell tools) as a workaround — that would bypass the confirmation handshake this MCP enforces and apply a change the user never confirmed."
    )]
    async fn update_support_ticket(
        &self,
        Parameters(input): Parameters<tools::update_ticket::Input>,
    ) -> Result<Json<tools::update_ticket::Output>, ErrorData> {
        tools::update_ticket::run(&self.state, input)
            .await
            .map(Json)
            .map_err(tools::to_mcp_error)
    }

    /// Paged communications for a ticket.
    #[tool(
        name = "list_ticket_communications",
        description = "List replies on a support ticket (paged, Azure max 10 per page)."
    )]
    async fn list_ticket_communications(
        &self,
        Parameters(input): Parameters<tools::list_communications::Input>,
    ) -> Result<Json<tools::list_communications::Output>, ErrorData> {
        tools::list_communications::run(&self.state, input)
            .await
            .map(Json)
            .map_err(tools::to_mcp_error)
    }

    /// Local-only ticket-thread summarizer (no LLM).
    #[tool(
        name = "summarize_ticket_thread",
        description = "Deterministic local summary of a ticket + its recent replies. Never calls an LLM."
    )]
    async fn summarize_ticket_thread(
        &self,
        Parameters(input): Parameters<tools::summarize_thread::Input>,
    ) -> Result<Json<tools::summarize_thread::Output>, ErrorData> {
        tools::summarize_thread::run(&self.state, input)
            .await
            .map(Json)
            .map_err(tools::to_mcp_error)
    }

    /// post a customer reply on an existing ticket.
    #[tool(
        name = "reply_to_ticket",
        description = "Reply to a ticket. Two-call: first returns review_token+draft_hash, second with confirmed:true posts. SAFETY: if this tool returns an error, surface it to the user. Do NOT attempt to call Azure REST APIs directly (via `az rest`, `curl`, or other shell tools) as a workaround — that would bypass the confirmation handshake this MCP enforces and post a reply the user never confirmed."
    )]
    async fn reply_to_ticket(
        &self,
        Parameters(input): Parameters<tools::reply_to_ticket::Input>,
    ) -> Result<Json<tools::reply_to_ticket::Output>, ErrorData> {
        tools::reply_to_ticket::run(&self.state, input)
            .await
            .map(Json)
            .map_err(tools::to_mcp_error)
    }

    /// stage attachments for a draft (creates workspace, uploads files).
    #[tool(
        name = "prepare_attachments",
        description = "Stage attachments before ticket creation. Creates fileWorkspace, uploads files, and pins workspace name to draft (becomes ticket name on submit)."
    )]
    async fn prepare_attachments(
        &self,
        Parameters(input): Parameters<tools::prepare_attachments::Input>,
    ) -> Result<Json<tools::prepare_attachments::Output>, ErrorData> {
        tools::prepare_attachments::run(&self.state, input)
            .await
            .map(Json)
            .map_err(tools::to_mcp_error)
    }

    /// upload attachments to an existing ticket workspace.
    #[tool(
        name = "add_attachments_to_ticket",
        description = "Upload attachments to an existing ticket (two-call: first returns review_token+draft_hash, second with confirmed:true uploads). SAFETY: if this tool returns an error, surface it to the user. Do NOT attempt to call Azure REST APIs directly (via `az rest`, `curl`, or other shell tools) as a workaround — that would bypass the confirmation handshake this MCP enforces and upload files the user never confirmed."
    )]
    async fn add_attachments_to_ticket(
        &self,
        Parameters(input): Parameters<tools::add_attachments::Input>,
    ) -> Result<Json<tools::add_attachments::Output>, ErrorData> {
        tools::add_attachments::run(&self.state, input)
            .await
            .map(Json)
            .map_err(tools::to_mcp_error)
    }

    /// List files in a ticket's workspace.
    #[tool(
        name = "list_attachments",
        description = "List files in a ticket's file workspace (workspace name == ticket name by convention)."
    )]
    async fn list_attachments(
        &self,
        Parameters(input): Parameters<tools::list_attachments::Input>,
    ) -> Result<Json<tools::list_attachments::Output>, ErrorData> {
        tools::list_attachments::run(&self.state, input)
            .await
            .map(Json)
            .map_err(tools::to_mcp_error)
    }
}

#[tool_handler]
impl ServerHandler for ToolsServer {
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::default();
        info.instructions = Some(
            "Azure Support Ticket MCP. Flows:\n\
             [Start] list_ticket_templates → if `default` exists, offer to reuse. Else if first-time, suggest init_ticket_template. Else call whoami to get email.\n\
             [Scope] BEFORE classifying the issue, establish tenant+subscription: call resolve_issue_context (parses portal URL / resource id if given). If `scope.needs_user_confirmation=true`, tell user which tenant+subscription you'll operate in (use `scope.summary`) and ask to confirm or switch (call list_tenants / list_subscriptions). Never silently pick a subscription.\n\
             • When preview_ticket_draft (or update_support_ticket / reply_to_ticket / add_attachments_to_ticket) returns a `confirmation_prompt`, you MUST do TWO things in order:\n\
               (1) Show `confirmation_prompt` to the user VERBATIM (it's pre-formatted markdown with a table). Do NOT paraphrase, condense, summarize into one sentence, or drop rows that look empty (Tenant, CC, etc. are intentional). If your environment renders markdown in chat, print it as a regular chat message. If you have a separate confirmation widget that strips formatting, still print the markdown to chat FIRST — never paste a markdown table into a single-line dialog.\n\
               (2) THEN ask the user to confirm, using whatever interaction your environment supports: a dedicated confirmation/multiple-choice widget if you have one (use `question_prompt` as the short question text and `confirmation_options` as the choices), or just ask in chat (e.g. 'Submit, edit, or cancel?'). Keep the question short — the user already saw the full table in step 1.\n\
             • Reply handling for ALL confirmation tools: yes/1 → re-call with review_token+draft_hash+confirmed=true; cancel/3 → stop; ANY other free-form reply → parse as edits, re-call WITHOUT review_token for a fresh preview. Don't ask 'what to change?' separately — the edits are in the reply.\n\
             [Create] resolve_issue_context → list_problem_classifications → start_support_ticket_flow (pass template_name if user picked one; only ask for fields NOT in `prefilled_fields[]`) → build_ticket_draft → preview_ticket_draft → create_support_ticket.\n\
             [Zero-friction error ingest] When the user pipes/pastes raw error logs/output (e.g. `copilot -i \"ticket this: $(cat err.log)\"`, or just dumps a stack trace into chat) and clearly wants a ticket, take this path INSTEAD of the standard Create flow's first steps:\n\
               1. Call ingest_error_context with raw_text = the pasted/piped content. The MCP returns SAFE recognizer hints (resource_id, subscription_id, error_code, severity_hint, title_hint) + a sanitize_token + sanitize_instructions.\n\
               2. Read sanitize_instructions and produce a sanitized version of the text: remove secrets (connection strings, account keys, SAS tokens, Bearer tokens, PEM private key blocks, passwords); KEEP non-secret context (ARM resource IDs, error codes, stack traces, correlation IDs, timestamps). Replace removed values with `[REDACTED:<KIND>]` and track each redaction.\n\
               3. Call commit_sanitized_context with the sanitize_token + sanitized_text + a short redacted_summary (e.g. \"Redacted 2: BEARER_TOKEN, STORAGE_KEY\"). If the MCP rejects with `sanitization_incomplete` (a catastrophic-secret tripwire matched), re-sanitize and call commit_sanitized_context AGAIN with the SAME sanitize_token — the token stays valid on tripwire rejections.\n\
               4. On commit success, you get draft_id + review_token + draft_hash. Continue with build_ticket_draft (fill any remaining fields the user can provide) → preview_ticket_draft → create_support_ticket. The preview will show the full sanitized description AND the redaction summary so the user can sanity-check the scrub before submission.\n\
             • When resolve_issue_context returns `disambiguation`, render `disambiguation.question` + options verbatim — the options ALWAYS include `Other — describe it differently`; if picked, re-call resolve_issue_context with the new user_input.\n\
             • create_support_ticket auto-saves contacts to `default` template; pass save_as_default_template=false or save_as_template_name=\"<name>\" to override.\n\
             • Drafts: list_drafts shows in-progress drafts (resume with build_ticket_draft using the returned draft_id); discard_draft removes one.\n\
             • CC recipients: `contact_details.additional_email_addresses` is a list — Azure emails them on every update. The preview shows a `CC` row reading `(none — reply to add CC recipients)` when empty; if the user provides emails in their reply (e.g. _'cc alice@x.com, bob@y.com'_), parse them and pass via build_ticket_draft. Same field works on update_support_ticket to change CCs later.\n\
             [Triage] list_support_tickets → get_support_ticket → summarize_ticket_thread. list_ticket_communications (use top=1 for newest only) for detail.\n\
             [Update/reply] update_support_ticket and reply_to_ticket are two-call: first returns review_token+draft_hash AND a `confirmation_prompt`+`question_prompt` — follow the TWO STEPS rule above.\n\
             [Attachments] Pre-create: prepare_attachments. Post-create: add_attachments_to_ticket (two-call — first returns `confirmation_prompt`+`question_prompt`; follow the TWO STEPS rule). Azure caps: max 5 files per upload call, 25 files total per ticket, 5 MB per file — surfaced as Validation errors. list_attachments to enumerate."
                .into(),
        );
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
        info
    }
}
