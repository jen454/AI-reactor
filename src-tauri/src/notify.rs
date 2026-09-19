//! The one notification AI reactor ever sends.
//!
//! # Why there is only one
//!
//! A menu bar app that interrupts you is a menu bar app you quit. The cat
//! already carries the state continuously and silently; a notification is for
//! the single moment that is worth pulling attention away from whatever you
//! are doing — the limit is nearly gone and you are probably mid-task.
//!
//! The decision of *whether* to fire lives in [`crate::cat::CatMachine`],
//! which latches on the window's reset time so it happens at most once per
//! window and never on numbers we are not sure about. This module only
//! phrases it.

use tauri::{AppHandle, Runtime};
use tauri_plugin_notification::NotificationExt;

use crate::usage::{format::format_remaining, LimitWindow, ProviderId};

/// Send one notification now, for checking that they work at all.
///
/// The real one fires at 90% of a limit, which is not a state you can conjure
/// on demand — and on macOS the very first notification is also what triggers
/// the system permission prompt, so there has to be a way to provoke it
/// deliberately rather than discovering at 90% that permission was never
/// granted. `AI_REACTOR_TEST_NOTIFICATION=1`.
pub fn test_notification<R: Runtime>(app: &AppHandle<R>) {
    if std::env::var("AI_REACTOR_TEST_NOTIFICATION").is_err() {
        return;
    }
    if let Err(e) = app
        .notification()
        .builder()
        .title("AI reactor")
        .body("알림 테스트입니다. 이게 보이면 90% 경고도 동작합니다.")
        .show()
    {
        eprintln!("[ai-reactor] 테스트 알림 실패: {e}");
    } else {
        eprintln!("[ai-reactor] 테스트 알림을 보냈습니다");
    }
}

/// Tell the user the limit is nearly gone.
pub fn limit_nearly_spent<R: Runtime>(
    app: &AppHandle<R>,
    provider: ProviderId,
    window: &LimitWindow,
    now_ms: i64,
) {
    let body = format!(
        "{} {} 한도가 {:.0}%만 남았습니다 · {} 후 리셋",
        provider.label(),
        window.label(),
        window.remaining_percent,
        format_remaining(window.resets_at_ms - now_ms)
    );

    // Best-effort. macOS may not have granted notification permission, or the
    // user may have turned them off for this app — neither is a reason to log
    // an error every time, and certainly not to fail the animation loop.
    if let Err(e) = app
        .notification()
        .builder()
        .title("AI reactor")
        .body(&body)
        .show()
    {
        eprintln!("[ai-reactor] 알림을 보내지 못했습니다: {e}");
    }
}
