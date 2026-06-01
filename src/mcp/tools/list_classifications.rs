use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::azure::support::classifications::list_classifications;
use crate::bootstrap::AppState;
use crate::cache::{now_unix, ProblemClassificationRow};
use crate::error::AppResult;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct Input {
    pub service_id: String,
    #[serde(default)]
    pub cache_only: bool,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Output {
    pub service_id: String,
    pub classifications: Vec<ClassificationOut>,
    pub source: &'static str,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ClassificationOut {
    pub id: String,
    pub display_name: String,
    pub parent_id: Option<String>,
}

pub async fn run(state: &AppState, input: Input) -> AppResult<Output> {
    let cached = state.cache.list_classifications(&input.service_id).await?;
    if !cached.is_empty() || input.cache_only {
        return Ok(Output {
            service_id: input.service_id,
            classifications: cached.into_iter().map(to_out).collect(),
            source: "cache",
        });
    }

    let (arm, _chain) = super::arm_for(state)?;
    let fetched = list_classifications(&arm, &input.service_id).await?;

    let cloud = state.cache.cloud().to_string();
    let now = now_unix();
    let mut out = Vec::with_capacity(fetched.len());
    for c in fetched {
        let row = ProblemClassificationRow {
            cloud: cloud.clone(),
            service_id: input.service_id.clone(),
            classification_id: c.id.clone(),
            display_name: c
                .properties
                .display_name
                .clone()
                .unwrap_or_else(|| c.name.clone()),
            parent_id: None,
            metadata_json: None,
            updated_at: now,
            etag: None,
        };
        state.cache.upsert_classification(&row).await?;
        out.push(to_out(row));
    }

    Ok(Output {
        service_id: input.service_id,
        classifications: out,
        source: "live",
    })
}

fn to_out(r: ProblemClassificationRow) -> ClassificationOut {
    ClassificationOut {
        id: r.classification_id,
        display_name: r.display_name,
        parent_id: r.parent_id,
    }
}
