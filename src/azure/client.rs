//! Typed ARM REST client.
//!
//! Wraps `reqwest` with auth injection, JSON parsing, retry on transient
//! errors (429/5xx), and structured Azure error extraction. Endpoints are
//! configurable per cloud.

use std::sync::Arc;
use std::time::Duration;

use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION};
use reqwest::{Method, Response, StatusCode};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use tracing::{debug, warn};

use crate::error::{AppError, AppResult};

use super::auth::{AuthProvider, TokenScope};

#[derive(Debug, Clone)]
pub struct ArmEndpoints {
    pub arm: String,
}

impl ArmEndpoints {
    pub fn for_cloud(cloud: &str) -> Self {
        let arm = match cloud {
            "AzureUSGovernment" => "https://management.usgovcloudapi.net",
            "AzureChinaCloud" => "https://management.chinacloudapi.cn",
            _ => "https://management.azure.com",
        }
        .to_string();
        Self { arm }
    }
}

#[derive(Clone)]
pub struct ArmClient {
    http: reqwest::Client,
    endpoints: ArmEndpoints,
    auth: Arc<dyn AuthProvider>,
}

impl ArmClient {
    pub fn new(endpoints: ArmEndpoints, auth: Arc<dyn AuthProvider>) -> AppResult<Self> {
        let http = reqwest::Client::builder()
            .user_agent(concat!(
                "azure-support-ticket-mcp/",
                env!("CARGO_PKG_VERSION")
            ))
            .timeout(Duration::from_secs(30))
            .build()?;
        Ok(Self {
            http,
            endpoints,
            auth,
        })
    }

    /// GET with auth, JSON-decoded.
    pub async fn get_json<T: DeserializeOwned>(&self, path_and_query: &str) -> AppResult<T> {
        let resp = self
            .request_with_retry(Method::GET, path_and_query, None::<()>)
            .await?;
        let body = resp.text().await?;
        serde_json::from_str::<T>(&body).map_err(|e| AppError::Azure {
            message: format!(
                "response decode failed: {e}; body snippet: {}",
                snippet(&body)
            ),
            code: None,
            status: None,
            request_id: None,
            operation_id: None,
        })
    }

    /// GET against an absolute URL (e.g. an `Azure-AsyncOperation` poll URL).
    pub async fn get_json_absolute<T: DeserializeOwned>(&self, url: &str) -> AppResult<T> {
        self.get_json(url).await
    }

    /// PUT a JSON body. Returns `ArmResponse::Sync` for 200/201, or
    /// `ArmResponse::Async` for 202 (carrying the async-op + location headers).
    pub async fn put_json_raw(
        &self,
        path_and_query: &str,
        body: &serde_json::Value,
    ) -> AppResult<ArmResponse> {
        self.body_json_raw(Method::PUT, path_and_query, body).await
    }

    /// PATCH a JSON body. Same response shape as `put_json_raw`.
    pub async fn patch_json_raw(
        &self,
        path_and_query: &str,
        body: &serde_json::Value,
    ) -> AppResult<ArmResponse> {
        self.body_json_raw(Method::PATCH, path_and_query, body)
            .await
    }

    /// POST a JSON body. Same response shape as `put_json_raw`.
    pub async fn post_json_raw(
        &self,
        path_and_query: &str,
        body: &serde_json::Value,
    ) -> AppResult<ArmResponse> {
        self.body_json_raw(Method::POST, path_and_query, body).await
    }

    async fn body_json_raw(
        &self,
        method: Method,
        path_and_query: &str,
        body: &serde_json::Value,
    ) -> AppResult<ArmResponse> {
        let resp = self
            .request_with_retry(method, path_and_query, Some(body))
            .await?;
        let status = resp.status();
        let azure_async_op = resp
            .headers()
            .get("azure-asyncoperation")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let location = resp
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let text = resp.text().await?;
        let value: serde_json::Value = if text.trim().is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::from_str(&text).map_err(|e| AppError::Azure {
                message: format!("response decode failed: {e}; body: {}", snippet(&text)),
                code: None,
                status: Some(status.as_u16()),
                request_id: None,
                operation_id: None,
            })?
        };
        if status == StatusCode::ACCEPTED {
            Ok(ArmResponse::Async {
                azure_async_op,
                location,
                initial_body: value,
            })
        } else {
            Ok(ArmResponse::Sync(value))
        }
    }

    pub async fn request_with_retry<B: Serialize>(
        &self,
        method: Method,
        path_and_query: &str,
        body: Option<B>,
    ) -> AppResult<Response> {
        let url = if path_and_query.starts_with("http") {
            path_and_query.to_string()
        } else {
            format!("{}{}", self.endpoints.arm, path_and_query)
        };

        let token = self.auth.get_token(TokenScope::Arm).await?;
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {}", token.value))
                .map_err(|e| AppError::Internal(format!("bearer header build: {e}")))?,
        );

        // Serialize body once.
        let body_json = match body {
            Some(b) => Some(serde_json::to_vec(&b)?),
            None => None,
        };

        let max_attempts = 4;
        let mut attempt = 0;
        loop {
            attempt += 1;
            let mut req = self
                .http
                .request(method.clone(), &url)
                .headers(headers.clone());
            if let Some(b) = &body_json {
                req = req
                    .header(reqwest::header::CONTENT_TYPE, "application/json")
                    .body(b.clone());
            }
            let resp = match req.send().await {
                Ok(r) => r,
                Err(e) if attempt < max_attempts && e.is_timeout() => {
                    warn!(attempt, "request timeout, retrying");
                    backoff(attempt).await;
                    continue;
                }
                Err(e) => return Err(AppError::Http(e)),
            };

            let status = resp.status();
            if status.is_success() {
                return Ok(resp);
            }
            if attempt < max_attempts && is_retryable(status) {
                warn!(attempt, %status, "retryable status, backing off");
                backoff(attempt).await;
                continue;
            }
            return Err(map_error(resp, status).await);
        }
    }
}

fn is_retryable(status: StatusCode) -> bool {
    status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error()
}

async fn backoff(attempt: usize) {
    let ms = 200u64.saturating_mul(1 << (attempt as u32 - 1)).min(2000);
    tokio::time::sleep(Duration::from_millis(ms)).await;
}

async fn map_error(resp: Response, status: StatusCode) -> AppError {
    let request_id = resp
        .headers()
        .get("x-ms-request-id")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let operation_id = resp
        .headers()
        .get("x-ms-correlation-request-id")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);

    let body = resp.text().await.unwrap_or_default();
    let (code, message) = parse_azure_error(&body).unwrap_or_else(|| (None, snippet(&body)));
    debug!(%status, ?request_id, ?code, ?message, "azure error");
    AppError::Azure {
        message,
        code,
        status: Some(status.as_u16()),
        request_id,
        operation_id,
    }
}

#[derive(Debug, Deserialize)]
pub struct AzureErrorBody {
    pub error: AzureErrorDetail,
}

#[derive(Debug)]
pub enum ArmResponse {
    Sync(serde_json::Value),
    Async {
        azure_async_op: Option<String>,
        location: Option<String>,
        initial_body: serde_json::Value,
    },
}
#[derive(Debug, Deserialize)]
pub struct AzureErrorDetail {
    pub code: Option<String>,
    pub message: Option<String>,
    /// Azure often returns the most actionable info here (e.g.
    /// `JsonDeserializationError: Provide a valid payload...`). Surface
    /// these alongside the top-level message so debugging doesn't require
    /// hitting the same endpoint with `az rest --verbose`.
    #[serde(default)]
    pub details: Vec<AzureErrorDetailItem>,
}

#[derive(Debug, Deserialize)]
pub struct AzureErrorDetailItem {
    #[serde(default)]
    pub code: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
}

/// Parse Azure's standard error body into `(code, message)`. The returned
/// `message` is the top-level message with every non-empty `details[]`
/// entry appended in the form ` | <code>: <message>` — Azure's most
/// actionable diagnostic (e.g. `JsonDeserializationError`) lives in
/// `details`, not in the top-level message.
fn parse_azure_error(body: &str) -> Option<(Option<String>, String)> {
    let parsed: AzureErrorBody = serde_json::from_str(body).ok()?;
    let mut message = parsed.error.message.unwrap_or_else(|| snippet(body));
    for d in parsed.error.details {
        match (d.code.as_deref(), d.message.as_deref()) {
            (Some(c), Some(m)) if !m.is_empty() => message.push_str(&format!(" | {c}: {m}")),
            (None, Some(m)) if !m.is_empty() => message.push_str(&format!(" | {m}")),
            _ => {}
        }
    }
    Some((parsed.error.code, message))
}

fn snippet(s: &str) -> String {
    const N: usize = 240;
    if s.len() <= N {
        s.to_string()
    } else {
        format!("{}…", &s[..N])
    }
}

#[cfg(test)]
mod tests {
    //! High-value test: when Azure returns an error body, the actionable
    //! diagnostic is often in `error.details[]` (e.g.
    //! `JsonDeserializationError: Provide a valid payload...`), not in the
    //! top-level message (which can be a generic "calling client sent a
    //! bad request"). Surfacing the details was the second half of the
    //! patch-shape bug fix — without it, the user only sees Azure's
    //! generic message and can't diagnose what went wrong.
    use super::*;

    #[test]
    fn parse_azure_error_appends_details_entries_to_message() {
        let body = r#"{
          "error": {
            "code": "InvalidParameterValue",
            "message": "The calling client sent a bad request to the service",
            "details": [
              {"code": "JsonDeserializationError",
               "message": "Provide a valid payload for support ticket update operation"}
            ]
          }
        }"#;
        let (code, msg) = parse_azure_error(body).expect("parsed");
        assert_eq!(code.as_deref(), Some("InvalidParameterValue"));
        // Top-level message remains; the details entry appended after `|`.
        assert!(
            msg.contains("The calling client sent a bad request"),
            "top-level message missing: {msg}"
        );
        assert!(
            msg.contains("JsonDeserializationError"),
            "details code missing: {msg}"
        );
        assert!(
            msg.contains("Provide a valid payload"),
            "details message missing: {msg}"
        );
    }
}
