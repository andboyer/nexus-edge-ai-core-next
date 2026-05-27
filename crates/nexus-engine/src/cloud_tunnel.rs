//! Boot-time cloud-tunnel supervisor.
//!
//! Reads `cloud_enrollment` once, and if present spawns a long-running
//! task that maintains the WSS+mTLS tunnel to `edge-gateway`. On
//! connect, the task sends a `Heartbeat` envelope every 30s. On
//! disconnect (any error or close frame), it backs off exponentially
//! and reconnects. The engine continues to serve locally even if the
//! tunnel never connects — Hard Rule 5 (fail-open).
//!
//! Phase 1.8 ships heartbeats only. RPC dispatch lands in the next
//! slice once `nexus-engine` has handlers.
//!
//! Phase 1.14 — this supervisor also owns the trace-uploader consumer
//! task: when the engine boots, `main.rs` calls
//! [`nexus_cloud_client::trace_uploader::TraceUploader::channel`] to
//! get the producer half (handed to the tracing subscriber) and the
//! receiver half (passed in here). Once `cloud_enrollment` is read,
//! the receiver is drained by a [`TraceUploader::run_with_mtls`] task
//! that reuses the same cert / key / CA chain as the tunnel itself.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use nexus_cloud_client::trace_uploader::{
    Span, TraceUploader, TraceUploaderConfig, DEFAULT_BATCH_SIZE, DEFAULT_FLUSH_INTERVAL,
    DEFAULT_QUEUE_CAPACITY,
};
use nexus_cloud_client::{TunnelClient, TunnelHandle};
use nexus_cloud_protocol::v1::{Envelope, EnvelopeBody, EnvelopeMeta, HeartbeatPayload};
use nexus_store::cloud::CloudEnrollment;
use nexus_store::Store;
use tokio::sync::{mpsc, oneshot};
use tracing::{info, warn};
use uuid::Uuid;

/// Heartbeat cadence. Matches the cloud edge-gateway's `liveness_timeout / 2`
/// expectation.
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);
/// Reconnect backoff bounds.
const BACKOFF_MIN: Duration = Duration::from_secs(2);
const BACKOFF_MAX: Duration = Duration::from_secs(60);

/// Spawn the tunnel supervisor. The task probes
/// `cloud_enrollment` itself; if the row is missing it logs and
/// exits immediately. Returns the shutdown sender + join handle pair
/// so the engine shutdown sequence can clean it up the same way it
/// cleans up the other long-running tasks.
///
/// `trace_rx`, when provided, is the consumer half of the
/// boot-time-allocated trace-uploader channel. After enrollment is
/// successfully read, a [`TraceUploader::run_with_mtls`] task is
/// spawned to drain the channel and ship batches to the edge-gateway.
/// When no enrollment is present (local-only mode), the receiver is
/// dropped: the bounded channel fills at `queue_capacity` and further
/// pushes from the `TraceLayer` fail silently per the fail-open
/// posture in Hard Rule 5.
pub fn spawn_tunnel(
    store: Arc<Store>,
    trace_rx: Option<mpsc::Receiver<Span>>,
) -> (oneshot::Sender<()>, tokio::task::JoinHandle<()>) {
    let (tx, rx) = oneshot::channel::<()>();
    let handle = tokio::spawn(async move {
        let enrollment = match store.get_cloud_enrollment().await {
            Ok(Some(e)) => e,
            Ok(None) => {
                info!("no cloud enrollment present; cloud tunnel disabled");
                // trace_rx is dropped here — `TraceLayer` pushes fail
                // closed once the bounded channel fills.
                return;
            }
            Err(e) => {
                warn!(error = %e, "could not read cloud_enrollment; cloud tunnel disabled");
                return;
            }
        };
        info!(
            core_id = %enrollment.core_id,
            gateway_url = %enrollment.gateway_url,
            "starting cloud tunnel supervisor",
        );
        if let Some(rx) = trace_rx {
            spawn_trace_uploader(&enrollment, rx);
        }
        run(enrollment, rx).await;
    });
    (tx, handle)
}

/// Derive the HTTP(S) base URL for cloud APIs from the websocket
/// tunnel URL: replace `wss://` with `https://` (or `ws://` with
/// `http://`), strip any trailing `/v1/tunnel` path, and append
/// `/v1/edge/traces`.
fn derive_trace_endpoint(wss_url: &str) -> String {
    let base = wss_url
        .strip_prefix("wss://")
        .map(|s| format!("https://{s}"))
        .or_else(|| wss_url.strip_prefix("ws://").map(|s| format!("http://{s}")))
        .unwrap_or_else(|| wss_url.to_string());
    let trimmed = base.trim_end_matches('/');
    let stripped = trimmed.strip_suffix("/v1/tunnel").unwrap_or(trimmed);
    format!("{stripped}/v1/edge/traces")
}

fn spawn_trace_uploader(enrollment: &CloudEnrollment, rx: mpsc::Receiver<Span>) {
    let core_id = match Uuid::parse_str(&enrollment.core_id) {
        Ok(id) => id,
        Err(e) => {
            warn!(
                core_id = %enrollment.core_id,
                error = %e,
                "cloud enrollment core_id is not a valid UUID; trace uploader disabled",
            );
            return;
        }
    };
    let endpoint_url = derive_trace_endpoint(&enrollment.gateway_url);
    let cfg = TraceUploaderConfig {
        endpoint_url,
        core_id,
        batch_size: DEFAULT_BATCH_SIZE,
        flush_interval: DEFAULT_FLUSH_INTERVAL,
        queue_capacity: DEFAULT_QUEUE_CAPACITY,
    };
    match TraceUploader::run_with_mtls(
        rx,
        cfg,
        enrollment.cert_pem.as_bytes(),
        enrollment.private_key_pem.as_bytes(),
        enrollment.ca_chain_pem.as_bytes(),
    ) {
        Ok(_join) => {
            info!(
                core_id = %enrollment.core_id,
                "trace uploader spawned; engine spans will ship to edge-gateway",
            );
        }
        Err(e) => {
            warn!(
                error = %e,
                "trace uploader spawn failed; engine spans will not ship",
            );
        }
    }
}

async fn run(enrollment: CloudEnrollment, mut shutdown: oneshot::Receiver<()>) {
    let client = TunnelClient::new(
        enrollment.gateway_url.clone(),
        enrollment.cert_pem.clone(),
        enrollment.private_key_pem.clone(),
        enrollment.ca_chain_pem.clone(),
    );
    let mut backoff = BACKOFF_MIN;
    let core_id = enrollment.core_id.clone();
    loop {
        // Check for shutdown before each connect attempt.
        if shutdown.try_recv().is_ok() {
            info!(core_id = %core_id, "cloud tunnel shutdown requested");
            return;
        }
        match client.connect().await {
            Ok(conn) => {
                backoff = BACKOFF_MIN;
                let pump = pump_heartbeats(&conn, &core_id);
                tokio::select! {
                    biased;
                    _ = &mut shutdown => {
                        info!(core_id = %core_id, "cloud tunnel shutdown requested");
                        return;
                    }
                    _ = pump => {
                        // pump returns when send fails -> tunnel down -> reconnect.
                        warn!(core_id = %core_id, "cloud tunnel pump exited; will reconnect");
                    }
                }
            }
            Err(e) => {
                warn!(
                    core_id = %core_id,
                    error = %e,
                    backoff_secs = backoff.as_secs(),
                    "cloud tunnel connect failed; backing off",
                );
            }
        }
        tokio::select! {
            biased;
            _ = &mut shutdown => {
                info!(core_id = %core_id, "cloud tunnel shutdown requested");
                return;
            }
            _ = tokio::time::sleep(backoff) => {}
        }
        backoff = std::cmp::min(backoff * 2, BACKOFF_MAX);
    }
}

async fn pump_heartbeats<H: TunnelHandle>(handle: &H, _core_id: &str) {
    let mut interval = tokio::time::interval(HEARTBEAT_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let start = std::time::Instant::now();
    let mut seq: u64 = 0;
    loop {
        interval.tick().await;
        seq = seq.wrapping_add(1);
        let env = Envelope {
            meta: EnvelopeMeta {
                id: uuid::Uuid::now_v7().to_string(),
                in_reply_to: None,
                seq: Some(seq),
                trace: None,
                ts: chrono::Utc::now().to_rfc3339(),
                v: 1,
            },
            body: EnvelopeBody::Heartbeat(HeartbeatPayload {
                edge_ts_unix_ms: Some(now_unix_ms()),
                online_cameras: 0,
                queued_alerts: 0,
                release: None,
                tier: "dev".to_string(),
                uptime_s: start.elapsed().as_secs(),
                version: env!("CARGO_PKG_VERSION").to_string(),
            }),
        };
        if let Err(e) = handle.send(env).await {
            warn!(error = %e, "heartbeat send failed; pump exiting");
            return;
        }
    }
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
