// Run with `npm test` (Node's own test runner; it strips the types).
import assert from "node:assert/strict";
import { test } from "node:test";
import type { CalendarEvent, EventDraft } from "../types.ts";
import {
  alertChoices,
  alertLabel,
  allDayDays,
  atMinutes,
  attendeesChanged,
  dayOf,
  draftChanged,
  draftOf,
  newDraft,
  newEventStart,
  parseTime,
  toUtcDate,
  withAllDay,
  withEnd,
  withStart,
} from "./eventDraft.ts";

const MINUTE = 60_000;
const DAY = 24 * 60 * MINUTE;
const defaults = { minutes: 60, alert: 10, allDayAlert: 720 };
const today = dayOf(new Date(2026, 9, 27, 15).getTime());

test("typed times are read loosely, as Spark reads them", () => {
  const cases: [string, number | null][] = [
    ["9", 540],
    ["930", 570],
    ["9:30", 570],
    ["9.30", 570],
    ["9h30", 570],
    ["9:30pm", 1290],
    ["9:30 PM", 1290],
    ["9 am", 540],
    ["12am", 0],
    ["12 pm", 720],
    ["21:30", 1290],
    ["0:05", 5],
    ["24:00", null],
    ["9:75", null],
    ["13pm", null],
    ["noon", null],
    ["", null],
  ];
  for (const [text, minutes] of cases) assert.equal(parseTime(text), minutes, text);
});

test("a new event starts at the next quarter hour today, else at 9:00", () => {
  const now = atMinutes(today, 14 * 60 + 7);
  assert.equal(newEventStart(now, today), atMinutes(today, 14 * 60 + 15));
  assert.equal(newEventStart(atMinutes(today, 14 * 60), today), atMinutes(today, 14 * 60 + 15));
  const tomorrow = atMinutes(today + 36 * 60 * MINUTE, 0);
  assert.equal(newEventStart(now, dayOf(tomorrow)), atMinutes(dayOf(tomorrow), 9 * 60));

  const d = newDraft(3, atMinutes(today, 10 * 60), false, defaults);
  assert.equal(d.end - d.start, 60 * MINUTE);
  assert.equal(d.reminder, 10);
  assert.equal(d.showAs, "busy");
  const allDay = newDraft(3, toUtcDate(today), true, defaults);
  assert.equal(allDay.end - allDay.start, DAY);
  assert.equal(allDay.reminder, 720);
  assert.equal(allDay.showAs, "free");
});

test("times keep the length, and an end before the start goes to the next day", () => {
  const d = newDraft(1, atMinutes(today, 10 * 60), false, { ...defaults, minutes: 45 });
  const moved = withStart(d, atMinutes(today, 13 * 60));
  assert.equal(moved.end, atMinutes(today, 13 * 60 + 45));
  const early = withEnd(moved, atMinutes(today, 12 * 60));
  assert.equal(early.end, atMinutes(today, 12 * 60) + DAY);
  const later = withEnd(moved, atMinutes(today, 14 * 60));
  assert.equal(later.end, atMinutes(today, 14 * 60));
});

test("all day on and off, keeping the days and the times it had", () => {
  const timed = newDraft(1, atMinutes(today, 10 * 60), false, defaults);
  const on = withAllDay(timed, true, defaults);
  assert.equal(on.start, toUtcDate(today));
  assert.equal(on.end, toUtcDate(today) + DAY);
  assert.equal(on.reminder, 720);
  assert.deepEqual(allDayDays(on), { first: today, last: today });
  const back = withAllDay(on, false, defaults, { start: timed.start, end: timed.end });
  assert.equal(back.start, timed.start);
  assert.equal(back.end, timed.end);
  assert.equal(back.reminder, 10);
  const fresh = withAllDay(on, false, defaults);
  assert.equal(fresh.start, atMinutes(today, 9 * 60));
  assert.equal(fresh.end - fresh.start, 60 * MINUTE);
  // An all-day event can't end before it starts.
  assert.equal(withEnd(on, on.start - DAY).end, on.start + DAY);
});

test("changes and new people are told apart", () => {
  const event: CalendarEvent = {
    id: 9,
    calendar: 1,
    account: 1,
    title: "Standup",
    start: atMinutes(today, 9 * 60 + 30),
    end: atMinutes(today, 9 * 60 + 45),
    allDay: false,
    attendees: [{ name: "Sam", address: "Sam@contoso.com", answer: "accepted" }],
    response: "organizer",
    canceled: false,
    repeats: false,
    showAs: "busy",
    private: false,
    onlineMeeting: false,
  };
  const before = draftOf(event);
  assert.deepEqual(before.attendees, [{ name: "Sam", address: "Sam@contoso.com" }]);
  assert.equal(draftOf({ ...event, private: true }).private, true, "a copy stays private");
  assert.equal(before.reminder, null);
  assert.equal(before.location, "");
  const same: EventDraft = { ...before, title: " Standup ", attendees: [{ name: "", address: "sam@contoso.com " }] };
  assert.equal(draftChanged(same, before), false);
  assert.equal(attendeesChanged(same, before), false);
  const more = { ...before, attendees: [...before.attendees, { name: "", address: "kai@contoso.com" }] };
  assert.equal(draftChanged(more, before), true);
  assert.equal(attendeesChanged(more, before), true);
  assert.equal(draftChanged({ ...before, reminder: 5 }, before), true);
});

test("alerts are worded as the editor lists them", () => {
  assert.equal(alertLabel(null, false), "None");
  assert.equal(alertLabel(0, false), "At the time of the event");
  assert.equal(alertLabel(1, false), "1 minute before");
  assert.equal(alertLabel(10, false), "10 minutes before");
  assert.equal(alertLabel(120, false), "2 hours before");
  assert.equal(alertLabel(1440, false), "1 day before");
  assert.equal(alertLabel(90, false), "1 h 30 min before");
  assert.match(alertLabel(720, true), /^1 day before at 12:00/);
  assert.match(alertLabel(1080, true), /^1 day before at 6:00/);
  assert.match(alertLabel(0, true), /^On the day at 12:00/);
  const timed = alertChoices(false, 25).map((c) => c.value);
  assert.deepEqual(timed, [null, 0, 5, 10, 15, 25, 30, 60, 120, 1440, 2880], "Outlook's own value is kept, in order");
  assert.equal(alertChoices(true).length, 4);
});

test("drags on the grid end where Spark's do", async () => {
  const { gestureOutcome, shiftDays } = await import("./eventDraft.ts");
  const days = [0, 1, 2].map((i) => {
    const d = new Date(today);
    d.setDate(d.getDate() + i);
    return d.getTime();
  });
  const at = (col: number, minutes: number, allDay = false) => ({ col, minutes, allDay });
  const event = (start: number, end: number, extra: Partial<CalendarEvent> = {}): CalendarEvent => ({
    id: 1,
    calendar: 1,
    account: 1,
    title: "x",
    start,
    end,
    allDay: false,
    attendees: [],
    response: "organizer",
    canceled: false,
    repeats: false,
    showAs: "busy",
    private: false,
    onlineMeeting: false,
    ...extra,
  });

  // A new event: from the quarter pressed to the one released, both included, upwards too.
  assert.deepEqual(gestureOutcome({ kind: "create", from: at(1, 9 * 60 + 7) }, at(2, 10 * 60 + 20), days), {
    start: atMinutes(days[1], 9 * 60),
    end: atMinutes(days[1], 10 * 60 + 30),
    allDay: false,
  });
  assert.deepEqual(gestureOutcome({ kind: "create", from: at(0, 11 * 60) }, at(0, 9 * 60 + 50), days), {
    start: atMinutes(days[0], 9 * 60 + 45),
    end: atMinutes(days[0], 11 * 60 + 15),
    allDay: false,
  });

  // Moved: the length kept, to the quarter hour; onto the all-day row, all day.
  const standup = event(atMinutes(days[0], 9 * 60 + 30), atMinutes(days[0], 9 * 60 + 45));
  const moved = gestureOutcome({ kind: "move", event: standup, grab: 5, floor: 0 }, at(2, 14 * 60 + 12), days)!;
  assert.equal(moved.start, atMinutes(days[2], 14 * 60));
  assert.equal(moved.end! - moved.start, 15 * MINUTE);
  assert.equal(gestureOutcome({ kind: "move", event: standup, grab: 0, floor: 0 }, at(0, 9 * 60 + 31), days), null, "no change");
  assert.deepEqual(gestureOutcome({ kind: "move", event: standup, grab: 0, floor: 0 }, at(1, 0, true), days), {
    start: toUtcDate(days[1]),
    end: toUtcDate(days[1]) + DAY,
    allDay: true,
  });
  assert.equal(
    gestureOutcome({ kind: "move", event: { ...standup, repeats: true }, grab: 0, floor: 0 }, at(1, 0, true), days),
    null,
    "a repeating event keeps its all day",
  );

  // Edges: no closer than 15 minutes to the other.
  const review = event(atMinutes(days[0], 10 * 60), atMinutes(days[0], 11 * 60));
  const end = gestureOutcome({ kind: "resize", event: review, edge: "end" }, at(0, 8 * 60), days)!;
  assert.equal(end.end, review.start + 15 * MINUTE);
  const start = gestureOutcome({ kind: "resize", event: review, edge: "start" }, at(0, 8 * 60 + 52), days)!;
  assert.equal(start.start, atMinutes(days[0], 8 * 60 + 45));
  assert.equal(start.end, review.end);
  assert.equal(gestureOutcome({ kind: "resize", event: standup, edge: "end" }, at(0, 8 * 60), days), null, "already 15 minutes");

  // The all-day row: moved by days; onto the hours, timed with the default length.
  const offsite = event(toUtcDate(days[0]), toUtcDate(days[0]) + 2 * DAY, { allDay: true });
  assert.deepEqual(gestureOutcome({ kind: "row", event: offsite, from: at(0, 0, true) }, at(1, 0, true), days), {
    start: offsite.start + DAY,
    end: offsite.end + DAY,
    allDay: true,
  });
  assert.deepEqual(gestureOutcome({ kind: "row", event: offsite, from: at(0, 0, true) }, at(2, 13 * 60 + 40), days), {
    start: atMinutes(days[2], 13 * 60 + 30),
    end: null,
    allDay: false,
  });

  // Days later, the same clock times.
  const shifted = shiftDays(standup, 2);
  assert.equal(minutesOfTest(shifted.start), 9 * 60 + 30);
  assert.equal(shifted.end - shifted.start, 15 * MINUTE);
});

function minutesOfTest(t: number) {
  const d = new Date(t);
  return d.getHours() * 60 + d.getMinutes();
}

test("moves keep clock times across a DST change and past midnight", async () => {
  const { gestureOutcome, moveGesture } = await import("./eventDraft.ts");
  const zone = process.env.TZ;
  process.env.TZ = "America/New_York";
  try {
    const midnight = (y: number, m: number, d: number) => new Date(y, m, d).getTime();
    const event = (start: number, end: number): CalendarEvent => ({
      id: 1,
      calendar: 1,
      account: 1,
      title: "x",
      start,
      end,
      allDay: false,
      attendees: [],
      response: "organizer",
      canceled: false,
      repeats: false,
      showAs: "busy",
      private: false,
      onlineMeeting: false,
    });
    const clock = (t: number) => `${new Date(t).getHours()}:${String(new Date(t).getMinutes()).padStart(2, "0")}`;

    // November 1, 2026 has 25 hours; 9:00 there to Monday is 9:00 still.
    const days = [midnight(2026, 10, 1), midnight(2026, 10, 2), midnight(2026, 10, 3)];
    const nine = event(atMinutes(days[0], 9 * 60), atMinutes(days[0], 10 * 60));
    const g = moveGesture(nine, { col: 0, minutes: 9 * 60 + 10, allDay: false }, days);
    const moved = gestureOutcome(g, { col: 1, minutes: 9 * 60 + 10, allDay: false }, days)!;
    assert.equal(clock(moved.start), "9:00");
    assert.equal(new Date(moved.start).getDate(), 2);

    // Monday 23:00 to Tuesday 1:00, picked up by Tuesday's part: a day later, Tuesday 23:00.
    const late = event(atMinutes(days[1], 23 * 60), atMinutes(days[2], 60));
    const night = moveGesture(late, { col: 2, minutes: 30, allDay: false }, days);
    assert.equal(night.kind === "move" && night.floor, -Infinity);
    assert.equal(gestureOutcome(night, { col: 2, minutes: 30, allDay: false }, days), null, "where it was");
    const week = [days[1], days[2], midnight(2026, 10, 4)];
    const tuesday = moveGesture(late, { col: 1, minutes: 30, allDay: false }, week);
    const shifted = gestureOutcome(tuesday, { col: 2, minutes: 30, allDay: false }, week)!;
    assert.equal(new Date(shifted.start).getDate(), 3);
    assert.equal(clock(shifted.start), "23:00");
    assert.equal(clock(shifted.end!), "1:00");
    // Picked up at 0:45 and let go at 0:30 the next day: a quarter hour earlier, 22:45.
    const earlier = gestureOutcome(
      moveGesture(late, { col: 1, minutes: 45, allDay: false }, week),
      { col: 2, minutes: 30, allDay: false },
      week,
    )!;
    assert.equal(clock(earlier.start), "22:45");
    assert.equal(new Date(earlier.start).getDate(), 3);
  } finally {
    if (zone === undefined) delete process.env.TZ;
    else process.env.TZ = zone;
  }
});

test("an all-day event's new first day keeps its length across a DST change", async () => {
  const { withFirstDay, allDayDays } = await import("./eventDraft.ts");
  const zone = process.env.TZ;
  process.env.TZ = "America/New_York";
  try {
    const local = (m: number, d: number) => new Date(2026, m, d).getTime();
    const d = { ...newDraft(1, toUtcDate(local(9, 27)), true, defaults), end: toUtcDate(local(9, 28)) + DAY };
    const moved = withFirstDay(d, local(10, 1));
    assert.deepEqual(allDayDays(moved), { first: local(10, 1), last: local(10, 2) });
  } finally {
    if (zone === undefined) delete process.env.TZ;
    else process.env.TZ = zone;
  }
});

test("an end after midnight is the next day on the clock, across a DST change too", () => {
  const zone = process.env.TZ;
  process.env.TZ = "America/New_York";
  try {
    // March 8, 2026 starts summer time at 2:00.
    const day = new Date(2026, 2, 7).getTime();
    const d = newDraft(1, atMinutes(day, 21 * 60), false, defaults);
    const end = withEnd(d, atMinutes(day, 3 * 60)).end;
    assert.equal(new Date(end).getDate(), 8);
    assert.equal(new Date(end).getHours(), 3);
  } finally {
    if (zone === undefined) delete process.env.TZ;
    else process.env.TZ = zone;
  }
});

test("a copy keeps a Zoom link in its note; a Teams meeting is made anew", async () => {
  const { copyOf, composing } = await import("./eventDraft.ts");
  const zoom: CalendarEvent = {
    id: 3,
    calendar: 1,
    account: 1,
    title: "Vendor call",
    start: 0,
    end: 1,
    allDay: false,
    location: "Room 4",
    notes: "Agenda",
    callLink: "https://us02web.zoom.us/j/123",
    attendees: [],
    response: "organizer",
    canceled: false,
    repeats: false,
    showAs: "busy",
    private: false,
    onlineMeeting: false,
  };
  assert.equal(copyOf(zoom).notes, "Agenda\n\nhttps://us02web.zoom.us/j/123");
  assert.equal(copyOf({ ...zoom, notes: undefined }).notes, "https://us02web.zoom.us/j/123");
  const teams = { ...zoom, callLink: "https://teams.microsoft.com/l/x", onlineMeeting: true };
  assert.equal(copyOf(teams).notes, "Agenda");
  assert.equal(copyOf(teams).teams, true);
  assert.equal(composing({ nativeEvent: { isComposing: true }, keyCode: 13 }), true);
  assert.equal(composing({ nativeEvent: {}, keyCode: 229 }), true);
  assert.equal(composing({ nativeEvent: { isComposing: false }, keyCode: 13 }), false);
});
