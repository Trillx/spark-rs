// Spark's Home Screen ("Email Focus"): what shows instead of the Inbox
// between sessions. A session is the time spent in the Inbox. It ends when
// spark-rs has been in the background for the inactivity timeout, or when
// the user goes to the Home Screen; launching starts outside one. Email Focus
// times say when to come back. Both kinds of state live in localStorage, like
// the other view settings.

import type { Category, Message } from "../types";

/**
 * `welcome` greets a new day, `welcomeBack` a return, and `betweenSessions`
 * waits for the next Email Focus time. The Home Screen shows in all three.
 */
export type HomeState = "welcome" | "betweenSessions" | "welcomeBack" | "sessionActive";

/** The feed's Preview Emails filter. */
export type PreviewFilter = "none" | "priority" | "people" | "all";

/** Morning, Afternoon and Evening, or "Email Focus Time n" for added ones. */
export type FocusName = "morning" | "afternoon" | "evening" | number;

export interface FocusTime {
  name: FocusName;
  /** Seconds after midnight, local time. */
  at: number;
  enabled: boolean;
  /** A notification at this time, when spark-rs isn't in front. */
  remindMe: boolean;
}

export interface HomeSettings {
  enabled: boolean;
  timeoutEnabled: boolean;
  /** Seconds in the background before the session ends: 1 to 99 minutes. */
  timeout: number;
  focusScheduleEnabled: boolean;
  focusTimes: FocusTime[];
  filter: PreviewFilter;
}

export interface HomeSession {
  state: HomeState;
  /** When `state` last changed. */
  changedAt: number;
  /** When the last session ended. "New" mail is mail that came after. */
  lastSessionEnd?: number;
  /** When spark-rs went to the background, until it's back in front. */
  inactiveSince?: number;
  /** Last seen running, so a session that ended with the app has an end. */
  lastSeen?: number;
  /** Clear All: the feed only shows mail that came after this. */
  clearedAt?: number;
  /** Mail removed from the feed with ✕. */
  dismissed: number[];
  /** The Email Focus time last reminded about, so each reminds once. */
  remindedFor?: number;
}

const HOUR = 3_600;
const MINUTE_MS = 60_000;
const HOUR_MS = 3_600_000;

/** A new day starts at 4:00, when the greeting turns to Good Morning. */
const DAY_START_HOUR = 4;

/** "It's time to check your Inbox" shows for an hour after a focus time, as in Spark. */
const FOCUS_WINDOW_MS = HOUR_MS;

export const TIMEOUT_MIN = 1;
export const TIMEOUT_MAX = 99;

/** Spark's three focus times: 9:00 and 18:00 on, 13:00 off. */
export const DEFAULT_FOCUS_TIMES: FocusTime[] = [
  { name: "morning", at: 9 * HOUR, enabled: true, remindMe: false },
  { name: "afternoon", at: 13 * HOUR, enabled: false, remindMe: false },
  { name: "evening", at: 18 * HOUR, enabled: true, remindMe: false },
];

/**
 * Spark's defaults, except two that Spark leaves off: the inactivity timeout
 * (15 minutes) and the Priority preview. With those off, going to the
 * background never brings the Home Screen back, and the feed is hidden.
 */
export const DEFAULT_SETTINGS: HomeSettings = {
  enabled: true,
  timeoutEnabled: true,
  timeout: 15 * 60,
  focusScheduleEnabled: true,
  focusTimes: DEFAULT_FOCUS_TIMES,
  filter: "priority",
};

/** The session state before any event: in a session, so the first launch opens on the Inbox. */
export function firstSession(now: number): HomeSession {
  return { state: "sessionActive", changedAt: now, dismissed: [] };
}

/** Whether the Home Screen shows in place of the Inbox. */
export const homeShows = (settings: HomeSettings, session: HomeSession) =>
  settings.enabled && session.state !== "sessionActive";

// ---- time ----

/** `t`'s day at `seconds` after midnight, `days` later; DST-safe. */
function atTime(t: number, seconds: number, days = 0) {
  const d = new Date(t);
  d.setDate(d.getDate() + days);
  d.setHours(0, 0, seconds, 0);
  return d.getTime();
}

/** The 4:00 that starts `t`'s day. */
export function dayStart(t: number) {
  const start = atTime(t, DAY_START_HOUR * HOUR);
  return start <= t ? start : atTime(t, DAY_START_HOUR * HOUR, -1);
}

/** The first 4:00 after `t`. */
export function nextDayStart(t: number) {
  const start = atTime(t, DAY_START_HOUR * HOUR);
  return start > t ? start : atTime(t, DAY_START_HOUR * HOUR, 1);
}

/** Whether a day started after `from`, up to `to`. */
export const isNewDay = (from: number, to: number) => nextDayStart(from) <= to;

const activeTimes = (settings: HomeSettings) =>
  settings.focusScheduleEnabled ? settings.focusTimes.filter((f) => f.enabled) : [];

/** The first focus time after `after` (today or tomorrow), or null without one. */
export function nextFocus(settings: HomeSettings, after: number): { at: number; time: FocusTime } | null {
  let best: { at: number; time: FocusTime } | null = null;
  for (const time of activeTimes(settings)) {
    for (const days of [0, 1]) {
      const at = atTime(after, time.at, days);
      if (at > after && (!best || at < best.at)) best = { at, time };
    }
  }
  return best;
}

/** The latest focus time at or before `now` (today or yesterday), or null. */
export function lastFocus(settings: HomeSettings, now: number): { at: number; time: FocusTime } | null {
  let best: { at: number; time: FocusTime } | null = null;
  for (const time of activeTimes(settings)) {
    for (const days of [0, -1]) {
      const at = atTime(now, time.at, days);
      if (at <= now && (!best || at > best.at)) best = { at, time };
    }
  }
  return best;
}

// ---- events ----

const changed = (s: HomeSession, state: HomeState, now: number): HomeSession =>
  s.state === state ? s : { ...s, state, changedAt: now };

/** Outside a session that ended at `end`: waiting for a focus time still to come, or Welcome back. */
function awayState(settings: HomeSettings, end: number, now: number): HomeState {
  const next = nextFocus(settings, end);
  return next && now < next.at ? "betweenSessions" : "welcomeBack";
}

/** A session ended at `end`. The feed starts over. */
function sessionEnded(s: HomeSession, settings: HomeSettings, end: number, now: number): HomeSession {
  return {
    ...s,
    state: awayState(settings, end, now),
    changedAt: now,
    lastSessionEnd: end,
    inactiveSince: undefined,
    clearedAt: undefined,
    dismissed: [],
  };
}

/**
 * spark-rs started. A session still open from last time ended when the app
 * was last seen: it was still running then, and a timeout that was due
 * would have ended it already. A new day greets with Good Morning;
 * otherwise it's the next focus time's wait, or Welcome back.
 */
export function launch(s: HomeSession | null, settings: HomeSettings, now: number): HomeSession {
  if (!s) return firstSession(now);
  const ended =
    s.state === "sessionActive"
      ? sessionEnded(s, settings, Math.min(now, s.lastSeen ?? s.inactiveSince ?? s.changedAt), now)
      : { ...s, inactiveSince: undefined };
  if (isNewDay(s.changedAt, now)) return { ...ended, state: "welcome", changedAt: now };
  if (s.state === "sessionActive") return ended;
  return changed(ended, awayState(settings, ended.lastSessionEnd ?? s.changedAt, now), now);
}

/** Check Inbox, New emails only, a list from the sidebar, a notification: the user is in the Inbox. */
export function startSession(s: HomeSession, now: number): HomeSession {
  return { ...changed(s, "sessionActive", now), inactiveSince: undefined, lastSeen: now };
}

/** The sidebar's Home Screen: the session ends now. */
export function endSession(s: HomeSession, settings: HomeSettings, now: number): HomeSession {
  return s.state === "sessionActive" ? sessionEnded(s, settings, now, now) : s;
}

/** No spark-rs window has focus. */
export function deactivate(s: HomeSession, now: number): HomeSession {
  return s.inactiveSince === undefined ? { ...s, inactiveSince: now } : s;
}

/** What `tick` asks the app to do besides saving the new session. */
export interface TickResult {
  session: HomeSession;
  /** Show the "It's time for Email Focus!" notification. */
  remind: boolean;
}

/**
 * Runs the timers: every few seconds while spark-rs runs, and when it comes
 * back in front (before `activate`), since timers in a hidden window run late.
 * A change is dated to when it was due, not when the check ran, so a late
 * check still sees the new day that followed it.
 *
 * - In a session, the inactivity timeout ends it, as of when spark-rs went to
 *   the background. It waits while `paused` (writing or reading an email).
 * - Between sessions, the next focus time brings Welcome back.
 * - A new day turns Welcome back and the wait into Good Morning.
 * - Outside a session, a focus time with Remind me notifies once, unless
 *   spark-rs is in front.
 */
export function tick(
  s: HomeSession,
  settings: HomeSettings,
  now: number,
  { active, paused }: { active: boolean; paused: boolean },
): TickResult {
  let next: HomeSession = { ...s, lastSeen: now };
  if (!settings.enabled) return { session: next, remind: false };

  if (
    next.state === "sessionActive" &&
    settings.timeoutEnabled &&
    !paused &&
    next.inactiveSince !== undefined &&
    now - next.inactiveSince >= settings.timeout * 1000
  ) {
    next = sessionEnded(next, settings, next.inactiveSince, next.inactiveSince + settings.timeout * 1000);
  }

  // Away from the Inbox, the next focus time and the next day apply in the
  // order they came. Waiting with no focus time left (the schedule was turned
  // off) is Welcome back, as of when the wait began.
  for (;;) {
    if (next.state === "betweenSessions" && !nextFocus(settings, next.changedAt)) {
      next = changed(next, "welcomeBack", next.changedAt);
    }
    if (next.state !== "betweenSessions" && next.state !== "welcomeBack") break;
    const day = nextDayStart(next.changedAt);
    const focus = next.state === "betweenSessions" ? nextFocus(settings, next.changedAt)!.at : Infinity;
    if (Math.min(day, focus) > now) break;
    next = focus < day ? changed(next, "welcomeBack", focus) : changed(next, "welcome", day);
  }

  let remind = false;
  const focus = lastFocus(settings, now);
  if (focus && now - focus.at < MINUTE_MS * 5 && focus.at !== next.remindedFor && next.state !== "sessionActive") {
    next = { ...next, remindedFor: focus.at };
    remind = focus.time.remindMe && !active;
  }
  return { session: next, remind };
}

/** spark-rs is back in front: run the timers first, then stop counting the time away. */
export function activate(
  s: HomeSession,
  settings: HomeSettings,
  now: number,
  paused: boolean,
): HomeSession {
  const { session } = tick(s, settings, now, { active: false, paused });
  return { ...session, inactiveSince: undefined };
}

/** The settings changed. Turning the Home Screen on starts in a session: the user is in the Inbox. */
export function reconfigure(s: HomeSession, before: HomeSettings, after: HomeSettings, now: number): HomeSession {
  if (after.enabled && !before.enabled) return startSession(s, now);
  return s;
}

// ---- what it shows ----

export type Greeting = "Good Night" | "Good Morning" | "Good Afternoon" | "Good Evening";

/** Spark's greeting by the hour: night until 4, morning until noon, afternoon until 5 PM, evening until 11 PM. */
export function greeting(hour: number): Greeting {
  if (hour < 4 || hour >= 23) return "Good Night";
  if (hour < 12) return "Good Morning";
  if (hour < 17) return "Good Afternoon";
  return "Good Evening";
}

/** The greeting with the user's first name, as Spark words it ("Hi, Ana!" in the afternoon). */
export function greetingLine(hour: number, name: string) {
  const g = greeting(hour);
  const first = name.trim().split(/\s+/)[0] ?? "";
  if (!first) return `${g}!`;
  return g === "Good Afternoon" ? `Hi, ${first}!` : `${g}, ${first}!`;
}

/** The line under the greeting, if any. */
export function secondLine(
  s: HomeSession,
  settings: HomeSettings,
  now: number,
): { kind: "time" } | { kind: "next"; at: number; tomorrow: boolean } | null {
  if (s.state === "betweenSessions") {
    const next = nextFocus(settings, now);
    // "Tomorrow" goes by the calendar, as Spark's does: 9:00 seen at 00:30 is today.
    if (next) return { kind: "next", at: next.at, tomorrow: new Date(next.at).getDate() !== new Date(now).getDate() };
    return null;
  }
  const last = lastFocus(settings, now);
  if (last && now - last.at < FOCUS_WINDOW_MS) return { kind: "time" };
  return null;
}

/** When mail arrived in the Inbox, or came back to it from Snooze. */
const arrived = (m: Message) => Math.max(m.timestamp, m.returnedAt ?? 0);

/** Where "new" starts: the last session's end, or when the state last changed. */
export const newSince = (s: HomeSession) => s.lastSessionEnd ?? s.changedAt;

export type HomeCounts = Record<Category, number>;

/** Inbox mail that came while the user was away, by category. `inbox` is the Inbox's mail, New Senders left out. */
export function homeCounts(inbox: Message[], since: number): HomeCounts {
  const counts: HomeCounts = { people: 0, notifications: 0, newsletters: 0 };
  for (const m of inbox) if (m.state === "inbox" && arrived(m) > since) counts[m.category]++;
  return counts;
}

/** "3 emails from people, 2 newsletters, 5 notifications", or "No new emails". */
export function summaryText(counts: HomeCounts) {
  const n = (count: number, one: string, many: string) => `${count} ${count === 1 ? one : many}`;
  if (!counts.people && !counts.notifications && !counts.newsletters) return "No new emails";
  const parts = [
    counts.people ? n(counts.people, "email from people", "emails from people") : "No new emails from people",
  ];
  if (counts.newsletters) parts.push(n(counts.newsletters, "newsletter", "newsletters"));
  if (counts.notifications) parts.push(n(counts.notifications, "notification", "notifications"));
  return parts.join(", ");
}

/** Whether `m` is in the feed under `filter`: Priority, People (Priority and people), or all of it. */
export function inFilter(m: Message, filter: PreviewFilter) {
  switch (filter) {
    case "none":
      return false;
    case "priority":
      return m.priority;
    case "people":
      return m.priority || m.category === "people";
    case "all":
      return true;
  }
}

/**
 * The feed: unread Inbox mail that came after the last session (or Clear All), newest first.
 * Emails in `read`, read while the Home Screen shows, keep their place until it closes.
 */
export function feed(
  inbox: Message[],
  s: HomeSession,
  filter: PreviewFilter,
  read: ReadonlySet<number> = new Set(),
): Message[] {
  const since = Math.max(newSince(s), s.clearedAt ?? 0);
  const dismissed = new Set(s.dismissed);
  const unread = (m: Message) => m.unread || read.has(m.id);
  return inbox
    .filter((m) => m.state === "inbox" && unread(m) && arrived(m) > since && !dismissed.has(m.id) && inFilter(m, filter))
    .sort((a, b) => arrived(b) - arrived(a));
}

/** ✕ on a feed row. */
export const dismiss = (s: HomeSession, id: number): HomeSession =>
  s.dismissed.includes(id) ? s : { ...s, dismissed: [...s.dismissed, id] };

/** Clear All: the feed empties until more mail comes. */
export const clearAll = (s: HomeSession, now: number): HomeSession => ({ ...s, clearedAt: now, dismissed: [] });

/** Inbox mail for "New emails only": what came after the last session. */
export const newEmails = (inbox: Message[], since: number) =>
  inbox.filter((m) => m.state === "inbox" && arrived(m) > since);

// ---- settings ----

/** "Morning", "Afternoon", "Evening" or "Email Focus Time 4". */
export function focusLabel(name: FocusName) {
  if (typeof name === "number") return `Email Focus Time ${name}`;
  return name[0].toUpperCase() + name.slice(1);
}

/** Add Email Focus Time: an hour after the latest one, numbered after the others. */
export function addFocusTime(times: FocusTime[]): FocusTime[] {
  const numbers = times.map((t) => (typeof t.name === "number" ? t.name : 0));
  const number = Math.max(times.length, ...numbers) + 1;
  const latest = Math.max(0, ...times.map((t) => t.at));
  const at = Math.min(latest + HOUR, 24 * HOUR - 60 * 15);
  return [...times, { name: number, at, enabled: true, remindMe: false }];
}

const SETTINGS_KEY = "spark-rs:home-settings";
const SESSION_KEY = "spark-rs:home-session";

const FILTERS: PreviewFilter[] = ["none", "priority", "people", "all"];
const STATES: HomeState[] = ["welcome", "betweenSessions", "welcomeBack", "sessionActive"];
const isTime = (v: unknown): v is number => typeof v === "number" && Number.isFinite(v);

/** Saved settings, with anything missing or broken from the defaults. */
export function parseSettings(raw: string | null): HomeSettings {
  let saved: Partial<HomeSettings> = {};
  try {
    saved = (raw && JSON.parse(raw)) || {};
  } catch {
    // Broken JSON: the defaults.
  }
  const bool = (v: unknown, fallback: boolean) => (typeof v === "boolean" ? v : fallback);
  const timeout = isTime(saved.timeout)
    ? Math.min(TIMEOUT_MAX * 60, Math.max(TIMEOUT_MIN * 60, Math.round(saved.timeout / 60) * 60))
    : DEFAULT_SETTINGS.timeout;
  const focusTimes = Array.isArray(saved.focusTimes)
    ? saved.focusTimes.filter(
        (f): f is FocusTime =>
          !!f &&
          (typeof f.name === "number" || f.name === "morning" || f.name === "afternoon" || f.name === "evening") &&
          isTime(f.at) &&
          f.at >= 0 &&
          f.at < 24 * HOUR &&
          typeof f.enabled === "boolean" &&
          typeof f.remindMe === "boolean",
      )
    : DEFAULT_FOCUS_TIMES;
  return {
    enabled: bool(saved.enabled, DEFAULT_SETTINGS.enabled),
    timeoutEnabled: bool(saved.timeoutEnabled, DEFAULT_SETTINGS.timeoutEnabled),
    timeout,
    focusScheduleEnabled: bool(saved.focusScheduleEnabled, DEFAULT_SETTINGS.focusScheduleEnabled),
    focusTimes,
    filter: FILTERS.includes(saved.filter as PreviewFilter) ? (saved.filter as PreviewFilter) : DEFAULT_SETTINGS.filter,
  };
}

/** The saved session, or null before the first launch (or if it's broken). */
export function parseSession(raw: string | null): HomeSession | null {
  try {
    const s = raw && JSON.parse(raw);
    if (!s || !STATES.includes(s.state) || !isTime(s.changedAt)) return null;
    const time = (v: unknown) => (isTime(v) ? v : undefined);
    return {
      state: s.state,
      changedAt: s.changedAt,
      lastSessionEnd: time(s.lastSessionEnd),
      inactiveSince: time(s.inactiveSince),
      lastSeen: time(s.lastSeen),
      clearedAt: time(s.clearedAt),
      dismissed: Array.isArray(s.dismissed) ? s.dismissed.filter(isTime) : [],
      remindedFor: time(s.remindedFor),
    };
  } catch {
    return null;
  }
}

export const loadHomeSettings = () => parseSettings(localStorage.getItem(SETTINGS_KEY));
export const saveHomeSettings = (settings: HomeSettings) =>
  localStorage.setItem(SETTINGS_KEY, JSON.stringify(settings));
export const loadHomeSession = () => parseSession(localStorage.getItem(SESSION_KEY));
export const saveHomeSession = (session: HomeSession) => localStorage.setItem(SESSION_KEY, JSON.stringify(session));
