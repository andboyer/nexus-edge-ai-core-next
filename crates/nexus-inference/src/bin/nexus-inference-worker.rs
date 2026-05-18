//! `nexus-inference-worker` — the out-of-process detector worker spawned by
//! [`WorkerProcessBackend`].
//!
//! Reads length-prefixed bincode [`WorkerRequest`]s from stdin, runs them
//! through the configured [`Detector`], and writes [`WorkerResponse`]s to
//! stdout. Diagnostics go to stderr (the parent forwards them onto its
//! tracing subscriber).
//!
//! ## Crash semantics
//! Any error returned by the detector becomes a `DetectErr` response and
//! the worker keeps serving. Errors on the wire (EOF, decode failure) are
//! terminal — the worker exits non-zero and the parent's restart loop
//! brings it back. That's the failure mode the trait+pool+fail-soft
//! pattern in `docs/ARCHITECTURE.md` is designed around.
//!
//! ## Configuration
//! Picks the model kind from `$NEXUS_WORKER_MODEL_KIND` (default `mock`).
//! For the `yolo` family the worker also reads `$NEXUS_WORKER_MODEL_PATH`
//! (full path to `yolo26n_dynamic.onnx`), `$NEXUS_WORKER_INPUT_W` /
//! `$NEXUS_WORKER_INPUT_H` (default 640), and
//! `$NEXUS_WORKER_SCORE_THRESHOLD` (default 0.30) and lights up the real
//! ORT session when the binary was built with `--features ort,ep-cpu`.
//! Without those features the worker silently falls back to the
//! MockDetector — same shape as `build_detector` in the library.

use std::env;
use std::process::ExitCode;
use std::sync::Arc;

use nexus_inference::detectors::{Detector, MockDetector};
use nexus_inference::worker_proto::{read_msg, write_msg, WorkerRequest, WorkerResponse};
use tokio::io::{stdin, stdout};

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    // Worker logs go to stderr; the parent treats this as opaque
    // diagnostic output. Plain eprintln keeps the binary cheap to build.
    let model_kind = env::var("NEXUS_WORKER_MODEL_KIND").unwrap_or_else(|_| "mock".to_string());

    let detector: Arc<dyn Detector> = build_worker_detector(&model_kind);

    eprintln!(
        "[nexus-inference-worker] ready pid={} model_kind={model_kind}",
        std::process::id()
    );

    let mut stdin = stdin();
    let mut stdout = stdout();

    loop {
        let req: WorkerRequest = match read_msg(&mut stdin).await {
            Ok(r) => r,
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                eprintln!("[nexus-inference-worker] parent closed stdin, exiting");
                return ExitCode::SUCCESS;
            }
            Err(e) => {
                eprintln!("[nexus-inference-worker] wire error: {e}");
                return ExitCode::FAILURE;
            }
        };

        match req {
            WorkerRequest::Detect {
                req_id,
                frame,
                prompts,
            } => {
                let resp = match detector.detect(&frame.into_frame(), &prompts).await {
                    Ok(detections) => WorkerResponse::DetectOk { req_id, detections },
                    Err(e) => WorkerResponse::DetectErr {
                        req_id,
                        message: e.to_string(),
                    },
                };
                if let Err(e) = write_msg(&mut stdout, &resp).await {
                    eprintln!("[nexus-inference-worker] write error: {e}");
                    return ExitCode::FAILURE;
                }
            }
            WorkerRequest::Shutdown => {
                eprintln!("[nexus-inference-worker] shutdown requested");
                return ExitCode::SUCCESS;
            }
        }
    }
}

/// Build the detector requested by `$NEXUS_WORKER_MODEL_KIND`. Mirrors
/// `build_detector` in the library: yolo* → real ORT YoloOrtDetector when
/// the cargo feature is on, otherwise MockDetector with a stderr warning.
/// `yolo_world` / `open_vocab` route through `build_yolo_world_detector`
/// instead, which also reads the prompt vocab from
/// `$NEXUS_WORKER_MODEL_PACK / models-manifest.json`.
fn build_worker_detector(kind: &str) -> Arc<dyn Detector> {
    match kind {
        "yolo" | "yolo26n" | "closed_vocab" => build_yolo_detector(),
        "yolo_world" | "open_vocab" => build_yolo_world_detector(),
        _ => Arc::new(MockDetector::new()),
    }
}

/// Parse `NEXUS_WORKER_EP_PRIORITY` as a comma-separated list of EP
/// names (e.g. `"openvino,cpu"` or `"coreml"`). Empty / unset → CPU
/// fallback only. The engine's `WorkerProcessBackend` sets this env
/// when spawning the worker so the worker registers the same EPs the
/// engine config requested.
#[cfg(feature = "ort")]
fn parse_ep_priority_env() -> Vec<String> {
    env::var("NEXUS_WORKER_EP_PRIORITY")
        .ok()
        .map(|s| {
            s.split(',')
                .map(|p| p.trim().to_string())
                .filter(|p| !p.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(feature = "ort")]
fn build_yolo_detector() -> Arc<dyn Detector> {
    use std::path::PathBuf;
    let path = match env::var("NEXUS_WORKER_MODEL_PATH").map(PathBuf::from) {
        Ok(p) if p.exists() => p,
        Ok(p) => {
            eprintln!(
                "[nexus-inference-worker] $NEXUS_WORKER_MODEL_PATH={} not found, using mock",
                p.display()
            );
            return Arc::new(MockDetector::new());
        }
        Err(_) => {
            eprintln!("[nexus-inference-worker] $NEXUS_WORKER_MODEL_PATH unset, using mock");
            return Arc::new(MockDetector::new());
        }
    };
    let input_w: u32 = env::var("NEXUS_WORKER_INPUT_W")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(640);
    let input_h: u32 = env::var("NEXUS_WORKER_INPUT_H")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(640);
    let score: f32 = env::var("NEXUS_WORKER_SCORE_THRESHOLD")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0.30);
    let ep_priority = parse_ep_priority_env();
    match nexus_inference::yolo::YoloOrtDetector::open(&path, input_w, input_h, score, &ep_priority)
    {
        Ok(d) => Arc::new(d),
        Err(e) => {
            eprintln!("[nexus-inference-worker] yolo open failed: {e}, using mock");
            Arc::new(MockDetector::new())
        }
    }
}

#[cfg(not(feature = "ort"))]
fn build_yolo_detector() -> Arc<dyn Detector> {
    eprintln!("[nexus-inference-worker] ort feature not compiled in, using mock");
    Arc::new(MockDetector::new())
}

#[cfg(feature = "ort")]
fn build_yolo_world_detector() -> Arc<dyn Detector> {
    use std::path::{Path, PathBuf};
    let onnx = match env::var("NEXUS_WORKER_MODEL_PATH").map(PathBuf::from) {
        Ok(p) if p.exists() => p,
        Ok(p) => {
            eprintln!(
                "[nexus-inference-worker] yolo_world $NEXUS_WORKER_MODEL_PATH={} \
                 not found, using mock",
                p.display()
            );
            return Arc::new(MockDetector::new());
        }
        Err(_) => {
            eprintln!(
                "[nexus-inference-worker] yolo_world $NEXUS_WORKER_MODEL_PATH unset, \
                 using mock"
            );
            return Arc::new(MockDetector::new());
        }
    };
    // Manifest sits alongside the ONNX by convention. Operator can override
    // by setting NEXUS_WORKER_MODEL_PACK to the directory.
    let manifest = match env::var("NEXUS_WORKER_MODEL_PACK").map(PathBuf::from) {
        Ok(p) => p.join("models-manifest.json"),
        Err(_) => onnx
            .parent()
            .map(|p| p.join("models-manifest.json"))
            .unwrap_or_else(|| Path::new("models-manifest.json").to_path_buf()),
    };
    if !manifest.exists() {
        eprintln!(
            "[nexus-inference-worker] yolo_world manifest {} not found, using mock",
            manifest.display()
        );
        return Arc::new(MockDetector::new());
    }
    let input_w: u32 = env::var("NEXUS_WORKER_INPUT_W")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(640);
    let input_h: u32 = env::var("NEXUS_WORKER_INPUT_H")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(640);
    let score: f32 = env::var("NEXUS_WORKER_SCORE_THRESHOLD")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0.10);
    let nms_iou: f32 = env::var("NEXUS_WORKER_NMS_IOU")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0.50);
    let model_id =
        env::var("NEXUS_WORKER_MODEL_ID").unwrap_or_else(|_| "yolo_world_v2_s".to_string());
    // Read vocab via the same helper the library uses.
    let vocab =
        match nexus_inference::yolo_world::load_vocab_from_manifest_public(&manifest, &model_id) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("[nexus-inference-worker] yolo_world vocab load failed: {e}, using mock");
                return Arc::new(MockDetector::new());
            }
        };
    match nexus_inference::yolo_world::YoloWorldDetector::open(
        &onnx,
        input_w,
        input_h,
        score,
        nms_iou,
        vocab,
        &parse_ep_priority_env(),
    ) {
        Ok(d) => Arc::new(d),
        Err(e) => {
            eprintln!("[nexus-inference-worker] yolo_world open failed: {e}, using mock");
            Arc::new(MockDetector::new())
        }
    }
}

#[cfg(not(feature = "ort"))]
fn build_yolo_world_detector() -> Arc<dyn Detector> {
    eprintln!("[nexus-inference-worker] ort feature not compiled in, yolo_world using mock");
    Arc::new(MockDetector::new())
}
