//! Integration tests for [`WorkerProcessBackend`].
//!
//! Uses cargo's `CARGO_BIN_EXE_nexus-inference-worker` env var to find the
//! freshly-built worker binary so this test doesn't depend on `cargo
//! install` or the worker being next to the test runner.

use std::path::PathBuf;
use std::sync::Arc;

use nexus_inference::backends::{BackendState, DetectorBackend, WorkerProcessBackend};
use nexus_inference::detectors::Detector;
use nexus_inference::pool::DetectorPool;
use nexus_types::{Frame, PixelFormat};

fn worker_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_nexus-inference-worker"))
}

fn test_frame() -> Frame {
    Frame {
        camera_id: 7,
        frame_id: 1,
        captured_at: chrono::Utc::now(),
        width: 16,
        height: 16,
        format: PixelFormat::Rgb24,
        data: Arc::new(vec![0u8; 16 * 16 * 3]),
        trace_id: "test-trace".into(),
    }
}

#[tokio::test]
async fn worker_process_round_trips_detection() {
    let backend =
        WorkerProcessBackend::start_with_program(0, &worker_bin(), "mock").expect("spawn worker");
    assert_eq!(backend.state(), BackendState::Ready);
    assert_eq!(backend.name(), "worker_process");

    let detections = backend
        .detect(&test_frame(), &[])
        .await
        .expect("detect over wire");
    assert!(
        !detections.is_empty(),
        "MockDetector always emits one person detection"
    );
}

#[tokio::test]
async fn worker_process_handles_multiple_in_flight_requests() {
    let backend = Arc::new(
        WorkerProcessBackend::start_with_program(0, &worker_bin(), "mock").expect("spawn worker"),
    );
    let mut joins = Vec::new();
    for i in 0..16u64 {
        let backend = backend.clone();
        joins.push(tokio::spawn(async move {
            let mut f = test_frame();
            f.frame_id = i;
            backend.detect(&f, &[]).await
        }));
    }
    for j in joins {
        let res = j.await.expect("task join").expect("detect ok");
        assert_eq!(res.len(), 1);
    }
}

#[tokio::test]
async fn pool_routes_through_worker_process_backends() {
    let mut workers: Vec<Arc<dyn DetectorBackend>> = Vec::new();
    for slot in 0..2 {
        let b = WorkerProcessBackend::start_with_program(slot, &worker_bin(), "mock")
            .expect("spawn worker");
        workers.push(Arc::new(b));
    }
    let pool = DetectorPool::new(workers, None);
    // The pool implements Detector; this is exactly what the engine pipeline calls.
    for i in 0..4u64 {
        let mut f = test_frame();
        f.frame_id = i;
        let detections = pool.detect(&f, &[]).await.expect("pool detect");
        assert_eq!(detections.len(), 1);
    }

    // Snapshot must include both workers in the Ready state.
    let snap = pool.snapshot();
    assert_eq!(snap.len(), 2);
    for s in snap {
        assert_eq!(s.state, BackendState::Ready);
        assert_eq!(s.name, "worker_process");
    }
}
