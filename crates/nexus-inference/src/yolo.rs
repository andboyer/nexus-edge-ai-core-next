//! Real ORT-backed YOLO detector.
//!
//! Mirrors the v1 `nexus-edge-ai-core` `YoloDetector` close enough that
//! the same models/yolo26n_<size>.onnx + same COCO→domain label table
//! produces equivalent detections — only the host language changes.
//!
//! Wiring (gated by the `ort` cargo feature):
//!
//!   * Construct: `YoloOrtDetector::open(model_path, input_w, input_h, threshold)?`
//!     loads the ONNX file with `Session::builder().commit_from_file(...)`.
//!     The session is held behind a `parking_lot::Mutex` because
//!     `ort::Session::run` takes `&mut self`. One detector instance per
//!     pool slot keeps lock contention to zero.
//!
//!   * Preprocess: plain bilinear resize of the input frame (currently
//!     RGB24 only) to (input_w × input_h), normalize 0..1, NCHW. v1 uses
//!     `cv::dnn::blobFromImage` which is the same path — no letterbox.
//!     Box coordinates therefore scale by the simple
//!     `(image_dim / input_dim)` factor.
//!
//!   * Postprocess: detect output column count and pick layout:
//!     - 6 cols  → NMS-free Ultralytics export (yolo10/11/26):
//!       `[x1, y1, x2, y2, score, class_id]`
//!     - ≥85     → YOLOv5: `[cx, cy, w, h, objectness, c0..cN]`
//!     - else    → YOLOv8: `[cx, cy, w, h, c0..cN]`
//!
//!     The shipped `yolo26n_<size>.onnx` exports are NMS-free (6 cols),
//!     so that's the hot path.
//!
//!   * Filtering: confidence threshold + COCO→domain label mapping. Every
//!     class id we don't have a domain label for is dropped (v1 does the
//!     same — `mapCocoToDomainLabel` returns "" for unknown ids).

#![cfg(feature = "ort")]
#![allow(unsafe_code)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use ndarray::{s, Array2, Array4, Ix2};
use nexus_config::InferenceConfig;
use nexus_types::{BBox, Detection, Frame, PixelFormat};
use ort::session::{builder::GraphOptimizationLevel, Session};
use ort::value::TensorRef;
use parking_lot::Mutex;
use tracing::{debug, info, warn};

use crate::detectors::{Detector, InferenceError};
use crate::execution_providers;

/// Real ORT-backed YOLO detector.
pub struct YoloOrtDetector {
    session: Mutex<Session>,
    input_w: u32,
    input_h: u32,
    score_threshold: f32,
    /// Cached for diagnostics; not used at inference time.
    _model_path: PathBuf,
}

impl YoloOrtDetector {
    /// Build from a resolved [`InferenceConfig`]. Picks the ONNX artifact
    /// inside `model.pack_path` by matching the configured `input_width`
    /// against the per-size files the engine ships:
    ///
    ///   * 640  → `yolo26n_640.onnx`
    ///   * 960  → `yolo26n_960.onnx`
    ///   * 1280 → `yolo26n_1280.onnx`
    ///
    /// Any other size falls back to whichever per-size file exists, then
    /// to the legacy `yolo26n_dynamic.onnx` (preserved for niche dev
    /// workflows). Returns `ModelLoad` if none of the candidates exist —
    /// we never silently fall back to mock under prod config.
    ///
    /// Note: static-shape ONNX is mandatory for the Intel NPU plugin
    /// (which otherwise silently routes every op to CPU, observed on
    /// k13 Lunar Lake under v0.1.18). Dynamic-shape fallback is the
    /// dev escape hatch, not a prod path.
    pub fn from_config(cfg: &InferenceConfig) -> Result<Self, InferenceError> {
        let pack = cfg.model.pack_path.as_ref().ok_or_else(|| {
            InferenceError::ModelLoad(
                "yolo detector needs inference.model.pack_path; \
                 point it at the directory holding the yolo26n_<size>.onnx files"
                    .into(),
            )
        })?;
        let path = resolve_yolo26n_path(pack, cfg.model.input_width, cfg.model.input_height)?;
        Self::open(
            &path,
            cfg.model.input_width,
            cfg.model.input_height,
            cfg.model.score_threshold,
            &cfg.ep_priority,
        )
    }

    /// Open a session against the given ONNX file. `ep_priority` is
    /// the operator-supplied EP list from `[inference]` in nexus.toml
    /// — see [`crate::execution_providers::selected_for_priority`]
    /// for how it gets translated to ORT dispatchers. Pass `&[]` for
    /// CPU-only (the default fallback path).
    pub fn open(
        model_path: &Path,
        input_w: u32,
        input_h: u32,
        score_threshold: f32,
        ep_priority: &[String],
    ) -> Result<Self, InferenceError> {
        let (eps, ep_names) = execution_providers::selected_for_priority(ep_priority);
        let session = Session::builder()
            .map_err(|e| InferenceError::ModelLoad(format!("session builder: {e}")))?
            .with_optimization_level(GraphOptimizationLevel::Level3)
            .map_err(|e| InferenceError::ModelLoad(format!("opt level: {e}")))?
            .with_execution_providers(eps)
            .map_err(|e| InferenceError::ModelLoad(format!("EP register: {e}")))?
            .commit_from_file(model_path)
            .map_err(|e| {
                InferenceError::ModelLoad(format!("load {}: {e}", model_path.display()))
            })?;
        // ORT 2.0 silently skips an EP that fails to attach at runtime
        // (no .so / no device). `ep_registered` is the list we *asked*
        // the session builder to register; the actual runtime EP per
        // node isn't surfaced through the ort 2.0 Session API — if a
        // silent fallback is suspected, set `RUST_LOG=ort=debug`.
        info!(
            model = %model_path.display(),
            input_w, input_h, score_threshold,
            ep_requested = ?ep_priority,
            ep_registered = ?ep_names,
            "yolo ORT detector ready"
        );
        Ok(Self {
            session: Mutex::new(session),
            input_w,
            input_h,
            score_threshold,
            _model_path: model_path.to_path_buf(),
        })
    }
}

/// Pick the right `yolo26n_*.onnx` inside `pack` for the requested input
/// size. Strategy:
///   1. Try the exact-size file (`yolo26n_<w>.onnx`) — matches what the
///      release ships for 640 / 960 / 1280.
///   2. Try every other ship-size file in order — useful when an operator
///      configures a non-standard size like 416 on a dev box that only
///      has the 640 file staged.
///   3. Fall back to the legacy `yolo26n_dynamic.onnx` — preserved for
///      niche dev workflows but NOT shipped from v0.1.19 onward.
///
/// Returns `ModelLoad` if no candidate exists. The selected path is
/// logged at INFO so operators can see whether the engine actually
/// honored their per-camera size override.
fn resolve_yolo26n_path(
    pack: &Path,
    input_w: u32,
    input_h: u32,
) -> Result<PathBuf, InferenceError> {
    // Square inputs only — the shipped exports are 1×3×N×N. Non-square
    // is a latent dev case; pick the per-width file and let the
    // preprocessor handle the asymmetric stretch (it does anyway).
    let primary = pack.join(format!("yolo26n_{input_w}.onnx"));
    if primary.exists() {
        info!(
            requested_w = input_w,
            requested_h = input_h,
            chosen = %primary.display(),
            "yolo: picked size-matched ONNX"
        );
        return Ok(primary);
    }

    // Try every ship-size file. Useful when the operator picked a
    // non-standard size (e.g. 416 for a memory-tight test) but only the
    // standard set is on disk.
    for sz in [640u32, 960, 1280] {
        let candidate = pack.join(format!("yolo26n_{sz}.onnx"));
        if candidate.exists() {
            warn!(
                requested_w = input_w,
                requested_h = input_h,
                fallback = %candidate.display(),
                "yolo: requested size has no matching ONNX in pack; \
                 falling back to nearest available — preprocessor will \
                 resize to requested input dims"
            );
            return Ok(candidate);
        }
    }

    // Legacy dev fallback. Production releases do NOT include this file.
    let legacy = pack.join("yolo26n_dynamic.onnx");
    if legacy.exists() {
        warn!(
            requested_w = input_w,
            requested_h = input_h,
            legacy = %legacy.display(),
            "yolo: no static-shape ONNX in pack, using deprecated \
             yolo26n_dynamic.onnx — DO NOT ship this in prod; the Intel \
             NPU plugin silently falls back to CPU on dynamic shapes"
        );
        return Ok(legacy);
    }

    Err(InferenceError::ModelLoad(format!(
        "no yolo26n_*.onnx in pack {}; expected yolo26n_{input_w}.onnx \
         (or one of yolo26n_{{640,960,1280}}.onnx)",
        pack.display()
    )))
}

#[async_trait]
impl Detector for YoloOrtDetector {
    async fn detect(
        &self,
        frame: &Frame,
        _prompts: &[String],
    ) -> Result<Vec<Detection>, InferenceError> {
        // Take owned copies of everything we need across the runtime hop.
        let input_w = self.input_w;
        let input_h = self.input_h;
        let frame_w = frame.width;
        let frame_h = frame.height;
        let score_threshold = self.score_threshold;
        let format = frame.format;

        let rgb = match format {
            PixelFormat::Rgb24 => frame.data.as_ref().clone(),
            PixelFormat::Bgr24 => bgr_to_rgb(frame.data.as_ref()),
            other => return Err(InferenceError::UnsupportedFormat(other)),
        };

        // ort sessions are !Sync and `run` takes &mut self, so do the
        // work on a blocking thread and acquire the mutex there.
        let session_for_blocking: &Mutex<Session> = &self.session;
        // SAFETY-equivalent: we hand the reference into a blocking task via
        // `tokio::task::block_in_place`, which keeps us on the same thread
        // (no Send required) — this is the cheapest way to call &mut from
        // async without restructuring the Detector trait.
        tokio::task::block_in_place(|| {
            let mut sess = session_for_blocking.lock();
            run_yolo(
                &mut sess,
                &rgb,
                frame_w,
                frame_h,
                input_w,
                input_h,
                score_threshold,
            )
        })
    }

    fn name(&self) -> &'static str {
        "yolo_ort"
    }
}

fn run_yolo(
    session: &mut Session,
    rgb: &[u8],
    frame_w: u32,
    frame_h: u32,
    input_w: u32,
    input_h: u32,
    score_threshold: f32,
) -> Result<Vec<Detection>, InferenceError> {
    // Preprocess: bilinear resize → NCHW float32 / 255.0.
    let nchw = preprocess_nchw(rgb, frame_w, frame_h, input_w, input_h)?;
    let input = TensorRef::from_array_view(nchw.view())
        .map_err(|e| InferenceError::Failed(format!("tensor wrap: {e}")))?;

    let outputs = session
        .run(ort::inputs![input])
        .map_err(|e| InferenceError::Failed(format!("session run: {e}")))?;

    // First output only — yolo26n exports a single det tensor.
    let (_name, value) = outputs
        .iter()
        .next()
        .ok_or_else(|| InferenceError::Failed("no outputs".into()))?;
    let view = value
        .try_extract_array::<f32>()
        .map_err(|e| InferenceError::Failed(format!("extract array: {e}")))?;

    let shape: Vec<usize> = view.shape().to_vec();
    // Normalize to (rows, cols) regardless of export orientation.
    let pred: Array2<f32> = match shape.len() {
        3 => {
            // [batch, dim1, dim2] — keep batch=0.
            let dim1 = shape[1];
            let dim2 = shape[2];
            // Ultralytics historically exports (1, 84, N) for v8 and
            // (1, N, 6) for the NMS-free yolo26 head. Treat the longer
            // axis as the row axis.
            if dim1 >= dim2 {
                view.slice(s![0, .., ..])
                    .to_owned()
                    .into_dimensionality::<Ix2>()
                    .ok()
            } else {
                view.slice(s![0, .., ..])
                    .to_owned()
                    .reversed_axes()
                    .into_dimensionality::<Ix2>()
                    .ok()
            }
        }
        2 => view.to_owned().into_dimensionality::<Ix2>().ok(),
        _ => None,
    }
    .ok_or_else(|| {
        InferenceError::Failed(format!(
            "unexpected output shape {:?} (want 2-D or 3-D)",
            shape
        ))
    })?;

    let (rows, cols) = pred.dim();
    if rows == 0 || cols < 6 {
        return Ok(Vec::new());
    }

    let nms_free = cols == 6;
    let yolov5_layout = cols >= 85;

    let x_scale = frame_w as f32 / input_w as f32;
    let y_scale = frame_h as f32 / input_h as f32;

    let mut out: Vec<Detection> = Vec::with_capacity(16);
    for r in 0..rows {
        let row = pred.slice(s![r, ..]);
        let (class_id, score, x1, y1, x2, y2) = if nms_free {
            // [x1, y1, x2, y2, score, class_id] in input-image space.
            let s = row[4];
            if s < score_threshold {
                continue;
            }
            (row[5] as i32, s, row[0], row[1], row[2], row[3])
        } else {
            let cx = row[0];
            let cy = row[1];
            let w = row[2];
            let h = row[3];
            let (class_id, score) = if yolov5_layout {
                let objectness = row[4];
                if objectness <= 0.0 {
                    continue;
                }
                let mut best_class = 0.0f32;
                let mut best_id: i32 = -1;
                for c in 5..cols {
                    let v = row[c];
                    if v > best_class {
                        best_class = v;
                        best_id = (c - 5) as i32;
                    }
                }
                (best_id, objectness * best_class)
            } else {
                let mut best_class = 0.0f32;
                let mut best_id: i32 = -1;
                for c in 4..cols {
                    let v = row[c];
                    if v > best_class {
                        best_class = v;
                        best_id = (c - 4) as i32;
                    }
                }
                (best_id, best_class)
            };
            if score < score_threshold || class_id < 0 {
                continue;
            }
            (
                class_id,
                score,
                cx - w * 0.5,
                cy - h * 0.5,
                cx + w * 0.5,
                cy + h * 0.5,
            )
        };

        let label = match map_coco_to_domain_label(class_id) {
            Some(l) => l,
            None => continue,
        };

        let bbox = BBox {
            x1: (x1 * x_scale).max(0.0),
            y1: (y1 * y_scale).max(0.0),
            x2: (x2 * x_scale).min(frame_w as f32),
            y2: (y2 * y_scale).min(frame_h as f32),
        };
        if bbox.width() <= 0.0 || bbox.height() <= 0.0 {
            continue;
        }

        out.push(Detection {
            label: label.into(),
            confidence: score.clamp(0.0, 1.0),
            bbox,
            attributes: Default::default(),
        });
    }

    debug!(out = out.len(), rows, cols, "yolo postprocess done");
    Ok(out)
}

/// Bilinear resize RGB → NCHW float32, in-place into a fresh ndarray.
/// Hot path; do as little allocation as possible, no per-pixel allocs.
fn preprocess_nchw(
    rgb: &[u8],
    src_w: u32,
    src_h: u32,
    dst_w: u32,
    dst_h: u32,
) -> Result<Array4<f32>, InferenceError> {
    if rgb.len() != (src_w as usize) * (src_h as usize) * 3 {
        return Err(InferenceError::Failed(format!(
            "rgb buffer wrong size: got {} expected {}",
            rgb.len(),
            (src_w as usize) * (src_h as usize) * 3
        )));
    }

    let mut tensor = Array4::<f32>::zeros((1, 3, dst_h as usize, dst_w as usize));
    let inv_255 = 1.0f32 / 255.0;
    let sx = src_w as f32 / dst_w as f32;
    let sy = src_h as f32 / dst_h as f32;

    for y in 0..dst_h as usize {
        // Sample center-pixel: src_y = (y + 0.5) * sy - 0.5
        let src_yf = ((y as f32) + 0.5) * sy - 0.5;
        let y0 = src_yf.floor().clamp(0.0, (src_h - 1) as f32) as usize;
        let y1 = (y0 + 1).min(src_h as usize - 1);
        let dy = (src_yf - y0 as f32).clamp(0.0, 1.0);

        for x in 0..dst_w as usize {
            let src_xf = ((x as f32) + 0.5) * sx - 0.5;
            let x0 = src_xf.floor().clamp(0.0, (src_w - 1) as f32) as usize;
            let x1 = (x0 + 1).min(src_w as usize - 1);
            let dx = (src_xf - x0 as f32).clamp(0.0, 1.0);

            // Four-corner indices in the source RGB buffer.
            let stride = src_w as usize * 3;
            let i00 = y0 * stride + x0 * 3;
            let i01 = y0 * stride + x1 * 3;
            let i10 = y1 * stride + x0 * 3;
            let i11 = y1 * stride + x1 * 3;

            for c in 0..3 {
                let v00 = rgb[i00 + c] as f32;
                let v01 = rgb[i01 + c] as f32;
                let v10 = rgb[i10 + c] as f32;
                let v11 = rgb[i11 + c] as f32;
                let v0 = v00 * (1.0 - dx) + v01 * dx;
                let v1 = v10 * (1.0 - dx) + v11 * dx;
                let v = v0 * (1.0 - dy) + v1 * dy;
                tensor[[0, c, y, x]] = v * inv_255;
            }
        }
    }

    Ok(tensor)
}

fn bgr_to_rgb(buf: &[u8]) -> Vec<u8> {
    let mut out = vec![0u8; buf.len()];
    for (i, chunk) in buf.chunks_exact(3).enumerate() {
        let off = i * 3;
        out[off] = chunk[2];
        out[off + 1] = chunk[1];
        out[off + 2] = chunk[0];
    }
    out
}

/// COCO class id → Tier 1 domain label. Mirrors the table in
/// nexus-edge-ai-core/src/inference/pipeline_utils.cpp::mapCocoToDomainLabel.
/// Keep in sync with `models/labels.taxonomy.json` (and the v1 table); if
/// you change one, change both.
fn map_coco_to_domain_label(class_id: i32) -> Option<&'static str> {
    Some(match class_id {
        0 => "person",
        1 => "vehicle.bicycle",
        2 => "vehicle.car",
        3 => "vehicle.motorcycle",
        5 => "vehicle.bus",
        7 => "vehicle.truck",
        14 => "animal.bird",
        15 => "animal.cat",
        16 => "animal.dog",
        24 => "carried.backpack",
        26 => "carried.handbag",
        28 => "carried.suitcase",
        _ => return None,
    })
}

/// Holds an `Arc<dyn Detector>` so the `build` path can swap freely
/// between mock and real ORT impls without a typed cast at the call site.
pub fn build_detector_for_yolo(cfg: &InferenceConfig) -> Result<Arc<dyn Detector>, InferenceError> {
    match YoloOrtDetector::from_config(cfg) {
        Ok(d) => Ok(Arc::new(d)),
        Err(e) => {
            warn!("yolo ORT detector unavailable, falling back to mock: {e}");
            Ok(Arc::new(crate::detectors::MockDetector::new()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preprocess_basic_shape() {
        // 4x4 red image -> 2x2 NCHW
        let rgb: Vec<u8> = (0..(4 * 4)).flat_map(|_| [255u8, 0, 0]).collect();
        let t = preprocess_nchw(&rgb, 4, 4, 2, 2).unwrap();
        assert_eq!(t.shape(), &[1, 3, 2, 2]);
        // Channel 0 (R) should be ~1.0 everywhere.
        for y in 0..2 {
            for x in 0..2 {
                assert!((t[[0, 0, y, x]] - 1.0).abs() < 1e-3);
                assert!(t[[0, 1, y, x]].abs() < 1e-3);
                assert!(t[[0, 2, y, x]].abs() < 1e-3);
            }
        }
    }

    #[test]
    fn coco_table_known_ids() {
        assert_eq!(map_coco_to_domain_label(0), Some("person"));
        assert_eq!(map_coco_to_domain_label(2), Some("vehicle.car"));
        assert_eq!(map_coco_to_domain_label(99), None);
    }
}
