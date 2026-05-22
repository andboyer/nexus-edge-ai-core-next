//! Bounded replay-protection cache for `actor_token` JWT IDs.
//!
//! Phase 1.7 calls for a "~10 000-entry replay-protection ring buffer".
//! We back it with a `VecDeque<String>` for FIFO eviction plus a
//! `HashSet<String>` for O(1) membership checks. The two structures are
//! kept in sync by [`JtiReplayCache::insert`].
//!
//! The cache is intentionally in-memory only — restart-time replay
//! windows are short enough (≤ 30 s `exp` window per Phase 1.15) that
//! we don't need persistence. Phase 1.16 may widen the key to
//! `(jti, request_id)` for the idempotency layer; for now the JTI alone
//! satisfies the dispatcher's needs because the cloud-console mints a
//! fresh `jti` for every `actor_token`.

use std::collections::{HashSet, VecDeque};

use parking_lot::Mutex;

/// Default capacity. Sized to ~30 s × ~300 mutating-RPC/s worst case with
/// generous headroom; the dispatcher is the bottleneck well before this
/// fills under realistic edge workloads.
pub const DEFAULT_CAPACITY: usize = 10_000;

/// Bounded JTI cache. Cheap to clone the `Arc<JtiReplayCache>` and share
/// it across handler threads — internal mutex is short-held (microseconds).
#[derive(Debug)]
pub struct JtiReplayCache {
    inner: Mutex<Inner>,
    capacity: usize,
}

#[derive(Debug)]
struct Inner {
    seen: HashSet<String>,
    order: VecDeque<String>,
}

impl JtiReplayCache {
    /// Build a fresh cache at [`DEFAULT_CAPACITY`].
    #[must_use]
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_CAPACITY)
    }

    /// Build a cache with the given capacity. A capacity of 0 disables
    /// replay protection (every `insert` returns `true`); the dispatcher
    /// will then accept every well-formed token regardless of JTI history.
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            inner: Mutex::new(Inner {
                seen: HashSet::with_capacity(capacity.max(1)),
                order: VecDeque::with_capacity(capacity.max(1)),
            }),
            capacity,
        }
    }

    /// Cache capacity supplied at construction.
    #[must_use]
    pub const fn capacity(&self) -> usize {
        self.capacity
    }

    /// Returns the current number of cached JTIs.
    pub fn len(&self) -> usize {
        self.inner.lock().order.len()
    }

    /// Returns `true` if the cache holds no JTIs.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Inserts a JTI. Returns `true` if it was newly accepted, `false`
    /// if it was already present (replay attempt). When the cache hits
    /// `capacity`, the oldest JTI is evicted.
    pub fn insert(&self, jti: &str) -> bool {
        if self.capacity == 0 {
            return true;
        }
        let mut guard = self.inner.lock();
        if guard.seen.contains(jti) {
            return false;
        }
        if guard.order.len() == self.capacity {
            if let Some(evicted) = guard.order.pop_front() {
                guard.seen.remove(&evicted);
            }
        }
        guard.seen.insert(jti.to_string());
        guard.order.push_back(jti.to_string());
        true
    }

    /// Returns `true` if `jti` is currently in the cache. Mainly useful
    /// for tests — production code should call [`Self::insert`] which
    /// folds the membership check and admit decision into one op.
    pub fn contains(&self, jti: &str) -> bool {
        self.inner.lock().seen.contains(jti)
    }
}

impl Default for JtiReplayCache {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_insert_accepts_replay_rejects() {
        let cache = JtiReplayCache::new();
        assert!(cache.insert("jti-1"));
        assert!(!cache.insert("jti-1"));
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn eviction_drops_oldest() {
        let cache = JtiReplayCache::with_capacity(3);
        assert!(cache.insert("a"));
        assert!(cache.insert("b"));
        assert!(cache.insert("c"));
        // Fourth insert evicts "a".
        assert!(cache.insert("d"));
        assert_eq!(cache.len(), 3);
        assert!(!cache.contains("a"));
        assert!(cache.contains("b"));
        // After eviction, the evicted JTI is admittable again. This is
        // safe under the ±30 s skew window because a token whose JTI has
        // aged out of the cache is also past its `exp` and would be
        // rejected by [`Verifier`] before this point.
        assert!(cache.insert("a"));
    }

    #[test]
    fn zero_capacity_disables_protection() {
        let cache = JtiReplayCache::with_capacity(0);
        assert!(cache.insert("x"));
        assert!(cache.insert("x"));
        assert!(cache.is_empty());
    }
}
