//! macOS-specific application behaviour.

use objc2::rc::autoreleasepool;
use objc2::runtime::AnyObject;
use objc2::msg_send;
use tauri::{AppHandle, Runtime, WebviewWindow};
use window_vibrancy::{apply_vibrancy, NSVisualEffectMaterial};
use zeroize::Zeroizing;

use crate::credentials::error::CredentialError;

/// Must match `.popover`'s `border-radius` in `src/styles.css`. The two are
/// drawn by different layers — this one by AppKit, the card's own edge by
/// WebKit — and nothing keeps them in sync automatically.
const POPOVER_CORNER_RADIUS: f64 = 12.0;

/// Give the popover a real macOS blur-behind instead of relying on CSS.
///
/// The card's frosted-glass look was built with the web platform's own
/// `backdrop-filter: blur()`, which asks WebKit to sample "whatever is behind
/// this view" itself, frame by frame. For an ordinary in-page element that
/// works immediately. For an entire *window* — borderless, transparent,
/// floating above everything else — it does not: WebKit has to wait for the
/// window server to hand it a live composite of the desktop behind the
/// window, and observed on this machine, that took about three seconds after
/// each show. In the meantime the card rendered with no backdrop to blur,
/// which reads as "opaque".
///
/// `NSVisualEffectView` is the native version of the same effect — the one
/// every first-party macOS popover (Notification Centre, Control Centre)
/// actually uses. It is driven by the window server directly rather than by
/// WebKit guessing at what is behind the window, so there is no equivalent
/// warm-up: the blur is correct on the very first frame the window is shown.
/// `NSVisualEffectMaterial::Popover` is Apple's own name for exactly this
/// card-dropping-from-a-status-item look.
///
/// This does not replace the CSS — `backdrop-filter` still runs on top of
/// whatever this paints and adds the light/dark tint from `--pc-bg`. It
/// replaces the thing CSS could not do on its own: making the window itself
/// non-solid from frame one.
///
/// Best-effort: a window without genuine vibrancy is a cosmetic regression
/// (the old three-second warm-up, worst case), not a reason to fail app
/// startup — so this logs rather than propagating an error through `?`.
pub fn apply_popover_vibrancy<R: Runtime>(window: &WebviewWindow<R>) {
    if let Err(e) = apply_vibrancy(window, NSVisualEffectMaterial::Popover, None, None) {
        eprintln!("[ai-reactor] 팝오버 vibrancy 적용 실패: {e}");
    }
    round_vibrancy_corners(window);
}

/// Clip the vibrancy view's corners to match the card drawn on top of it.
///
/// `apply_vibrancy` fills the whole window rectangle with an
/// `NSVisualEffectView` — square corners, because it has no idea the card
/// sitting above it is rounded. The webview itself is transparent outside
/// `.popover`'s own rounded rect, so without this, that square view peeks out
/// past the card's corners: four small opaque-looking triangles that look like
/// a rendering bug even though every individual layer is doing what it was
/// told.
///
/// `window-vibrancy`'s public API has no corner-radius knob for this crate
/// version, so this reaches into AppKit directly: round and clip the content
/// view's own layer, which sits above the vibrancy view and contains it. That
/// also fixes the window's drop shadow, which macOS derives from the actual
/// visible alpha of a transparent window — square vibrancy meant a square
/// shadow before this ran.
fn round_vibrancy_corners<R: Runtime>(window: &WebviewWindow<R>) {
    let Ok(ns_window) = window.ns_window() else {
        return;
    };
    autoreleasepool(|_| unsafe {
        let ns_window = ns_window as *mut AnyObject;
        let content_view: *mut AnyObject = msg_send![ns_window, contentView];
        if content_view.is_null() {
            return;
        }
        let _: () = msg_send![content_view, setWantsLayer: true];
        let layer: *mut AnyObject = msg_send![content_view, layer];
        if layer.is_null() {
            return;
        }
        let _: () = msg_send![layer, setCornerRadius: POPOVER_CORNER_RADIUS];
        let _: () = msg_send![layer, setMasksToBounds: true];
    });
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CgSize {
    width: f64,
    height: f64,
}

unsafe impl objc2::Encode for CgSize {
    const ENCODING: objc2::Encoding =
        objc2::Encoding::Struct("CGSize", &[f64::ENCODING, f64::ENCODING]);
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CgRect {
    origin: CgSize,
    size: CgSize,
}

unsafe impl objc2::Encode for CgRect {
    const ENCODING: objc2::Encoding = objc2::Encoding::Struct(
        "CGRect",
        &[
            objc2::Encoding::Struct("CGPoint", &[f64::ENCODING, f64::ENCODING]),
            CgSize::ENCODING,
        ],
    );
}

/// Show the tray icon at `height_pt` tall instead of `tray-icon`'s fixed 18pt.
///
/// The `tray-icon` crate hard-codes every status-item image to 18pt high, a
/// few points short of what the bar can hold. That was fine for a single
/// shape; with a logo *and* a number stacked inside the gauge, those four
/// points are most of the number's legibility. The image's own pixels are
/// 2× this size, so it lands 1:1 on a Retina display.
///
/// Must run after every `set_icon` — the crate resets the size each time.
pub fn set_tray_image_size<R: Runtime>(tray: &tauri::tray::TrayIcon<R>, width_pt: f64, height_pt: f64) {
    let result = tray.with_inner_tray_icon(move |inner| {
        let Some(item) = inner.ns_status_item() else {
            return;
        };
        let item = objc2::rc::Retained::as_ptr(&item) as *mut AnyObject;
        unsafe {
            let button: *mut AnyObject = msg_send![item, button];
            if button.is_null() {
                return;
            }
            let image: *mut AnyObject = msg_send![button, image];
            if image.is_null() {
                return;
            }
            let size = CgSize { width: width_pt, height: height_pt };
            let _: () = msg_send![image, setSize: size];
            let _: () = msg_send![button, setNeedsDisplay: true];
            if std::env::var_os("AI_REACTOR_DEBUG_TRAY").is_some() {
                let frame: CgRect = msg_send![button, bounds];
                let scaling: usize = msg_send![button, imageScaling];
                eprintln!(
                    "[ai-reactor] tray button {}x{}pt, image {}x{}pt, imageScaling={}",
                    frame.size.width, frame.size.height, width_pt, height_pt, scaling
                );
            }
        }
    });
    if let Err(e) = result {
        eprintln!("[ai-reactor] 트레이 아이콘 크기 조정 실패: {e}");
    }
}

/// Turn the process into a menu bar ("accessory") app.
///
/// macOS decides whether an app gets a Dock tile and a menu bar from its
/// *activation policy*. The default is `Regular`. `Accessory` means: no Dock
/// icon, no app menu, no entry in the Cmd-Tab switcher — the app exists only
/// as its status item. This is the same thing an Info.plist `LSUIElement`
/// key does, but setting it in code keeps it next to the tray setup.
///
/// Returns `Result` because Tauri's setup hook is fallible; we let a failure
/// bubble up rather than silently shipping an app with a stray Dock icon.
pub fn hide_from_dock<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<()> {
    app.set_activation_policy(tauri::ActivationPolicy::Accessory)
}

/// The keychain item the `claude` CLI writes after an OAuth login.
const SERVICE: &str = "Claude Code-credentials";

// OSStatus values from <Security/SecBase.h>. The crate hands back the raw
// code, and these are the only ones whose meaning changes what we show the
// user, so we name them rather than matching bare integers.
const ERR_SEC_USER_CANCELED: i32 = -128;
const ERR_SEC_AUTH_FAILED: i32 = -25293;
const ERR_SEC_ITEM_NOT_FOUND: i32 = -25300;
const ERR_SEC_INTERACTION_NOT_ALLOWED: i32 = -25308;
/// The keychain needs to ask the user something, but the machine is in a
/// low-power wake with the display off and no UI can be shown. Seen in the
/// wild: the app polls on its timer while the lid is shut, the ACL prompt
/// cannot be drawn, and this comes back.
const ERR_SEC_IN_DARK_WAKE: i32 = -25320;
/// Same shape of problem — interaction is required and not currently possible.
const ERR_SEC_INTERACTION_REQUIRED: i32 = -25315;

/// Read the stored credential blob out of the login keychain.
///
/// # Why search by service instead of `get_generic_password`
///
/// The convenience helper wants a service *and* an account. Inspecting the
/// real item shows the account is the macOS username, which
/// means hardcoding it breaks on every other machine, and deriving it means
/// guessing at whatever the CLI used. Searching on the service alone sidesteps
/// the question — that attribute is a fixed string the CLI controls.
///
/// # The prompt
///
/// The item's ACL lists the `claude` CLI, not us, so the first read shows the
/// system "AI reactor wants to use your confidential information" dialog. That is
/// expected and unavoidable for a second reader. What matters is that a
/// *denial* is returned as `Denied` and the caller stops asking — see
/// `CredentialStore`, which latches on that and never polls the keychain
/// again until the user explicitly retries.
pub fn keychain_read() -> Result<Zeroizing<Vec<u8>>, CredentialError> {
    use security_framework::item::{ItemClass, ItemSearchOptions, Limit, SearchResult};

    let results = ItemSearchOptions::new()
        .class(ItemClass::generic_password())
        .service(SERVICE)
        // Without `load_data` the search returns only attributes — which, note,
        // does *not* trigger the ACL prompt. Asking for the data is what makes
        // macOS check whether we are allowed to see the secret.
        .load_data(true)
        .limit(Limit::Max(1))
        .search()
        .map_err(map_os_error)?;

    for result in results {
        if let SearchResult::Data(bytes) = result {
            return Ok(Zeroizing::new(bytes));
        }
    }

    // A search that matched nothing comes back as `errSecItemNotFound` above;
    // reaching here means it matched but returned no data payload.
    Err(CredentialError::NotFound)
}

/// Map Security.framework's `OSStatus` onto the distinctions the UI cares about.
fn map_os_error(err: security_framework::base::Error) -> CredentialError {
    match err.code() {
        ERR_SEC_ITEM_NOT_FOUND => CredentialError::NotFound,
        // "Cancelled" is the Deny button; "auth failed" is the same story from
        // the other direction (wrong password on an unlock sheet). Both mean:
        // the user did not let us in, so do not ask again on a timer.
        ERR_SEC_USER_CANCELED | ERR_SEC_AUTH_FAILED => CredentialError::Denied,
        // The keychain cannot ask the user anything right now — locked, or the
        // machine is awake only enough to run timers. Unlike a denial these
        // clear on their own once someone is actually at the machine, so they
        // must not latch the way a refusal does.
        ERR_SEC_INTERACTION_NOT_ALLOWED
        | ERR_SEC_IN_DARK_WAKE
        | ERR_SEC_INTERACTION_REQUIRED => CredentialError::Locked,
        code => CredentialError::Backend(format!("OSStatus {code}")),
    }
}
