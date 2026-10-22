import type { Account } from "../types";

const DAY_MS = 24 * 60 * 60 * 1000;

function startOfDay(t: number) {
  const d = new Date(t);
  d.setHours(0, 0, 0, 0);
  return d.getTime();
}

/** Whole calendar days between `t` and now (0 = today, 1 = yesterday). */
function daysAgo(t: number, now = Date.now()) {
  return Math.round((startOfDay(now) - startOfDay(t)) / DAY_MS);
}

const timeFormat = new Intl.DateTimeFormat(undefined, { hour: "numeric", minute: "2-digit" });
const weekdayFormat = new Intl.DateTimeFormat(undefined, { weekday: "short" });
const monthDayFormat = new Intl.DateTimeFormat(undefined, { month: "short", day: "numeric" });
const fullFormat = new Intl.DateTimeFormat(undefined, {
  month: "short",
  day: "numeric",
  hour: "numeric",
  minute: "2-digit",
});

/** List date: the time today, then "Yesterday", the weekday within a week, then "Oct 18". */
export function formatListDate(t: number, now = Date.now()) {
  const days = daysAgo(t, now);
  if (days <= 0) return timeFormat.format(t);
  if (days === 1) return "Yesterday";
  if (days < 7) return weekdayFormat.format(t);
  return monthDayFormat.format(t);
}

/** Thread header and summary dates, e.g. "Oct 20, 9:41 AM" ("20 Oct, 09:41" in en-GB). */
export function formatFullDate(t: number) {
  return fullFormat.format(t);
}

export type Period = "Today" | "Yesterday" | "This week" | "Earlier";

export function datePeriod(t: number, now = Date.now()): Period {
  const days = daysAgo(t, now);
  if (days <= 0) return "Today";
  if (days === 1) return "Yesterday";
  if (days < 7) return "This week";
  return "Earlier";
}

export function initials(name: string) {
  const words = name.split(/\s+/).filter(Boolean);
  const letters = words.length > 1 ? words[0][0] + words[words.length - 1][0] : name.slice(0, 2);
  return letters.toUpperCase();
}

// Full class names so Tailwind's scanner sees them.
const AVATAR_COLORS = [
  "bg-rose-500",
  "bg-emerald-500",
  "bg-violet-500",
  "bg-pink-500",
  "bg-cyan-600",
  "bg-indigo-500",
  "bg-orange-600",
  "bg-teal-600",
  "bg-green-700",
  "bg-fuchsia-600",
];

export function avatarColor(name: string) {
  let hash = 0;
  for (const ch of name) hash = (hash * 31 + ch.charCodeAt(0)) >>> 0;
  return AVATAR_COLORS[hash % AVATAR_COLORS.length];
}

export const ACCOUNTS: Record<
  Account,
  { name: string; provider: string; email: string; ring: string; dot: string }
> = {
  personal: {
    name: "Personal",
    provider: "Gmail",
    email: "me@gmail.com",
    ring: "border-sky-500",
    dot: "bg-sky-500",
  },
  work: {
    name: "Work",
    provider: "Outlook",
    email: "me@outlook.com",
    ring: "border-amber-500",
    dot: "bg-amber-500",
  },
};

export const ACCOUNT_IDS: Account[] = ["personal", "work"];

/** Snooze, send-later and reminder presets. */
export const LATER_PRESETS = [
  { label: "Later today", detail: "6:00 PM" },
  { label: "Tomorrow", detail: "9:00 AM" },
  { label: "This weekend", detail: "Sat 9:00 AM" },
  { label: "Next week", detail: "Mon 9:00 AM" },
] as const;

export type LaterPreset = (typeof LATER_PRESETS)[number];

export function presetText(p: LaterPreset) {
  return `${p.label} · ${p.detail}`;
}

export function plural(n: number, one: string, many = `${one}s`) {
  return `${n} ${n === 1 ? one : many}`;
}
