//! `CloudConsoleSink` — engine-facing sink that projects local
//! `AlertEvent`s into wire `Envelope::Alert` frames and hands them to
//! the [`crate::tunnel::TunnelHandle`].
//!
//! Phase 1.7 ships the projection function and the sink shell; the
//! engine wires the sink into its bus in Phase 1.11 alongside the
//! tunnel body.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use nexus_cloud_protocol::v1::{
    AlertPayload, ClipReplicatedPayload, Envelope, EnvelopeBody, EnvelopeMeta,
};
use uuid::Uuid;

use crate::tunnel::{TunnelError, TunnelHandle};

/// Edge-side projection of an alert as the sink expects it. The engine
/// already has a richer `AlertEvent` in [`nexus_types`]; this struct
/// names just the fields that travel over the wire.
#[derive(Debug, Clone)]
pub struct AlertProjection {
    /// Core-local id. Dedup key on cloud INSERT (`cores.id × edge_event_id`).
    pub edge_event_id: String,
    /// Wall-clock timestamp at which the event was detected.
    pub ts: DateTime<Utc>,
    /// Per-core integer id (matches `cameras.edge_camera_id`).
    pub camera_id: u64,
    /// 0..255 severity. The cloud-console rule mapper bins these into
    /// the SPA's visual severities.
    pub severity: u64,
    /// Edge rule id that fired, when known.
    pub edge_rule_id: Option<String>,
    /// Matched label (`"person"`, `"vehicle"`, …) when the rule was
    /// label-bound.
    pub matched_label: Option<String>,
    /// Detector confidence, when applicable.
    pub confidence: Option<f64>,
    /// Bounding box as `[x, y, w, h]` normalised 0..1.
    pub bbox: Option<Vec<f64>>,
    /// SAS URL of the snapshot blob, pre-uploaded by the storage layer.
    pub snapshot_blob_url: Option<String>,
    /// SAS URL of the clip blob, when the rule had clip-on.
    pub clip_blob_url: Option<String>,
    /// Phase 21.2 — set when the clip was pre-attached on edge.
    pub attached_history: Option<bool>,
}

/// Sink shell. Wraps any [`TunnelHandle`] impl so the engine can
/// substitute a mock tunnel in tests.
pub struct CloudConsoleSink {
    tunnel: Arc<dyn TunnelHandle>,
}

impl CloudConsoleSink {
    /// Build a sink around the given tunnel handle.
    #[must_use]
    pub fn new(tunnel: Arc<dyn TunnelHandle>) -> Self {
        Self { tunnel }
    }

    /// Publish one alert. The sink builds the envelope and hands it to
    /// the tunnel; persistence + retry on send-failure is the tunnel
    /// implementation's job (the engine outbox sits in front of this).
    ///
    /// # Errors
    ///
    /// Returns [`TunnelError`] from the underlying handle. Phase 1.7
    /// always surfaces [`TunnelError::NotImplemented`] from the default
    /// [`TunnelClient`](crate::tunnel::TunnelClient).
    pub async fn publish_alert(&self, alert: AlertProjection) -> Result<(), TunnelError> {
        let envelope = build_alert_envelope(alert);
        self.tunnel.send(envelope).await
    }

    /// Publish one cold-replicated clip. The sink builds the envelope
    /// from the edge-side projection and hands it to the tunnel.
    ///
    /// Phase 2 · Step 2.8 — paired with the cold replicator: after
    /// `Store::mark_cold_replicated` stamps the row, the replicator
    /// builds a [`ClipReplicatedProjection`] from the [`ClipRow`]
    /// plus the `PutReceipt::cold_url` returned by the backend, then
    /// fires this method via the shared [`crate::TunnelOutbox`].
    ///
    /// # Errors
    ///
    /// Returns [`TunnelError`] from the underlying handle (e.g.
    /// `Disconnected` when the WSS tunnel is currently down — the
    /// replicator logs and continues; the polling backstop +
    /// future Phase 6.17 sweep reconcile any lost envelopes).
    pub async fn publish_clip_replicated(
        &self,
        clip: ClipReplicatedProjection,
    ) -> Result<(), TunnelError> {
        let envelope = build_clip_replicated_envelope(clip);
        self.tunnel.send(envelope).await
    }
}

/// Pure-function projection. Public so engine tests can construct
/// reference envelopes without instantiating a sink.
#[must_use]
pub fn build_alert_envelope(alert: AlertProjection) -> Envelope {
    let payload = AlertPayload {
        edge_event_id: alert.edge_event_id,
        ts: alert.ts.to_rfc3339(),
        camera_id: alert.camera_id,
        severity: alert.severity,
        edge_rule_id: alert.edge_rule_id,
        matched_label: alert.matched_label,
        confidence: alert.confidence,
        bbox: alert.bbox,
        snapshot_blob_url: alert.snapshot_blob_url,
        clip_blob_url: alert.clip_blob_url,
        attached_history: alert.attached_history,
    };
    Envelope {
        meta: EnvelopeMeta {
            v: 1,
            id: Uuid::now_v7().to_string(),
            ts: Utc::now().to_rfc3339(),
            in_reply_to: None,
            seq: None,
            trace: None,
        },
        body: EnvelopeBody::Alert(payload),
    }
}

#[async_trait]
trait _SinkContract: Send + Sync {
    async fn publish_alert(&self, alert: AlertProjection) -> Result<(), TunnelError>;
    async fn publish_clip_replicated(
        &self,
        clip: ClipReplicatedProjection,
    ) -> Result<(), TunnelError>;
}

/// Edge-side projection of a cold-replicated clip as the sink expects it.
/// Mirrors [`AlertProjection`] in shape (engine has a richer [`nexus_store::motion::ClipRow`];
/// this struct names just the fields that travel over the wire as a
/// [`ClipReplicatedPayload`]).
///
/// Phase 2 · Step 2.8.
#[derive(Debug, Clone)]
pub struct ClipReplicatedProjection {
    /// Core-local clip id. Dedup key on cloud INSERT (`cores.id × edge_clip_id`).
    pub edge_clip_id: String,
    /// Per-core integer id (matches `cameras.edge_camera_id`).
    pub camera_id: u64,
    /// Bare blob URL (no SAS query) of the freshly-uploaded clip MP4.
    /// Sourced from `PutReceipt::cold_url`, which the Azure backend
    /// stamps as the bare `blob_url_unsigned` minted by the SAS issuer.
    pub blob_url: String,
    /// Wall-clock at which the clip recording started.
    pub started_at: DateTime<Utc>,
    /// Final clip duration in milliseconds.
    pub duration_ms: u64,
    /// Final on-disk byte count after MP4 close.
    pub size_bytes: u64,
    /// 64-character lowercase hex of the streaming SHA-256 computed
    /// over the MP4 bytes during write on the edge. Cloud stores in
    /// `clips.sha256` and the Phase 6.17 integrity sweep verifies
    /// against Blob on read.
    pub sha256_hex: String,
    /// Video codec inside the MP4. `None` → cloud defaults to `h264`.
    pub codec: Option<String>,
    /// Container format. `None` → cloud defaults to `mp4`.
    pub container: Option<String>,
    /// SAS-stripped URL of the per-clip thumbnail, when generated.
    pub thumbnail_blob_url: Option<String>,
    /// Phase 21.2 — set when the clip was pre-attached on edge
    /// (i.e. existed on disk before this core enrolled).
    pub attached_history: Option<bool>,
}

/// Pure-function projection. Public so engine tests can construct
/// reference envelopes without instantiating a sink.
///
/// Phase 2 · Step 2.8.
#[must_use]
pub fn build_clip_replicated_envelope(clip: ClipReplicatedProjection) -> Envelope {
    let payload = ClipReplicatedPayload {
        attached_history: clip.attached_history,
        blob_url: clip.blob_url,
        camera_id: clip.camera_id,
        codec: clip.codec,
        container: clip.container,
        duration_ms: clip.duration_ms,
        edge_clip_id: clip.edge_clip_id,
        sha256_hex: clip.sha256_hex,
        size_bytes: clip.size_bytes,
        started_at: clip.started_at.to_rfc3339(),
        thumbnail_blob_url: clip.thumbnail_blob_url,
    };
    Envelope {
        meta: EnvelopeMeta {
            v: 1,
            id: Uuid::now_v7().to_string(),
            ts: Utc::now().to_rfc3339(),
            in_reply_to: None,
            seq: None,
            trace: None,
        },
        body: EnvelopeBody::ClipReplicated(payload),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct CapturingTunnel {
        last: parking_lot::Mutex<Option<Envelope>>,
    }

    #[async_trait]
    impl TunnelHandle for CapturingTunnel {
        async fn send(&self, envelope: Envelope) -> Result<(), TunnelError> {
            *self.last.lock() = Some(envelope);
            Ok(())
        }
    }

    #[tokio::test]
    async fn publish_alert_builds_v1_envelope() {
        let tunnel = Arc::new(CapturingTunnel {
            last: parking_lot::Mutex::new(None),
        });
        let sink = CloudConsoleSink::new(tunnel.clone());
        sink.publish_alert(AlertProjection {
            edge_event_id: "evt-1".into(),
            ts: Utc::now(),
            camera_id: 7,
            severity: 4,
            edge_rule_id: Some("rule_person".into()),
            matched_label: Some("person".into()),
            confidence: Some(0.91),
            bbox: Some(vec![0.0, 0.0, 1.0, 1.0]),
            snapshot_blob_url: None,
            clip_blob_url: None,
            attached_history: None,
        })
        .await
        .expect("send");
        let captured = tunnel.last.lock().clone().expect("captured envelope");
        assert_eq!(captured.meta.v, 1);
        match captured.body {
            EnvelopeBody::Alert(p) => {
                assert_eq!(p.edge_event_id, "evt-1");
                assert_eq!(p.camera_id, 7);
            }
            other => panic!("expected Alert, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn publish_clip_replicated_builds_v1_envelope() {
        let tunnel = Arc::new(CapturingTunnel {
            last: parking_lot::Mutex::new(None),
        });
        let sink = CloudConsoleSink::new(tunnel.clone());
        let started_at = Utc::now();
        sink.publish_clip_replicated(ClipReplicatedProjection {
            edge_clip_id: "clip-42".into(),
            camera_id: 3,
            blob_url: "https://core.blob.core.windows.net/clips/clip-42.mp4".into(),
            started_at,
            duration_ms: 30_000,
            size_bytes: 1_234_567,
            sha256_hex: "a".repeat(64),
            codec: Some("h264".into()),
            container: Some("mp4".into()),
            thumbnail_blob_url: None,
            attached_history: None,
        })
        .await
        .expect("send");
        let captured = tunnel.last.lock().clone().expect("captured envelope");
        assert_eq!(captured.meta.v, 1);
        match captured.body {
            EnvelopeBody::ClipReplicated(p) => {
                assert_eq!(p.edge_clip_id, "clip-42");
                assert_eq!(p.camera_id, 3);
                assert_eq!(p.duration_ms, 30_000);
                assert_eq!(p.size_bytes, 1_234_567);
                assert_eq!(p.sha256_hex.len(), 64);
                assert_eq!(p.started_at, started_at.to_rfc3339());
            }
            other => panic!("expected ClipReplicated, got {other:?}"),
        }
    }
}
