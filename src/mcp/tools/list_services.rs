use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::bootstrap::AppState;
use crate::cache::SupportServiceRow;
use crate::error::AppResult;
use crate::resolver::extractors::parse_resource_id;

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct Input {
    #[serde(default)]
    pub resource_id: Option<String>,
    #[serde(default)]
    pub resource_type: Option<String>,
    #[serde(default)]
    pub keyword: Option<String>,
    #[serde(default = "default_limit")]
    pub limit: usize,
}

fn default_limit() -> usize {
    10
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Output {
    pub candidates: Vec<Candidate>,
    pub total_candidates: usize,
    pub cache_source: &'static str,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Candidate {
    pub service_id: String,
    pub display_name: String,
    pub group: Option<String>,
    pub resource_types: Vec<String>,
    pub confidence: f32,
    pub reason: String,
    pub source: String,
}

pub async fn run(state: &AppState, input: Input) -> AppResult<Output> {
    let resource_type = input
        .resource_id
        .as_deref()
        .and_then(parse_resource_id)
        .map(|p| p.resource_type)
        .or_else(|| input.resource_type.clone());

    let keyword_lc = input.keyword.as_deref().map(|s| s.to_ascii_lowercase());

    let rows = state.cache.list_support_services().await?;
    let mut candidates: Vec<Candidate> = rows
        .into_iter()
        .filter_map(|row| score_row(&row, resource_type.as_deref(), keyword_lc.as_deref()))
        .collect();

    candidates.sort_by(|a, b| {
        b.confidence
            .partial_cmp(&a.confidence)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let total = candidates.len();
    candidates.truncate(input.limit);

    Ok(Output {
        candidates,
        total_candidates: total,
        cache_source: "seed",
    })
}

fn score_row(
    row: &SupportServiceRow,
    resource_type: Option<&str>,
    keyword_lc: Option<&str>,
) -> Option<Candidate> {
    let rts = row.resource_types();
    let mut confidence: f32 = 0.0;
    let mut reasons: Vec<String> = Vec::new();

    if let Some(rt) = resource_type {
        if rts.iter().any(|t| t.eq_ignore_ascii_case(rt)) {
            confidence += 0.85;
            reasons.push(format!("exact resource type match `{rt}`"));
        } else {
            let lhs_prov = rt.split('/').next().unwrap_or("");
            if !lhs_prov.is_empty() && rts.iter().any(|t| t.split('/').next() == Some(lhs_prov)) {
                confidence += 0.45;
                reasons.push(format!("provider namespace match `{lhs_prov}`"));
            }
        }
    }

    if let Some(kw) = keyword_lc {
        let hay = row.display_name.to_ascii_lowercase();
        if hay.contains(kw) {
            confidence += 0.25;
            reasons.push("display name contains keyword".into());
        }
        if let Some(g) = &row.service_group {
            if g.to_ascii_lowercase().contains(kw) {
                confidence += 0.10;
                reasons.push("group matches keyword".into());
            }
        }
    }

    if confidence == 0.0 {
        return None;
    }

    Some(Candidate {
        service_id: row.service_id.clone(),
        display_name: row.display_name.clone(),
        group: row.service_group.clone(),
        resource_types: rts,
        confidence: confidence.min(1.0),
        reason: if reasons.is_empty() {
            "keyword match".into()
        } else {
            reasons.join("; ")
        },
        source: row.source.clone(),
    })
}
