//! Exponential-backoff helper for the sink dispatcher.
//!
//! Lives in `nexus-sinks` (not `nexus-engine`) so the math is unit-
//! testable in the trait crate, with no `tokio`/`sqlx`/`Store`
//! plumbing required. The engine's `SinkDispatcher` calls
//! [`backoff_for`] for every `failed` outcome and writes the
//! resulting [`std::time::Duration`] into
//! `alert_sink_outbox.next_attempt_at`.
//!
//! Defaults — `BASE = 500ms`, `CAP = 60s`, `MAX_ATTEMPTS = 8` —
//! match the surveillance methodology in
//! [`docs/M7_DELIVERY.md`](../../docs/M7_DELIVERY.md), which in
//! turn ports v1's `webhook_retry_queue.cpp` constants with a
//! longer cap (60 s, vs. v1's 15 s) because SureView's incident
//! endpoint is slower than v1's webhook customers ever were.

use std::time::Duration;

/// Base delay before the first retry. With the default sequence
/// (`500ms × 2^(n-1)`), the schedule between attempts is:
///
/// ```text
/// after N attempts | wait before attempt N+1
///        1        |   500 ms
///        2        |    1.0 s
///        3        |    2.0 s
///        4        |    4.0 s
///        5        |    8.0 s
///        6        |   16.0 s
///        7        |   32.0 s
///        8        |   — (mark dead)
/// ```
///
/// Net: at most 8 `deliver()` calls per outbox row.
pub const BASE: Duration = Duration::from_millis(500);

/// Hard ceiling on the per-attempt delay. Prevents the exponential
/// curve from running away on a permanently-flapping endpoint.
pub const CAP: Duration = Duration::from_secs(60);

/// Total number of `deliver()` calls allowed before the row is
/// marked `dead`. Counted INCLUSIVE: `attempts == MAX_ATTEMPTS`
/// after the dispatcher's bump means the row has been tried
/// `MAX_ATTEMPTS` times.
pub const MAX_ATTEMPTS: u32 = 8;

/// Compute the delay before the next retry, given the number of
/// attempts already performed (i.e. `alert_sink_outbox.attempts`
/// AFTER the dispatcher's `attempts + 1` bump on the last failure).
///
/// Returns `None` when retries are exhausted (`attempts >=
/// MAX_ATTEMPTS`) — caller must mark the row `dead`.
///
/// Uses the default constants. For unit tests against custom
/// schedules call [`backoff_for_with`].
pub fn backoff_for(attempts: u32) -> Option<Duration> {
    backoff_for_with(attempts, BASE, CAP, MAX_ATTEMPTS)
}

/// `backoff_for` with explicit constants — exposed for tests and
/// for future per-sink retry policies.
///
/// Math: `min(base * 2^(attempts-1), cap)`. The `-1` shifts the
/// curve so attempt #1 returns `base` (not `2*base`). Attempts of
/// `0` yield `base` too (the dispatcher shouldn't ever call this
/// with `0`, but defensive).
pub fn backoff_for_with(
    attempts: u32,
    base: Duration,
    cap: Duration,
    max_attempts: u32,
) -> Option<Duration> {
    if attempts >= max_attempts {
        return None;
    }
    let exp = attempts.saturating_sub(1);
    // Use u64 millis to avoid float drift and saturate at the cap.
    let base_ms = base.as_millis() as u64;
    // Cap the shift so we don't overflow `1u64 << 63`; CAP will
    // bound the result anyway. The current MAX_ATTEMPTS=8 means
    // exp ∈ 0..=6 and the shift is trivially safe, but
    // `backoff_for_with` is public and a caller could pass a
    // bigger max.
    let shift = exp.min(63);
    let raw_ms = base_ms.saturating_mul(1u64 << shift);
    let raw = Duration::from_millis(raw_ms);
    Some(raw.min(cap))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_schedule_matches_design_doc() {
        // The table from the module doc. Entry (N, d) means: "after
        // N attempts have completed, wait d before attempt N+1".
        let expected: &[(u32, Duration)] = &[
            (1, Duration::from_millis(500)),
            (2, Duration::from_secs(1)),
            (3, Duration::from_secs(2)),
            (4, Duration::from_secs(4)),
            (5, Duration::from_secs(8)),
            (6, Duration::from_secs(16)),
            (7, Duration::from_secs(32)),
        ];
        for (attempts, want) in expected {
            let got = backoff_for(*attempts).expect("not yet dead");
            assert_eq!(got, *want, "attempts={attempts}");
        }
        // Boundary: after MAX_ATTEMPTS the dispatcher must mark dead.
        assert!(backoff_for(MAX_ATTEMPTS).is_none());
    }

    #[test]
    fn exhausted_after_max() {
        assert!(backoff_for(MAX_ATTEMPTS).is_none());
        assert!(backoff_for(MAX_ATTEMPTS + 1).is_none());
        assert!(backoff_for(100).is_none());
    }

    #[test]
    fn zero_attempts_is_base() {
        // Defensive — dispatcher shouldn't ever call with 0,
        // but the math should still return `base` rather than
        // panic on a bad shift.
        assert_eq!(backoff_for(0), Some(BASE));
    }

    #[test]
    fn cap_holds_under_extreme_max() {
        // If a custom policy allowed 30 attempts, the curve would
        // saturate at `cap` somewhere around attempt 8 and stay
        // there until the boundary returns None.
        for n in 8..30 {
            assert_eq!(
                backoff_for_with(n, BASE, CAP, 30),
                Some(CAP),
                "attempts={n}"
            );
        }
        // Boundary: attempts == max_attempts returns None.
        assert!(backoff_for_with(30, BASE, CAP, 30).is_none());
    }
}
