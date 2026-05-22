//! M3.1 — `VisualPromptStore` trait, the per-camera binding shape it
//! returns, and an in-memory test impl. Hoisted out of
//! [`crate::yoloe_visual`] so callers can construct a store-backed
//! impl ([`crate::visual_store_sqlite::StoreBackedVisualPromptStore`])
//! without depending on the `ort` cargo feature — the visual-mode
//! detector consumes the trait but isn't the only consumer.
//!
//! Two impls ship with the crate:
//!
//! * [`InMemoryVisualPromptStore`] — used by tests, fixtures, and
//!   the engine in the rare case where no SQLite store is wired
//!   (smoke runs, CI matrix builds without persistence).
//!
//! * [`crate::visual_store_sqlite::StoreBackedVisualPromptStore`] —
//!   the production path; wraps an `Arc<nexus_store::Store>` and
//!   runs the same `list_camera_visual_prompts` join from the
//!   engine and the inference-worker process.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use nexus_types::CameraId;

use crate::detectors::InferenceError;

/// One operator-attached visual prompt as the detector needs it:
/// a stable label (used directly as `Detection.label`) and the
/// pre-computed image embedding. `Arc<[f32]>` lets the same
/// embedding bind across many cameras without cloning the bytes.
#[derive(Clone, Debug)]
pub struct VisualPromptBinding {
    pub label: String,
    pub embedding: Arc<[f32]>,
}

/// Source of per-camera visual-prompt bindings. The engine
/// implements this against `nexus_store::Store`; the inference
/// worker implements it against a read-only SQLite connection
/// opened from `NEXUS_WORKER_DB_URL` (Phase F).
///
/// The trait is `async` because both impls call into IO; the
/// detector only invokes it from `push_camera_config`, never
/// from the per-frame hot path.
#[async_trait]
pub trait VisualPromptStore: Send + Sync {
    async fn list_for_camera(
        &self,
        camera_id: CameraId,
    ) -> Result<Vec<VisualPromptBinding>, InferenceError>;
}

/// In-memory `VisualPromptStore` used by tests and any caller that
/// wants to inject fixed bindings without an SQLite back-end.
pub struct InMemoryVisualPromptStore {
    inner: parking_lot::RwLock<HashMap<CameraId, Vec<VisualPromptBinding>>>,
}

impl InMemoryVisualPromptStore {
    pub fn new() -> Self {
        Self {
            inner: parking_lot::RwLock::new(HashMap::new()),
        }
    }

    pub fn set(&self, camera_id: CameraId, bindings: Vec<VisualPromptBinding>) {
        self.inner.write().insert(camera_id, bindings);
    }

    pub fn clear(&self, camera_id: CameraId) {
        self.inner.write().remove(&camera_id);
    }
}

impl Default for InMemoryVisualPromptStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl VisualPromptStore for InMemoryVisualPromptStore {
    async fn list_for_camera(
        &self,
        camera_id: CameraId,
    ) -> Result<Vec<VisualPromptBinding>, InferenceError> {
        Ok(self
            .inner
            .read()
            .get(&camera_id)
            .cloned()
            .unwrap_or_default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn binding(label: &str, dim: usize, fill: f32) -> VisualPromptBinding {
        VisualPromptBinding {
            label: label.to_string(),
            embedding: Arc::from(vec![fill; dim].into_boxed_slice()),
        }
    }

    #[tokio::test]
    async fn in_memory_store_returns_set_bindings() {
        let store = InMemoryVisualPromptStore::new();
        let cam: CameraId = 7;
        store.set(cam, vec![binding("hardhat", 8, 0.1)]);
        let got = store.list_for_camera(cam).await.expect("ok");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].label, "hardhat");
        assert_eq!(got[0].embedding.len(), 8);
    }

    #[tokio::test]
    async fn in_memory_store_clear_removes_bindings() {
        let store = InMemoryVisualPromptStore::new();
        let cam: CameraId = 3;
        store.set(cam, vec![binding("a", 4, 0.0)]);
        store.clear(cam);
        let got = store.list_for_camera(cam).await.expect("ok");
        assert!(got.is_empty());
    }
}
