//! Bounded replay-protection cache for `actor_token` JWT IDs.
//!
//! Phase 1.7 calls for a "~10 000-entry replay-protection ring buffer".
//! We back it with a `VecDeque<String>` for FIFO eviction plus a
//! `HashSet<String>` for O(1) membership checks. The two structures are
//! kept in sync by [`JtiReplayCache::insert`].
//!
//! The cache is intentionally in-memory only — restart-time replay
//! windows are short enough (≤ 30 s `exp` window per Phase 1.15) that
//! we don't need persistence.
//!
//! ## Phase 1.16 — widened to `(jti, request_id)`
//!
//! The cloud-side `Idempotency-Key` middleware (Phase 1.11) propagates
//! a UUID into `RpcCallPayload.request_id` (Phase 1.16). When set, the
//! engine MUST key its replay window on the tuple, NOT on `jti` alone:
//! a legitimate idempotent retry from the cloud will carry the SAME
//! `request_id` AND a freshly-minted `actor_token` (with a fresh `jti`).
//! The two-element key lets the engine recognise it as a retry, while
//! still rejecting a true replay (same `(jti, request_id)`).
//!
//! [`JtiReplayCache::insert_keyed`] is the v1.16 API; [`Self::insert`]
//! remains as the v1.7 shortcut for `(jti, None)`.

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
    ///
    /// Phase 1.7 shorthand for `insert_keyed(jti, None)`.
    pub fn insert(&self, jti: &str) -> bool {
        self.insert_keyed(jti, None)
    }

    /// Phase 1.16: inserts a composite `(jti, request_id)` key. When
    /// `request_id` is `None` this behaves exactly like
    /// [`Self::insert`] (same wire shape as v1.7).
    ///
    /// Returns `true` if newly accepted, `false` if the tuple was
    /// already present (real replay). A retry that carries the same
    /// `request_id` but a freshly-minted `jti` will be admitted by
    /// this layer; the engine's HTTP-side idempotency layer is
    /// responsible for replaying the cached response in that case.
    pub fn insert_keyed(&self, jti: &str, request_id: Option<&str>) -> bool {
        if self.capacity == 0 {
            return true;
        }
        let composite = compose_key(jti, request_id);
        let mut guard = self.inner.lock();
        if guard.seen.contains(&composite) {
            return false;
        }
        if guard.order.len() == self.capacity {
            if let Some(evicted) = guard.order.pop_front() {
                guard.seen.remove(&evicted);
            }
        }
        guard.seen.insert(composite.clone());
        guard.order.push_back(composite);
        true
    }

    /// Returns `true` if `jti` (with no `request_id` companion) is
    /// currently in the cache. Mainly useful for tests — production
    /// code should call [`Self::insert_keyed`] which folds the
    /// membership check and admit decision into one op.
    pub fn contains(&self, jti: &str) -> bool {
        self.contains_keyed(jti, None)
    }

    /// Phase 1.16 companion to [`Self::contains`] — looks up the
    /// composite `(jti, request_id)` key.
    pub fn contains_keyed(&self, jti: &str, request_id: Option<&str>) -> bool {
        self.inner
            .lock()
            .seen
            .contains(&compose_key(jti, request_id))
    }
}

/// Build the composite key used by [`JtiReplayCache`].
/// The pipe separator is illegal in UUIDs (Phase 1.16 `request_id`s are
/// UUIDv4; `jti`s are UUIDv7) so the encoding is unambiguous.
fn compose_key(jti: &str, request_id: Option<&str>) -> String {
    match request_id {
        None => jti.to_string(),
        Some(rid) => {
            let mut s = String::with_capacity(jti.len() + 1 + rid.len());
            s.push_str(jti);
            s.push('|');
            s.push_str(rid);
            s
        }
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

    #[test]
    fn request_id_is_part_of_replay_key() {
        // Phase 1.16: same jti + different request_id => NOT a replay;
        // same (jti, request_id) tuple => replay.
        let cache = JtiReplayCache::new();
        assert!(cache.insert_keyed("jti-1", Some("req-a")));
        // Same jti, different request_id: cloud retried with a freshly
        // signed token but the same Idempotency-Key. Must be admitted.
        assert!(cache.insert_keyed("jti-1", Some("req-b")));
        // Same (jti, request_id) again: real replay.
        assert!(!cache.insert_keyed("jti-1", Some("req-a")));
        assert_eq!(cache.len(), 2);
    }

    #[test]
    fn jti_alone_and_jti_with_request_id_are_distinct_keys() {
        let cache = JtiReplayCache::new();
        assert!(cache.insert_keyed("jti-only", None));
        assert!(cache.insert_keyed("jti-only", Some("req-x")));
        // Each tuple counted once — two distinct keys.
        assert_eq!(cache.len(), 2);
    }
}
