//! RTSP inline Verify probe — **stub** (Step D).
//!
//! Future work: connect TCP, send `OPTIONS` + `DESCRIBE`,
//! retry `DESCRIBE` with Digest auth on 401, parse the SDP body
//! for `m=video` / `a=rtpmap` / `a=control:` lines and return
//! a per-track summary. Tracked in
//! [`docs/M_ADMIN.md`](../../../../docs/M_ADMIN.md#phase-1b--camera-discovery-onvif--ip-scan).
//!
//! Until that lands, this stub returns `ok: false, status: 0`
//! so the UI can wire the Verify button against the route shape
//! without a hard 501.

use super::{ProbeRtspReq, ProbeRtspResult};

pub(crate) async fn probe(_req: &ProbeRtspReq) -> ProbeRtspResult {
    // TODO(m-admin step D): real RTSP OPTIONS + DESCRIBE w/ Digest.
    ProbeRtspResult {
        ok: false,
        status: 0,
        sdp_streams: Vec::new(),
    }
}
