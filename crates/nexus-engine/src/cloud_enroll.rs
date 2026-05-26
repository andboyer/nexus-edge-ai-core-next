//! `nexus-engine enroll` — one-shot enrollment flow against
//! cloud-side `enrollment-svc /v1/enroll`.
//!
//! Sequence:
//! 1. Generate a per-core Ed25519 keypair + PKCS#10 CSR locally.
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

use anyhow::{Context, Result};
use nexus_cloud_client::{generate_keypair_and_csr, EnrollmentClient, EnrollmentRequest};
use nexus_config::Config;
use nexus_store::cloud::CloudEnrollment;
use nexus_store::Store;
use tracing::info;

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
}

/// Run the enrollment subcommand. Loads the store from `cfg.store`,
/// hits enrollment-svc, persists, exits.
pub async fn run_enroll(cfg: &Config, args: &EnrollArgs) -> Result<()> {
    let label = args
        .label
        .clone()
        .unwrap_or_else(|| hostname_or("nexus-edge"));

    // 1. Local keypair + CSR.
    let csr = generate_keypair_and_csr(&label).context("generate CSR")?;
    info!(label = %label, "generated local Ed25519 keypair + CSR");

    // 2. Hardware fingerprint. Best-effort; the cloud accepts any
    //    stable opaque string today. Salt with the CSR's public key
    //    SHA-256 so even a perfectly cloned VM ends up with a distinct
    //    fingerprint per enrollment attempt.
    let fingerprint = compute_fingerprint(&csr.csr_pem);
    info!(fingerprint = %fingerprint, "derived hardware fingerprint");

    // 3. POST.
    let client = EnrollmentClient::new(&args.cloud_host);
    let req = EnrollmentRequest {
        code: args.code.clone(),
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

    // 4. Persist.
    let store = Store::open(&cfg.store).await.context("open local store")?;
    store
        .set_cloud_enrollment(&CloudEnrollment {
            core_id: resp.core_id.clone(),
            gateway_url: resp.gateway_url.clone(),
            cert_pem: resp.cert_pem,
            private_key_pem: csr.private_key_pem,
            ca_chain_pem: resp.ca_chain_pem,
            entitlement_jwt: resp.entitlement_jwt,
            signing_key_pem: resp.entitlement_signing_key_pem,
            signing_kid: resp.entitlement_signing_kid,
            enrolled_at: chrono::Utc::now(),
        })
        .await
        .context("persist cloud_enrollment row")?;

    // 5. Banner. Use eprintln so it shows up even if telemetry is
    //    routed to a file.
    eprintln!(
        "nexus-engine: enrolled as core_id={} gateway_url={}",
        resp.core_id, resp.gateway_url,
    );
    Ok(())
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
