// Run with `npm test` (Node's own test runner; it strips the types).
import assert from "node:assert/strict";
import { test } from "node:test";
import { estimateBytes, loadCache } from "./cache.ts";

/** A body like `MessageContent`, about `chars` characters of HTML, the same size whatever the id. */
const body = (id: number, chars: number) => ({
  text: [`Message ${String(id).padStart(4, "0")}`],
  html: `<p>${id}</p>`.padEnd(chars, "x"),
  to: ["Ana Ruiz <ana@parts.com>"],
  cc: [],
  attachments: [],
});

/** Embedded images like `InlineImages`, each about `chars` characters of data URL. */
const images = (id: number, count: number, chars: number) =>
  Object.fromEntries(
    Array.from({ length: count }, (_, i) => [`img${i}@${id}`, "data:image/png;base64,".padEnd(chars, "A")]),
  );

/** A server whose answers come when the test says, counting how often it's asked. */
function server<V>(answer: (id: number) => V) {
  const asked: number[] = [];
  const waiting: (() => void)[] = [];
  return {
    asked,
    fetch: (id: number) => {
      asked.push(id);
      return new Promise<V>((resolve) => waiting.push(() => resolve(answer(id))));
    },
    /** Answers the `count` oldest asks still waiting, else all of them. */
    answer: async (count = waiting.length) => {
      waiting.splice(0, count).forEach((resolve) => resolve());
      await new Promise((resolve) => setTimeout(resolve));
    },
  };
}

test("sizes count two bytes a character, keys included, plus each value's overhead", () => {
  assert.equal(estimateBytes(""), 16);
  assert.equal(estimateBytes("abcd"), 16 + 8);
  assert.equal(estimateBytes("é😀"), 16 + 6);
  assert.equal(estimateBytes(["ab", "c"]), 16 + (16 + 4) + (16 + 2));
  assert.equal(estimateBytes({ k: "vv" }), 16 + (16 + 2) + (16 + 4));
  assert.equal(estimateBytes({ html: null, size: 3, ok: true }), 16 + 3 * 16 + (2 * 4 + 2 * 4 + 2 * 2) + 3 * 16);
  const content = body(1, 100_000);
  assert.ok(estimateBytes(content) > 2 * 100_000);
});

test("hundreds of large bodies stay within the budget, the newest kept", async () => {
  const budget = 4_000_000;
  const cache = loadCache({ fetch: async (id: number) => body(id, 150_000), budget, entryMax: budget / 4 });
  for (let id = 1; id <= 300; id++) {
    const content = await cache.load(id);
    assert.equal(content.text[0], `Message ${String(id).padStart(4, "0")}`);
    assert.ok(cache.weight() <= budget, `${cache.weight()} bytes after ${id}`);
  }
  const each = estimateBytes(body(0, 150_000));
  const fits = Math.floor(budget / each);
  assert.equal(cache.weight(), fits * each);
  assert.ok(cache.get(300) && cache.get(301 - fits));
  assert.equal(cache.get(300 - fits), undefined);
  assert.equal(cache.get(1), undefined);
});

test("hundreds of image sets stay within the budget", async () => {
  const budget = 10_000_000;
  const cache = loadCache({
    fetch: async (id: number) => images(id, 1 + (id % 5), 50_000 * (1 + (id % 7))),
    budget,
    entryMax: budget / 4,
  });
  for (let id = 1; id <= 400; id++) {
    await cache.load(id);
    assert.ok(cache.weight() <= budget, `${cache.weight()} bytes after ${id}`);
  }
  assert.ok(cache.get(400));
});

test("a hit marks the value as just used, so the next one out is older", async () => {
  const each = estimateBytes(body(0, 1000));
  const cache = loadCache({ fetch: async (id: number) => body(id, 1000), budget: 3 * each, entryMax: each });
  for (const id of [1, 2, 3]) await cache.load(id);
  assert.ok(cache.get(1));
  await cache.load(4);
  assert.equal(cache.get(2), undefined);
  await cache.load(3);
  await cache.load(5);
  assert.equal(cache.get(1), undefined);
  assert.ok(cache.get(3) && cache.get(4) && cache.get(5));
});

test("a value over the entry limit is handed over but not kept, and pushes nothing out", async () => {
  const s = server((id: number) => body(id, id === 9 ? 600_000 : 1000));
  const cache = loadCache({ fetch: s.fetch, budget: 4_000_000, entryMax: 1_000_000 });
  const small = cache.load(1);
  await s.answer();
  await small;
  const before = cache.weight();
  const big = cache.load(9);
  await s.answer();
  assert.equal((await big).html.length, 600_000);
  assert.equal(cache.get(9), undefined);
  assert.equal(cache.weight(), before);
  assert.ok(cache.get(1));
  const again = cache.load(9);
  await s.answer();
  await again;
  assert.deepEqual(s.asked, [1, 9, 9]);
});

test("asks while a value loads share one fetch; a failed one is tried again", async () => {
  let fail = true;
  const asked: number[] = [];
  const cache = loadCache({
    fetch: async (id: number) => {
      asked.push(id);
      if (fail) throw new Error("offline");
      return body(id, 100);
    },
    budget: 1_000_000,
    entryMax: 100_000,
  });
  const first = cache.load(1);
  const second = cache.load(1);
  assert.equal(first, second);
  await assert.rejects(first, /offline/);
  assert.equal(cache.get(1), undefined);
  fail = false;
  await cache.load(1);
  await cache.load(1);
  assert.deepEqual(asked, [1, 1]);
});

test("a load under way when the cache clears answers its asker but isn't kept", async () => {
  const s = server((id: number) => body(id, 100));
  const cache = loadCache({ fetch: s.fetch, budget: 1_000_000, entryMax: 100_000 });
  const old = cache.load(1);
  cache.clear();
  const fresh = cache.load(1);
  assert.notEqual(fresh, old);
  assert.deepEqual(s.asked, [1, 1]);
  await s.answer(1);
  assert.ok(await old);
  assert.equal(cache.get(1), undefined);
  // The old load settling mustn't drop the new one, or this ask would fetch again.
  assert.equal(cache.load(1), fresh);
  await s.answer();
  await fresh;
  assert.ok(cache.get(1));
  assert.deepEqual(s.asked, [1, 1]);
  cache.clear();
  assert.equal(cache.weight(), 0);

  const late = cache.load(2);
  cache.clear();
  await s.answer();
  assert.ok(await late);
  assert.equal(cache.get(2), undefined);
  assert.equal(cache.weight(), 0);
});

test("forgetting a value lets its load under way go: that answers its asker, but only a new one is kept", async () => {
  const s = server((id: number) => body(id, 100));
  const cache = loadCache({ fetch: s.fetch, budget: 1_000_000, entryMax: 100_000 });
  const first = cache.load(2);
  await s.answer();
  await first;
  cache.forget(2);
  assert.equal(cache.get(2), undefined);
  assert.equal(cache.weight(), 0);

  const old = cache.load(1);
  cache.forget(1);
  const fresh = cache.load(1);
  assert.notEqual(fresh, old);
  await s.answer(1);
  assert.ok(await old);
  assert.equal(cache.get(1), undefined);
  // The old load settling mustn't drop the new one, or this ask would fetch again.
  assert.equal(cache.load(1), fresh);
  await s.answer();
  await fresh;
  assert.ok(cache.get(1));
  assert.deepEqual(s.asked, [2, 1, 1]);
  // Another id's value and load are left be.
  const other = cache.load(3);
  cache.forget(1);
  await s.answer();
  await other;
  assert.ok(cache.get(3));
  assert.equal(cache.get(1), undefined);
});

test("update replaces a kept value and weighs it again, but keeps nothing new", async () => {
  const cache = loadCache({ fetch: async (id: number) => body(id, 100), budget: 1_000_000, entryMax: 100_000 });
  cache.update(1, (content) => ({ ...content, html: "changed" }));
  assert.equal(cache.get(1), undefined);
  await cache.load(1);
  cache.update(1, (content) => ({ ...content, html: "x".repeat(10_000) }));
  assert.equal(cache.get(1)?.html.length, 10_000);
  assert.equal(cache.weight(), estimateBytes(cache.get(1)));
  cache.update(1, (content) => ({ ...content, html: "x".repeat(60_000) }));
  assert.equal(cache.get(1), undefined);
  assert.equal(cache.weight(), 0);
});
