/**
 * Mirror of the Rust usage model.
 *
 * Two conventions the Rust side fixes at its boundary, repeated here so the
 * UI never has to second-guess them:
 *
 * 1. Everything is **remaining**, never used.
 * 2. A window is identified by **its length in minutes**, not a name. Claude
 *    has 300 and 10080; this Codex plan has one 43200 and no second. Nothing
 *    may assume a fixed set.
 */

export type ProviderId = "claude" | "codex";

export const PROVIDER_LABEL: Record<ProviderId, string> = {
  claude: "Claude",
  codex: "Codex",
};

/** Who is signed in, and on what plan. Mirror of the Rust `ProviderAccount`. */
export type ProviderAccount = {
  provider: ProviderId;
  /** Absent for Codex: its email lives only next to its tokens. */
  email: string | null;
  /** "Pro", "Max 20x", "Go"… */
  plan: string | null;
};

export type LimitWindow = {
  /** Derived from the length on the Rust side, for grouping and copy. */
  kind: WindowKind;
  /** The window's length, and its identity. */
  windowMinutes: number;
  /** 0–100 remaining. */
  remainingPercent: number;
  /** Milliseconds since the Unix epoch. */
  resetsAtMs: number;
};

export type SnapshotStatus =
  /** Current. */
  | "ok"
  /** A read failed; these are the last real numbers, dimmed. */
  | "stale"
  /** Credential expired. Numbers survive, dimmed; running the CLI fixes it. */
  | "expired"
  /** Not set up here. A quiet invitation, never a spinner. */
  | "notInstalled"
  /** Nothing we can stand behind. `errorReason` says which of the several
   *  quite different causes it was. */
  | "error";

/** What kind of period a window covers, derived from its length. */
export type WindowKind = "session" | "weekly" | "monthly";

/**
 * Why a provider has nothing to show.
 *
 * `status` says *that* there is nothing; this says why. The four answers need
 * four different sentences — telling someone with no Codex plan to go run
 * Codex is useless advice.
 */
export type ErrorReason =
  /** Signed in, but the account has no limits to report. */
  | "noPlan"
  /** Nothing recorded yet. Running the CLI once fixes it. */
  | "noData"
  /** We have a reading, but its window already reset. */
  | "windowReset"
  /** The read failed and there was nothing cached to fall back on. */
  | "readFailed"
  /** The keychain could not be consulted — locked, or the machine is only
   *  half awake. Needs a person, not a retry. */
  | "locked"
  /** The user chose Deny at the keychain prompt. It will be asked again. */
  | "denied";

export type ProviderSnapshot = {
  provider: ProviderId;
  windows: LimitWindow[];
  /** When the numbers were captured. For Codex this can be days ago. */
  capturedAtMs: number;
  status: SnapshotStatus;
  /** Set when there is nothing to show. */
  errorReason: ErrorReason | null;
};

/** Whether a snapshot's numbers may be shown at all. */
export function hasUsableNumbers(status: SnapshotStatus): boolean {
  return status === "ok" || status === "stale" || status === "expired";
}

/**
 * A human name for a window length.
 *
 * Mirrors `LimitWindow::label` on the Rust side: the common durations get the
 * words people use, anything else is derived so an unfamiliar plan renders
 * "12시간" rather than a blank.
 */
export function windowLabel(windowMinutes: number): string {
  switch (windowMinutes) {
    case 300:
      return "5시간";
    case 1440:
      return "일간";
    case 10080:
      return "주간";
    case 43200:
      return "월간";
  }
  if (windowMinutes < 60) return `${windowMinutes}분`;
  if (windowMinutes < 1440) return `${Math.floor(windowMinutes / 60)}시간`;
  return `${Math.floor(windowMinutes / 1440)}일`;
}

/** The window closest to running out — under a remaining model, the smallest. */
export function tightestWindow(
  snapshot: ProviderSnapshot,
): LimitWindow | undefined {
  if (!hasUsableNumbers(snapshot.status)) return undefined;
  return snapshot.windows.reduce<LimitWindow | undefined>(
    (tightest, w) =>
      tightest === undefined || w.remainingPercent < tightest.remainingPercent
        ? w
        : tightest,
    undefined,
  );
}
