import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

import type { ProviderAccount } from "./types";

/**
 * Each provider's signed-in account and plan, re-read every time the panel
 * opens. Accounts change rarely, but a re-login should show up the next time
 * someone looks, not after a restart — and the read is local files only.
 */
export function useAccounts(visible: boolean): ProviderAccount[] {
  const [accounts, setAccounts] = useState<ProviderAccount[]>([]);

  useEffect(() => {
    if (!visible) return;
    let cancelled = false;
    void invoke<ProviderAccount[]>("account_info").then((next) => {
      if (!cancelled) setAccounts(next);
    });
    return () => {
      cancelled = true;
    };
  }, [visible]);

  return accounts;
}
