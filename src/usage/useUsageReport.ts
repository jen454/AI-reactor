import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

import type { ProviderSnapshot } from "./types";

/** Mirror of the Rust `UsageReport`. */
export type UsageReport = {
  providers: ProviderSnapshot[];
  /** When the last automatic health check ran. `null` before the first. */
  lastCheckedAtMs: number | null;
  /** How often that check runs while this panel is open. */
  checkIntervalMs: number;
};

/** How often the panel re-reads the cached report and re-renders countdowns. */
const READ_MS = 10_000;

/**
 * The current report, plus a clock.
 *
 * This never triggers a read of its own. `usage_report` returns what the
 * background health check last found, so opening the popover during a
 * rate-limit backoff cannot punch through it — the backoff would be pointless
 * if the UI could bypass it.
 *
 * `now` comes back alongside because every countdown and every prediction is
 * derived from it. One clock in one place means the gauges never disagree by a
 * render, and there is one timer rather than one per countdown.
 *
 * `null` means "not read yet" — the loading state, which the spec insists must
 * look different from an empty provider.
 */
export function useUsageReport(visible: boolean) {
  const [report, setReport] = useState<UsageReport | null>(null);
  const [now, setNow] = useState(() => Date.now());
  const [refreshing, setRefreshing] = useState(false);

  /**
   * Read every provider now.
   *
   * Goes through the poller, so each provider's backoff still applies — the
   * button cannot punch through a rate-limit wait, it can only skip the idle
   * part of the cycle.
   */
  const refresh = useCallback(async () => {
    setRefreshing(true);
    try {
      setNow(Date.now());
      setReport(await invoke<UsageReport>("refresh_usage"));
    } finally {
      setRefreshing(false);
    }
  }, []);

  useEffect(() => {
    if (!visible) return;

    let cancelled = false;
    const tick = async () => {
      if (cancelled) return;
      setNow(Date.now());
      const next = await invoke<UsageReport>("usage_report");
      if (!cancelled) setReport(next);
    };

    void tick();
    const id = setInterval(tick, READ_MS);
    return () => {
      cancelled = true;
      clearInterval(id);
    };
  }, [visible]);

  return { report, now, refresh, refreshing };
}
