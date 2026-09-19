//! The menu bar status item.
//!
//! This module only builds the status item and handles clicks. What it
//! *shows* is driven by [`crate::menubar::driver`], which owns the polling
//! loop and the rule that the icon is never redrawn unless its picture
//! changed.
//!
//! # Back to one status item, both providers stitched into it (2026-09-17)
//!
//! For part of this same day each provider had its own status item, so two
//! subscriptions did not have to share one icon. Asked to undo that: two
//! separate items behaved like two separate apps living in the bar — each
//! highlighted independently on click, each had its own gap around it — and
//! that read as more separation than was wanted. "The two need to be one
//! bundle" won out over "each provider gets full-size real estate." So this
//! is one `TrayIconBuilder` again; `menubar/driver.rs` renders each visible
//! provider's ring on its own (`art::render`) and stitches the bitmaps
//! side by side before handing the combined image to this one icon.

use tauri::{
    image::Image,
    menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Runtime,
};
use tauri_plugin_autostart::ManagerExt;

use crate::art;
use crate::popover;
use crate::usage::ProviderId;

/// Stable id so the driver can fetch the tray back out of the app handle
/// (`app.tray_by_id(tray::ID)`) to swap its icon.
pub const ID: &str = "ai-reactor";

/// Menu item ids. Strings because that is what the menu event carries.
const ID_AUTOSTART: &str = "autostart";
const ID_QUIT: &str = "quit";

/// Which logo goes in this provider's ring. The mark itself is rasterised in
/// `art.rs` from the same path data as the popover's SVGs; this is only the
/// lookup from provider to mark.
pub(crate) fn mark_for(provider: ProviderId) -> art::Mark {
    match provider {
        ProviderId::Claude => art::Mark::Claude,
        ProviderId::Codex => art::Mark::Codex,
    }
}

/// Build the status item and wire up its click handling.
pub fn create<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<()> {
    // Reading the current setting rather than assuming: the launch agent
    // outlives the app, so a reinstall must not silently show it as off while
    // the plist is still there.
    let autostart_on = app.autolaunch().is_enabled().unwrap_or(false);

    // `with_id` gives the string we match on in the handler below. `true` =
    // enabled, `None` = no keyboard accelerator (a menu bar extra has no key
    // equivalents to speak of).
    let autostart = CheckMenuItem::with_id(
        app,
        ID_AUTOSTART,
        "로그인 시 자동 실행",
        true,
        autostart_on,
        None::<&str>,
    )?;
    let quit = MenuItem::with_id(app, ID_QUIT, "AI reactor 종료", true, None::<&str>)?;
    let menu = Menu::with_items(
        app,
        &[&autostart, &PredefinedMenuItem::separator(app)?, &quit],
    )?;

    // Start on the "no reading" icon, Claude only — at launch nothing has
    // been read yet, so we do not yet know whether Codex is even set up
    // here. `menubar/driver.rs`'s first tick corrects this within a second.
    let (pixels, width, height) = art::render(None, mark_for(ProviderId::Claude));

    TrayIconBuilder::with_id(ID)
        .icon(Image::new(&pixels, width, height))
        .icon_as_template(true)
        .menu(&menu)
        // macOS convention: left click belongs to the app (open the popover),
        // right click opens the menu. Tauri's default is to show the menu on
        // *either* button, so we have to opt out explicitly.
        .show_menu_on_left_click(false)
        .on_menu_event(move |app, event| {
            match event.id().as_ref() {
                ID_QUIT => app.exit(0),
                ID_AUTOSTART => {
                    // The check mark has already flipped visually by the time
                    // this fires, so read it and make reality match — rather
                    // than tracking our own copy of the state and risking the
                    // two drifting apart.
                    let wanted = autostart.is_checked().unwrap_or(false);
                    let result = if wanted {
                        app.autolaunch().enable()
                    } else {
                        app.autolaunch().disable()
                    };
                    if let Err(e) = result {
                        eprintln!("[ai-reactor] 자동 실행 설정 실패: {e}");
                        // Put the tick back where it was, so the menu never
                        // claims a setting that did not take.
                        let _ = autostart.set_checked(!wanted);
                    }
                }
                _ => {}
            }
        })
        .on_tray_icon_event(|tray, event| {
            let app = tray.app_handle();

            // Hand every event to the positioner plugin first. It records the
            // icon's screen rectangle, which is the only way to know where to
            // put the popover — the click coordinates alone are not enough.
            tauri_plugin_positioner::on_tray_event(app, &event);

            // Toggle on button *release*, not press: matching the OS feel, and
            // it avoids firing twice for a single click.
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                if let Err(e) = popover::toggle(app) {
                    eprintln!("[ai-reactor] failed to toggle popover: {e}");
                }
            }
        })
        .build(app)?;

    Ok(())
}
