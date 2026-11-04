import assert from "node:assert/strict";
import { test } from "node:test";
import { wheelZoom } from "./wheelZoom.ts";

const px = (deltaY: number, timeStamp: number) => ({ deltaY, deltaMode: 0, timeStamp });

test("a mouse notch is one step: up zooms in, down zooms out", () => {
  const wheel = wheelZoom();
  assert.equal(wheel(px(-100, 0)), 1);
  assert.equal(wheel(px(-100, 50)), 1);
  assert.equal(wheel(px(100, 100)), -1);
  assert.equal(wheel({ deltaY: 3, deltaMode: 1, timeStamp: 150 }), -1);
});

test("a pinch's small moves add up to a step", () => {
  const wheel = wheelZoom();
  const steps = [-8, -12, -15, -10, -9].map((d, i) => wheel(px(d, i * 16)));
  assert.deepEqual(steps, [0, 0, 0, 0, 1]);
});

test("a pause forgets travel that never made a step", () => {
  const wheel = wheelZoom();
  assert.equal(wheel(px(-40, 0)), 0);
  assert.equal(wheel(px(-40, 1000)), 0);
});
