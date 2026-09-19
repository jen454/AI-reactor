import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";

/** Must match `popover::VISIBILITY_EVENT` on the Rust side. */
const VISIBILITY_EVENT = "popover-visibility";

/**
 * Whether the popover is actually on screen.
 *
 * Hiding a Tauri window does not unmount the webview: React stays mounted and
 * timers keep firing. Everything in this panel that polls is gated on this, so
 * a hidden popover costs nothing.
 */
export function usePopoverVisibility() {
  // The window is created hidden, so "not visible" is the correct start.
  const [visible, setVisible] = useState(false);

  useEffect(() => {
    // `listen` resolves to an unlisten function. The component can unmount
    // before that promise settles, so track it and unlisten on arrival.
    let unlisten: (() => void) | undefined;
    let cancelled = false;

    void listen<boolean>(VISIBILITY_EVENT, (event) => {
      setVisible(event.payload);
    }).then((fn) => {
      if (cancelled) fn();
      else unlisten = fn;
    });

    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  return visible;
}
