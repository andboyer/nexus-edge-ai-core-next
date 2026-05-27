//! Phase 1.15 — local NTP / `chrony` posture probe.
//!
//! Two consumers:
//!
//! 1. [`nexus-engine enroll`] runs [`require_synchronized`] as a
//!    precheck. Enrollment artifacts (mTLS certs, `actor_token`
//!    bundle, entitlement JWT) all expire on wall-clock; a freshly
//!    flashed appliance that boots with a stale RTC battery and
//!    skips NTP convergence will hand the cloud a CSR signed at the
//!    wrong epoch, and the cloud-side `actor_token` verifier's ±30 s
//!    skew window (Phase 1.15 ARCHITECTURE §3.7) will reject every
//!    mutating RPC the engine subsequently makes. Failing fast at
//!    enroll time is the only way to surface this to a human
//!    operator who can still poke at `chronyc tracking` /
//!    `timedatectl status` while the install console is open.
//!
//! 2. [`nexus-doctor`] surfaces [`probe`]'s result as the
//!    `time.sync_state` / `time.skew_ms` row in its report so an
//!    operator running the smoke check against an already-enrolled
//!    box can spot drift before the `last_skew_ms` rolling EMA in
//!    the cloud console crosses the 30 000 ms threshold and the
//!    remediation banner appears.
//!
//! We shell out to `chronyc tracking` rather than implementing the
//! chronyd control protocol ourselves. The output format has been
//! stable since chrony 1.x (the `Reference ID`, `Leap status`, and
//! `System time` lines are all parsed below). `chronyc` is in
//! `apt install chrony` which `scripts/lib/install-common.sh`
//! already provisions, so it's always present on a real edge box.
//!
//! Falls back to `timedatectl show` for systemd-timesyncd boxes that
//! do not have chrony installed (`SyncState` is exposed there since
//! systemd 234 — older than Ubuntu 22.04's baseline).
//!
//! ## Override
//!
//! Both [`require_synchronized`] and [`probe`] honour
//! `NEXUS_TIME_SYNC_OVERRIDE=allow_unsynced` for the rare case
//! where an operator is enrolling a box in an offline lab or has a
//! GPS/PTP source not visible to chrony. The override is logged at
//! `warn!` so it shows up in any audit bundle the operator hands to
//! support.

use std::process::Command;
use std::time::Duration;

/// Coarse status bucket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncState {
    /// The local clock is actively tracking a reachable upstream
    /// source — `chronyc tracking` shows `Leap status: Normal` and
    /// the `Reference ID` is non-`00000000` / non-`7f7f0101`
    /// (placeholder values chrony emits while still converging).
    Synchronized,
    /// chrony / timesyncd is running but has not yet locked onto a
    /// source, OR the leap-status is `Not synchronised`. Either is
    /// disqualifying for enrollment; doctor surfaces it as a warn
    /// row.
    Unsynchronized,
    /// Neither `chronyc` nor `timedatectl` is available, OR the
    /// command failed with a non-zero exit. Treated as
    /// `Unsynchronized` for gate purposes but surfaced separately
    /// in the doctor report so an operator can spot a missing
    /// `chrony` package vs. a network issue.
    Unavailable,
}

impl SyncState {
    pub fn as_str(&self) -> &'static str {
        match self {
            SyncState::Synchronized => "synchronized",
            SyncState::Unsynchronized => "unsynchronized",
            SyncState::Unavailable => "unavailable",
        }
    }
}

/// Probe result. `skew_ms` is `None` when no source could be
/// parsed; numeric value when chrony returned a `System time`
/// reading. Positive = local clock is AHEAD of NTP; negative =
/// BEHIND.
#[derive(Debug, Clone)]
pub struct TimeSync {
    pub state: SyncState,
    pub skew_ms: Option<i64>,
    /// Human-readable detail line — e.g. `"chronyc: Leap status:
    /// Normal, System time: 0.000345 s ahead"` or
    /// `"timedatectl: SyncState=yes"`. Surfaced as the `actual`
    /// field in the doctor report.
    pub detail: String,
}

/// Run the probe. Prefers chrony, falls back to timedatectl, then
/// gives up. Never panics — any unexpected stderr / parse error
/// becomes `Unavailable` with a diagnostic message in `detail`.
pub fn probe() -> TimeSync {
    if let Some(ts) = probe_chronyc() {
        return ts;
    }
    if let Some(ts) = probe_timedatectl() {
        return ts;
    }
    TimeSync {
        state: SyncState::Unavailable,
        skew_ms: None,
        detail: "neither chronyc nor timedatectl is on PATH".into(),
    }
}

/// Enrollment precheck. Returns `Ok(())` when the local clock is
/// synchronized OR when the operator opted out via env var. Returns
/// `Err(message)` when the gate refuses.
///
/// The error message points the operator at the next diagnostic
/// step (`chronyc tracking`) and at the override.
//
// `#[allow(dead_code)]` is for the `nexus-doctor` binary, which
// `#[path]`-includes this module but only calls `probe()` — the
// gate variant is exclusive to `nexus-engine enroll`.
#[allow(dead_code)]
pub fn require_synchronized() -> Result<TimeSync, String> {
    let ts = probe();
    if std::env::var("NEXUS_TIME_SYNC_OVERRIDE")
        .map(|v| v == "allow_unsynced")
        .unwrap_or(false)
    {
        tracing::warn!(
            state = %ts.state.as_str(),
            skew_ms = ?ts.skew_ms,
            "NEXUS_TIME_SYNC_OVERRIDE=allow_unsynced is set — enrollment will proceed without a synchronized clock; signed actor_tokens may fail cloud-side verification"
        );
        return Ok(ts);
    }
    match ts.state {
        SyncState::Synchronized => Ok(ts),
        SyncState::Unsynchronized | SyncState::Unavailable => Err(format!(
            "local clock is not synchronized ({}). enrollment refuses to proceed because the actor_token verifier rejects ±30 s skew. fix with `sudo systemctl restart chrony && chronyc tracking` (Leap status must read `Normal`), or rerun with NEXUS_TIME_SYNC_OVERRIDE=allow_unsynced to bypass at your own risk. detail: {}",
            ts.state.as_str(),
            ts.detail,
        )),
    }
}

// ---------------------------------------------------------------------------
// chrony backend
// ---------------------------------------------------------------------------

fn probe_chronyc() -> Option<TimeSync> {
    let output = Command::new("chronyc").arg("tracking").output().ok()?;
    if !output.status.success() {
        return Some(TimeSync {
            state: SyncState::Unavailable,
            skew_ms: None,
            detail: format!(
                "chronyc tracking exited {}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        });
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    Some(parse_chronyc_tracking(&stdout))
}

/// Parse `chronyc tracking` output. Public for unit testing.
pub(crate) fn parse_chronyc_tracking(stdout: &str) -> TimeSync {
    let mut leap_normal = false;
    let mut ref_id_present = false;
    let mut skew_ms: Option<i64> = None;

    /// Strip a `Key:` prefix; return the value half ("Normal",
    /// "0.000345 seconds fast of NTP time", etc.) or `None` if
    /// the line doesn't start with the given key.
    fn value_after(line: &str, key: &str) -> Option<String> {
        let rest = line.strip_prefix(key)?;
        // Skip whitespace + the colon + more whitespace.
        let rest = rest.trim_start();
        let rest = rest.strip_prefix(':')?;
        Some(rest.trim().to_string())
    }

    for raw in stdout.lines() {
        let line = raw.trim_end();
        if let Some(v) = value_after(line, "Leap status") {
            if v.eq_ignore_ascii_case("normal") {
                leap_normal = true;
            }
        } else if let Some(v) = value_after(line, "Reference ID") {
            // "C0A80101 (ntp.example)" or "7F7F0101 ()"
            let token = v.split_whitespace().next().unwrap_or("");
            if !token.is_empty()
                && !token.eq_ignore_ascii_case("00000000")
                && !token.eq_ignore_ascii_case("7F7F0101")
            {
                ref_id_present = true;
            }
        } else if let Some(v) = value_after(line, "System time") {
            // "0.000034521 seconds fast of NTP time"
            // "0.001234567 seconds slow of NTP time"
            let mut parts = v.split_whitespace();
            if let Some(secs_str) = parts.next() {
                if let Ok(secs) = secs_str.parse::<f64>() {
                    let mut ms = (secs * 1000.0).round() as i64;
                    // "fast" => local ahead => positive; "slow" =>
                    // local behind => negative. Default to positive
                    // if the descriptor is missing (older chrony).
                    if v.contains(" slow ") {
                        ms = -ms;
                    }
                    skew_ms = Some(ms);
                }
            }
        }
    }

    let state = if leap_normal && ref_id_present {
        SyncState::Synchronized
    } else {
        SyncState::Unsynchronized
    };
    let detail = format!(
        "chronyc: leap_normal={}, reference_id_present={}, skew_ms={}",
        leap_normal,
        ref_id_present,
        skew_ms.map(|v| v.to_string()).unwrap_or_else(|| "?".into()),
    );
    TimeSync {
        state,
        skew_ms,
        detail,
    }
}

// ---------------------------------------------------------------------------
// systemd-timesyncd fallback
// ---------------------------------------------------------------------------

fn probe_timedatectl() -> Option<TimeSync> {
    let output = Command::new("timedatectl")
        .args(["show", "--property=NTPSynchronized", "--value"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let synced = stdout.eq_ignore_ascii_case("yes");
    Some(TimeSync {
        state: if synced {
            SyncState::Synchronized
        } else {
            SyncState::Unsynchronized
        },
        // timedatectl does not expose a per-sample offset; only
        // chrony / chronyc reports skew. doctor will show `None`.
        skew_ms: None,
        detail: format!("timedatectl: NTPSynchronized={stdout}"),
    })
}

/// Re-export for callers (doctor) that want to time out a probe.
#[allow(dead_code)]
pub const DEFAULT_PROBE_TIMEOUT: Duration = Duration::from_secs(2);

#[cfg(test)]
mod tests {
    use super::*;

    const SYNCED: &str = "Reference ID    : C0A80164 (ntp.local)
Stratum         : 3
Ref time (UTC)  : Mon Nov  6 14:23:11 2025
System time     : 0.000345678 seconds fast of NTP time
Last offset     : +0.000123 seconds
RMS offset      : 0.000456 seconds
Frequency       : 12.345 ppm slow
Residual freq   : +0.000 ppm
Skew            : 0.456 ppm
Root delay      : 0.001234 seconds
Root dispersion : 0.002345 seconds
Update interval : 64.2 seconds
Leap status     : Normal
";

    const UNSYNCED_PLACEHOLDER: &str = "Reference ID    : 7F7F0101 ()
Stratum         : 0
Ref time (UTC)  : Thu Jan  1 00:00:00 1970
System time     : 0.000000000 seconds fast of NTP time
Last offset     : +0.000000000 seconds
RMS offset      : 0.000000000 seconds
Frequency       : 0.000 ppm slow
Residual freq   : +0.000 ppm
Skew            : 0.000 ppm
Root delay      : 1.000000000 seconds
Root dispersion : 1.000000000 seconds
Update interval : 0.0 seconds
Leap status     : Not synchronised
";

    const SLOW: &str = "Reference ID    : C0A80164 (ntp.local)
System time     : 0.012345678 seconds slow of NTP time
Leap status     : Normal
";

    #[test]
    fn parse_synced_reports_positive_skew_and_synchronized() {
        let ts = parse_chronyc_tracking(SYNCED);
        assert_eq!(ts.state, SyncState::Synchronized);
        assert_eq!(ts.skew_ms, Some(0));
        // 0.000345678 s rounds to 0 ms — exercise larger value next test.
    }

    #[test]
    fn parse_unsynced_placeholder_is_unsynchronized() {
        let ts = parse_chronyc_tracking(UNSYNCED_PLACEHOLDER);
        assert_eq!(ts.state, SyncState::Unsynchronized);
    }

    #[test]
    fn parse_slow_negates_skew_ms() {
        let ts = parse_chronyc_tracking(SLOW);
        assert_eq!(ts.state, SyncState::Synchronized);
        assert_eq!(ts.skew_ms, Some(-12));
    }

    #[test]
    fn empty_output_is_unsynchronized() {
        let ts = parse_chronyc_tracking("");
        assert_eq!(ts.state, SyncState::Unsynchronized);
        assert_eq!(ts.skew_ms, None);
    }

    #[test]
    fn override_env_var_allows_unsynced() {
        // Save+restore to avoid clobbering other parallel tests.
        let prev = std::env::var("NEXUS_TIME_SYNC_OVERRIDE").ok();
        std::env::set_var("NEXUS_TIME_SYNC_OVERRIDE", "allow_unsynced");
        let result = require_synchronized();
        assert!(result.is_ok(), "override should bypass the gate");
        match prev {
            Some(v) => std::env::set_var("NEXUS_TIME_SYNC_OVERRIDE", v),
            None => std::env::remove_var("NEXUS_TIME_SYNC_OVERRIDE"),
        }
    }
}
