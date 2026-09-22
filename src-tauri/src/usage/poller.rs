//! Reading each provider on a schedule, and deciding how much to trust what
//! comes back.
//!
//! # The contract with the UI
//!
//! Every provider always has a snapshot. A failure never produces an error
//! screen; it produces the previous numbers marked `stale`, or an honest
//! "we don't know". The monkey is always reporting something, even if what it
//! is reporting is that it cannot see.
//!
//! # One poller per provider
//!
//! Each provider gets its own backoff and its own last-good snapshot. They
//! must not share: Claude being rate-limited says nothing about whether the
//! Codex log is readable, and one provider's outage must not blank the other.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use super::{
    backoff::Backoff, schedule::PollSchedule, LimitWindow, ProviderId, ProviderSnapshot, Reading,
    SnapshotStatus, UsageError, UsageProvider, ErrorReason,
};
use crate::credentials::{now_ms, CredentialState};

/// One provider's poller: the adapter, its cache, and its backoff.
pub struct ProviderPoller {
    provider: Box<dyn UsageProvider>,
    state: Mutex<State>,
}

struct State {
    current: ProviderSnapshot,
    /// The last reading we actually got, whatever has happened since. This is
    /// what `stale` is made of.
    last_good: Option<ProviderSnapshot>,
    /// Why the last read failed, if it did. The status says *that* something
    /// went wrong; this says what, which is the difference between a
    /// diagnosable report and a shrug.
    last_error: Option<String>,
    backoff: Backoff,
}

impl ProviderPoller {
    #[allow(dead_code)] // the registry builds pollers; this is the single door
    pub fn new(provider: Box<dyn UsageProvider>) -> Self {
        Self::with_backoff(provider, Backoff::new())
    }

    fn with_backoff(provider: Box<dyn UsageProvider>, backoff: Backoff) -> Self {
        let id = provider.id();
        Self {
            provider,
            state: Mutex::new(State {
                // Before the first read we know nothing. Not "installed", not
                // "empty" — unknown, which is `Void`.
                current: ProviderSnapshot::empty(id, SnapshotStatus::Error, now_ms()),
                last_good: None,
                last_error: None,
                backoff,
            }),
        }
    }

    pub fn id(&self) -> ProviderId {
        self.provider.id()
    }

    /// The current snapshot without reading anything.
    pub fn snapshot(&self) -> ProviderSnapshot {
        self.lock().current.clone()
    }

    pub fn last_error(&self) -> Option<String> {
        self.lock().last_error.clone()
    }

    /// How long until this provider may be read again, in milliseconds.
    #[allow(dead_code)] // surfaced on the provider card in milestone 4
    pub fn backoff_remaining_ms(&self, now_ms: i64) -> i64 {
        self.lock().backoff.remaining_ms(now_ms)
    }

    #[allow(dead_code)] // the registry polls; this is the single-provider door
    pub fn poll(&self) -> ProviderSnapshot {
        self.poll_at(now_ms())
    }

    fn allow_manual_retry(&self) {
        self.lock().backoff.clear_transient_wait();
    }

    /// `poll`, with the clock injected so backoff and staleness are testable.
    pub fn poll_at(&self, now_ms: i64) -> ProviderSnapshot {
        // Check the backoff *before* reading, and release the lock before any
        // I/O — holding a mutex across a five-second network call would block
        // the menu bar behind it.
        {
            let state = self.lock();
            if !state.backoff.allows_request(now_ms) {
                return state.current.clone();
            }
        }

        // "Not installed" is permanent and free to detect, so it short-circuits
        // ahead of any credential read or network call.
        let result = if self.provider.installed() {
            self.fetch_without_dying()
        } else {
            Err(UsageError::NotInstalled)
        };

        let mut state = self.lock();
        let id = self.provider.id();
        state.current = state.apply(id, result, now_ms);
        state.current.clone()
    }

    /// Call the adapter, turning a panic into an ordinary failure.
    ///
    /// Adapters wrap undocumented formats and third-party stacks. A panic in
    /// one — a TLS provider misconfiguration did exactly this once — would
    /// otherwise kill the polling thread, and the app would sit there forever
    /// showing its last snapshot with no sign anything was wrong. Failure is a
    /// normal path, and a panic is a failure that forgot to say so.
    fn fetch_without_dying(&self) -> Result<Reading, UsageError> {
        use std::panic::{catch_unwind, AssertUnwindSafe};

        match catch_unwind(AssertUnwindSafe(|| self.provider.fetch())) {
            Ok(result) => result,
            Err(payload) => {
                let detail = payload
                    .downcast_ref::<&str>()
                    .map(|s| s.to_string())
                    .or_else(|| payload.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "알 수 없음".into());
                Err(UsageError::Transport(format!("내부 오류: {detail}")))
            }
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        // Poisoning means a previous poll panicked. Behind the lock is a
        // snapshot and a counter; carrying on with them beats taking the menu
        // bar app down.
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl State {
    fn apply(
        &mut self,
        id: ProviderId,
        result: Result<Reading, UsageError>,
        now_ms: i64,
    ) -> ProviderSnapshot {
        self.last_error = result.as_ref().err().map(|e| e.to_string());

        match result {
            Ok(reading) => {
                self.backoff.note_success();

                // Claude answers for right now; Codex hands back a log entry
                // that may be months old. Taking the provider's word for
                // *when* is what keeps the second case from being stamped as
                // current.
                let captured_at_ms = reading.captured_at_ms.unwrap_or(now_ms);
                let windows = reading.windows;

                let status = freshness(&windows, captured_at_ms, now_ms);
                let snapshot = ProviderSnapshot {
                    provider: id,
                    windows,
                    captured_at_ms,
                    status,
                    error_reason: (status == SnapshotStatus::Error)
                        .then_some(ErrorReason::WindowReset),
                };
                // Only cache a reading we would actually show. Caching a void
                // one would make `degrade` resurrect it later.
                if status != SnapshotStatus::Error {
                    self.last_good = Some(snapshot.clone());
                }
                snapshot
            }

            // Not set up here. Permanent and quiet: no spinner, no retry
            // storm, and the card says so rather than pretending to load.
            Err(UsageError::NotInstalled)
            | Err(UsageError::NoCredential(CredentialState::Missing)) => {
                self.backoff.note_expired(now_ms);
                ProviderSnapshot::empty(id, SnapshotStatus::NotInstalled, now_ms)
            }

            // Set up, but nothing has ever been recorded. Same shrug as a
            // rolled-over window — no number we can stand behind — but the fix
            // is different, so the reason travels with it.
            Err(UsageError::NoData) => {
                self.backoff.note_expired(now_ms);
                ProviderSnapshot::empty(id, SnapshotStatus::Error, now_ms)
                    .with_reason(ErrorReason::NoData)
            }

            // Signed in, but the account has no limits to report. Telling this
            // person to go run the CLI would be useless advice.
            Err(UsageError::NoPlan) => {
                self.backoff.note_expired(now_ms);
                ProviderSnapshot::empty(id, SnapshotStatus::Error, now_ms)
                    .with_reason(ErrorReason::NoPlan)
            }

            // The token is dead. We do not refresh — the CLI owns it. Keep the
            // last numbers, because "37% as of ten minutes ago" beats a blank
            // panel, but mark them so the card can explain the fix.
            Err(UsageError::Unauthorized)
            | Err(UsageError::NoCredential(CredentialState::Expired { .. })) => {
                self.backoff.note_expired(now_ms);
                self.degrade(id, SnapshotStatus::Expired, now_ms)
            }

            // A keychain we cannot consult is not a failed read. It clears
            // once someone is at the machine, so it must not latch, and the
            // card must not tell them to retry something they cannot.
            Err(UsageError::NoCredential(CredentialState::Unreadable { .. })) => {
                self.backoff.note_expired(now_ms);
                match self.degrade(id, SnapshotStatus::Stale, now_ms) {
                    mut snapshot if snapshot.windows.is_empty() => {
                        snapshot.error_reason = Some(ErrorReason::Locked);
                        snapshot
                    }
                    snapshot => snapshot,
                }
            }

            // Denied at the keychain prompt. Same backoff as any credential
            // miss, but the card needs to say *why* there is nothing, or the
            // next prompt looks unexplained and gets denied again.
            Err(UsageError::NoCredential(CredentialState::Denied)) => {
                self.backoff.note_expired(now_ms);
                match self.degrade(id, SnapshotStatus::Stale, now_ms) {
                    mut snapshot if snapshot.windows.is_empty() => {
                        snapshot.error_reason = Some(ErrorReason::Denied);
                        snapshot
                    }
                    snapshot => snapshot,
                }
            }

            Err(UsageError::NoCredential(_)) => {
                self.backoff.note_expired(now_ms);
                self.degrade(id, SnapshotStatus::Stale, now_ms)
            }

            Err(UsageError::RateLimited { retry_after_ms }) => {
                // Throttling of the usage query, not a spent limit: pause
                // briefly and let the regular poll try again.
                self.backoff.note_rate_limited(now_ms, retry_after_ms);
                self.degrade(id, SnapshotStatus::Stale, now_ms)
            }

            // Network down, 5xx, unreadable body — same story to the user:
            // these numbers are old.
            Err(_) => {
                self.backoff.note_failure(now_ms);
                self.degrade(id, SnapshotStatus::Stale, now_ms)
            }
        }
    }

    /// Re-badge the last good reading, or admit we have nothing.
    ///
    /// `captured_at_ms` is deliberately *not* touched: it still says when the
    /// numbers were true, which is the whole point of a stale snapshot.
    fn degrade(&self, id: ProviderId, status: SnapshotStatus, now_ms: i64) -> ProviderSnapshot {
        match &self.last_good {
            Some(good) => {
                // Even the cached reading expires. Once its windows have
                // rolled over it stops being old news and becomes wrong.
                if good.windows.iter().all(|w| w.is_void_at(now_ms)) {
                    return ProviderSnapshot::empty(id, SnapshotStatus::Error, now_ms)
                        .with_reason(ErrorReason::WindowReset);
                }
                // A failed check leaves the last reading standing; it only
                // becomes "stale" once it is old enough to doubt.
                let status = match status {
                    SnapshotStatus::Stale
                        if now_ms - good.captured_at_ms <= STALE_AFTER_MS =>
                    {
                        SnapshotStatus::Ok
                    }
                    other => other,
                };
                ProviderSnapshot {
                    provider: id,
                    windows: good.windows.clone(),
                    captured_at_ms: good.captured_at_ms,
                    status,
                    error_reason: None,
                }
            }
            // Nothing has ever been read. "Stale" would be a lie — it claims
            // there are old numbers behind it.
            None => match status {
                SnapshotStatus::Stale => ProviderSnapshot::empty(id, SnapshotStatus::Error, now_ms)
                    .with_reason(ErrorReason::ReadFailed),
                other => ProviderSnapshot::empty(id, other, now_ms),
            },
        }
    }
}

/// How old a reading may get before the card flags it.
///
/// The poller refreshes every three minutes, so a reading older than this
/// means about three checks in a row came back empty-handed — a real problem
/// worth a warning. One failed check (say, right after wake, before Wi-Fi is
/// back) leaves a reading a few minutes old that is still right, and flagging
/// it would only teach people to ignore the flag.
const STALE_AFTER_MS: i64 = 10 * 60 * 1000;

/// How much to trust a reading, given when it was captured.
fn freshness(windows: &[LimitWindow], captured_at_ms: i64, now_ms: i64) -> SnapshotStatus {
    if windows.is_empty() {
        return SnapshotStatus::Error;
    }
    // A reading whose windows have all already reset describes periods that no
    // longer exist. That is not a fresh reading of an empty limit — it is no
    // reading at all. Codex hits this routinely, because its numbers come from
    // a log that stops being written the moment you stop using it.
    if windows.iter().all(|w| w.is_void_at(now_ms)) {
        return SnapshotStatus::Error;
    }

    if now_ms - captured_at_ms > STALE_AFTER_MS {
        SnapshotStatus::Stale
    } else {
        SnapshotStatus::Ok
    }
}

/// Every provider, polled together.
pub struct UsageRegistry {
    pollers: Vec<Arc<ProviderPoller>>,
    /// When the last health check ran, whatever it found.
    ///
    /// There is no manual refresh button — the app checks itself on a timer —
    /// so the panel has to be able to say "checked a moment ago". Without that
    /// a quiet screen is indistinguishable from a stuck one, which is exactly
    /// the reassurance the button used to provide by being pressable.
    last_checked_at_ms: Mutex<Option<i64>>,
}

impl UsageRegistry {
    pub fn new(providers: Vec<Box<dyn UsageProvider>>) -> Self {
        Self::build(providers, Backoff::new)
    }

    /// Poll without ever backing off. Demo mode only — every state it walks
    /// through is a failure, and a real backoff would park on the first one.
    pub fn unthrottled(providers: Vec<Box<dyn UsageProvider>>) -> Self {
        Self::build(providers, Backoff::disabled)
    }

    fn build(providers: Vec<Box<dyn UsageProvider>>, backoff: fn() -> Backoff) -> Self {
        Self {
            pollers: providers
                .into_iter()
                .map(|p| Arc::new(ProviderPoller::with_backoff(p, backoff())))
                .collect(),
            last_checked_at_ms: Mutex::new(None),
        }
    }

    /// When the last check ran. `None` before the first one.
    pub fn last_checked_at_ms(&self) -> Option<i64> {
        *self
            .last_checked_at_ms
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    /// Every provider's signed-in account and plan, in `ProviderId::ALL`
    /// order. Reads local files each call — meant for "the popover opened",
    /// not for a timer.
    pub fn accounts(&self) -> Vec<super::ProviderAccount> {
        let mut out: Vec<super::ProviderAccount> = self
            .pollers
            .iter()
            .map(|p| super::ProviderAccount {
                provider: p.id(),
                account: p.provider.account(),
            })
            .collect();
        out.sort_by_key(|a| a.provider);
        out
    }

    /// Current snapshots, in `ProviderId::ALL` order so the monkey's hands
    /// never swap.
    pub fn snapshots(&self) -> Vec<ProviderSnapshot> {
        let mut out: Vec<ProviderSnapshot> = self.pollers.iter().map(|p| p.snapshot()).collect();
        out.sort_by_key(|s| s.provider);
        out
    }

    /// Read every provider once.
    ///
    /// The timestamp records the *attempt*, not a success. A check that found
    /// everything broken still happened, and "3분 전 확인" is the honest thing
    /// to say about it.
    pub fn poll_all_at(&self, now_ms: i64) -> Vec<ProviderSnapshot> {
        for poller in &self.pollers {
            poller.poll_at(now_ms);
        }
        *self
            .last_checked_at_ms
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(now_ms);

        self.snapshots()
    }

    pub fn poll_all(&self) -> Vec<ProviderSnapshot> {
        self.poll_all_at(now_ms())
    }

    /// User-requested refresh: retry ordinary wake/network/auth failures now,
    /// while preserving a provider's explicit 429 deadline.
    pub fn poll_all_manual(&self) -> Vec<ProviderSnapshot> {
        for poller in &self.pollers {
            poller.allow_manual_retry();
        }
        self.poll_all()
    }

    /// Failure reasons, for the log and the provider cards.
    pub fn errors(&self) -> Vec<(ProviderId, String)> {
        self.pollers
            .iter()
            .filter_map(|p| p.last_error().map(|e| (p.id(), e)))
            .collect()
    }

    /// Only the providers that are actually set up here. The menu bar shows
    /// these; the popover shows all of them so an empty card can invite.
    #[allow(dead_code)] // the menu bar filters with this in milestone 5
    pub fn installed(&self) -> Vec<ProviderSnapshot> {
        self.snapshots()
            .into_iter()
            .filter(|s| s.status != SnapshotStatus::NotInstalled)
            .collect()
    }
}

/// Run the poll loop on its own thread until the process exits.
///
/// A plain OS thread rather than an async task: a file read and one HTTP GET
/// per cycle, on a thread parked on a condition variable. `on_poll` runs on
/// that thread, so keep it cheap.
pub fn spawn(
    registry: Arc<UsageRegistry>,
    schedule: Arc<PollSchedule>,
    on_poll: impl Fn(&[ProviderSnapshot], &[(ProviderId, String)]) + Send + 'static,
) {
    std::thread::Builder::new()
        .name("ai-reactor-usage-poller".into())
        .spawn(move || loop {
            // Stamp the time *before* the reads, so a slow response does not
            // push the next cycle out by however long it took.
            let started = Instant::now();
            let snapshots = registry.poll_all();
            on_poll(&snapshots, &registry.errors());
            schedule.wait_until_due(started);
        })
        .expect("failed to spawn the usage poller thread");
}

/// A logging callback for [`spawn`] that only speaks when something changed.
///
/// While backing off, every cycle returns the identical cached snapshot.
/// Printing that each time reads like a retry storm — the opposite of what is
/// happening — and buries the lines that matter.
pub fn deduplicating_logger(
    render: impl Fn(&[ProviderSnapshot], &[(ProviderId, String)]) -> String + Send + 'static,
) -> impl Fn(&[ProviderSnapshot], &[(ProviderId, String)]) + Send + 'static {
    let last = Mutex::new(String::new());
    move |snapshots, errors| {
        let line = render(snapshots, errors);
        let mut last = last.lock().unwrap_or_else(|e| e.into_inner());
        if *last == line {
            return;
        }
        *last = line.clone();
        eprintln!("{line}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const MIN: i64 = 60_000;
    const HOUR: i64 = 60 * MIN;

    /// A provider that replays a scripted sequence of results.
    struct Scripted {
        id: ProviderId,
        installed: bool,
        results: Mutex<Vec<Result<Reading, UsageError>>>,
        calls: Arc<AtomicUsize>,
    }

    impl UsageProvider for Scripted {
        fn id(&self) -> ProviderId {
            self.id
        }
        fn installed(&self) -> bool {
            self.installed
        }
        fn fetch(&self) -> Result<Reading, UsageError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let mut results = self.results.lock().unwrap();
            if results.is_empty() {
                return Err(UsageError::Transport("script exhausted".into()));
            }
            results.remove(0)
        }
    }

    fn poller_from(
        results: Vec<Result<Reading, UsageError>>,
    ) -> (ProviderPoller, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        let provider = Scripted {
            id: ProviderId::Claude,
            installed: true,
            results: Mutex::new(results),
            calls: calls.clone(),
        };
        (ProviderPoller::new(Box::new(provider)), calls)
    }

    /// A live reading whose window resets far enough ahead to stay valid.
    fn windows(remaining: f64) -> Reading {
        Reading::live(vec![LimitWindow {
            window_minutes: 300,
            remaining_percent: remaining,
            resets_at_ms: 1_000 * HOUR,
        }])
    }

    #[test]
    fn a_successful_poll_is_fresh() {
        let (poller, _) = poller_from(vec![Ok(windows(58.0))]);
        let snapshot = poller.poll_at(0);
        assert_eq!(snapshot.status, SnapshotStatus::Ok);
        assert_eq!(snapshot.remaining_percent(), Some(58.0));
        assert_eq!(snapshot.provider, ProviderId::Claude);
    }

    /// The core promise: a failure shows the old numbers, not a blank panel —
    /// and only flags them once they are old enough to doubt.
    #[test]
    fn a_failure_after_a_success_goes_stale_keeping_the_numbers() {
        let (poller, _) = poller_from(vec![
            Ok(windows(58.0)),
            Err(UsageError::Transport("offline".into())),
            Err(UsageError::Transport("offline".into())),
        ]);
        poller.poll_at(0);

        // One missed check (right after wake, say): still trusted.
        let recent = poller.poll_at(3 * MIN);
        assert_eq!(recent.status, SnapshotStatus::Ok);
        assert_eq!(recent.captured_at_ms, 0);

        let stale = poller.poll_at(11 * MIN);
        assert_eq!(stale.status, SnapshotStatus::Stale);
        assert_eq!(stale.remaining_percent(), Some(58.0), "numbers are kept");
        assert_eq!(stale.captured_at_ms, 0, "timestamp says when they were true");
    }

    #[test]
    fn a_failure_with_nothing_cached_reports_unknown() {
        let (poller, _) = poller_from(vec![Err(UsageError::Transport("offline".into()))]);
        let snapshot = poller.poll_at(0);
        assert_eq!(snapshot.status, SnapshotStatus::Error);
        assert!(snapshot.windows.is_empty());
    }

    // ---- the void state ----

    /// The distinction the spec insists on. A reading whose window has already
    /// reset is not old news; it describes a period that no longer exists.
    #[test]
    fn a_reading_whose_window_already_reset_is_void_not_fresh() {
        let stale_window = Reading::live(vec![LimitWindow {
            window_minutes: 43_200,
            remaining_percent: 0.0,
            resets_at_ms: 10 * MIN,
        }]);
        let (poller, _) = poller_from(vec![Ok(stale_window)]);

        // Read it an hour after that window ended.
        let snapshot = poller.poll_at(HOUR);
        assert_eq!(snapshot.status, SnapshotStatus::Error);
        assert_eq!(
            snapshot.remaining_percent(),
            None,
            "a rolled-over window must not report 0% remaining"
        );
    }

    /// And a cached reading rots the same way: fine while its window lives,
    /// worthless once the window turns over.
    #[test]
    fn a_cached_reading_becomes_void_once_its_window_resets() {
        let short = Reading::live(vec![LimitWindow {
            window_minutes: 300,
            remaining_percent: 40.0,
            resets_at_ms: 2 * HOUR,
        }]);
        let (poller, _) = poller_from(vec![
            Ok(short),
            Err(UsageError::Transport("offline".into())),
            Err(UsageError::Transport("still offline".into())),
        ]);
        assert_eq!(poller.poll_at(0).status, SnapshotStatus::Ok);

        // Offline, but the cached window is still live: stale with numbers.
        let stale = poller.poll_at(HOUR);
        assert_eq!(stale.status, SnapshotStatus::Stale);
        assert_eq!(stale.remaining_percent(), Some(40.0));

        // Offline past the reset: the cached number is now about a window that
        // ended, so we must stop showing it.
        let void = poller.poll_at(5 * HOUR);
        assert_eq!(void.status, SnapshotStatus::Error);
        assert_eq!(void.remaining_percent(), None);
    }

    #[test]
    fn an_empty_reading_is_void() {
        let (poller, _) = poller_from(vec![Ok(Reading::live(vec![]))]);
        assert_eq!(poller.poll_at(0).status, SnapshotStatus::Error);
    }

    #[test]
    fn no_recorded_data_is_void() {
        let (poller, _) = poller_from(vec![Err(UsageError::NoData)]);
        assert_eq!(poller.poll_at(0).status, SnapshotStatus::Error);
    }

    // ---- freshness ----

    /// Three missed three-minute checks is the line: past ten minutes old, a
    /// reading is flagged, whatever the window length.
    #[test]
    fn a_reading_goes_stale_after_ten_minutes() {
        let logged = |window_minutes| Reading {
            windows: vec![LimitWindow {
                window_minutes,
                remaining_percent: 40.0,
                resets_at_ms: 1_000 * HOUR,
            }],
            captured_at_ms: Some(0),
        };

        for window in [300, 43_200] {
            let (poller, _) = poller_from(vec![Ok(logged(window)), Ok(logged(window))]);
            assert_eq!(poller.poll_at(10 * MIN).status, SnapshotStatus::Ok);
            assert_eq!(poller.poll_at(10 * MIN + 1).status, SnapshotStatus::Stale);
        }
    }

    /// A provider that answers for *now* — Claude — never ages on its own.
    #[test]
    fn a_live_reading_is_always_fresh() {
        let (poller, _) = poller_from(vec![Ok(windows(40.0))]);
        assert_eq!(poller.poll_at(500 * HOUR).status, SnapshotStatus::Ok);
    }

    /// Codex's numbers come out of a log; taking its word for *when* is what
    /// keeps a months-old entry from being stamped as current.
    #[test]
    fn the_providers_capture_time_is_kept_not_overwritten() {
        let logged = Reading {
            windows: vec![LimitWindow {
                window_minutes: 43_200,
                remaining_percent: 64.0,
                resets_at_ms: 1_000 * HOUR,
            }],
            captured_at_ms: Some(3 * HOUR),
        };
        let (poller, _) = poller_from(vec![Ok(logged)]);
        let snapshot = poller.poll_at(10 * HOUR);
        assert_eq!(
            snapshot.captured_at_ms,
            3 * HOUR,
            "should report when the log said, not when we read it"
        );
    }

    // ---- why there is nothing to show ----

    /// The three empty answers need three different sentences on the card, so
    /// they have to survive as far as the UI.
    #[test]
    fn the_reason_for_an_empty_card_survives() {
        let cases = [
            (UsageError::NoData, ErrorReason::NoData),
            (UsageError::NoPlan, ErrorReason::NoPlan),
            (
                UsageError::Transport("offline".into()),
                ErrorReason::ReadFailed,
            ),
        ];
        for (error, expected) in cases {
            let (poller, _) = poller_from(vec![Err(error.clone())]);
            let snapshot = poller.poll_at(0);
            assert_eq!(snapshot.status, SnapshotStatus::Error, "{error:?}");
            assert_eq!(snapshot.error_reason, Some(expected), "{error:?}");
        }
    }

    /// Signed in with no plan must not be reported as "go run the CLI" — that
    /// advice does nothing for someone who has no subscription.
    #[test]
    fn no_plan_is_distinguishable_from_no_data() {
        let (no_plan, _) = poller_from(vec![Err(UsageError::NoPlan)]);
        let (no_data, _) = poller_from(vec![Err(UsageError::NoData)]);
        assert_ne!(
            no_plan.poll_at(0).error_reason,
            no_data.poll_at(0).error_reason
        );
    }

    #[test]
    fn a_rolled_over_window_says_so() {
        let expired = Reading {
            windows: vec![LimitWindow {
                window_minutes: 43_200,
                remaining_percent: 0.0,
                resets_at_ms: 1 * HOUR,
            }],
            captured_at_ms: Some(0),
        };
        let (poller, _) = poller_from(vec![Ok(expired)]);
        let snapshot = poller.poll_at(10 * HOUR);
        assert_eq!(snapshot.error_reason, Some(ErrorReason::WindowReset));
    }

    // ---- not installed ----

    /// An uninstalled provider must never be read. Detecting it has to be free
    /// — no keychain prompt, no network — or the empty card costs more than
    /// the real one.
    #[test]
    fn an_uninstalled_provider_is_never_fetched() {
        let calls = Arc::new(AtomicUsize::new(0));
        let poller = ProviderPoller::new(Box::new(Scripted {
            id: ProviderId::Codex,
            installed: false,
            results: Mutex::new(vec![Ok(windows(90.0))]),
            calls: calls.clone(),
        }));

        for t in 0..10 {
            let snapshot = poller.poll_at(t * HOUR);
            assert_eq!(snapshot.status, SnapshotStatus::NotInstalled);
            assert!(snapshot.windows.is_empty());
        }
        assert_eq!(calls.load(Ordering::SeqCst), 0, "must not read the adapter");
    }

    #[test]
    fn a_missing_credential_reads_as_not_installed() {
        let (poller, _) = poller_from(vec![Err(UsageError::NoCredential(
            CredentialState::Missing,
        ))]);
        assert_eq!(poller.poll_at(0).status, SnapshotStatus::NotInstalled);
    }

    /// A Deny at the keychain prompt must say so on the card, not read as a
    /// generic failure — the fix is a specific button the next time it asks.
    #[test]
    fn a_denied_keychain_says_so() {
        let (poller, _) = poller_from(vec![Err(UsageError::NoCredential(
            CredentialState::Denied,
        ))]);
        let snapshot = poller.poll_at(0);
        assert_eq!(snapshot.error_reason, Some(ErrorReason::Denied));
        assert!(snapshot.windows.is_empty());
    }

    // ---- expiry and backoff ----

    #[test]
    fn a_401_reports_expired_and_keeps_the_last_numbers() {
        let (poller, _) = poller_from(vec![Ok(windows(58.0)), Err(UsageError::Unauthorized)]);
        poller.poll_at(0);

        let snapshot = poller.poll_at(MIN);
        assert_eq!(snapshot.status, SnapshotStatus::Expired);
        assert_eq!(snapshot.remaining_percent(), Some(58.0));
    }

    /// A 429 throttles the usage query, not the user's limit: the gauge must
    /// not freeze until the window resets, only skip until the next poll.
    #[test]
    fn a_429_resumes_on_the_next_poll_cycle_not_at_the_window_reset() {
        let reset = 5 * 60 * MIN;
        let reading = Reading::live(vec![LimitWindow {
            window_minutes: 300,
            remaining_percent: 40.0,
            resets_at_ms: reset,
        }]);
        let (poller, calls) = poller_from(vec![
            Ok(reading),
            Err(UsageError::RateLimited {
                retry_after_ms: Some(0),
            }),
            Ok(windows(35.0)),
        ]);

        poller.poll_at(0);
        poller.poll_at(3 * MIN);
        assert_eq!(calls.load(Ordering::SeqCst), 2);

        // `Retry-After: 0` is floored, not taken literally.
        poller.poll_at(3 * MIN + 1_000);
        assert_eq!(calls.load(Ordering::SeqCst), 2, "must not hammer");

        let snapshot = poller.poll_at(6 * MIN);
        assert_eq!(calls.load(Ordering::SeqCst), 3, "next cycle retries");
        assert_eq!(snapshot.status, SnapshotStatus::Ok);
        assert_eq!(snapshot.remaining_percent(), Some(35.0));
    }

    /// Whatever keeps going wrong, both providers are asked again on every
    /// three-minute tick — the wait never swallows a whole cycle.
    #[test]
    fn every_idle_tick_polls_even_through_repeated_failures() {
        let cycle = super::super::schedule::IDLE_INTERVAL.as_millis() as i64;
        for id in [ProviderId::Claude, ProviderId::Codex] {
            let calls = Arc::new(AtomicUsize::new(0));
            let results: Vec<_> = (0..20)
                .map(|i| match i % 2 {
                    0 => Err(UsageError::Transport("down".into())),
                    _ => Err(UsageError::RateLimited { retry_after_ms: None }),
                })
                .collect();
            let poller = ProviderPoller::new(Box::new(Scripted {
                id,
                installed: true,
                results: Mutex::new(results),
                calls: calls.clone(),
            }));

            // A request stamps its wait after it returns; model a slow one.
            for tick in 0..20 {
                poller.poll_at(tick * cycle + 4_000);
                assert_eq!(calls.load(Ordering::SeqCst), tick as usize + 1, "{id:?} tick {tick}");
            }
        }
    }

    #[test]
    fn repeated_failures_back_off_instead_of_hammering() {
        let (poller, calls) = poller_from(vec![
            Err(UsageError::Transport("a".into())),
            Err(UsageError::Transport("b".into())),
            Err(UsageError::Transport("c".into())),
        ]);

        poller.poll_at(0);
        for t in 1..=9 {
            poller.poll_at(t * 1_000);
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        poller.poll_at(10_000);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        poller.poll_at(29_000);
        assert_eq!(calls.load(Ordering::SeqCst), 2, "still parked");
        poller.poll_at(30_000);
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn the_failure_reason_is_recorded_and_cleared() {
        let (poller, _) = poller_from(vec![Err(UsageError::Unauthorized), Ok(windows(90.0))]);
        poller.poll_at(0);
        assert_eq!(
            poller.last_error().as_deref(),
            Some("토큰이 만료되었습니다")
        );
        poller.poll_at(5 * MIN);
        assert_eq!(poller.last_error(), None, "success must clear it");
    }

    #[test]
    fn a_panicking_provider_does_not_kill_the_poller() {
        struct Exploding;
        impl UsageProvider for Exploding {
            fn id(&self) -> ProviderId {
                ProviderId::Claude
            }
            fn installed(&self) -> bool {
                true
            }
            fn fetch(&self) -> Result<Reading, UsageError> {
                panic!("TLS provider is on fire");
            }
        }

        let poller = ProviderPoller::new(Box::new(Exploding));
        assert_eq!(poller.poll_at(0).status, SnapshotStatus::Error);
        assert!(poller
            .last_error()
            .is_some_and(|e| e.contains("TLS provider is on fire")));
        assert_eq!(poller.poll_at(10 * MIN).status, SnapshotStatus::Error);
    }

    #[test]
    fn snapshot_does_not_fetch() {
        let (poller, calls) = poller_from(vec![Ok(windows(90.0))]);
        poller.poll_at(0);
        for _ in 0..10 {
            let _ = poller.snapshot();
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    // ---- the registry ----

    fn registry_with(claude_installed: bool, codex_installed: bool) -> UsageRegistry {
        let make = |id, installed, remaining| -> Box<dyn UsageProvider> {
            Box::new(Scripted {
                id,
                installed,
                results: Mutex::new(vec![Ok(windows(remaining))]),
                calls: Arc::new(AtomicUsize::new(0)),
            })
        };
        // Deliberately registered out of order, to prove the registry sorts.
        UsageRegistry::new(vec![
            make(ProviderId::Codex, codex_installed, 70.0),
            make(ProviderId::Claude, claude_installed, 20.0),
        ])
    }



    /// The panel needs this to say "checked a moment ago" now that there is
    /// no button to press.
    #[test]
    fn the_registry_records_when_it_last_checked() {
        let registry = registry_with(true, true);
        assert_eq!(registry.last_checked_at_ms(), None, "before the first check");

        registry.poll_all_at(5 * MIN);
        assert_eq!(registry.last_checked_at_ms(), Some(5 * MIN));
    }

    /// A check that found nothing working still happened, and saying so beats
    /// looking stuck.
    #[test]
    fn a_failed_check_still_counts_as_checked() {
        let registry = UsageRegistry::new(vec![Box::new(Scripted {
            id: ProviderId::Claude,
            installed: true,
            results: Mutex::new(vec![Err(UsageError::Transport("offline".into()))]),
            calls: Arc::new(AtomicUsize::new(0)),
        })]);
        registry.poll_all_at(9 * MIN);
        assert_eq!(registry.last_checked_at_ms(), Some(9 * MIN));
    }

    /// The monkey's hands must not swap around as providers come and go.
    #[test]
    fn snapshots_come_back_in_a_stable_order() {
        let registry = registry_with(true, true);
        let snapshots = registry.poll_all_at(0);
        assert_eq!(
            snapshots.iter().map(|s| s.provider).collect::<Vec<_>>(),
            vec![ProviderId::Claude, ProviderId::Codex]
        );
    }

    /// The spec's headline requirement: one subscription has to work fully.
    #[test]
    fn a_single_installed_provider_works_on_its_own() {
        let registry = registry_with(true, false);
        let snapshots = registry.poll_all_at(0);

        assert_eq!(snapshots.len(), 2, "both are reported to the popover");
        let installed = registry.installed();
        assert_eq!(installed.len(), 1, "only one reaches the menu bar");
        assert_eq!(installed[0].provider, ProviderId::Claude);
        assert_eq!(installed[0].remaining_percent(), Some(20.0));

        let codex = snapshots
            .iter()
            .find(|s| s.provider == ProviderId::Codex)
            .unwrap();
        assert_eq!(codex.status, SnapshotStatus::NotInstalled);
    }

    /// One provider's outage must not blank the other.
    #[test]
    fn providers_fail_independently() {
        let registry = UsageRegistry::new(vec![
            Box::new(Scripted {
                id: ProviderId::Claude,
                installed: true,
                results: Mutex::new(vec![Err(UsageError::RateLimited {
                    retry_after_ms: Some(600_000),
                })]),
                calls: Arc::new(AtomicUsize::new(0)),
            }),
            Box::new(Scripted {
                id: ProviderId::Codex,
                installed: true,
                results: Mutex::new(vec![Ok(windows(64.0))]),
                calls: Arc::new(AtomicUsize::new(0)),
            }),
        ]);

        let snapshots = registry.poll_all_at(0);
        assert_eq!(snapshots[0].status, SnapshotStatus::Error, "Claude failed");
        assert_eq!(snapshots[1].status, SnapshotStatus::Ok, "Codex is fine");
        assert_eq!(snapshots[1].remaining_percent(), Some(64.0));
    }
}
