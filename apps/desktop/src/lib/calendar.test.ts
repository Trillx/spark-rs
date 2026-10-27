// Run with `npm test` (Node's own test runner; it strips the types).
import assert from "node:assert/strict";
import { test } from "node:test";
import type { CalendarEvent } from "../types.ts";
import { alertText } from "./calendar.ts";

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
