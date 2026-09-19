//! The adapter for Anthropic's OAuth usage endpoint.
//!
//! # This file is the blast radius
//!
//! This endpoint is what the `claude` CLI reads to draw its `/usage` screen.
//! It is **not a documented public API** and can change or vanish without
//! notice. Everything that knows its URL, its field names, its units and its
//! status codes is in this one file, behind [`UsageProvider`]. When it breaks,
//! it breaks here, and the poller turns that into a `stale` snapshot.
//!
//! # What this file must never do
//!
//! Refresh the token. The CLI's own client retries a 401 by refreshing, which
//! is correct for the CLI — it owns the credential. We hold a *copy*, and
//! refreshing rotates the refresh token and logs the CLI out. So [`USAGE_URL`]
//! is the only URL this program ever requests, a 401 becomes
//! [`UsageError::Unauthorized`], and there is no code path from here to a
//! token endpoint.

use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;

use super::{LimitWindow, ProviderId, Reading, UsageError, UsageProvider};
use crate::credentials::store::CredentialStore;

/// The only URL AI reactor ever requests.
pub const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";

/// Matches what the CLI uses. Long enough for a slow network, short enough
/// that a hung connection cannot stall a poll cycle.
const TIMEOUT: Duration = Duration::from_secs(5);

/// The endpoint names its windows (`five_hour`, `seven_day`) but the rest of
/// the app identifies a window by its length, so the names are translated
/// here — the one place that knows this provider's vocabulary.
const FIVE_HOUR_MINUTES: u32 = 5 * 60;
const WEEKLY_MINUTES: u32 = 7 * 24 * 60;

/// The subset of the response we use.
///
/// Both fields are optional because the endpoint only includes a window when
/// it applies to the account — a missing `seven_day` is a normal answer, not
/// an error. Other keys it sends (`seven_day_opus`, `seven_day_sonnet`,
/// `overage`, …) are deliberately not modelled: the spec wants two gauges, and
/// unmodelled keys are dropped by serde, so the endpoint can grow fields
/// without breaking us.
#[derive(Debug, Deserialize)]
struct UsageResponse {
    five_hour: Option<RawWindow>,
    seven_day: Option<RawWindow>,
}

#[derive(Debug, Deserialize)]
struct RawWindow {
    /// Despite the name, this is **already a percentage** (0–100), not a
    /// 0–1 fraction.
    ///
    /// Verified against the live endpoint: it returned `62` and `34` for
    /// windows the popover should show as 62% and 34%. The `utilization * 100`
    /// that appears in the CLI's own code operates on a *normalized* internal
    /// struct further down its pipeline, not on this response — copying that
    /// line here made every window pin at 100%.
    utilization: f64,
    resets_at: ResetsAt,
}

/// When the window resets.
///
/// Observed in the wild as an RFC 3339 string
/// (`"2026-09-11T04:00:01.028710+00:00"`). The CLI's internals also handle a
/// Unix-seconds number, so we accept both rather than bet on one: this is an
/// undocumented endpoint, and `#[serde(untagged)]` costs nothing to tolerate a
/// change that would otherwise take the app down to `unavailable`.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ResetsAt {
    Seconds(i64),
    Timestamp(String),
}

impl ResetsAt {
    /// Milliseconds since the Unix epoch, or `None` if unparseable.
    fn to_unix_ms(&self) -> Option<i64> {
        match self {
            Self::Seconds(s) => s.checked_mul(1000),
            Self::Timestamp(text) => {
                use time::format_description::well_known::Rfc3339;
                let parsed = time::OffsetDateTime::parse(text, &Rfc3339).ok()?;
                Some(parsed.unix_timestamp() * 1000)
            }
        }
    }
}

impl RawWindow {
    fn into_window(self, window_minutes: u32) -> Result<LimitWindow, UsageError> {
        let resets_at_ms = self.resets_at.to_unix_ms().ok_or_else(|| {
            // Shape only — the value is a timestamp, but keeping one rule for
            // every diagnostic is what stops a token leaking through the next
            // one someone adds.
            UsageError::Malformed(format!("{window_minutes}분 창의 resets_at을 해석할 수 없음"))
        })?;

        // Clamping guards the UI — a gauge cannot render -3% or 140% — but a
        // clamp also silently swallows a unit change. That is exactly how the
        // first version of this code shipped a bug: it assumed a 0–1 fraction,
        // multiplied by 100, and the clamp turned every window into a
        // confident, wrong 100%. So say something when the input leaves the
        // range we assume, rather than quietly squaring it away.
        if !(0.0..=100.0).contains(&self.utilization) {
            eprintln!(
                "[ai-reactor] 경고: {window_minutes}분 창의 utilization이 0~100 범위 밖입니다 \
                 ({}). 단위가 바뀌었을 수 있습니다.",
                self.utilization
            );
        }

        Ok(LimitWindow {
            window_minutes,
            // The endpoint reports usage; the rest of the app speaks
            // remaining. This is the only place the flip happens for Claude.
            remaining_percent: 100.0 - self.utilization.clamp(0.0, 100.0),
            resets_at_ms,
        })
    }
}

/// Turn a response body into windows. Split out from the HTTP call so the
/// response contract can be tested without a network or a credential.
pub fn parse_response(body: &[u8]) -> Result<Vec<LimitWindow>, UsageError> {
    let parsed: UsageResponse = serde_json::from_slice(body).map_err(|e| {
        // Include the *shape* that actually arrived. When an undocumented
        // endpoint changes, serde's message alone ("expected i64 at column
        // 79") does not say what it changed to.
        let shape = std::str::from_utf8(body)
            .map(crate::json_shape::describe_text)
            .unwrap_or_else(|_| "UTF-8이 아님".into());
        UsageError::Malformed(format!("JSON 해석 실패: {e} / 실제 구조 {shape}"))
    })?;

    let mut windows = Vec::with_capacity(2);
    if let Some(w) = parsed.five_hour {
        windows.push(w.into_window(FIVE_HOUR_MINUTES)?);
    }
    if let Some(w) = parsed.seven_day {
        windows.push(w.into_window(WEEKLY_MINUTES)?);
    }

    if windows.is_empty() {
        // Valid JSON with neither window is not something we can draw, and
        // silently showing an empty popover would look like a bug.
        return Err(UsageError::Malformed(
            "five_hour / seven_day 둘 다 없음".into(),
        ));
    }
    Ok(windows)
}

pub struct AnthropicUsageProvider {
    credentials: Arc<CredentialStore>,
    agent: ureq::Agent,
}

impl AnthropicUsageProvider {
    pub fn new(credentials: Arc<CredentialStore>) -> Self {
        let agent = ureq::Agent::config_builder()
            // ureq defaults to rustls even when only native-tls is compiled
            // in, and then panics at the first https request. Say it
            // explicitly. We use the system's TLS stack because macOS already
            // links Security.framework: bundling rustls plus a copy of the
            // root certificates cost about half a megabyte for a single GET
            // per minute.
            .tls_config(
                ureq::tls::TlsConfig::builder()
                    .provider(ureq::tls::TlsProvider::NativeTls)
                    .build(),
            )
            .timeout_global(Some(TIMEOUT))
            // We want to *inspect* 4xx/5xx — a 429 carries a `Retry-After`
            // header we need — so turn off ureq's default of converting them
            // into errors before we can look.
            .http_status_as_error(false)
            // Belt and braces: refuse to follow a redirect off TLS, so a
            // bearer token can never be replayed over plaintext.
            .https_only(true)
            .user_agent(concat!("AI reactor/", env!("CARGO_PKG_VERSION")))
            .build()
            .new_agent();

        Self { credentials, agent }
    }
}

impl UsageProvider for AnthropicUsageProvider {
    fn id(&self) -> ProviderId {
        ProviderId::Claude
    }

    /// Claude counts as set up when a credential exists at all.
    ///
    /// Reads the *state*, never the secret, so this cannot raise a keychain
    /// prompt: a missing item answers without unlocking anything.
    fn installed(&self) -> bool {
        !matches!(
            self.credentials.state(),
            crate::credentials::CredentialState::Missing
        )
    }

    fn account(&self) -> super::Account {
        let Some(home) = std::env::var_os("HOME") else {
            return super::Account::default();
        };
        let path = std::path::PathBuf::from(home).join(".claude.json");
        std::fs::read_to_string(path)
            .ok()
            .and_then(|text| parse_account(&text))
            .unwrap_or_default()
    }

    fn fetch(&self) -> Result<Reading, UsageError> {
        // `use_token` hands the token to this closure by reference and keeps
        // it inside the store's lock, so it never lands in a local that could
        // be logged or outlive the request.
        let response = self
            .credentials
            .use_token(|token| {
                self.agent
                    .get(USAGE_URL)
                    .header("Authorization", &format!("Bearer {}", token.expose()))
                    .header("Content-Type", "application/json")
                    .call()
            })
            .map_err(UsageError::NoCredential)?;

        let mut response = match response {
            Ok(r) => r,
            Err(e) => return Err(transport_error(e)),
        };

        let status = response.status().as_u16();
        match status {
            200 => {
                let body = response
                    .body_mut()
                    .read_to_vec()
                    .map_err(|e| UsageError::Malformed(format!("본문 읽기 실패: {e}")))?;
                // No timestamp: the endpoint answers for right now, which is
                // what `Reading::live` means.
                parse_response(&body).map(Reading::live)
            }
            // The token is dead. We report it and stop; the CLI will refresh
            // it the next time the user runs `claude`.
            401 | 403 => Err(UsageError::Unauthorized),
            429 => Err(UsageError::RateLimited {
                retry_after_ms: retry_after_ms(&response),
            }),
            other => Err(UsageError::Server { status: other }),
        }
    }
}

/// Read `Retry-After`. The header may be a delay in seconds or an HTTP date;
/// we only honour the numeric form, and let the backoff choose a floor for
/// anything else rather than pull in a date parser for a rare case.
fn retry_after_ms<B>(response: &ureq::http::Response<B>) -> Option<i64> {
    let raw = response.headers().get("retry-after")?.to_str().ok()?;
    let seconds: i64 = raw.trim().parse().ok()?;
    // A hostile or buggy value should not park the app for a week.
    Some(seconds.clamp(0, 3600) * 1000)
}

fn transport_error(err: ureq::Error) -> UsageError {
    match err {
        ureq::Error::Timeout(_) => UsageError::Transport("응답 시간 초과".into()),
        ureq::Error::HostNotFound => UsageError::Transport("호스트를 찾을 수 없음".into()),
        ureq::Error::Io(e) => UsageError::Transport(e.to_string()),
        other => UsageError::Transport(other.to_string()),
    }
}

/// Unused today, but it is the seam the spec asks for: if the endpoint changes
/// shape, a replacement provider drops in here without touching the poller.
impl std::fmt::Debug for AnthropicUsageProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnthropicUsageProvider").finish_non_exhaustive()
    }
}

/// The signed-in account, from the `oauthAccount` block the `claude` CLI
/// writes into `~/.claude.json` after login.
///
/// That file is the CLI's own settings, not a credential — the token lives in
/// the keychain — and we only ever read it. Only the fields named below are
/// deserialised; everything else in it (project history, settings) is
/// skipped by serde and never held.
#[derive(Debug, Deserialize)]
struct ClaudeConfig {
    #[serde(rename = "oauthAccount")]
    oauth_account: Option<OauthAccount>,
}

#[derive(Debug, Deserialize)]
struct OauthAccount {
    #[serde(rename = "emailAddress")]
    email_address: Option<String>,
    /// `claude_pro`, `claude_max`, `claude_team`, `claude_enterprise`.
    #[serde(rename = "organizationType")]
    organization_type: Option<String>,
    /// Carries the Max multiplier (`…_max_5x` / `…_max_20x`) when there is one.
    #[serde(rename = "userRateLimitTier")]
    user_rate_limit_tier: Option<String>,
    #[serde(rename = "organizationRateLimitTier")]
    organization_rate_limit_tier: Option<String>,
}

fn parse_account(text: &str) -> Option<super::Account> {
    let account = serde_json::from_str::<ClaudeConfig>(text).ok()?.oauth_account?;
    let tier = account
        .user_rate_limit_tier
        .or(account.organization_rate_limit_tier)
        .unwrap_or_default();
    Some(super::Account {
        email: account.email_address,
        plan: account.organization_type.as_deref().map(|t| plan_label(t, &tier)),
    })
}

/// `claude_max` + `default_claude_max_20x` → "Max 20x"; `claude_pro` → "Pro".
/// An organisation type we have not seen still gets a readable name rather
/// than nothing.
fn plan_label(organization_type: &str, tier: &str) -> String {
    let base = organization_type.strip_prefix("claude_").unwrap_or(organization_type);
    let mut label = capitalize(base);
    if base == "max" {
        if tier.contains("20x") {
            label.push_str(" 20x");
        } else if tier.contains("5x") {
            label.push_str(" 5x");
        }
    }
    label
}

pub(crate) fn capitalize(word: &str) -> String {
    let mut chars = word.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape actually returned by the endpoint: `utilization` already a
    /// percentage, `resets_at` an RFC 3339 string.
    const SAMPLE: &str = r#"{
      "five_hour":  { "utilization": 62,  "resets_at": "2026-09-11T04:00:01.028710+00:00" },
      "seven_day":  { "utilization": 7.5, "resets_at": "2026-09-15T00:00:00+00:00" },
      "seven_day_opus": { "utilization": 90, "resets_at": "2026-09-15T00:00:00+00:00" },
      "overage": null
    }"#;

    /// Regression guard for the unit bug: 62 must stay 62, not become 100.
    #[test]
    fn utilization_is_already_a_percentage_and_is_flipped_to_remaining() {
        let windows = parse_response(SAMPLE.as_bytes()).expect("should parse");
        assert_eq!(windows.len(), 2, "only the two spec windows");

        assert_eq!(windows[0].window_minutes, FIVE_HOUR_MINUTES);
        assert!(
            (windows[0].remaining_percent - 38.0).abs() < 1e-9,
            "62% used must become 38% remaining, got {}",
            windows[0].remaining_percent
        );
        // 2026-09-11T04:00:01Z. Sub-second precision is dropped, which is
        // irrelevant for a countdown shown in minutes.
        assert_eq!(windows[0].resets_at_ms, 1_789_099_201_000);

        assert_eq!(windows[1].window_minutes, WEEKLY_MINUTES);
        assert!((windows[1].remaining_percent - 92.5).abs() < 1e-9);
        assert_eq!(windows[1].resets_at_ms, 1_789_430_400_000);
    }

    /// The numeric form is accepted too, so a change back does not break us.
    #[test]
    fn unix_seconds_resets_at_still_works() {
        let body = r#"{"five_hour":{"utilization":50,"resets_at":1789003600}}"#;
        let windows = parse_response(body.as_bytes()).expect("should parse");
        assert_eq!(windows[0].resets_at_ms, 1_789_003_600_000);
    }

    #[test]
    fn an_unparseable_resets_at_is_malformed() {
        let body = r#"{"five_hour":{"utilization":50,"resets_at":"next tuesday"}}"#;
        assert!(matches!(
            parse_response(body.as_bytes()),
            Err(UsageError::Malformed(_))
        ));
    }

    /// A shape change must produce a diagnosable message, not just serde's
    /// column number.
    #[test]
    fn a_shape_change_is_described_in_the_error() {
        let body = r#"{"five_hour":{"utilization":"50","resets_at":1}}"#;
        let err = parse_response(body.as_bytes()).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("utilization: string(len=2)"), "{msg}");
    }

    /// Extra keys must not break us — the endpoint can add fields any time.
    #[test]
    fn unmodelled_fields_are_ignored() {
        let body = r#"{"five_hour":{"utilization":50,"resets_at":1},"brand_new_key":{"a":1}}"#;
        let windows = parse_response(body.as_bytes()).expect("should parse");
        assert_eq!(windows.len(), 1);
    }

    #[test]
    fn a_missing_window_is_normal_not_an_error() {
        let body = r#"{"five_hour":{"utilization":50,"resets_at":1}}"#;
        assert_eq!(parse_response(body.as_bytes()).unwrap().len(), 1);
    }

    #[test]
    fn no_windows_at_all_is_malformed() {
        assert!(matches!(
            parse_response(b"{}"),
            Err(UsageError::Malformed(_))
        ));
    }

    #[test]
    fn garbage_is_malformed_not_a_panic() {
        assert!(matches!(
            parse_response(b"<html>502</html>"),
            Err(UsageError::Malformed(_))
        ));
    }

    /// Defensive: the gauge and the cat thresholds both assume 0–100.
    #[test]
    fn out_of_range_utilization_is_clamped() {
        let body = r#"{"five_hour":{"utilization":140,"resets_at":1},"seven_day":{"utilization":-20,"resets_at":1}}"#;
        let windows = parse_response(body.as_bytes()).unwrap();
        assert_eq!(windows[0].remaining_percent, 0.0);
        assert_eq!(windows[1].remaining_percent, 100.0);
    }

    #[test]
    fn retry_after_is_read_and_capped() {
        let build = |v: &str| {
            ureq::http::Response::builder()
                .status(429)
                .header("retry-after", v)
                .body(())
                .unwrap()
        };
        assert_eq!(retry_after_ms(&build("30")), Some(30_000));
        // An HTTP-date form is not parsed; the backoff picks a floor instead.
        assert_eq!(retry_after_ms(&build("Wed, 21 Oct 2026 07:28:00 GMT")), None);
        // Absurd values are capped at an hour.
        assert_eq!(retry_after_ms(&build("999999")), Some(3_600_000));
    }

    #[test]
    fn missing_retry_after_is_none() {
        let response = ureq::http::Response::builder().status(429).body(()).unwrap();
        assert_eq!(retry_after_ms(&response), None);
    }

    /// Guardrail, asserted rather than left to a comment: the one URL this
    /// program requests is the usage endpoint, over TLS.
    #[test]
    fn the_only_endpoint_is_the_usage_endpoint() {
        assert_eq!(USAGE_URL, "https://api.anthropic.com/api/oauth/usage");
        assert!(USAGE_URL.starts_with("https://"));
        assert!(!USAGE_URL.contains("token"), "must never point at a token endpoint");
        assert!(!USAGE_URL.contains("refresh"), "must never point at a refresh endpoint");
    }

    #[test]
    fn reads_email_and_plan_from_the_cli_config() {
        let text = r#"{"projects":{"/x":{}},"oauthAccount":{"emailAddress":"a@b.c","organizationType":"claude_pro","organizationRateLimitTier":"default_claude_ai"}}"#;
        let account = parse_account(text).unwrap();
        assert_eq!(account.email.as_deref(), Some("a@b.c"));
        assert_eq!(account.plan.as_deref(), Some("Pro"));
    }

    #[test]
    fn max_plans_carry_their_multiplier() {
        assert_eq!(plan_label("claude_max", "default_claude_max_20x"), "Max 20x");
        assert_eq!(plan_label("claude_max", "default_claude_max_5x"), "Max 5x");
        assert_eq!(plan_label("claude_team", ""), "Team");
    }

    #[test]
    fn no_login_means_no_account() {
        assert_eq!(parse_account(r#"{"projects":{}}"#), None);
        assert_eq!(parse_account("not json"), None);
    }
}
