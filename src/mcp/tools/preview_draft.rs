//! `preview_ticket_draft`: a human-readable rendering of the current draft.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::bootstrap::AppState;
use crate::error::AppResult;
use crate::resolver::hints::first_quoted_identifier;
use crate::workflow::draft::TicketDraft;
use crate::workflow::share::severity_label;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct Input {
    pub draft_id: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ConfirmationOption {
    /// Label to show in the prompt (with leading `1.`/`2.`/`3.` numbering).
    pub label: String,
    /// One of `submit` | `modify` | `cancel`.
    pub action: String,
    /// Short description of what happens if the user picks this option.
    pub description: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Output {
    pub draft_id: String,
    /// `false` when the requested `draft_id` was not found. When `false`,
    /// the `preview`, `confirmation_prompt`, `question_prompt`,
    /// `confirmation_options`, `draft_hash`, and `review_token` fields are
    /// empty/sentinel and the assistant should follow
    /// `assistant_instructions` (which directs it to call `list_drafts`
    /// and either pick a real `draft_id` from the returned `available_drafts`
    /// or tell the user there are no drafts).
    pub found: bool,
    /// Rich multi-line preview block (markdown).
    pub preview: String,
    pub draft_hash: String,
    pub review_token: Option<String>,
    /// FULL markdown confirmation block (table + warnings + 3 options).
    /// **Print this to chat verbatim BEFORE calling any user-prompt UI.**
    /// Markdown tables only render in chat surfaces, not inside confirmation
    /// widgets that strip formatting.
    pub confirmation_prompt: String,
    /// Short one-liner safe for a single-line confirmation widget. Hosts
    /// with a confirmation dialog use this as the question text; hosts that
    /// just ask in chat can ignore it.
    pub question_prompt: String,
    /// Explicit three-option list. Clients SHOULD use these exact labels.
    pub confirmation_options: Vec<ConfirmationOption>,
    /// Instructions for the calling assistant on how to handle each option,
    /// OR how to recover when `found == false`.
    pub assistant_instructions: String,
    /// Fields that look missing or weak (e.g. no resource_id, missing
    /// classification). Surfaced so the assistant can mention them in the
    /// confirmation block.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    /// Populated only when `found == false`. Lists the draft IDs that DO
    /// exist in the store right now, so the assistant can either pick one
    /// (if the user clearly meant a different draft) or tell the user
    /// "you have no drafts" (when this list is empty).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub available_drafts: Vec<DraftSummary>,
}

/// Compact summary of an existing draft, surfaced in the `available_drafts`
/// recovery field when the requested `draft_id` was not found.
#[derive(Debug, Serialize, JsonSchema)]
pub struct DraftSummary {
    pub draft_id: String,
    pub title: Option<String>,
    pub service_id: Option<String>,
    pub severity: Option<String>,
    pub subscription_id: Option<String>,
}

pub async fn run(state: &AppState, input: Input) -> AppResult<Output> {
    let mut draft = match state.drafts.get(&input.draft_id).await {
        Ok(d) => d,
        Err(crate::error::AppError::NotFound(_)) => {
            return Ok(not_found_response(state, &input.draft_id).await);
        }
        Err(other) => return Err(other),
    };
    // Defensive backfill: if a previous tool path skipped it (or auth/cache
    // wasn't ready), make one last attempt before the user sees the table.
    super::tenant_lookup::backfill_tenant(state, &mut draft).await;
    if draft.tenant_id.is_some() {
        // Persist so subsequent build/create calls don't have to look it up.
        let _ = state.drafts.put(draft.clone()).await;
    }
    let preview = render(&draft);
    let hash = draft.content_hash();
    let warnings = collect_warnings(&draft);
    // If validator errors are present, surface them at the option layer too:
    // the "Yes, submit" choice gets a clear "WILL FAIL" warning so the model
    // doesn't propose it as the first reply option for an invalid draft.
    let validation = crate::workflow::validator::validate(&draft);
    let submit_warning = if !validation.valid {
        let fields: Vec<String> = validation.errors.iter().map(|e| e.field.clone()).collect();
        format!(
            " WARNING: draft is NOT submit-ready — missing required field(s): \
             {}. Calling create_support_ticket WILL be rejected by Azure. \
             Fill the missing fields via build_ticket_draft and re-preview \
             BEFORE offering this option to the user.",
            fields.join(", ")
        )
    } else {
        String::new()
    };
    let confirmation_options = vec![
        ConfirmationOption {
            label: "Yes, submit the ticket".into(),
            action: "submit".into(),
            description: format!(
                "Call create_support_ticket with confirmed:true to send to Azure.{submit_warning}"
            ),
        },
        ConfirmationOption {
            label: "Type your edits inline (e.g. 'change severity to A and update phone to 555-…')".into(),
            action: "modify".into(),
            description: "Treat ANY free-form reply that is not a clear yes/cancel as edit feedback. Parse the user's message into a TicketDraftPatch, call build_ticket_draft with it, then re-run preview_ticket_draft for a fresh review_token before submitting. Do not ask 'what would you like to change?' as a separate turn — the feedback is already in the user's reply.".into(),
        },
        ConfirmationOption {
            label: "Cancel — don't submit".into(),
            action: "cancel".into(),
            description: "Abandon the draft; nothing is sent to Azure.".into(),
        },
    ];
    let confirmation_prompt = render_confirmation_prompt(&draft, &warnings);
    let question_prompt = "Submit this ticket?".to_string();
    let assistant_instructions = if !validation.valid {
        // Validator-failed path: don't even surface the submit option as
        // an option until missing fields are filled. Saves the user from
        // clicking submit on a draft that's about to be rejected by Azure.
        let fields: Vec<String> = validation.errors.iter().map(|e| e.field.clone()).collect();
        format!(
            "DRAFT IS NOT SUBMIT-READY. Missing required field(s): {fields}. \
             Do NOT offer the user the submit option. Instead, identify the \
             missing fields, ask the user the SINGLE focused question needed \
             to resolve each (e.g. for `advanced_diagnostic_consent` ask \
             'Do you grant Microsoft Support permission to collect advanced \
             diagnostic information for this resource? (yes/no)'), patch the \
             draft via `build_ticket_draft`, then re-call \
             `preview_ticket_draft` to get a fresh, valid preview. Only \
             after the validator passes should you proceed to the standard \
             confirm-then-submit flow.",
            fields = fields.join(", "),
        )
    } else {
        "TWO STEPS, in order:\n\
        \n\
        1. SHOW `confirmation_prompt` to the user VERBATIM (it's pre-formatted markdown with a table AND a fenced Description block below the table). If your environment renders markdown in chat, print it as a normal chat message. If you have a separate confirmation widget that strips formatting, still print the markdown to chat FIRST. Never paste the markdown table into a single-line dialog. Do NOT paraphrase, condense, summarize the description, or drop rows that look empty (Tenant, CC, etc. are intentional). The user MUST see the full Description text exactly as it will be submitted to Azure — never replace it with a one-line summary.\n\
        \n\
        2. THEN ask the user to confirm, using whatever interaction your environment supports: a confirmation/multiple-choice widget (use `question_prompt` as the question and `confirmation_options` as choices), or just ask in chat. Keep the question short — the user already saw the full table in step 1.\n\
        \n\
        Reply handling:\n\
        - `yes` / `submit` / `1` → call create_support_ticket with the returned review_token + draft_hash and confirmed:true.\n\
        - `cancel` / `no` / `3` → acknowledge and stop.\n\
        - ANY other free-form reply (including picking option 2 and inlining edits like 'change severity to A, update phone to 555-1212, mention timeouts in description') → parse into a TicketDraftPatch, call build_ticket_draft with it in the same turn, then preview_ticket_draft again. Do NOT ask a follow-up clarifying question first — the user already told you what to change. Only ask back if the edit is genuinely ambiguous.".into()
    };

    Ok(Output {
        draft_id: draft.draft_id,
        found: true,
        preview,
        draft_hash: hash,
        review_token: None,
        confirmation_prompt,
        question_prompt,
        confirmation_options,
        assistant_instructions,
        warnings,
        available_drafts: Vec::new(),
    })
}

/// Build a soft "draft not found" response that includes the list of drafts
/// that DO exist, so the calling assistant can recover without surfacing a
/// dead-end error to the user.
///
/// Returns instead of erroring because the error path forces the model to
/// give up; this path lets it either pick a real draft (when the user
/// clearly meant a different one) or tell the user "you have no drafts"
/// (when the store is empty).
async fn not_found_response(state: &AppState, requested_id: &str) -> Output {
    let available: Vec<DraftSummary> = state
        .drafts
        .list()
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|d| DraftSummary {
            draft_id: d.draft_id,
            title: d.title,
            service_id: d.service_id,
            severity: d.severity,
            subscription_id: d.subscription_id,
        })
        .collect();

    let assistant_instructions = if available.is_empty() {
        format!(
            "Draft `{requested_id}` does not exist, and the draft store is currently \
             EMPTY. Do NOT invent a draft_id. Tell the user there are no in-progress \
             drafts. If they want to create one, call `start_support_ticket_flow` (or \
             `build_ticket_draft` if they've already given enough context). NEVER \
             retry `preview_ticket_draft` with another guessed ID."
        )
    } else {
        format!(
            "Draft `{requested_id}` does not exist. {n} other draft(s) ARE available \
             — see `available_drafts`. If the user clearly meant one of them, call \
             `preview_ticket_draft` again with that draft_id. Otherwise, summarize \
             `available_drafts` to the user and ask which they want to preview. \
             NEVER invent a draft_id.",
            n = available.len()
        )
    };

    Output {
        draft_id: requested_id.to_string(),
        found: false,
        preview: String::new(),
        draft_hash: String::new(),
        review_token: None,
        confirmation_prompt: String::new(),
        question_prompt: String::new(),
        confirmation_options: Vec::new(),
        assistant_instructions,
        warnings: Vec::new(),
        available_drafts: available,
    }
}

fn render(d: &TicketDraft) -> String {
    let mut s = String::new();
    s.push_str("Azure Support Ticket (draft)\n");
    s.push_str("============================\n");
    line(&mut s, "Title", d.title.as_deref());
    line(
        &mut s,
        "Severity",
        d.severity.as_deref().map(severity_label).as_deref(),
    );
    line(&mut s, "Tenant", d.tenant_id.as_deref());
    line(&mut s, "Subscription", d.subscription_id.as_deref());
    line(&mut s, "Service", d.service_id.as_deref());
    line(
        &mut s,
        "Problem classification",
        d.problem_classification_id.as_deref(),
    );
    line(&mut s, "Resource", d.resource_id.as_deref());
    line(
        &mut s,
        "Advanced diagnostic consent",
        d.advanced_diagnostic_consent.as_deref(),
    );
    line(
        &mut s,
        "Problem start time",
        d.problem_start_time.as_deref(),
    );

    s.push_str("\nContact\n-------\n");
    let c = &d.contact_details;
    let name = match (&c.first_name, &c.last_name) {
        (Some(f), Some(l)) => Some(format!("{f} {l}")),
        (Some(f), None) => Some(f.clone()),
        (None, Some(l)) => Some(l.clone()),
        _ => None,
    };
    line(&mut s, "Name", name.as_deref());
    line(&mut s, "Email", c.primary_email_address.as_deref());
    let cc = if c.additional_email_addresses.is_empty() {
        None
    } else {
        Some(c.additional_email_addresses.join(", "))
    };
    line(&mut s, "CC (also email)", cc.as_deref());
    line(&mut s, "Phone", c.phone_number.as_deref());
    line(&mut s, "Method", c.preferred_contact_method.as_deref());
    line(&mut s, "Language", c.preferred_support_language.as_deref());
    line(&mut s, "Time zone", c.preferred_time_zone.as_deref());
    line(&mut s, "Country", c.country.as_deref());

    s.push_str("\nDescription\n-----------\n");
    s.push_str(d.description.as_deref().unwrap_or("(none)"));
    s.push('\n');
    s
}

/// Surface "soft" issues that don't block submission but are worth mentioning
/// in the confirmation block so the user can decide to fix them first.
///
/// Three flavors of warnings, all rendered the same way to the user:
/// - **Blocking** (e.g. missing required field) — Azure WILL reject the
///   ticket; surface at the top with a clear BLOCKING marker. These come
///   from running the validator before drawing the preview.
/// - **Display gaps** (e.g. tenant unknown) — degrade the user's ability to
///   sanity-check the ticket but don't affect Azure's ability to create it.
/// - **Submission warnings** (e.g. critical severity, short description) —
///   things Azure WILL accept but that are likely to slow triage.
fn collect_warnings(d: &TicketDraft) -> Vec<String> {
    let mut w = Vec::new();

    // --- Blocking issues (validator errors) ---
    //
    // Without this section, missing required fields like
    // `advanced_diagnostic_consent` would only surface at create-ticket time
    // as a server-side validation failure — by which point the user has
    // already clicked submit on a draft that was never going to succeed.
    // Surface them HERE so the model knows the draft isn't submit-ready
    // and can fix-then-re-preview instead of submit-then-fail.
    let report = crate::workflow::validator::validate(d);
    if !report.valid {
        let missing: Vec<String> = report.errors.iter().map(|e| e.field.clone()).collect();
        w.push(format!(
            "BLOCKING — draft is NOT submit-ready. Missing required field(s): \
             {fields}. Azure will reject `create_support_ticket` until these \
             are set. Use `build_ticket_draft` to fill them, then re-preview. \
             Do NOT click submit on this draft as-is.",
            fields = missing.join(", "),
        ));
    }

    // --- Display gaps (informational; never block submission) ---
    if d.tenant_id.is_none() && d.subscription_id.is_some() {
        w.push("Tenant ID could not be resolved for this subscription — the ticket will still submit, but the preview won't show *which* tenant it's under. The MCP tried both `GET /subscriptions/{id}` and `GET /subscriptions`; both failed. Most often this means no Azure credentials are available (run `az login`) or there's no network reach to ARM.".into());
    }

    // --- Submission warnings ---
    if d.resource_id.is_none() && d.technical_ticket_details.resource_id.is_none() {
        if let Some(hint) = extract_resource_hint(d) {
            w.push(format!(
                "RESOURCE NOT RESOLVED: the description references `{hint}` but no \
                 resource_id is set. BEFORE submitting, call `resolve_issue_context` \
                 with `text: \"{hint}\"` (and the selected subscription_id) — it \
                 queries Azure Resource Graph and ARM to find the exact resource. \
                 Take the top-ranked candidate from its output, then call \
                 `build_ticket_draft` with `technical_ticket_details.resource_id` \
                 set. Submitting without resolution slows Azure routing \
                 significantly and the engineer will ask for the ARM resource ID \
                 anyway. If the issue is genuinely not tied to a specific \
                 resource, the user can override; otherwise do NOT submit yet."
            ));
        } else {
            w.push(
                "No resource ID — Azure routing may be slower and the engineer \
                 will likely ask for one. If the description names a specific \
                 resource, call `resolve_issue_context` to look it up first."
                    .into(),
            );
        }
    }
    if d.problem_classification_id.is_none() {
        w.push("No problem classification selected — required by Azure.".into());
    }
    if d.severity.as_deref() == Some("critical")
        || d.severity.as_deref() == Some("highestcriticalimpact")
    {
        w.push("Severity is critical — make sure 24x7 contact info is correct.".into());
    }
    match d.description.as_deref() {
        None => {
            w.push("No description — Azure will accept the ticket but engineers will ask for one. Add steps to reproduce, timestamps, and error text.".into());
        }
        Some(x) if x.len() < 50 => {
            w.push("Description is short — engineers triage faster with steps to reproduce, timestamps, and error text.".into());
        }
        _ => {}
    }
    w
}

/// Detect when a draft's title/description names a specific resource that
/// hasn't been resolved yet. Triggers the strong "RESOURCE NOT RESOLVED"
/// warning that pushes the assistant to call `resolve_issue_context`.
///
/// High-signal heuristic: look for a quoted identifier in title or description.
/// Users (and models) naturally write `"contoso-b2c"`, `'prod-aks'`, or
/// `` `gpt-4o-prod` `` when naming a specific Azure resource. A bare token
/// scan would have far more false positives (every common noun would
/// trigger), so we deliberately stay narrow — better to miss some hints
/// than to nag on every draft.
/// Pull a resource-name hint out of a draft. Walks title then description,
/// returning the first quoted identifier found via
/// [`crate::resolver::hints::first_quoted_identifier`].
fn extract_resource_hint(d: &TicketDraft) -> Option<String> {
    let mut sources: Vec<&str> = Vec::new();
    if let Some(t) = d.title.as_deref() {
        sources.push(t);
    }
    if let Some(desc) = d.description.as_deref() {
        sources.push(desc);
    }
    for text in sources {
        if let Some(hint) = first_quoted_identifier(text) {
            return Some(hint);
        }
    }
    None
}

/// One-line snippet for the field table. Keeps Description visible even when
/// the full fenced block below the table gets stripped or paraphrased by the
/// host's rendering.
fn description_snippet(desc: &str) -> String {
    // Collapse all whitespace (incl. newlines) to single spaces for the row.
    let collapsed: String = desc.split_whitespace().collect::<Vec<_>>().join(" ");
    const MAX: usize = 80;
    if collapsed.chars().count() <= MAX {
        collapsed
    } else {
        let truncated: String = collapsed.chars().take(MAX).collect();
        format!("{truncated}… _(full text below)_")
    }
}

/// Build a multi-line markdown confirmation block. Clients are instructed to
/// render this verbatim (no condensing) so the user sees structured fields,
/// any warnings, and the three options.
fn render_confirmation_prompt(d: &TicketDraft, warnings: &[String]) -> String {
    let mut s = String::new();
    s.push_str("**Ready to submit this Azure Support ticket?**\n\n");

    s.push_str("| Field | Value |\n");
    s.push_str("|---|---|\n");
    row(&mut s, "Title", d.title.as_deref());
    row(
        &mut s,
        "Severity",
        d.severity.as_deref().map(severity_label).as_deref(),
    );
    row(&mut s, "Service", d.service_id.as_deref());
    row(
        &mut s,
        "Classification",
        d.problem_classification_id.as_deref(),
    );
    let resource = d
        .resource_id
        .as_deref()
        .or(d.technical_ticket_details.resource_id.as_deref());
    row(&mut s, "Resource", resource);
    let tenant_display = d
        .tenant_id
        .clone()
        .unwrap_or_else(|| "**unknown** (couldn't reach Azure - see warnings below)".to_string());
    row(&mut s, "Tenant", Some(&tenant_display));
    row(&mut s, "Subscription", d.subscription_id.as_deref());
    let c = &d.contact_details;
    let name = match (&c.first_name, &c.last_name) {
        (Some(f), Some(l)) => Some(format!("{f} {l}")),
        _ => c.first_name.clone().or_else(|| c.last_name.clone()),
    };
    row(&mut s, "Contact", name.as_deref());
    row(&mut s, "Email", c.primary_email_address.as_deref());
    if !c.additional_email_addresses.is_empty() {
        row(
            &mut s,
            "CC (also email)",
            Some(&c.additional_email_addresses.join(", ")),
        );
    } else {
        row(
            &mut s,
            "CC (also email)",
            Some("(none — reply to add CC recipients)"),
        );
    }
    row(
        &mut s,
        "Diagnostic consent",
        d.advanced_diagnostic_consent.as_deref(),
    );
    // Description snippet inline in the table — keeps the field visible even
    // if the full fenced block below gets stripped or paraphrased.
    let desc_snippet = d.description.as_deref().map(description_snippet);
    row(&mut s, "Description", desc_snippet.as_deref());
    s.push('\n');

    // Full description block. Always rendered — when missing, we say so
    // explicitly so the user notices the absence rather than silently
    // dropping the section.
    s.push_str("**Description (full text — review carefully before submitting):**\n\n");
    if let Some(desc) = d.description.as_deref() {
        s.push_str("```text\n");
        s.push_str(desc);
        if !desc.ends_with('\n') {
            s.push('\n');
        }
        s.push_str("```\n\n");
    } else {
        s.push_str("_(no description set — Azure will accept the ticket but engineers will ask for one)_\n\n");
    }

    // Redaction summary from the LLM sanitization step, if any.
    if let Some(rs) = d.redacted_summary.as_deref() {
        s.push_str("**Sanitization summary** (from the LLM scrubbing step):\n");
        s.push_str(&format!("- {rs}\n"));
        s.push_str("- The MCP also ran a catastrophic-secret tripwire (storage conn strings, account keys, PEM private keys, Bearer JWTs) — none matched, otherwise this preview would not exist.\n\n");
    }

    if !warnings.is_empty() {
        s.push_str("**Things to note:**\n");
        for w in warnings {
            s.push_str(&format!("- {w}\n"));
        }
        s.push('\n');
    }

    s.push_str("**Reply with one of:**\n");
    s.push_str("1. **Yes, submit** — send to Azure now.\n");
    s.push_str("2. **Your edits, inline** — just type what to change in plain English (e.g. _'change severity to A, set phone to 555-1212, mention timeouts in the description'_) and I'll apply it and re-confirm. No need to pick this option first — any reply that isn't a yes/cancel is treated as edits.\n");
    s.push_str("3. **Cancel** — don't submit.\n");
    s
}

fn line(s: &mut String, label: &str, value: Option<&str>) {
    if let Some(v) = value {
        if !v.is_empty() {
            s.push_str(&format!("{label}: {v}\n"));
        }
    }
}

fn row(s: &mut String, label: &str, value: Option<&str>) {
    let v = value.filter(|x| !x.is_empty()).unwrap_or("_(not set)_");
    s.push_str(&format!("| {label} | {v} |\n"));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft_with(title: &str, classification: Option<&str>) -> TicketDraft {
        let mut d = TicketDraft::new();
        d.title = Some(title.into());
        d.severity = Some("moderate".into());
        d.service_id = Some("Azure AI Foundry".into());
        d.problem_classification_id = classification.map(str::to_string);
        d.contact_details.primary_email_address = Some("alice@contoso.com".into());
        d.contact_details.first_name = Some("Alice".into());
        d.contact_details.last_name = Some("Example".into());
        d.description = Some("short".into());
        d
    }

    // Regression guard for Bug 1: a draft missing a required field MUST
    // surface that as a BLOCKING warning in collect_warnings, so the
    // assistant sees it before offering submit. Prior bug: missing
    // `advanced_diagnostic_consent` was only caught at create_support_ticket
    // time, AFTER the user clicked submit. This locks in that any required
    // field appears in the preview warnings (not just the one specific
    // case from the trace).
    #[test]
    fn missing_required_field_surfaces_blocking_warning_in_preview() {
        let mut d = draft_with("HTTP 429", Some("Deployments / Rate Limit"));
        // Intentionally leave advanced_diagnostic_consent unset to mirror
        // the real trace.
        d.advanced_diagnostic_consent = None;
        let warnings = collect_warnings(&d);
        let blocking = warnings
            .iter()
            .find(|w| w.contains("BLOCKING"))
            .expect("missing required field must produce BLOCKING warning");
        assert!(
            blocking.contains("advanced_diagnostic_consent"),
            "blocking warning must name the missing field, got: {blocking}"
        );
        assert!(
            blocking.contains("build_ticket_draft"),
            "blocking warning must point at how to fix, got: {blocking}"
        );
        assert!(
            blocking.contains("Do NOT click submit"),
            "blocking warning must explicitly forbid submit, got: {blocking}"
        );
    }

    #[test]
    fn confirmation_prompt_is_multiline_and_three_options() {
        let d = draft_with("HTTP 429", Some("Deployments / Rate Limit"));
        let warnings = collect_warnings(&d);
        let s = render_confirmation_prompt(&d, &warnings);
        assert!(s.contains("Ready to submit"));
        assert!(s.contains("| Title | HTTP 429 |"));
        assert!(s.contains("| Classification | Deployments / Rate Limit |"));
        assert!(s.contains("Yes, submit"));
        assert!(s.contains("Your edits, inline"));
        assert!(s.contains("Cancel"));
        // multi-line, not a single paragraph
        assert!(
            s.lines().count() > 10,
            "prompt should be multi-line, got:\n{s}"
        );
    }

    #[test]
    fn cc_row_shows_hint_when_empty() {
        let d = draft_with("X", None);
        let s = render_confirmation_prompt(&d, &[]);
        assert!(
            s.contains("| CC (also email) | (none — reply to add CC recipients) |"),
            "expected empty-CC hint, got:\n{s}"
        );
    }

    #[test]
    fn cc_row_joins_addresses_when_present() {
        let mut d = draft_with("X", None);
        d.contact_details.additional_email_addresses =
            vec!["alice@x.com".into(), "bob@y.com".into()];
        let s = render_confirmation_prompt(&d, &[]);
        assert!(
            s.contains("| CC (also email) | alice@x.com, bob@y.com |"),
            "expected joined CC list, got:\n{s}"
        );
    }

    #[test]
    fn warnings_flag_missing_resource_and_classification() {
        let mut d = draft_with("x", None);
        d.resource_id = None;
        let w = collect_warnings(&d);
        assert!(w.iter().any(|m| m.contains("resource ID")));
        assert!(w.iter().any(|m| m.contains("classification")));
        assert!(w.iter().any(|m| m.contains("Description is short")));
    }

    #[test]
    fn quoted_resource_hint_in_description_escalates_warning() {
        // When the user names a specific resource in quotes and resource_id is
        // unset, the warning must be the strong RESOURCE-NOT-RESOLVED variant
        // that names the hint and tells the assistant to call resolve_issue_context.
        let mut d = draft_with("Cannot delete resource", Some("c"));
        d.resource_id = None;
        d.description = Some("I cannot delete the \"contoso-b2c\" resource. Help.".into());
        let w = collect_warnings(&d);
        let strong = w
            .iter()
            .find(|m| m.contains("RESOURCE NOT RESOLVED"))
            .expect("expected escalated warning when a quoted hint is present");
        assert!(
            strong.contains("contoso-b2c"),
            "warning must name the hint, got: {strong}"
        );
        assert!(
            strong.contains("resolve_issue_context"),
            "warning must point at the resolver tool, got: {strong}"
        );
        assert!(
            strong.contains("build_ticket_draft"),
            "warning must point at how to apply the fix, got: {strong}"
        );
        // The soft fallback variant must NOT fire alongside the strong one.
        assert!(
            !w.iter()
                .any(|m| m.starts_with("No resource ID — Azure routing may be slower")),
            "soft warning must not duplicate the strong one, got: {w:?}"
        );
    }

    #[test]
    fn quoted_resource_hint_in_title_also_escalates_warning() {
        let mut d = draft_with("Delete \"prod-aks\" failing", Some("c"));
        d.resource_id = None;
        d.description = Some("It just fails.".into());
        let w = collect_warnings(&d);
        let strong = w
            .iter()
            .find(|m| m.contains("RESOURCE NOT RESOLVED"))
            .expect("title-only hint must also trigger the escalated warning");
        assert!(strong.contains("prod-aks"));
    }

    #[test]
    fn no_quoted_hint_keeps_soft_warning() {
        // When the user wrote about the issue generally without naming a
        // specific resource, the soft warning fires (still actionable but
        // doesn't yell about a hint we didn't actually detect).
        let mut d = draft_with("Generic issue", Some("c"));
        d.resource_id = None;
        d.description = Some("The thing does not work as expected.".into());
        let w = collect_warnings(&d);
        assert!(
            !w.iter().any(|m| m.contains("RESOURCE NOT RESOLVED")),
            "should not escalate when no hint is present, got: {w:?}"
        );
        assert!(
            w.iter().any(|m| m.starts_with("No resource ID")),
            "soft warning must still fire, got: {w:?}"
        );
    }

    #[test]
    fn quoted_hint_with_resource_id_set_suppresses_warning() {
        // Once resource is resolved, neither warning fires.
        let mut d = draft_with("Cannot delete", Some("c"));
        d.description = Some("Cannot delete the \"contoso-b2c\" resource.".into());
        d.resource_id = Some(
            "/subscriptions/00000000-0000-0000-0000-000000000000/resourceGroups/contoso-b2c".into(),
        );
        let w = collect_warnings(&d);
        assert!(
            !w.iter().any(|m| m.contains("RESOURCE NOT RESOLVED")),
            "resolved draft must not trigger the strong warning, got: {w:?}"
        );
        assert!(
            !w.iter().any(|m| m.starts_with("No resource ID")),
            "resolved draft must not trigger the soft warning either, got: {w:?}"
        );
    }

    #[test]
    fn warns_when_description_missing_entirely() {
        let mut d = draft_with("x", Some("c"));
        d.description = None;
        let w = collect_warnings(&d);
        assert!(
            w.iter().any(|m| m.contains("No description")),
            "expected missing-description warning, got: {w:?}"
        );
    }

    #[test]
    fn description_row_in_table_with_full_text_when_short() {
        let mut d = draft_with("x", Some("c"));
        d.description = Some("short desc".into());
        let s = render_confirmation_prompt(&d, &[]);
        assert!(
            s.contains("| Description | short desc |"),
            "expected inline short description row, got:\n{s}"
        );
    }

    #[test]
    fn description_row_in_table_truncates_long_text() {
        let mut d = draft_with("x", Some("c"));
        d.description = Some("a".repeat(200));
        let s = render_confirmation_prompt(&d, &[]);
        assert!(
            s.contains("… _(full text below)_"),
            "expected truncation marker in description row, got:\n{s}"
        );
    }

    #[test]
    fn description_row_collapses_newlines() {
        let mut d = draft_with("x", Some("c"));
        d.description = Some("line one\n\nline two\nline three".into());
        let s = render_confirmation_prompt(&d, &[]);
        assert!(
            s.contains("| Description | line one line two line three |"),
            "expected collapsed-whitespace description row, got:\n{s}"
        );
    }

    #[test]
    fn description_row_shows_not_set_when_missing() {
        let mut d = draft_with("x", Some("c"));
        d.description = None;
        let s = render_confirmation_prompt(&d, &[]);
        assert!(
            s.contains("| Description | _(not set)_ |"),
            "expected (not set) marker in description row, got:\n{s}"
        );
    }

    #[test]
    fn full_description_block_always_rendered_even_when_missing() {
        let mut d = draft_with("x", Some("c"));
        d.description = None;
        let s = render_confirmation_prompt(&d, &[]);
        assert!(
            s.contains("**Description (full text"),
            "expected Description section header even when missing, got:\n{s}"
        );
        assert!(
            s.contains("no description set"),
            "expected explicit 'no description set' placeholder, got:\n{s}"
        );
    }

    #[test]
    fn full_description_block_shows_text_in_fenced_block() {
        let mut d = draft_with("x", Some("c"));
        d.description = Some("Step 1: do thing\nStep 2: error appears".into());
        let s = render_confirmation_prompt(&d, &[]);
        assert!(
            s.contains("```text\nStep 1: do thing\nStep 2: error appears\n```"),
            "expected fenced description block with full text, got:\n{s}"
        );
    }

    #[test]
    fn warnings_flag_critical_severity() {
        let mut d = draft_with("x", Some("c"));
        d.severity = Some("critical".into());
        let w = collect_warnings(&d);
        assert!(w.iter().any(|m| m.contains("Severity is critical")));
    }

    #[test]
    fn warns_when_tenant_missing_but_subscription_set() {
        let mut d = draft_with("x", Some("c"));
        d.subscription_id = Some("00000000-0000-0000-0000-000000000000".into());
        d.tenant_id = None;
        let w = collect_warnings(&d);
        assert!(
            w.iter()
                .any(|m| m.contains("Tenant ID could not be resolved")),
            "expected tenant warning, got: {w:?}"
        );
    }

    #[test]
    fn no_tenant_warning_when_subscription_also_missing() {
        let mut d = draft_with("x", Some("c"));
        d.subscription_id = None;
        d.tenant_id = None;
        let w = collect_warnings(&d);
        assert!(
            !w.iter()
                .any(|m| m.contains("Tenant ID could not be resolved")),
            "tenant warning should be suppressed without a subscription, got: {w:?}"
        );
    }

    #[test]
    fn tenant_row_shows_warning_placeholder_when_missing() {
        let mut d = draft_with("x", Some("c"));
        d.subscription_id = Some("00000000-0000-0000-0000-000000000000".into());
        d.tenant_id = None;
        let s = render_confirmation_prompt(&d, &collect_warnings(&d));
        assert!(
            s.contains("| Tenant | **unknown**"),
            "expected explicit tenant placeholder, got:\n{s}"
        );
    }

    // --- Not-found recovery ---------------------------------------------

    async fn fresh_state() -> AppState {
        let tmp = tempfile::tempdir().unwrap();
        let mut cfg = crate::config::Config::default();
        cfg.cache.path = tmp.path().join("cache.sqlite");
        cfg.drafts.sqlite_path = tmp.path().join("drafts.sqlite");
        std::mem::forget(tmp);
        crate::bootstrap::ensure_initialized(&cfg).await.unwrap()
    }

    #[tokio::test]
    async fn missing_draft_with_empty_store_returns_soft_recovery() {
        let s = fresh_state().await;
        let out = run(
            &s,
            Input {
                draft_id: "draft_does_not_exist".into(),
            },
        )
        .await
        .expect("not-found path should return Ok, not Err");

        assert!(!out.found, "expected found=false for missing draft");
        assert_eq!(out.draft_id, "draft_does_not_exist");
        assert!(
            out.available_drafts.is_empty(),
            "store is empty so available_drafts must be empty"
        );
        // Empty fields — nothing to show the user.
        assert!(out.preview.is_empty());
        assert!(out.confirmation_prompt.is_empty());
        assert!(out.confirmation_options.is_empty());
        assert!(out.draft_hash.is_empty());
        assert!(out.review_token.is_none());
        // Steering text must guide the model away from inventing IDs and
        // toward telling the user there are no drafts.
        let i = &out.assistant_instructions;
        assert!(
            i.contains("EMPTY"),
            "must call out the empty store, got: {i}"
        );
        assert!(
            i.contains("NEVER"),
            "must forbid retrying with another ID, got: {i}"
        );
        assert!(
            i.contains("start_support_ticket_flow") || i.contains("build_ticket_draft"),
            "must suggest a real entry point, got: {i}"
        );
    }

    #[tokio::test]
    async fn missing_draft_with_existing_drafts_lists_alternatives() {
        let s = fresh_state().await;
        // Stage a real draft so available_drafts is non-empty.
        let mut real = TicketDraft::new();
        real.title = Some("AKS scale-out fails".into());
        real.service_id = Some("Azure Kubernetes Service".into());
        real.severity = Some("moderate".into());
        real.subscription_id = Some("00000000-0000-0000-0000-000000000000".into());
        let real_id = real.draft_id.clone();
        s.drafts.put(real).await.unwrap();

        let out = run(
            &s,
            Input {
                draft_id: "draft_does_not_exist".into(),
            },
        )
        .await
        .expect("not-found path should return Ok, not Err");

        assert!(!out.found);
        assert_eq!(out.draft_id, "draft_does_not_exist");
        assert_eq!(out.available_drafts.len(), 1);
        let summary = &out.available_drafts[0];
        assert_eq!(summary.draft_id, real_id);
        assert_eq!(summary.title.as_deref(), Some("AKS scale-out fails"));
        assert_eq!(
            summary.service_id.as_deref(),
            Some("Azure Kubernetes Service")
        );
        assert_eq!(summary.severity.as_deref(), Some("moderate"));
        let i = &out.assistant_instructions;
        assert!(
            i.contains("available_drafts"),
            "must reference the recovery field, got: {i}"
        );
        assert!(i.contains("NEVER"), "must forbid inventing IDs, got: {i}");
    }
}
