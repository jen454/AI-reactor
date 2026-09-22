//! When we are allowed to touch the endpoint again.
//!
//! A 429 from the usage endpoint throttles *the usage query itself*, not the
//! user's Claude limit — a spent limit comes back as a normal 200 with
//! `utilization: 100`. So a 429 only pauses us for as long as the server asks
//! (floored, so `Retry-After: 0` cannot turn us into a hammer), and the next
//! regular poll tries again. Waiting for the window reset instead left the
//! gauge frozen on an hours-old reading after a single refusal.

/// The longest any wait may last: just under one idle poll cycle.
///
/// Both providers must refresh on every three-minute tick, whatever went
/// wrong on the last one. The margin matters: a tick is timed from when the
/// previous cycle *started*, but a wait is stamped when its request *ended*,
/// so a wait of exactly one cycle expires just after the next tick and the
/// real gap doubles to six minutes.
pub const MAX_WAIT_MS: i64 = super::schedule::IDLE_INTERVAL.as_millis() as i64 - 15_000;

/// Wait for a 429 that arrived without a usable `Retry-After`.
pub const RATE_LIMIT_FLOOR_MS: i64 = MAX_WAIT_MS;

/// The shortest we will *ever* wait after a 429, whatever the header says.
///
/// Seen in the wild: `Retry-After: 0`. Taken literally that means "retry now",
/// which against a rate limiter means retry into the same refusal, forever.
/// However soon the server claims to be ready, coming back faster than one
/// ordinary poll cycle cannot help and can only make things worse.
pub const MIN_RATE_LIMIT_WAIT_MS: i64 = 30_000;

/// First delay after a network or server failure.
const FAILURE_BASE_MS: i64 = 10_000;

#[derive(Debug)]
pub struct Backoff {
    /// No request before this instant. `None` means "go ahead".
    blocked_until_ms: Option<i64>,
    consecutive_failures: u32,
    rate_limited: bool,
    /// Off only in demo mode, where nothing is being fetched and there is
    /// nothing to be gentle towards. A disabled backoff still *records* its
    /// state so nothing downstream has to special-case it.
    enabled: bool,
}

impl Default for Backoff {
    fn default() -> Self {
        Self {
            blocked_until_ms: None,
            consecutive_failures: 0,
            rate_limited: false,
            enabled: true,
        }
    }
}

impl Backoff {
    pub fn new() -> Self {
        Self::default()
    }

    /// Never hold anything back. Demo mode only.
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            ..Self::default()
        }
    }

    /// Whether a fetch may happen now.
    pub fn allows_request(&self, now_ms: i64) -> bool {
        if !self.enabled {
            return true;
        }
        match self.blocked_until_ms {
            Some(until) => now_ms >= until,
            None => true,
        }
    }

    /// How much longer we are parked, for logging and the popover.
    #[allow(dead_code)] // used by tests; the popover surfaces it in milestone 4
    pub fn remaining_ms(&self, now_ms: i64) -> i64 {
        self.blocked_until_ms
            .map(|until| (until - now_ms).max(0))
            .unwrap_or(0)
    }

    /// A fetch worked. Clear everything.
    pub fn note_success(&mut self) {
        self.blocked_until_ms = None;
        self.consecutive_failures = 0;
        self.rate_limited = false;
    }

    /// A 429: wait for the server's `Retry-After` (floored), or one poll
    /// cycle when it gave none — never past the next idle tick.
    pub fn note_rate_limited(&mut self, now_ms: i64, retry_after_ms: Option<i64>) {
        let wait = retry_after_ms
            .map_or(RATE_LIMIT_FLOOR_MS, |ms| ms.max(MIN_RATE_LIMIT_WAIT_MS))
            .min(MAX_WAIT_MS);
        let until = now_ms + wait;

        self.blocked_until_ms = Some(until);
        self.rate_limited = true;
        // A 429 is not a fault we should escalate against.
        self.consecutive_failures = 0;
    }

    /// A network error, a 5xx, or an unreadable body: wait, doubling each time.
    pub fn note_failure(&mut self, now_ms: i64) {
        self.rate_limited = false;
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        // 10s, 20s, 40s, … then pinned just under one idle cycle. A short
        // first retry helps after wake, when Wi-Fi often becomes usable
        // moments after us.
        let delay = FAILURE_BASE_MS
            .saturating_mul(1_i64 << (self.consecutive_failures - 1).min(20))
            .min(MAX_WAIT_MS);
        self.blocked_until_ms = Some(now_ms + delay);
    }

    /// An expired token. There is nothing to back off *from* — the request
    /// never reaches the network in a useful way — but we should not retry on
    /// every tick either, because recovery needs the user to run `claude`.
    pub fn note_expired(&mut self, now_ms: i64) {
        self.blocked_until_ms = Some(now_ms + FAILURE_BASE_MS);
        self.consecutive_failures = 0;
        self.rate_limited = false;
    }

    /// Let an explicit user refresh retry transient failures immediately.
    /// A real 429 still waits out its (capped) server-given deadline.
    pub fn clear_transient_wait(&mut self) {
        if !self.rate_limited {
            self.blocked_until_ms = None;
            self.consecutive_failures = 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: i64 = 60_000;
    const FAILURE_BASE: i64 = 10_000;

    #[test]
    fn a_fresh_backoff_allows_requests() {
        assert!(Backoff::new().allows_request(0));
    }

    /// Demo mode walks the UI through states that are all failures; a real
    /// backoff would park on the first one and the walk would never happen.
    #[test]
    fn a_disabled_backoff_never_holds_anything_back() {
        let mut b = Backoff::disabled();
        for _ in 0..10 {
            b.note_failure(0);
        }
        b.note_rate_limited(0, Some(600_000));
        assert!(b.allows_request(0));
    }

    #[test]
    fn rate_limit_honours_retry_after() {
        let mut b = Backoff::new();
        b.note_rate_limited(1_000, Some(120_000));
        assert!(!b.allows_request(120_000));
        assert!(b.allows_request(121_000));
    }

    /// Every wait must end before the next three-minute tick.
    #[test]
    fn no_wait_outlasts_one_poll_cycle() {
        assert!(MAX_WAIT_MS < 3 * MIN);

        let mut b = Backoff::new();
        b.note_rate_limited(0, Some(3_600_000));
        assert_eq!(b.remaining_ms(0), MAX_WAIT_MS);

        let mut b = Backoff::new();
        b.note_rate_limited(0, None);
        assert_eq!(b.remaining_ms(0), MAX_WAIT_MS);
    }

    /// Seen in the wild: `Retry-After: 0`. Believing it means retrying
    /// instantly into the same refusal, which is how a polite client becomes
    /// a hammer.
    #[test]
    fn an_absurdly_short_retry_after_is_floored() {
        let mut b = Backoff::new();
        b.note_rate_limited(0, Some(0));
        assert!(!b.allows_request(MIN_RATE_LIMIT_WAIT_MS - 1));
        assert!(b.allows_request(MIN_RATE_LIMIT_WAIT_MS));

        let mut b = Backoff::new();
        b.note_rate_limited(0, Some(1_000));
        assert!(!b.allows_request(MIN_RATE_LIMIT_WAIT_MS - 1));
    }


    #[test]
    fn failures_double_up_to_a_ceiling() {
        let mut b = Backoff::new();
        b.note_failure(0);
        assert_eq!(b.remaining_ms(0), FAILURE_BASE);
        b.note_failure(0);
        assert_eq!(b.remaining_ms(0), 2 * FAILURE_BASE);
        b.note_failure(0);
        assert_eq!(b.remaining_ms(0), 4 * FAILURE_BASE);
        for _ in 0..20 {
            b.note_failure(0);
        }
        assert_eq!(b.remaining_ms(0), MAX_WAIT_MS, "should pin at the ceiling");
    }

    #[test]
    fn success_clears_the_backoff() {
        let mut b = Backoff::new();
        b.note_failure(0);
        assert!(!b.allows_request(0));
        b.note_success();
        assert!(b.allows_request(0));
        // And the doubling restarts from the base.
        b.note_failure(0);
        assert_eq!(b.remaining_ms(0), FAILURE_BASE);
    }

    #[test]
    fn manual_retry_clears_failures_but_not_rate_limits() {
        let mut failed = Backoff::new();
        failed.note_failure(0);
        failed.clear_transient_wait();
        assert!(failed.allows_request(0));

        let mut limited = Backoff::new();
        limited.note_rate_limited(0, Some(120_000));
        limited.clear_transient_wait();
        assert!(!limited.allows_request(0));
    }

    /// A long outage must not overflow the shift and wrap into a negative delay.
    #[test]
    fn a_very_long_outage_does_not_overflow() {
        let mut b = Backoff::new();
        for _ in 0..500 {
            b.note_failure(0);
        }
        assert_eq!(b.remaining_ms(0), MAX_WAIT_MS);
    }
}
