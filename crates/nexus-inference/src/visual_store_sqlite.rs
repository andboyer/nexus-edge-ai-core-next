//! M3.1 Phase F: Store-backed [`VisualPromptStore`] for both the
//! engine and the inference-worker process.
//!
//! Wraps a [`nexus_store::Store`] handle so the visual-prompt
//! detector ([`crate::yoloe_visual::YoloeVisualDetector`]) can pull
//! its per-camera `(label, embedding)` bindings without knowing
//! anything about SQLite.
//!
//! * **Engine**: constructs one of these at boot from the same
//!   `Arc<Store>` it uses for everything else. Lives in `ApiState`
//!   for the admin HTTP handlers and gets passed to
//!   [`crate::router::InferenceRouter::build_with_visual_store`].
//!
//! * **Worker process**: the binary in
//!   `crates/nexus-inference/src/bin/nexus-inference-worker.rs`
//!   opens its own read-only `Store` from `NEXUS_WORKER_DB_URL`
//!   and wraps it the same way.
//!
//! Both impls perform exactly the same query (the join lives in
//! [`nexus_store::Store::list_camera_visual_prompts`]) — code-sharing
//! the wrapper avoids drift.

use std::sync::Arc;

use async_trait::async_trait;
use nexus_store::Store;
use nexus_types::CameraId;
use tracing::warn;

use crate::detectors::InferenceError;
use crate::visual_prompts::{VisualPromptBinding, VisualPromptStore};

/// `VisualPromptStore` impl backed by a real `nexus_store::Store`.
/// The label used in detection output is the prompt's stored `name`
/// — operators pick it at upload time and it never changes.
pub struct StoreBackedVisualPromptStore {
    store: Arc<Store>,
}

impl StoreBackedVisualPromptStore {
    pub fn new(store: Arc<Store>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl VisualPromptStore for StoreBackedVisualPromptStore {
    async fn list_for_camera(
        &self,
        camera_id: CameraId,
    ) -> Result<Vec<VisualPromptBinding>, InferenceError> {
        let rows = self
            .store
            .list_camera_visual_prompts(camera_id)
            .await
            .map_err(|e| {
                // Treat DB errors as soft failures — caller logs &
                // keeps existing bindings.
                InferenceError::Failed(format!(
                    "store.list_camera_visual_prompts({camera_id}): {e}"
                ))
            })?;
        let mut out: Vec<VisualPromptBinding> = Vec::with_capacity(rows.len());
        for (prompt, embedding) in rows {
            let declared = prompt.embedding_dim as usize;
            if embedding.len() != declared {
                warn!(
                    camera = camera_id,
                    prompt_id = prompt.id,
                    got = embedding.len(),
                    declared,
                    "visual_store_sqlite: row's embedding blob length \
                     disagrees with stored embedding_dim; dropping"
                );
                continue;
            }
            out.push(VisualPromptBinding {
                label: prompt.name,
                embedding: Arc::from(embedding.into_boxed_slice()),
            });
        }
        Ok(out)
    }
}
