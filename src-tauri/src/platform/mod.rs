//! Everything that only makes sense on one operating system lives under here.
//!
//! The rest of the crate calls these functions unconditionally; each platform
//! module provides its own implementation, and platforms we have not ported to
//! yet get a no-op stub. That way adding Windows later means adding a file
//! here, not hunting for `#[cfg]` attributes scattered through the app.

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::*;

#[cfg(not(target_os = "macos"))]
mod stub;
#[cfg(not(target_os = "macos"))]
pub use stub::*;
