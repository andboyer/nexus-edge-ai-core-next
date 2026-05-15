//! Token-bucket rate limiter used by the cold replicator to cap
//! upload bytes/sec.
//!
//! The replicator can have a 60 GiB backlog when a previously-down
//! cold link comes back; without a throttle the burst would
//! saturate the operator's WAN link and starve every other engine
//! task. We honour the `storage_cold_replica.throttle_bps` setting
//! by `acquire(n_bytes).await`-ing before each `put`, and the
//! bucket refills at the configured rate.
//!
//! Implementation notes:
//!
//!   * The bucket holds *credit* in bytes, not tokens. `capacity`
//!     defaults to one second's worth (`bps`) so a perfectly-paced
//!     uploader never blocks while a burst smooths over a single
//!     beat.
//!   * Refill is computed lazily from `Instant::now()` on each
//!     `acquire`. No background task; nothing to drop.
//!   * Concurrent acquisitions are serialised through a `Mutex`.
//!     A single replicator task means there's only ever one
//!     waiter, but the lock keeps the math correct if we ever
//!     fan out to a worker pool.

use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::Mutex;

/// Token bucket. Cheap to clone (it's just an `Arc<Mutex<…>>`).
#[derive(Clone)]
pub struct TokenBucket {
    inner: Arc<Mutex<BucketState>>,
    bps: u64,
    capacity: u64,
}

struct BucketState {
    /// Bytes currently available. Float because the refill rate is
    /// fractional bytes-per-tick and rounding to integers would
    /// permanently leak budget on slow refills.
    bytes: f64,
    last_refill: Instant,
}

impl TokenBucket {
    /// Create a bucket that hands out `bytes_per_sec` bytes per
    /// second on average and absorbs bursts up to `bytes_per_sec`
    /// (one-second smoothing window). A `bytes_per_sec` of zero
    /// disables throttling — `acquire` is then instant.
    pub fn new(bytes_per_sec: u64) -> Self {
        Self {
            inner: Arc::new(Mutex::new(BucketState {
                bytes: bytes_per_sec as f64,
                last_refill: Instant::now(),
            })),
            bps: bytes_per_sec,
            capacity: bytes_per_sec,
        }
    }

    /// The configured rate. Surfaced on the admin API.
    pub fn rate_bps(&self) -> u64 {
        self.bps
    }

    /// Block until at least `n` bytes of credit are available, then
    /// deduct them. Returns immediately if throttling is disabled.
    pub async fn acquire(&self, n: u64) {
        if self.bps == 0 {
            return;
        }
        // A request bigger than the bucket capacity must still
        // succeed; we just charge the full amount and the bucket
        // goes negative for a while.
        loop {
            let wait = {
                let mut s = self.inner.lock().await;
                let now = Instant::now();
                let elapsed = now.duration_since(s.last_refill).as_secs_f64();
                s.bytes = (s.bytes + elapsed * self.bps as f64).min(self.capacity as f64);
                s.last_refill = now;

                if s.bytes >= n as f64 {
                    s.bytes -= n as f64;
                    return;
                }
                // Compute time-to-refill. f64 is fine — we only need
                // millisecond precision.
                let deficit = n as f64 - s.bytes;
                let secs = deficit / self.bps as f64;
                // Cap the per-loop sleep so a misconfigured
                // throttle (1 byte/sec) doesn't lock the task for
                // hours; we'll just re-check more often.
                Duration::from_secs_f64(secs.min(0.250))
            };
            tokio::time::sleep(wait).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn zero_bps_is_instant() {
        let bucket = TokenBucket::new(0);
        let start = Instant::now();
        bucket.acquire(1_000_000).await;
        assert!(start.elapsed() < Duration::from_millis(1));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn first_acquire_inside_bucket_is_instant() {
        let bucket = TokenBucket::new(1_000);
        let start = Instant::now();
        bucket.acquire(500).await;
        assert!(start.elapsed() < Duration::from_millis(1));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn second_acquire_blocks_for_refill() {
        let bucket = TokenBucket::new(1_000);
        // Drain the initial credit.
        bucket.acquire(1_000).await;
        let start = Instant::now();
        // Asking for another 500 bytes at 1000 bps should take ~0.5 s.
        bucket.acquire(500).await;
        let elapsed = start.elapsed();
        // Allow some slack for the 250 ms cap inside acquire's sleep loop.
        assert!(
            elapsed >= Duration::from_millis(450),
            "acquire returned in {elapsed:?}; expected >= 450 ms"
        );
    }
}
