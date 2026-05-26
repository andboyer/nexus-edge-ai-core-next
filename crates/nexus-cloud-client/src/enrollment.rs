//! Enrollment client. Phase 1.7 wires the HTTP body against the
//! cloud-side `POST /v1/enroll` handler in
//! `nexus-cloud-console/services/enrollment-svc/src/handlers.rs`.
//!
//! At enrollment time the engine:
//!
//! 1. Generates a fresh keypair, builds a CSR.
//! 2. Computes its hardware fingerprint (TPM EK / CPU serial / etc.).
//! 3. POSTs `{code, csr_pem, fingerprint}` to
//!    `https://<cloud-host>/v1/enroll`.
//! 4. Persists `cert_pem`, `ca_chain_pem`, and `entitlement_jwt` under
//!    the enrollment artifact directory.
//! 5. Pins `gateway_url` for the [`crate::tunnel::TunnelClient`].
//!
//! ## Deferred — actor_token verifying-key bundling
//!
//! The cloud-side `EnrollResponse` does NOT yet include the entitlement
//! signer's public key. Until that lands (a small follow-up slice on the
//! cloud-console enrollment-svc), the engine reads the verifying key
//! from a sidecar file at boot. The optional
//! [`EnrollmentResponse::entitlement_signing_key_pem`] field is left in
//! the struct so the response can be wire-compatible the moment the
//! cloud adds it; today it deserialises to `None` for every real
//! response.

use serde::{Deserialize, Serialize};

/// Enrollment request body. The engine builds this from the
/// operator-issued code and the locally-generated CSR + fingerprint.
///
/// Matches `EnrollRequest` in
/// `nexus-cloud-console/services/enrollment-svc/src/handlers.rs` exactly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnrollmentRequest {
    /// Short single-use code printed by the cloud-console enrollment
    /// flow (typically a 12-character base32 token, e.g. `XJ4K-PMQ7-9NAB`).
    pub code: String,
    /// PEM-encoded PKCS#10 CSR for the per-core mTLS client certificate.
    pub csr_pem: String,
    /// Hardware-derived fingerprint. Stable across reboots; the engine
    /// computes this from CPU serial / TPM EK / equivalent at first
    /// boot and stores it under the enrollment artifact directory.
    pub fingerprint: String,
}

/// Enrollment response. The engine writes each PEM field to a separate
/// file under the enrollment artifact directory.
///
/// Matches `EnrollResponse` in
/// `nexus-cloud-console/services/enrollment-svc/src/handlers.rs` plus
/// two forward-compat optional fields for the actor_token verifying key
/// (see module docs).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnrollmentResponse {
    /// PEM-encoded leaf mTLS certificate signed by the internal CA.
    pub cert_pem: String,
    /// PEM-encoded CA chain the engine validates the gateway against.
    pub ca_chain_pem: String,
    /// Initial entitlement JWT pre-loaded so the engine can run before
    /// the first heartbeat round-trip.
    pub entitlement_jwt: String,
    /// WSS endpoint the [`crate::tunnel::TunnelClient`] connects to.
    pub gateway_url: String,
    /// UUID the cloud-console assigned to this core (`cores.id`).
    pub core_id: String,
    /// Forward-compat — PEM-encoded Ed25519 public key the engine uses
    /// to verify `actor_token` JWTs and the inbound `EntitlementUpdate`
    /// JWT. Currently always absent on the cloud side; bundled in a
    /// Phase 1.7.x follow-up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entitlement_signing_key_pem: Option<String>,
    /// Forward-compat — optional `kid` to bind the bundled key to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entitlement_signing_kid: Option<String>,
}

/// Phase 1.7 client. The constructor pins the cloud-console base URL;
/// [`Self::enroll`] POSTs the enrollment body and deserialises the
/// response.
#[derive(Debug, Clone)]
pub struct EnrollmentClient {
    cloud_host: String,
    http: reqwest::Client,
}

impl EnrollmentClient {
    /// Build a new client targeting `cloud_host`
    /// (e.g. `https://cloud.example`).
    ///
    /// The underlying [`reqwest::Client`] is built with rustls-only TLS
    /// (no openssl) and no proxy auto-detection — same defaults as
    /// every other HTTP caller in the workspace.
    #[must_use]
    pub fn new(cloud_host: impl Into<String>) -> Self {
        Self::with_client(cloud_host, reqwest::Client::new())
    }

    /// Inject a custom [`reqwest::Client`] (useful for tests that need
    /// timeouts, custom root certs, or wiremock-style local URLs).
    #[must_use]
    pub fn with_client(cloud_host: impl Into<String>, http: reqwest::Client) -> Self {
        Self {
            cloud_host: cloud_host.into(),
            http,
        }
    }

    /// Cloud-console base URL bound at construction.
    #[must_use]
    pub fn cloud_host(&self) -> &str {
        &self.cloud_host
    }

    /// Perform the enrollment round-trip. POSTs `req` as JSON to
    /// `<cloud_host>/v1/enroll` and deserialises the response.
    ///
    /// # Errors
    ///
    /// * [`EnrollmentError::Transport`] — network or TLS failure.
    /// * [`EnrollmentError::BadStatus`] — server returned a non-2xx
    ///   status. The captured `body` is the verbatim server payload
    ///   (typically a JSON error envelope from the enrollment-svc).
    /// * [`EnrollmentError::Deserialize`] — server returned 2xx but the
    ///   body could not be parsed as [`EnrollmentResponse`]. Most
    ///   likely cause: a wire version skew between this crate's vendored
    ///   `proto/v1.json` and the cloud-side schema.
    pub async fn enroll(
        &self,
        req: &EnrollmentRequest,
    ) -> Result<EnrollmentResponse, EnrollmentError> {
        let url = format!("{}/v1/enroll", self.cloud_host.trim_end_matches('/'));
        let resp = self
            .http
            .post(&url)
            .json(req)
            .send()
            .await
            .map_err(EnrollmentError::Transport)?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(EnrollmentError::BadStatus {
                status: status.as_u16(),
                body,
            });
        }
        let bytes = resp.bytes().await.map_err(EnrollmentError::Transport)?;
        serde_json::from_slice::<EnrollmentResponse>(&bytes)
            .map_err(|e| EnrollmentError::Deserialize(e.to_string()))
    }
}

/// Errors the enrollment client can surface.
#[derive(Debug, thiserror::Error)]
pub enum EnrollmentError {
    /// Network, DNS, or TLS-handshake failure.
    #[error("enrollment HTTP transport: {0}")]
    Transport(#[from] reqwest::Error),
    /// Server returned a non-2xx status.
    #[error("enrollment server returned {status}: {body}")]
    BadStatus {
        /// HTTP status code.
        status: u16,
        /// Verbatim server response body.
        body: String,
    },
    /// Server returned 2xx but the body did not match
    /// [`EnrollmentResponse`].
    #[error("enrollment response deserialise: {0}")]
    Deserialize(String),
}
