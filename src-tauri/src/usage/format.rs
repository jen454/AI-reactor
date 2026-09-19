//! Console rendering of the current state.
//!
//! Lets the data layer be watched working before there is UI to show it in,
//! and stays as the thing you read when something looks wrong.

use super::{ProviderId, ProviderSnapshot, SnapshotStatus};

/// "1시간 12분", "5일 3시간", "지금".
pub fn format_remaining(ms: i64) -> String {
    if ms <= 0 {
        return "지금".into();
    }
    let minutes = ms / 60_000;
    let (days, hours, mins) = (minutes / 1440, (minutes % 1440) / 60, minutes % 60);

    if days > 0 {
        format!("{days}일 {hours}시간")
    } else if hours > 0 {
        format!("{hours}시간 {mins}분")
    } else {
        format!("{mins}분")
    }
}

fn status_label(status: SnapshotStatus) -> &'static str {
    match status {
        SnapshotStatus::Ok => "ok",
        SnapshotStatus::Stale => "stale",
        SnapshotStatus::Error => "error",
        SnapshotStatus::Expired => "expired",
        SnapshotStatus::NotInstalled => "not-installed",
    }
}

/// One provider's line.
fn format_provider(snapshot: &ProviderSnapshot, now_ms: i64, error: Option<&str>) -> String {
    let name = snapshot.provider.label();
    let status = status_label(snapshot.status);

    // A provider with no usable numbers says why, not a row of dashes. `Void`
    // and `NotInstalled` look identical on a gauge but mean different things.
    if !snapshot.status.has_usable_numbers() || snapshot.windows.is_empty() {
        return match error {
            Some(e) => format!("{name}: {status} ({e})"),
            None => format!("{name}: {status}"),
        };
    }

    let windows: Vec<String> = snapshot
        .windows
        .iter()
        .map(|w| {
            format!(
                "{} {:.0}% 남음 (리셋 {} 후)",
                w.label(),
                w.remaining_percent,
                format_remaining(w.resets_at_ms - now_ms)
            )
        })
        .collect();

    // Only worth mentioning when the numbers are not current; on a fresh
    // snapshot the age is zero by definition and would just be noise.
    let age = match snapshot.status {
        SnapshotStatus::Ok => String::new(),
        _ => format!(" [{} 전]", format_remaining(now_ms - snapshot.captured_at_ms)),
    };

    // A provider showing old numbers has to say what went wrong. Without this
    // a frozen reading looks identical to a quiet one, and the only way to
    // tell them apart is to watch the timestamp creep — which is exactly the
    // kind of silent failure the rest of this app refuses to ship.
    let why = match (snapshot.status, error) {
        (SnapshotStatus::Ok, _) | (_, None) => String::new(),
        (_, Some(e)) => format!(" ({e})"),
    };

    format!("{name}: {status}{why} {}{age}", windows.join(" | "))
}

/// One line per poll cycle, covering every provider.
pub fn format_snapshots(
    snapshots: &[ProviderSnapshot],
    errors: &[(ProviderId, String)],
    now_ms: i64,
) -> String {
    if snapshots.is_empty() {
        return "[ai-reactor] 등록된 프로바이더 없음".into();
    }

    let parts: Vec<String> = snapshots
        .iter()
        .map(|s| {
            let error = errors
                .iter()
                .find(|(id, _)| *id == s.provider)
                .map(|(_, e)| e.as_str());
            format_provider(s, now_ms, error)
        })
        .collect();

    format!("[ai-reactor] {}", parts.join("  ·  "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usage::LimitWindow;

    const MIN: i64 = 60_000;
    const HOUR: i64 = 60 * MIN;

    fn window(window_minutes: u32, remaining_percent: f64, resets_at_ms: i64) -> LimitWindow {
        LimitWindow {
            window_minutes,
            remaining_percent,
            resets_at_ms,
        }
    }

    #[test]
    fn formats_durations_at_each_scale() {
        assert_eq!(format_remaining(0), "지금");
        assert_eq!(format_remaining(-5 * MIN), "지금");
        assert_eq!(format_remaining(42 * MIN), "42분");
        assert_eq!(format_remaining(HOUR + 12 * MIN), "1시간 12분");
        assert_eq!(format_remaining(5 * 24 * HOUR + 3 * HOUR), "5일 3시간");
    }

    #[test]
    fn a_fresh_provider_reads_cleanly() {
        let s = ProviderSnapshot {
            provider: ProviderId::Claude,
            windows: vec![window(300, 58.0, HOUR), window(10_080, 92.0, 3 * 24 * HOUR)],
            captured_at_ms: 0,
            status: SnapshotStatus::Ok,
            error_reason: None,
        };
        assert_eq!(
            format_snapshots(&[s], &[], 0),
            "[ai-reactor] Claude: ok 5시간 58% 남음 (리셋 1시간 0분 후) | 주간 92% 남음 (리셋 3일 0시간 후)"
        );
    }

    #[test]
    fn a_stale_provider_says_how_old_it_is() {
        let s = ProviderSnapshot {
            provider: ProviderId::Claude,
            windows: vec![window(300, 58.0, 10 * HOUR)],
            captured_at_ms: 0,
            status: SnapshotStatus::Stale,
            error_reason: None,
        };
        let line = format_snapshots(
            &[s],
            &[(ProviderId::Claude, "네트워크 오류: offline".into())],
            2 * HOUR,
        );
        assert!(line.contains("stale"), "{line}");
        assert!(line.contains("[2시간 0분 전]"), "{line}");
        assert!(
            line.contains("네트워크 오류: offline"),
            "a frozen reading must say why: {line}"
        );
    }

    /// The three no-number states have to be distinguishable in the log, or a
    /// bug report cannot say which one happened.
    #[test]
    fn the_empty_states_are_distinguishable() {
        let void = ProviderSnapshot::empty(ProviderId::Codex, SnapshotStatus::Error, 0);
        let line = format_snapshots(
            &[void],
            &[(ProviderId::Codex, "기록된 한도 데이터가 없습니다".into())],
            0,
        );
        assert_eq!(
            line,
            "[ai-reactor] Codex: error (기록된 한도 데이터가 없습니다)"
        );

        let absent = ProviderSnapshot::empty(ProviderId::Codex, SnapshotStatus::NotInstalled, 0);
        assert_eq!(
            format_snapshots(&[absent], &[], 0),
            "[ai-reactor] Codex: not-installed"
        );
    }

    /// A void snapshot must not print its numbers even if it still carries
    /// them — that is the whole point of the state.
    #[test]
    fn a_void_snapshot_prints_no_numbers() {
        let s = ProviderSnapshot {
            provider: ProviderId::Codex,
            windows: vec![window(43_200, 0.0, -HOUR)],
            captured_at_ms: -2 * HOUR,
            status: SnapshotStatus::Error,
            error_reason: None,
        };
        let line = format_snapshots(&[s], &[], 0);
        assert!(!line.contains('%'), "leaked a number: {line}");
    }



    #[test]
    fn both_providers_appear_on_one_line() {
        let claude = ProviderSnapshot {
            provider: ProviderId::Claude,
            windows: vec![window(300, 58.0, HOUR)],
            captured_at_ms: 0,
            status: SnapshotStatus::Ok,
            error_reason: None,
        };
        let codex = ProviderSnapshot::empty(ProviderId::Codex, SnapshotStatus::NotInstalled, 0);
        let line = format_snapshots(&[claude, codex], &[], 0);
        assert!(line.contains("Claude: ok"), "{line}");
        assert!(line.contains("Codex: not-installed"), "{line}");
    }
}
