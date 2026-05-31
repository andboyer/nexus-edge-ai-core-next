//! Engine-side glue that turns per-stable-track
//! [`nexus_pipeline::SightingSnapshot`]s into wire `entity_sighting`
//! envelopes.
//!
//! Phase 5.6 · slice 4c-ii.
//!
//! ### Why the buffer + worker pattern?
//!
//! [`nexus_pipeline::SightingHook::submit`] is called on the per-camera
//! supervisor's per-frame hot path. The actual work is heavy:
//!
//! * `crop_and_resize` (~1-3 ms on a 960×540 RGB frame).
//! * `Extractor::extract` (~6-30 ms depending on EP — CPU vs OpenVINO vs CoreML).
//! * `TunnelOutbox::send` (network round-trip on the WSS write side).
//!
//! Doing any of that synchronously would stall the supervisor and
//! cap the camera's effective FPS. Instead, `submit` pushes onto a
//! bounded `tokio::sync::mpsc` channel (cheap — one heap alloc + an
//! `Arc::clone` of the frame) and returns immediately. A dedicated
//! `worker` task drains the channel and runs the extract + publish
//! sequentially. Back-pressure surfaces as a `warn!` log when the
//! channel is full (TrySendError::Full), never as a frame stall.
//!
//! ### Cloud-allowlist gate
//!
//! The cloud's edge-gateway rejects any `embedding_model` not in
//! `('dinov2-s-v1', 'osnet-x1.0-v1')` (see migration `0035` CHECK).
//! When the configured extractor's `model_id` starts with `"mock_"`
//! we treat this as a dev-mode round-trip test and skip the cloud
//! publish entirely (just log at debug). That lets a developer run
//! the engine + cloud-tunnel against a real cloud without polluting
//! `entity_sightings` with rows that don't actually carry a real
//! embedding.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use nexus_cloud_client::{
    cloud_capabilities,
    sink::{
        build_entity_sighting_batch_envelope, build_entity_sighting_envelope_with_dtype,
        EntitySightingProjection,
    },
    TunnelOutbox,
};
use nexus_pipeline::{SightingHook, SightingSnapshot};
use nexus_reid::Extractor;
use parking_lot::Mutex;
use tokio::sync::mpsc;
use tracing::{debug, warn};

/// Phase 5.6 · R7 — observability snapshot for a single camera's
/// re-ID pipeline. One row per `(camera_id)` last touched by the
/// worker. Pure metadata: emit counter (lifetime, since boot),
/// timestamp of the most recent successful emit, and an 8-byte hex
/// prefix of the most recent embedding for "is this actually
/// producing different outputs per identity?" eyeball verification.
///
/// We deliberately DO NOT keep the full embedding here — the whole
/// point of the wedge plan is that embeddings are write-only at the
/// edge. The 8-byte prefix is short enough to never be re-projectable
/// into the source identity but long enough (16 hex chars,
/// 2^64 states) for an operator to see "yes, two consecutive emits
/// for the same track produce nearly-identical hashes; the model
/// isn't randomly hallucinating".
#[derive(Debug, Clone, serde::Serialize)]
pub struct ReidCameraStats {
    /// Lifetime count of successful per-track emits (extract + at
    /// least attempted publish) since engine boot. Reset only on
    /// process restart.
    pub emit_count: u64,
    /// UTC timestamp of the most recent successful emit. `None`
    /// when the worker has never processed a snapshot for this
    /// camera since boot.
    pub last_emit_at: Option<DateTime<Utc>>,
    /// Hex-encoded first 8 bytes of `embedding.vec` interpreted as
    /// little-endian f32 -> raw bytes. 16 chars. Empty when the
    /// worker has never processed a snapshot for this camera since
    /// boot.
    pub last_embedding_hex8: String,
}

impl ReidCameraStats {
    fn new() -> Self {
        Self {
            emit_count: 0,
            last_emit_at: None,
            last_embedding_hex8: String::new(),
        }
    }
}

/// Shared registry of per-camera re-ID stats. Cheap to clone (Arc).
/// Read by the `/v1/admin/reid/status` admin endpoint; written by
/// the worker task on every successful extract.
#[derive(Debug, Default)]
pub struct ReidStatsRegistry {
    inner: Mutex<HashMap<i64, ReidCameraStats>>,
}

impl ReidStatsRegistry {
    /// Build a fresh empty registry. Caller is expected to wrap in
    /// `Arc` so the worker + API state can share it.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Snapshot every per-camera row, sorted by `camera_id` for a
    /// stable wire response. O(n log n) on the camera count (~tens
    /// in practice).
    #[must_use]
    pub fn snapshot(&self) -> Vec<(i64, ReidCameraStats)> {
        let guard = self.inner.lock();
        let mut rows: Vec<(i64, ReidCameraStats)> =
            guard.iter().map(|(k, v)| (*k, v.clone())).collect();
        drop(guard);
        rows.sort_by_key(|(cam, _)| *cam);
        rows
    }

    /// Record a successful emit. Called by the worker AFTER a
    /// successful `extract` returns; happens regardless of whether
    /// the cloud publish itself succeeds (so the operator can tell
    /// "model is running, network is down" from "model isn't even
    /// invoked").
    fn record_emit(&self, camera_id: i64, embedding: &[f32], ts: DateTime<Utc>) {
        let hex8 = embedding_prefix_hex8(embedding);
        let mut guard = self.inner.lock();
        let entry = guard.entry(camera_id).or_insert_with(ReidCameraStats::new);
        entry.emit_count = entry.emit_count.saturating_add(1);
        entry.last_emit_at = Some(ts);
        entry.last_embedding_hex8 = hex8;
    }
}

/// Render the first 8 bytes of a `[f32]` (interpreted as
/// little-endian f32 byte representation) as a 16-char hex string.
/// Returns `""` when the slice is empty.
fn embedding_prefix_hex8(embedding: &[f32]) -> String {
    if embedding.is_empty() {
        return String::new();
    }
    // 2 f32 = 8 bytes. Most extractors return dim >= 384 so this
    // branch is the steady-state path.
    let mut buf = [0u8; 8];
    let mut idx = 0usize;
    for f in embedding.iter().take(2) {
        let bytes = f.to_le_bytes();
        for b in bytes {
            if idx >= 8 {
                break;
            }
            buf[idx] = b;
            idx += 1;
        }
    }
    // Inline 16-char hex render to avoid pulling `hex` as a direct
    // engine dep just for one call site.
    let mut out = String::with_capacity(idx * 2);
    for b in &buf[..idx] {
        use std::fmt::Write as _;
        let _ = write!(out, "{:02x}", b);
    }
    out
}

/// Hook the supervisor calls. Owns a bounded mpsc sender; the
/// matching receiver lives in [`run_worker`].
pub struct CloudEntitySightingHook {
    tx: mpsc::Sender<SightingSnapshot>,
}

impl CloudEntitySightingHook {
    /// Spawn the worker task and return the supervisor-side hook.
    /// `capacity` bounds the per-camera queue depth (default `64`
    /// from the engine boot site is a good starting point — at 5s
    /// cadence per track and ~10 concurrent tracks per camera the
    /// steady-state queue is ~2 messages).
    ///
    /// `stats` is the observability sink wired to the
    /// `/v1/admin/reid/status` admin endpoint. The worker bumps
    /// the per-camera counter on every successful extract — pass
    /// a fresh `Arc::new(ReidStatsRegistry::new())` and hand the
    /// same Arc to `ApiState::reid_stats` so the admin UI can
    /// read it.
    #[must_use]
    pub fn spawn(
        extractor: Arc<dyn Extractor>,
        outbox: Arc<TunnelOutbox>,
        capacity: usize,
        stats: Arc<ReidStatsRegistry>,
    ) -> Self {
        let (tx, rx) = mpsc::channel::<SightingSnapshot>(capacity.max(1));
        tokio::spawn(run_worker(extractor, outbox, rx, stats));
        Self { tx }
    }
}

impl SightingHook for CloudEntitySightingHook {
    fn submit(&self, snapshot: SightingSnapshot) {
        // try_send is the right primitive on the hot path — `send`
        // would await on a full queue and stall the supervisor.
        match self.tx.try_send(snapshot) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(snap)) => {
                warn!(
                    camera_id = snap.camera_id,
                    track_id = snap.track_id,
                    "entity-sighting queue full; dropping snapshot"
                );
            }
            Err(mpsc::error::TrySendError::Closed(snap)) => {
                warn!(
                    camera_id = snap.camera_id,
                    track_id = snap.track_id,
                    "entity-sighting worker gone; dropping snapshot"
                );
            }
        }
    }
}

/// Phase M_PERF_CROWD A3 — max sightings per `entity_sighting_batch`
/// envelope. Matches the wire schema (`items: maxItems: 32`).
const BATCH_MAX: usize = 32;

/// Phase M_PERF_CROWD A3 — drain window for batched mode. Worker
/// blocks for the first snapshot, then opportunistically drains up
/// to `BATCH_MAX-1` more arrivals within this window before flushing.
const BATCH_WINDOW: Duration = Duration::from_millis(100);

async fn run_worker(
    extractor: Arc<dyn Extractor>,
    outbox: Arc<TunnelOutbox>,
    mut rx: mpsc::Receiver<SightingSnapshot>,
    stats: Arc<ReidStatsRegistry>,
) {
    let model_id = extractor.model_id().to_string();
    let dim = extractor.dim();
    // The cloud's edge-gateway CHECK constraint rejects anything
    // outside the allowlist. Mock extractors (default id starts with
    // "mock_") are dev-only — log + drop instead of guaranteeing a
    // 400 from every cloud round-trip.
    let cloud_eligible = !model_id.starts_with("mock_");
    if !cloud_eligible {
        debug!(
            model_id = %model_id,
            "entity-sighting worker running in DEV mode (mock extractor); will run extract for self-test but skip cloud publish"
        );
    }
    while let Some(snapshot) = rx.recv().await {
        let Some(first) = extract_projection(
            &*extractor,
            &model_id,
            dim,
            cloud_eligible,
            &stats,
            snapshot,
        )
        .await
        else {
            continue;
        };
        // Snapshot the cloud's advertised capabilities ONCE per
        // batch — the heartbeat_ack pump updates the outbox set in
        // the background, but checking inside the drain loop would
        // let a mid-batch flip produce a mixed-mode envelope.
        let use_batch = outbox.supports(cloud_capabilities::ENTITY_SIGHTING_BATCH);
        let use_f16 = outbox.supports(cloud_capabilities::EMBEDDING_DTYPE_F16);
        if !use_batch {
            publish_single(&outbox, first, use_f16).await;
            continue;
        }
        let mut buf: Vec<EntitySightingProjection> = Vec::with_capacity(BATCH_MAX);
        buf.push(first);
        let deadline = Instant::now() + BATCH_WINDOW;
        while buf.len() < BATCH_MAX {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            match tokio::time::timeout(remaining, rx.recv()).await {
                Ok(Some(snap)) => {
                    if let Some(p) = extract_projection(
                        &*extractor,
                        &model_id,
                        dim,
                        cloud_eligible,
                        &stats,
                        snap,
                    )
                    .await
                    {
                        buf.push(p);
                    }
                }
                Ok(None) => break, // channel closed
                Err(_) => break,   // drain window elapsed
            }
        }
        publish_batch(&outbox, buf, use_f16).await;
    }
}

async fn publish_single(
    outbox: &TunnelOutbox,
    projection: EntitySightingProjection,
    use_f16: bool,
) {
    let camera_id = projection.camera_id;
    let envelope = build_entity_sighting_envelope_with_dtype(projection, use_f16);
    match outbox.send(envelope).await {
        Ok(()) => debug!(
            camera_id,
            dtype = if use_f16 { "f16" } else { "f32" },
            "entity_sighting envelope published"
        ),
        Err(e) => debug!(
            camera_id,
            error = %e,
            "entity_sighting envelope publish failed (tunnel down?); dropping"
        ),
    }
}

async fn publish_batch(outbox: &TunnelOutbox, items: Vec<EntitySightingProjection>, use_f16: bool) {
    if items.is_empty() {
        return;
    }
    // Singleton batches are wasteful — the unwrap below would also
    // panic the debug_assert in `build_entity_sighting_batch_envelope`
    // for a zero-item input. Fall back to the plain envelope.
    if items.len() == 1 {
        publish_single(outbox, items.into_iter().next().unwrap(), use_f16).await;
        return;
    }
    let count = items.len();
    let envelope = build_entity_sighting_batch_envelope(items, use_f16);
    match outbox.send(envelope).await {
        Ok(()) => debug!(
            count,
            dtype = if use_f16 { "f16" } else { "f32" },
            "entity_sighting_batch envelope published"
        ),
        Err(e) => debug!(
            count,
            error = %e,
            "entity_sighting_batch envelope publish failed (tunnel down?); dropping"
        ),
    }
}

/// Extract a single snapshot into a wire projection. Returns `None`
/// when the snapshot should be dropped (extractor error, dim
/// mismatch, or dev-mode mock extractor). All counter updates and
/// log emissions match the pre-batching behaviour so the admin
/// `/reid/status` semantics are unchanged.
async fn extract_projection(
    extractor: &dyn Extractor,
    model_id: &str,
    dim: usize,
    cloud_eligible: bool,
    stats: &ReidStatsRegistry,
    snapshot: SightingSnapshot,
) -> Option<EntitySightingProjection> {
    let SightingSnapshot {
        camera_id,
        track_id,
        entity_local_id,
        frame,
        bbox,
        confidence,
        started_ts,
        ts,
        is_first,
    } = snapshot;
    let frame_w = frame.width;
    let frame_h = frame.height;
    let embedding = match extractor.extract(&frame, &bbox).await {
        Ok(emb) => emb,
        Err(e) => {
            warn!(
                camera_id,
                track_id,
                error = %e,
                "entity-sighting extractor failed; dropping snapshot"
            );
            return None;
        }
    };
    if embedding.vec.len() != dim {
        warn!(
            camera_id,
            track_id,
            got = embedding.vec.len(),
            want = dim,
            "entity-sighting embedding dimension mismatch; dropping snapshot"
        );
        return None;
    }
    // Phase 5.6 · R7 — record the emit in the shared stats
    // registry BEFORE the publish branch. The admin /reid
    // diagnostic page MUST be able to distinguish "extractor
    // is running, cloud is down" from "extractor isn't even
    // invoked", so we bump the counter even for the dev-mode
    // skip path below.
    stats.record_emit(camera_id, &embedding.vec, ts);
    if !cloud_eligible {
        debug!(
            camera_id,
            track_id,
            model_id = %model_id,
            "entity-sighting extracted (dev mode); skipping cloud publish"
        );
        return None;
    }
    // Saturating casts here: the engine's CameraId is i64 and
    // BBox::{x1,y1,x2,y2} are f32. Negative cam_id never happens
    // in practice (POST /cameras assigns from SQLite rowid which
    // is always > 0), but `as u64` would underflow if it ever
    // did — clamp explicitly so the wire bbox can never carry a
    // surprise huge value.
    Some(EntitySightingProjection {
        camera_id: u64::try_from(camera_id).unwrap_or(0),
        entity_local_id,
        embedding: embedding.vec,
        embedding_model: model_id.to_string(),
        bbox: [
            bbox.x1.max(0.0).round() as i64,
            bbox.y1.max(0.0).round() as i64,
            bbox.width().max(0.0).round() as i64,
            bbox.height().max(0.0).round() as i64,
        ],
        confidence: f64::from(confidence).clamp(0.0, 1.0),
        frame_w: u64::from(frame_w),
        frame_h: u64::from(frame_h),
        started_ts,
        ts,
        is_first_sighting: is_first,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use nexus_reid::MockExtractor;
    use nexus_types::{BBox, Frame, PixelFormat};

    fn dummy_snapshot(camera_id: i64, track_id: u64) -> SightingSnapshot {
        let now = Utc::now();
        SightingSnapshot {
            camera_id,
            track_id,
            entity_local_id: uuid::Uuid::now_v7().to_string(),
            frame: Arc::new(Frame {
                camera_id,
                frame_id: 1,
                captured_at: now,
                width: 960,
                height: 540,
                format: PixelFormat::Rgb24,
                data: Arc::new(vec![64u8; 960 * 540 * 3]),
                trace_id: "test".into(),
            }),
            bbox: BBox {
                x1: 100.0,
                y1: 200.0,
                x2: 250.0,
                y2: 500.0,
            },
            confidence: 0.9,
            started_ts: now,
            ts: now,
            is_first: true,
        }
    }

    /// A mock extractor never produces a real wire envelope. Worker
    /// must accept the snapshot, run the extract (proves the crop
    /// path works end-to-end), then SKIP the cloud publish because
    /// the model_id starts with `"mock_"` (cloud-side CHECK would
    /// reject anyway). Outbox is empty → outbox.send is never
    /// called → no panic on a no-handle outbox.
    #[tokio::test]
    async fn mock_extractor_skips_cloud_publish() {
        let extractor: Arc<dyn Extractor> = Arc::new(MockExtractor::new());
        let outbox = Arc::new(TunnelOutbox::new());
        let stats = Arc::new(ReidStatsRegistry::new());
        let hook = CloudEntitySightingHook::spawn(extractor, outbox.clone(), 8, stats.clone());
        hook.submit(dummy_snapshot(7, 1));
        // Let the worker drain. tokio::time::sleep is fine here —
        // no production code path polls.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        // Nothing observable to assert other than "the test did not
        // hang and did not panic" — the dev-mode skip path is the
        // success case. (A future memory observer wired into the
        // outbox would let us assert send-count==0 explicitly.)
        assert!(!outbox.is_connected(), "outbox stays empty in dev mode");
    }

    /// Phase 5.6 · R7 — even when the dev-mode mock extractor
    /// short-circuits the cloud publish, the stats registry MUST
    /// reflect that the extractor was invoked successfully. The
    /// `/v1/admin/reid/status` page uses this signal to distinguish
    /// "model running, network down" from "model not running at
    /// all".
    #[tokio::test]
    async fn dev_mode_emit_still_records_stats() {
        let extractor: Arc<dyn Extractor> = Arc::new(MockExtractor::new());
        let outbox = Arc::new(TunnelOutbox::new());
        let stats = Arc::new(ReidStatsRegistry::new());
        let hook = CloudEntitySightingHook::spawn(extractor, outbox, 8, stats.clone());
        hook.submit(dummy_snapshot(11, 1));
        hook.submit(dummy_snapshot(11, 2));
        hook.submit(dummy_snapshot(12, 1));
        tokio::time::sleep(std::time::Duration::from_millis(80)).await;
        let snap = stats.snapshot();
        assert_eq!(snap.len(), 2, "two distinct cameras seen");
        let cam11 = snap.iter().find(|(c, _)| *c == 11).unwrap();
        let cam12 = snap.iter().find(|(c, _)| *c == 12).unwrap();
        assert_eq!(cam11.1.emit_count, 2);
        assert_eq!(cam12.1.emit_count, 1);
        assert!(cam11.1.last_emit_at.is_some());
        assert_eq!(cam11.1.last_embedding_hex8.len(), 16);
    }

    #[tokio::test]
    async fn full_queue_does_not_block_submitter() {
        // Capacity=1 so the second submit fills the queue. The
        // worker is held off by a slow extractor (we use a real
        // MockExtractor but submit 5 in a row before yielding).
        let extractor: Arc<dyn Extractor> = Arc::new(MockExtractor::new());
        let outbox = Arc::new(TunnelOutbox::new());
        let stats = Arc::new(ReidStatsRegistry::new());
        let hook = CloudEntitySightingHook::spawn(extractor, outbox, 1, stats);
        // First two get into the channel (sender slot + receiver
        // slot); the next three should hit TrySendError::Full and
        // be dropped without blocking. Test guard: this loop must
        // complete in milliseconds. If submit() were awaiting, this
        // would hang well past any reasonable test timeout.
        let start = std::time::Instant::now();
        for i in 0..50 {
            hook.submit(dummy_snapshot(1, i));
        }
        assert!(
            start.elapsed() < std::time::Duration::from_millis(200),
            "submit must be non-blocking even when the queue is full"
        );
    }

    // --------------------------------------------------------------------
    // Phase M_PERF_CROWD A3 — entity_sighting_batch coverage.
    // --------------------------------------------------------------------

    use async_trait::async_trait;
    use nexus_cloud_client::tunnel::{TunnelError, TunnelHandle};
    use nexus_cloud_protocol::v1::{Envelope, EnvelopeBody};

    struct CapturingTunnel {
        sent: parking_lot::Mutex<Vec<Envelope>>,
    }

    #[async_trait]
    impl TunnelHandle for CapturingTunnel {
        async fn send(&self, envelope: Envelope) -> Result<(), TunnelError> {
            self.sent.lock().push(envelope);
            Ok(())
        }
    }

    /// Force `cloud_eligible = true` by using a non-mock model id —
    /// the worker only batches when the gateway will actually accept
    /// the envelopes.
    fn real_extractor() -> Arc<dyn Extractor> {
        Arc::new(MockExtractor::with_config("dinov2-s-v1", 384))
    }

    fn install(outbox: &TunnelOutbox, caps: &[&str]) -> Arc<CapturingTunnel> {
        let cap = Arc::new(CapturingTunnel {
            sent: parking_lot::Mutex::new(Vec::new()),
        });
        outbox.set_handle(Some(cap.clone() as Arc<dyn TunnelHandle>));
        let owned: Vec<String> = caps.iter().map(|s| (*s).to_string()).collect();
        outbox.update_caps(Some(&owned));
        cap
    }

    /// Phase M_PERF_CROWD A3 — when the cloud advertises
    /// `entity_sighting_batch`, 64 snapshots arriving back-to-back
    /// MUST be flushed as ≤ 4 envelopes (BATCH_MAX = 32), every
    /// non-final envelope MUST be an `EntitySightingBatch`, and every
    /// payload MUST carry `embedding_dtype = Some("f16")` when the
    /// gateway also advertised `embedding_dtype_f16`.
    #[tokio::test]
    async fn batch_envelope_emitted_when_capability_advertised() {
        let outbox = Arc::new(TunnelOutbox::new());
        let cap = install(
            &outbox,
            &[
                cloud_capabilities::ENTITY_SIGHTING_BATCH,
                cloud_capabilities::EMBEDDING_DTYPE_F16,
            ],
        );
        let stats = Arc::new(ReidStatsRegistry::new());
        let hook = CloudEntitySightingHook::spawn(real_extractor(), outbox.clone(), 128, stats);
        for i in 0..64 {
            hook.submit(dummy_snapshot(1, i));
        }
        // Real wall-clock here; one BATCH_WINDOW per envelope at
        // worst, so 64/32 = 2 windows plus a generous fudge for
        // the per-snapshot extract pass.
        tokio::time::sleep(Duration::from_millis(800)).await;
        let sent = cap.sent.lock().clone();
        assert!(
            sent.len() <= 4,
            "batch mode produced {} envelopes for 64 snapshots; expected ≤ 4",
            sent.len()
        );
        let mut total_items = 0usize;
        for env in &sent {
            match &env.body {
                EnvelopeBody::EntitySightingBatch(b) => {
                    assert!(!b.items.is_empty() && b.items.len() <= BATCH_MAX);
                    for item in &b.items {
                        assert_eq!(item.embedding_dtype.as_deref(), Some("f16"));
                    }
                    total_items += b.items.len();
                }
                EnvelopeBody::EntitySighting(p) => {
                    // Permitted only as a trailing singleton.
                    assert_eq!(p.embedding_dtype.as_deref(), Some("f16"));
                    total_items += 1;
                }
                other => panic!("unexpected envelope body: {other:?}"),
            }
        }
        assert_eq!(total_items, 64);
    }

    /// Without the `entity_sighting_batch` capability the worker
    /// MUST fall back to the legacy per-item envelope and MUST NOT
    /// stamp `embedding_dtype = "f16"`.
    #[tokio::test]
    async fn no_batching_when_capability_absent() {
        let outbox = Arc::new(TunnelOutbox::new());
        let cap = install(&outbox, &[]);
        let stats = Arc::new(ReidStatsRegistry::new());
        let hook = CloudEntitySightingHook::spawn(real_extractor(), outbox.clone(), 32, stats);
        for i in 0..4 {
            hook.submit(dummy_snapshot(1, i));
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
        let sent = cap.sent.lock().clone();
        assert_eq!(sent.len(), 4, "legacy mode is one envelope per snapshot");
        for env in &sent {
            match &env.body {
                EnvelopeBody::EntitySighting(p) => {
                    assert!(
                        p.embedding_dtype.is_none(),
                        "FP16 must not be selected without capability"
                    );
                }
                other => panic!("expected EntitySighting, got {other:?}"),
            }
        }
    }
}
