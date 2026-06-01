use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::azure::subscriptions::{list_subscriptions, Subscription};
use crate::bootstrap::AppState;
use crate::cache::now_unix;
use crate::error::AppResult;

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct Input {
    #[serde(default)]
    pub tenant_id: Option<String>,
    #[serde(default)]
    pub cache_only: bool,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Output {
    pub subscriptions: Vec<Subscription>,
    pub count: usize,
    pub source: &'static str,
}

pub async fn run(state: &AppState, input: Input) -> AppResult<Output> {
    if input.cache_only {
        let rows = sqlx::query_as::<_, (String, String, String, Option<String>)>(
            "SELECT tenant_id, subscription_id, display_name, state FROM subscriptions
             WHERE (?1 IS NULL OR tenant_id = ?1) ORDER BY display_name",
        )
        .bind(&input.tenant_id)
        .fetch_all(state.cache.pool())
        .await?;
        let subs: Vec<Subscription> = rows
            .into_iter()
            .map(|(tid, sid, name, st)| Subscription {
                subscription_id: sid,
                display_name: name,
                tenant_id: Some(tid),
                state: st,
            })
            .collect();
        let count = subs.len();
        return Ok(Output {
            subscriptions: subs,
            count,
            source: "cache",
        });
    }

    let (arm, _chain) = super::arm_for(state)?;
    let mut subs = list_subscriptions(&arm).await?;
    if let Some(tid) = &input.tenant_id {
        subs.retain(|s| s.tenant_id.as_deref() == Some(tid));
    }

    let now = now_unix();
    for s in &subs {
        sqlx::query(
            "INSERT INTO subscriptions(tenant_id, subscription_id, display_name, state, updated_at)
             VALUES (?, ?, ?, ?, ?)
             ON CONFLICT(tenant_id, subscription_id) DO UPDATE SET
                display_name = excluded.display_name,
                state = excluded.state,
                updated_at = excluded.updated_at",
        )
        .bind(s.tenant_id.as_deref().unwrap_or(""))
        .bind(&s.subscription_id)
        .bind(&s.display_name)
        .bind(&s.state)
        .bind(now)
        .execute(state.cache.pool())
        .await?;
    }

    let count = subs.len();
    Ok(Output {
        subscriptions: subs,
        count,
        source: "live",
    })
}
