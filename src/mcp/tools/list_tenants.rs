use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::azure::tenants::{list_tenants, Tenant};
use crate::bootstrap::AppState;
use crate::cache::now_unix;
use crate::error::AppResult;

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct Input {
    #[serde(default)]
    pub cache_only: bool,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Output {
    pub tenants: Vec<Tenant>,
    pub count: usize,
    pub source: &'static str,
}

pub async fn run(state: &AppState, input: Input) -> AppResult<Output> {
    if input.cache_only {
        let rows = sqlx::query_as::<_, (String, Option<String>)>(
            "SELECT tenant_id, display_name FROM tenants ORDER BY tenant_id",
        )
        .fetch_all(state.cache.pool())
        .await?;
        let tenants = rows
            .into_iter()
            .map(|(id, name)| Tenant {
                tenant_id: id,
                display_name: name,
                default_domain: None,
            })
            .collect::<Vec<_>>();
        let count = tenants.len();
        return Ok(Output {
            tenants,
            count,
            source: "cache",
        });
    }

    let (arm, _chain) = super::arm_for(state)?;
    let tenants = list_tenants(&arm).await?;

    let now = now_unix();
    for t in &tenants {
        sqlx::query(
            "INSERT INTO tenants(account_id, tenant_id, display_name, updated_at)
             VALUES (?, ?, ?, ?)
             ON CONFLICT(account_id, tenant_id) DO UPDATE SET
                display_name = excluded.display_name,
                updated_at = excluded.updated_at",
        )
        .bind("_user")
        .bind(&t.tenant_id)
        .bind(&t.display_name)
        .bind(now)
        .execute(state.cache.pool())
        .await?;
    }

    let count = tenants.len();
    Ok(Output {
        tenants,
        count,
        source: "live",
    })
}
