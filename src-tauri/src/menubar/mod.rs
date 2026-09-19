//! What the menu bar shows.
//!
//! # One icon per provider (2026-09-17)
//!
//! Each icon answers one question — *do I need to care about this provider
//! right now?* This used to be one icon answering that question about
//! whichever provider's window was tightest, merged across all of them. That
//! was a deliberate simplification when only one provider was normally in
//! use; subscribing to both Claude and Codex exposed its real cost, which is
//! that the icon can say a number is low without saying *whose*. Two
//! full-size status items side by side don't have the "too small to tell
//! apart" problem a single 22-point slot split in two would, so each
//! provider now gets its own — see `tray.rs` for the colours.
//!
//! # A live number, not a badge
//!
//! `MenubarView` carries the exact remaining percentage of a provider's
//! tightest window — this used to be a quantised six-step shape with no
//! number at all, on the theory that a continuous value was unreadable at
//! 22pt. That held for a *shape*; it does not hold now that the number is
//! also drawn as real text beside the ring (the status item's own title, in
//! `driver.rs`), so this carries the real figure instead of a band.

pub mod driver;

use std::collections::HashMap;

use crate::usage::{LimitWindow, ProviderId, ProviderSnapshot, SnapshotStatus};

/// Under this, the window counts as nearly spent and earns one notification.
const SPENT_BELOW: f64 = 10.0;

/// One provider's own icon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MenubarView {
    /// The tightest window's remaining percentage, rounded. `None` when the
    /// provider is set up but nothing is currently readable — not "0%,"
    /// which would claim a reading that does not exist.
    pub percent: Option<u8>,
}

/// One provider's tray entry.
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderView {
    pub provider: ProviderId,
    /// `None` means this provider is not set up here at all — its status
    /// item should not exist, the same "quiet invitation" the popover's
    /// cards already give an absent provider.
    pub view: Option<MenubarView>,
}

/// The result of one update.
#[derive(Debug, Clone, PartialEq)]
pub struct Tick {
    /// One entry per known provider, in a fixed order, so the tray items
    /// never swap positions as data changes.
    pub views: Vec<ProviderView>,
    /// Fire a notification for this window. At most once per window.
    pub notify: Option<(ProviderId, LimitWindow)>,
}

#[derive(Default)]
pub struct Menubar {
    /// The `resets_at` of each window we have already warned about. Keyed by
    /// window rather than by a boolean so a dip and a recovery inside one
    /// window cannot warn twice, while a genuinely new window can.
    notified_for: HashMap<(ProviderId, u32), i64>,
}

impl Menubar {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn update(&mut self, snapshots: &[ProviderSnapshot]) -> Tick {
        let views = ProviderId::ALL
            .into_iter()
            .map(|provider| {
                let snapshot = snapshots.iter().find(|s| s.provider == provider);
                let view = match snapshot {
                    Some(s) if s.status != SnapshotStatus::NotInstalled => Some(MenubarView {
                        percent: session_percent(s).map(|p| p.round().clamp(0.0, 100.0) as u8),
                    }),
                    _ => None,
                };
                ProviderView { provider, view }
            })
            .collect();

        Tick {
            views,
            notify: self.notification(snapshots),
        }
    }

    /// The window that has just run low, if any, at most once per window.
    fn notification(&mut self, snapshots: &[ProviderSnapshot]) -> Option<(ProviderId, LimitWindow)> {
        for snapshot in snapshots {
            // Never warn on numbers we are not sure about. A stale reading is
            // the last thing we saw, not the current state.
            if snapshot.status != SnapshotStatus::Ok {
                continue;
            }
            for window in &snapshot.windows {
                if window.remaining_percent >= SPENT_BELOW {
                    continue;
                }
                let key = (snapshot.provider, window.window_minutes);
                if self.notified_for.get(&key) == Some(&window.resets_at_ms) {
                    continue;
                }
                self.notified_for.insert(key, window.resets_at_ms);
                return Some((snapshot.provider, window.clone()));
            }
        }
        None
    }
}

/// The current session's remaining percentage — the provider's shortest
/// window (Claude's five hours), not whichever window happens to be lowest.
/// The menu bar answers "can I keep going right now?", and a weekly limit at
/// 40% says nothing about the next hour. The popover still shows every
/// window. A plan with only one window (a Codex plan reporting just 30 days)
/// shows that one, since it is the only session there is.
fn session_percent(snapshot: &ProviderSnapshot) -> Option<f64> {
    if !snapshot.status.has_usable_numbers() {
        return None;
    }
    snapshot
        .windows
        .iter()
        .min_by_key(|w| w.window_minutes)
        .map(|w| w.remaining_percent)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usage::ErrorReason;

    const HOUR: i64 = 3_600_000;

    fn window(window_minutes: u32, remaining: f64, resets_at_ms: i64) -> LimitWindow {
        LimitWindow {
            window_minutes,
            remaining_percent: remaining,
            resets_at_ms,
        }
    }

    fn snapshot(
        provider: ProviderId,
        windows: Vec<LimitWindow>,
        status: SnapshotStatus,
    ) -> ProviderSnapshot {
        ProviderSnapshot {
            provider,
            windows,
            captured_at_ms: 0,
            status,
            error_reason: None,
        }
    }

    fn claude(remaining: f64) -> ProviderSnapshot {
        snapshot(
            ProviderId::Claude,
            vec![window(300, remaining, 10 * HOUR)],
            SnapshotStatus::Ok,
        )
    }

    /// The percentage a provider's own status item would show. `None` means
    /// no status item at all for this provider (not set up here); `Some(None)`
    /// means the item exists but cannot currently show a number.
    fn percent_of(tick: &Tick, provider: ProviderId) -> Option<Option<u8>> {
        tick.views
            .iter()
            .find(|v| v.provider == provider)
            .map(|v| v.view.map(|mv| mv.percent))
            .unwrap_or(None)
    }

    // ---- the icon ----

    #[test]
    fn each_icon_follows_its_own_provider() {
        let mut bar = Menubar::new();
        assert_eq!(
            percent_of(&bar.update(&[claude(90.0)]), ProviderId::Claude),
            Some(Some(90))
        );
        assert_eq!(
            percent_of(&bar.update(&[claude(30.0)]), ProviderId::Claude),
            Some(Some(30))
        );
        assert_eq!(
            percent_of(&bar.update(&[claude(0.0)]), ProviderId::Claude),
            Some(Some(0))
        );
    }

    /// Two providers, two icons — neither one's number should leak into the
    /// other's.
    #[test]
    fn providers_get_independent_icons() {
        let mut bar = Menubar::new();
        let snapshots = vec![
            snapshot(
                ProviderId::Claude,
                vec![window(300, 90.0, HOUR), window(10_080, 70.0, 100 * HOUR)],
                SnapshotStatus::Ok,
            ),
            snapshot(
                ProviderId::Codex,
                vec![window(43_200, 15.0, 500 * HOUR)],
                SnapshotStatus::Ok,
            ),
        ];
        let tick = bar.update(&snapshots);
        assert_eq!(percent_of(&tick, ProviderId::Claude), Some(Some(90)));
        assert_eq!(percent_of(&tick, ProviderId::Codex), Some(Some(15)));
    }

    /// The menu bar shows the session window even when a longer one is lower:
    /// a weekly limit at 20% says nothing about whether the next hour is safe.
    #[test]
    fn the_icon_shows_the_session_not_the_lowest_window() {
        let mut bar = Menubar::new();
        let claude = snapshot(
            ProviderId::Claude,
            vec![window(10_080, 20.0, 100 * HOUR), window(300, 85.0, HOUR)],
            SnapshotStatus::Ok,
        );
        assert_eq!(
            percent_of(&bar.update(&[claude]), ProviderId::Claude),
            Some(Some(85))
        );
    }

    /// An agent the user does not have must not get a status item at all.
    #[test]
    fn an_uninstalled_provider_gets_no_icon() {
        let mut bar = Menubar::new();
        let tick = bar.update(&[
            claude(90.0),
            ProviderSnapshot::empty(ProviderId::Codex, SnapshotStatus::NotInstalled, 0),
        ]);
        assert_eq!(percent_of(&tick, ProviderId::Claude), Some(Some(90)));
        assert_eq!(percent_of(&tick, ProviderId::Codex), None);
    }

    /// A provider that failed to read this minute still keeps its icon — it
    /// just cannot show a number, which is a different situation from not
    /// being set up here at all.
    #[test]
    fn an_unreadable_provider_keeps_its_icon_but_not_a_number() {
        let mut bar = Menubar::new();
        let broken = ProviderSnapshot::empty(ProviderId::Codex, SnapshotStatus::Error, 0)
            .with_reason(ErrorReason::ReadFailed);
        let tick = bar.update(&[claude(90.0), broken]);
        assert_eq!(percent_of(&tick, ProviderId::Claude), Some(Some(90)));
        assert_eq!(percent_of(&tick, ProviderId::Codex), Some(None));
    }

    /// Stale numbers are still numbers — the gauge shows them, dimmed only in
    /// the popover. Unreadable ones are not, and fall back to the slashed dial.
    #[test]
    fn stale_numbers_still_drive_the_gauge() {
        let mut bar = Menubar::new();
        let stale = snapshot(
            ProviderId::Claude,
            vec![window(300, 20.0, 10 * HOUR)],
            SnapshotStatus::Stale,
        );
        assert_eq!(
            percent_of(&bar.update(&[stale]), ProviderId::Claude),
            Some(Some(20))
        );
    }

    // ---- the notification ----

    #[test]
    fn running_low_notifies_once_per_window() {
        let mut bar = Menubar::new();
        assert!(bar.update(&[claude(40.0)]).notify.is_none());
        assert!(bar.update(&[claude(9.0)]).notify.is_some());
        for _ in 0..20 {
            assert!(bar.update(&[claude(5.0)]).notify.is_none());
        }
    }

    /// The latch keys on the window's reset time, so a bounce back above the
    /// line and down again is still the same window.
    #[test]
    fn bouncing_back_does_not_warn_twice() {
        let mut bar = Menubar::new();
        assert!(bar.update(&[claude(9.0)]).notify.is_some());
        bar.update(&[claude(20.0)]);
        assert!(bar.update(&[claude(8.0)]).notify.is_none());
    }

    #[test]
    fn a_new_window_may_warn_again() {
        let mut bar = Menubar::new();
        assert!(bar.update(&[claude(5.0)]).notify.is_some());
        let next = snapshot(
            ProviderId::Claude,
            vec![window(300, 4.0, 99 * HOUR)],
            SnapshotStatus::Ok,
        );
        assert!(bar.update(&[next]).notify.is_some());
    }

    #[test]
    fn stale_numbers_never_warn() {
        let mut bar = Menubar::new();
        let stale = snapshot(
            ProviderId::Claude,
            vec![window(300, 1.0, 10 * HOUR)],
            SnapshotStatus::Stale,
        );
        assert!(bar.update(&[stale]).notify.is_none());
    }

    /// Launching with the limit already gone should still say something once.
    #[test]
    fn starting_low_warns() {
        let mut bar = Menubar::new();
        assert!(bar.update(&[claude(3.0)]).notify.is_some());
    }
}
