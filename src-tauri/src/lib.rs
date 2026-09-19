//! AI reactor — Claude Code usage limits, performed by a cat in the menu bar.

mod art;
mod commands;
mod credentials;
mod json_shape;
mod menubar;
mod notify;
mod platform;
mod popover;
mod tray;
mod usage;

use std::sync::Arc;

use tauri::{Manager, WindowEvent};

use credentials::store::CredentialStore;
use usage::{
    anthropic::AnthropicUsageProvider,
    codex::CodexUsageProvider,
    format::format_snapshots,
    poller::{self, UsageRegistry},
    schedule::PollSchedule,
    UsageProvider,
};

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_positioner::init())
        .plugin(tauri_plugin_notification::init())
        // `LaunchAgent` rather than the login-items API: a launch agent is a
        // plist in the user's own Library that survives app moves and can be
        // inspected and removed by hand, which suits a tool people install
        // outside the App Store.
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .setup(|app| {
            let handle = app.handle();

            // Do this before anything draws, so the Dock icon never flashes.
            platform::hide_from_dock(handle)?;

            // Give the popover real macOS vibrancy rather than leaning on CSS
            // `backdrop-filter` alone — see `platform::apply_popover_vibrancy`
            // for why the CSS-only version had a multi-second warm-up on every
            // open. The window exists from here on (declared in
            // `tauri.conf.json`, created before `setup` runs) even though it
            // stays hidden until the tray icon is clicked.
            if let Some(popover) = popover::get(handle) {
                platform::apply_popover_vibrancy(&popover);
            }

            // One credential store for the whole app. It must be a single
            // instance: the cache and the denial latch only work if every
            // caller shares them. `Arc` because the poller thread holds it too.
            let credentials = Arc::new(CredentialStore::new());
            app.manage(credentials.clone());

            // Providers are the swappable seam. Everything above works against
            // the `UsageProvider` trait, so each one's very different data path
            // — an undocumented HTTP endpoint here, a local log there — stays
            // behind it, and a third provider is a new file rather than a
            // rewrite.
            //
            // Both are registered unconditionally. An adapter reports its own
            // absence — Codex's `installed()` is a directory check — and a
            // provider the user does not have still earns a quiet card in the
            // popover inviting them to. Registering only what is present would
            // make that card impossible to draw.
            let providers: Vec<Box<dyn UsageProvider>> = if usage::demo::enabled() {
                // `AI_REACTOR_DEMO=1` walks the panel through every state on a
                // timer. Four of them cannot be produced on demand — you
                // cannot uninstall Claude to look at the empty card — and the
                // alternative is shipping screens nobody has seen.
                eprintln!("[ai-reactor] 데모 모드: 실제 데이터를 읽지 않습니다");
                usage::demo::DemoProvider::pair()
            } else {
                vec![
                    Box::new(AnthropicUsageProvider::new(credentials)),
                    Box::new(CodexUsageProvider::new()),
                ]
            };
            let usage = Arc::new(if usage::demo::enabled() {
                UsageRegistry::unthrottled(providers)
            } else {
                UsageRegistry::new(providers)
            });
            app.manage(usage.clone());

            // Owned by the app so `popover::show`/`hide` can reach it, and by
            // the poller thread so it can wait on it.
            let schedule = Arc::new(if usage::demo::enabled() {
                PollSchedule::new(usage::demo::POLL, usage::demo::POLL)
            } else {
                PollSchedule::default()
            });
            app.manage(schedule.clone());

            tray::create(handle)?;
            notify::test_notification(handle);

            // The menu bar reads the same snapshots the popover does, so the
            // icon and the panel can never disagree about what is happening.
            menubar::driver::spawn(handle.clone(), usage.clone());

            // Milestone 3's deliverable: watch the data layer work.
            //
            // Note this poll runs whether or not the popover is open — unlike
            // the webview's poll, which is gated on visibility. The difference
            // is that something actually needs the data: from milestone 5 the
            // tray icon has to stay current with the panel closed. The first
            // cycle reads the credential, so on a machine that has never
            // granted AI reactor keychain access this is where the permission
            // dialog appears.
            // Say what we are about to do, so the console output below has a
            // header and a silent app is distinguishable from a stuck one.
            eprintln!(
                "[ai-reactor] 시작 — {} 폴링 (팝오버 닫힘 {}초 / 열림 {}초)",
                usage::anthropic::USAGE_URL,
                usage::schedule::IDLE_INTERVAL.as_secs(),
                usage::schedule::ACTIVE_INTERVAL.as_secs()
            );

            poller::spawn(
                usage,
                schedule,
                poller::deduplicating_logger(move |snapshots, errors| {
                    format_snapshots(snapshots, errors, credentials::now_ms())
                }),
            );

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::credential_state,
            commands::recheck_credentials,
            commands::usage_report,
            commands::refresh_usage,
            commands::account_info
        ])
        .on_window_event(|window, event| {
            // A real macOS popover closes as soon as you click elsewhere.
            // Tauri gives us focus changes, so "lost focus" is our cue.
            if window.label() == popover::LABEL {
                if let WindowEvent::Focused(false) = event {
                    // Route through `popover::hide` rather than calling
                    // `window.hide()` here, so the webview is told to stop
                    // polling. Hiding it behind our back would leave the
                    // timers running.
                    if let Some(popover) = popover::get(window.app_handle()) {
                        let _ = popover::hide(&popover);
                    }
                }
                if let WindowEvent::CloseRequested { api, .. } = event {
                    api.prevent_close();
                    if let Some(popover) = popover::get(window.app_handle()) {
                        let _ = popover::hide(&popover);
                    }
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
