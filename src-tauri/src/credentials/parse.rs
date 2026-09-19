//! Turning the stored JSON blob into `Credentials`.
//!
//! # The refresh token
//!
//! The blob the CLI stores contains a `refreshToken`. **We do not have a field
//! for it.** serde drops unknown keys silently, so the refresh token stops at
//! this boundary and never exists anywhere else in the program.
//!
//! That is not a stylistic choice, it is the safety mechanism. Rotating the
//! refresh token would invalidate the copy the `claude` CLI holds and log the
//! user out. By never parsing it, building such a request is not something a
//! future edit can do by accident — it would not compile, because there is no
//! value to send.

use serde::Deserialize;

use super::{error::CredentialError, token::AccessToken, Credentials};

/// The shape the CLI writes today: everything nested under one key.
#[derive(Deserialize)]
struct Wrapper {
    #[serde(rename = "claudeAiOauth")]
    oauth: Oauth,
}

/// Exactly the two fields we need, and nothing else.
#[derive(Deserialize)]
struct Oauth {
    #[serde(rename = "accessToken")]
    access_token: String,
    /// Milliseconds since the Unix epoch.
    #[serde(rename = "expiresAt")]
    expires_at_ms: i64,
}

/// Parse the credential blob.
///
/// This endpoint's storage format is not documented and may change, so we try
/// the nested shape first and fall back to a flat one, and on total failure we
/// report *which keys were present* — never their values — so a future
/// breakage is diagnosable from a bug report without anyone pasting a token.
pub fn parse(bytes: &[u8]) -> Result<Credentials, CredentialError> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| CredentialError::Malformed("UTF-8이 아님".into()))?;

    if let Ok(w) = serde_json::from_str::<Wrapper>(text) {
        return Ok(Credentials {
            token: AccessToken::new(w.oauth.access_token),
            expires_at_ms: w.oauth.expires_at_ms,
        });
    }

    // Older/other layouts may store the same two fields at the top level.
    if let Ok(o) = serde_json::from_str::<Oauth>(text) {
        return Ok(Credentials {
            token: AccessToken::new(o.access_token),
            expires_at_ms: o.expires_at_ms,
        });
    }

    Err(CredentialError::Malformed(format!(
        "예상치 못한 구조 {}",
        crate::json_shape::describe_text(text)
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape we expect in the wild, refresh token included.
    const REAL_SHAPE: &str = r#"{
      "claudeAiOauth": {
        "accessToken": "sk-ant-oat01-EXAMPLE",
        "refreshToken": "sk-ant-ort01-EXAMPLE",
        "expiresAt": 1789000000000,
        "scopes": ["user:inference", "user:profile"],
        "subscriptionType": "max"
      }
    }"#;

    #[test]
    fn parses_the_nested_shape() {
        let c = parse(REAL_SHAPE.as_bytes()).expect("should parse");
        assert_eq!(c.token.expose(), "sk-ant-oat01-EXAMPLE");
        assert_eq!(c.expires_at_ms, 1789000000000);
    }

    /// The important one. There is no API to reach the refresh token, so this
    /// test asserts the *observable* consequence: parsing succeeds and the
    /// resulting value carries only the access token. If someone ever adds a
    /// `refresh_token` field to `Credentials`, this test's name is the comment
    /// explaining why they should not.
    #[test]
    fn refresh_token_is_discarded_at_the_parse_boundary() {
        let c = parse(REAL_SHAPE.as_bytes()).expect("should parse");
        let debug = format!("{c:?}");
        assert!(!debug.contains("ort01"), "refresh token leaked into Debug");
        assert!(!debug.contains("oat01"), "access token leaked into Debug");
    }

    #[test]
    fn parses_a_flat_shape_too() {
        let flat = r#"{"accessToken": "abc", "expiresAt": 1}"#;
        let c = parse(flat.as_bytes()).expect("should parse");
        assert_eq!(c.token.expose(), "abc");
    }

    #[test]
    fn unknown_shape_reports_keys_but_never_values() {
        let odd = r#"{"somethingElse": {"token": "SUPERSECRETVALUE"}}"#;
        let err = parse(odd.as_bytes()).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("somethingElse"), "should name the key: {msg}");
        assert!(msg.contains("string(len=16)"), "should report type: {msg}");
        assert!(!msg.contains("SUPERSECRET"), "value leaked: {msg}");
    }

    #[test]
    fn garbage_is_malformed_not_a_panic() {
        assert!(matches!(
            parse(b"not json at all"),
            Err(CredentialError::Malformed(_))
        ));
        assert!(matches!(
            parse(&[0xff, 0xfe]),
            Err(CredentialError::Malformed(_))
        ));
    }
}
