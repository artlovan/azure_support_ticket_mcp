//! `DraftStore` trait + in-memory implementation.
//!
//! `MemoryDraftStore` is process-lifetime; sufficient for a single stdio
//! session. A future SQLite-backed implementation will share the same trait.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::RwLock;

use crate::error::{AppError, AppResult};

use super::draft::TicketDraft;

#[async_trait]
pub trait DraftStore: Send + Sync {
    async fn put(&self, draft: TicketDraft) -> AppResult<()>;
    async fn get(&self, draft_id: &str) -> AppResult<TicketDraft>;
    async fn delete(&self, draft_id: &str) -> AppResult<()>;
    async fn list(&self) -> AppResult<Vec<TicketDraft>>;
}

pub type SharedDraftStore = Arc<dyn DraftStore>;

#[derive(Default)]
pub struct MemoryDraftStore {
    inner: RwLock<HashMap<String, TicketDraft>>,
}

impl MemoryDraftStore {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl DraftStore for MemoryDraftStore {
    async fn put(&self, draft: TicketDraft) -> AppResult<()> {
        self.inner.write().insert(draft.draft_id.clone(), draft);
        Ok(())
    }

    async fn get(&self, draft_id: &str) -> AppResult<TicketDraft> {
        self.inner
            .read()
            .get(draft_id)
            .cloned()
            .ok_or_else(|| AppError::NotFound(format!("draft {draft_id} not found")))
    }

    async fn delete(&self, draft_id: &str) -> AppResult<()> {
        self.inner.write().remove(draft_id);
        Ok(())
    }

    async fn list(&self) -> AppResult<Vec<TicketDraft>> {
        Ok(self.inner.read().values().cloned().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn put_get_delete_roundtrip() {
        let s = MemoryDraftStore::new();
        let d = TicketDraft::new();
        let id = d.draft_id.clone();
        s.put(d).await.unwrap();
        let got = s.get(&id).await.unwrap();
        assert_eq!(got.draft_id, id);
        s.delete(&id).await.unwrap();
        assert!(s.get(&id).await.is_err());
    }
}
