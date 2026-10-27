// Run with `npm test` (Node's own test runner; it strips the types).
import assert from "node:assert/strict";
import { test } from "node:test";
import type { Message } from "../types.ts";
import {
  DEFAULT_SETTINGS,
  activate,
  addFocusTime,
  clearAll,
  dayStart,
  deactivate,
  dismiss,
  endSession,
  feed,
  firstSession,
  focusLabel,
  greetingLine,
  homeCounts,
  homeShows,
  launch,
  nextFocus,
  parseSession,
  parseSettings,
  reconfigure,
  secondLine,
  startSession,
  summaryText,
  tick,
  type HomeSession,
  type HomeSettings,
} from "./homeScreen.ts";

/** Local time on 26 October 2026 (a Monday), plus `days`. */
const at = (hour: number, minute = 0, days = 0) => new Date(2026, 9, 26 + days, hour, minute).getTime();
const MIN = 60_000;

const settings: HomeSettings = DEFAULT_SETTINGS;
const noSchedule: HomeSettings = { ...DEFAULT_SETTINGS, focusScheduleEnabled: false };
const running = { active: false, paused: false };

const mail = (id: number, timestamp: number, patch: Partial<Message> = {}): Message => ({
  id,
  account: 1,
  category: "people",
  state: "inbox",
  unread: true,
  pinned: false,
  priority: false,
  screening: false,
  from: "Ana",
  address: "ana@x.com",
  timestamp,
  subject: "",
  snippet: "",
  ...patch,
});

test("the first launch opens on the Inbox", () => {
  const s = launch(null, settings, at(10));
  assert.equal(s.state, "sessionActive");
  assert.equal(homeShows(settings, s), false);
});

test("going to the background for the timeout ends the session as of when it left", () => {
  let s = startSession(firstSession(at(9)), at(9));
  s = deactivate(s, at(10));
  assert.equal(tick(s, noSchedule, at(10, 14), running).session.state, "sessionActive");
  const ended = tick(s, noSchedule, at(10, 15), running).session;
  assert.equal(ended.state, "welcomeBack");
  assert.equal(ended.lastSessionEnd, at(10));
  assert.ok(homeShows(noSchedule, ended));
});

test("the timeout waits while an email is being written or read, and needs the timeout on", () => {
  const s = deactivate(startSession(firstSession(at(9)), at(9)), at(10));
  assert.equal(tick(s, noSchedule, at(11), { active: false, paused: true }).session.state, "sessionActive");
  const off = { ...noSchedule, timeoutEnabled: false };
  assert.equal(tick(s, off, at(11), running).session.state, "sessionActive");
});

test("coming back before the timeout keeps the session; coming back late ends it", () => {
  const s = deactivate(startSession(firstSession(at(9)), at(9)), at(10));
  const early = activate(s, noSchedule, at(10, 5), false);
  assert.equal(early.state, "sessionActive");
  assert.equal(early.inactiveSince, undefined);
  // A hidden window's timers run late: the check on the way back catches it.
  const late = activate(s, noSchedule, at(11), false);
  assert.equal(late.state, "welcomeBack");
  assert.equal(late.lastSessionEnd, at(10));
});

test("with a focus time still to come, the Home Screen waits for it, then welcomes back", () => {
  const s = deactivate(startSession(firstSession(at(9)), at(9)), at(10));
  const waiting = tick(s, settings, at(10, 15), running).session;
  assert.equal(waiting.state, "betweenSessions");
  assert.deepEqual(secondLine(waiting, settings, at(12)), { kind: "next", at: at(18), tomorrow: false });
  assert.equal(tick(waiting, settings, at(17, 59), running).session.state, "betweenSessions");
  const back = tick(waiting, settings, at(18), running).session;
  assert.equal(back.state, "welcomeBack");
  assert.deepEqual(secondLine(back, settings, at(18, 30)), { kind: "time" });
  assert.equal(secondLine(back, settings, at(19, 1)), null);
});

test("after the last focus time of the day, the next one is tomorrow morning", () => {
  const s = endSession(startSession(firstSession(at(19)), at(19)), settings, at(20));
  assert.equal(s.state, "betweenSessions");
  assert.deepEqual(secondLine(s, settings, at(20)), { kind: "next", at: at(9, 0, 1), tomorrow: true });
  // Past midnight, 9:00 is today.
  assert.deepEqual(secondLine(s, settings, at(0, 30, 1)), { kind: "next", at: at(9, 0, 1), tomorrow: false });
});

test("a new day turns the wait and Welcome back into Good Morning", () => {
  const s = endSession(startSession(firstSession(at(19)), at(19)), noSchedule, at(20));
  assert.equal(s.state, "welcomeBack");
  assert.equal(tick(s, noSchedule, at(3, 59, 1), running).session.state, "welcomeBack");
  assert.equal(tick(s, noSchedule, at(4, 0, 1), running).session.state, "welcome");
  assert.equal(dayStart(at(2, 0, 1)), at(4));
});

test("a check that runs late still sees the new day after the change it was due", () => {
  // Waiting for 18:00 on Monday, first checked at 4:00 on Tuesday.
  const waiting = endSession(startSession(firstSession(at(9)), at(9)), settings, at(10));
  assert.equal(tick(waiting, settings, at(4, 0, 1), running).session.state, "welcome");
  // Waiting since 20:00 on Monday for 9:00, first checked at 9:01 on Tuesday: 4:00 came first.
  const evening = endSession(startSession(firstSession(at(19)), at(19)), settings, at(20));
  assert.equal(evening.state, "betweenSessions");
  assert.equal(tick(evening, settings, at(9, 1, 1), running).session.state, "welcome");
  // A timeout due at 20:15 on Monday, first checked at 5:00 on Tuesday.
  const away = deactivate(startSession(firstSession(at(19)), at(19)), at(20));
  const late = tick(away, noSchedule, at(5, 0, 1), running).session;
  assert.equal(late.state, "welcome");
  assert.equal(late.lastSessionEnd, at(20));
});

test("relaunching ends an open session when the app was last seen, not when it left the front", () => {
  // The composer held the timeout off from 10:00 until spark-rs quit at 15:00.
  const open: HomeSession = { ...deactivate(startSession(firstSession(at(9)), at(9)), at(10)), lastSeen: at(15) };
  assert.equal(launch(open, noSchedule, at(15, 1)).lastSessionEnd, at(15));
});

test("launching ends the last session where the app was last seen", () => {
  const open: HomeSession = { ...startSession(firstSession(at(9)), at(9)), lastSeen: at(11) };
  const same = launch(open, noSchedule, at(13));
  assert.equal(same.state, "welcomeBack");
  assert.equal(same.lastSessionEnd, at(11));
  const nextDay = launch(open, noSchedule, at(8, 0, 1));
  assert.equal(nextDay.state, "welcome");
  assert.equal(nextDay.lastSessionEnd, at(11));
});

test("relaunching between sessions keeps Clear All and dismissed rows", () => {
  let s = endSession(startSession(firstSession(at(9)), at(9)), noSchedule, at(10));
  s = dismiss(clearAll(s, at(10, 30)), 7);
  const again = launch(s, noSchedule, at(11));
  assert.equal(again.clearedAt, at(10, 30));
  assert.deepEqual(again.dismissed, [7]);
});

test("Remind me notifies once at its focus time, only outside a session and out of sight", () => {
  const remind: HomeSettings = {
    ...settings,
    focusTimes: settings.focusTimes.map((f) => (f.name === "evening" ? { ...f, remindMe: true } : f)),
  };
  const waiting = endSession(startSession(firstSession(at(9)), at(9)), remind, at(10));
  const first = tick(waiting, remind, at(18, 1), running);
  assert.equal(first.remind, true);
  assert.equal(first.session.state, "welcomeBack");
  assert.equal(tick(first.session, remind, at(18, 2), running).remind, false);
  assert.equal(tick(waiting, remind, at(18, 1), { active: true, paused: false }).remind, false);
  const inSession = startSession(waiting, at(17));
  assert.equal(tick(inSession, remind, at(18, 1), running).remind, false);
  // Morning has no reminder.
  assert.equal(tick(waiting, remind, at(9, 1, 1), running).remind, false);
});

test("turning the Home Screen on starts in the Inbox", () => {
  const off = { ...settings, enabled: false };
  const s = endSession(startSession(firstSession(at(9)), at(9)), settings, at(10));
  assert.equal(homeShows(off, s), false);
  assert.equal(reconfigure(s, off, settings, at(11)).state, "sessionActive");
});

test("the summary counts what came after the last session", () => {
  const s = endSession(startSession(firstSession(at(9)), at(9)), noSchedule, at(10));
  const inbox = [
    mail(1, at(9, 30)),
    mail(2, at(10, 30)),
    mail(3, at(11), { category: "newsletters", unread: false }),
    mail(4, at(11), { category: "notifications" }),
    mail(5, at(8), { returnedAt: at(12) }),
  ];
  const counts = homeCounts(inbox, s.lastSessionEnd!);
  assert.deepEqual(counts, { people: 2, notifications: 1, newsletters: 1 });
  assert.equal(summaryText(counts), "2 emails from people, 1 newsletter, 1 notification");
  assert.equal(summaryText({ people: 0, notifications: 0, newsletters: 3 }), "No new emails from people, 3 newsletters");
  assert.equal(summaryText({ people: 0, notifications: 0, newsletters: 0 }), "No new emails");
});

test("the feed shows new unread mail by the Preview filter, minus dismissed and cleared", () => {
  let s = endSession(startSession(firstSession(at(9)), at(9)), noSchedule, at(10));
  const inbox = [
    mail(1, at(10, 30), { priority: true }),
    mail(2, at(11)),
    mail(3, at(11, 30), { category: "notifications" }),
    mail(4, at(12), { unread: false }),
    mail(5, at(9)),
  ];
  const ids = (filter: Parameters<typeof feed>[2]) => feed(inbox, s, filter).map((m) => m.id);
  assert.deepEqual(ids("priority"), [1]);
  assert.deepEqual(ids("people"), [2, 1]);
  assert.deepEqual(ids("all"), [3, 2, 1]);
  assert.deepEqual(ids("none"), []);
  s = dismiss(s, 2);
  assert.deepEqual(ids("all"), [3, 1]);
  s = clearAll(s, at(11, 15));
  assert.deepEqual(ids("all"), [3]);
});

test("greetings follow the hour and use the first name", () => {
  assert.equal(greetingLine(3, "Ana Ruiz"), "Good Night, Ana!");
  assert.equal(greetingLine(4, "Ana Ruiz"), "Good Morning, Ana!");
  assert.equal(greetingLine(12, "Ana"), "Hi, Ana!");
  assert.equal(greetingLine(17, ""), "Good Evening!");
  assert.equal(greetingLine(23, "Ana"), "Good Night, Ana!");
});

test("focus times are named and added like Spark's", () => {
  const times = addFocusTime(settings.focusTimes);
  assert.equal(times.length, 4);
  assert.equal(focusLabel(times[3].name), "Email Focus Time 4");
  assert.equal(times[3].at, 19 * 3600);
  assert.equal(focusLabel("morning"), "Morning");
  assert.equal(nextFocus(noSchedule, at(8)), null);
});

test("saved settings and sessions are checked, falling back to the defaults", () => {
  assert.deepEqual(parseSettings(null), DEFAULT_SETTINGS);
  assert.deepEqual(parseSettings("not json"), DEFAULT_SETTINGS);
  const saved = parseSettings(JSON.stringify({ enabled: false, timeout: 100_000, filter: "bogus", focusTimes: [{}] }));
  assert.equal(saved.enabled, false);
  assert.equal(saved.timeout, 99 * 60);
  assert.equal(saved.filter, DEFAULT_SETTINGS.filter);
  assert.deepEqual(saved.focusTimes, []);
  assert.equal(parseSession(JSON.stringify({ state: "nope", changedAt: 1 })), null);
  const s = parseSession(JSON.stringify({ state: "welcomeBack", changedAt: 5, dismissed: [1, "x"] }));
  assert.deepEqual(s?.dismissed, [1]);
});
