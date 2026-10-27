import type { SummaryStyle } from "../ai";

/**
 * Spark's "Open Thread in a new Window": the conversation this window is for, or null in the
 * app's main window. The desktop shell says which before the page runs (`thread_window.rs`); in
 * a browser, `?thread=<id>`.
 */
export const THREAD_WINDOW: number | null = (() => {
  const given = (window as { __sparkThread?: unknown }).__sparkThread;
  if (typeof given === "number" && Number.isInteger(given)) return given;
  const query = new URLSearchParams(window.location.search).get("thread");
  return query && /^\d+$/.test(query) ? Number(query) : null;
})();

/** What this window was opened to do as well: an +AI summary in that style ("short", …), if any. */
export const THREAD_THEN: string | null = (() => {
  const given = (window as { __sparkThreadThen?: unknown }).__sparkThreadThen;
  if (typeof given === "string") return given;
  return new URLSearchParams(window.location.search).get("then");
})();

/** The summary style a window was asked for, if it's one of Spark's. */
export function summaryStyle(value: string | null | undefined): SummaryStyle | null {
  return value === "short" || value === "detailed" || value === "actionPoints" ? value : null;
}

const POP_OUT_KEY = "spark-rs.threadWindows";

/** Spark's "Always open Thread Viewer in new window" (for the one-column layout). Off unless turned on. */
export function loadPopOut() {
  return localStorage.getItem(POP_OUT_KEY) === "on";
}

export function savePopOut(on: boolean) {
  localStorage.setItem(POP_OUT_KEY, on ? "on" : "off");
}
