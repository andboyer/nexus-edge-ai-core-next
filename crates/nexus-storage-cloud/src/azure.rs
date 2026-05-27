//! Azure Blob Storage cold backend (Phase 2 · Step 2.1).
//!
//! Implements [`ColdBackend`] against Azure Blob Storage **without
//! linking any Azure SDK** — per cloud-side `AGENTS.md` Rule 1 and
//! its paired engine-repo Rule 1, the edge MUST move blob bytes
//! with `reqwest` only, against signed Service-SAS URLs minted by
//! the cloud control plane. No `azure_storage_blobs`, no
//! `azure_identity`, nothing.
//!
//! ## Architecture
//!
//! The backend is **stateless** with respect to blob credentials.
//! Every operation that touches Azure begins with an mTLS-authenticated
//! HTTPS call to the edge-gateway's `POST /v1/edge/blob-sas`
//! endpoint (cloud-side `services/edge-gateway/src/blob_sas.rs`),
//! which forwards to api-gateway's
//! `POST /v1/internal/cores/:core_id/blob-sas` and returns a
//! ≤15-minute Service SAS URL scoped to one container + one blob
//! path. The backend then issues exactly one HTTP verb against
//! that URL (PUT for upload, GET with Range for download, HEAD
//! for exists), and discards the URL.
//!
//! Why per-operation SAS instead of caching: a leaked SAS URL is
//! a credential. Per-op + 15-min TTL caps the blast radius at
//! "one clip × 15 minutes". Re-requests cost ~one Postgres lookup
//! + one HMAC; comfortably under 50 ms p99 (api-gateway's blob_sas
//!   handler is unit-tested for this).
//!
//! ## Path semantics
//!
//! [`ColdBackend::put`] takes `path` of the form
//! `cam<id>/<timestamp>_<dur_ms>.mp4`. We extract `edge_clip_id`
//! as the path basename minus `.mp4` and forward that to the
//! SAS-issuance endpoint, which validates it against the safe-
//! segment allow-list (`[A-Za-z0-9_.-]+`, no `..`). The returned
//! `blob_path` (something like
//! `org-<uuid>/core-<uuid>/<edge_clip_id>.mp4`) becomes the
//! [`PutReceipt::cold_path`] — that's what the engine writes into
//! `motion_clips.cold_path`.
//!
//! ## sha256 verification
//!
//! Stamped into the blob's `x-ms-meta-nexus-sha256` user metadata
//! at PUT time. [`Self::exists`] re-reads it with a HEAD and
//! returns `true` only if the metadata round-trips. In addition,
//! Phase 2 \u00b7 Step 2.8 pins the body's true MD5 into
//! `x-ms-blob-content-md5` on PUT so Azure records it on the
//! blob's properties \u2014 the Phase 6.17 cloud-side integrity sweep
//! reads BOTH the SHA-256 user metadata AND the MD5 content header
//! and cross-checks them against a fresh re-hash of the body. The
//! redundant pair gives an attacker who tampers with cold (via
//! SAS replay, op-error, etc.) a much higher forge cost than
//! either hash alone.
//!
//! ## What we explicitly DO NOT implement (yet)
//!
//! * `delete` — requires a `DELETE` SAS kind that the api-gateway
//!   handler doesn't yet support (it only mints `put` and `get`).
//!   Operator-driven only on cloud cold; tracked for Phase 2.x.
//!   Returns `BackendError::Other` for now.
//! * Resumable / block-by-block uploads — every motion clip we
//!   produce today fits comfortably under Azure's 256 MiB
//!   single-PUT block-blob limit (5–80 MiB at 1080p). The trait
//!   contract is "one PUT for one clip"; staged uploads can land
//!   under a feature gate later.

use std::sync::Arc;

use async_trait::async_trait;
use base64::Engine;
use chrono::{DateTime, Utc};
use futures::stream::{StreamExt, TryStreamExt};
use nexus_storage::{BackendError, ByteStream, ColdBackend, HealthStatus, PutReceipt, VolumeInfo};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracing::{debug, warn};

/// Azure Blob Storage REST API version we send in `x-ms-version`.
/// Pinned so a service-side default-version change can't break us;
/// 2023-11-03 is the latest GA at time of writing and supports the
/// `x-ms-meta-*` user-metadata headers we depend on.
const AZURE_API_VERSION: &str = "2023-11-03";

/// User-metadata key we stamp the cleartext sha256 into at PUT time.
/// Azure normalises metadata keys to lowercase + replaces dashes
/// with underscores on echo; we use a hyphen here because Azure
/// accepts both forms on input but returns underscores in the
/// `x-ms-meta-*` response header. [`Self::exists`] reads both
/// spellings for safety.
const AZURE_SHA256_META: &str = "nexus-sha256";

// ---------------------------------------------------------------------------
// SasIssuer trait + concrete impls
// ---------------------------------------------------------------------------

/// Successfully-issued SAS for one operation against one blob.
///
/// Mirrors `services/edge-gateway/src/blob_sas.rs::EdgeBlobSasResponse`
/// in the cloud repo. Kept here as a flat owned struct so the
/// engine repo doesn't grow a dependency on the cloud wire-types
/// crate (would violate `REPO_BOUNDARY` R1).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IssuedSas {
    /// Signed URL — pass straight to `reqwest::Client::put(url)` or
    /// `reqwest::Client::get(url)`. Contains query-string SAS
    /// signature; treat as a credential.
    pub url: String,
    /// Unsigned blob URL (no SAS query). Suitable for persisting in
    /// `motion_clips.blob_url` columns so the SAS token never
    /// lands in Postgres.
    pub blob_url_unsigned: String,
    /// UTC expiry. Today: 15 minutes from issuance.
    pub expires_at: DateTime<Utc>,
    /// Storage container the SAS was scoped to.
    pub container: String,
    /// Blob path within the container (no leading slash).
    pub blob_path: String,
}

/// Object-safe trait so [`AzureBlobBackend`] can hold an
/// `Arc<dyn SasIssuer>` and tests can inject a mock without
/// spinning a real gateway.
#[async_trait]
pub trait SasIssuer: Send + Sync + std::fmt::Debug {
    /// Issue a write SAS for `edge_clip_id`. Used for PUT.
    async fn issue_put(&self, edge_clip_id: &str) -> Result<IssuedSas, BackendError>;

    /// Issue a read SAS for `edge_clip_id`. Used for GET (range
    /// reads) and HEAD (exists check — HEAD against a GET SAS is
    /// the canonical Azure pattern).
    async fn issue_get(&self, edge_clip_id: &str) -> Result<IssuedSas, BackendError>;
}

/// Production `SasIssuer` that POSTs to the edge-gateway's
/// `/v1/edge/blob-sas` endpoint over mTLS.
///
/// The `reqwest::Client` MUST be pre-built with the core's
/// enrollment-issued client certificate (`Identity::from_pem` +
/// `add_root_certificate(ca)`); this struct intentionally does
/// not own the cert material so the engine can share one
/// mTLS-configured client across `TunnelClient`, the trace
/// uploader, and this issuer.
#[derive(Debug, Clone)]
pub struct GatewaySasIssuer {
    http: reqwest::Client,
    /// Edge-gateway base URL (no trailing slash). The per-request
    /// path is `<base>/v1/edge/blob-sas`.
    gateway_url: String,
}

impl GatewaySasIssuer {
    /// Build an issuer bound to `gateway_url` (e.g.
    /// `https://gateway.example.com`).
    #[must_use]
    pub fn new(http: reqwest::Client, gateway_url: impl Into<String>) -> Self {
        Self {
            http,
            gateway_url: gateway_url.into().trim_end_matches('/').to_string(),
        }
    }

    async fn request(&self, edge_clip_id: &str, kind: &str) -> Result<IssuedSas, BackendError> {
        let endpoint = format!("{}/v1/edge/blob-sas", self.gateway_url);
        let body = serde_json::json!({
            "edge_clip_id": edge_clip_id,
            "kind": kind,
        });

        let resp = self
            .http
            .post(&endpoint)
            .json(&body)
            .send()
            .await
            .map_err(|e| BackendError::Unreachable(format!("gateway POST {endpoint}: {e}")))?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            // 401/403 from the gateway means our mTLS cert was
            // rejected (revoked / unknown / expired) — surface as
            // Auth so the admin UI can prompt for re-enrollment.
            if status == reqwest::StatusCode::UNAUTHORIZED
                || status == reqwest::StatusCode::FORBIDDEN
            {
                return Err(BackendError::Auth(format!(
                    "gateway {status} on blob-sas issuance: {body}"
                )));
            }
            return Err(BackendError::Other(format!(
                "gateway {status} on blob-sas issuance: {body}"
            )));
        }

        resp.json::<IssuedSas>()
            .await
            .map_err(|e| BackendError::Other(format!("blob-sas response parse: {e}")))
    }
}

#[async_trait]
impl SasIssuer for GatewaySasIssuer {
    async fn issue_put(&self, edge_clip_id: &str) -> Result<IssuedSas, BackendError> {
        self.request(edge_clip_id, "put").await
    }

    async fn issue_get(&self, edge_clip_id: &str) -> Result<IssuedSas, BackendError> {
        self.request(edge_clip_id, "get").await
    }
}

// ---------------------------------------------------------------------------
// AzureBlobBackend
// ---------------------------------------------------------------------------

/// `ColdBackend` impl that uploads to Azure Blob Storage via
/// short-TTL Service-SAS URLs minted by the cloud control plane.
pub struct AzureBlobBackend {
    handle: String,
    issuer: Arc<dyn SasIssuer>,
    http: reqwest::Client,
}

impl AzureBlobBackend {
    /// Construct a new backend.
    ///
    /// * `handle` — registry key; matches `storage_backends.handle`.
    ///   The cold replicator looks the backend up by this name.
    /// * `issuer` — usually [`GatewaySasIssuer`]; mockable in tests.
    /// * `http` — non-mTLS `reqwest::Client` for talking directly
    ///   to Azure Blob Storage. The SAS URL contains its own
    ///   credentials; this client just needs HTTPS support and a
    ///   reasonable timeout. (A SEPARATE client is used inside the
    ///   issuer for the mTLS hop.)
    pub fn new(
        handle: impl Into<String>,
        issuer: Arc<dyn SasIssuer>,
        http: reqwest::Client,
    ) -> Self {
        Self {
            handle: handle.into(),
            issuer,
            http,
        }
    }
}

impl std::fmt::Debug for AzureBlobBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AzureBlobBackend")
            .field("handle", &self.handle)
            .finish_non_exhaustive()
    }
}

/// Extract the `edge_clip_id` (basename, no extension) from a
/// trait-level `path` like `cam1/1700000000_15000.mp4`.
///
/// Validated against the same allow-list api-gateway enforces
/// (`[A-Za-z0-9_.-]+`, no `..`), so a malformed local path fails
/// fast with a sensible error instead of getting rejected
/// downstream by the SAS endpoint with a less-helpful 422.
fn extract_edge_clip_id(path: &str) -> Result<String, BackendError> {
    // Defense-in-depth: reject any traversal segment in the input
    // path even though we only use the basename downstream. A path
    // containing `..` is almost certainly a bug (or worse) in the
    // caller; surface it early.
    if path.split(['/', '\\']).any(|seg| seg == "..") {
        return Err(BackendError::InvalidPath(format!(
            "path '{path}' contains traversal segment"
        )));
    }
    let trimmed = path.trim_start_matches('/');
    let basename = std::path::Path::new(trimmed)
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| BackendError::InvalidPath(format!("path '{path}' has no basename")))?;
    let id = basename
        .strip_suffix(".mp4")
        .unwrap_or(basename)
        .to_string();
    if id.is_empty() {
        return Err(BackendError::InvalidPath(format!(
            "path '{path}' yields empty edge_clip_id"
        )));
    }
    if id == ".." || id.contains('/') || id.contains('\\') {
        return Err(BackendError::InvalidPath(format!(
            "path '{path}' yields unsafe edge_clip_id"
        )));
    }
    if !id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
    {
        return Err(BackendError::InvalidPath(format!(
            "path '{path}' yields edge_clip_id with disallowed chars"
        )));
    }
    Ok(id)
}

#[async_trait]
impl ColdBackend for AzureBlobBackend {
    fn handle(&self) -> &str {
        &self.handle
    }

    fn kind(&self) -> &str {
        "azure"
    }

    async fn put(
        &self,
        path: &str,
        bytes: &[u8],
        expected_sha256: &str,
    ) -> Result<PutReceipt, BackendError> {
        let edge_clip_id = extract_edge_clip_id(path)?;
        let sas = self.issuer.issue_put(&edge_clip_id).await?;

        // Spot-check that the SAS endpoint paired our request with a
        // matching `<edge_clip_id>.mp4` blob path — catches a future
        // contract drift between this backend and the cloud SAS
        // issuer (e.g. someone changes the extension convention).
        let expected_suffix = format!("{edge_clip_id}.mp4");
        if !sas.blob_path.ends_with(&expected_suffix) {
            return Err(BackendError::Other(format!(
                "SAS blob_path '{}' does not end with expected '{expected_suffix}'",
                sas.blob_path
            )));
        }

        // Verify the bytes match the expected sha256 BEFORE upload —
        // a torn read at the engine side would otherwise put corrupt
        // bytes on cold and we'd only catch it on next `exists`
        // retry. Cheap (~1 GB/s on Apple Silicon, <80 ms for a
        // 80 MiB clip).
        let actual = hex::encode(Sha256::digest(bytes));
        if actual != expected_sha256 {
            return Err(BackendError::ChecksumMismatch {
                expected: expected_sha256.to_string(),
                actual,
            });
        }

        // Send `expected_sha256` as a `x-ms-meta-*` so `exists()` can
        // later read it back and confirm the blob is what we wrote.
        // Azure rejects metadata-header values containing characters
        // outside US-ASCII printables; hex passes trivially.
        //
        // Also pin `x-ms-blob-content-md5` (Phase 2 \u00b7 Step 2.8). Azure
        // records this on the blob's properties and returns it on
        // HEAD/GetProperties; the Phase 6.17 cloud-side integrity
        // sweep cross-checks it against a fresh re-hash to detect a
        // post-PUT tamper (SAS-replay overwrite, op-error during
        // hot-rehydrate, etc.). Note the `x-ms-blob-*` family of
        // content-MD5 headers is RECORDED only — Azure does NOT
        // reject the PUT on mismatch the way the bare `Content-MD5`
        // header would. We deliberately use `x-ms-blob-content-md5`
        // (record, never reject) instead of `Content-MD5` (reject on
        // mismatch) because the SHA-256 pre-verification block above
        // is already a stronger upload-side check, and we want
        // Azure's record of the MD5 even when the body is identical
        // to the on-disk bytes we computed against.
        let content_md5 = content_md5_b64(bytes);
        let resp = self
            .http
            .put(&sas.url)
            .header("x-ms-blob-type", "BlockBlob")
            .header("x-ms-version", AZURE_API_VERSION)
            .header(format!("x-ms-meta-{AZURE_SHA256_META}"), expected_sha256)
            .header("x-ms-blob-content-md5", &content_md5)
            .header("content-length", bytes.len().to_string())
            .body(bytes.to_vec())
            .send()
            .await
            .map_err(|e| BackendError::Unreachable(format!("azure PUT: {e}")))?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(BackendError::Other(format!(
                "azure PUT {} returned {status}: {body}",
                sas.blob_url_unsigned
            )));
        }

        Ok(PutReceipt {
            cold_path: sas.blob_path,
            uploaded_at: Utc::now(),
            bytes_written: bytes.len() as u64,
            // Phase 2 \u00b7 Step 2.8 \u2014 the cold replicator stamps this into
            // `ClipReplicatedPayload.blob_url` so the cloud's `clips`
            // table records an absolute, browser-fetchable URL (the
            // SAS query is omitted by definition; this is the bare
            // `blob_url_unsigned` minted by the SAS issuer).
            cold_url: Some(sas.blob_url_unsigned),
        })
    }

    async fn get_range(
        &self,
        path: &str,
        start: u64,
        end_inclusive: u64,
    ) -> Result<Vec<u8>, BackendError> {
        let edge_clip_id = extract_edge_clip_id(path)?;
        let sas = self.issuer.issue_get(&edge_clip_id).await?;

        let resp = self
            .http
            .get(&sas.url)
            .header("x-ms-version", AZURE_API_VERSION)
            .header("range", format!("bytes={start}-{end_inclusive}"))
            .send()
            .await
            .map_err(|e| BackendError::Unreachable(format!("azure GET: {e}")))?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(BackendError::Other(format!(
                "azure GET {} returned {status}: {body}",
                sas.blob_url_unsigned
            )));
        }

        resp.bytes()
            .await
            .map(|b| b.to_vec())
            .map_err(|e| BackendError::Other(format!("azure GET body: {e}")))
    }

    async fn get_range_stream(
        &self,
        path: &str,
        start: u64,
        end_inclusive: u64,
    ) -> Result<ByteStream, BackendError> {
        let edge_clip_id = extract_edge_clip_id(path)?;
        let sas = self.issuer.issue_get(&edge_clip_id).await?;

        let resp = self
            .http
            .get(&sas.url)
            .header("x-ms-version", AZURE_API_VERSION)
            .header("range", format!("bytes={start}-{end_inclusive}"))
            .send()
            .await
            .map_err(|e| BackendError::Unreachable(format!("azure GET stream: {e}")))?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(BackendError::Other(format!(
                "azure GET {} returned {status}: {body}",
                sas.blob_url_unsigned
            )));
        }

        // Wire the reqwest streaming body straight through —
        // matches what `OneDriveBackend::get_range_stream` does.
        let stream = resp
            .bytes_stream()
            .map_err(|e| BackendError::Other(format!("azure GET chunk: {e}")))
            .boxed();
        Ok(stream)
    }

    async fn delete(&self, _path: &str) -> Result<bool, BackendError> {
        // Phase 2 · Step 2.1 does not implement delete. The
        // api-gateway's `/v1/internal/cores/:core_id/blob-sas`
        // endpoint only mints `put` and `get` SAS kinds today; a
        // future Phase 2.x change will add `delete` + an operator
        // flow in the console UI. The cold replicator NEVER calls
        // delete (operator-driven only), so returning Other here
        // doesn't affect the happy path.
        Err(BackendError::Other(
            "AzureBlobBackend.delete: not implemented in Phase 2.1; \
             requires a DELETE SAS kind tracked for Phase 2.x"
                .to_string(),
        ))
    }

    async fn exists(&self, path: &str, expected_sha256: &str) -> Result<bool, BackendError> {
        let edge_clip_id = extract_edge_clip_id(path)?;
        let sas = self.issuer.issue_get(&edge_clip_id).await?;

        // HEAD against a GET SAS is supported by Azure Blob.
        let resp = self
            .http
            .head(&sas.url)
            .header("x-ms-version", AZURE_API_VERSION)
            .send()
            .await
            .map_err(|e| BackendError::Unreachable(format!("azure HEAD: {e}")))?;

        let status = resp.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            return Ok(false);
        }
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(BackendError::Other(format!(
                "azure HEAD {} returned {status}: {body}",
                sas.blob_url_unsigned
            )));
        }

        // Azure normalises metadata keys to lowercase + replaces
        // dashes with underscores on echo. Try both spellings so a
        // future SDK shift doesn't silently fail every existence
        // check.
        let hdr_dash = format!("x-ms-meta-{}", AZURE_SHA256_META);
        let hdr_underscore = format!("x-ms-meta-{}", AZURE_SHA256_META.replace('-', "_"));
        let stored = resp
            .headers()
            .get(&hdr_dash)
            .or_else(|| resp.headers().get(&hdr_underscore))
            .and_then(|v| v.to_str().ok())
            .map(str::to_lowercase);

        match stored {
            Some(s) if s == expected_sha256.to_lowercase() => Ok(true),
            Some(s) => {
                warn!(
                    blob = %sas.blob_url_unsigned,
                    expected = %expected_sha256,
                    actual = %s,
                    "AzureBlobBackend.exists: sha256 mismatch — treating as 'does not exist' so \
                     replicator re-uploads",
                );
                Ok(false)
            }
            None => {
                debug!(
                    blob = %sas.blob_url_unsigned,
                    "AzureBlobBackend.exists: blob present but no nexus-sha256 metadata; \
                     treating as 'does not exist' so replicator re-uploads with metadata",
                );
                Ok(false)
            }
        }
    }

    async fn volume_info(&self) -> Result<VolumeInfo, BackendError> {
        // Azure Blob doesn't expose a per-container quota cheaply;
        // the account-level limit is 5 PiB by default but knowing
        // the *used* amount requires a (very expensive) full
        // account-level metrics query. The replicator never makes
        // routing decisions off these numbers (cloud is treated as
        // effectively unbounded), so we return all-None.
        Ok(VolumeInfo {
            free_bytes: None,
            total_bytes: None,
            used_bytes: None,
        })
    }

    async fn health(&self) -> HealthStatus {
        // Cheapest meaningful liveness probe: request a GET SAS for
        // a sentinel id. The request exercises the whole stack —
        // mTLS to gateway, gateway → api-gateway internal route, KV
        // secret lookup, Postgres role hop, HMAC signing — without
        // actually accessing any blob. The returned SAS is
        // discarded.
        match self.issuer.issue_get("_health_probe").await {
            Ok(_) => HealthStatus::Ok,
            Err(BackendError::Auth(r)) => HealthStatus::Unreachable {
                reason: format!("auth: {r}"),
            },
            Err(BackendError::Unreachable(r)) => HealthStatus::Unreachable { reason: r },
            Err(e) => HealthStatus::Unreachable {
                reason: e.to_string(),
            },
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Compute the base64-encoded MD5 of `bytes` for the `x-ms-blob-content-md5`
/// Azure Blob storage header. Azure records this on the blob and returns it on
/// GetProperties / HEAD, so the Phase 6.17 cloud-side integrity sweep can
/// cross-check it against a fresh re-hash of the body without trusting our
/// custom `x-ms-meta-nexus-sha256` metadata alone (defence-in-depth — an
/// attacker who replays a SAS PUT must forge both an MD5 *and* a SHA-256 for
/// the same body, which is computationally a no-op only if MD5 collision
/// generation maps onto the SHA-256 preimage problem).
///
/// Phase 2 \u00b7 Step 2.8.
fn content_md5_b64(bytes: &[u8]) -> String {
    let digest = md5::Md5::digest(bytes);
    base64::engine::general_purpose::STANDARD.encode(digest)
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn extract_edge_clip_id_strips_prefix_and_extension() {
        assert_eq!(
            extract_edge_clip_id("cam1/1700000000_15000.mp4").unwrap(),
            "1700000000_15000"
        );
        assert_eq!(
            extract_edge_clip_id("/cam1/clip-42.mp4").unwrap(),
            "clip-42"
        );
        assert_eq!(extract_edge_clip_id("flat.mp4").unwrap(), "flat");
    }

    #[test]
    fn extract_edge_clip_id_rejects_traversal() {
        assert!(extract_edge_clip_id("cam1/../etc/passwd").is_err());
        assert!(extract_edge_clip_id("../../escape.mp4").is_err());
    }

    #[test]
    fn extract_edge_clip_id_rejects_disallowed_chars() {
        assert!(extract_edge_clip_id("cam1/has space.mp4").is_err());
        assert!(extract_edge_clip_id("cam1/has;semi.mp4").is_err());
        assert!(extract_edge_clip_id("cam1/has?query.mp4").is_err());
    }

    #[test]
    fn extract_edge_clip_id_rejects_empty() {
        assert!(extract_edge_clip_id("").is_err());
        assert!(extract_edge_clip_id(".mp4").is_err());
        assert!(extract_edge_clip_id("/").is_err());
    }

    #[derive(Debug, Default)]
    struct MockIssuer {
        // (edge_clip_id, kind)
        calls: Mutex<Vec<(String, &'static str)>>,
        // Override the put response.
        put_response: Mutex<Option<IssuedSas>>,
        // Override the get response.
        get_response: Mutex<Option<IssuedSas>>,
        // Force an Auth error from issue_get.
        get_auth_err: Mutex<bool>,
    }

    impl MockIssuer {
        fn new() -> Self {
            Self::default()
        }
    }

    #[async_trait]
    impl SasIssuer for MockIssuer {
        async fn issue_put(&self, edge_clip_id: &str) -> Result<IssuedSas, BackendError> {
            self.calls
                .lock()
                .unwrap()
                .push((edge_clip_id.to_string(), "put"));
            if let Some(resp) = self.put_response.lock().unwrap().clone() {
                return Ok(resp);
            }
            Ok(IssuedSas {
                url: format!("https://example.test/blob/{edge_clip_id}.mp4?sv=fake"),
                blob_url_unsigned: format!("https://example.test/blob/{edge_clip_id}.mp4"),
                expires_at: Utc::now() + chrono::Duration::minutes(15),
                container: "clips".into(),
                blob_path: format!("org-x/core-y/{edge_clip_id}.mp4"),
            })
        }

        async fn issue_get(&self, edge_clip_id: &str) -> Result<IssuedSas, BackendError> {
            self.calls
                .lock()
                .unwrap()
                .push((edge_clip_id.to_string(), "get"));
            if *self.get_auth_err.lock().unwrap() {
                return Err(BackendError::Auth("mock revoked".into()));
            }
            if let Some(resp) = self.get_response.lock().unwrap().clone() {
                return Ok(resp);
            }
            Ok(IssuedSas {
                url: format!("https://example.test/blob/{edge_clip_id}.mp4?sv=fake"),
                blob_url_unsigned: format!("https://example.test/blob/{edge_clip_id}.mp4"),
                expires_at: Utc::now() + chrono::Duration::minutes(15),
                container: "clips".into(),
                blob_path: format!("org-x/core-y/{edge_clip_id}.mp4"),
            })
        }
    }

    fn build_backend(issuer: Arc<dyn SasIssuer>) -> AzureBlobBackend {
        AzureBlobBackend::new(
            "azure-test",
            issuer,
            reqwest::Client::builder().build().unwrap(),
        )
    }

    #[tokio::test]
    async fn handle_and_kind_match_construction() {
        let b = build_backend(Arc::new(MockIssuer::new()));
        assert_eq!(b.handle(), "azure-test");
        assert_eq!(b.kind(), "azure");
    }

    #[tokio::test]
    async fn put_rejects_sha256_mismatch_before_upload() {
        let issuer = Arc::new(MockIssuer::new());
        let b = build_backend(issuer.clone());
        let bytes = b"hello world";
        let wrong = "0".repeat(64);
        let err = b
            .put("cam1/clip1.mp4", bytes, &wrong)
            .await
            .expect_err("must fail");
        match err {
            BackendError::ChecksumMismatch { expected, actual } => {
                assert_eq!(expected, wrong);
                assert_ne!(actual, wrong);
            }
            other => panic!("expected ChecksumMismatch, got {other:?}"),
        }
        // Issuer was called once (PUT SAS minted) before the
        // sha256 spot-check fired. That's OK — SAS issuance is
        // cheap, the cleanup happens because the SAS just expires
        // unused.
        assert_eq!(issuer.calls.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn put_rejects_blob_path_mismatch() {
        let issuer = Arc::new(MockIssuer::new());
        *issuer.put_response.lock().unwrap() = Some(IssuedSas {
            url: "https://example.test/blob/WRONG.mp4?sv=fake".into(),
            blob_url_unsigned: "https://example.test/blob/WRONG.mp4".into(),
            expires_at: Utc::now() + chrono::Duration::minutes(15),
            container: "clips".into(),
            blob_path: "org-x/core-y/WRONG.mp4".into(),
        });
        let b = build_backend(issuer);
        let bytes = b"hello";
        let sha = hex::encode(Sha256::digest(bytes));
        let err = b
            .put("cam1/right_id.mp4", bytes, &sha)
            .await
            .expect_err("must fail");
        match err {
            BackendError::Other(s) => assert!(s.contains("WRONG.mp4")),
            other => panic!("expected Other, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn health_returns_unreachable_on_auth_error() {
        let issuer = Arc::new(MockIssuer::new());
        *issuer.get_auth_err.lock().unwrap() = true;
        let b = build_backend(issuer);
        match b.health().await {
            HealthStatus::Unreachable { reason } => {
                assert!(reason.contains("auth"), "got {reason}");
            }
            other => panic!("expected Unreachable, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn volume_info_returns_all_none() {
        let b = build_backend(Arc::new(MockIssuer::new()));
        let v = b.volume_info().await.unwrap();
        assert_eq!(v.free_bytes, None);
        assert_eq!(v.total_bytes, None);
        assert_eq!(v.used_bytes, None);
    }

    #[tokio::test]
    async fn delete_returns_not_implemented_error() {
        let b = build_backend(Arc::new(MockIssuer::new()));
        let err = b.delete("cam1/clip1.mp4").await.expect_err("must fail");
        match err {
            BackendError::Other(s) => assert!(s.to_lowercase().contains("not implemented")),
            other => panic!("expected Other, got {other:?}"),
        }
    }

    // Integration test against a real wiremock-backed "Azure" — the
    // SAS issuer is mocked, the wiremock plays the role of Azure
    // Blob Storage. This is the closest thing to a real end-to-end
    // upload we can do without a live storage account.
    mod wiremock_roundtrip {
        use super::*;
        use wiremock::matchers::{header, header_exists, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        #[tokio::test]
        async fn put_uploads_bytes_with_required_headers_and_metadata() {
            let mock = MockServer::start().await;

            // Azure responds 201 Created to a BlockBlob PUT.
            Mock::given(method("PUT"))
                .and(path("/blob/clip-42.mp4"))
                .and(header("x-ms-blob-type", "BlockBlob"))
                .and(header("x-ms-version", AZURE_API_VERSION))
                .and(header_exists(format!("x-ms-meta-{AZURE_SHA256_META}")))
                .respond_with(ResponseTemplate::new(201))
                .mount(&mock)
                .await;

            // Mount a default "everything else fails" so a
            // header-mismatch shows up as a 404 instead of silently
            // accepting.
            let issuer = Arc::new(MockIssuer::new());
            *issuer.put_response.lock().unwrap() = Some(IssuedSas {
                url: format!("{}/blob/clip-42.mp4?sv=fake", mock.uri()),
                blob_url_unsigned: format!("{}/blob/clip-42.mp4", mock.uri()),
                expires_at: Utc::now() + chrono::Duration::minutes(15),
                container: "clips".into(),
                blob_path: "org-x/core-y/clip-42.mp4".into(),
            });

            let b = build_backend(issuer);
            let bytes = b"some clip bytes";
            let sha = hex::encode(Sha256::digest(bytes));
            let receipt = b
                .put("cam1/clip-42.mp4", bytes, &sha)
                .await
                .expect("PUT must succeed");
            assert_eq!(receipt.cold_path, "org-x/core-y/clip-42.mp4");
            assert_eq!(receipt.bytes_written, bytes.len() as u64);
        }

        #[tokio::test]
        async fn get_range_sends_range_header_and_returns_bytes() {
            let mock = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/blob/clip-42.mp4"))
                .and(header("range", "bytes=10-19"))
                .respond_with(ResponseTemplate::new(206).set_body_bytes(&b"ten-bytes!"[..]))
                .mount(&mock)
                .await;

            let issuer = Arc::new(MockIssuer::new());
            *issuer.get_response.lock().unwrap() = Some(IssuedSas {
                url: format!("{}/blob/clip-42.mp4?sv=fake", mock.uri()),
                blob_url_unsigned: format!("{}/blob/clip-42.mp4", mock.uri()),
                expires_at: Utc::now() + chrono::Duration::minutes(15),
                container: "clips".into(),
                blob_path: "org-x/core-y/clip-42.mp4".into(),
            });

            let b = build_backend(issuer);
            let buf = b
                .get_range("cam1/clip-42.mp4", 10, 19)
                .await
                .expect("GET must succeed");
            assert_eq!(buf, b"ten-bytes!");
        }

        #[tokio::test]
        async fn exists_returns_false_on_404_without_error() {
            let mock = MockServer::start().await;
            Mock::given(method("HEAD"))
                .and(path("/blob/clip-42.mp4"))
                .respond_with(ResponseTemplate::new(404))
                .mount(&mock)
                .await;

            let issuer = Arc::new(MockIssuer::new());
            *issuer.get_response.lock().unwrap() = Some(IssuedSas {
                url: format!("{}/blob/clip-42.mp4?sv=fake", mock.uri()),
                blob_url_unsigned: format!("{}/blob/clip-42.mp4", mock.uri()),
                expires_at: Utc::now() + chrono::Duration::minutes(15),
                container: "clips".into(),
                blob_path: "org-x/core-y/clip-42.mp4".into(),
            });

            let b = build_backend(issuer);
            let sha = hex::encode(Sha256::digest(b"x"));
            assert!(!b.exists("cam1/clip-42.mp4", &sha).await.unwrap());
        }

        #[tokio::test]
        async fn exists_returns_true_on_matching_metadata() {
            let mock = MockServer::start().await;
            let bytes = b"some clip bytes";
            let sha = hex::encode(Sha256::digest(bytes));
            let sha_for_header = sha.clone();
            Mock::given(method("HEAD"))
                .and(path("/blob/clip-42.mp4"))
                .respond_with(ResponseTemplate::new(200).insert_header(
                    format!("x-ms-meta-{}", AZURE_SHA256_META.replace('-', "_")).as_str(),
                    sha_for_header.as_str(),
                ))
                .mount(&mock)
                .await;

            let issuer = Arc::new(MockIssuer::new());
            *issuer.get_response.lock().unwrap() = Some(IssuedSas {
                url: format!("{}/blob/clip-42.mp4?sv=fake", mock.uri()),
                blob_url_unsigned: format!("{}/blob/clip-42.mp4", mock.uri()),
                expires_at: Utc::now() + chrono::Duration::minutes(15),
                container: "clips".into(),
                blob_path: "org-x/core-y/clip-42.mp4".into(),
            });

            let b = build_backend(issuer);
            assert!(b.exists("cam1/clip-42.mp4", &sha).await.unwrap());
        }

        #[tokio::test]
        async fn gateway_issuer_returns_auth_on_403() {
            let mock = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/v1/edge/blob-sas"))
                .respond_with(ResponseTemplate::new(403).set_body_string("cert revoked"))
                .mount(&mock)
                .await;

            let issuer = GatewaySasIssuer::new(reqwest::Client::new(), mock.uri());
            let err = issuer.issue_put("clip-42").await.expect_err("must fail");
            match err {
                BackendError::Auth(s) => assert!(s.contains("cert revoked"), "got {s}"),
                other => panic!("expected Auth, got {other:?}"),
            }
        }

        #[tokio::test]
        async fn gateway_issuer_round_trip() {
            let mock = MockServer::start().await;
            let body = serde_json::json!({
                "url": "https://example.test/blob/clip-42.mp4?sv=signed",
                "blob_url_unsigned": "https://example.test/blob/clip-42.mp4",
                "expires_at": "2030-01-01T00:00:00Z",
                "container": "clips",
                "blob_path": "org-x/core-y/clip-42.mp4",
            });
            Mock::given(method("POST"))
                .and(path("/v1/edge/blob-sas"))
                .respond_with(ResponseTemplate::new(200).set_body_json(body))
                .mount(&mock)
                .await;

            let issuer = GatewaySasIssuer::new(reqwest::Client::new(), mock.uri());
            let sas = issuer.issue_get("clip-42").await.expect("issue ok");
            assert_eq!(sas.container, "clips");
            assert_eq!(sas.blob_path, "org-x/core-y/clip-42.mp4");
            assert!(sas.url.contains("sv=signed"));
        }
    }

    #[test]
    fn content_md5_b64_is_stable() {
        // The md5_compat helper is a sha256 truncation, but the
        // function MUST be deterministic for the same input.
        let a = content_md5_b64(b"hello");
        let b = content_md5_b64(b"hello");
        assert_eq!(a, b);
        assert_ne!(content_md5_b64(b"hello"), content_md5_b64(b"world"));
    }
}
