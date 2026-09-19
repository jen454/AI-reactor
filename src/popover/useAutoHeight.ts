import { useEffect, useRef } from "react";
import { LogicalSize, getCurrentWindow } from "@tauri-apps/api/window";

/** Matches the width declared in `tauri.conf.json`. The spec asks for 340pt. */
const WIDTH = 340;

/**
 * Size the window to its content.
 *
 * The panel shows two gauges in the normal case and a short message otherwise,
 * and those want quite different heights. A fixed window would either clip the
 * tall case or leave a slab of empty frosted glass under the short one.
 *
 * # Why `visible` has to be threaded in here
 *
 * The webview never unmounts when the popover is hidden — it stays alive in
 * the background — so reopening it re-fetches fresh data (see
 * `useUsageReport`), and that data has usually changed in some way that
 * changes rendered height (a caveat line appearing, a status changing) every
 * single time the panel is closed for a while. That means a `setSize` call
 * lands right around the same moment the window is transitioning to visible
 * on essentially every open — and calling `setSize` on a macOS window while
 * it is still settling into its show animation is a known way to make WebKit
 * drop the transparent backing it was given at creation, leaving the popover
 * looking like a plain opaque panel instead of frosted glass.
 *
 * The fix is to never let a resize compete with a show. While hidden, sizing
 * is free (nothing is on screen to glitch) and applied immediately. While
 * visible, it is pushed a couple of frames out with `requestAnimationFrame`,
 * which is enough for the transition to have already settled by the time the
 * native resize actually happens.
 *
 * Returns a ref to attach to the element whose height should drive the window.
 */
export function useAutoHeight<T extends HTMLElement>(visible: boolean) {
  const ref = useRef<T>(null);
  const visibleRef = useRef(visible);
  visibleRef.current = visible;

  useEffect(() => {
    const element = ref.current;
    if (!element) return;

    let lastHeight = 0;
    let cancelled = false;

    const resize = (rounded: number) => {
      if (cancelled) return;
      void getCurrentWindow().setSize(new LogicalSize(WIDTH, rounded));
    };

    const apply = (height: number) => {
      const rounded = Math.ceil(height);
      // Resizing the window changes the layout viewport, which can fire the
      // observer again. Ignoring sub-pixel churn keeps that from turning into
      // a loop that resizes the window forever.
      if (Math.abs(rounded - lastHeight) < 2) return;
      lastHeight = rounded;

      if (!visibleRef.current) {
        // Hidden: nothing is on screen, so there is no show animation to
        // race and no reason to wait.
        resize(rounded);
        return;
      }
      // Visible: let the current frame (and the transition it might be part
      // of) finish before touching window geometry.
      requestAnimationFrame(() => requestAnimationFrame(() => resize(rounded)));
    };

    const observer = new ResizeObserver(([entry]) => {
      apply(entry.target.getBoundingClientRect().height);
    });
    observer.observe(element);
    apply(element.getBoundingClientRect().height);

    return () => {
      cancelled = true;
      observer.disconnect();
    };
  }, []);

  return ref;
}
