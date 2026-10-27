// Run with `npm test` (Node's own test runner; it strips the types).
import assert from "node:assert/strict";
import { test } from "node:test";
import {
  READING_CONTEXT,
  bodyScheduler,
  mountOrder,
  mountedBodies,
  movedByUser,
  overlaps,
  scrollToPlace,
  type Places,
} from "./readerViewport.ts";

/** Cards 1…n. */
const thread = (n: number) => Array.from({ length: n }, (_, i) => i + 1);
const range = (from: number, to: number) => thread(to).filter((id) => id >= from);
const sorted = (ids: Iterable<number>) => [...ids].sort((a, b) => a - b);
const places = (p: Partial<Places>): Places => ({
  near: new Set(),
  visible: new Set(),
  known: new Set(),
  loading: new Set(),
  loaded: new Set(),
  ...p,
});

test("A card is near when it comes within the overscan, touching included", () => {
  const screen = { top: 0, bottom: 800 };
  assert.ok(overlaps({ top: 100, bottom: 300 }, screen));
  assert.ok(overlaps({ top: 800, bottom: 900 }, screen), "touching the bottom edge");
  assert.ok(!overlaps({ top: 801, bottom: 900 }, screen));
  assert.ok(overlaps({ top: 1500, bottom: 1700 }, screen, 800));
  assert.ok(!overlaps({ top: -2000, bottom: -801 }, screen, 800));
});

test("Bodies on screen get their turn first, then outwards, the one below first", () => {
  assert.deepEqual(mountOrder(thread(7), new Set([3, 4])), [3, 4, 5, 2, 6, 1, 7]);
  assert.deepEqual(mountOrder(thread(3), new Set()), [1, 2, 3]);
});

test("Only near bodies mount, and no more than the limit start loading", () => {
  const cards = thread(200);
  const near = new Set(range(10, 16));
  const next = mountedBodies(cards, new Set(), places({ near, known: new Set(cards), visible: new Set([12, 13]) }), 4);
  assert.deepEqual(sorted(next), [11, 12, 13, 14]);
});

test("Bodies that loaded before come back without a turn", () => {
  const cards = thread(10);
  const p = places({ near: new Set(range(1, 6)), known: new Set(cards), loaded: new Set([1, 2, 3]), visible: new Set([4]) });
  assert.deepEqual(sorted(mountedBodies(cards, new Set(), p, 2)), [1, 2, 3, 4, 5]);
});

test("A loading body stays mounted after it scrolls away, and keeps its turn", () => {
  const cards = thread(10);
  const p = places({ near: new Set([8, 9]), known: new Set(cards), loading: new Set([1, 2]), visible: new Set([8]) });
  assert.deepEqual(sorted(mountedBodies(cards, new Set([1, 2]), p, 3)), [1, 2, 8]);
});

test("A body that's done unmounts once it's far; one whose place isn't known yet stays", () => {
  const cards = thread(10);
  const p = places({ near: new Set([5]), known: new Set([1, 5]) });
  assert.deepEqual(sorted(mountedBodies(cards, new Set([1, 2, 5]), p)), [2, 5]);
});


test("When nothing changes, the same set comes back", () => {
  const mounted = new Set([2, 3]);
  const p = places({ near: new Set([2, 3]), known: new Set([2, 3]) });
  assert.equal(mountedBodies(thread(5), mounted, p), mounted);
});

test("Reading's first email mounts at once; the rest wait for their place and turn", () => {
  const changes: ReadonlySet<number>[] = [];
  const s = bodyScheduler(5, (m) => changes.push(m), 2);
  s.cards(thread(10));
  assert.deepEqual(sorted(s.mounted), [5], "nothing's placed yet");
  assert.equal(changes.length, 0);
  s.place(thread(10).map((id) => ({ id, near: id >= 4 && id <= 8, visible: id === 5 || id === 6 })));
  assert.deepEqual(sorted(s.mounted), [5, 6], "5 counts as loading until it says otherwise");
  s.content(5, "loading");
  s.content(6, "loading");
  assert.deepEqual(sorted(s.mounted), [5, 6]);
  s.content(5, "ready");
  assert.deepEqual(sorted(s.mounted), [5, 6, 7]);
  s.content(7, "ready");
  s.content(6, "error");
  assert.deepEqual(sorted(s.mounted), [4, 5, 6, 7, 8]);
  assert.equal(changes.at(-1), s.mounted);
});

test("Expand all on a long thread loads only what's near, never more than the limit at once", () => {
  const s = bodyScheduler(undefined, () => {}, 4);
  const cards = thread(200);
  s.cards(cards);
  s.place(cards.map((id) => ({ id, near: id <= 8, visible: id <= 3 })));
  assert.deepEqual(sorted(s.mounted), [1, 2, 3, 4]);
  // Newly mounted bodies start loading; each one that finishes lets the next in.
  const started = new Set<number>();
  const inFlight: number[] = [];
  do {
    for (const id of s.mounted) {
      if (started.has(id)) continue;
      started.add(id);
      inFlight.push(id);
      s.content(id, "loading");
    }
    assert.ok(inFlight.length <= 4, `${inFlight.length} loading at once`);
    const done = inFlight.shift();
    if (done !== undefined) s.content(done, "ready");
  } while (inFlight.length > 0);
  assert.deepEqual(sorted(started), thread(8), "nothing beyond the overscan loaded");
  assert.deepEqual(sorted(s.mounted), thread(8));
});

test("Scrolling past fast: bodies left behind finish before others start", () => {
  const s = bodyScheduler(undefined, () => {}, 2);
  const cards = thread(100);
  s.cards(cards);
  s.place(cards.map((id) => ({ id, near: id <= 3, visible: id === 1 })));
  assert.deepEqual(sorted(s.mounted), [1, 2]);
  s.content(1, "loading");
  s.content(2, "loading");
  s.place(cards.map((id) => ({ id, near: id >= 50 && id <= 53, visible: id === 51 })));
  assert.deepEqual(sorted(s.mounted), [1, 2], "still loading, they keep both turns");
  s.content(1, "ready");
  assert.deepEqual(sorted(s.mounted), [2, 51]);
  s.content(51, "loading");
  s.content(2, "ready");
  assert.deepEqual(sorted(s.mounted), [51, 52]);
});

test("While Find is open every body mounts, a few loads at a time, and stays when far", () => {
  const s = bodyScheduler(undefined, () => {}, 2);
  const cards = thread(6);
  s.cards(cards);
  s.place(cards.map((id) => ({ id, near: id <= 2, visible: id === 1 })));
  for (const id of s.mounted) s.content(id, "ready");
  assert.deepEqual(sorted(s.mounted), [1, 2]);
  s.cards(cards, true);
  assert.deepEqual(sorted(s.mounted), [1, 2, 3, 4], "far ones take their turns too");
  s.content(3, "loading");
  s.content(4, "loading");
  s.place(cards.map((id) => ({ id, near: id >= 5, visible: id === 6 })));
  assert.deepEqual(sorted(s.mounted), [1, 2, 3, 4], "scrolled past, 1 and 2 stay");
  s.content(3, "ready");
  s.content(4, "ready");
  assert.deepEqual(sorted(s.mounted), thread(6));
  s.cards(cards);
  assert.deepEqual(sorted(s.mounted), [5, 6], "closed, the far ones go again");
});

test("A card that collapses lets go of its body and its turn", () => {
  const changes: ReadonlySet<number>[] = [];
  const s = bodyScheduler(undefined, (m) => changes.push(m), 1);
  s.cards(thread(3));
  s.place(thread(3).map((id) => ({ id, near: true, visible: id === 1 })));
  assert.deepEqual(sorted(s.mounted), [1]);
  // Its article goes first, then its body unmounts and says so.
  s.forget(1);
  s.content(1, null);
  assert.deepEqual(sorted(s.mounted), [2]);
  s.cards([2, 3]);
  assert.deepEqual(sorted(s.mounted), [2]);
  assert.deepEqual(changes.map(sorted), [[1], [2]]);
});

test("Bodies that report before the expanded cards are passed in keep their place", () => {
  const s = bodyScheduler(4, () => {}, 2);
  s.content(4, "ready");
  assert.deepEqual(sorted(s.mounted), [4]);
  s.mount(6);
  s.content(6, "loading");
  assert.deepEqual(sorted(s.mounted), [4, 6]);
  s.cards(thread(6));
  s.place(thread(6).map((id) => ({ id, near: id >= 4, visible: id >= 4 })));
  assert.deepEqual(sorted(s.mounted), [4, 5, 6]);
});

test("Embedded images count as loading", () => {
  const s = bodyScheduler(undefined, () => {}, 1);
  s.cards(thread(3));
  s.place(thread(3).map((id) => ({ id, near: true, visible: id === 1 })));
  s.images(1, true);
  s.content(1, "ready");
  assert.deepEqual(sorted(s.mounted), [1]);
  s.images(1, false);
  assert.deepEqual(sorted(s.mounted), [1, 2]);
});

test("An email the user expands mounts at once, beyond the limit", () => {
  const s = bodyScheduler(1, () => {}, 1);
  s.cards(thread(3));
  s.mount(3);
  s.cards(thread(3));
  assert.deepEqual(sorted(s.mounted), [1, 3]);
  s.place([{ id: 1, near: true }, { id: 2, near: true }, { id: 3, near: true }]);
  assert.deepEqual(sorted(s.mounted), [1, 3], "2 waits: 1 and 3 are loading");
});

test("A body seen again after scrolling back mounts in its last height", () => {
  const s = bodyScheduler(undefined, () => {}, 1);
  s.cards([1]);
  s.place([{ id: 1, near: true }]);
  s.content(1, "ready");
  s.measure(1, 640);
  s.place([{ id: 1, near: false }]);
  assert.equal(s.mounted.size, 0);
  s.measure(1, 0);
  assert.equal(s.height(1), 640, "a hidden reader's zero isn't its height");
  assert.equal(s.height(2), undefined);
  s.place([{ id: 1, near: true }]);
  assert.deepEqual(sorted(s.mounted), [1]);
});

test("Reading starts with a little of the email before it in view", () => {
  assert.equal(scrollToPlace(0, 900, READING_CONTEXT), 800);
  assert.equal(scrollToPlace(300, 50, 100), 250);
  assert.equal(scrollToPlace(0, 40, 100), 0, "never above the start");
});

test("Only a move with the same content is the user's", () => {
  assert.ok(movedByUser({ top: 800, height: 5000 }, { top: 700, height: 5000 }));
  assert.ok(!movedByUser({ top: 800, height: 5000 }, { top: 1100, height: 5300 }));
  assert.ok(!movedByUser({ top: 800, height: 5000 }, { top: 801, height: 5000 }));
});
