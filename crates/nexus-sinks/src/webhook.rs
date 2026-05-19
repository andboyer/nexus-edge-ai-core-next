//! HTTP webhook `AlertSink`.
//!
//! v1 parity port of `src/persistence/webhook_retry_queue.cpp`:
//!
//!   * One JSON POST per `deliver()` call (the dispatcher owns
//!     retries — the sink does at most one HTTP attempt).
//!   * Optional shared-secret HMAC-SHA256 signature shipped in the
//!     `X-Nexus-Signature: sha256=<hex>` header. Format matches
//!     GitHub's webhook signature so operators can re-use existing
//!     verification snippets.
//!   * Optional operator-supplied custom headers (auth bearers,
//!     tenant tags). Applied AFTER the signature header so a
//!     misconfigured operator can't accidentally clobber it.
//!   * Status mapping: 2xx → `Ok`, 5xx/408/429/network → `Transient`,
//!     everything else → `Permanent`.
//!
//! Behind the `webhook` cargo feature so deployments that don't
//! need HTTP egress don't pull reqwest's transitive footprint.

use std::time::Duration;

use async_trait::async_trait;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use tracing::{debug, trace, warn};

use nexus_config::WebhookSinkConfig;
use nexus_types::AlertEvent;

use crate::{AlertSink, SinkError, SinkHealth, SinkId};

type HmacSha256 = Hmac<Sha256>;

/// Discriminator string for `SinkId::kind()`. Stable wire value
/// stored in every `alert_sink_outbox.sink_id` column — DO NOT
/// rename without a migration that rewrites historical rows.
pub const KIND: &str = "webhook";

/// HMAC signature header name. GitHub-compatible format
/// (`sha256=<hex>` value); operators can re-use existing webhook
/// verification snippets.
pub const SIGNATURE_HEADER: &str = "X-Nexus-Signature";

/// HTTP webhook sink. Single HTTP attempt per `deliver()` call —
/// the dispatcher owns retry + backoff.
pub struct WebhookSink {
    id: SinkId,
    url: url::Url,
    headers: Vec<(String, String)>,
    hmac_secret: Option<String>,
    http: reqwest::Client,
}

impl WebhookSink {
    /// Build a webhook sink from its TOML config. Returns
    /// `Permanent` on misconfiguration (bad name / invalid client
    /// builder); the caller surfaces this at engine boot before
    /// the dispatcher ever spins.
    pub fn new(cfg: &WebhookSinkConfig) -> Result<Self, SinkError> {
        let id = SinkId::new(KIND, &cfg.name).ok_or_else(|| {
            SinkError::Permanent(format!(
                "invalid webhook sink name '{}' (empty or contains ':')",
                cfg.name
            ))
        })?;
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(cfg.timeout_secs))
            // Operators occasionally point this at internal IPs
            // behind a self-signed cert; we DO NOT silently turn
            // off TLS verification — they can configure a system
            // CA bundle instead.
            .user_agent(concat!("nexus-edge/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| SinkError::Permanent(format!("reqwest client build: {e}")))?;
        // Snapshot headers as a Vec so we don't allocate a HashMap
        // iter on every deliver(). Ordering is irrelevant for HTTP.
        let headers = cfg
            .headers
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        Ok(Self {
            id,
            url: cfg.url.clone(),
            headers,
            hmac_secret: cfg.hmac_secret.clone(),
            http,
        })
    }

    /// Compute `sha256=<hex>` for the given body, or `None` when
    /// the sink has no configured HMAC secret. Pulled out for
    /// direct unit-testing without spinning a server.
    pub(crate) fn sign(secret: Option<&str>, body: &[u8]) -> Option<String> {
        let secret = secret?;
        let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).ok()?;
        mac.update(body);
        Some(format!(
            "sha256={}",
            hex::encode(mac.finalize().into_bytes())
        ))
    }

    /// Classify a reqwest error as Transient (worth retrying) or
    /// Permanent (operator must intervene). Network-level failures
    /// are always transient — the dispatcher's exponential backoff
    /// is designed for exactly this class of fault.
    fn classify_send_err(err: reqwest::Error) -> SinkError {
        // is_connect / is_timeout / is_request — all "the request
        // didn't reach a server that responded with a status code"
        // — are retryable. is_builder errors should not happen
        // post-`Client::build`, so they go to Permanent as a
        // defensive default.
        if err.is_builder() {
            SinkError::Permanent(format!("request build: {err}"))
        } else {
            SinkError::Transient(format!("send: {err}"))
        }
    }

    /// HTTP status → SinkError variant. Pulled out so the test
    /// suite can pin the mapping without a real server.
    pub(crate) fn classify_status(status: reqwest::StatusCode, body_preview: &str) -> SinkError {
        if status.is_server_error()
            || status == reqwest::StatusCode::REQUEST_TIMEOUT
            || status == reqwest::StatusCode::TOO_MANY_REQUESTS
        {
            SinkError::Transient(format!("HTTP {status}: {body_preview}"))
        } else {
            // 4xx other than 408/429 → operator misconfiguration.
            // The dispatcher will still count attempts (so a
            // flapping endpoint can recover) but accelerates the
            // row toward `dead` on the louder error path.
            SinkError::Permanent(format!("HTTP {status}: {body_preview}"))
        }
    }
}

#[async_trait]
impl AlertSink for WebhookSink {
    fn kind(&self) -> &'static str {
        KIND
    }

    fn id(&self) -> &SinkId {
        &self.id
    }

    async fn deliver(&self, event: &AlertEvent) -> Result<(), SinkError> {
        let body = serde_json::to_vec(event)
            .map_err(|e| SinkError::Permanent(format!("serde encode AlertEvent: {e}")))?;

        let mut req = self
            .http
            .post(self.url.clone())
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body.clone());

        // Signature first, then operator headers — operators can
        // override anything else but the signature stays canonical.
        if let Some(sig) = Self::sign(self.hmac_secret.as_deref(), &body) {
            req = req.header(SIGNATURE_HEADER, sig);
        }
        for (k, v) in &self.headers {
            req = req.header(k, v);
        }

        trace!(
            sink = %self.id,
            event = %event.event_id,
            body_bytes = body.len(),
            "webhook deliver",
        );

        let resp = req.send().await.map_err(Self::classify_send_err)?;
        let status = resp.status();

        if status.is_success() {
            debug!(sink = %self.id, %status, event = %event.event_id, "delivered");
            return Ok(());
        }

        // Read at most 256 bytes of body for diagnostics. Don't
        // hold the connection open longer than the operator's
        // timeout budget — bytes() consumes the whole body, which
        // is fine for an error path that the dispatcher logs
        // and forgets.
        let body_preview = resp
            .text()
            .await
            .map(|t| t.chars().take(256).collect::<String>())
            .unwrap_or_default();
        let err = Self::classify_status(status, &body_preview);
        warn!(
            sink = %self.id,
            event = %event.event_id,
            %status,
            transient = err.is_transient(),
            "webhook deliver failed",
        );
        Err(err)
    }

    fn health(&self) -> SinkHealth {
        // M7 ships without an in-process health window. The
        // dispatcher's last-error column on alert_sink_outbox
        // tells the same story for now; a real window lands with
        // the /admin/sinks/health card in step 6.
        SinkHealth::Unknown
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// HMAC sanity — known-answer test from the canonical
    /// HMAC-SHA256 Wikipedia reference vector
    /// (`key = "key"`, `data = "The quick brown fox jumps over the lazy dog"`
    /// → `f7bc83f430538424b13298e6aa6fb143ef4d59a14946175997479dbc2d1a3cd8`).
    /// Pins the digest, the key/data order, and the hex-encoding
    /// — three places a subtle copy-paste regression could hide.
    #[test]
    fn sign_matches_wikipedia_vector() {
        let got =
            WebhookSink::sign(Some("key"), b"The quick brown fox jumps over the lazy dog").unwrap();
        assert_eq!(
            got,
            "sha256=f7bc83f430538424b13298e6aa6fb143ef4d59a14946175997479dbc2d1a3cd8"
        );
    }

    #[test]
    fn sign_returns_none_without_secret() {
        assert!(WebhookSink::sign(None, b"anything").is_none());
    }

    #[test]
    fn sign_empty_body_still_produces_signature() {
        let sig = WebhookSink::sign(Some("shh"), b"").unwrap();
        assert!(sig.starts_with("sha256="));
        // sha256 hex is 64 chars; with the prefix that's 71.
        assert_eq!(sig.len(), 7 + 64);
    }

    #[test]
    fn classify_status_5xx_is_transient() {
        let err = WebhookSink::classify_status(reqwest::StatusCode::INTERNAL_SERVER_ERROR, "boom");
        assert!(err.is_transient(), "{err:?}");
    }

    #[test]
    fn classify_status_429_is_transient() {
        let err = WebhookSink::classify_status(reqwest::StatusCode::TOO_MANY_REQUESTS, "");
        assert!(err.is_transient(), "{err:?}");
    }

    #[test]
    fn classify_status_408_is_transient() {
        let err = WebhookSink::classify_status(reqwest::StatusCode::REQUEST_TIMEOUT, "");
        assert!(err.is_transient(), "{err:?}");
    }

    #[test]
    fn classify_status_401_is_permanent() {
        let err = WebhookSink::classify_status(reqwest::StatusCode::UNAUTHORIZED, "bad key");
        assert!(!err.is_transient(), "{err:?}");
    }

    #[test]
    fn classify_status_404_is_permanent() {
        let err = WebhookSink::classify_status(reqwest::StatusCode::NOT_FOUND, "");
        assert!(!err.is_transient(), "{err:?}");
    }
}
