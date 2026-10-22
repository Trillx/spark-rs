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

/** Snooze, send-later and reminder presets. */
export const LATER_PRESETS = [
  { key: "laterToday", label: "Later today" },
  { key: "tomorrow", label: "Tomorrow" },
  { key: "weekend", label: "This weekend" },
  { key: "nextWeek", label: "Next week" },
] as const;

export type LaterPreset = (typeof LATER_PRESETS)[number];

/** A picked preset, with its time fixed when it was picked. */
export interface Later {
  key: LaterPreset["key"];
  at: number;
}

const HOUR_MS = 60 * 60 * 1000;

/** `days` from `t` at `hour`:00 local time. */
function dayAt(t: number, days: number, hour: number) {
  const d = new Date(t);
  d.setDate(d.getDate() + days);
  d.setHours(hour, 0, 0, 0);
  return d.getTime();
}

/**
 * When a preset comes due: Later today is 6 PM, or 3 hours from now once that
 * has passed; Tomorrow is 9 AM; This weekend the next Saturday and Next week
 * the next Monday, both at 9 AM.
 */
export function laterTime(p: LaterPreset, now = Date.now()) {
  const weekday = new Date(now).getDay();
  switch (p.key) {
    case "laterToday": {
      const evening = dayAt(now, 0, 18);
      return evening > now ? evening : Math.ceil((now + 3 * HOUR_MS) / 60_000) * 60_000;
    }
    case "tomorrow":
      return dayAt(now, 1, 9);
    case "weekend":
      return dayAt(now, (6 - weekday + 7) % 7 || 7, 9);
    case "nextWeek":
      return dayAt(now, (1 - weekday + 7) % 7 || 7, 9);
  }
}

export function pickLater(p: LaterPreset): Later {
  return { key: p.key, at: laterTime(p) };
}

/** The menu's right column: "6:00 PM", or "Sat 9:00 AM" past tomorrow. */
export function presetDetail(p: LaterPreset, now = Date.now()) {
  const t = laterTime(p, now);
  const days = -daysAgo(t, now);
  if (days === 0 || p.key === "tomorrow") return timeFormat.format(t);
  return `${weekdayFormat.format(t)} ${timeFormat.format(t)}`;
}

/** A future time: "Today, 6:00 PM", "Tomorrow, 9:00 AM", "Sat, 9:00 AM", then "Oct 30, 9:00 AM". */
export function formatLater(t: number, now = Date.now()) {
  const days = -daysAgo(t, now);
  const time = timeFormat.format(t);
  if (days === 0) return `Today, ${time}`;
  if (days === 1) return `Tomorrow, ${time}`;
  if (days > 1 && days < 7) return `${weekdayFormat.format(t)}, ${time}`;
  return fullFormat.format(t);
}

/** The list row's short form: the time today, then "Tomorrow", a weekday, "Oct 30". */
export function formatLaterShort(t: number, now = Date.now()) {
  const days = -daysAgo(t, now);
  if (days <= 0) return timeFormat.format(t);
  if (days === 1) return "Tomorrow";
  if (days < 7) return weekdayFormat.format(t);
  return monthDayFormat.format(t);
}

/** "just now", "5 min ago", "3 hours ago", then the date. */
export function formatAgo(t: number, now = Date.now()) {
  const minutes = Math.floor((now - t) / 60_000);
  if (minutes < 1) return "just now";
  if (minutes < 60) return `${minutes} min ago`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${plural(hours, "hour")} ago`;
  return fullFormat.format(t);
}

/** "Ana Ruiz <ana@x.com>" gives both parts; a bare address is its own name. */
export function parseAddress(text: string) {
  const match = /^\s*"?([^"<]*?)"?\s*<([^>]*)>\s*$/.exec(text);
  if (!match) return { name: text.trim(), address: text.trim() };
  const address = match[2].trim();
  return { name: match[1].trim() || address, address };
}

export function plural(n: number, one: string, many = `${one}s`) {
  return `${n} ${n === 1 ? one : many}`;
}
