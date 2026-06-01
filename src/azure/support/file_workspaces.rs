//! Microsoft.Support `fileWorkspaces` REST surface.
//!
//! ```text
//! PUT  /fileWorkspaces/{ws}?api-version=...
//! GET  /fileWorkspaces/{ws}?api-version=...
//! GET  /fileWorkspaces/{ws}/files?api-version=...
//! PUT  /fileWorkspaces/{ws}/files/{file}?api-version=...
//! POST /fileWorkspaces/{ws}/files/{file}/upload?api-version=...
//! ```
//!
//! By convention `wsName == supportTicketName`, so a workspace prepared
//! pre-create is reused for the same ticket post-create.

use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use serde_json::{json, Value};

use crate::azure::client::{ArmClient, ArmResponse};
use crate::error::{AppError, AppResult};

use super::services::SUPPORT_API_VERSION;

/// Hard cap per Azure REST docs. We enforce client-side to surface a clean
/// error instead of an opaque 400.
pub const MAX_FILE_BYTES: usize = 5 * 1024 * 1024;
/// Per-call upload cap (matches the portal's "upload max 5 at a time" hint).
pub const MAX_FILES_PER_CALL: usize = 5;
/// Total attachments cap per ticket (matches the portal hint "attach up to
/// 25 files total"). Enforced on add_attachments_to_ticket only — at draft
/// time we don't yet know what's on the workspace.
pub const MAX_FILES_PER_TICKET: usize = 25;
/// Base64 chunk size cap (the Azure spec is in *base64 characters*, not bytes).
pub const MAX_CHUNK_B64_CHARS: usize = 2_500_000; // 2.5 MB worth of base64 chars

/// Create the workspace. Idempotent: if Azure rejects with
/// `ResourceNameExists` (PUT is *not* truly idempotent for this resource —
/// once created, re-PUT returns 400), we transparently fall back to GET so
/// callers can always assume "after this call the workspace exists".
pub async fn create_workspace(arm: &ArmClient, sub_id: &str, ws_name: &str) -> AppResult<Value> {
    let path = format!(
        "/subscriptions/{sub_id}/providers/Microsoft.Support/fileWorkspaces/{ws_name}?api-version={SUPPORT_API_VERSION}"
    );
    match arm.put_json_raw(&path, &json!({})).await {
        Ok(ArmResponse::Sync(v)) => Ok(v),
        Ok(ArmResponse::Async { initial_body, .. }) => Ok(initial_body),
        Err(e) if is_resource_name_exists(&e) => {
            tracing::debug!(workspace = %ws_name, "workspace already exists, fetching");
            get_workspace(arm, sub_id, ws_name).await
        }
        Err(e) => Err(e),
    }
}

/// True if the Azure error reports the resource already exists. Treated as
/// success for idempotent PUTs (workspaces and file metadata).
fn is_resource_name_exists(e: &AppError) -> bool {
    matches!(
        e,
        AppError::Azure { code: Some(c), .. } if c.eq_ignore_ascii_case("ResourceNameExists")
    )
}

pub async fn get_workspace(arm: &ArmClient, sub_id: &str, ws_name: &str) -> AppResult<Value> {
    let path = format!(
        "/subscriptions/{sub_id}/providers/Microsoft.Support/fileWorkspaces/{ws_name}?api-version={SUPPORT_API_VERSION}"
    );
    arm.get_json::<Value>(&path).await
}

pub async fn list_files(arm: &ArmClient, sub_id: &str, ws_name: &str) -> AppResult<Vec<Value>> {
    let path = format!(
        "/subscriptions/{sub_id}/providers/Microsoft.Support/fileWorkspaces/{ws_name}/files?api-version={SUPPORT_API_VERSION}"
    );
    let v: Value = arm.get_json(&path).await?;
    Ok(v.get("value")
        .and_then(|x| x.as_array())
        .cloned()
        .unwrap_or_default())
}

/// PUT file metadata. Must be called before any chunk uploads.
pub async fn create_file(
    arm: &ArmClient,
    sub_id: &str,
    ws_name: &str,
    file_name: &str,
    file_size: usize,
    chunk_size: usize,
    num_chunks: usize,
) -> AppResult<Value> {
    let path = format!(
        "/subscriptions/{sub_id}/providers/Microsoft.Support/fileWorkspaces/{ws_name}/files/{file_name}?api-version={SUPPORT_API_VERSION}"
    );
    let body = json!({
        "properties": {
            "chunkSize": chunk_size,
            "fileSize": file_size,
            "numberOfChunks": num_chunks,
        }
    });
    match arm.put_json_raw(&path, &body).await? {
        ArmResponse::Sync(v) => Ok(v),
        ArmResponse::Async { initial_body, .. } => Ok(initial_body),
    }
}

/// POST a single base64-encoded chunk.
pub async fn upload_chunk(
    arm: &ArmClient,
    sub_id: &str,
    ws_name: &str,
    file_name: &str,
    chunk_index: usize,
    base64_content: &str,
) -> AppResult<()> {
    let path = format!(
        "/subscriptions/{sub_id}/providers/Microsoft.Support/fileWorkspaces/{ws_name}/files/{file_name}/upload?api-version={SUPPORT_API_VERSION}"
    );
    let body = json!({
        "content": base64_content,
        "chunkIndex": chunk_index,
    });
    let _ = arm.post_json_raw(&path, &body).await?;
    Ok(())
}

/// Convenience: split raw bytes into base64 chunks under `MAX_CHUNK_B64_CHARS`.
#[derive(Debug)]
pub struct EncodedFile {
    pub file_size: usize,
    pub chunk_b64: Vec<String>,
}

pub fn encode_for_upload(bytes: &[u8]) -> AppResult<EncodedFile> {
    if bytes.len() > MAX_FILE_BYTES {
        return Err(AppError::Validation(format!(
            "file is {} bytes; max is {} bytes ({} MB)",
            bytes.len(),
            MAX_FILE_BYTES,
            MAX_FILE_BYTES / (1024 * 1024)
        )));
    }
    let b64 = B64.encode(bytes);
    let mut chunks = Vec::new();
    let mut i = 0;
    while i < b64.len() {
        let end = (i + MAX_CHUNK_B64_CHARS).min(b64.len());
        chunks.push(b64[i..end].to_string());
        i = end;
    }
    if chunks.is_empty() {
        // Zero-byte file: still one (empty) chunk so the file is registered.
        chunks.push(String::new());
    }
    Ok(EncodedFile {
        file_size: bytes.len(),
        chunk_b64: chunks,
    })
}

/// One-shot helper: create_file + upload all chunks. Returns the file's
/// metadata body.
pub async fn upload_file(
    arm: &ArmClient,
    sub_id: &str,
    ws_name: &str,
    file_name: &str,
    bytes: &[u8],
) -> AppResult<Value> {
    let encoded = encode_for_upload(bytes)?;
    let chunk_size = if encoded.chunk_b64.is_empty() {
        0
    } else {
        encoded.chunk_b64[0].len()
    };
    let num_chunks = encoded.chunk_b64.len();
    let meta = create_file(
        arm,
        sub_id,
        ws_name,
        file_name,
        encoded.file_size,
        chunk_size,
        num_chunks,
    )
    .await?;
    for (i, c) in encoded.chunk_b64.iter().enumerate() {
        upload_chunk(arm, sub_id, ws_name, file_name, i, c).await?;
    }
    Ok(meta)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_file_is_single_chunk() {
        let bytes = b"hello world";
        let e = encode_for_upload(bytes).unwrap();
        assert_eq!(e.file_size, 11);
        assert_eq!(e.chunk_b64.len(), 1);
    }

    #[test]
    fn rejects_oversized_file() {
        let bytes = vec![0u8; MAX_FILE_BYTES + 1];
        let err = encode_for_upload(&bytes).unwrap_err();
        assert!(err.to_string().contains("max is"));
    }

    #[test]
    fn splits_into_multiple_chunks_at_b64_boundary() {
        // 3 MB of zeros → base64 ≈ 4 MB → split into 2 chunks.
        let bytes = vec![0u8; 3 * 1024 * 1024];
        let e = encode_for_upload(&bytes).unwrap();
        assert!(e.chunk_b64.len() >= 2);
        for c in &e.chunk_b64 {
            assert!(c.len() <= MAX_CHUNK_B64_CHARS);
        }
    }

    #[test]
    fn detects_resource_name_exists_error() {
        let e = AppError::Azure {
            message: "Resource name foo already exists".into(),
            code: Some("ResourceNameExists".into()),
            status: Some(400),
            request_id: None,
            operation_id: None,
        };
        assert!(is_resource_name_exists(&e));

        // Case-insensitive.
        let e2 = AppError::Azure {
            message: "x".into(),
            code: Some("resourcenameexists".into()),
            status: Some(400),
            request_id: None,
            operation_id: None,
        };
        assert!(is_resource_name_exists(&e2));

        // Other codes are not matched.
        let e3 = AppError::Azure {
            message: "x".into(),
            code: Some("SomethingElse".into()),
            status: Some(400),
            request_id: None,
            operation_id: None,
        };
        assert!(!is_resource_name_exists(&e3));

        // Non-Azure errors are not matched.
        assert!(!is_resource_name_exists(&AppError::Validation("x".into())));
    }
}
