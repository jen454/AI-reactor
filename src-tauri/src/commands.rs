//! The webview's entire view of the Rust side.
//!
//! Everything here returns plain data. The access token cannot appear in any
//! of it even by mistake: `AccessToken` has no `Serialize` impl, so a command
//! that tried to hand it over would fail to compile.

use std::sync::Arc;

use tauri::State;

use crate::credentials::{store::CredentialStore, CredentialState};
use serde::Serialize;

use crate::usage::{poller::UsageRegistry, schedule, ProviderAccount, ProviderSnapshot};

/// Cheap: answers from cache in the steady state, so the popover can call it
/// on its polling interval without provoking a keychain prompt.
#[tauri::command]
pub fn credential_state(store: State<'_, Arc<CredentialStore>>) -> CredentialState {
    store.state()
}

/// Expensive and user-initiated: drops the cache, clears the denial latch, and
/// reads through — which may raise the system permission dialog. Wired to the
/// popover's retry button, never to a timer.
#[tauri::command]
pub fn recheck_credentials(store: State<'_, Arc<CredentialStore>>) -> CredentialState {
    store.recheck()
}

/// What the popover draws.
///
/// One call rather than two, so the snapshots and the "last checked" line can
/// never disagree by a render.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageReport {
    /// Every provider, in a stable order. Uninstalled ones are included on
    /// purpose: the popover shows them as quiet invitations, and only the menu
    /// bar filters them out.
    pub providers: Vec<ProviderSnapshot>,
    /// When the last automatic health check ran. `None` before the first.
    pub last_checked_at_ms: Option<i64>,
    /// How often the check runs while the panel is open, so the footer can
    /// state the cadence instead of guessing at it.
    pub check_interval_ms: u64,
}

/// Read every provider now.
///
/// The spec asks for a refresh button, and this is it — but it goes through
/// the poller rather than around it. Each provider's own backoff still
/// applies, so a button press during a rate limit is a no-op for that provider
/// instead of a way to punch through the wait. A manual escape hatch that can
/// make things worse is not an escape hatch.
#[tauri::command]
pub fn refresh_usage(registry: State<'_, Arc<UsageRegistry>>) -> UsageReport {
    registry.poll_all();
    usage_report(registry)
}

/// Which account each provider is signed in with, and on what plan.
///
/// Local files only — no network, no keychain — so the popover can call it
/// every time it opens without cost or prompts.
#[tauri::command]
pub fn account_info(registry: State<'_, Arc<UsageRegistry>>) -> Vec<ProviderAccount> {
    registry.accounts()
}

/// The current report.
///
/// Deliberately does *not* read anything. The popover must never be the thing
/// that decides to hit the network — the poller owns that, backoff and all —
/// or opening the panel during a rate limit would punch straight through it.
#[tauri::command]
pub fn usage_report(registry: State<'_, Arc<UsageRegistry>>) -> UsageReport {
    UsageReport {
        providers: registry.snapshots(),
        last_checked_at_ms: registry.last_checked_at_ms(),
        check_interval_ms: schedule::ACTIVE_INTERVAL.as_millis() as u64,
    }
}
