//! Fallbacks for platforms AI reactor has not been ported to yet.
//!
//! Porting to Windows means writing a sibling `windows.rs` that implements
//! these same functions — `keychain_read` against the Credential Manager
//! (`CredReadW`), and a `apply_popover_vibrancy` using Windows' own
//! acrylic/mica APIs via the `window-vibrancy` crate, which already supports
//! both platforms — and adding it to `platform/mod.rs`. Nothing outside this
//! directory has to change.

use tauri::{AppHandle, Runtime, WebviewWindow};
use zeroize::Zeroizing;

use crate::credentials::error::CredentialError;

/// No-op: only macOS has an activation policy to change.
pub fn hide_from_dock<R: Runtime>(_app: &AppHandle<R>) -> tauri::Result<()> {
    Ok(())
}

/// No-op: the 18pt cap this works around is a macOS-only detail of `tray-icon`.
pub fn set_tray_image_size<R: Runtime>(_tray: &tauri::tray::TrayIcon<R>, _width_pt: f64, _height_pt: f64) {}

/// No-op until a Windows-specific vibrancy call is wired up here.
pub fn apply_popover_vibrancy<R: Runtime>(_window: &WebviewWindow<R>) {}

/// No OS credential store wired up yet; the caller falls back to the file.
pub fn keychain_read() -> Result<Zeroizing<Vec<u8>>, CredentialError> {
    Err(CredentialError::NotFound)
}
