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
//! For M2/M3 the worker will also honor a `$NEXUS_WORKER_MODEL_PATH` for
//! a real ORT session — the wiring shape on this binary is stable.

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
    // M2 will branch on `model_kind` here to build a real ORT-backed
    // Detector. For now every kind degrades to MockDetector — same
    // behavior as `build_detector` in the library for unknown kinds.
    let detector: Arc<dyn Detector> = Arc::new(MockDetector::new());

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
