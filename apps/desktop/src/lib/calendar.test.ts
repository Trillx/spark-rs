// Run with `npm test` (Node's own test runner; it strips the types).
import assert from "node:assert/strict";
import { test } from "node:test";
import type { CalendarEvent } from "../types.ts";
import { alertText, snoozeChoices } from "./calendar.ts";

const event = (reminder: number | undefined, allDay = false): CalendarEvent => ({
  id: 1,
  calendar: 1,
  account: 1,
  title: "Standup",
  start: 0,
  end: 0,
  allDay,
  attendees: [],
  response: "accepted",
  canceled: false,
  repeats: false,
  showAs: "busy",
  private: false,
  reminder,
});

test("An event's alert reads as Spark words it", () => {
  assert.equal(alertText(event(undefined)), undefined);
  assert.equal(alertText(event(0)), "Alert at the event time");
  assert.equal(alertText(event(0, true)), "Alert at the event day");
  assert.equal(alertText(event(15)), "Alert 15m before");
  assert.equal(alertText(event(60)), "Alert 1h before");
  assert.equal(alertText(event(90)), "Alert 1h 30m before");
  assert.equal(alertText(event(18 * 60, true)), "Alert 18h before");
  assert.equal(alertText(event(24 * 60)), "Alert 1d before");
  assert.equal(alertText(event(14 * 24 * 60)), "Alert 14d before");
  assert.equal(alertText(event(24 * 60 + 30)), "Alert 24h 30m before");
});

test("snooze choices offer lengths and the start, soonest first, none past the end", () => {
  const min = 60_000;
  const labels = (start: number, end: number) => snoozeChoices(start, end, 0).map((c) => c.label);
  assert.deepEqual(labels(14 * min, 44 * min), ["1 min", "5 min", "10 min", "At start", "15 min", "30 min"]);
  assert.equal(snoozeChoices(14 * min, 44 * min, 0).find((c) => c.label === "At start")?.until, 14 * min);
  // Started, or starting within the minute: no "At start".
  assert.deepEqual(labels(-5 * min, 12 * min), ["1 min", "5 min", "10 min"]);
  assert.deepEqual(labels(30_000, 60 * min), ["1 min", "5 min", "10 min", "15 min", "30 min"]);
});
