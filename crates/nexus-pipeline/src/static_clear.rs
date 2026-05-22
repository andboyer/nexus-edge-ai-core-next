//! Per-camera operator-clear signal for the static-object filter.
//!
//! Owned by the engine; shared with every supervisor task and with
//! the `DELETE /api/cameras/{id}/static-anchors` HTTP handler. The
//! API handler bumps a per-camera sequence counter; the supervisor
//! polls the counter once per frame and invokes
//! [`nexus_tracker::static_object::StaticObjectFilter::clear`]
//! whenever it has changed since the previous frame.
//!
//! Why a polled counter instead of a `tokio::sync::Notify` or a
//! broadcast subscription:
//!   1. The supervisor's hot loop already runs on every frame —
//!      adding an atomic-load is cheaper than a `select!` arm.
//!   2. We don't care about "missing" events: every supervisor
//!      converges on the latest sequence value, and the operator
//!      cares about end-state (anchors gone) not transitions.
//!   3. No per-camera task lifecycle to manage — the counter lives
//!      as long as the registry, and supervisors that come and go
//!      across reconciler restarts just observe whatever the
//!      current value is on entry.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use nexus_types::CameraId;
use parking_lot::RwLock;

/// Shared bus of "operator requested an anchor wipe" sequence
/// counters, keyed by camera id. Cheap to clone — the inner
/// [`RwLock`] wraps a small [`HashMap`] of [`Arc<AtomicU64>`].
#[derive(Default)]
pub struct StaticAnchorClearRegistry {
    inner: RwLock<HashMap<CameraId, Arc<AtomicU64>>>,
}

impl StaticAnchorClearRegistry {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Bump the per-camera clear sequence. Returns the new value
    /// (mostly for tests / logging). The supervisor polling
    /// [`current`] on its next iteration will see a delta and
    /// invoke `StaticObjectFilter::clear`.
    pub fn request_clear(&self, camera_id: CameraId) -> u64 {
        let counter = {
            let guard = self.inner.read();
            guard.get(&camera_id).cloned()
        };
        let counter = match counter {
            Some(c) => c,
            None => {
                let mut guard = self.inner.write();
                guard
                    .entry(camera_id)
                    .or_insert_with(|| Arc::new(AtomicU64::new(0)))
                    .clone()
            }
        };
        counter.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// Current sequence value for the supervisor's per-frame poll.
    /// Returns `0` for cameras that have never had a clear
    /// requested — supervisors snapshot this on entry and only
    /// react to deltas, so a fresh counter doesn't trigger an
    /// initial spurious wipe.
    pub fn current(&self, camera_id: CameraId) -> u64 {
        let guard = self.inner.read();
        guard
            .get(&camera_id)
            .map(|c| c.load(Ordering::SeqCst))
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_starts_at_zero_for_unknown_camera() {
        let reg = StaticAnchorClearRegistry::new();
        assert_eq!(reg.current(42), 0);
    }

    #[test]
    fn request_clear_increments_per_camera() {
        let reg = StaticAnchorClearRegistry::new();
        assert_eq!(reg.request_clear(1), 1);
        assert_eq!(reg.request_clear(1), 2);
        assert_eq!(reg.current(1), 2);
        // Other cameras are independent.
        assert_eq!(reg.current(2), 0);
        assert_eq!(reg.request_clear(2), 1);
        assert_eq!(reg.current(1), 2);
    }
}
