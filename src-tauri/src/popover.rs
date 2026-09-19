//! The panel that drops down from the menu bar icon.
//!
//! It is an ordinary Tauri window declared in `tauri.conf.json` with the
//! chrome switched off (no title bar, not resizable, never in the task
//! switcher) and `visible: false`, so it exists from launch but stays hidden
//! until the tray icon is clicked. Creating it once up front and only
//! toggling visibility is deliberate: building a webview on every click would
//! cost ~100ms and a burst of CPU, which is exactly what this app promises
//! not to do.

use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use tauri::{AppHandle, Emitter, Manager, Runtime, WebviewWindow};

use crate::usage::schedule::PollSchedule;

/// The `label` given to the window in `tauri.conf.json`.
pub const LABEL: &str = "popover";

/// Emitted with `true` when the popover appears and `false` when it goes away.
///
/// The webview needs this because a hidden Tauri window is still a *live*
/// webview — React stays mounted, timers keep firing. Without an explicit
/// signal the popover would poll for usage data while nobody is looking at it,
/// and on a cold launch that first poll is what triggers the keychain
/// permission dialog, seemingly out of nowhere. Gating on this event means the
/// first read happens because the user opened the panel.
pub const VISIBILITY_EVENT: &str = "popover-visibility";

fn announce_visibility<R: Runtime>(window: &WebviewWindow<R>, visible: bool) {
    // Best-effort: if the webview is gone there is nobody to tell, and that is
    // not a reason to fail the show/hide itself.
    let _ = window.emit(VISIBILITY_EVENT, visible);

    // The poller cares too — an open panel is read at 10s, a closed one at 60.
    // `try_state` rather than `state`: during teardown the managed value may
    // already be gone, and a hide on the way out should not panic.
    if let Some(schedule) = window.app_handle().try_state::<Arc<PollSchedule>>() {
        schedule.set_popover_open(visible);
        eprintln!(
            "[ai-reactor] 팝오버 {} — 폴링 주기 {}초",
            if visible { "열림" } else { "닫힘" },
            if visible {
                crate::usage::schedule::ACTIVE_INTERVAL.as_secs()
            } else {
                crate::usage::schedule::IDLE_INTERVAL.as_secs()
            }
        );
    }
}

/// Look the popover window up by label.
///
/// `Option` rather than a panic: during shutdown the window may already be
/// gone, and a tray click arriving at that moment should do nothing rather
/// than crash the app.
pub fn get<R: Runtime>(app: &AppHandle<R>) -> Option<WebviewWindow<R>> {
    app.get_webview_window(LABEL)
}

/// When the popover was last hidden.
///
/// Needed to close the panel by clicking the tray icon again — see
/// [`toggle`]. Process-global because there is exactly one popover, and
/// threading it through the tray callback would buy nothing.
fn last_hidden_at() -> &'static Mutex<Option<Instant>> {
    static LAST_HIDDEN: OnceLock<Mutex<Option<Instant>>> = OnceLock::new();
    LAST_HIDDEN.get_or_init(|| Mutex::new(None))
}

/// How recently a hide counts as "caused by the click we are handling".
///
/// Long enough to cover the gap between macOS delivering the focus-lost event
/// and the tray click, short enough that a deliberate second click — a person
/// reopening the panel — is never swallowed.
const CLICK_DEBOUNCE: Duration = Duration::from_millis(300);

/// Show the popover anchored under the tray icon, and focus it.
pub fn show<R: Runtime>(window: &WebviewWindow<R>) -> tauri::Result<()> {
    // `TrayCenter` uses the icon rectangle that the positioner plugin recorded
    // from the last tray event, so the panel lines up with the icon that was
    // actually clicked — correct on multi-monitor setups too.
    if window.is_visible()? {
        return Ok(());
    }

    use tauri_plugin_positioner::{Position, WindowExt};
    let _ = window.move_window(Position::TrayBottomCenter);

    window.show()?;
    window.set_focus()?;
    announce_visibility(window, true);
    Ok(())
}

/// Whether `hide` ran within [`CLICK_DEBOUNCE`] of now.
fn hidden_just_now() -> bool {
    last_hidden_at()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .is_some_and(|at| at.elapsed() < CLICK_DEBOUNCE)
}

/// Hide the popover. Cheap — the webview stays alive in the background, which
/// is exactly why we announce the change: it has to stop polling on its own.
///
/// Idempotent. macOS can deliver more than one reason to hide a panel for a
/// single click (a focus change and a close request), and without this guard
/// each one would re-announce the same transition — waking the poller and the
/// webview twice for nothing.
pub fn hide<R: Runtime>(window: &WebviewWindow<R>) -> tauri::Result<()> {
    if !window.is_visible()? {
        return Ok(());
    }
    window.hide()?;
    *last_hidden_at().lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now());
    announce_visibility(window, false);
    Ok(())
}

/// Show if hidden, hide if shown. Bound to a left click on the tray icon.
///
/// # The reopen race
///
/// Clicking the tray icon while the panel is open produces two events: the
/// panel loses focus (so the focus handler hides it) and *then* the click
/// arrives here. By that point the window is already hidden, so a naive
/// toggle shows it again — and the icon appears unable to close the panel.
///
/// So a click that lands right after a hide is read as the click that caused
/// it, and does nothing. The panel is already closed; that is what the user
/// asked for.
pub fn toggle<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<()> {
    let Some(window) = get(app) else {
        return Ok(());
    };
    if window.is_visible()? {
        hide(&window)
    } else if hidden_just_now() {
        Ok(())
    } else {
        show(&window)
    }
}
