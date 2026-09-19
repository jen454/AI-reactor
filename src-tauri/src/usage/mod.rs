//! Usage limits — the numbers the monkey is reporting on.
//!
//! # Shape of this module
//!
//! [`UsageProvider`] is the seam. Everything above it — the poller, the menu
//! bar, the popover — works against the trait, so each provider's very
//! different data path stays behind it.
//!
//! That the two paths *are* different is the point. Claude needs an
//! undocumented OAuth endpoint over the network; Codex's limits are already
//! sitting in a local session log. If both looked the same, the abstraction
//! would be shaped around one of them and the third provider would break it.
//!
//! # Two conventions, fixed at the boundary
//!
//! 1. **Everything is remaining, never used.** Both providers report
//!    `used_percent`; adapters flip it once, on the way in. Mixing the two
//!    means a screen where "40%" points in opposite directions depending on
//!    which widget you look at.
//! 2. **Windows are identified by their length, not by a name.** Claude has a
//!    5-hour and a weekly window; this Codex plan has one 30-day window and no
//!    second. Anything that assumes "two windows, session and weekly" breaks
//!    on the first plan that disagrees.

pub mod anthropic;
pub mod backoff;
pub mod codex;
pub mod demo;
pub mod format;
pub mod poller;
pub mod schedule;

use serde::Serialize;

/// Which agent a reading belongs to.
///
/// Adding a provider is adding a variant plus an adapter; nothing above the
/// trait needs to enumerate them except where the UI needs a display name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ProviderId {
    Claude,
    // The adapter lands in milestone 2; the identity exists now so the model,
    // the registry ordering and the tests are already multi-provider.
    #[allow(dead_code)]
    Codex,
}

impl ProviderId {
    /// Shown on the provider card. The logo marks are used only to say which
    /// provider a card or gauge is about — never in the app's own name or
    /// icon (SPEC §6, §8).
    pub fn label(self) -> &'static str {
        match self {
            Self::Claude => "Claude",
            Self::Codex => "Codex",
        }
    }

    /// Fixed order, so neither the tray's status items nor the popover's
    /// cards swap positions as providers come and go.
    pub const ALL: [ProviderId; 2] = [ProviderId::Claude, ProviderId::Codex];
}

/// What kind of period a window covers.
///
/// A *label*, derived from the window's length — not its identity. The spec's
/// data model names two kinds, but the live Codex plan reports a 43200-minute
/// (30-day) window that is neither a session nor a week. Deriving the label
/// from the length means an unfamiliar plan gets a sensible name instead of a
/// wrong one, and nothing downstream has to enumerate the possibilities.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum WindowKind {
    /// Up to a day. Claude's five-hour window.
    Session,
    /// Up to a fortnight. Claude's seven-day window.
    Weekly,
    /// Longer. The Codex plan's thirty days.
    Monthly,
}

impl WindowKind {
    pub fn for_minutes(minutes: u32) -> Self {
        // Generous bands rather than exact matches: a plan with a six-day or
        // an eight-day window should still read as "weekly".
        if minutes < 1_440 {
            Self::Session
        } else if minutes < 20_160 {
            Self::Weekly
        } else {
            Self::Monthly
        }
    }
}

/// One window's reading.
///
/// `kind` is not stored — it is derived from the length on the way out (see
/// the `Serialize` impl below). Keeping it out of the struct means there is
/// exactly one source of truth for what kind of window this is, and no way for
/// a stored label to drift out of step with the length it describes.
#[derive(Debug, Clone, PartialEq)]
pub struct LimitWindow {
    /// The window's length. This *is* its identity: Claude's 300 and 10080,
    /// this Codex plan's 43200. Providers may add or drop windows, and a plan
    /// change can alter the set, so nothing keys off a fixed list.
    pub window_minutes: u32,
    /// 0–100 **remaining**. Providers report usage; adapters flip it.
    pub remaining_percent: f64,
    /// Milliseconds since the Unix epoch. Providers report Unix seconds or an
    /// RFC 3339 string; adapters normalise.
    pub resets_at_ms: i64,
}

/// Serialised with `kind` alongside the length, because the UI wants both:
/// the kind for grouping and copy, the length for the exact label.
impl Serialize for LimitWindow {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut out = serializer.serialize_struct("LimitWindow", 4)?;
        out.serialize_field("kind", &self.kind())?;
        out.serialize_field("windowMinutes", &self.window_minutes)?;
        out.serialize_field("remainingPercent", &self.remaining_percent)?;
        out.serialize_field("resetsAtMs", &self.resets_at_ms)?;
        out.end()
    }
}

impl LimitWindow {
    /// The kind of period this window covers.
    pub fn kind(&self) -> WindowKind {
        WindowKind::for_minutes(self.window_minutes)
    }

    /// A human name for the window.
    ///
    /// The common durations get the words people actually use; anything else
    /// is derived, so an unfamiliar plan shows "12시간" rather than a blank or
    /// a wrong label.
    pub fn label(&self) -> String {
        match self.window_minutes {
            300 => "5시간".into(),
            1_440 => "일간".into(),
            10_080 => "주간".into(),
            43_200 => "월간".into(),
            m if m < 60 => format!("{m}분"),
            m if m < 1_440 => format!("{}시간", m / 60),
            m => format!("{}일", m / 1_440),
        }
    }

    /// Whether this reading is about a window that has already rolled over.
    ///
    /// A snapshot read from a log can outlive its own window. When it does,
    /// the number is not merely old — it describes a window that no longer
    /// exists, and the real remaining figure is probably "full". Showing the
    /// stale number, even dimmed, would be a confident lie.
    pub fn is_void_at(&self, now_ms: i64) -> bool {
        self.resets_at_ms <= now_ms
    }
}

/// How much to trust a provider's snapshot.
///
/// The spec's four empty states live here, minus "loading" — that is the
/// absence of a snapshot, which `Option` already expresses, and giving it a
/// variant would let code forget to distinguish "not fetched yet" from
/// "fetched and empty".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum SnapshotStatus {
    /// Just read successfully, and the windows are current.
    #[serde(rename = "ok")]
    Ok,
    /// A read failed; these are the last numbers we actually got.
    Stale,
    /// Nothing we can stand behind. The reason travels alongside, because the
    /// four ways of having no numbers need four different sentences.
    #[serde(rename = "error")]
    Error,
    /// The credential is past its lifetime. Recovers when the user runs the
    /// CLI once — we never refresh it ourselves.
    Expired,
    /// The provider is not set up on this machine. Not an error, and not
    /// something to show a spinner for.
    #[serde(rename = "notInstalled")]
    NotInstalled,
}

impl SnapshotStatus {
    /// Whether the numbers in this snapshot may be shown at all.
    ///
    /// `Stale` and `Expired` still carry real figures, dimmed. `Void` does
    /// not — that is the whole distinction.
    pub fn has_usable_numbers(self) -> bool {
        matches!(self, Self::Ok | Self::Stale | Self::Expired)
    }
}

/// Why a provider has no numbers to show.
///
/// `SnapshotStatus` says *that* there is nothing; this says why, and the four
/// answers need four different sentences on the card. Collapsing them into one
/// "no data" message would tell someone with no Codex plan to go run Codex.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ErrorReason {
    /// Set up and signed in, but the account has no limits to report — no
    /// subscription, or an entitlement that does not express itself as a
    /// window. Observed in the wild as a record whose windows are all null.
    NoPlan,
    /// Nothing has ever been recorded. Running the CLI once fixes it.
    NoData,
    /// We have a reading, but the window it describes has already reset, so the
    /// number in it was never about the period we are in now.
    WindowReset,
    /// The read itself failed and there was no earlier reading to fall back on.
    ReadFailed,
    /// The keychain could not be consulted — locked, or the machine is awake
    /// only enough to run timers and cannot show a prompt. Its own case
    /// because the fix is different: this one needs a person at the machine,
    /// not a retry.
    Locked,
    /// The user answered the keychain prompt with Deny. We ask again after a
    /// backoff; the card says to pick "Always Allow" when it comes back.
    Denied,
}

/// What an adapter hands back.
///
/// Carries *when* as well as *what*, because the two providers differ on it:
/// Claude's numbers are true as of the request, while Codex's come out of a
/// log that may not have been written to in months. A model that assumed
/// "now" would quietly date-stamp a three-month-old reading as current.
#[derive(Debug, Clone, PartialEq)]
pub struct Reading {
    pub windows: Vec<LimitWindow>,
    /// When the provider itself says these numbers were true. `None` means
    /// "as of this read" — a live fetch.
    pub captured_at_ms: Option<i64>,
}

impl Reading {
    /// A live reading, true as of now.
    pub fn live(windows: Vec<LimitWindow>) -> Self {
        Self {
            windows,
            captured_at_ms: None,
        }
    }
}

/// One provider's state.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderSnapshot {
    pub provider: ProviderId,
    pub windows: Vec<LimitWindow>,
    /// When these numbers were captured — not when they were last read out of
    /// a cache. For Codex this is the session log's own timestamp, which can
    /// be days old, and the card shows it.
    pub captured_at_ms: i64,
    pub status: SnapshotStatus,
    /// Set when there is nothing to show, so the card can say why.
    pub error_reason: Option<ErrorReason>,
}

impl ProviderSnapshot {
    pub fn empty(provider: ProviderId, status: SnapshotStatus, captured_at_ms: i64) -> Self {
        Self {
            provider,
            windows: Vec::new(),
            captured_at_ms,
            status,
            error_reason: None,
        }
    }

    pub fn with_reason(mut self, reason: ErrorReason) -> Self {
        self.error_reason = Some(reason);
        self
    }

    /// The window closest to running out — the one worth reporting.
    pub fn tightest_window(&self) -> Option<&LimitWindow> {
        self.windows.iter().min_by(|a, b| {
            a.remaining_percent
                .partial_cmp(&b.remaining_percent)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
    }

    /// Remaining percent for the tightest window, if there is one worth
    /// trusting. `None` means "we do not know", which the UI must render
    /// differently from "0%".
    #[allow(dead_code)] // the tests' shorthand; the menu bar uses the session window
    pub fn remaining_percent(&self) -> Option<f64> {
        if !self.status.has_usable_numbers() {
            return None;
        }
        self.tightest_window().map(|w| w.remaining_percent)
    }
}

/// Anything that can report a provider's limits.
///
/// Synchronous on purpose: one blocking read per cycle, on a thread that
/// sleeps. No async runtime, no coloured functions, for what is a file read
/// and one HTTP GET a minute.
pub trait UsageProvider: Send + Sync {
    fn id(&self) -> ProviderId;

    /// Whether this provider is set up on the machine at all.
    ///
    /// Separate from `fetch` because "not installed" is a permanent, quiet
    /// state — an empty card, no spinner, no retry — and finding it out must
    /// not cost a network round trip or a keychain prompt.
    fn installed(&self) -> bool;

    /// The provider's current reading.
    fn fetch(&self) -> Result<Reading, UsageError>;

    /// Which account is signed in here, and on what plan — as far as this
    /// provider can say from local, non-secret files. Separate from `fetch`
    /// because it changes rarely and is only wanted when the popover opens.
    fn account(&self) -> Account {
        Account::default()
    }
}

/// Who is signed in, and on what plan. Both optional: a provider only fills
/// in what it can learn without reading a secret (Codex's email lives only in
/// `~/.codex/auth.json`, which this app never opens).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    pub email: Option<String>,
    /// A display name for the plan — "Pro", "Max 20x", "Go".
    pub plan: Option<String>,
}

/// One provider's account, for the popover.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderAccount {
    pub provider: ProviderId,
    #[serde(flatten)]
    pub account: Account,
}

/// Why a read did not produce numbers.
///
/// Every variant is a normal outcome with an answer in the UI. None is a
/// reason to panic or to show an error dialog.
#[derive(Debug, Clone, PartialEq)]
pub enum UsageError {
    /// 401, or a credential we know is past its lifetime. **Never refresh.**
    Unauthorized,
    /// 429. `retry_after_ms` is the server's `Retry-After` when it sent one.
    RateLimited { retry_after_ms: Option<i64> },
    /// No usable credential; carries the credential-layer state verbatim.
    NoCredential(crate::credentials::CredentialState),
    /// The provider is not set up here.
    NotInstalled,
    /// Set up, but it has never recorded any limit data we can read.
    NoData,
    /// Set up and signed in, but the account reports no limits at all.
    NoPlan,
    /// DNS, TLS, timeout, offline, or a file we could not read.
    Transport(String),
    /// 5xx, or any other unexpected status.
    Server { status: u16 },
    /// We got a response but could not read it. Describes shape, never values.
    Malformed(String),
}

impl std::fmt::Display for UsageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unauthorized => write!(f, "토큰이 만료되었습니다"),
            Self::RateLimited { retry_after_ms } => match retry_after_ms {
                Some(ms) => write!(f, "요청 한도 초과 ({}초 후 재시도)", ms / 1000),
                None => write!(f, "요청 한도 초과 (재시도 시각 미지정)"),
            },
            Self::NoCredential(state) => write!(f, "자격증명 없음 ({state:?})"),
            Self::NotInstalled => write!(f, "설치되어 있지 않습니다"),
            Self::NoData => write!(f, "기록된 한도 데이터가 없습니다"),
            Self::NoPlan => write!(f, "이 계정에 표시할 한도가 없습니다"),
            Self::Transport(what) => write!(f, "읽기 오류: {what}"),
            Self::Server { status } => write!(f, "서버 오류 (HTTP {status})"),
            Self::Malformed(what) => write!(f, "응답을 해석할 수 없습니다: {what}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(window_minutes: u32, remaining_percent: f64, resets_at_ms: i64) -> LimitWindow {
        LimitWindow {
            window_minutes,
            remaining_percent,
            resets_at_ms,
        }
    }

    #[test]
    fn known_window_lengths_get_the_words_people_use() {
        assert_eq!(window(300, 0.0, 0).label(), "5시간");
        assert_eq!(window(10_080, 0.0, 0).label(), "주간");
        assert_eq!(window(43_200, 0.0, 0).label(), "월간");
    }

    /// An unfamiliar plan must still render something sensible rather than a
    /// blank or a wrong name.
    #[test]
    fn unknown_window_lengths_are_derived() {
        assert_eq!(window(30, 0.0, 0).label(), "30분");
        assert_eq!(window(720, 0.0, 0).label(), "12시간");
        assert_eq!(window(4_320, 0.0, 0).label(), "3일");
    }

    #[test]
    fn a_window_past_its_reset_is_void() {
        let w = window(300, 12.0, 1_000);
        assert!(!w.is_void_at(999));
        assert!(w.is_void_at(1_000));
        assert!(w.is_void_at(5_000));
    }

    /// The icon and the prediction both care about the window closest to
    /// running out, which under a remaining model is the *smallest*.
    #[test]
    fn the_tightest_window_is_the_one_with_least_left() {
        let snapshot = ProviderSnapshot {
            provider: ProviderId::Claude,
            windows: vec![window(300, 88.0, 0), window(10_080, 12.0, 0)],
            captured_at_ms: 0,
            status: SnapshotStatus::Ok,
            error_reason: None,
        };
        assert_eq!(snapshot.tightest_window().unwrap().window_minutes, 10_080);
        assert_eq!(snapshot.remaining_percent(), Some(12.0));
    }

    /// The distinction the spec insists on: void carries no number at all,
    /// while stale carries a dimmed one.
    #[test]
    fn void_reports_no_number_but_stale_still_does() {
        let mut snapshot = ProviderSnapshot {
            provider: ProviderId::Codex,
            windows: vec![window(43_200, 4.0, 0)],
            captured_at_ms: 0,
            status: SnapshotStatus::Stale,
            error_reason: None,
        };
        assert_eq!(snapshot.remaining_percent(), Some(4.0));

        snapshot.status = SnapshotStatus::Error;
        assert_eq!(
            snapshot.remaining_percent(),
            None,
            "a rolled-over window must not report its old number"
        );

        snapshot.status = SnapshotStatus::NotInstalled;
        assert_eq!(snapshot.remaining_percent(), None);
    }

    #[test]
    fn an_empty_snapshot_has_no_tightest_window() {
        let s = ProviderSnapshot::empty(ProviderId::Codex, SnapshotStatus::NotInstalled, 0);
        assert!(s.tightest_window().is_none());
        assert_eq!(s.remaining_percent(), None);
    }

    #[test]
    fn snapshot_serializes_with_remaining_not_used() {
        let s = ProviderSnapshot {
            provider: ProviderId::Claude,
            windows: vec![window(300, 37.0, 1_789_000_000_000)],
            captured_at_ms: 1_788_999_000_000,
            status: SnapshotStatus::Ok,
            error_reason: None,
        };
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains(r#""remainingPercent":37.0"#), "{json}");
        assert!(!json.contains("used"), "usage leaked into the model: {json}");
        assert!(json.contains(r#""provider":"claude""#), "{json}");
        assert!(json.contains(r#""windowMinutes":300"#), "{json}");
        assert!(json.contains(r#""kind":"session""#), "{json}");
        assert!(json.contains(r#""status":"ok""#), "{json}");
    }

    /// The spec names two kinds; the live Codex plan reports a third length.
    /// Deriving the label means an unfamiliar plan gets a sensible name rather
    /// than a wrong one.
    #[test]
    fn window_kinds_come_from_the_length() {
        assert_eq!(WindowKind::for_minutes(300), WindowKind::Session);
        assert_eq!(WindowKind::for_minutes(1_439), WindowKind::Session);
        assert_eq!(WindowKind::for_minutes(10_080), WindowKind::Weekly);
        assert_eq!(WindowKind::for_minutes(20_159), WindowKind::Weekly);
        // The one that broke the two-kind model.
        assert_eq!(WindowKind::for_minutes(43_200), WindowKind::Monthly);
    }
}
