import type { CalendarEvent, EventDraft } from "../types";

// The event editor's draft: what a new event starts as, an event as the editor
// shows it, and the rules Spark's editor keeps as its times change. Timed
// events are instants; all-day events carry their days as UTC midnights of the
// dates (see lib/calendar). Values here don't come from sibling modules, so
// Node's test runner can load it.

const MINUTE = 60_000;
const DAY = 24 * 60 * MINUTE;

/** Every quarter hour of a day, as minutes after midnight: the times the editor lists. */
export const QUARTERS = Array.from({ length: 96 }, (_, i) => i * 15);

/** The local midnight of `t`'s day. */
export function dayOf(t: number) {
  const d = new Date(t);
  d.setHours(0, 0, 0, 0);
  return d.getTime();
}

/** `minutes` after the local midnight `day`, on the clock (a DST change in between doesn't shift it). */
export function atMinutes(day: number, minutes: number) {
  const d = new Date(day);
  d.setHours(Math.floor(minutes / 60), minutes % 60, 0, 0);
  return d.getTime();
}

/** The minutes after midnight `t` is, on the clock. */
export function minutesOf(t: number) {
  const d = new Date(t);
  return d.getHours() * 60 + d.getMinutes();
}

/** The UTC midnight of a local day's date, as all-day events carry it. */
export function toUtcDate(localMidnight: number) {
  const d = new Date(localMidnight);
  return Date.UTC(d.getFullYear(), d.getMonth(), d.getDate());
}

/** The local midnight of an all-day event's date (given as that date's UTC midnight). */
export function fromUtcDate(utcMidnight: number) {
  const d = new Date(utcMidnight);
  return new Date(d.getUTCFullYear(), d.getUTCMonth(), d.getUTCDate()).getTime();
}

/** One local day after `day` (a local midnight), whatever the clocks did. */
function nextDay(day: number) {
  const d = new Date(day);
  d.setDate(d.getDate() + 1);
  return d.getTime();
}

/** An event as the editor shows it. */
export function draftOf(e: CalendarEvent): EventDraft {
  return {
    calendar: e.calendar,
    title: e.title,
    start: e.start,
    end: e.end,
    allDay: e.allDay,
    location: e.location ?? "",
    notes: e.notes ?? "",
    attendees: e.attendees.map(({ name, address }) => ({ name, address })),
    reminder: e.reminder ?? null,
    showAs: e.showAs,
    teams: e.onlineMeeting,
  };
}

/** What new events get: Settings → Calendar's length and alerts. */
export interface NewEventDefaults {
  minutes: number;
  alert: number | null;
  allDayAlert: number | null;
}

/** A new event in `calendar` from `start` (all day: the UTC midnight of its date), with the defaults. */
export function newDraft(calendar: number, start: number, allDay: boolean, defaults: NewEventDefaults): EventDraft {
  return {
    calendar,
    title: "",
    start,
    end: allDay ? start + DAY : start + defaults.minutes * MINUTE,
    allDay,
    location: "",
    notes: "",
    attendees: [],
    reminder: allDay ? defaults.allDayAlert : defaults.alert,
    showAs: allDay ? "free" : "busy",
    teams: false,
  };
}

/** Where New Event starts: the next quarter hour, when `day` is today; else 9:00 on `day`. */
export function newEventStart(now: number, day: number) {
  if (dayOf(now) !== day) return atMinutes(day, 9 * 60);
  return Math.floor(now / (15 * MINUTE)) * 15 * MINUTE + 15 * MINUTE;
}

/** The start moved, the length kept. */
export function withStart(d: EventDraft, start: number): EventDraft {
  return { ...d, start, end: start + (d.end - d.start) };
}

/**
 * The end changed. A timed event's end before its start moves to the next day, as Spark's
 * editor does; an all-day event ends no earlier than the day it starts.
 */
export function withEnd(d: EventDraft, end: number): EventDraft {
  if (d.allDay) return { ...d, end: Math.max(end, d.start + DAY) };
  let next = end;
  while (next <= d.start) next += DAY;
  return { ...d, end: next };
}

/**
 * All day on or off. On, the event covers its days, with the all-day alert; off, it gets
 * `timed` back (the times it had), else the default length from 9:00 on its first day.
 */
export function withAllDay(
  d: EventDraft,
  on: boolean,
  defaults: NewEventDefaults,
  timed?: { start: number; end: number },
): EventDraft {
  if (on === d.allDay) return d;
  if (on) {
    const first = toUtcDate(dayOf(d.start));
    const last = toUtcDate(dayOf(Math.max(d.start, d.end - 1)));
    return { ...d, allDay: true, start: first, end: last + DAY, reminder: defaults.allDayAlert, showAs: "free" };
  }
  const day = fromUtcDate(d.start);
  const times =
    timed && dayOf(timed.start) === day
      ? timed
      : { start: atMinutes(day, 9 * 60), end: atMinutes(day, 9 * 60) + defaults.minutes * MINUTE };
  return { ...d, allDay: false, ...times, reminder: defaults.alert, showAs: "busy" };
}

/** An all-day event's first and last days, as local midnights. */
export function allDayDays(d: EventDraft) {
  const first = fromUtcDate(d.start);
  return { first, last: Math.max(first, fromUtcDate(d.end - DAY)) };
}

/** The draft with its all-day days set (local midnights). */
export function withDays(d: EventDraft, first: number, last: number): EventDraft {
  const start = toUtcDate(first);
  return { ...d, start, end: Math.max(toUtcDate(last), start) + DAY };
}

/**
 * Minutes after midnight from a typed time: "9", "930", "9:30", "9.30", "9h30", "9:30pm", "9 am",
 * "21:30", "12am". Null if it isn't one.
 */
export function parseTime(text: string): number | null {
  const m = /^\s*(\d{1,2})(?:[:.h]?(\d{2}))?\s*([ap])?\.?\s*m?\.?\s*$/i.exec(text);
  if (!m) return null;
  let hours = Number(m[1]);
  const minutes = m[2] === undefined ? 0 : Number(m[2]);
  if (minutes > 59) return null;
  const half = m[3]?.toLowerCase();
  if (half) {
    if (hours < 1 || hours > 12) return null;
    hours = (hours % 12) + (half === "p" ? 12 : 0);
  } else if (hours > 23) {
    return null;
  }
  return hours * 60 + minutes;
}

/** The people invited, by address, as they'd go to the server. */
function people(d: EventDraft) {
  return d.attendees.map((p) => p.address.trim().toLowerCase()).join("\n");
}

/** The draft says something other than `before`. */
export function draftChanged(d: EventDraft, before: EventDraft) {
  return (
    d.calendar !== before.calendar ||
    d.title.trim() !== before.title.trim() ||
    d.start !== before.start ||
    d.end !== before.end ||
    d.allDay !== before.allDay ||
    d.location.trim() !== before.location.trim() ||
    d.notes.trim() !== before.notes.trim() ||
    people(d) !== people(before) ||
    d.reminder !== before.reminder ||
    d.showAs !== before.showAs ||
    d.teams !== before.teams
  );
}

/** Who's invited changed: saving mails them (the button says Send Invitation). */
export function attendeesChanged(d: EventDraft, before: EventDraft) {
  return people(d) !== people(before);
}

const plural = (n: number, unit: string) => `${n} ${unit}${n === 1 ? "" : "s"}`;
const clock = new Intl.DateTimeFormat(undefined, { hour: "numeric", minute: "2-digit" });

/**
 * An alert, as the editor words it. Timed: "10 minutes before", "1 hour before", "At the time of
 * the event". All day, counting back from the day's midnight: "1 day before at 12:00 PM".
 */
export function alertLabel(minutes: number | null, allDay: boolean) {
  if (minutes === null) return "None";
  if (allDay) {
    const days = Math.ceil(minutes / (24 * 60));
    const time = clock.format(new Date(2000, 0, 1, 0, days * 24 * 60 - minutes));
    return days === 0 ? `On the day at ${time}` : `${plural(days, "day")} before at ${time}`;
  }
  if (minutes === 0) return "At the time of the event";
  if (minutes < 60) return `${plural(minutes, "minute")} before`;
  if (minutes % (24 * 60) === 0) return `${plural(minutes / (24 * 60), "day")} before`;
  if (minutes % 60 === 0) return `${plural(minutes / 60, "hour")} before`;
  return `${Math.floor(minutes / 60)} h ${minutes % 60} min before`;
}

/** Spark's alert choices, as Microsoft 365 takes them: one alert, at or before the start. */
const TIMED_ALERTS = [null, 0, 5, 10, 15, 30, 60, 120, 24 * 60, 48 * 60];
/** All day, at noon: the day before, two days before, a week before. */
const ALL_DAY_ALERTS = [null, 12 * 60, 36 * 60, (7 * 24 - 12) * 60];

/** The alerts to choose from, with `current` among them if it's another (one Outlook set). */
export function alertChoices(allDay: boolean, current?: number | null) {
  const values: (number | null)[] = [...(allDay ? ALL_DAY_ALERTS : TIMED_ALERTS)];
  if (current !== undefined && !values.includes(current)) {
    values.push(current);
    values.sort((a, b) => (a ?? -1) - (b ?? -1));
  }
  return values.map((value) => ({ value, label: alertLabel(value, allDay) }));
}
