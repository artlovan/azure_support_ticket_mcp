//! `resolve_issue_context`: turn free-form user input + optional resource id
//! / portal URL into a deterministic context blob plus ranked service
//! candidates. This is the "smart" front door that lets the client skip ahead.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::bootstrap::AppState;
use crate::error::AppResult;
use crate::resolver::extractors::{parse_portal_url, parse_resource_id};
use crate::resolver::hints::extract_search_hint;
use crate::resolver::resource_search::{search_resources_by_hint, ResourceCandidate};

use super::list_services;

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct Input {
    /// Free-form description of the issue (e.g. "AKS prod-aks won't scale").
    #[serde(default)]
    pub user_input: Option<String>,
    /// Optional ARM resource id.
    #[serde(default)]
    pub resource_id: Option<String>,
    /// Optional Azure portal URL.
    #[serde(default)]
    pub portal_url: Option<String>,
    /// Limit on returned service candidates.
    #[serde(default = "default_limit")]
    pub limit: usize,
}

fn default_limit() -> usize {
    5
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Output {
    pub parsed_resource_id: Option<String>,
    pub resource_type: Option<String>,
    pub resource_name: Option<String>,
    pub subscription_id: Option<String>,
    pub resource_group: Option<String>,
    pub keyword: Option<String>,
    pub service_candidates: list_services::Output,
    /// Tenant + subscription scope the MCP intends to operate in. The client
    /// MUST surface this to the user (e.g. "I'll open the ticket against
    /// tenant <T>, subscription <S> — confirm or switch?") before continuing.
    pub scope: ScopeContext,
    /// Disambiguation prompt the client should render when service_candidates
    /// is ambiguous. Already includes a sentinel `Other — describe it
    /// differently` option so users can refine instead of being forced to
    /// pick from the top-N.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disambiguation: Option<Disambiguation>,
    /// Ranked Azure resource matches when the user typed a free-form name
    /// hint (e.g. `contoso-b2c`, `prod-aks`) without a full ARM ID or portal URL.
    /// Sourced from Azure Resource Graph via a 3-pass query (exact name →
    /// substring name → substring id) so weird-naming cases like B2C
    /// directories (`oncontoso-b2c.onmicrosoft.com`) and DNS zones still surface.
    /// Empty when no hint was extracted or Resource Graph returned nothing.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub resource_candidates: Vec<ResourceCandidateOut>,
    /// Picker UX for `resource_candidates`. Populated whenever there is at
    /// least one candidate OR the user clearly named a resource but none was
    /// found (so the client can offer the "Other — paste ARM ID" sentinel).
    /// Always includes two sentinel options:
    /// - `"none"` — issue is not tied to a specific resource (proceed without).
    /// - `"other"` — user wants to paste a full ARM ID manually.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource_disambiguation: Option<Disambiguation>,
    pub next_steps: Vec<String>,
}

/// Serializable mirror of `resolver::resource_search::ResourceCandidate`,
/// kept in this module to avoid leaking the internal type's JsonSchema
/// derivation requirements across crate boundaries.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ResourceCandidateOut {
    pub id: String,
    pub name: String,
    pub resource_type: String,
    pub resource_group: Option<String>,
    pub subscription_id: Option<String>,
    /// Why this candidate matched, surfaced verbatim so the user can judge
    /// confidence at a glance: `"name exact"`, `"name contains"`, or
    /// `"id contains"`.
    pub match_reason: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ScopeContext {
    /// Where the tenant/subscription came from: `"resource_id"`, `"portal_url"`,
    /// `"cache_single"` (only one cached subscription so we picked it),
    /// `"identity_only"` (we know the tenant from sign-in but no subscription),
    /// or `"unknown"`.
    pub source: &'static str,
    pub tenant_id: Option<String>,
    pub subscription_id: Option<String>,
    pub subscription_display_name: Option<String>,
    /// True when the client MUST ask the user to confirm or switch before
    /// proceeding (e.g. inferred from cache, multiple subscriptions exist, or
    /// nothing was parsed).
    pub needs_user_confirmation: bool,
    /// Short human-readable summary the client can show verbatim.
    pub summary: String,
    /// If there are multiple cached subscriptions, a short preview list (up
    /// to 5) so the client can offer them as alternatives. Empty when scope
    /// is unambiguous.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub other_subscriptions: Vec<SubscriptionRef>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct SubscriptionRef {
    pub tenant_id: String,
    pub subscription_id: String,
    pub display_name: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Disambiguation {
    /// Markdown-rendered question the client can show verbatim.
    pub question: String,
    pub options: Vec<DisambiguationOption>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct DisambiguationOption {
    /// `"service:<service_id>"` for a candidate, `"other"` for the free-form
    /// escape hatch. The client should treat `other` by re-prompting the user
    /// for a fuller description and calling resolve_issue_context again.
    pub kind: String,
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

pub async fn run(state: &AppState, input: Input) -> AppResult<Output> {
    let parsed = input
        .resource_id
        .as_deref()
        .and_then(parse_resource_id)
        .or_else(|| input.portal_url.as_deref().and_then(parse_portal_url));

    let resource_type = parsed.as_ref().map(|p| p.resource_type.clone());
    let resource_name = parsed.as_ref().map(|p| p.name.clone());
    let subscription_id = parsed.as_ref().map(|p| p.subscription_id.clone());
    let resource_group = parsed.as_ref().and_then(|p| p.resource_group.clone());
    let parsed_resource_id = parsed.as_ref().map(|p| {
        if let Some(rg) = &p.resource_group {
            format!(
                "/subscriptions/{}/resourceGroups/{}/providers/{}/{}",
                p.subscription_id, rg, p.resource_type, p.name
            )
        } else {
            input.resource_id.clone().unwrap_or_default()
        }
    });

    let keyword = input
        .user_input
        .as_deref()
        .and_then(extract_top_keyword)
        .map(|s| s.to_string());

    // Delegate to list_relevant_support_services for ranking.
    let candidates = list_services::run(
        state,
        list_services::Input {
            resource_id: input.resource_id.clone(),
            resource_type: resource_type.clone(),
            keyword: keyword.clone(),
            limit: input.limit,
        },
    )
    .await?;

    let mut next_steps = Vec::new();
    if candidates.candidates.is_empty() {
        // Iterate-first rather than escalate-first: model gets concrete
        // alternatives to try before going back to the user.
        next_steps.push(
            "No service matched automatically. Before asking the user: \
             (a) if you have not yet called `resolve_issue_context` with \
             the resource name, do so now — that path queries Resource \
             Graph and often yields a service via the matched resource's \
             type; (b) if the user mentioned an error code or operation \
             name, retry with that as `user_input`; (c) if you've already \
             tried both, ONLY THEN ask the user for a clearer description \
             (one focused question, not many)."
                .into(),
        );
    } else if candidates.candidates.len() == 1 || candidates.candidates[0].confidence >= 0.85 {
        let top = &candidates.candidates[0];
        next_steps.push(format!(
            "Top candidate `{}` (confidence {:.2}). Confirm scope (see `scope`), then call list_problem_classifications with service_id=`{}`.",
            top.display_name, top.confidence, top.service_id
        ));
    } else {
        next_steps.push(
            "Multiple candidates with similar confidence — render `disambiguation.question` to the user. ALWAYS include the `Other — describe it differently` option so they can refine instead of guessing."
                .into(),
        );
    }

    // Build disambiguation prompt whenever the top candidate is ambiguous
    // (more than one and top confidence < 0.85). Always includes an `other`
    // escape hatch.
    let disambiguation = if candidates.candidates.len() >= 2
        && candidates.candidates[0].confidence < 0.85
    {
        let mut options: Vec<DisambiguationOption> = candidates
            .candidates
            .iter()
            .take(input.limit.min(5))
            .map(|c| DisambiguationOption {
                kind: format!("service:{}", c.service_id),
                label: c.display_name.clone(),
                service_id: Some(c.service_id.clone()),
                hint: c.group.clone(),
            })
            .collect();
        options.push(DisambiguationOption {
            kind: "other".into(),
            label: "Other — describe it differently".into(),
            service_id: None,
            hint: Some(
                "Pick this if none of the above matches. The assistant will ask for a fuller description and re-run resolve_issue_context."
                    .into(),
            ),
        });
        let question = if let Some(name) = &resource_name {
            format!(
                "Multiple Azure services could match `{}`. Which one is the ticket about?",
                name
            )
        } else {
            "Multiple Azure services could match. Which one is the ticket about?".into()
        };
        Some(Disambiguation { question, options })
    } else {
        None
    };

    // Resolve tenant + subscription scope. Priority:
    //   1. subscription parsed from resource_id/portal URL → enrich from cache
    //   2. exactly one cached subscription → suggest it but require confirmation
    //   3. tenant known from signed-in identity but no subscription → identity_only
    //   4. nothing → unknown
    let scope = resolve_scope(state, subscription_id.as_deref()).await;
    if scope.needs_user_confirmation {
        next_steps.push(format!(
            "Confirm scope with the user before continuing: {}. They can switch via list_tenants / list_subscriptions.",
            scope.summary
        ));
    }

    // Resource search via Azure Resource Graph (Gates 1 + 2 from the design):
    //   1. Already done above — if parse_resource_id / parse_portal_url
    //      produced a fully-identified `parsed_resource_id`, we have the
    //      resource and skip Resource Graph entirely.
    //   2. Otherwise: extract a quoted-identifier hint from user_input. Only
    //      when a hint is present do we spend the network call. Bare prose
    //      like "my cluster won't scale" has no specific name to search for,
    //      so we don't query.
    // Gate 3 lives inside search_resources_by_hint itself (< 2 chars → empty).
    let (resource_candidates, resource_disambiguation) = if parsed_resource_id.is_some() {
        (Vec::new(), None)
    } else {
        maybe_search_resources(state, &input, &scope, &mut next_steps).await
    };

    Ok(Output {
        parsed_resource_id,
        resource_type,
        resource_name,
        subscription_id,
        resource_group,
        keyword,
        service_candidates: candidates,
        scope,
        disambiguation,
        resource_candidates,
        resource_disambiguation,
        next_steps,
    })
}

async fn resolve_scope(state: &AppState, parsed_sub: Option<&str>) -> ScopeContext {
    // Pull cached subscriptions (best-effort; never fail the resolve call).
    let cached: Vec<(String, String, String)> = sqlx::query_as::<_, (String, String, String)>(
        "SELECT tenant_id, subscription_id, display_name FROM subscriptions ORDER BY display_name",
    )
    .fetch_all(state.cache.pool())
    .await
    .unwrap_or_default();

    if let Some(sub) = parsed_sub {
        let hit = cached.iter().find(|(_, s, _)| s == sub).cloned();
        let (tenant_id, display_name) = match hit {
            Some((t, _, n)) => (Some(t), Some(n)),
            None => (None, None),
        };
        let summary = format!(
            "tenant={} subscription={}{}",
            tenant_id.as_deref().unwrap_or("?"),
            sub,
            display_name
                .as_deref()
                .map(|n| format!(" ({n})"))
                .unwrap_or_default()
        );
        return ScopeContext {
            source: "resource_id",
            tenant_id,
            subscription_id: Some(sub.to_string()),
            subscription_display_name: display_name,
            // Parsed from input — confidence is high, but still ask for an
            // explicit yes so we never silently bill the wrong sub.
            needs_user_confirmation: true,
            summary,
            other_subscriptions: Vec::new(),
        };
    }

    if cached.len() == 1 {
        let (t, s, n) = cached.into_iter().next().unwrap();
        let summary = format!("tenant={t} subscription={s} ({n}) [only cached subscription]");
        return ScopeContext {
            source: "cache_single",
            tenant_id: Some(t),
            subscription_id: Some(s),
            subscription_display_name: Some(n),
            needs_user_confirmation: true,
            summary,
            other_subscriptions: Vec::new(),
        };
    }

    if cached.len() > 1 {
        let preview: Vec<SubscriptionRef> = cached
            .iter()
            .take(5)
            .map(|(t, s, n)| SubscriptionRef {
                tenant_id: t.clone(),
                subscription_id: s.clone(),
                display_name: n.clone(),
            })
            .collect();
        let summary = format!(
            "{} cached subscriptions across tenants — user must pick one before continuing.",
            cached.len()
        );
        return ScopeContext {
            source: "unknown",
            tenant_id: None,
            subscription_id: None,
            subscription_display_name: None,
            needs_user_confirmation: true,
            summary,
            other_subscriptions: preview,
        };
    }

    // Nothing cached — try identity for at least the tenant.
    let tenant_id = if let Ok((_, chain)) = super::arm_for(state) {
        crate::azure::identity::discover(chain.as_ref())
            .await
            .ok()
            .and_then(|id| id.tenant_id)
    } else {
        None
    };
    if let Some(t) = tenant_id {
        return ScopeContext {
            source: "identity_only",
            tenant_id: Some(t.clone()),
            subscription_id: None,
            subscription_display_name: None,
            needs_user_confirmation: true,
            summary: format!(
                "tenant={t}; no cached subscriptions. Call list_subscriptions to populate, then ask the user to pick one."
            ),
            other_subscriptions: Vec::new(),
        };
    }

    ScopeContext {
        source: "unknown",
        tenant_id: None,
        subscription_id: None,
        subscription_display_name: None,
        needs_user_confirmation: true,
        summary: "No tenant or subscription known. Call list_tenants then list_subscriptions, and confirm with the user.".into(),
        other_subscriptions: Vec::new(),
    }
}

/// Very lightweight keyword extractor: longest token that looks like a service
/// or product name (≥4 letters, mostly alphabetic). Good enough for ranking;
/// the LLM client is expected to refine.
fn extract_top_keyword(s: &str) -> Option<&str> {
    s.split(|c: char| !c.is_alphanumeric() && c != '-')
        .filter(|w| {
            w.len() >= 4
                && w.chars().all(|c| c.is_ascii_alphabetic() || c == '-')
                && !STOPWORDS.iter().any(|sw| sw.eq_ignore_ascii_case(w))
        })
        .max_by_key(|w| w.len())
}

const STOPWORDS: &[&str] = &[
    "with", "from", "that", "this", "have", "what", "when", "where", "issue", "issues", "error",
    "errors", "problem", "ticket", "support", "azure", "please", "cannot", "doesnt", "doesn",
    "wont", "wont't", "open", "create", "service", "cluster", "scale",
];

/// Look for a quoted resource-name hint in `input.user_input`. If one is
/// present, run a Resource Graph multi-pass search scoped to the user's
/// current subscription (if known) and build the picker UX. Returns
/// `(candidates, disambiguation)`. Both are empty/`None` when no hint is
/// extractable or the search itself returns nothing AND there's no reason
/// to surface the "Other — paste ARM ID" sentinel.
///
/// Errors from the search are SOFT — we log via `next_steps` and return
/// empty rather than failing the whole `resolve_issue_context` call. The
/// rest of the response (service candidates, scope) is still useful even
/// if Resource Graph is unreachable.
async fn maybe_search_resources(
    state: &AppState,
    input: &Input,
    scope: &ScopeContext,
    next_steps: &mut Vec<String>,
) -> (Vec<ResourceCandidateOut>, Option<Disambiguation>) {
    let Some(hint) = input.user_input.as_deref().and_then(extract_search_hint) else {
        return (Vec::new(), None);
    };

    // Build the ARM client. If auth setup fails (no creds, etc.), degrade
    // gracefully — return the "Other" sentinel so the user can still paste
    // an ARM ID manually.
    let (client, _auth) = match super::arm_for(state) {
        Ok(c) => c,
        Err(e) => {
            next_steps.push(format!(
                "Resource Graph search skipped (auth setup failed: {e}). \
                 Ask the user to paste the full ARM resource ID for `{hint}` \
                 manually, or run `az login` and retry."
            ));
            return (Vec::new(), Some(build_resource_disambiguation(&hint, &[])));
        }
    };

    let subs: Option<Vec<String>> = scope.subscription_id.as_ref().map(|s| vec![s.clone()]);

    let candidates = match search_resources_by_hint(&client, &hint, subs.as_deref()).await {
        Ok(c) => c,
        Err(e) => {
            next_steps.push(format!(
                "Resource Graph search for `{hint}` failed ({e}). \
                 Surface this to the user and offer to either retry, paste \
                 the ARM ID manually (`Other`), or proceed without a \
                 resource (`None of these`)."
            ));
            return (Vec::new(), Some(build_resource_disambiguation(&hint, &[])));
        }
    };

    let out: Vec<ResourceCandidateOut> = candidates
        .iter()
        .map(|c| ResourceCandidateOut {
            id: c.id.clone(),
            name: c.name.clone(),
            resource_type: c.resource_type.clone(),
            resource_group: c.resource_group.clone(),
            subscription_id: c.subscription_id.clone(),
            match_reason: c.match_reason.label().to_string(),
        })
        .collect();

    if candidates.is_empty() {
        // BIG ITERATION HINT: this is the case that historically caused
        // the model to give up and escalate to the user prematurely. Give
        // the model concrete, copy-pasteable alternatives to try BEFORE
        // it falls back to the disambiguation picker or the user.
        let scope_note = match &scope.subscription_id {
            Some(sub) => format!(
                "Current search was scoped to subscription `{sub}`. The resource may live in a different subscription."
            ),
            None => "Current search was unscoped (all accessible subscriptions).".to_string(),
        };
        next_steps.push(format!(
            "Resource Graph returned no matches for `{hint}` — but DON'T \
             give up yet. {scope_note} Try at least ONE of the following \
             before falling back to the user:\n\
             \n\
             1. **Widen the scope.** Call `azure_resource_search` with \
                `query: \"Resources | where name contains '{hint}' | \
                project id, name, type, resourceGroup, subscriptionId | \
                limit 20\"` and `subscriptions: null` to search every \
                subscription.\n\
             2. **Check sub-resources.** If `{hint}` could be a child \
                resource (Azure OpenAI deployment, SQL database, Key Vault \
                secret), it won't appear in Resource Graph at the top \
                level. Ask the user for the PARENT resource (e.g. the \
                Cognitive Services account, the SQL server), then run \
                `resolve_issue_context` with that PARENT name — once the \
                parent is found, you can navigate to the child via the \
                appropriate ARM endpoint.\n\
             3. **Try a different hint.** If the user said \"the prod \
                cluster\", the actual resource might be named \
                `prod-aks-eastus` or similar — ask the user for the exact \
                Azure portal name (NOT a description), then re-run \
                `resolve_issue_context`.\n\
             \n\
             Only after trying option 1 above (the cheapest), surface \
             `resource_disambiguation` to the user. The picker includes \
             `None of these` and `Other — paste ARM ID` sentinels so the \
             user can always proceed if the iteration also fails."
        ));
    } else {
        next_steps.push(format!(
            "Resource Graph returned {n} candidate(s) for `{hint}`. Show the \
             user `resource_disambiguation` verbatim and get a choice (top \
             match first; `None of these` and `Other — paste ARM ID` \
             sentinels are appended automatically). Apply the chosen ARM ID \
             via `build_ticket_draft` with \
             `technical_ticket_details.resource_id`.",
            n = candidates.len()
        ));
    }

    let disambiguation = build_resource_disambiguation(&hint, &candidates);
    (out, Some(disambiguation))
}

/// Build the picker prompt for a resource hint. Always appends two
/// sentinel options: `None of these` (proceed without a resource_id) and
/// `Other — paste ARM ID` (user knows the full ID; have them supply it).
fn build_resource_disambiguation(hint: &str, candidates: &[ResourceCandidate]) -> Disambiguation {
    let mut options: Vec<DisambiguationOption> = candidates
        .iter()
        .map(|c| {
            let location_hint = match (&c.resource_group, &c.subscription_id) {
                (Some(rg), Some(sub)) => Some(format!("rg `{rg}` · sub `{sub}`")),
                (Some(rg), None) => Some(format!("rg `{rg}`")),
                (None, Some(sub)) => Some(format!("sub `{sub}`")),
                (None, None) => None,
            };
            let hint_text = match location_hint {
                Some(loc) => format!("{} · {} · {}", c.resource_type, c.match_reason.label(), loc),
                None => format!("{} · {}", c.resource_type, c.match_reason.label()),
            };
            DisambiguationOption {
                kind: format!("resource:{}", c.id),
                label: c.name.clone(),
                service_id: None,
                hint: Some(hint_text),
            }
        })
        .collect();

    options.push(DisambiguationOption {
        kind: "none".into(),
        label: "None of these — the issue isn't tied to a specific resource".into(),
        service_id: None,
        hint: Some(
            "Proceed without a resource_id. Azure routing may be slower and \
             engineers will likely ask for the resource later."
                .into(),
        ),
    });

    options.push(DisambiguationOption {
        kind: "other".into(),
        label: "Other — let me paste a different ARM ID".into(),
        service_id: None,
        hint: Some(
            "Ask the user for the full ARM resource ID (e.g. \
             /subscriptions/.../resourceGroups/.../providers/.../<name>) \
             and pass it to build_ticket_draft as \
             technical_ticket_details.resource_id."
                .into(),
        ),
    });

    let question = if candidates.is_empty() {
        format!(
            "I searched Azure Resource Graph for `{hint}` but found no \
             matches. How do you want to proceed?"
        )
    } else {
        format!(
            "I found {n} Azure resource(s) matching `{hint}` — which is the \
             ticket about?",
            n = candidates.len()
        )
    };

    Disambiguation { question, options }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyword_prefers_long_meaningful_token() {
        let k = extract_top_keyword("Open ticket: AKS kubernetes scale issue");
        assert_eq!(k, Some("kubernetes"));
    }

    #[test]
    fn keyword_none_for_pure_stopwords() {
        assert_eq!(extract_top_keyword("open ticket please"), None);
    }
}
