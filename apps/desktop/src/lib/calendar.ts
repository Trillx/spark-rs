import type { CalendarEvent } from "../types";

// Calendar dates and layout. Days are local midnights (Unix ms). Timed events
// are instants shown in local time; all-day events carry their days as UTC
// midnights of the dates as written, so they land on the same dates anywhere.

export const DAY_MS = 24 * 60 * 60 * 1000;
export const MINUTES_PER_DAY = 24 * 60;

export type CalendarKind = "day" | "week" | "month";

const VIEW_KEY = "spark.calendarView";
const HIDDEN_KEY = "spark.calendarHidden";
const SIDE_KEY = "spark.calendarSidePanel";

export function loadKind(): CalendarKind {
  const saved = localStorage.getItem(VIEW_KEY);
  return saved === "day" || saved === "month" ? saved : "week";
}

export function saveKind(kind: CalendarKind) {
  localStorage.setItem(VIEW_KEY, kind);
}

/** Calendars the user unticked in the Calendars menu, by id. */
export function loadHidden(): Set<number> {
  try {
    const ids: unknown = JSON.parse(localStorage.getItem(HIDDEN_KEY) ?? "[]");
    return new Set(Array.isArray(ids) ? ids.filter((id): id is number => typeof id === "number") : []);
  } catch {
    return new Set();
  }
}

export function saveHidden(hidden: ReadonlySet<number>) {
  localStorage.setItem(HIDDEN_KEY, JSON.stringify([...hidden]));
}

/** The side panel's open state; `null` until the user toggles it. */
export function loadSidePanel(): boolean | null {
  const saved = localStorage.getItem(SIDE_KEY);
  return saved === null ? null : saved === "1";
}

export function saveSidePanel(open: boolean) {
  localStorage.setItem(SIDE_KEY, open ? "1" : "0");
}

/** Local midnight of the day holding `t`. */
export function startOfDay(t: number) {
  const d = new Date(t);
  d.setHours(0, 0, 0, 0);
  return d.getTime();
}

/** Local midnight `n` days after the day holding `t`; days around a clock change aren't 24 hours. */
export function addDays(t: number, n: number) {
  const d = new Date(t);
  d.setDate(d.getDate() + n);
  d.setHours(0, 0, 0, 0);
  return d.getTime();
}

/** The first of the month `n` months after the one holding `t`. */
export function addMonths(t: number, n: number) {
  const d = new Date(t);
  d.setHours(0, 0, 0, 0);
  d.setDate(1);
  d.setMonth(d.getMonth() + n);
  return d.getTime();
}

export function startOfMonth(t: number) {
  return addMonths(t, 0);
}

export function sameDay(a: number, b: number) {
  return startOfDay(a) === startOfDay(b);
}

/** The locale's first day of the week, 0 for Sunday as in `Date.getDay`. */
export function firstDayOfWeek(): number {
  try {
    const locale = new Intl.Locale(navigator.language) as Intl.Locale & {
      weekInfo?: { firstDay: number };
      getWeekInfo?: () => { firstDay: number };
    };
    const info = locale.getWeekInfo?.() ?? locale.weekInfo;
    if (info?.firstDay) return info.firstDay % 7;
  } catch {
    // An unknown language tag: fall through.
  }
  return 0;
}

export function startOfWeek(t: number) {
  const day = startOfDay(t);
  return addDays(day, -((new Date(day).getDay() - firstDayOfWeek() + 7) % 7));
}

/** The days a view shows: one, seven, or the whole weeks around the month. */
export function viewDays(kind: CalendarKind, anchor: number): number[] {
  if (kind === "day") return [startOfDay(anchor)];
  if (kind === "week") return Array.from({ length: 7 }, (_, i) => addDays(startOfWeek(anchor), i));
  const first = startOfMonth(anchor);
  const start = startOfWeek(first);
  const last = addDays(addMonths(first, 1), -1);
  const days: number[] = [];
  for (let day = start; day <= last || days.length % 7 !== 0; day = addDays(day, 1)) days.push(day);
  return days;
}

/** The anchor one page before or after. */
export function stepAnchor(kind: CalendarKind, anchor: number, n: number) {
  if (kind === "day") return addDays(anchor, n);
  if (kind === "week") return addDays(anchor, 7 * n);
  return addMonths(anchor, n);
}

/** The local midnight of an all-day event's date (given as that date's UTC midnight). */
export function localDate(utcMidnight: number) {
  const d = new Date(utcMidnight);
  return new Date(d.getUTCFullYear(), d.getUTCMonth(), d.getUTCDate()).getTime();
}

/** The UTC midnight of a local day's date, as all-day events carry it. */
export function utcDate(localMidnight: number) {
  const d = new Date(localMidnight);
  return Date.UTC(d.getFullYear(), d.getMonth(), d.getDate());
}

/** The first and last local days an event covers. */
export function eventDays(e: CalendarEvent): { first: number; last: number } {
  if (e.allDay) {
    const first = localDate(e.start);
    return { first, last: e.end > e.start ? localDate(e.end - 1) : first };
  }
  const first = startOfDay(e.start);
  return { first, last: e.end > e.start ? startOfDay(e.end - 1) : first };
}

/** All-day events, and timed ones of a day or more, go in the all-day row. */
export function inAllDayRow(e: CalendarEvent) {
  return e.allDay || e.end - e.start >= DAY_MS;
}

/** The events on local day `day`. */
export function onDay(events: CalendarEvent[], day: number) {
  return events.filter((e) => {
    const { first, last } = eventDays(e);
    return first <= day && day <= last;
  });
}

/** Earlier first; all-day before timed; longer first. */
export function compareEvents(a: CalendarEvent, b: CalendarEvent) {
  const da = eventDays(a);
  const db = eventDays(b);
  if (da.first !== db.first) return da.first - db.first;
  if (a.allDay !== b.allDay) return a.allDay ? -1 : 1;
  if (a.start !== b.start) return a.start - b.start;
  return b.end - b.start - (a.end - a.start);
}

/** Over, or past: tinted lighter. */
export function isPast(e: CalendarEvent, now: number) {
  return e.allDay ? localDate(e.end) <= startOfDay(now) : e.end <= now;
}

/** Declined or canceled: struck through. */
export function isOff(e: CalendarEvent) {
  return e.canceled || e.response === "declined";
}

export interface Placed<T> {
  item: T;
  /** Minutes into the day. */
  top: number;
  bottom: number;
  column: number;
  /** Columns it takes, from `column` on. */
  span: number;
  /** Columns in its cluster. */
  columns: number;
}

/**
 * Side by side layout for one day's timed events, in minutes into the day.
 * Overlapping events form a cluster whose events share its width: each goes
 * into the first column free at its start, then widens over the columns to
 * its right that stay free while it lasts. Events shorter than `minMinutes`
 * count as that long, since they're drawn that tall.
 */
export function layoutDay<T>(items: { item: T; start: number; end: number }[], minMinutes = 20): Placed<T>[] {
  const sorted = items
    .map((i) => ({ ...i, bottom: Math.max(i.end, i.start + minMinutes) }))
    .sort((a, b) => a.start - b.start || b.bottom - a.bottom);
  const placed: Placed<T>[] = [];
  let cluster: (Placed<T> & { end: number })[] = [];
  let columnEnds: number[] = [];
  let clusterEnd = -Infinity;

  const finish = () => {
    const columns = columnEnds.length;
    for (const p of cluster) {
      let span = 1;
      while (
        p.column + span < columns &&
        !cluster.some((o) => o.column === p.column + span && o.top < p.end && p.top < o.end)
      ) {
        span++;
      }
      placed.push({ item: p.item, top: p.top, bottom: p.bottom, column: p.column, span, columns });
    }
    cluster = [];
    columnEnds = [];
  };

  for (const i of sorted) {
    if (i.start >= clusterEnd) finish();
    let column = columnEnds.findIndex((end) => end <= i.start);
    if (column < 0) {
      column = columnEnds.length;
      columnEnds.push(i.bottom);
    } else {
      columnEnds[column] = i.bottom;
    }
    cluster.push({ item: i.item, top: i.start, bottom: i.end, end: i.bottom, column, span: 1, columns: 0 });
    clusterEnd = Math.max(clusterEnd, i.bottom);
  }
  finish();
  return placed;
}

/**
 * Rows for bars that span columns `[from, to]` (the all-day row): each takes
 * the first row free over all its columns. Returns the row of each, in order.
 */
export function laneLayout(spans: { from: number; to: number }[]): number[] {
  const rows: boolean[][] = [];
  return spans.map(({ from, to }) => {
    let row = rows.findIndex((taken) => !taken.slice(from, to + 1).some(Boolean));
    if (row < 0) {
      row = rows.length;
      rows.push([]);
    }
    for (let c = from; c <= to; c++) rows[row][c] = true;
    return row;
  });
}

const timeFormat = new Intl.DateTimeFormat(undefined, { hour: "numeric", minute: "2-digit" });
const utcDayFormat = new Intl.DateTimeFormat(undefined, { weekday: "short", month: "short", day: "numeric", timeZone: "UTC" });
const dayFormat = new Intl.DateTimeFormat(undefined, { weekday: "short", month: "short", day: "numeric" });

export function formatTime(t: number) {
  return timeFormat.format(t);
}

/** "9:30 – 10:15 AM", with days when it ends on another one; "All day", with the days if several. */
export function eventTime(e: CalendarEvent) {
  if (e.allDay) {
    const last = e.end > e.start ? e.end - DAY_MS : e.start;
    return last > e.start ? `${utcDayFormat.format(e.start)} – ${utcDayFormat.format(last)}` : "All day";
  }
  if (sameDay(e.start, Math.max(e.start, e.end - 1))) return timeFormat.formatRange(e.start, e.end);
  return `${dayFormat.format(e.start)}, ${timeFormat.format(e.start)} – ${dayFormat.format(e.end)}, ${timeFormat.format(e.end)}`;
}

/** "Fri, Oct 24" for the day an event starts. */
export function eventDate(e: CalendarEvent) {
  return e.allDay ? utcDayFormat.format(e.start) : dayFormat.format(e.start);
}

/** "in 25 min", "in 2 h 5 min", "now". */
export function countdown(t: number, now: number) {
  const minutes = Math.ceil((t - now) / 60_000);
  if (minutes <= 0) return "now";
  if (minutes < 60) return `in ${minutes} min`;
  const h = Math.floor(minutes / 60);
  const m = minutes % 60;
  return m ? `in ${h} h ${m} min` : `in ${h} h`;
}

/**
 * The meeting pop-up's time, as Spark words it: "in 45 seconds", "in 1 minute",
 * "27 minutes left", "1 hour 5 minutes left". Minutes round up.
 */
export function timeLeft(start: number, end: number, now: number) {
  return now < start ? `in ${duration(start - now)}` : `${duration(end - now)} left`;
}

function duration(ms: number) {
  const seconds = Math.max(0, Math.ceil(ms / 1000));
  if (seconds < 60) return plural(seconds, "second");
  const minutes = Math.ceil(seconds / 60);
  const h = Math.floor(minutes / 60);
  const m = minutes % 60;
  if (!h) return plural(m, "minute");
  return m ? `${plural(h, "hour")} ${plural(m, "minute")}` : plural(h, "hour");
}

function plural(n: number, unit: string) {
  return `${n} ${unit}${n === 1 ? "" : "s"}`;
}

/** The next meeting to show in the side panel: timed, not declined or canceled, not over, today or tomorrow. */
export function nextMeeting(events: CalendarEvent[], now: number) {
  const until = addDays(now, 2);
  return events
    .filter((e) => !e.allDay && !isOff(e) && e.end > now && e.start < until && e.end - e.start < DAY_MS)
    .sort((a, b) => a.start - b.start)[0];
}

/** `color` at `percent` opacity, for tints. */
export function tint(color: string, percent: number) {
  return `color-mix(in srgb, ${color} ${percent}%, transparent)`;
}
