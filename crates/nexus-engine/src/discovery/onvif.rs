//! ONVIF WS-Discovery probe — **stub** (Step C).
//!
//! Future work: send a `<wsd:Probe>` SOAP envelope over UDP
//! multicast `239.255.255.250:3702`, listen 5 s, parse each
//! `ProbeMatch` envelope's `<wsd:Scopes>` for vendor / model /
//! MAC. Tracked in [`docs/M_ADMIN.md`](../../../../docs/M_ADMIN.md#phase-1b--camera-discovery-onvif--ip-scan).
//!
//! Until that lands, this module merely marks the session done
//! with zero devices so the UI can demonstrate the end-to-end
//! polling flow without 501-ing.

use std::sync::Arc;

use parking_lot::Mutex;
use uuid::Uuid;

use super::{mark_finished, DiscoverySessions, SessionInner};

pub(crate) async fn run_session(
    _sessions: DiscoverySessions,
    _session_id: Uuid,
    inner: Arc<Mutex<SessionInner>>,
) {
    // TODO(m-admin step C): real WS-Discovery probe.
    mark_finished(&inner, None);
}
