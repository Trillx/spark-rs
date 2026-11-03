import type { Message } from "../types";

/**
 * How long after it arrives a row offers to copy its email's sign-in code, as long as its notification stands out
 * for (CODE_FRESH_MS in notify.rs): codes last minutes, and older ones would only clutter the list. The open email
 * offers its code whatever its age.
 */
export const CODE_FRESH_MS = 60 * 60 * 1000;

/** The sign-in code a list row offers to copy, if its email has a fresh one. */
export function rowCode(m: Message, now = Date.now()): string | undefined {
  if (!m.signInCode || m.state === "deleted" || m.state === "blocked") return undefined;
  return now - m.timestamp < CODE_FRESH_MS ? m.signInCode : undefined;
}

/** What the toast says once a code is copied, and what Settings → Notifications made copying do. */
export function copiedText(code: string, marked: "read" | "done" | null) {
  if (marked === "read") return `Copied ${code} and marked as read`;
  if (marked === "done") return `Copied ${code} and marked as Done`;
  return `Copied ${code}`;
}
