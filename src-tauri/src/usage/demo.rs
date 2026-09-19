//! Fake providers that walk through every state, for looking at the UI.
//!
//! # Why this exists
//!
//! Four of the popover's states cannot be produced on demand. You cannot
//! uninstall Claude to see the empty card, cannot revoke a subscription to see
//! `noPlan`, and cannot make a token expire on cue. The alternative to this
//! file is shipping states nobody has ever looked at.
//!
//! `AI_REACTOR_DEMO=1` swaps the real providers for these. It is never on by
//! accident: nothing reads the env var except at startup, and the real
//! adapters are untouched.

use std::time::{SystemTime, UNIX_EPOCH};

use super::{LimitWindow, ProviderId, Reading, UsageError, UsageProvider};

/// How long each state is held before moving to the next.
///
/// Must be comfortably longer than the demo poll interval below, or states
/// change faster than they are read and some are never seen at all — which
/// defeats the point of looking.
const STEP_SECONDS: u64 = 10;

/// Demo mode polls faster than the real app so a full cycle takes a minute
/// rather than six. Nothing is being fetched, so there is nothing to be
/// gentle towards.
pub const POLL: std::time::Duration = std::time::Duration::from_secs(4);

/// Whether demo mode is on.
pub fn enabled() -> bool {
    std::env::var("AI_REACTOR_DEMO").is_ok()
}

/// The states worth looking at, in order.
///
/// Ordered to tell a story rather than alphabetically: healthy, then the
/// numbers going stale, then the token dying, then the three different ways a
/// provider can have nothing to say.
#[derive(Debug, Clone, Copy)]
enum Step {
    Healthy,
    Stale,
    Expired,
    NoPlan,
    NoData,
    NotInstalled,
}

const CLAUDE_STEPS: [Step; 6] = [
    Step::Healthy,
    Step::Stale,
    Step::Expired,
    Step::NoData,
    Step::NoPlan,
    Step::NotInstalled,
];

/// Codex runs the same cycle offset by half, so the two cards are rarely in
/// the same state — which is the arrangement that actually exercises the
/// layout.
const CODEX_OFFSET: usize = 3;

pub struct DemoProvider {
    id: ProviderId,
    offset: usize,
}

impl DemoProvider {
    pub fn pair() -> Vec<Box<dyn UsageProvider>> {
        vec![
            Box::new(DemoProvider {
                id: ProviderId::Claude,
                offset: 0,
            }),
            Box::new(DemoProvider {
                id: ProviderId::Codex,
                offset: CODEX_OFFSET,
            }),
        ]
    }

    fn step(&self) -> Step {
        let seconds = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let index = (seconds / STEP_SECONDS) as usize + self.offset;
        CLAUDE_STEPS[index % CLAUDE_STEPS.len()]
    }
}

impl UsageProvider for DemoProvider {
    fn id(&self) -> ProviderId {
        self.id
    }

    fn installed(&self) -> bool {
        !matches!(self.step(), Step::NotInstalled)
    }

    fn fetch(&self) -> Result<Reading, UsageError> {
        let now_ms = crate::credentials::now_ms();

        match self.step() {
            Step::Healthy => Ok(Reading::live(windows(now_ms, 58.0, 86.0))),
            // Reported as captured a while ago, which is what the freshness
            // rule turns into `stale`.
            Step::Stale => Ok(Reading {
                windows: windows(now_ms, 58.0, 86.0),
                captured_at_ms: Some(now_ms - 40 * 60_000),
            }),
            Step::Expired => Err(UsageError::Unauthorized),
            Step::NoPlan => Err(UsageError::NoPlan),
            Step::NoData => Err(UsageError::NoData),
            Step::NotInstalled => Err(UsageError::NotInstalled),
        }
    }
}

fn windows(now_ms: i64, session: f64, weekly: f64) -> Vec<LimitWindow> {
    vec![
        LimitWindow {
            window_minutes: 300,
            remaining_percent: session,
            resets_at_ms: now_ms + 2 * 3_600_000,
        },
        LimitWindow {
            window_minutes: 10_080,
            remaining_percent: weekly,
            resets_at_ms: now_ms + 4 * 24 * 3_600_000,
        },
    ]
}
