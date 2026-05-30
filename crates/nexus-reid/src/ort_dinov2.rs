//! ORT-backed DINOv2-S appearance embedding extractor.
//!
//! Compiled only with `--features ort`. The session is opened once
//! at construction time and reused across calls behind a
//! [`parking_lot::Mutex`] — ORT sessions are not `Sync` so the
//! mutex is mandatory, but ORT also blocks the OS thread on
//! `session.run()` so we wrap the call in
//! [`tokio::task::block_in_place`] to keep the runtime healthy.
//!
//! **No model file ships with this crate.** The ONNX comes from
//! `models/dinov2_s_224.onnx` of the engine's model pack — landing
//! that file (along with the matching `models-manifest.json` entry,
//! the `pack_version` bump, and the `.github/workflows/release.yml`
//! asset list patch) is Phase 5.6 slice 4c. Until then, calling
//! [`DinoV2Extractor::open`] against a non-existent file simply
//! returns [`ExtractorError::ModelLoad`] without panicking, which
//! is exactly what the unit tests in this module exercise.

// Same opt-in as `nexus_inference::yoloe` — `ort::inputs!` expands
// to unsafe blocks. Nothing in this module touches `unsafe` directly.
#![allow(unsafe_code)]

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use ndarray::Array4;
use nexus_inference::execution_providers;
use nexus_types::{BBox, Frame};
use ort::session::{builder::GraphOptimizationLevel, Session};
use ort::value::TensorRef;
use parking_lot::Mutex;
use tracing::{debug, info};

use crate::{
    apply_imagenet_normalize, crop_and_resize, frame_to_rgb_borrowed_or_owned, l2_normalise_mut,
    Embedding, Extractor, ExtractorError,
};

/// DINOv2-S backbone CLS-token extractor.
///
/// Input shape: `[1, 3, 224, 224]` float32 (ImageNet mean/std
/// normalised). Output shape: `[1, 384]` float32 (CLS token). The
/// output is L2-normalised in postprocessing so callers can use plain
/// dot product as cosine similarity.
pub struct DinoV2Extractor {
    session: Mutex<Session>,
    model_id: String,
    input_w: u32,
    input_h: u32,
    expected_dim: usize,
    _model_path: PathBuf,
}

impl DinoV2Extractor {
    /// Open a DINOv2-S ORT session.
    ///
    /// * `model_path` — path to the ONNX, normally
    ///   `models/dinov2_s_224.onnx` resolved from the engine config.
    /// * `model_id` — model id from `models-manifest.json` (carried
    ///   into every [`Embedding::model_id`]). Typically
    ///   `"dinov2_s_224"`.
    /// * `ep_priority` — execution-provider priority list, same
    ///   semantics as the nexus-inference detectors. Pass `&[]` for
    ///   CPU-only.
    pub fn open(
        model_path: &Path,
        model_id: impl Into<String>,
        ep_priority: &[String],
    ) -> Result<Self, ExtractorError> {
        let (eps, ep_names) = execution_providers::selected_for_priority(ep_priority);
        let session = Session::builder()
            .map_err(|e| ExtractorError::ModelLoad(format!("session builder: {e}")))?
            .with_optimization_level(GraphOptimizationLevel::Level3)
            .map_err(|e| ExtractorError::ModelLoad(format!("opt level: {e}")))?
            .with_execution_providers(eps)
            .map_err(|e| ExtractorError::ModelLoad(format!("EP register: {e}")))?
            .commit_from_file(model_path)
            .map_err(|e| {
                ExtractorError::ModelLoad(format!("load {}: {e}", model_path.display()))
            })?;
        let model_id = model_id.into();
        info!(
            model = %model_path.display(),
            model_id = %model_id,
            input_w = 224,
            input_h = 224,
            ep_requested = ?ep_priority,
            ep_registered = ?ep_names,
            "dinov2 ORT extractor ready"
        );
        Ok(Self {
            session: Mutex::new(session),
            model_id,
            input_w: 224,
            input_h: 224,
            expected_dim: 384,
            _model_path: model_path.to_path_buf(),
        })
    }
}

#[async_trait]
impl Extractor for DinoV2Extractor {
    fn model_id(&self) -> &str {
        &self.model_id
    }

    fn dim(&self) -> usize {
        self.expected_dim
    }

    async fn extract(&self, frame: &Frame, bbox: &BBox) -> Result<Embedding, ExtractorError> {
        let rgb = frame_to_rgb_borrowed_or_owned(frame)?;
        let crop = crop_and_resize(
            rgb.as_slice(),
            frame.width,
            frame.height,
            bbox,
            self.input_w,
            self.input_h,
        )?;
        let mut nchw_flat = vec![0f32; 3 * (self.input_w as usize) * (self.input_h as usize)];
        apply_imagenet_normalize(&crop, self.input_w, self.input_h, &mut nchw_flat);
        let nchw = Array4::<f32>::from_shape_vec(
            (1, 3, self.input_h as usize, self.input_w as usize),
            nchw_flat,
        )
        .map_err(|e| ExtractorError::InferenceFailed(format!("shape into ndarray: {e}")))?;

        let model_id = self.model_id.clone();
        let expected_dim = self.expected_dim;
        let session_for_blocking: &Mutex<Session> = &self.session;
        let result = tokio::task::block_in_place(|| -> Result<Vec<f32>, ExtractorError> {
            let mut sess = session_for_blocking.lock();
            run_dinov2(&mut sess, &nchw, expected_dim)
        })?;

        debug!(model_id = %model_id, dim = result.len(), "dinov2 embedding emitted");
        let mut vec = result;
        l2_normalise_mut(&mut vec);
        Ok(Embedding {
            model_id,
            dim: vec.len(),
            vec,
        })
    }
}

/// Single inference step. Public for the integration tests that ship
/// alongside the model in 5.6 4c.
pub fn run_dinov2(
    session: &mut Session,
    nchw: &Array4<f32>,
    expected_dim: usize,
) -> Result<Vec<f32>, ExtractorError> {
    let input = TensorRef::from_array_view(nchw.view())
        .map_err(|e| ExtractorError::InferenceFailed(format!("tensor wrap: {e}")))?;
    let outputs = session
        .run(ort::inputs![input])
        .map_err(|e| ExtractorError::InferenceFailed(format!("session run: {e}")))?;

    // DINOv2-S exports a single output named "cls_token" (or
    // "last_hidden_state" depending on export config) of shape [1, 384].
    let (_name, value) = outputs
        .iter()
        .next()
        .ok_or_else(|| ExtractorError::InferenceFailed("no outputs".into()))?;
    let view = value
        .try_extract_array::<f32>()
        .map_err(|e| ExtractorError::InferenceFailed(format!("extract array: {e}")))?;
    let shape: Vec<usize> = view.shape().to_vec();

    // Acceptable output shapes:
    //   [1, 384]                       — bare CLS token (preferred)
    //   [1, N, 384] where N=patch_count — last_hidden_state; CLS is index 0
    let v: Vec<f32> = match shape.as_slice() {
        [1, d] if *d == expected_dim => view.iter().copied().collect(),
        [1, _patches, d] if *d == expected_dim => {
            // Grab CLS token (first patch slot).
            view.iter().take(expected_dim).copied().collect()
        }
        other => {
            return Err(ExtractorError::UnexpectedDim {
                got: other.iter().product(),
                expected: expected_dim,
            });
        }
    };

    if v.len() != expected_dim {
        return Err(ExtractorError::UnexpectedDim {
            got: v.len(),
            expected: expected_dim,
        });
    }
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn open_missing_model_returns_model_load_error_not_panic() {
        let bogus = PathBuf::from("/tmp/__nexus_reid_does_not_exist__.onnx");
        let result = DinoV2Extractor::open(&bogus, "dinov2_s_224", &[]);
        match result {
            Err(ExtractorError::ModelLoad(msg)) => {
                assert!(
                    msg.contains("__nexus_reid_does_not_exist__"),
                    "error msg should reference path: {msg}"
                );
            }
            Err(other) => panic!("expected ModelLoad, got {other:?}"),
            Ok(_) => panic!("expected ModelLoad error, got Ok"),
        }
    }
}
