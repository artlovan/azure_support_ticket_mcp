//! Shared helper: backfill `draft.tenant_id` from `draft.subscription_id`.
//!
//! Sub→tenant is 1:1; ARM exposes it via `GET /subscriptions/{id}`. Call
//! this from any tool that mutates the draft and may surface it to the user
//! (start_flow, build_draft, preview_draft). Best-effort — any failure is
//! logged and ignored (tenant_id is optional on the wire, but always
//! desirable for display).

use crate::bootstrap::AppState;
use crate::workflow::draft::TicketDraft;

/// Populate `draft.tenant_id` if missing. Tries the local subscriptions cache
/// first (instant), then falls back to a direct ARM call and write-through.
pub async fn backfill_tenant(state: &AppState, draft: &mut TicketDraft) {
    if draft.tenant_id.is_some() {
        return;
    }
    let Some(sub_id) = draft.subscription_id.clone() else {
        return;
    };

    // 1. Cache lookup.
    let cached: Result<Option<(String,)>, _> = sqlx::query_as(
        "SELECT tenant_id FROM subscriptions WHERE subscription_id = ?1 AND tenant_id != '' LIMIT 1",
    )
    .bind(&sub_id)
    .fetch_optional(state.cache.pool())
    .await;
    if let Ok(Some((tid,))) = &cached {
        if !tid.is_empty() {
            tracing::debug!(subscription_id = %sub_id, tenant_id = %tid, "tenant from cache");
            draft.tenant_id = Some(tid.clone());
            return;
        }
    }
    if let Err(e) = &cached {
        tracing::warn!(error = %e, "tenant cache lookup failed (non-fatal)");
    }

    // 2. ARM fallback. Requires auth — silently skip if unavailable.
    let arm = match super::arm_for(state) {
        Ok((arm, _chain)) => arm,
        Err(e) => {
            tracing::debug!(error = %e, "no ARM client for tenant backfill; skipping");
            return;
        }
    };
    // 2a. Try the single-sub GET first (cheap, one round trip).
    match crate::azure::subscriptions::get_subscription(&arm, &sub_id).await {
        Ok(s) => {
            if let Some(tid) = s.tenant_id.clone().filter(|t| !t.is_empty()) {
                tracing::info!(subscription_id = %sub_id, tenant_id = %tid, "tenant from ARM get");
                draft.tenant_id = Some(tid.clone());
                write_through(state, &tid, &sub_id, &s.display_name, s.state.as_deref()).await;
                return;
            }
        }
        Err(e) => {
            tracing::debug!(error = %e, subscription_id = %sub_id, "ARM get_subscription failed; will try list")
        }
    }

    // 2b. Fallback: enumerate via `GET /subscriptions`. This endpoint only
    // returns subs the caller can see, so it usually works even when the
    // single-resource GET is denied. Bonus: we populate the entire cache.
    match crate::azure::subscriptions::list_subscriptions(&arm).await {
        Ok(subs) => {
            for s in &subs {
                if let Some(tid) = s.tenant_id.as_deref().filter(|t| !t.is_empty()) {
                    write_through(
                        state,
                        tid,
                        &s.subscription_id,
                        &s.display_name,
                        s.state.as_deref(),
                    )
                    .await;
                    if s.subscription_id == sub_id && draft.tenant_id.is_none() {
                        tracing::info!(subscription_id = %sub_id, tenant_id = %tid, "tenant from ARM list");
                        draft.tenant_id = Some(tid.to_string());
                    }
                }
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, subscription_id = %sub_id, "ARM list_subscriptions fallback failed (non-fatal)")
        }
    }
}

async fn write_through(
    state: &AppState,
    tenant_id: &str,
    subscription_id: &str,
    display_name: &str,
    sub_state: Option<&str>,
) {
    let now = crate::cache::now_unix();
    let _ = sqlx::query(
        "INSERT INTO subscriptions (tenant_id, subscription_id, display_name, state, updated_at)
         VALUES (?, ?, ?, ?, ?)
         ON CONFLICT(tenant_id, subscription_id) DO UPDATE SET
            display_name = excluded.display_name,
            state = excluded.state,
            updated_at = excluded.updated_at",
    )
    .bind(tenant_id)
    .bind(subscription_id)
    .bind(display_name)
    .bind(sub_state.unwrap_or(""))
    .bind(now)
    .execute(state.cache.pool())
    .await;
}
