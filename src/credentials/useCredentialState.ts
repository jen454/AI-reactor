import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

import type { CredentialState } from "./types";

const POLL_MS = 10_000;

/**
 * Credential state, polled only while the popover is visible.
 *
 * The popover mostly draws usage numbers; this is what it falls back to when
 * there are none, because "no numbers" has several different causes and each
 * one needs different words.
 *
 * Cheap to call: the Rust store answers from an in-memory cache, so this
 * interval does not translate into keychain access.
 */
export function useCredentialState(visible: boolean) {
  const [state, setState] = useState<CredentialState | null>(null);
  const [rechecking, setRechecking] = useState(false);

  /**
   * Explicit user retry. The only path that clears the denial latch, and the
   * only one that may raise the keychain prompt again.
   */
  const recheck = useCallback(async () => {
    setRechecking(true);
    try {
      setState(await invoke<CredentialState>("recheck_credentials"));
    } finally {
      setRechecking(false);
    }
  }, []);

  useEffect(() => {
    if (!visible) return;

    let cancelled = false;
    const tick = async () => {
      if (cancelled) return;
      const next = await invoke<CredentialState>("credential_state");
      if (!cancelled) setState(next);
    };

    void tick();
    const id = setInterval(tick, POLL_MS);
    return () => {
      cancelled = true;
      clearInterval(id);
    };
  }, [visible]);

  return { state, recheck, rechecking };
}
