//! Caching layer in front of the credential store.
//!
//! # Why cache at all
//!
//! The keychain item's ACL does not list AI reactor, so every read that actually
//! touches the keychain can raise the system permission dialog. The app polls
//! every 60 seconds. Reading through on every poll would, in the bad case,
//! mean a dialog every minute — which makes the app unusable.
//!
//! So: read once, hold the result in memory, and go back to the keychain only
//! when the cached token is at or near expiry. In steady state the 60-second
//! poll never touches the keychain.
//!
//! # The denial backoff
//!
//! If the user clicks Deny, we stop reading the keychain for a while. A denied
//! permission that retries every minute is a dialog loop with extra steps.
//!
//! But it must not latch *forever* either. There is no longer a manual refresh
//! button — the app health-checks itself on a timer — so a permanent latch
//! would leave an accidental misclick unrecoverable short of relaunching.
//!
//! So the wait doubles: a minute, then two, four, up to an hour. A misclick
//! costs sixty seconds; a deliberate refusal settles down to one prompt an
//! hour instead of sixty.

use std::sync::Mutex;

use zeroize::Zeroizing;

use super::{
    error::CredentialError, file, now_ms, parse, token::AccessToken, CredentialState, Credentials,
};

/// Produces the raw credential bytes. Boxed so tests can substitute a fake
/// without a keychain, a `claude` login, or a permission dialog.
type Loader = Box<dyn Fn() -> Result<Zeroizing<Vec<u8>>, CredentialError> + Send + Sync>;

pub struct CredentialStore {
    load: Loader,
    inner: Mutex<Inner>,
}

/// First wait after a denial. Short enough that a misclick is forgiven almost
/// immediately.
const DENIAL_BASE_MS: i64 = 60_000;
/// Ceiling on the doubling. One prompt an hour is the floor of annoyance we
/// are willing to impose on someone who genuinely meant to refuse.
const DENIAL_MAX_MS: i64 = 60 * 60_000;

#[derive(Default)]
struct Inner {
    cached: Option<Credentials>,
    /// No keychain read before this instant, because the last one was refused.
    denied_until_ms: Option<i64>,
    /// How many refusals in a row, which sets the length of the next wait.
    denials: u32,
}

impl Inner {
    /// Record a refusal and work out how long to stay away.
    fn note_denied(&mut self, now_ms: i64) {
        self.denials = self.denials.saturating_add(1);
        // `min(20)` on the shift keeps the exponent from overflowing after a
        // very long stand-off.
        let wait = DENIAL_BASE_MS
            .saturating_mul(1_i64 << (self.denials - 1).min(20))
            .min(DENIAL_MAX_MS);
        self.denied_until_ms = Some(now_ms + wait);
    }

    fn denial_active(&self, now_ms: i64) -> bool {
        self.denied_until_ms.is_some_and(|until| now_ms < until)
    }
}

impl CredentialStore {
    /// The real thing: OS credential store first, file second.
    pub fn new() -> Self {
        Self::with_loader(Box::new(default_load))
    }

    pub fn with_loader(load: Loader) -> Self {
        Self {
            load,
            inner: Mutex::new(Inner::default()),
        }
    }

    /// Current state, using the cache where possible.
    pub fn state(&self) -> CredentialState {
        self.state_at(now_ms())
    }

    /// `state`, with the clock injected so expiry behaviour is testable
    /// without touching the system clock.
    pub fn state_at(&self, now_ms: i64) -> CredentialState {
        // A poisoned mutex means another thread panicked while holding it.
        // Recovering the guard is right here: the data behind it is a cache
        // and a bool, neither of which can be left half-written, and taking
        // the whole app down over it would be worse than carrying on.
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());

        if inner.denial_active(now_ms) {
            return CredentialState::Denied;
        }

        if let Some(cached) = &inner.cached {
            if cached.is_usable_at(now_ms) {
                return CredentialState::Ok {
                    expires_at_ms: cached.expires_at_ms(),
                };
            }
            // Stale. Drop it and read through — the CLI may have refreshed.
            inner.cached = None;
        }

        match (self.load)().and_then(|bytes| parse::parse(&bytes)) {
            Ok(cred) => {
                let state = CredentialState::from_credentials(&cred, now_ms);
                // Only cache a credential we would actually use. Caching an
                // expired one would just make us re-read it every poll.
                if matches!(state, CredentialState::Ok { .. }) {
                    inner.cached = Some(cred);
                }
                state
            }
            Err(CredentialError::Denied) => {
                inner.note_denied(now_ms);
                CredentialState::Denied
            }
            Err(other) => other.into(),
        }
    }

    /// Forget everything and read through immediately, prompt and all.
    ///
    /// Kept for a deliberate user action — "ask me again now" — even though
    /// nothing in the UI calls it today. The automatic health check gets there
    /// on its own; this only skips the wait.
    #[allow(dead_code)]
    pub fn recheck(&self) -> CredentialState {
        {
            let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            inner.denied_until_ms = None;
            inner.denials = 0;
            inner.cached = None;
        }
        self.state()
    }

    /// Run `f` with the access token, if there is a usable one.
    ///
    /// Milestone 3 builds its request inside this closure. Handing out a
    /// borrow instead of a copy means the token never outlives the lock and
    /// never lands in a variable someone might log.
    // Unused until milestone 3 makes the first network call.
    #[allow(dead_code)]
    pub fn use_token<T>(&self, f: impl FnOnce(&AccessToken) -> T) -> Result<T, CredentialState> {
        self.use_token_at(now_ms(), f)
    }

    /// `use_token`, with the clock injected — same split as
    /// [`CredentialStore::state_at`], so expiry behaviour is testable without
    /// touching the system clock.
    #[allow(dead_code)]
    pub fn use_token_at<T>(
        &self,
        now_ms: i64,
        f: impl FnOnce(&AccessToken) -> T,
    ) -> Result<T, CredentialState> {
        let state = self.state_at(now_ms);
        if !matches!(state, CredentialState::Ok { .. }) {
            return Err(state);
        }
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        match &inner.cached {
            Some(cred) => Ok(f(cred.access_token())),
            // `state` said Ok, so the cache was populated a moment ago; only a
            // racing `recheck` can land us here. Report it rather than guess.
            None => Err(CredentialState::Unreadable {
                reason: "자격증명이 방금 초기화됨".into(),
            }),
        }
    }
}

impl Default for CredentialStore {
    fn default() -> Self {
        Self::new()
    }
}

/// Keychain first, file second.
///
/// Order matters. On macOS the keychain is where the CLI actually keeps the
/// live credential; a `.credentials.json` left over from an older CLI version
/// could be stale, and reading it first would hand us a long-dead token.
fn default_load() -> Result<Zeroizing<Vec<u8>>, CredentialError> {
    match crate::platform::keychain_read() {
        // Only "nothing there" falls through to the file. A denial or a locked
        // keychain is a real answer and must not be papered over.
        Err(CredentialError::NotFound) => file::read().map(Zeroizing::new),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    const HOUR_MS: i64 = 3_600_000;

    fn blob(expires_at_ms: i64) -> Zeroizing<Vec<u8>> {
        Zeroizing::new(
            format!(
                r#"{{"claudeAiOauth":{{"accessToken":"EXAMPLE","refreshToken":"NOPE","expiresAt":{expires_at_ms}}}}}"#
            )
            .into_bytes(),
        )
    }

    /// A loader that counts how many times it was actually called.
    fn counting(
        result: impl Fn() -> Result<Zeroizing<Vec<u8>>, CredentialError> + Send + Sync + 'static,
    ) -> (CredentialStore, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        let c = calls.clone();
        let store = CredentialStore::with_loader(Box::new(move || {
            c.fetch_add(1, Ordering::SeqCst);
            result()
        }));
        (store, calls)
    }

    #[test]
    fn valid_credential_reports_ok() {
        let (store, _) = counting(|| Ok(blob(10 * HOUR_MS)));
        assert_eq!(
            store.state_at(0),
            CredentialState::Ok {
                expires_at_ms: 10 * HOUR_MS
            }
        );
    }

    /// The whole point of the cache: repeated polls must not touch the store.
    #[test]
    fn repeated_polls_read_the_keychain_once() {
        let (store, calls) = counting(|| Ok(blob(10 * HOUR_MS)));
        for _ in 0..60 {
            assert!(matches!(store.state_at(0), CredentialState::Ok { .. }));
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1, "cache did not hold");
    }

    /// Once the cached token ages out, we go back to the store — the CLI may
    /// have refreshed it in the meantime.
    #[test]
    fn expiring_cache_reads_through_again() {
        let (store, calls) = counting(|| Ok(blob(HOUR_MS)));
        assert!(matches!(store.state_at(0), CredentialState::Ok { .. }));
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        // Two hours later the cached token is long dead.
        assert!(matches!(
            store.state_at(2 * HOUR_MS),
            CredentialState::Expired { .. }
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn missing_credential_reports_missing() {
        let (store, _) = counting(|| Err(CredentialError::NotFound));
        assert_eq!(store.state_at(0), CredentialState::Missing);
    }

    #[test]
    fn expired_credential_reports_expired_and_never_refreshes() {
        let (store, _) = counting(|| Ok(blob(HOUR_MS)));
        assert_eq!(
            store.state_at(5 * HOUR_MS),
            CredentialState::Expired {
                expired_at_ms: HOUR_MS
            }
        );
    }

    /// The most important test in this file. A denial must not turn into a
    /// permission dialog every 60 seconds.
    #[test]
    fn denial_stops_the_health_check_from_reading() {
        let (store, calls) = counting(|| Err(CredentialError::Denied));
        assert_eq!(store.state_at(0), CredentialState::Denied);
        // Every health check for the next minute answers from the latch.
        for t in 0..60 {
            assert_eq!(store.state_at(t * 1_000), CredentialState::Denied);
        }
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "denial did not hold — this would spam the user with dialogs"
        );
    }

    /// ...but it must not hold forever either. With no manual refresh button,
    /// a permanent latch would make a misclick unrecoverable.
    #[test]
    fn denial_is_retried_after_a_minute() {
        let (store, calls) = counting(|| Err(CredentialError::Denied));
        assert_eq!(store.state_at(0), CredentialState::Denied);
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        store.state_at(DENIAL_BASE_MS);
        assert_eq!(calls.load(Ordering::SeqCst), 2, "should have asked again");
    }

    /// And repeated refusals back off, so someone who means it is not asked
    /// sixty times an hour.
    #[test]
    fn repeated_denials_back_off_towards_hourly() {
        let (store, calls) = counting(|| Err(CredentialError::Denied));
        let mut now = 0;
        store.state_at(now); // 1st refusal -> wait 1 min

        now += DENIAL_BASE_MS;
        store.state_at(now); // 2nd -> wait 2 min
        assert_eq!(calls.load(Ordering::SeqCst), 2);

        // One minute later is too soon now.
        store.state_at(now + DENIAL_BASE_MS);
        assert_eq!(calls.load(Ordering::SeqCst), 2, "should still be waiting");

        store.state_at(now + 2 * DENIAL_BASE_MS);
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    /// A very long stand-off must not overflow the shift into a negative wait.
    #[test]
    fn a_long_standoff_does_not_overflow() {
        let (store, _) = counting(|| Err(CredentialError::Denied));
        let mut now = 0_i64;
        for _ in 0..200 {
            store.state_at(now);
            now += DENIAL_MAX_MS;
        }
        // Still refusing, still bounded: one more hour gets one more attempt.
        assert_eq!(store.state_at(now), CredentialState::Denied);
    }

    /// Granting access after a refusal recovers on the next health check,
    /// with no user action beyond the dialog itself.
    #[test]
    fn granting_access_recovers_on_the_next_check() {
        use std::sync::atomic::AtomicBool;
        let refuse = Arc::new(AtomicBool::new(true));
        let flag = refuse.clone();
        let store = CredentialStore::with_loader(Box::new(move || {
            if flag.load(Ordering::SeqCst) {
                Err(CredentialError::Denied)
            } else {
                Ok(blob(10 * HOUR_MS))
            }
        }));

        assert_eq!(store.state_at(0), CredentialState::Denied);
        refuse.store(false, Ordering::SeqCst);

        assert!(matches!(
            store.state_at(DENIAL_BASE_MS),
            CredentialState::Ok { .. }
        ));
    }

    /// An explicit retry skips the wait entirely.
    #[test]
    fn recheck_asks_again_immediately() {
        let (store, calls) = counting(|| Err(CredentialError::Denied));
        assert_eq!(store.state_at(0), CredentialState::Denied);
        assert_eq!(store.recheck(), CredentialState::Denied);
        assert_eq!(calls.load(Ordering::SeqCst), 2, "retry should read through");
    }

    #[test]
    fn locked_keychain_is_unreadable_not_denied() {
        let (store, _) = counting(|| Err(CredentialError::Locked));
        assert!(matches!(
            store.state_at(0),
            CredentialState::Unreadable { .. }
        ));
    }

    #[test]
    fn garbage_in_the_store_is_unreadable_not_a_panic() {
        let (store, _) = counting(|| Ok(Zeroizing::new(b"{}".to_vec())));
        assert!(matches!(
            store.state_at(0),
            CredentialState::Unreadable { .. }
        ));
    }

    #[test]
    fn use_token_yields_the_token_only_when_usable() {
        let (store, _) = counting(|| Ok(blob(10 * HOUR_MS)));
        let len = store
            .use_token_at(0, |t| t.len())
            .expect("should have a token");
        assert_eq!(len, "EXAMPLE".len());
    }

    /// An expired credential hands back the *state*, not a token — so the
    /// caller in milestone 3 is forced to deal with expiry instead of firing a
    /// request that would 401.
    #[test]
    fn use_token_refuses_an_expired_credential() {
        let (store, _) = counting(|| Ok(blob(HOUR_MS)));
        assert!(matches!(
            store.use_token_at(5 * HOUR_MS, |t| t.len()),
            Err(CredentialState::Expired { .. })
        ));
    }

    /// And a denial reaches the caller as a denial, not as a missing token.
    #[test]
    fn use_token_refuses_after_a_denial() {
        let (store, _) = counting(|| Err(CredentialError::Denied));
        assert!(matches!(
            store.use_token_at(0, |t| t.len()),
            Err(CredentialState::Denied)
        ));
    }
}
