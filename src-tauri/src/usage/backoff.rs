//! When we are allowed to touch the endpoint again.
//!
//! The spec's rule for 429 is blunt and deliberate: back off immediately and
//! do not retry until the reset. Hammering a rate-limited endpoint every 60
//! seconds is how a polling app gets itself throttled harder — and this one
//! shares an account with the user's actual `claude` sessions, so our retries
//! would be competing with their work.

/// Floor for a 429 that arrived without a usable `Retry-After` and without a
/// known reset time. Long enough not to hammer, short enough that a transient
/// limit does not leave the app blind all afternoon.
pub const RATE_LIMIT_FLOOR_MS: i64 = 5 * 60 * 1000;

/// The shortest we will *ever* wait after a 429, whatever the header says.
///
/// Seen in the wild: `Retry-After: 0`. Taken literally that means "retry now",
/// which against a rate limiter means retry into the same refusal, forever.
/// However soon the server claims to be ready, coming back faster than one
/// ordinary poll cycle cannot help and can only make things worse.
pub const MIN_RATE_LIMIT_WAIT_MS: i64 = 30_000;

/// First delay after a network or server failure.
const FAILURE_BASE_MS: i64 = 10_000;
/// Ceiling for the doubling.
const FAILURE_MAX_MS: i64 = 15 * 60 * 1000;

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

    /// A 429.
    ///
    /// Preference order: the server's own `Retry-After`, then the next window
    /// reset we know about (the limit cannot lift before then), then a floor.
    /// We take whichever is *longest* of the first two rather than the first
    /// available — coming back before the window resets just earns another 429.
    pub fn note_rate_limited(
        &mut self,
        now_ms: i64,
        retry_after_ms: Option<i64>,
        next_reset_ms: Option<i64>,
    ) {
        let from_header = retry_after_ms.map(|ms| now_ms + ms.max(MIN_RATE_LIMIT_WAIT_MS));
        let from_reset = next_reset_ms.filter(|&reset| reset > now_ms);

        let until = match (from_header, from_reset) {
            (Some(a), Some(b)) => a.max(b),
            (Some(a), None) => a,
            (None, Some(b)) => b,
            (None, None) => now_ms + RATE_LIMIT_FLOOR_MS,
        };

        self.blocked_until_ms = Some(until);
        self.rate_limited = true;
        // A 429 is not a fault we should escalate against; the reset time
        // already governs when we return.
        self.consecutive_failures = 0;
    }

    /// A network error, a 5xx, or an unreadable body: wait, doubling each time.
    pub fn note_failure(&mut self, now_ms: i64) {
        self.rate_limited = false;
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        // 10s, 20s, 40s, … then pinned at 15min. A short first retry helps
        // after wake, when Wi-Fi often becomes usable moments after us.
        let delay = FAILURE_BASE_MS
            .saturating_mul(1_i64 << (self.consecutive_failures - 1).min(20))
            .min(FAILURE_MAX_MS);
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
    /// A real 429 remains protected by its server/reset-derived deadline.
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
        b.note_rate_limited(0, Some(600_000), None);
        assert!(b.allows_request(0));
    }

    #[test]
    fn rate_limit_honours_retry_after() {
        let mut b = Backoff::new();
        b.note_rate_limited(1_000, Some(120_000), None);
        assert!(!b.allows_request(120_000));
        assert!(b.allows_request(121_000));
    }

    /// Seen in the wild: `Retry-After: 0`. Believing it means retrying
    /// instantly into the same refusal, which is how a polite client becomes
    /// a hammer.
    #[test]
    fn an_absurdly_short_retry_after_is_floored() {
        let mut b = Backoff::new();
        b.note_rate_limited(0, Some(0), None);
        assert!(!b.allows_request(MIN_RATE_LIMIT_WAIT_MS - 1));
        assert!(b.allows_request(MIN_RATE_LIMIT_WAIT_MS));

        let mut b = Backoff::new();
        b.note_rate_limited(0, Some(1_000), None);
        assert!(!b.allows_request(MIN_RATE_LIMIT_WAIT_MS - 1));
    }

    /// The spec's rule: do not come back before the window resets.
    #[test]
    fn rate_limit_waits_for_the_window_reset_when_it_is_later() {
        let mut b = Backoff::new();
        let reset = 10 * MIN;
        // Server says 30s, but the window does not reset for 10 minutes.
        // Returning at 30s would just earn another 429.
        b.note_rate_limited(0, Some(30_000), Some(reset));
        assert!(!b.allows_request(reset - 1));
        assert!(b.allows_request(reset));
    }

    #[test]
    fn rate_limit_without_any_hint_uses_the_floor() {
        let mut b = Backoff::new();
        b.note_rate_limited(0, None, None);
        assert!(!b.allows_request(RATE_LIMIT_FLOOR_MS - 1));
        assert!(b.allows_request(RATE_LIMIT_FLOOR_MS));
    }

    /// A reset time already in the past tells us nothing; fall back.
    #[test]
    fn a_stale_reset_time_is_ignored() {
        let mut b = Backoff::new();
        b.note_rate_limited(10 * MIN, None, Some(1 * MIN));
        assert!(!b.allows_request(10 * MIN + RATE_LIMIT_FLOOR_MS - 1));
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
        assert_eq!(b.remaining_ms(0), 15 * MIN, "should pin at the ceiling");
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
        limited.note_rate_limited(0, Some(120_000), None);
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
        assert_eq!(b.remaining_ms(0), 15 * MIN);
    }
}
