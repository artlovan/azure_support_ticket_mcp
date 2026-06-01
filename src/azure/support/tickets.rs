//! `PUT /subscriptions/{sub}/providers/Microsoft.Support/supportTickets/{ticketName}`
//! and friends (LIST, GET, PATCH).
//!
//! Creates a Technical support ticket. Handles both `200` (created
//! synchronously) and `202` (long-running operation with `Azure-AsyncOperation`
//! header to poll).

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::azure::client::{ArmClient, ArmResponse};
use crate::error::{AppError, AppResult};
use crate::workflow::draft::TicketDraft;

use super::services::SUPPORT_API_VERSION;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SupportTicket {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub properties: Option<Value>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CreatedTicket {
    pub ticket_name: String,
    pub status: String,
    pub title: String,
    pub severity: String,
    pub support_ticket_id: Option<String>,
    pub raw: Value,
}

/// Build the ARM body for a Technical support ticket from a draft. The draft
/// must already pass validation.
pub fn build_ticket_body(draft: &TicketDraft) -> Value {
    let c = &draft.contact_details;
    let mut contact = serde_json::json!({
        "firstName": c.first_name,
        "lastName": c.last_name,
        "country": c.country,
        "preferredContactMethod": c.preferred_contact_method,
        "primaryEmailAddress": c.primary_email_address,
        "preferredSupportLanguage": c.preferred_support_language,
        "preferredTimeZone": c.preferred_time_zone,
    });
    if let Some(p) = &c.phone_number {
        contact["phoneNumber"] = Value::String(p.clone());
    }
    if !c.additional_email_addresses.is_empty() {
        contact["additionalEmailAddresses"] = Value::Array(
            c.additional_email_addresses
                .iter()
                .cloned()
                .map(Value::String)
                .collect(),
        );
    }

    let pc_id = draft.problem_classification_id.clone().unwrap_or_default();
    let svc_id = draft.service_id.clone().unwrap_or_default();

    let mut props = serde_json::json!({
        "title": draft.title,
        "description": draft.description,
        "severity": draft.severity,
        "advancedDiagnosticConsent": draft.advanced_diagnostic_consent.clone().unwrap_or_else(|| "No".into()),
        "contactDetails": contact,
        "supportTicketId": ticket_id_value(draft),
        "problemClassificationId": pc_id,
        "serviceId": svc_id,
    });
    if let Some(t) = &draft.problem_start_time {
        props["problemStartTime"] = Value::String(t.clone());
    }
    if let Some(r) = &draft
        .technical_ticket_details
        .resource_id
        .clone()
        .or_else(|| draft.resource_id.clone())
    {
        props["technicalTicketDetails"] = serde_json::json!({ "resourceId": r });
    }
    if let Some(w) = &draft.file_workspace_name {
        props["fileWorkspaceName"] = Value::String(w.clone());
    }
    if let Some(s) = &draft.support_plan_id {
        props["supportPlanId"] = Value::String(s.clone());
    }
    if let Some(b) = draft.require_24x7_response {
        props["require24x7Response"] = Value::Bool(b);
    }
    serde_json::json!({ "properties": props })
}

/// Stable, unique value used both as the ticket name (URL segment) and as the
/// `supportTicketId` property. Azure accepts UUIDs here.
pub fn generate_ticket_name() -> String {
    Uuid::new_v4().to_string()
}

fn ticket_id_value(draft: &TicketDraft) -> String {
    // We don't carry the chosen ticket name on the draft (it's assigned at
    // submit). The caller passes the chosen name as the URL segment; for the
    // `supportTicketId` property we want a stable string that round-trips,
    // so we derive it from the draft id (it's also unique).
    format!(
        "{}-{}",
        draft.draft_id.trim_start_matches("draft_"),
        crate::cache::now_unix()
    )
}

/// Submit a ticket. Returns the created ticket on success. On 202 we poll the
/// async-operation endpoint up to `max_polls` times with `poll_interval` delay.
pub async fn create_ticket(
    arm: &ArmClient,
    subscription_id: &str,
    ticket_name: &str,
    body: &Value,
    max_polls: u32,
    poll_interval: Duration,
) -> AppResult<CreatedTicket> {
    let path = format!(
        "/subscriptions/{subscription_id}/providers/Microsoft.Support/supportTickets/{ticket_name}?api-version={SUPPORT_API_VERSION}"
    );
    let resp = arm.put_json_raw(&path, body).await?;
    match resp {
        ArmResponse::Sync(value) => Ok(into_created(ticket_name, value)),
        ArmResponse::Async {
            azure_async_op,
            location,
            initial_body,
        } => {
            // Try to poll async-operation first; fall back to location.
            let poll_url = azure_async_op.or(location).ok_or_else(|| {
                AppError::Validation(
                    "Azure returned 202 with no Azure-AsyncOperation or Location header".into(),
                )
            })?;

            for _ in 0..max_polls {
                tokio::time::sleep(poll_interval).await;
                let status: Value = arm.get_json_absolute(&poll_url).await?;
                let s = status
                    .get("status")
                    .and_then(|v| v.as_str())
                    .unwrap_or("InProgress");
                match s.to_ascii_lowercase().as_str() {
                    "succeeded" => {
                        // Re-read the ticket to get the final body.
                        let final_body: Value = arm.get_json(&path).await?;
                        return Ok(into_created(ticket_name, final_body));
                    }
                    "failed" | "canceled" => {
                        return Err(parse_async_failure(s, &status));
                    }
                    _ => continue,
                }
            }
            // Out of polls — return the initial body with InProgress marker.
            Ok(CreatedTicket {
                ticket_name: ticket_name.to_string(),
                status: "InProgress".into(),
                title: initial_body
                    .get("properties")
                    .and_then(|p| p.get("title"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .into(),
                severity: initial_body
                    .get("properties")
                    .and_then(|p| p.get("severity"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .into(),
                support_ticket_id: initial_body
                    .get("properties")
                    .and_then(|p| p.get("supportTicketId"))
                    .and_then(|v| v.as_str())
                    .map(String::from),
                raw: initial_body,
            })
        }
    }
}

fn into_created(ticket_name: &str, value: Value) -> CreatedTicket {
    let props = value.get("properties").cloned().unwrap_or(Value::Null);
    CreatedTicket {
        ticket_name: ticket_name.to_string(),
        status: props
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("Open")
            .into(),
        title: props
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .into(),
        severity: props
            .get("severity")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .into(),
        support_ticket_id: props
            .get("supportTicketId")
            .and_then(|v| v.as_str())
            .map(String::from),
        raw: value,
    }
}

/// Convert a failed/canceled async-operation poll body into an `AppError`
/// with a clean, actionable message. Recognises common Azure Support codes
/// and adds remediation hints.
fn parse_async_failure(state: &str, body: &Value) -> AppError {
    let err = body.get("error");
    let code = err
        .and_then(|e| e.get("code"))
        .and_then(|v| v.as_str())
        .unwrap_or("Unknown");
    let message = err
        .and_then(|e| e.get("message"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let op_id = body
        .get("id")
        .and_then(|v| v.as_str())
        .and_then(|s| s.rsplit('/').next())
        .unwrap_or("");

    let hint = match code {
        "InvalidSupportPlan" => Some(
            "Your Azure subscription's support plan does not allow this severity for technical tickets. \
             Common cause: Internal / free / Developer plans only allow Severity A (critical) for technical issues — \
             lower severities (B/C) require Standard or higher. Re-run with severity=`critical`, or upgrade the support plan."
        ),
        "QuotaExceeded" => Some(
            "You've hit the Azure Support ticket quota for this subscription. Close existing open tickets or contact billing."
        ),
        _ => None,
    };

    let mut msg = format!(
        "Azure refused to create the ticket (async status={state}, code={code}): {message}"
    );
    if !op_id.is_empty() {
        msg.push_str(&format!(" [operation_id={op_id}]"));
    }
    if let Some(h) = hint {
        msg.push_str("\nHint: ");
        msg.push_str(h);
    }
    AppError::Validation(msg)
}

#[cfg(test)]
mod async_failure_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn invalid_support_plan_adds_hint() {
        let body = json!({
            "error": {
                "code": "InvalidSupportPlan",
                "message": "Your support plan type is Internal..."
            },
            "id": "/subscriptions/x/providers/Microsoft.Support/operationsStatus/op-123",
            "status": "Failed"
        });
        let e = parse_async_failure("Failed", &body);
        let s = format!("{e}");
        assert!(s.contains("InvalidSupportPlan"));
        assert!(s.contains("severity=`critical`"));
        assert!(s.contains("operation_id=op-123"));
    }

    #[test]
    fn unknown_code_still_returns_clean_message() {
        let body = json!({
            "error": { "code": "WeirdThing", "message": "boom" },
            "status": "Failed"
        });
        let e = parse_async_failure("Failed", &body);
        let s = format!("{e}");
        assert!(s.contains("WeirdThing"));
        assert!(s.contains("boom"));
        assert!(!s.contains("Hint:"));
    }
}

// ---------------------------------------------------------------------------
// LIST / GET / PATCH
// ---------------------------------------------------------------------------

/// Page of support tickets.
#[derive(Debug, Clone)]
pub struct TicketPage {
    pub tickets: Vec<Value>,
    pub next_link: Option<String>,
}

/// GET supportTickets list (paged).
///
/// If `next_link` is provided, we follow it absolutely (Azure returns full URL
/// with continuation token). Otherwise we issue a fresh request from
/// `$top`/`$filter`.
pub async fn list_tickets(
    arm: &ArmClient,
    sub_id: &str,
    top: Option<u32>,
    filter: Option<&str>,
    next_link: Option<&str>,
) -> AppResult<TicketPage> {
    let value: Value = if let Some(link) = next_link {
        arm.get_json_absolute(link).await?
    } else {
        let mut path = format!(
            "/subscriptions/{sub_id}/providers/Microsoft.Support/supportTickets?api-version={SUPPORT_API_VERSION}"
        );
        if let Some(t) = top {
            path.push_str(&format!("&$top={t}"));
        }
        if let Some(f) = filter {
            // Caller is responsible for OData-safe escaping.
            path.push_str(&format!("&$filter={}", urlencoding::encode(f)));
        }
        arm.get_json(&path).await?
    };
    let tickets = value
        .get("value")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let next_link = value
        .get("nextLink")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    Ok(TicketPage { tickets, next_link })
}

/// GET a single support ticket.
pub async fn get_ticket(arm: &ArmClient, sub_id: &str, ticket_name: &str) -> AppResult<Value> {
    let path = format!(
        "/subscriptions/{sub_id}/providers/Microsoft.Support/supportTickets/{ticket_name}?api-version={SUPPORT_API_VERSION}"
    );
    arm.get_json::<Value>(&path).await
}

/// Allowed PATCH fields per Azure spec: severity, status, advancedDiagnosticConsent,
/// contactDetails, secondaryConsent. Returns the updated ticket body.
/// PATCH a support ticket. Per the Microsoft.Support REST schema, the PATCH
/// body for `supportTickets` takes its fields at the **root** (not nested
/// under a `properties` envelope — that's the shape PUT uses, but PATCH
/// differs). Sending `{"properties":{...}}` to PATCH yields
/// `InvalidParameterValue` / `JsonDeserializationError`; sending `{...}`
/// directly is what Azure accepts.
///
/// Verified empirically: a PATCH of `{"properties":{"status":"closed"}}`
/// against a real ticket returns `JsonDeserializationError`; the same
/// payload without the wrapper returns a meaningful state-related error
/// (e.g. `UpdateOperationDenied` on a ticket that's already closed).
pub async fn patch_ticket(
    arm: &ArmClient,
    sub_id: &str,
    ticket_name: &str,
    patch_props: &Value,
) -> AppResult<Value> {
    let path = format!(
        "/subscriptions/{sub_id}/providers/Microsoft.Support/supportTickets/{ticket_name}?api-version={SUPPORT_API_VERSION}"
    );
    match arm.patch_json_raw(&path, patch_props).await? {
        ArmResponse::Sync(v) => Ok(v),
        ArmResponse::Async { initial_body, .. } => Ok(initial_body),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::draft::TicketDraft;

    #[test]
    fn body_includes_required_keys() {
        let mut d = TicketDraft::new();
        d.service_id = Some("svc".into());
        d.problem_classification_id = Some("pc".into());
        d.title = Some("t".into());
        d.description = Some("d".into());
        d.severity = Some("moderate".into());
        d.advanced_diagnostic_consent = Some("Yes".into());
        d.contact_details.first_name = Some("Ada".into());
        d.contact_details.last_name = Some("L".into());
        d.contact_details.country = Some("USA".into());
        d.contact_details.preferred_contact_method = Some("email".into());
        d.contact_details.preferred_support_language = Some("en-us".into());
        d.contact_details.preferred_time_zone = Some("PST".into());
        d.contact_details.primary_email_address = Some("a@b.com".into());
        d.resource_id = Some("/subscriptions/s/.../x".into());

        let body = build_ticket_body(&d);
        let p = &body["properties"];
        assert_eq!(p["title"], "t");
        assert_eq!(p["severity"], "moderate");
        assert_eq!(p["problemClassificationId"], "pc");
        assert_eq!(p["serviceId"], "svc");
        assert_eq!(
            p["technicalTicketDetails"]["resourceId"],
            "/subscriptions/s/.../x"
        );
        assert_eq!(p["contactDetails"]["primaryEmailAddress"], "a@b.com");
    }
}

#[cfg(test)]
mod patch_shape_tests {
    //! Regression guard: `patch_ticket` MUST send fields at the root of the
    //! request body, not nested under `properties`. Sending
    //! `{"properties":{"status":"closed"}}` to Microsoft.Support's PATCH
    //! returns `JsonDeserializationError`; sending `{"status":"closed"}`
    //! is what Azure actually accepts.
    //!
    //! This single test locks in the correct shape and would have caught
    //! the long-standing bug. Verified empirically against the live ARM
    //! endpoint before this test was written.
    use super::*;
    use crate::azure::auth::{AccessToken, AuthProvider, AuthSource, TokenScope};
    use crate::azure::client::ArmEndpoints;
    use serde_json::json;
    use std::sync::Arc;
    use time::{Duration, OffsetDateTime};
    use wiremock::matchers::{body_json_string, method, path_regex};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    struct DummyAuth;
    #[async_trait::async_trait]
    impl AuthProvider for DummyAuth {
        fn source(&self) -> AuthSource {
            AuthSource::EnvClientSecret
        }
        async fn get_token(&self, _scope: TokenScope) -> AppResult<AccessToken> {
            Ok(AccessToken {
                value: "dummy".into(),
                expires_on: OffsetDateTime::now_utc() + Duration::seconds(3600),
                source: AuthSource::EnvClientSecret,
            })
        }
    }

    #[tokio::test]
    async fn patch_body_has_fields_at_root_not_nested_in_properties() {
        let server = MockServer::start().await;
        // body_json_string matches the EXACT body string — any deviation
        // (e.g. wrapping in {"properties":{...}}) causes the mock to NOT
        // match, and the test fails with a connection-refused-style error.
        // This is exactly the shape the live Azure endpoint accepts.
        Mock::given(method("PATCH"))
            .and(path_regex(
                r"^/subscriptions/sub-x/providers/Microsoft\.Support/supportTickets/tkt-y$",
            ))
            .and(body_json_string(r#"{"status":"closed"}"#))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true})))
            .mount(&server)
            .await;

        let arm = ArmClient::new(ArmEndpoints { arm: server.uri() }, Arc::new(DummyAuth)).unwrap();

        let result = patch_ticket(&arm, "sub-x", "tkt-y", &json!({"status": "closed"})).await;
        assert!(
            result.is_ok(),
            "patch failed (likely sent wrong body shape): {:?}",
            result.err()
        );
    }
}
