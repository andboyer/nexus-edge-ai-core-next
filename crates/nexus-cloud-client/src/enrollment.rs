//! Enrollment client. Phase 1.7 ships the type contract; Phase 1.11
//! (cloud enrollment HTTP) wires the body.
//!
//! At enrollment time the engine generates a fresh keypair, builds a
//! CSR, POSTs it to `https://<cloud-host>/v1/cores/enroll` together
//! with the operator-issued enrollment code, and persists the returned
//! certificate chain + bundled signing public key + entitlement JWT to
//! the local enrollment artifact directory (under the engine data root).

use serde::{Deserialize, Serialize};

/// Enrollment request body. The engine builds this from the
/// operator-issued code and the locally-generated CSR.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnrollmentRequest {
    /// Short single-use code printed by the cloud-console enrollment
    /// flow (typically a 12-character base32 token).
    pub code: String,
    /// PEM-encoded CSR for the per-core mTLS client certificate.
    pub csr_pem: String,
    /// Hardware tier from `config/tiers/*.toml` so the cloud-console
    /// can register the core at the correct entitlement size.
    pub tier: String,
    /// Engine semver — recorded on `cores.first_seen_version` for
    /// debugging long-lived deployments.
    pub engine_version: String,
}

/// Enrollment response. The engine writes each field to a separate
/// file under the enrollment artifact directory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnrollmentResponse {
    /// UUID the cloud-console assigned to this core.
    pub core_id: String,
    /// PEM-encoded mTLS client certificate signed by the internal CA.
    pub client_cert_pem: String,
    /// PEM-encoded CA chain the engine validates the gateway against.
    pub ca_chain_pem: String,
    /// PEM-encoded Ed25519 public key the engine uses to verify
    /// `actor_token` JWTs (see [`crate::actor_token::Verifier`]) and
    /// the inbound `EntitlementUpdate` JWT.
    pub entitlement_signing_key_pem: String,
    /// Optional `kid` to bind the bundled key to (when omitted, the
    /// verifier accepts the key as the unique default).
    pub entitlement_signing_kid: Option<String>,
    /// Initial entitlement JWT pre-loaded so the engine can run before
    /// the first heartbeat round-trip.
    pub entitlement_jwt: String,
    /// WSS endpoint the [`crate::tunnel::TunnelClient`] connects to.
    pub gateway_url: String,
}

/// Phase 1.7 client shell. The constructor pins the cloud-console base
/// URL; [`Self::enroll`] is left unimplemented until Phase 1.11 brings
/// in the HTTP client (and the corresponding integration test against
/// the api-gateway `/v1/cores/enroll` endpoint).
#[derive(Debug, Clone)]
pub struct EnrollmentClient {
    cloud_host: String,
}

impl EnrollmentClient {
    /// Build a new client targeting `cloud_host` (e.g. `https://cloud.example`).
    #[must_use]
    pub fn new(cloud_host: impl Into<String>) -> Self {
        Self {
            cloud_host: cloud_host.into(),
        }
    }

    /// Cloud-console base URL bound at construction.
    #[must_use]
    pub fn cloud_host(&self) -> &str {
        &self.cloud_host
    }

    /// Perform the enrollment round-trip. **Stub** in Phase 1.7 — the
    /// real implementation lands in Phase 1.11 (`cloud-enrollment-http`
    /// row in `docs/PHASES.md`).
    ///
    /// # Errors
    ///
    /// Always returns [`EnrollmentError::NotImplemented`] in Phase 1.7.
    pub fn enroll(&self, _req: &EnrollmentRequest) -> Result<EnrollmentResponse, EnrollmentError> {
        Err(EnrollmentError::NotImplemented)
    }
}

/// Errors the enrollment client can surface.
#[derive(Debug, Clone, thiserror::Error)]
pub enum EnrollmentError {
    /// Phase 1.7 stub return. Removed once Phase 1.11 wires the body.
    #[error("enrollment HTTP client not wired yet (Phase 1.11)")]
    NotImplemented,
}
