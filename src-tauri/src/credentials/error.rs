//! How reading the credential can fail.
//!
//! These variants exist so the popover can say something useful. "Not logged
//! in" and "you clicked Deny on the keychain prompt" both mean *no usage
//! data*, but the user has to do completely different things about them, so we
//! keep them apart all the way to the UI instead of collapsing them into one
//! "error" early.

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialError {
    /// No credential anywhere: the user has never run `claude`, or logged out.
    NotFound,
    /// macOS asked, the user said no. We must NOT keep asking.
    Denied,
    /// Keychain is locked and no UI is allowed to prompt right now.
    Locked,
    /// The store answered, but with something we could not parse.
    /// The string names keys/shape only — never any value.
    Malformed(String),
    /// Anything else the OS reported.
    Backend(String),
}

impl fmt::Display for CredentialError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound => write!(f, "자격증명을 찾을 수 없습니다"),
            Self::Denied => write!(f, "키체인 접근이 거부되었습니다"),
            Self::Locked => write!(f, "키체인이 잠겨 있습니다"),
            Self::Malformed(what) => write!(f, "자격증명 형식을 해석할 수 없습니다: {what}"),
            Self::Backend(what) => write!(f, "자격증명 저장소 오류: {what}"),
        }
    }
}

impl std::error::Error for CredentialError {}
