//! The access token, wrapped so it cannot leak by accident.

use std::fmt;
use zeroize::Zeroizing;

/// An OAuth access token.
///
/// Three things about this type are deliberate, and all three exist to stop a
/// one-line mistake from putting a live credential somewhere permanent:
///
/// 1. **No `#[derive(Debug)]`.** The derived impl would print the token
///    verbatim, so a stray `eprintln!("{cred:?}")` — the most natural thing in
///    the world to type while debugging — would dump it to the terminal. The
///    hand-written impl below prints a redaction instead.
/// 2. **No `Serialize`.** Without it, the token physically cannot be returned
///    from a `#[tauri::command]`; it would not compile. That is what keeps it
///    out of the webview.
/// 3. **`Zeroizing`** overwrites the bytes with zeroes when the value drops,
///    rather than leaving them in freed heap memory for whatever reads it next.
pub struct AccessToken(Zeroizing<String>);

impl AccessToken {
    pub fn new(raw: String) -> Self {
        Self(Zeroizing::new(raw))
    }

    /// Hand out the raw token. Named `expose` rather than `as_str` so that a
    /// reader of the call site has to notice what is happening — milestone 3's
    /// HTTP request is the only place this should ever be called.
    #[allow(dead_code)] // milestone 3 is the first caller
    pub fn expose(&self) -> &str {
        &self.0
    }

    #[allow(dead_code)] // used by tests and by the milestone 3 request builder
    pub fn len(&self) -> usize {
        self.0.len()
    }
}

impl fmt::Debug for AccessToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "AccessToken(<redacted, {} bytes>)", self.0.len())
    }
}
