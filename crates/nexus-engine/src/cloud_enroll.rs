//! `nexus-engine enroll` — one-shot enrollment flow against
//! cloud-side `enrollment-svc /v1/enroll`.
//!
//! Sequence:
//! 1. Generate a per-core ECDSA P-256 keypair + PKCS#10 CSR locally.
//! 2. Derive a stable hardware fingerprint.
//! 3. POST `EnrollmentRequest { code, csr_pem, fingerprint }` to the
//!    configured `--cloud-host`.
//! 4. Persist the returned bundle (mTLS leaf + CA chain + entitlement
//!    JWT + signing-key forward-compat fields) into the local
//!    `cloud_enrollment` row, replacing any previous enrollment.
//! 5. Print a one-line success banner.
//!
//! On next boot, `serve` reads that row and spawns the WSS tunnel.
//!
//! Lives in its own module so the serve path's startup graph stays
//! untouched.

use anyhow::{anyhow, Context, Result};
use nexus_cloud_client::{generate_keypair_and_csr, EnrollmentClient, EnrollmentRequest};
use nexus_config::Config;
use nexus_store::cloud::CloudEnrollment;
use nexus_store::Store;
use tracing::info;

use crate::time_sync;

/// CLI args for `nexus-engine enroll`.
#[derive(Debug, Clone, clap::Args)]
pub struct EnrollArgs {
    /// Short single-use enrollment code minted from the cloud console
    /// ("Add Core" flow). E.g. `XJ4K-PMQ7-9NAB`.
    #[arg(long)]
    pub code: String,

    /// Cloud-console base URL. Must include scheme.
    /// E.g. `https://cloud.example`.
    #[arg(long)]
    pub cloud_host: String,

    /// Human-friendly label baked into the CSR's CommonName.
    /// Defaults to the local hostname.
    #[arg(long)]
    pub label: Option<String>,

    /// Phase 2 · Step 2.9 / ARCHITECTURE §21.2 — replay the local
    /// motion-clip backlog into the cloud after enrollment. On the
    /// next `nexus-engine serve` boot the engine waits for the WSS
    /// tunnel to come up and then re-sends every clip recorded
    /// since `now - history_days` as a `clip_replicated` envelope
    /// with `attached_history: true`. The cloud renders an
    /// "imported" badge and suppresses notify-svc fan-out for these.
    ///
    /// Default: off. Most M7-migration customers will not want
    /// pre-cloud noise in their fresh console.
    #[arg(long, default_value_t = false)]
    pub keep_history: bool,

    /// Phase 2 · Step 2.9 — how many days of local history to replay
    /// when `--keep-history` is set. Defaults to 30 (the same number
    /// the spec calls out in ARCHITECTURE.md §21.2). Capped at 365
    /// to keep the replay window bounded; values above 365 are
    /// clamped with a warning.
    #[arg(long, default_value_t = 30)]
    pub history_days: u32,
}

/// Run the enrollment subcommand. Loads the store from `cfg.store`,
/// hits enrollment-svc, persists, exits.
pub async fn run_enroll(cfg: &Config, args: &EnrollArgs) -> Result<()> {
    let store = Store::open(&cfg.store).await.context("open local store")?;
    let enrolled = perform_enrollment(
        &store,
        &args.code,
        &args.cloud_host,
        args.label.as_deref(),
        args.keep_history,
        args.history_days,
    )
    .await?;
    // Banner. Use eprintln so it shows up even if telemetry is
    // routed to a file.
    eprintln!(
        "nexus-engine: enrolled as core_id={} gateway_url={}",
        enrolled.core_id, enrolled.gateway_url,
    );
    Ok(())
}

/// Engine-side enrollment flow shared between the
/// `nexus-engine enroll` CLI subcommand and the
/// `POST /v1/admin/cloud/enroll` HTTP handler.
///
/// Generates a fresh per-core ECDSA P-256 keypair + CSR, derives a stable
/// hardware fingerprint, POSTs the enrollment request to
/// `<cloud_host>/v1/enroll`, and persists the response into the local
/// `cloud_enrollment` row (replacing any previous enrollment).
///
/// The returned [`CloudEnrollment`] is the persisted row, useful for
/// the HTTP handler that wants to echo `core_id`/`gateway_url`/
/// `enrolled_at` back to the operator without a follow-up SELECT.
///
/// # Errors
///
/// * Time-sync gate fails (clock not synced, no `NEXUS_TIME_SYNC_OVERRIDE`).
/// * CSR generation fails.
/// * Cloud-side `/v1/enroll` returns a non-2xx status or a malformed body.
/// * Persisting the `cloud_enrollment` row fails.
pub async fn perform_enrollment(
    store: &Store,
    code: &str,
    cloud_host: &str,
    label: Option<&str>,
    keep_history: bool,
    history_days: u32,
) -> Result<CloudEnrollment> {
    // Phase 1.15 — refuse to enroll a box whose clock isn't synced.
    // The actor_token verifier has a ±30 s skew window; a freshly
    // imaged appliance with a stale RTC will mint tokens the cloud
    // rejects on every mutating RPC. Surface this NOW while a human
    // is watching the console. `NEXUS_TIME_SYNC_OVERRIDE=allow_unsynced`
    // bypasses for offline-lab / PTP / GPS sources.
    let ts = time_sync::require_synchronized().map_err(|e| anyhow!(e))?;
    info!(
        time_sync_state = %ts.state.as_str(),
        time_skew_ms = ?ts.skew_ms,
        "clock sync verified — proceeding with enrollment"
    );

    let label = label
        .map(str::to_string)
        .unwrap_or_else(|| hostname_or("nexus-edge"));

    // 1. Local keypair + CSR.
    let csr = generate_keypair_and_csr(&label).context("generate CSR")?;
    info!(label = %label, "generated local ECDSA P-256 keypair + CSR");

    // 2. Hardware fingerprint. Best-effort; the cloud accepts any
    //    stable opaque string today. Salt with the CSR's public key
    //    SHA-256 so even a perfectly cloned VM ends up with a distinct
    //    fingerprint per enrollment attempt.
    let fingerprint = compute_fingerprint(&csr.csr_pem);
    info!(fingerprint = %fingerprint, "derived hardware fingerprint");

    // 3. POST.
    let client = EnrollmentClient::new(cloud_host);
    let req = EnrollmentRequest {
        code: code.to_string(),
        csr_pem: csr.csr_pem.clone(),
        fingerprint,
    };
    let resp = client
        .enroll(&req)
        .await
        .context("POST /v1/enroll failed")?;
    info!(
        core_id = %resp.core_id,
        gateway_url = %resp.gateway_url,
        "enrollment accepted by cloud",
    );

    // 4. Persist. Phase 2 · Step 2.9 — when `keep_history` is set,
    //    record the replay cutoff alongside the enrollment so the
    //    next `serve` boot's attach-replay task knows to drain the
    //    local backlog through the cloud outbox. The JWT's `iat` is
    //    approximately `now()` (the cloud just minted it), so we
    //    don't bother parsing the JWT — `Utc::now() - history_days`
    //    is within milliseconds of `iat - history_days` and the
    //    cloud-side `ON CONFLICT (core_id, edge_clip_id) DO UPDATE`
    //    upsert tolerates any minor over-replay.
    let attach_replay_after = if keep_history {
        let mut days = history_days;
        if days > 365 {
            tracing::warn!(history_days = days, "history_days capped at 365; clamping");
            days = 365;
        }
        let cutoff = chrono::Utc::now() - chrono::Duration::days(i64::from(days));
        info!(
            history_days = days,
            cutoff = %cutoff.to_rfc3339(),
            "keep_history set: local clip backlog since cutoff will be replayed on next serve"
        );
        Some(cutoff)
    } else {
        None
    };

    let persisted = CloudEnrollment {
        core_id: resp.core_id.clone(),
        gateway_url: resp.gateway_url.clone(),
        cert_pem: resp.cert_pem,
        private_key_pem: csr.private_key_pem,
        ca_chain_pem: resp.ca_chain_pem,
        entitlement_jwt: resp.entitlement_jwt,
        signing_key_pem: resp.entitlement_signing_key_pem,
        signing_kid: resp.entitlement_signing_kid,
        enrolled_at: chrono::Utc::now(),
        attach_replay_after,
    };
    store
        .set_cloud_enrollment(&persisted)
        .await
        .context("persist cloud_enrollment row")?;
    Ok(persisted)
}

fn hostname_or(default: &str) -> String {
    hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .unwrap_or_else(|| default.to_string())
}

fn compute_fingerprint(csr_pem: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(csr_pem.as_bytes());
    if let Some(hn) = hostname::get().ok().and_then(|h| h.into_string().ok()) {
        h.update(hn.as_bytes());
    }
    let digest = h.finalize();
    format!("sha256:{}", hex_lower(&digest))
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}
