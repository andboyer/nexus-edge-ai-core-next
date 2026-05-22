//! WSS tunnel client to `edge-gateway /v1/tunnel`.
//!
//! Phase 1.7 ships the type contract; Phase 1.11 wires the body (real
//! `tokio-tungstenite` client with mTLS via `rustls`, envelope reader/
//! writer, reconnect-with-backoff). The interface is intentionally
//! small so the engine can stub it in tests by implementing the
//! [`TunnelHandle`] trait.

use async_trait::async_trait;
use nexus_cloud_protocol::v1::Envelope;

/// Handle the engine talks to. Phase 1.7 exposes the minimum surface:
/// send one envelope outbound, register an inbound handler. The
/// concrete implementation in Phase 1.11 will add backpressure +
/// reconnect signalling.
#[async_trait]
pub trait TunnelHandle: Send + Sync {
    /// Send an outbound envelope (edge → cloud). Returns when the frame
    /// has been queued for the WSS writer task; not when the cloud has
    /// acknowledged it.
    async fn send(&self, envelope: Envelope) -> Result<(), TunnelError>;
}

/// Errors the tunnel client can surface.
#[derive(Debug, Clone, thiserror::Error)]
pub enum TunnelError {
    /// Tunnel is currently disconnected (engine should retry after
    /// the reconnect backoff has elapsed).
    #[error("tunnel disconnected")]
    Disconnected,
    /// Phase 1.7 stub return. Removed once Phase 1.11 wires the body.
    #[error("tunnel client not wired yet (Phase 1.11)")]
    NotImplemented,
}

/// Phase 1.7 client shell. The constructor pins the gateway URL; an
/// internal connect loop lands in Phase 1.11.
#[derive(Debug, Clone)]
pub struct TunnelClient {
    gateway_url: String,
}

impl TunnelClient {
    /// Build a client targeting the resolved `wss://gateway/v1/tunnel`
    /// URL from the enrollment artifact.
    #[must_use]
    pub fn new(gateway_url: impl Into<String>) -> Self {
        Self {
            gateway_url: gateway_url.into(),
        }
    }

    /// Configured gateway URL.
    #[must_use]
    pub fn gateway_url(&self) -> &str {
        &self.gateway_url
    }
}

#[async_trait]
impl TunnelHandle for TunnelClient {
    async fn send(&self, _envelope: Envelope) -> Result<(), TunnelError> {
        Err(TunnelError::NotImplemented)
    }
}
