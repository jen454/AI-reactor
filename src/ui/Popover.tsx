import { useAutoHeight } from "../popover/useAutoHeight";
import { usePopoverVisibility } from "../popover/useVisibility";
import { useAccounts } from "../usage/useAccounts";
import { useUsageReport } from "../usage/useUsageReport";
import { HealthDot } from "./HealthDot";
import { ProviderCard } from "./ProviderCard";

/** Providers we expect, so the loading skeleton has the right shape. */
const EXPECTED_PROVIDERS = 2;

/**
 * The panel that drops down from the menu bar icon, 340pt wide.
 *
 * Top to bottom: the monkey (milestone 6), a card per provider, and nothing
 * else. There is no refresh button — the app health-checks itself and the dot
 * in the header says so — and no settings row yet.
 *
 * Everything is gated on `visible`. A hidden Tauri window still runs its
 * webview, so without that gate the panel would poll all day for nobody.
 */
/**
 * A small vector echo of the menu bar glyph — a segmented ring around a lit
 * core — so the name "AI reactor" has a mark to go with it instead of just
 * being a word. Static rather than live: the tray icon and the status item's
 * own title text are already where the real-time number lives, so this one
 * stays a plain brand mark. Our own geometry (stroke-dasharray notches, not a
 * literal prop), same as the tray icon in `art.rs`.
 */
function ArcReactorMark() {
  return (
    <svg
      className="popover__mark"
      viewBox="0 0 24 24"
      width="16"
      height="16"
      aria-hidden="true"
    >
      <circle
        cx="12"
        cy="12"
        r="8.5"
        fill="none"
        stroke="var(--pc-accent)"
        strokeWidth="3"
        strokeDasharray="5.2 2.4"
      />
      <circle cx="12" cy="12" r="3" fill="var(--pc-accent)" />
    </svg>
  );
}

export function Popover() {
  const visible = usePopoverVisibility();
  const { report, now, refresh, refreshing } = useUsageReport(visible);
  const accounts = useAccounts(visible);
  const cardRef = useAutoHeight<HTMLElement>(visible);

  return (
    <main className="popover" ref={cardRef}>
      <header className="popover__header">
        <span className="popover__brand">
          <ArcReactorMark />
          <span className="popover__title">AI reactor</span>
        </span>
        <HealthDot
          lastCheckedAtMs={report?.lastCheckedAtMs ?? null}
          checkIntervalMs={report?.checkIntervalMs ?? 60_000}
          now={now}
        />
      </header>

      {/* Milestone 6: the monkey goes here, above the numbers. This is the
          place you meet the character; the menu bar is the instrument panel. */}

      {report === null ? (
        // Loading is its own state on purpose. A skeleton says "data is
        // coming"; an empty card says "there is none". Using one for the other
        // leaves people waiting for something that will never arrive.
        <div className="popover__cards" aria-busy="true" aria-label="불러오는 중">
          {Array.from({ length: EXPECTED_PROVIDERS }, (_, i) => (
            <div key={i} className="skeleton" />
          ))}
        </div>
      ) : (
        <div className="popover__cards">
          {report.providers.map((snapshot) => (
            <ProviderCard
              key={snapshot.provider}
              snapshot={snapshot}
              now={now}
              account={accounts.find((a) => a.provider === snapshot.provider)}
            />
          ))}
        </div>
      )}
      <footer className="popover__footer">
        {/* The check runs on its own — the dot in the header says so — and
            this is the escape hatch for when you do not want to wait for the
            next one. It goes through the poller, so it cannot jump a
            rate-limit backoff. */}
        <button
          className="popover__action"
          onClick={refresh}
          disabled={refreshing || report === null}
        >
          {refreshing ? "확인 중…" : "새로고침"}
        </button>
        {/* Milestone 6: icon pack selection lands here. */}
      </footer>
    </main>
  );
}
