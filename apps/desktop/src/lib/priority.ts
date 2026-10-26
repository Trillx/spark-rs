// Settings → Priority: how many Priority emails the Inbox shows before
// "Show all", as Spark's "Priority emails in Inbox". It's a view setting,
// so it lives in localStorage like the theme.

const LINES_KEY = "spark-rs:priority-lines";

/** Rows the Priority card shows by default; 0 shows them all. */
export const PRIORITY_LINES = 3;

/** The choices: every Priority email, or 1 to 10 of them. */
export const PRIORITY_LINE_CHOICES = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10];

export function loadPriorityLines(): number {
  const saved = Number(localStorage.getItem(LINES_KEY) ?? PRIORITY_LINES);
  return PRIORITY_LINE_CHOICES.includes(saved) ? saved : PRIORITY_LINES;
}

export function savePriorityLines(lines: number) {
  localStorage.setItem(LINES_KEY, String(lines));
}
