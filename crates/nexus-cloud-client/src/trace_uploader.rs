//! # trace_uploader
//!
//! Phase 1.14a — engine-side OTLP-style trace exporter.
//!
//! Captures `tracing::span!` events into an in-memory ring buffer and
//! periodically ships them, batched, to the edge-gateway over the same
//! mTLS identity the WSS tunnel uses. The gateway forwards to the
//! api-gateway, which encodes each [`Span`] as an Application Insights
//! `RemoteDependencyData` envelope (Phase 1.14b).
//!
//! ## Why not `opentelemetry-otlp`?
//!
//! The workspace lists the crates, but the OTLP-gRPC exporter wants to
//! reach an OTLP collector directly. The cloud architecture routes all
//! edge → cloud HTTP through `edge-gateway` (mTLS-validated, per-core
//! quota, audit-loggable) — no other endpoint is reachable from the
//! edge. So we ship a small JSON batch over the same mTLS identity the
//! WSS tunnel uses; the gateway/api-gateway handle the App Insights
//! wire format on the cloud side, where the connection string is
//! already injected as `APPLICATIONINSIGHTS_CONNECTION_STRING` (see
//! `infra/modules/{api-gateway,edge-gateway}.bicep`).
//!
//! ## Wire format
//!
//! A batch is a single JSON object:
//!
//! ```json
//! {
//!   "core_id": "<uuid>",
//!   "spans": [
//!     {
//!       "trace_id":       "<32 hex>",
//!       "span_id":        "<16 hex>",
//!       "parent_span_id": "<16 hex>",            // optional
//!       "name":           "rpc.dispatch",
//!       "kind":           "internal",            // internal | server | client | producer | consumer
//!       "start_unix_ns":  1700000000000000000,
//!       "end_unix_ns":    1700000000100000000,
//!       "status":         "ok",                  // ok | error
//!       "attributes":     { "rpc.method": "POST", "rpc.path": "/cameras" }
//!     }
//!   ]
//! }
//! ```
//!
//! Empty batches are skipped (no POST). Failures are logged at WARN
//! and dropped; tracing is best-effort and must never block the engine.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tracing::{debug, warn};
use uuid::Uuid;

/// Default batch flush interval. The uploader also flushes whenever the
/// buffer fills, so under load the effective cadence is much faster.
pub const DEFAULT_FLUSH_INTERVAL: Duration = Duration::from_secs(5);

/// Default soft cap on spans buffered before a flush is forced. Set
/// well below the channel capacity so backpressure manifests as flushes
/// rather than dropped spans.
pub const DEFAULT_BATCH_SIZE: usize = 256;

/// Hard cap on the in-memory queue. Beyond this, `push` silently drops
/// (returns `Err`) so a busy engine never grows unbounded memory.
pub const DEFAULT_QUEUE_CAPACITY: usize = 4096;

/// Reasons the uploader may have dropped a span.
#[derive(Debug, Clone, thiserror::Error)]
pub enum TraceUploaderError {
    /// Background task has exited (uploader stopped, channel closed).
    #[error("trace uploader stopped")]
    Stopped,
    /// Queue is full; caller's span was dropped. Best-effort delivery.
    #[error("trace queue full; span dropped")]
    QueueFull,
}

/// W3C-compatible span kind. Mapped to App Insights envelope types in
/// the api-gateway ingester (Phase 1.14b).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SpanKind {
    #[default]
    Internal,
    Server,
    Client,
    Producer,
    Consumer,
}

/// Terminal status of a span.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SpanStatus {
    #[default]
    Ok,
    Error,
}

/// One captured span. Field shape is deliberately a subset of the
/// OpenTelemetry data model — what we need for App Insights without
/// pulling the full OTLP wire format.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Span {
    /// W3C trace-id, 32 hex chars (16 bytes).
    pub trace_id: String,
    /// W3C span-id, 16 hex chars (8 bytes).
    pub span_id: String,
    /// Parent span-id, omitted for root spans.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub parent_span_id: Option<String>,
    /// Operation name (`tracing` span name).
    pub name: String,
    #[serde(default)]
    pub kind: SpanKind,
    /// UNIX time at span start, nanoseconds.
    pub start_unix_ns: u64,
    /// UNIX time at span end, nanoseconds.
    pub end_unix_ns: u64,
    #[serde(default)]
    pub status: SpanStatus,
    /// String-valued attributes. Numeric/bool values are serialised as
    /// strings to keep the wire format trivially round-trippable.
    #[serde(default)]
    pub attributes: HashMap<String, String>,
}

impl Span {
    /// Duration of the span in nanoseconds. Saturates at zero if the
    /// caller passed end < start (clock skew).
    #[must_use]
    pub fn duration_ns(&self) -> u64 {
        self.end_unix_ns.saturating_sub(self.start_unix_ns)
    }

    /// Convenience: span duration in milliseconds (rounded down).
    #[must_use]
    pub fn duration_ms(&self) -> u64 {
        self.duration_ns() / 1_000_000
    }
}

/// Wire-format batch the uploader POSTs to the edge-gateway.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceBatch {
    /// Origin core id (cloud side uses this for partitioning).
    pub core_id: Uuid,
    pub spans: Vec<Span>,
}

/// Configuration handed to [`TraceUploader::spawn`].
#[derive(Debug, Clone)]
pub struct TraceUploaderConfig {
    /// Absolute URL of the edge-gateway trace ingest endpoint,
    /// e.g. `https://edge.nexusedge.ai/v1/edge/traces`.
    pub endpoint_url: String,
    /// Per-core stable id stamped into every batch.
    pub core_id: Uuid,
    /// Soft batch-size trigger.
    pub batch_size: usize,
    /// Wall-clock flush interval (also applies to small/empty buffers,
    /// but empty buffers don't POST).
    pub flush_interval: Duration,
    /// Queue capacity. Pushes beyond this drop with `QueueFull`.
    pub queue_capacity: usize,
}

impl Default for TraceUploaderConfig {
    fn default() -> Self {
        Self {
            endpoint_url: String::new(),
            core_id: Uuid::nil(),
            batch_size: DEFAULT_BATCH_SIZE,
            flush_interval: DEFAULT_FLUSH_INTERVAL,
            queue_capacity: DEFAULT_QUEUE_CAPACITY,
        }
    }
}

/// Pluggable transport so tests can avoid real HTTP. Production wires
/// [`ReqwestMtlsTransport`].
#[async_trait::async_trait]
pub trait BatchTransport: Send + Sync {
    /// Ship one batch. Errors are logged at WARN and the batch is
    /// dropped (no retry — the next batch will simply be larger).
    async fn send_batch(&self, batch: TraceBatch) -> Result<(), String>;
}

/// Production transport: a `reqwest::Client` already configured with the
/// engine's mTLS identity (same cert/key/ca-chain as the WSS tunnel).
pub struct ReqwestMtlsTransport {
    client: reqwest::Client,
    endpoint_url: String,
}

impl ReqwestMtlsTransport {
    /// Build a transport wrapping an existing `reqwest::Client`. The
    /// caller is responsible for configuring mTLS on the client.
    #[must_use]
    pub fn new(client: reqwest::Client, endpoint_url: impl Into<String>) -> Self {
        Self {
            client,
            endpoint_url: endpoint_url.into(),
        }
    }
}

#[async_trait::async_trait]
impl BatchTransport for ReqwestMtlsTransport {
    async fn send_batch(&self, batch: TraceBatch) -> Result<(), String> {
        let resp = self
            .client
            .post(&self.endpoint_url)
            .json(&batch)
            .send()
            .await
            .map_err(|e| format!("reqwest send: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!("status {}", resp.status()));
        }
        Ok(())
    }
}

/// Handle the engine holds to push spans into the uploader. Cheap to
/// clone (just an `mpsc::Sender`).
#[derive(Clone, Debug)]
pub struct TraceUploaderHandle {
    tx: mpsc::Sender<Span>,
    dropped: Arc<Mutex<u64>>,
}

impl TraceUploaderHandle {
    /// Best-effort push. Returns `QueueFull` if the bounded channel is
    /// saturated — the caller should NOT retry; the next span will
    /// likely fit.
    pub fn push(&self, span: Span) -> Result<(), TraceUploaderError> {
        match self.tx.try_send(span) {
            Ok(()) => Ok(()),
            Err(mpsc::error::TrySendError::Full(_)) => {
                *self.dropped.lock() += 1;
                Err(TraceUploaderError::QueueFull)
            }
            Err(mpsc::error::TrySendError::Closed(_)) => Err(TraceUploaderError::Stopped),
        }
    }

    /// Cumulative count of spans dropped due to `QueueFull` since the
    /// uploader was spawned. Useful for a heartbeat-stamped diagnostic.
    #[must_use]
    pub fn dropped_count(&self) -> u64 {
        *self.dropped.lock()
    }
}

/// The background uploader task. Spawned by [`spawn_uploader`].
///
/// Cancellation: drop the returned [`TraceUploaderHandle`] (closes the
/// channel) → the task drains remaining spans, sends one final batch,
/// then exits.
pub struct TraceUploader;

impl TraceUploader {
    /// Build a `reqwest::Client` configured with mTLS identity matching
    /// the WSS tunnel. Returns a [`TraceUploaderHandle`] + the spawned
    /// `JoinHandle`. The caller owns both; dropping the handle is the
    /// canonical shutdown signal.
    ///
    /// # Errors
    ///
    /// Returns an error string if `reqwest::Client` construction fails
    /// (typically: bad PEM, no key, rustls install failure).
    pub fn spawn_with_mtls(
        cfg: TraceUploaderConfig,
        cert_pem: &[u8],
        key_pem: &[u8],
        ca_chain_pem: &[u8],
    ) -> Result<(TraceUploaderHandle, tokio::task::JoinHandle<()>), String> {
        let identity = reqwest::Identity::from_pem(&[cert_pem, key_pem].concat())
            .map_err(|e| format!("reqwest identity from PEM: {e}"))?;
        let ca = reqwest::Certificate::from_pem(ca_chain_pem)
            .map_err(|e| format!("reqwest ca from PEM: {e}"))?;
        let client = reqwest::Client::builder()
            .use_rustls_tls()
            .identity(identity)
            .add_root_certificate(ca)
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| format!("reqwest build: {e}"))?;
        let transport = Arc::new(ReqwestMtlsTransport::new(client, cfg.endpoint_url.clone()));
        Ok(Self::spawn(cfg, transport))
    }

    /// Spawn the uploader against an arbitrary [`BatchTransport`]. Tests
    /// use this with an in-memory transport that records batches.
    #[must_use]
    pub fn spawn(
        cfg: TraceUploaderConfig,
        transport: Arc<dyn BatchTransport>,
    ) -> (TraceUploaderHandle, tokio::task::JoinHandle<()>) {
        let (tx, mut rx) = mpsc::channel::<Span>(cfg.queue_capacity);
        let dropped = Arc::new(Mutex::new(0_u64));
        let handle = TraceUploaderHandle {
            tx,
            dropped: dropped.clone(),
        };
        let core_id = cfg.core_id;
        let batch_size = cfg.batch_size.max(1);
        let flush_interval = cfg.flush_interval;

        let join = tokio::spawn(async move {
            let mut buf: Vec<Span> = Vec::with_capacity(batch_size);
            let mut ticker = tokio::time::interval(flush_interval);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            // Skip the immediate first tick so we don't flush an empty
            // buffer on startup.
            ticker.tick().await;
            loop {
                tokio::select! {
                    biased;
                    maybe = rx.recv() => {
                        match maybe {
                            Some(span) => {
                                buf.push(span);
                                if buf.len() >= batch_size {
                                    flush(&transport, core_id, &mut buf).await;
                                }
                            }
                            None => {
                                // Sender dropped — drain and exit.
                                debug!(remaining = buf.len(), "trace uploader: channel closed; final flush");
                                flush(&transport, core_id, &mut buf).await;
                                break;
                            }
                        }
                    }
                    _ = ticker.tick() => {
                        flush(&transport, core_id, &mut buf).await;
                    }
                }
            }
            debug!("trace uploader: exited");
        });

        (handle, join)
    }
}

/// Flush helper. No-op on empty buffer; logs WARN on transport error
/// and discards the batch (best-effort delivery).
async fn flush(transport: &Arc<dyn BatchTransport>, core_id: Uuid, buf: &mut Vec<Span>) {
    if buf.is_empty() {
        return;
    }
    let batch = TraceBatch {
        core_id,
        spans: std::mem::take(buf),
    };
    let count = batch.spans.len();
    if let Err(e) = transport.send_batch(batch).await {
        warn!(error = %e, count, "trace uploader: batch send failed; spans dropped");
    } else {
        debug!(count, "trace uploader: batch sent");
    }
}

/// Convenience: current wall-clock nanoseconds since UNIX epoch.
/// Saturates at `u64::MAX` rather than panicking on time-travel.
#[must_use]
pub fn now_unix_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tokio::sync::Mutex as TokioMutex;

    /// Test transport that records every batch it receives.
    #[derive(Default, Clone)]
    struct RecordingTransport {
        batches: Arc<TokioMutex<Vec<TraceBatch>>>,
    }

    #[async_trait::async_trait]
    impl BatchTransport for RecordingTransport {
        async fn send_batch(&self, batch: TraceBatch) -> Result<(), String> {
            self.batches.lock().await.push(batch);
            Ok(())
        }
    }

    fn sample_span(name: &str) -> Span {
        Span {
            trace_id: "0".repeat(32),
            span_id: "0".repeat(16),
            parent_span_id: None,
            name: name.to_string(),
            kind: SpanKind::Internal,
            start_unix_ns: 1_700_000_000_000_000_000,
            end_unix_ns: 1_700_000_000_100_000_000,
            status: SpanStatus::Ok,
            attributes: HashMap::new(),
        }
    }

    #[tokio::test]
    async fn batches_flush_on_size_trigger() {
        let transport = Arc::new(RecordingTransport::default());
        let cfg = TraceUploaderConfig {
            endpoint_url: "https://test/".into(),
            core_id: Uuid::nil(),
            batch_size: 3,
            flush_interval: Duration::from_secs(60),
            queue_capacity: 16,
        };
        let (handle, _join) =
            TraceUploader::spawn(cfg, transport.clone() as Arc<dyn BatchTransport>);
        for i in 0..3 {
            handle.push(sample_span(&format!("s-{i}"))).unwrap();
        }
        // Give the task a chance to flush.
        for _ in 0..20 {
            if !transport.batches.lock().await.is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let batches = transport.batches.lock().await;
        assert_eq!(batches.len(), 1, "exactly one batch should have flushed");
        assert_eq!(batches[0].spans.len(), 3);
    }

    #[tokio::test]
    async fn batches_flush_on_timer_trigger() {
        let transport = Arc::new(RecordingTransport::default());
        let cfg = TraceUploaderConfig {
            endpoint_url: "https://test/".into(),
            core_id: Uuid::nil(),
            batch_size: 1000,
            flush_interval: Duration::from_millis(50),
            queue_capacity: 16,
        };
        let (handle, _join) =
            TraceUploader::spawn(cfg, transport.clone() as Arc<dyn BatchTransport>);
        handle.push(sample_span("only-one")).unwrap();
        // Wait long enough for the ticker to fire at least twice.
        tokio::time::sleep(Duration::from_millis(200)).await;
        let batches = transport.batches.lock().await;
        assert!(!batches.is_empty(), "timer should have flushed");
        assert_eq!(batches[0].spans.len(), 1);
    }

    #[tokio::test]
    async fn empty_buffer_does_not_post() {
        let transport = Arc::new(RecordingTransport::default());
        let cfg = TraceUploaderConfig {
            endpoint_url: "https://test/".into(),
            core_id: Uuid::nil(),
            batch_size: 1000,
            flush_interval: Duration::from_millis(20),
            queue_capacity: 16,
        };
        let (_handle, _join) =
            TraceUploader::spawn(cfg, transport.clone() as Arc<dyn BatchTransport>);
        tokio::time::sleep(Duration::from_millis(80)).await;
        let batches = transport.batches.lock().await;
        assert!(batches.is_empty(), "no spans pushed, no batches sent");
    }

    #[tokio::test]
    async fn drop_handle_drains_final_batch() {
        let transport = Arc::new(RecordingTransport::default());
        let cfg = TraceUploaderConfig {
            endpoint_url: "https://test/".into(),
            core_id: Uuid::nil(),
            batch_size: 1000,
            flush_interval: Duration::from_secs(60),
            queue_capacity: 16,
        };
        let (handle, join) =
            TraceUploader::spawn(cfg, transport.clone() as Arc<dyn BatchTransport>);
        handle.push(sample_span("last")).unwrap();
        drop(handle);
        // Task should drain and exit promptly.
        let _ = tokio::time::timeout(Duration::from_secs(2), join).await;
        let batches = transport.batches.lock().await;
        assert_eq!(batches.len(), 1, "final flush on shutdown");
        assert_eq!(batches[0].spans[0].name, "last");
    }

    #[tokio::test]
    async fn queue_full_returns_error_and_increments_counter() {
        // Transport that never returns (so the worker can't drain).
        struct StuckTransport;
        #[async_trait::async_trait]
        impl BatchTransport for StuckTransport {
            async fn send_batch(&self, _batch: TraceBatch) -> Result<(), String> {
                std::future::pending::<()>().await;
                unreachable!()
            }
        }
        let cfg = TraceUploaderConfig {
            endpoint_url: "https://test/".into(),
            core_id: Uuid::nil(),
            batch_size: 1, // flushes every push -> worker blocks
            flush_interval: Duration::from_secs(60),
            queue_capacity: 2,
        };
        let (handle, _join) = TraceUploader::spawn(cfg, Arc::new(StuckTransport));
        // Fill: first push triggers flush (in flight, stuck). Subsequent
        // pushes accumulate until queue saturates.
        let mut full_seen = false;
        for i in 0..20 {
            match handle.push(sample_span(&format!("s-{i}"))) {
                Ok(()) => {}
                Err(TraceUploaderError::QueueFull) => {
                    full_seen = true;
                    break;
                }
                Err(other) => panic!("unexpected error: {other}"),
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(full_seen, "queue must saturate against stuck transport");
        assert!(handle.dropped_count() >= 1);
    }

    #[test]
    fn span_duration_helpers_saturate_on_clock_skew() {
        let s = Span {
            trace_id: "0".repeat(32),
            span_id: "0".repeat(16),
            parent_span_id: None,
            name: "skew".into(),
            kind: SpanKind::Internal,
            start_unix_ns: 100,
            end_unix_ns: 50, // clock went backwards
            status: SpanStatus::Ok,
            attributes: HashMap::new(),
        };
        assert_eq!(s.duration_ns(), 0);
        assert_eq!(s.duration_ms(), 0);
    }

    #[test]
    fn span_kind_default_is_internal() {
        assert_eq!(SpanKind::default(), SpanKind::Internal);
    }

    #[test]
    fn span_status_default_is_ok() {
        assert_eq!(SpanStatus::default(), SpanStatus::Ok);
    }

    #[test]
    fn now_unix_ns_returns_recent_value() {
        let ns = now_unix_ns();
        // Should be after 2024-01-01 (1704067200_000_000_000) and before 2100.
        assert!(ns > 1_704_067_200_000_000_000);
        assert!(ns < 4_102_444_800_000_000_000);
    }
}
