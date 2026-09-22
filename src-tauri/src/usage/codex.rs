//! The adapter for Codex's live App Server limits, with local logs as fallback.
//!
//! # Live first, logs second
//!
//! `codex app-server` exposes `account/rateLimits/read`, the same supported
//! account surface used by richer Codex clients. The subprocess owns login and
//! token refresh; AI reactor never opens `~/.codex/auth.json` or receives a
//! credential. If the installed CLI is too old, cannot be found, is offline,
//! or times out, we fall back to the server snapshot Codex last wrote to disk.
//!
//! # What the log looks like
//!
//! `~/.codex/sessions/<year>/<month>/<day>/rollout-*.jsonl`, one JSON object
//! per line. The ones we want carry `payload.rate_limits`:
//!
//! ```json
//! { "timestamp": "2026-06-08T01:42:38.234Z",
//!   "payload": { "rate_limits": {
//!     "primary":   { "used_percent": 100.0, "window_minutes": 43200,
//!                    "resets_at": 1783127522 },
//!     "secondary": null } } }
//! ```
//!
//! Two shapes appear in practice, both observed on the same machine:
//!
//! - **With windows** — a plan that has limits.
//! - **All windows null** — signed in, but the account has no limits to
//!   report. That is [`UsageError::NoPlan`], and it is not the same thing as
//!   not having Codex installed.
//!
//! # This data goes stale in a way Claude's cannot
//!
//! The log is only written while the CLI is running. Stop using Codex and the
//! newest record simply ages; eventually the window it describes resets and
//! the number in it stops being about any period that exists. The freshness
//! rules live in the poller, but this is where the timestamp comes from, and
//! reporting it honestly is the whole reason [`Reading`] carries one.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::sync::Mutex;
use std::time::Duration;

use serde::Deserialize;

use super::{LimitWindow, ProviderId, Reading, UsageError, UsageProvider};

/// How many recent session files to look through before giving up.
///
/// The newest file usually has what we want. It might not — a session can end
/// before the server sends a limit update — so we walk back a little, but not
/// far: if the last dozen sessions recorded nothing, another hundred will not
/// help, and the answer "no data" is already correct.
const MAX_FILES_SCANNED: usize = 12;
const APP_SERVER_TIMEOUT: Duration = Duration::from_secs(5);

const INITIALIZED: &str = r#"{"method":"initialized","params":{}}"#;
const READ_LIMITS: &str = r#"{"method":"account/rateLimits/read","id":2}"#;

/// One line of the rollout log, as much of it as we care about.
#[derive(Debug, Deserialize)]
struct LogLine {
    /// RFC 3339, and the reason this adapter exists in the shape it does.
    timestamp: String,
    payload: Payload,
}

#[derive(Debug, Deserialize)]
struct Payload {
    rate_limits: Option<RateLimits>,
}

#[derive(Debug, Deserialize)]
struct RateLimits {
    /// `"go"`, `"plus"`, `"pro"`… or null for an account with no plan.
    plan_type: Option<String>,
    primary: Option<RawWindow>,
    secondary: Option<RawWindow>,
}

#[derive(Debug, Deserialize)]
struct RawWindow {
    /// 0–100 used, like Claude's despite the different name.
    used_percent: f64,
    /// The window's length, and its identity. This plan reports 43200 (30
    /// days); nothing assumes a particular value.
    window_minutes: u32,
    /// Unix **seconds**.
    resets_at: i64,
}

impl RawWindow {
    fn into_window(self) -> LimitWindow {
        LimitWindow {
            window_minutes: self.window_minutes,
            // The log reports usage; the app speaks remaining. This is the
            // only place the flip happens for Codex.
            remaining_percent: 100.0 - self.used_percent.clamp(0.0, 100.0),
            resets_at_ms: self.resets_at.saturating_mul(1000),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AppServerResult {
    rate_limits: Option<AppServerLimits>,
}

#[derive(Debug, Deserialize)]
struct AppServerEnvelope {
    id: Option<serde_json::Value>,
    result: Option<AppServerResult>,
    error: Option<AppServerError>,
}

#[derive(Debug, Deserialize)]
struct AppServerError {
    message: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AppServerLimits {
    primary: Option<AppServerWindow>,
    secondary: Option<AppServerWindow>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AppServerWindow {
    used_percent: f64,
    window_duration_mins: u32,
    resets_at: i64,
}

impl AppServerWindow {
    fn into_window(self) -> LimitWindow {
        LimitWindow {
            window_minutes: self.window_duration_mins,
            remaining_percent: 100.0 - self.used_percent.clamp(0.0, 100.0),
            resets_at_ms: self.resets_at.saturating_mul(1000),
        }
    }
}

fn parse_app_server_line(line: &str) -> Result<Option<Reading>, String> {
    let envelope: AppServerEnvelope =
        serde_json::from_str(line).map_err(|error| format!("응답 JSON: {error}"))?;
    if envelope.id != Some(serde_json::json!(2)) {
        return Ok(None);
    }
    if let Some(error) = envelope.error {
        return Err(format!("App Server: {}", error.message));
    }
    let result = envelope
        .result
        .ok_or_else(|| "App Server 응답에 result가 없습니다".to_string())?;
    let limits = result
        .rate_limits
        .ok_or_else(|| "App Server가 한도를 반환하지 않았습니다".to_string())?;
    let windows: Vec<LimitWindow> = [limits.primary, limits.secondary]
        .into_iter()
        .flatten()
        .map(AppServerWindow::into_window)
        .collect();
    if windows.is_empty() {
        return Err("App Server가 표시할 한도 구간을 반환하지 않았습니다".into());
    }
    Ok(Some(Reading::live(windows)))
}

fn query_app_server(codex: &Path) -> Result<Reading, String> {
    let mut child = Command::new(codex)
        .arg("app-server")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("App Server 실행: {error}"))?;

    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| "App Server stdin을 열 수 없습니다".to_string())?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "App Server stdout을 열 수 없습니다".to_string())?;

    let initialize = serde_json::json!({
        "method": "initialize",
        "id": 1,
        "params": {
            "clientInfo": {
                "name": "ai_reactor",
                "title": "AI reactor",
                "version": env!("CARGO_PKG_VERSION")
            }
        }
    })
    .to_string();
    for message in [initialize.as_str(), INITIALIZED, READ_LIMITS] {
        writeln!(stdin, "{message}").map_err(|error| format!("App Server 요청: {error}"))?;
    }
    stdin
        .flush()
        .map_err(|error| format!("App Server 요청 전송: {error}"))?;

    let (sender, receiver) = mpsc::sync_channel(1);
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let result = match line {
                Ok(line) => parse_app_server_line(&line),
                Err(error) => Err(format!("App Server 응답 읽기: {error}")),
            };
            match result {
                Ok(None) => continue,
                answer => {
                    let _ = sender.send(answer.and_then(|reading| {
                        reading.ok_or_else(|| "App Server 응답이 비어 있습니다".into())
                    }));
                    return;
                }
            }
        }
        let _ = sender.send(Err("App Server가 응답 없이 종료되었습니다".into()));
    });

    let answer = receiver
        .recv_timeout(APP_SERVER_TIMEOUT)
        .map_err(|_| "App Server 응답 시간이 초과되었습니다".to_string());
    let _ = child.kill();
    let _ = child.wait();
    drop(stdin);
    let _ = reader.join();
    answer?
}

fn codex_binary() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("AI_REACTOR_CODEX_BIN").map(PathBuf::from) {
        if path.is_file() {
            return Some(path);
        }
    }

    if let Some(path) = std::env::var_os("PATH") {
        if let Some(found) = std::env::split_paths(&path)
            .map(|dir| dir.join("codex"))
            .find(|candidate| candidate.is_file())
        {
            return Some(found);
        }
    }

    let home = std::env::var_os("HOME").map(PathBuf::from);
    if let Some(home) = home.as_ref() {
        for relative in [
            ".local/bin/codex",
            ".npm-global/bin/codex",
            ".volta/bin/codex",
            ".bun/bin/codex",
        ] {
            let candidate = home.join(relative);
            if candidate.is_file() {
                return Some(candidate);
            }
        }

        let versions = home.join(".nvm/versions/node");
        if let Ok(entries) = fs::read_dir(versions) {
            let mut candidates: Vec<PathBuf> = entries
                .flatten()
                .map(|entry| entry.path().join("bin/codex"))
                .filter(|path| path.is_file())
                .collect();
            candidates.sort_by_key(|path| {
                path.parent()
                    .and_then(Path::parent)
                    .and_then(Path::file_name)
                    .and_then(|name| name.to_str())
                    .map(|version| {
                        version
                            .trim_start_matches('v')
                            .split('.')
                            .filter_map(|part| part.parse::<u64>().ok())
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default()
            });
            if let Some(candidate) = candidates.pop() {
                return Some(candidate);
            }
        }
    }

    ["/opt/homebrew/bin/codex", "/usr/local/bin/codex"]
        .into_iter()
        .map(PathBuf::from)
        .find(|candidate| candidate.is_file())
}

/// The newest limit record found in a file, if any.
#[derive(Debug, Clone)]
pub(crate) struct Record {
    pub(crate) windows: Vec<LimitWindow>,
    pub(crate) captured_at_ms: i64,
    pub(crate) plan_type: Option<String>,
}

/// Parse a rollout file's contents, returning its **last** limit record.
///
/// Last rather than first: the file is append-only, so later lines supersede
/// earlier ones, and a session that ran for an hour holds a hundred updates.
pub(crate) fn parse_rollout(contents: &str) -> Option<Record> {
    let mut newest: Option<Record> = None;

    for line in contents.lines() {
        // Most lines are not limit records, and a rollout can be megabytes, so
        // skip the JSON parse entirely unless the key is present.
        if !line.contains("rate_limits") {
            continue;
        }
        let Ok(parsed) = serde_json::from_str::<LogLine>(line) else {
            continue;
        };
        let Some(limits) = parsed.payload.rate_limits else {
            continue;
        };
        let Some(captured_at_ms) = rfc3339_to_ms(&parsed.timestamp) else {
            continue;
        };

        let windows: Vec<LimitWindow> = [limits.primary, limits.secondary]
            .into_iter()
            .flatten()
            .map(RawWindow::into_window)
            .collect();

        // A record with no windows still counts: it is the newest thing the
        // account said about itself, and "this account has no limits" is an
        // answer. Dropping it would make us report a *older* record's windows
        // as current, which is worse than reporting nothing.
        newest = Some(Record {
            windows,
            captured_at_ms,
            plan_type: limits.plan_type,
        });
    }

    newest
}

fn rfc3339_to_ms(text: &str) -> Option<i64> {
    use time::format_description::well_known::Rfc3339;
    let parsed = time::OffsetDateTime::parse(text, &Rfc3339).ok()?;
    Some(parsed.unix_timestamp() * 1000)
}

/// `~/.codex`, or `None` if we cannot tell where home is.
fn codex_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".codex"))
}

/// The most recent rollout files, newest first.
///
/// # Why this walks newest-first instead of listing everything
///
/// Codex files sessions as `sessions/<year>/<month>/<day>/rollout-*.jsonl`, and
/// someone who has used it daily for two years has thousands of them. We only
/// ever want the newest handful, so listing the whole tree every minute would
/// be work that grows with history and is thrown away each time.
///
/// The date directories are zero-padded, so sorting their names descending is
/// the same as sorting by date. Descending into the newest ones first and
/// stopping as soon as we have enough candidates bounds the walk to a few
/// directories no matter how long the history is.
///
/// Within the collected set we still sort by mtime: a session that began
/// before midnight carries yesterday's date in its path but has the newest
/// contents.
fn rollout_files(sessions_dir: &Path, want: usize) -> Vec<PathBuf> {
    let mut found: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    collect_newest_first(sessions_dir, &mut found, want, 0);
    found.sort_by(|a, b| b.0.cmp(&a.0));
    found.into_iter().map(|(_, path)| path).collect()
}

fn collect_newest_first(
    dir: &Path,
    out: &mut Vec<(std::time::SystemTime, PathBuf)>,
    want: usize,
    depth: usize,
) {
    // year/month/day is three levels; a little slack, but bounded so a symlink
    // loop cannot walk forever.
    if depth > 4 || out.len() >= want {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };

    let mut dirs: Vec<PathBuf> = Vec::new();
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = entry.metadata() else { continue };

        if meta.is_dir() {
            dirs.push(path);
        } else if path.extension().is_some_and(|e| e == "jsonl") {
            files.push((meta.modified().unwrap_or(std::time::UNIX_EPOCH), path));
        }
    }

    // read_dir has no ordering guarantee. A busy day can contain more than
    // `want` rollouts, so taking entries as they arrive may omit the active
    // session forever. Select the newest local files before spending the
    // remaining budget.
    files.sort_by_key(|entry| std::cmp::Reverse(entry.0));
    out.extend(files.into_iter().take(want.saturating_sub(out.len())));

    // Newest date first. Names are zero-padded, so this is a date sort.
    dirs.sort_by(|a, b| b.file_name().cmp(&a.file_name()));
    for child in dirs {
        collect_newest_first(&child, out, want, depth + 1);
        if out.len() >= want {
            return;
        }
    }
}

pub struct CodexUsageProvider {
    root: PathBuf,
    app_server_bin: Option<PathBuf>,
    /// The file we last read, and what we found in it.
    ///
    /// Rollouts are append-only and the newest one is usually unchanged
    /// between polls, so re-reading it every minute is pure waste. Keyed on
    /// (path, mtime, len) because any real append moves at least one of them.
    cache: Mutex<Option<(CacheKey, Option<Record>)>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CacheKey {
    path: PathBuf,
    modified: std::time::SystemTime,
    len: u64,
}

impl CodexUsageProvider {
    pub fn new() -> Self {
        Self {
            root: codex_dir().unwrap_or_else(|| PathBuf::from("/nonexistent")),
            app_server_bin: codex_binary(),
            cache: Mutex::new(None),
        }
    }

    /// Point the adapter at a directory. Tests use this to read fixtures
    /// instead of the real `~/.codex`, which is read-only to us and, on a
    /// machine that has not run Codex in months, has nothing useful in it.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn rooted(root: PathBuf) -> Self {
        Self {
            root,
            // Fixture-backed unit tests exercise the deterministic log path.
            // App Server parsing and discovery have their own tests below.
            app_server_bin: None,
            cache: Mutex::new(None),
        }
    }

    fn sessions_dir(&self) -> PathBuf {
        self.root.join("sessions")
    }

    /// Read one file, using the cache when it has not changed.
    fn read_record(&self, path: &Path) -> Option<Record> {
        let meta = fs::metadata(path).ok()?;
        let key = CacheKey {
            path: path.to_path_buf(),
            modified: meta.modified().unwrap_or(std::time::UNIX_EPOCH),
            len: meta.len(),
        };

        {
            let cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
            if let Some((cached_key, cached)) = cache.as_ref() {
                if *cached_key == key {
                    return cached.clone();
                }
            }
        }

        let found = fs::read_to_string(path)
            .ok()
            .and_then(|contents| parse_rollout(&contents));

        *self.cache.lock().unwrap_or_else(|e| e.into_inner()) = Some((key, found.clone()));
        found
    }

    fn fetch_log(&self) -> Result<Reading, UsageError> {
        let files = rollout_files(&self.sessions_dir(), MAX_FILES_SCANNED);
        if files.is_empty() {
            return Err(UsageError::NoData);
        }

        for path in files.iter() {
            let Some(Record {
                windows,
                captured_at_ms,
                ..
            }) = self.read_record(path)
            else {
                continue;
            };

            if windows.is_empty() {
                return Err(UsageError::NoPlan);
            }

            return Ok(Reading {
                windows,
                captured_at_ms: Some(captured_at_ms),
            });
        }

        Err(UsageError::NoData)
    }
}

impl Default for CodexUsageProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl UsageProvider for CodexUsageProvider {
    fn id(&self) -> ProviderId {
        ProviderId::Codex
    }

    /// Codex counts as set up when its directory exists.
    ///
    /// A cheap existence check on purpose: this runs every cycle and decides
    /// whether to draw a quiet empty card, so it must not cost a file scan.
    /// Being installed says nothing about having data — that distinction is
    /// `fetch`'s job, and conflating them is how an empty card ends up telling
    /// someone to install what they already have.
    fn installed(&self) -> bool {
        self.root.is_dir() || self.app_server_bin.is_some()
    }

    /// The plan named in the newest limit record. No email: Codex only
    /// keeps that in `~/.codex/auth.json`, next to the tokens, and this app
    /// never opens that file.
    fn account(&self) -> super::Account {
        let plan = rollout_files(&self.sessions_dir(), MAX_FILES_SCANNED)
            .iter()
            .find_map(|path| self.read_record(path))
            .and_then(|record| record.plan_type)
            .map(|p| super::anthropic::capitalize(&p));
        super::Account { email: None, plan }
    }

    fn fetch(&self) -> Result<Reading, UsageError> {
        if !self.installed() {
            return Err(UsageError::NotInstalled);
        }

        if let Some(codex) = self.app_server_bin.as_deref() {
            if let Ok(reading) = query_app_server(codex) {
                return Ok(reading);
            }
        }

        self.fetch_log()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const APP_SERVER_LIMITS: &str = r#"{"id":2,"result":{"rateLimits":{"limitId":"codex","primary":{"usedPercent":25.0,"windowDurationMins":300,"resetsAt":1783127522},"secondary":{"usedPercent":40.0,"windowDurationMins":10080,"resetsAt":1783227522}}}}"#;

    /// A record with real windows, as seen on a plan that has limits.
    const WITH_WINDOWS: &str = r#"{"timestamp":"2026-06-08T01:42:38.234Z","type":"event_msg","payload":{"type":"token_count","rate_limits":{"limit_id":"codex","plan_type":"go","primary":{"used_percent":100.0,"window_minutes":43200,"resets_at":1783127522},"secondary":null,"credits":{"has_credits":true,"unlimited":false,"balance":null}}}}"#;

    /// The shape an account with nothing to report produces. Both of these
    /// were observed on the same machine, days apart.
    const NO_PLAN: &str = r#"{"timestamp":"2026-06-10T01:55:18.174Z","type":"event_msg","payload":{"type":"token_count","rate_limits":{"limit_id":"premium","limit_name":null,"primary":null,"secondary":null,"credits":{"has_credits":false,"unlimited":false,"balance":null}}}}"#;

    const UNRELATED: &str = r#"{"timestamp":"2026-06-08T01:40:00.000Z","type":"event_msg","payload":{"type":"agent_message","message":"hello"}}"#;

    #[test]
    fn app_server_limits_are_live_and_flipped_to_remaining() {
        let reading = parse_app_server_line(APP_SERVER_LIMITS)
            .expect("valid response")
            .expect("matching response id");
        assert_eq!(reading.captured_at_ms, None);
        assert_eq!(reading.windows.len(), 2);
        assert_eq!(reading.windows[0].remaining_percent, 75.0);
        assert_eq!(reading.windows[0].window_minutes, 300);
        assert_eq!(reading.windows[1].remaining_percent, 60.0);
    }

    #[test]
    fn app_server_ignores_other_response_ids() {
        let initialized = r#"{"id":1,"result":{"userAgent":"codex"}}"#;
        assert_eq!(parse_app_server_line(initialized).unwrap(), None);
    }

    #[test]
    fn app_server_errors_are_safe_to_fall_back_from() {
        let error = r#"{"id":2,"error":{"code":-32601,"message":"method unavailable"}}"#;
        assert!(parse_app_server_line(error)
            .expect_err("error response")
            .contains("method unavailable"));
    }

    struct Fixture(PathBuf);

    impl Fixture {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir()
                .join(format!("ai-reactor-codex-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(dir.join("sessions/2026/06/08")).expect("fixture dirs");
            Self(dir)
        }

        /// Write a rollout file. Later files are written after, so their
        /// mtimes sort correctly.
        fn rollout(&self, name: &str, lines: &[&str]) -> PathBuf {
            self.rollout_on("2026/06/08", name, lines)
        }

        /// Write a rollout under a specific date directory.
        fn rollout_on(&self, date: &str, name: &str, lines: &[&str]) -> PathBuf {
            let dir = self.0.join("sessions").join(date);
            fs::create_dir_all(&dir).expect("fixture dirs");
            let path = dir.join(format!("rollout-{name}.jsonl"));
            fs::write(&path, lines.join("\n")).expect("write rollout");
            path
        }

        fn provider(&self) -> CodexUsageProvider {
            CodexUsageProvider::rooted(self.0.clone())
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    // ---- parsing ----

    #[test]
    fn the_plan_type_is_kept_with_the_record() {
        let record = parse_rollout(WITH_WINDOWS).unwrap();
        assert_eq!(record.plan_type.as_deref(), Some("go"));
    }

    #[test]
    fn reads_used_percent_as_remaining_and_seconds_as_millis() {
        let record = parse_rollout(WITH_WINDOWS).expect("should find a record");
        assert_eq!(record.windows.len(), 1, "secondary was null");

        let w = &record.windows[0];
        assert_eq!(w.window_minutes, 43_200);
        assert_eq!(w.remaining_percent, 0.0, "100% used is 0% left");
        assert_eq!(w.resets_at_ms, 1_783_127_522_000);
        // 2026-06-08T01:42:38.234Z; sub-second precision is dropped.
        assert_eq!(record.captured_at_ms, 1_780_882_958_000);
    }

    #[test]
    fn both_windows_are_read_when_present() {
        let two = r#"{"timestamp":"2026-06-08T01:42:38.234Z","payload":{"rate_limits":{"primary":{"used_percent":40.0,"window_minutes":300,"resets_at":1783127522},"secondary":{"used_percent":10.0,"window_minutes":10080,"resets_at":1783227522}}}}"#;
        let record = parse_rollout(two).unwrap();
        assert_eq!(record.windows.len(), 2);
        assert_eq!(record.windows[0].remaining_percent, 60.0);
        assert_eq!(record.windows[1].window_minutes, 10_080);
    }

    /// The file is append-only, so the last record wins.
    #[test]
    fn the_last_record_in_a_file_wins() {
        let older = r#"{"timestamp":"2026-06-08T01:00:00.000Z","payload":{"rate_limits":{"primary":{"used_percent":10.0,"window_minutes":300,"resets_at":1783127522},"secondary":null}}}"#;
        let newer = r#"{"timestamp":"2026-06-08T02:00:00.000Z","payload":{"rate_limits":{"primary":{"used_percent":80.0,"window_minutes":300,"resets_at":1783127522},"secondary":null}}}"#;
        let record = parse_rollout(&format!("{older}\n{newer}")).unwrap();
        assert_eq!(record.windows[0].remaining_percent, 20.0, "newer record");
    }

    #[test]
    fn lines_without_limits_are_skipped() {
        assert!(parse_rollout(UNRELATED).is_none());
        let mixed = format!("{UNRELATED}\n{WITH_WINDOWS}\n{UNRELATED}");
        assert!(parse_rollout(&mixed).is_some());
    }

    /// A truncated final line is normal — the CLI may have been killed
    /// mid-write — and must not lose the records before it.
    #[test]
    fn a_corrupt_line_does_not_discard_the_file() {
        let mixed = format!("{WITH_WINDOWS}\n{{\"timestamp\":\"2026-06-09T00:00:00Z\",\"payl");
        let record = parse_rollout(&mixed).expect("earlier record survives");
        assert_eq!(record.windows.len(), 1);
    }

    /// A record whose windows are all null is an answer, not a miss.
    #[test]
    fn a_record_with_no_windows_is_kept_as_empty() {
        let record = parse_rollout(NO_PLAN).expect("should find the record");
        assert!(record.windows.is_empty());
    }

    // ---- the provider ----

    #[test]
    fn a_missing_codex_directory_is_not_installed() {
        let provider = CodexUsageProvider::rooted(PathBuf::from("/nonexistent/codex"));
        assert!(!provider.installed());
        assert_eq!(provider.fetch(), Err(UsageError::NotInstalled));
    }

    /// Installed but never run. Distinct from not installed, because the fix
    /// is different: run it, rather than get it.
    #[test]
    fn an_empty_sessions_directory_is_no_data() {
        let fixture = Fixture::new("nodata");
        let provider = fixture.provider();
        assert!(provider.installed());
        assert_eq!(provider.fetch(), Err(UsageError::NoData));
    }

    #[test]
    fn reads_the_newest_rollout() {
        let fixture = Fixture::new("newest");
        fixture.rollout("a", &[WITH_WINDOWS]);
        std::thread::sleep(std::time::Duration::from_millis(20));
        let newer = r#"{"timestamp":"2026-06-09T00:00:00.000Z","payload":{"rate_limits":{"primary":{"used_percent":25.0,"window_minutes":43200,"resets_at":1783127522},"secondary":null}}}"#;
        fixture.rollout("b", &[newer]);

        let reading = fixture.provider().fetch().expect("should read");
        assert_eq!(reading.windows[0].remaining_percent, 75.0);
        assert_eq!(
            reading.captured_at_ms,
            Some(1_780_963_200_000),
            "the log's own timestamp, not now"
        );
    }

    /// The case this machine is actually in: signed in, no plan.
    #[test]
    fn a_newest_record_without_windows_reports_no_plan() {
        let fixture = Fixture::new("noplan");
        fixture.rollout("a", &[WITH_WINDOWS]);
        std::thread::sleep(std::time::Duration::from_millis(20));
        fixture.rollout("b", &[NO_PLAN]);

        assert_eq!(fixture.provider().fetch(), Err(UsageError::NoPlan));
    }

    /// ...and specifically it must not fall back to the older plan's numbers,
    /// which would show a limit the account no longer has.
    #[test]
    fn no_plan_does_not_fall_back_to_an_older_plans_numbers() {
        let fixture = Fixture::new("nofallback");
        fixture.rollout("old", &[WITH_WINDOWS]);
        std::thread::sleep(std::time::Duration::from_millis(20));
        fixture.rollout("new", &[NO_PLAN]);

        match fixture.provider().fetch() {
            Err(UsageError::NoPlan) => {}
            other => panic!("resurrected a stale plan: {other:?}"),
        }
    }

    /// A session that recorded nothing should not hide an older one that did.
    #[test]
    fn walks_back_past_files_with_no_records() {
        let fixture = Fixture::new("walkback");
        fixture.rollout("has-data", &[WITH_WINDOWS]);
        std::thread::sleep(std::time::Duration::from_millis(20));
        fixture.rollout("empty", &[UNRELATED]);

        let reading = fixture.provider().fetch().expect("should walk back");
        assert_eq!(reading.windows.len(), 1);
    }

    /// The walk must not grow with history. Someone who has used Codex daily
    /// for years has thousands of files, and we only ever want the newest few.
    #[test]
    fn only_the_newest_date_directories_are_walked() {
        let fixture = Fixture::new("bounded");
        // Two years of history, one file a day.
        for month in 1..=12 {
            for day in [1, 15] {
                fixture.rollout_on(
                    &format!("2025/{month:02}/{day:02}"),
                    &format!("old-{month}-{day}"),
                    &[UNRELATED],
                );
            }
        }
        let wanted = 3;
        let files = rollout_files(&fixture.0.join("sessions"), wanted);
        assert!(
            files.len() <= wanted + 2,
            "walked {} files for a budget of {wanted}",
            files.len()
        );
    }

    /// ...and newest-first has to mean newest, not just "whatever we hit
    /// first". Date directories sort by name; files within them by mtime.
    #[test]
    fn the_newest_date_directory_wins() {
        let fixture = Fixture::new("dateorder");
        fixture.rollout_on("2025/01/01", "ancient", &[WITH_WINDOWS]);
        let newer = r#"{"timestamp":"2026-06-09T00:00:00.000Z","payload":{"rate_limits":{"primary":{"used_percent":25.0,"window_minutes":43200,"resets_at":1783127522},"secondary":null}}}"#;
        fixture.rollout_on("2026/06/09", "recent", &[newer]);

        let reading = fixture.provider().fetch().expect("should read");
        assert_eq!(reading.windows[0].remaining_percent, 75.0);
    }

    /// A single busy day may have more rollouts than the scan budget. Directory
    /// iteration order is unspecified, so the budget must be applied only
    /// after sorting those files by modification time.
    #[test]
    fn the_newest_files_win_when_one_day_exceeds_the_budget() {
        let fixture = Fixture::new("busy-day");
        for index in 0..MAX_FILES_SCANNED {
            fixture.rollout(&format!("old-{index:02}"), &[UNRELATED]);
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
        let newest = fixture.rollout("newest", &[WITH_WINDOWS]);

        let files = rollout_files(&fixture.0.join("sessions"), MAX_FILES_SCANNED);
        assert_eq!(files.len(), MAX_FILES_SCANNED);
        assert_eq!(files.first(), Some(&newest));

        fixture
            .provider()
            .fetch()
            .expect("newest rollout should be read");
    }

    /// Rollouts are append-only and mostly unchanged between polls; re-reading
    /// them every minute would be pure waste.
    #[test]
    fn an_unchanged_file_is_not_re_read() {
        let fixture = Fixture::new("cache");
        let path = fixture.rollout("a", &[WITH_WINDOWS]);
        let provider = fixture.provider();

        let first = provider.fetch().expect("first read");

        // Replace the contents *without* touching mtime or length, so only a
        // genuine re-read could notice. The cached answer must come back.
        let same_length = WITH_WINDOWS.replace("100.0", "001.0");
        assert_eq!(same_length.len(), WITH_WINDOWS.len());
        let meta = fs::metadata(&path).unwrap();
        fs::write(&path, &same_length).unwrap();
        filetime_set(&path, meta.modified().unwrap());

        let second = provider.fetch().expect("second read");
        assert_eq!(first, second, "should have answered from cache");
    }

    /// Restore an mtime so the cache key is genuinely unchanged.
    fn filetime_set(path: &Path, when: std::time::SystemTime) {
        use std::os::unix::fs::MetadataExt;
        let _ = fs::metadata(path).map(|m| m.mtime());
        // `utimensat` via libc would be cleaner, but the test only needs the
        // key to match, and `File::set_times` is stable for this.
        if let Ok(file) = fs::OpenOptions::new().write(true).open(path) {
            let times = fs::FileTimes::new().set_modified(when);
            let _ = file.set_times(times);
        }
    }
}
