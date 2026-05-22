//! Cache for the most recent `entitlement_update` payload.
//!
//! The engine applies entitlement quota (max cameras, max storage,
//! enabled features) from the most recent JWT the cloud-console pushed.
//! Caching it in-process lets the engine start up before the first
//! heartbeat round-trip lands; persistence to the local data dir is the
//! engine's concern (this crate only provides the in-memory cache).

use parking_lot::RwLock;

/// Latest entitlement JWT. Phase 1.7 stores the compact JWS verbatim;
/// the engine decodes + verifies it against the bundled signing key
/// (same key the [`crate::actor_token::Verifier`] uses, per
/// `WIRE_PROTOCOL.md §11`).
#[derive(Debug, Default)]
pub struct EntitlementCache {
    inner: RwLock<Option<String>>,
}

impl EntitlementCache {
    /// Fresh empty cache.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace the cached JWT with `jwt`. Returns the previous value
    /// (if any) so callers can compare for change-detection.
    pub fn store(&self, jwt: impl Into<String>) -> Option<String> {
        let mut guard = self.inner.write();
        guard.replace(jwt.into())
    }

    /// Snapshot the current JWT, cloning it. Returns `None` if no
    /// entitlement has been received yet.
    pub fn current(&self) -> Option<String> {
        self.inner.read().clone()
    }

    /// `true` once at least one entitlement update has been stored.
    pub fn is_populated(&self) -> bool {
        self.inner.read().is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_returns_previous_value() {
        let cache = EntitlementCache::new();
        assert!(cache.store("v1").is_none());
        assert_eq!(cache.store("v2"), Some("v1".to_string()));
        assert_eq!(cache.current(), Some("v2".to_string()));
        assert!(cache.is_populated());
    }
}
