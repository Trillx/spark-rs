// Run with `npm test` (Node's own test runner; it strips the types).
import assert from "node:assert/strict";
import { test } from "node:test";
import {
  anchoredTop,
  blockAt,
  blockOffsets,
  blocksBetween,
  covers,
  gaps,
  joinSpans,
  reaches,
  stretchAround,
  type Stretch,
} from "./virtualList.ts";

/** A Focused list's start: the Priority card, New senders, a date header, then rows. */
const FOCUSED = [399, 192, 40, 70, 70, 70, 40, 60, 70];

test("Blocks of any height sit one after another", () => {
  assert.deepEqual(blockOffsets([]), [0]);
  assert.deepEqual(blockOffsets(FOCUSED), [0, 399, 591, 631, 701, 771, 841, 881, 941, 1011]);
});

test("A point falls in the block that ends below it", () => {
  const offsets = blockOffsets(FOCUSED);
  assert.equal(blockAt(offsets, -20), 0, "above the list: the first block");
  assert.equal(blockAt(offsets, 0), 0);
  assert.equal(blockAt(offsets, 398.5), 0);
  assert.equal(blockAt(offsets, 399), 1, "a block's end is the next one's start");
  assert.equal(blockAt(offsets, 940), 7);
  assert.equal(blockAt(offsets, 1011), 9, "past the end: the block count");
  assert.equal(blockAt(blockOffsets([0, 70]), 0), 1, "an empty block takes no room");
});

test("A stretch takes in every block that shows in it, even partly", () => {
  const offsets = blockOffsets(FOCUSED);
  assert.deepEqual(blocksBetween(offsets, 0, 1011), { start: 0, end: 9 });
  assert.deepEqual(blocksBetween(offsets, 600, 700), { start: 2, end: 4 });
  assert.deepEqual(blocksBetween(offsets, 631, 701), { start: 3, end: 4 }, "edges that only touch stay out");
  assert.deepEqual(blocksBetween(offsets, -500, 10), { start: 0, end: 1 });
  assert.deepEqual(blocksBetween(offsets, 2000, 3000), { start: 9, end: 9 });
  assert.deepEqual(blocksBetween(blockOffsets([]), 0, 800), { start: 0, end: 0 });
});

test("Spans join where they meet and leave space where they don't", () => {
  const spans = joinSpans([
    { start: 40, end: 50 },
    { start: 3, end: 3 },
    { start: 2, end: 5 },
    { start: 5, end: 9 },
    { start: 45, end: 60 },
  ]);
  assert.deepEqual(spans, [
    { start: 2, end: 9 },
    { start: 40, end: 60 },
  ]);
  const offsets = blockOffsets(new Array(100).fill(70));
  assert.deepEqual(gaps(offsets, spans), [140, 31 * 70, 40 * 70]);
  assert.deepEqual(gaps(offsets, []), [7000]);
  const total = (heights: number[]) => heights.reduce((a, b) => a + b, 0);
  const rendered = spans.reduce((sum, s) => sum + offsets[s.end] - offsets[s.start], 0);
  assert.equal(total(gaps(offsets, spans)) + rendered, 7000, "gaps and blocks add up to the whole list");
});

test("What's rendered is the screen and another one either side", () => {
  assert.deepEqual(stretchAround(1000, 800), { from: 200, to: 2600 });
  const stretch = stretchAround(1000, 800);
  assert.ok(covers(stretch, 1000, 800, 50_000));
  assert.ok(covers(stretch, 600, 800, 50_000), "half a screen up still has half a screen above");
  assert.ok(!covers(stretch, 590, 800, 50_000));
  assert.ok(!covers(stretch, 1410, 800, 50_000));
  assert.ok(!covers(stretch, 1000, 1200, 50_000), "a taller screen needs more");
  // The ends of the list need nothing past them.
  assert.ok(covers(stretchAround(0, 800), 0, 800, 50_000));
  assert.ok(covers({ from: 1000, to: 3000 }, 2200, 800, 3000));
  assert.ok(!covers({ from: 9000, to: 12000 }, 400, 800, 1200), "a list that shrank under it");
  assert.ok(reaches({ from: -800, to: 1600 }, -800, 1600, 50_000));
  assert.ok(!reaches({ from: 0, to: 1600 }, 0, 2400, 50_000));
});

test("Scrolling a long list end to end keeps the rendered rows near the screen", () => {
  // 50,000 rows under date headers, with a Priority card and New senders on top.
  const heights = [399, 192];
  for (let i = 0; i < 50_000; i++) heights.push(i % 1000 === 0 ? 40 : 70);
  const offsets = blockOffsets(heights);
  const total = offsets.at(-1)!;
  const screen = 900;
  let stretch: Stretch = stretchAround(0, screen);
  let renders = 0;
  let most = 0;
  for (let top = 0; top <= total - screen; top += 37) {
    if (!covers(stretch, top, screen, total)) {
      stretch = stretchAround(top, screen);
      renders++;
    }
    const { start, end } = blocksBetween(offsets, stretch.from, stretch.to);
    most = Math.max(most, end - start);
    const showing = blocksBetween(offsets, top, top + screen);
    assert.ok(start <= showing.start && end >= showing.end, `the screen at ${top} is rendered`);
  }
  assert.ok(most <= Math.ceil((3 * screen) / 40) + 2, `${most} blocks rendered at most`);
  assert.ok(most <= 42, "about three screens of 70px rows");
  // Re-rendered about every half screen, not on every scroll.
  assert.ok(renders <= Math.ceil(total / (screen / 2)) + 1, `${renders} renders`);
});

test("Blocks added above keep what showed in place", () => {
  const before = blockOffsets([40, 70, 70, 70, 70]);
  // Two rows arrive under the date header; everything else moves 140px down.
  const after = blockOffsets([40, 70, 70, 70, 70, 70, 70]);
  const moved = (i: number) => (i === 0 ? 0 : i + 2);
  assert.equal(anchoredTop(before, after, 150, moved), 290);
  assert.equal(anchoredTop(before, after, 20, moved), 20, "the header showing stays put");
  assert.equal(anchoredTop(before, after, 150, () => undefined), undefined, "the row that showed is gone");
  assert.equal(anchoredTop(before, after, -36, moved), undefined, "the search count showed above the blocks");
  assert.equal(anchoredTop(before, after, 320, moved), undefined, "past the end");
  // Removed above: the screen moves up with what showed.
  assert.equal(anchoredTop(after, before, 290, (i) => (i < 3 ? undefined : i - 2)), 150);
});
