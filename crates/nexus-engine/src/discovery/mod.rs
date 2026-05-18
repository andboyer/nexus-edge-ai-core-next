//! Camera discovery for M-Admin Phase 1B.
//!
//! Two probe modes share a single in-memory **session registry**:
//!
//! * **`scan`** — bounded-concurrency CIDR sweep that TCP-probes
//!   ports 554 (RTSP `OPTIONS`) + 80 / 8080 (ONVIF SOAP
//!   `GetDeviceInformation`). Implemented in [`scan`].
//! * **`onvif`** — WS-Discovery Probe over UDP multicast. Stubbed
//!   in this commit (Step C); [`onvif::run_session`] currently
//!   marks the session done with zero devices.
//!
//! Plus a third, **inline** endpoint:
//!
//! * **`probe-rtsp`** — `OPTIONS` + `DESCRIBE` against a single
//!   `(host, port, path, optional creds)` to confirm a stream is
//!   reachable before the operator clicks **Add** in the UI.
//!   Stubbed in this commit (Step D); [`rtsp_probe::probe`]
//!   currently returns `ok=false, status=0`.
//!
//! All four HTTP routes are admin-gated by the existing
//! [`crate::admin_auth`] layer. See
//! [`docs/M_ADMIN.md`](../../../../docs/M_ADMIN.md#phase-1b--camera-discovery-onvif--ip-scan)
//! for the design rationale, the engine-side CIDR guardrails
//! (reject `< /22` unconditionally, require `confirm: true` for
//! `/22`), and the audit-log contract.

pub mod onvif;
pub mod rtsp_probe;
pub mod scan;

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::api::{ApiError, ApiState};

/// How long a finished session lives before the eviction sweep
/// drops it. Matches the figure documented in
/// `docs/M_ADMIN.md` (10 min).
pub const SESSION_TTL: Duration = Duration::from_secs(10 * 60);

/// How often the background sweep checks for expired sessions.
/// Cheap (DashMap iteration); a generous interval keeps idle CPU
/// at zero.
pub const SWEEP_INTERVAL: Duration = Duration::from_secs(60);

/// Smallest CIDR prefix the operator may scan WITHOUT supplying
/// `confirm: true`. Anything stricter than `/22` is rejected
/// unconditionally — even with `confirm: true`. A `/22` is
/// 1024 hosts, ~16 s at 64-way concurrency and the 250 ms median
/// per-host RTT we budget for.
pub const MIN_PREFIX_LEN: u8 = 22;
pub const CONFIRM_REQUIRED_AT_PREFIX_LEN: u8 = 22;

// ---------------------------------------------------------------------------
// Wire types — kept in lock-step with the TypeScript declarations
// in `ui/src/api/types.ts` (M_ADMIN.md §Wire types).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum DiscoveryKind {
    Onvif,
    Scan,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum DiscoveryState {
    Running,
    Done,
    Error,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum DeviceKind {
    /// RTSP `OPTIONS` returned a `Public:` line.
    Rtsp,
    /// ONVIF SOAP `GetDeviceInformation` returned a fault or 200,
    /// OR WS-Discovery ProbeMatch was received.
    Onvif,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveredDevice {
    pub ip: String,
    pub port: u16,
    pub kind: DeviceKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vendor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hardware: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub firmware: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mac: Option<String>,
    /// RTSP paths the operator can plug straight into a camera
    /// URL. Populated by the inline probe-rtsp handler post-Verify;
    /// empty on initial probe.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub rtsp_paths: Vec<String>,
}

/// Request body for `POST /api/v1/admin/discovery/scan`.
#[derive(Debug, Clone, Deserialize)]
pub struct ScanReq {
    pub cidr: String,
    /// Defaults to `[554, 80, 8080]` when missing.
    #[serde(default)]
    pub ports: Option<Vec<u16>>,
    /// Defaults to 64 when missing.
    #[serde(default)]
    pub concurrency: Option<usize>,
    /// Required to scan `/22`; ignored on `/23` and stricter.
    #[serde(default)]
    pub confirm: Option<bool>,
}

/// Response body for `POST /api/v1/admin/discovery/scan` (and
/// the ONVIF variant, with `total_targets: None`).
#[derive(Debug, Clone, Serialize)]
pub struct SessionCreatedResp {
    pub session_id: Uuid,
    pub started_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_targets: Option<u32>,
}

/// Wire shape returned by `GET /api/v1/admin/discovery/sessions/:id`.
#[derive(Debug, Clone, Serialize)]
pub struct DiscoverySessionView {
    pub session_id: Uuid,
    pub state: DiscoveryState,
    pub kind: DiscoveryKind,
    pub started_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_targets: Option<u32>,
    pub scanned: u32,
    pub found: Vec<DiscoveredDevice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProbeRtspReq {
    pub host: String,
    pub port: u16,
    pub path: String,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SdpStream {
    pub codec: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolution: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub control: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProbeRtspResult {
    pub ok: bool,
    pub status: u16,
    pub sdp_streams: Vec<SdpStream>,
}

// ---------------------------------------------------------------------------
// Session registry
// ---------------------------------------------------------------------------

/// Mutable per-session state held under a single
/// `parking_lot::Mutex` inside the registry. Cheap to lock; the
/// only writers are (a) the spawned discovery task appending
/// devices and (b) the eviction sweep.
#[derive(Debug)]
pub(crate) struct SessionInner {
    pub state: DiscoveryState,
    pub kind: DiscoveryKind,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<Instant>,
    pub total_targets: Option<u32>,
    pub scanned: u32,
    pub found: Vec<DiscoveredDevice>,
    pub error: Option<String>,
}

impl SessionInner {
    fn snapshot(&self, session_id: Uuid) -> DiscoverySessionView {
        DiscoverySessionView {
            session_id,
            state: self.state,
            kind: self.kind,
            started_at: self.started_at,
            total_targets: self.total_targets,
            scanned: self.scanned,
            found: self.found.clone(),
            error: self.error.clone(),
        }
    }
}

/// Lock-free registry shared via `ApiState`. The handler clones
/// the `Arc<DiscoverySessions>` into spawned tasks so the per-
/// session task can update progress without holding any borrow
/// on the request handler.
#[derive(Debug, Clone, Default)]
pub struct DiscoverySessions {
    inner: Arc<DashMap<Uuid, Arc<Mutex<SessionInner>>>>,
}

impl DiscoverySessions {
    /// Construct an empty registry. Equivalent to
    /// `DiscoverySessions::default()` but provided as a named
    /// constructor so call sites (e.g. `ApiState` setup in
    /// `main.rs`) read more clearly.
    pub fn new() -> Self {
        Self::default()
    }

    /// Inserts a new running session and returns the id +
    /// the shared `Arc<Mutex<...>>` the spawned task should
    /// hold to push progress.
    pub(crate) fn start(
        &self,
        kind: DiscoveryKind,
        total_targets: Option<u32>,
    ) -> (Uuid, Arc<Mutex<SessionInner>>) {
        let id = Uuid::now_v7();
        let inner = Arc::new(Mutex::new(SessionInner {
            state: DiscoveryState::Running,
            kind,
            started_at: Utc::now(),
            finished_at: None,
            total_targets,
            scanned: 0,
            found: Vec::new(),
            error: None,
        }));
        self.inner.insert(id, inner.clone());
        (id, inner)
    }

    pub fn get(&self, id: &Uuid) -> Option<DiscoverySessionView> {
        let entry = self.inner.get(id)?;
        let snap = entry.value().lock().snapshot(*id);
        Some(snap)
    }

    /// Drop every session whose `finished_at` is older than
    /// `SESSION_TTL`. Called by the background sweep launched
    /// from [`spawn_eviction_sweep`].
    fn evict_expired(&self, now: Instant) {
        let mut to_remove = Vec::new();
        for entry in self.inner.iter() {
            let guard = entry.value().lock();
            if let Some(finished) = guard.finished_at {
                if now.saturating_duration_since(finished) > SESSION_TTL {
                    to_remove.push(*entry.key());
                }
            }
        }
        for id in to_remove {
            self.inner.remove(&id);
        }
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.inner.len()
    }
}

/// Spawn the background eviction sweep. Should be called once at
/// engine boot. Holds a weak-ref-free clone — the sweep keeps the
/// registry alive for the engine lifetime, which is what we want
/// (the registry IS engine-process-scoped).
pub fn spawn_eviction_sweep(sessions: DiscoverySessions) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(SWEEP_INTERVAL);
        // Skip the initial tick (fires immediately) so the first
        // eviction pass happens one SWEEP_INTERVAL after boot, not
        // at boot itself.
        ticker.tick().await;
        loop {
            ticker.tick().await;
            sessions.evict_expired(Instant::now());
        }
    });
}

/// Helper used by the discovery tasks to mark a session terminal
/// (Done or Error). Also stamps `finished_at` so the eviction
/// sweep can drop it after `SESSION_TTL`.
pub(crate) fn mark_finished(inner: &Mutex<SessionInner>, error: Option<String>) {
    let mut guard = inner.lock();
    guard.state = if error.is_some() {
        DiscoveryState::Error
    } else {
        DiscoveryState::Done
    };
    guard.finished_at = Some(Instant::now());
    guard.error = error;
}

// ---------------------------------------------------------------------------
// HTTP handlers
//
// All four routes are mounted under the existing admin sub-router
// in `api::router()`; the `admin_auth_layer` already gates them.
// See `api.rs` for the wiring.
// ---------------------------------------------------------------------------

/// `POST /api/v1/admin/discovery/onvif`
///
/// Spawns a 5-second WS-Discovery probe. Returns the session id
/// immediately; the UI polls `GET /sessions/:session_id` for
/// results.
pub async fn post_discovery_onvif(
    State(s): State<ApiState>,
) -> Result<Json<SessionCreatedResp>, ApiError> {
    let (session_id, inner) = s.discovery_sessions.start(DiscoveryKind::Onvif, None);
    let started_at = inner.lock().started_at;

    // Audit-log mirrors the pattern used by the storage-admin
    // mutations. ONVIF doesn't carry a target argument; the
    // session id is enough to correlate.
    s.store
        .write_audit(
            "api",
            "discovery_onvif_start",
            &format!("session/{session_id}"),
            &serde_json::json!({}),
        )
        .await?;

    let sessions = s.discovery_sessions.clone();
    tokio::spawn(async move {
        onvif::run_session(sessions, session_id, inner).await;
    });

    Ok(Json(SessionCreatedResp {
        session_id,
        started_at,
        total_targets: None,
    }))
}

/// `POST /api/v1/admin/discovery/scan`
///
/// Validates the CIDR against the engine-side guardrails
/// ([`MIN_PREFIX_LEN`] / [`CONFIRM_REQUIRED_AT_PREFIX_LEN`]),
/// audit-logs the start, then spawns the sweep. Returns the
/// session id + projected host count.
pub async fn post_discovery_scan(
    State(s): State<ApiState>,
    Json(req): Json<ScanReq>,
) -> Result<Json<SessionCreatedResp>, ApiError> {
    let plan = validate_scan_req(&req).map_err(|e| ApiError(StatusCode::BAD_REQUEST, e))?;

    let total_targets = plan.total_targets;
    let (session_id, inner) = s
        .discovery_sessions
        .start(DiscoveryKind::Scan, Some(total_targets));
    let started_at = inner.lock().started_at;

    s.store
        .write_audit(
            "api",
            "discovery_scan_start",
            &format!("session/{session_id}"),
            &serde_json::json!({
                "cidr": &req.cidr,
                "ports": &plan.ports,
                "concurrency": plan.concurrency,
                "total_targets": total_targets,
            }),
        )
        .await?;

    let sessions = s.discovery_sessions.clone();
    tokio::spawn(async move {
        scan::run_session(sessions, session_id, inner, plan).await;
    });

    Ok(Json(SessionCreatedResp {
        session_id,
        started_at,
        total_targets: Some(total_targets),
    }))
}

/// `GET /api/v1/admin/discovery/sessions/:session_id`
pub async fn get_discovery_session(
    State(s): State<ApiState>,
    Path(session_id): Path<Uuid>,
) -> Result<Json<DiscoverySessionView>, ApiError> {
    s.discovery_sessions
        .get(&session_id)
        .map(Json)
        .ok_or_else(|| {
            ApiError(
                StatusCode::NOT_FOUND,
                format!("session {session_id} not found"),
            )
        })
}

/// `POST /api/v1/admin/discovery/sessions/:session_id/probe-rtsp`
///
/// Runs inline (≤5 s) against the supplied `(host, port, path)`.
/// Does NOT allocate a new session; the path parameter is only
/// used to correlate the audit log entry. A 404 on the session
/// is intentionally tolerated — operators may probe an IP after
/// the parent session has been evicted by TTL.
pub async fn post_probe_rtsp(
    State(s): State<ApiState>,
    Path(session_id): Path<Uuid>,
    Json(req): Json<ProbeRtspReq>,
) -> Result<Json<ProbeRtspResult>, ApiError> {
    s.store
        .write_audit(
            "api",
            "discovery_probe_rtsp",
            &format!("session/{session_id}"),
            &serde_json::json!({
                "host": &req.host,
                "port": req.port,
                "path": &req.path,
            }),
        )
        .await?;
    Ok(Json(rtsp_probe::probe(&req).await))
}

// ---------------------------------------------------------------------------
// CIDR guardrail validation
// ---------------------------------------------------------------------------

/// Resolved + validated scan request. The CIDR has been parsed,
/// ports defaulted, host count computed.
#[derive(Debug, Clone)]
pub(crate) struct ScanPlan {
    pub cidr: ipnet::Ipv4Net,
    pub ports: Vec<u16>,
    pub concurrency: usize,
    pub total_targets: u32,
}

/// Apply the M_ADMIN.md guardrails to a [`ScanReq`]. Returns a
/// validated [`ScanPlan`] or a human-readable error string the
/// HTTP handler echoes back as the 400 body.
pub(crate) fn validate_scan_req(req: &ScanReq) -> Result<ScanPlan, String> {
    let cidr: ipnet::Ipv4Net = req
        .cidr
        .parse()
        .map_err(|e| format!("malformed CIDR `{}`: {}", req.cidr, e))?;

    if cidr.prefix_len() < MIN_PREFIX_LEN {
        return Err(format!(
            "scan refused: prefix /{} is shorter than the minimum allowed (/{})",
            cidr.prefix_len(),
            MIN_PREFIX_LEN
        ));
    }
    if cidr.prefix_len() == CONFIRM_REQUIRED_AT_PREFIX_LEN && !req.confirm.unwrap_or(false) {
        return Err(format!(
            "scan refused: /{} requires `confirm: true` (1024 hosts)",
            CONFIRM_REQUIRED_AT_PREFIX_LEN
        ));
    }

    let ports = req
        .ports
        .clone()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| vec![554, 80, 8080]);

    let concurrency = req.concurrency.filter(|&n| n > 0).unwrap_or(64).min(256);

    // `ipnet::Ipv4Net::hosts()` excludes network + broadcast for
    // /30 and shorter prefixes, and yields the two addresses for
    // /31, and a single address for /32. For our guardrail-
    // bounded range (/22..=/32) `hosts().count()` is what the
    // operator means by "number of IPs we'll TCP-probe".
    let total_targets = u32::try_from(cidr.hosts().count())
        .map_err(|_| "CIDR host count exceeds u32::MAX".to_string())?;

    Ok(ScanPlan {
        cidr,
        ports,
        concurrency,
        total_targets,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_rejects_prefix_shorter_than_22() {
        // /21 = 2046 hosts. Hard refused even with confirm: true.
        for cidr in ["10.0.0.0/8", "10.0.0.0/16", "10.0.0.0/20", "10.0.0.0/21"] {
            let err = validate_scan_req(&ScanReq {
                cidr: cidr.to_string(),
                ports: None,
                concurrency: None,
                confirm: Some(true),
            })
            .unwrap_err();
            assert!(
                err.contains("prefix") && err.contains("shorter"),
                "expected guardrail rejection for {cidr}, got: {err}"
            );
        }
    }

    #[test]
    fn validate_requires_confirm_for_22() {
        let req = ScanReq {
            cidr: "192.168.0.0/22".to_string(),
            ports: None,
            concurrency: None,
            confirm: None,
        };
        let err = validate_scan_req(&req).unwrap_err();
        assert!(err.contains("confirm: true"), "got: {err}");

        // With confirm: true, /22 is allowed. `ipnet::hosts()`
        // strips the network + broadcast addresses, so a /22 (1024
        // total) sweeps 1022 probable hosts.
        let plan = validate_scan_req(&ScanReq {
            confirm: Some(true),
            ..req
        })
        .unwrap();
        assert_eq!(plan.total_targets, 1022);
    }

    #[test]
    fn validate_accepts_24_without_confirm() {
        let plan = validate_scan_req(&ScanReq {
            cidr: "192.168.1.0/24".to_string(),
            ports: None,
            concurrency: None,
            confirm: None,
        })
        .unwrap();
        // /24 hosts() yields .1 ..= .254 → 254.
        assert_eq!(plan.total_targets, 254);
        assert_eq!(plan.ports, vec![554, 80, 8080]);
        assert_eq!(plan.concurrency, 64);
    }

    #[test]
    fn validate_defaults_empty_ports_to_canonical_set() {
        let plan = validate_scan_req(&ScanReq {
            cidr: "192.168.1.0/24".to_string(),
            ports: Some(vec![]),
            concurrency: None,
            confirm: None,
        })
        .unwrap();
        assert_eq!(plan.ports, vec![554, 80, 8080]);
    }

    #[test]
    fn validate_clamps_concurrency_to_256() {
        let plan = validate_scan_req(&ScanReq {
            cidr: "192.168.1.0/24".to_string(),
            ports: None,
            concurrency: Some(99_999),
            confirm: None,
        })
        .unwrap();
        assert_eq!(plan.concurrency, 256);
    }

    #[test]
    fn validate_zero_concurrency_falls_back_to_default() {
        let plan = validate_scan_req(&ScanReq {
            cidr: "192.168.1.0/24".to_string(),
            ports: None,
            concurrency: Some(0),
            confirm: None,
        })
        .unwrap();
        assert_eq!(plan.concurrency, 64);
    }

    #[test]
    fn validate_rejects_malformed_cidr() {
        let err = validate_scan_req(&ScanReq {
            cidr: "not a cidr".to_string(),
            ports: None,
            concurrency: None,
            confirm: None,
        })
        .unwrap_err();
        assert!(err.contains("malformed CIDR"), "got: {err}");
    }

    #[tokio::test]
    async fn registry_round_trip() {
        let reg = DiscoverySessions::new();
        let (id, inner) = reg.start(DiscoveryKind::Scan, Some(10));
        assert_eq!(reg.len(), 1);

        let view = reg.get(&id).expect("session present");
        assert_eq!(view.state, DiscoveryState::Running);
        assert_eq!(view.total_targets, Some(10));
        assert_eq!(view.scanned, 0);
        assert!(view.found.is_empty());

        // Simulate the spawned task reporting progress.
        {
            let mut guard = inner.lock();
            guard.scanned = 5;
            guard.found.push(DiscoveredDevice {
                ip: "192.168.1.10".to_string(),
                port: 554,
                kind: DeviceKind::Rtsp,
                vendor: None,
                model: None,
                hardware: None,
                firmware: None,
                mac: None,
                rtsp_paths: Vec::new(),
            });
        }
        let view = reg.get(&id).unwrap();
        assert_eq!(view.scanned, 5);
        assert_eq!(view.found.len(), 1);

        mark_finished(&inner, None);
        let view = reg.get(&id).unwrap();
        assert_eq!(view.state, DiscoveryState::Done);
        assert!(view.error.is_none());
    }

    #[tokio::test]
    async fn eviction_drops_finished_sessions_past_ttl() {
        let reg = DiscoverySessions::new();
        let (id, inner) = reg.start(DiscoveryKind::Onvif, None);

        // Pretend the session finished SESSION_TTL + 30s ago.
        {
            let mut guard = inner.lock();
            guard.state = DiscoveryState::Done;
            guard.finished_at = Instant::now().checked_sub(SESSION_TTL + Duration::from_secs(30));
        }
        assert_eq!(reg.len(), 1);
        reg.evict_expired(Instant::now());
        assert_eq!(reg.len(), 0, "expired session was not evicted");

        // Eviction only touches finished sessions; running ones
        // stay regardless of age.
        let (id2, _) = reg.start(DiscoveryKind::Onvif, None);
        reg.evict_expired(Instant::now() + SESSION_TTL * 100);
        assert!(
            reg.get(&id2).is_some(),
            "running session must not be evicted"
        );
        // Sanity check the evicted id really is gone.
        assert!(reg.get(&id).is_none());
    }
}
