//! Reading the `claude` CLI's OAuth credential — and nothing else.
//!
//! # The one rule
//!
//! AI reactor **never refreshes the token**. The refresh token rotates on use:
//! spending it mints a new one and invalidates the old, so if this app
//! refreshed with its copy, the CLI's copy would go dead and the user would be
//! logged out the next time they ran `claude`. The CLI owns these credentials;
//! we read them.
//!
//! When the access token expires we report [`CredentialState::Expired`] and do
//! nothing. The user runs `claude` at some point, the CLI refreshes, and our
//! next poll picks up the new value on its own.
//!
//! See [`parse`] for how that rule is enforced structurally rather than by
//! good intentions.

pub mod error;
pub mod file;
pub mod parse;
pub mod store;
pub mod token;

use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

use token::AccessToken;

/// A credential we successfully read and parsed.
///
/// No `Serialize`, no derived `Debug` — see [`AccessToken`] for why.
pub struct Credentials {
    pub(crate) token: AccessToken,
    /// Milliseconds since the Unix epoch, straight from the stored blob.
    pub(crate) expires_at_ms: i64,
}

impl Credentials {
    /// The only way to reach the token. Milestone 3's request builder is the
    /// intended — and ideally only — caller.
    // Unused until milestone 3 makes the first network call.
    #[allow(dead_code)]
    pub fn access_token(&self) -> &AccessToken {
        &self.token
    }

    pub fn expires_at_ms(&self) -> i64 {
        self.expires_at_ms
    }

    /// Whether this credential is usable *right now*, allowing for
    /// [`CLOCK_SKEW_MS`].
    pub fn is_usable_at(&self, now_ms: i64) -> bool {
        self.expires_at_ms - CLOCK_SKEW_MS > now_ms
    }
}

impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Credentials")
            .field("token", &self.token)
            .field("expires_at_ms", &self.expires_at_ms)
            .finish()
    }
}

/// Treat a token as expired this long before it actually is.
///
/// Without a margin we could check the clock, decide the token is good with
/// two seconds to spare, and have it expire in flight — turning a predictable
/// `Expired` into a confusing 401. A minute is comfortably more than a request
/// takes and far less than the token's lifetime.
pub const CLOCK_SKEW_MS: i64 = 60_000;

/// What the frontend is told. Note what is *not* here: the token.
///
/// This enum is the entire credential-related API surface of the webview. The
/// token cannot travel over IPC because [`AccessToken`] does not implement
/// `Serialize`, so this is enforced by the compiler, not by review.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum CredentialState {
    /// Usable. The cat can be awake.
    Ok { expires_at_ms: i64 },
    /// Present but past its lifetime. Recovers when the user runs `claude`.
    Expired { expired_at_ms: i64 },
    /// Nothing stored at all — the user has never logged in.
    Missing,
    /// The user declined the keychain prompt. We stop asking until they retry.
    Denied,
    /// Something else went wrong. `reason` describes shape, never secrets.
    Unreadable { reason: String },
}

impl CredentialState {
    /// Classify a freshly-read credential against the clock.
    pub fn from_credentials(cred: &Credentials, now_ms: i64) -> Self {
        if cred.is_usable_at(now_ms) {
            Self::Ok {
                expires_at_ms: cred.expires_at_ms,
            }
        } else {
            Self::Expired {
                expired_at_ms: cred.expires_at_ms,
            }
        }
    }
}

impl From<error::CredentialError> for CredentialState {
    fn from(err: error::CredentialError) -> Self {
        use error::CredentialError as E;
        match err {
            E::NotFound => Self::Missing,
            E::Denied => Self::Denied,
            other => Self::Unreadable {
                reason: other.to_string(),
            },
        }
    }
}

/// Wall clock in milliseconds since the Unix epoch.
///
/// Saturates rather than panicking if the system clock is set before 1970 —
/// a nonsense clock should make the cat sleep, not crash the app.
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cred(expires_at_ms: i64) -> Credentials {
        Credentials {
            token: AccessToken::new("EXAMPLE".into()),
            expires_at_ms,
        }
    }

    #[test]
    fn comfortably_valid_token_is_ok() {
        let now = 1_000_000;
        let state = CredentialState::from_credentials(&cred(now + 3_600_000), now);
        assert_eq!(
            state,
            CredentialState::Ok {
                expires_at_ms: now + 3_600_000
            }
        );
    }

    #[test]
    fn already_past_expiry_is_expired() {
        let now = 1_000_000;
        let state = CredentialState::from_credentials(&cred(now - 1), now);
        assert!(matches!(state, CredentialState::Expired { .. }));
    }

    /// The skew margin: a token with 30s left is technically still valid but we
    /// refuse it, because a request started now might outlive it.
    #[test]
    fn token_inside_the_skew_margin_counts_as_expired() {
        let now = 1_000_000;
        assert!(matches!(
            CredentialState::from_credentials(&cred(now + 30_000), now),
            CredentialState::Expired { .. }
        ));
        // Just outside the margin, it is fine.
        assert!(matches!(
            CredentialState::from_credentials(&cred(now + 61_000), now),
            CredentialState::Ok { .. }
        ));
    }

    #[test]
    fn errors_map_to_distinct_states() {
        use error::CredentialError as E;
        assert_eq!(CredentialState::from(E::NotFound), CredentialState::Missing);
        assert_eq!(CredentialState::from(E::Denied), CredentialState::Denied);
        assert!(matches!(
            CredentialState::from(E::Locked),
            CredentialState::Unreadable { .. }
        ));
    }

    /// Whatever we send to the webview must not contain the token.
    #[test]
    fn serialized_state_carries_no_secret() {
        let json = serde_json::to_string(&CredentialState::Ok {
            expires_at_ms: 1789000000000,
        })
        .unwrap();
        assert_eq!(json, r#"{"kind":"ok","expiresAtMs":1789000000000}"#);
    }

    #[test]
    fn debug_of_credentials_redacts_the_token() {
        let debug = format!("{:?}", cred(42));
        assert!(!debug.contains("EXAMPLE"), "token leaked: {debug}");
        assert!(debug.contains("redacted"), "{debug}");
    }
}
