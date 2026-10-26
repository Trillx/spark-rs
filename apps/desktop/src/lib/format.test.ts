// Run with `npm test` (Node's own test runner; it strips the types).
import assert from "node:assert/strict";
import { test } from "node:test";
import { futurePeriod, laterPresets, laterTime, presetOf } from "./format.ts";

/** Local time, so the tests hold in any time zone. */
const at = (y: number, m: number, d: number, h = 0, min = 0) => new Date(y, m - 1, d, h, min).getTime();

test("Snooze presets come due as Spark's do", () => {
  const wednesdayNoon = at(2026, 10, 28, 12);
  assert.equal(laterTime(presetOf("thisEvening"), wednesdayNoon), at(2026, 10, 28, 20));
  assert.equal(laterTime(presetOf("tomorrowEvening"), wednesdayNoon), at(2026, 10, 29, 18));
  assert.equal(laterTime(presetOf("laterThisWeek"), wednesdayNoon), at(2026, 10, 30, 9));
  assert.equal(laterTime(presetOf("weekend"), wednesdayNoon), at(2026, 10, 31, 9));
  assert.equal(laterTime(presetOf("nextWeek"), wednesdayNoon), at(2026, 11, 2, 9));
  assert.equal(laterTime(presetOf("inAMonth"), wednesdayNoon), at(2026, 11, 28, 9));
});

test("Only presets that make sense now are offered", () => {
  const keys = (t: number) => laterPresets(t).map((p) => p.key);
  assert.ok(keys(at(2026, 10, 28, 12)).includes("thisEvening"));
  assert.ok(!keys(at(2026, 10, 28, 19, 30)).includes("thisEvening"));
  assert.ok(keys(at(2026, 10, 28, 12)).includes("laterThisWeek"));
  assert.ok(!keys(at(2026, 10, 29, 12)).includes("laterThisWeek"), "not on a Thursday");
});

test("Snoozed sections go by when mail comes back", () => {
  const now = at(2026, 10, 28, 12);
  assert.equal(futurePeriod(at(2026, 10, 28, 18), now), "Later today");
  assert.equal(futurePeriod(at(2026, 10, 29, 9), now), "Tomorrow");
  assert.equal(futurePeriod(at(2026, 11, 2, 9), now), "This week");
  assert.equal(futurePeriod(at(2026, 11, 9, 9), now), "Next week");
  assert.equal(futurePeriod(at(2026, 12, 1, 9), now), "Later");
});
