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
  cached: () => false,
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

test("Bodies that would show from the cache come back without a turn", () => {
  const cards = thread(10);
  const p = places({ near: new Set(range(1, 6)), known: new Set(cards), cached: (id) => id <= 3, visible: new Set([4]) });
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

test("Collapse All while Find is open lets go of the collapsed bodies and their turns", () => {
  // Review finding UI-1: unmounting, each collapsed body let the cards from
  // before the collapse mount it again, and it held a turn for good.
  const s = bodyScheduler(undefined, () => {}, 4);
  s.cards(thread(5), true);
  s.place(thread(5).map((id) => ({ id, near: id <= 2, visible: id === 1 })));
  for (const id of range(1, 4)) s.content(id, "loading");
  assert.deepEqual(sorted(s.mounted), [1, 2, 3, 4], "the latest waits its turn");
  // Collapse All keeps the latest open. As React commits it, each collapsed
  // card's article goes, then its body says so; then the reader passes the
  // cards still expanded.
  for (const id of range(1, 4)) {
    s.forget(id);
    s.content(id, null);
  }
  s.cards([5], true);
  assert.deepEqual(sorted(s.mounted), [5]);
  s.place([{ id: 5, near: true, visible: true }]);
  s.content(5, "loading");
  s.cards([5]);
  assert.deepEqual(sorted(s.mounted), [5], "Find closed, it stays");
  s.content(5, "ready");
  // One expanded again mounts at once.
  s.mount(2);
  s.content(2, "loading");
  s.cards([2, 5], true);
  assert.deepEqual(sorted(s.mounted), [2, 5]);
});

test("A body whose card is no longer expanded goes, loading or not", () => {
  // Review finding UI-1, its second half: dropped from the cards, a loading
  // body stayed mounted and kept its turn.
  const s = bodyScheduler(1, () => {}, 2);
  s.cards(thread(3));
  s.content(1, "loading");
  s.mount(3);
  s.cards([2, 3]);
  assert.deepEqual(sorted(s.mounted), [3]);
  s.place([{ id: 2, near: true }, { id: 3, near: true }]);
  assert.deepEqual(sorted(s.mounted), [2, 3], "1's load doesn't count");
});

test("A body that reports is mounted, even just after its card was forgotten", () => {
  // As Strict Mode tries a card that mounts: its article goes and its body
  // says so, then the body reports again before the article is placed again.
  const s = bodyScheduler(undefined, () => {}, 1);
  s.cards(thread(2));
  s.place(thread(2).map((id) => ({ id, near: id === 1, visible: id === 1 })));
  s.content(1, "loading");
  s.forget(1);
  s.content(1, null);
  s.content(1, "loading");
  assert.deepEqual(sorted(s.mounted), [1]);
  s.place([{ id: 1, near: true, visible: true }, { id: 2, near: true }]);
  assert.deepEqual(sorted(s.mounted), [1], "its load holds the turn");
  s.content(1, "ready");
  assert.deepEqual(sorted(s.mounted), [1, 2]);
});

test("Bodies the cache let go of take their turn again, even while Find is open", () => {
  // Review finding UI-2: a body that had loaded once came back without a
  // turn, though the cache might have let it go since.
  /** The content cache: room for 7 of these 8 MiB emails in its 64 MiB, the most recent last. */
  const kept: number[] = [];
  const keep = (id: number) => {
    kept.push(id);
    if (kept.length > 7) kept.shift();
  };
  const s = bodyScheduler(undefined, () => {}, 4, (id) => kept.includes(id));
  const cards = thread(20);
  s.cards(cards);
  // Reading down through them, each body loads as it comes near.
  for (const at of cards) {
    const before = s.mounted;
    s.place(cards.map((id) => ({ id, near: Math.abs(id - at) <= 1, visible: id === at })));
    for (const id of s.mounted) {
      if (before.has(id)) continue;
      s.content(id, "loading");
      keep(id);
      s.content(id, "ready");
    }
  }
  assert.deepEqual(kept, range(14, 20));
  // Opening Find, the ones still cached come back at once; the rest take turns.
  s.cards(cards, true);
  const misses = sorted([...s.mounted].filter((id) => !kept.includes(id)));
  assert.deepEqual(misses, range(10, 13));
  for (const id of misses) s.content(id, "loading");
  keep(13);
  s.content(13, "ready");
  assert.ok(s.mounted.has(9) && !s.mounted.has(8), "one done, the next starts");
});

test("A body that loads with embedded images still to fetch keeps its turn", () => {
  // Review finding UI-3: as its content became ready, a body let go of its
  // turn before saying it was fetching embedded images, so a fifth started.
  const s = bodyScheduler(undefined, () => {}, 4);
  s.cards(thread(5));
  s.place(thread(5).map((id) => ({ id, near: true, visible: id <= 2 })));
  for (const id of range(1, 4)) s.content(id, "loading");
  assert.deepEqual(sorted(s.mounted), [1, 2, 3, 4]);
  // 1's content arrives, embedding images that aren't cached: its body says both at once.
  s.content(1, "ready", true);
  assert.deepEqual(sorted(s.mounted), [1, 2, 3, 4], "5 still waits");
  s.content(1, "ready", false);
  assert.deepEqual(sorted(s.mounted), thread(5));
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
  s.content(1, "ready", true);
  assert.deepEqual(sorted(s.mounted), [1]);
  s.content(1, "ready", false);
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
