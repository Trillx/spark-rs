// Run with `npm test` (Node's own test runner; it strips the types).
import assert from "node:assert/strict";
import { test } from "node:test";
import type { TestContext } from "node:test";
import type { Message } from "../types.ts";
import type { SearchHits } from "./layout.ts";
import { indexChanged, indexSearches } from "./searchIndex.ts";

const DELAY = 150;

const mail = (id: number): Message => ({
  id,
  account: 1,
  category: "people",
  state: "inbox",
  unread: true,
  pinned: false,
  priority: false,
  screening: false,
  from: "Ana Ruiz",
  address: "ana@parts.com",
  to: ["Me <me@x.com>"],
  timestamp: 1_000 + id,
  subject: `Message ${id}`,
  snippet: "",
});

const plain = (hits: SearchHits) =>
  Object.fromEntries([...hits].map(([text, ids]) => [text, [...ids].sort((a, b) => a - b)]));

/** Lets answers that are in come back. */
const settle = () => new Promise((resolve) => setImmediate(resolve));

/** Searches against an index answering from `table`, recording what it's asked and what's shown. */
function searchesOf(t: TestContext, table: Record<string, number[] | null>) {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  const asked: string[] = [];
  const shown: SearchHits[] = [];
  const searches = indexSearches({
    ask: (text) => {
      asked.push(text);
      return Promise.resolve(text in table ? table[text] : []);
    },
    show: (hits) => shown.push(hits),
    delay: DELAY,
  });
  const rest = async () => {
    t.mock.timers.tick(DELAY);
    await settle();
  };
  return { searches, asked, shown, rest };
}

test("only new mail or changed words change what the index finds", () => {
  const before = [mail(1), mail(2), mail(3)];
  const changed = (change: Partial<Message>) => before.map((m) => (m.id === 3 ? { ...m, ...change } : m));
  assert.equal(indexChanged(before, before), false);
  // Read, pinned, done, reloaded as they were, sorted otherwise, or gone.
  assert.equal(indexChanged(before, changed({ unread: false, pinned: true, state: "done", folders: [4] })), false);
  assert.equal(indexChanged(before, structuredClone(before)), false);
  assert.equal(indexChanged(before, [before[2], before[0]]), false);
  // New mail, or a draft saved again.
  assert.equal(indexChanged(before, [mail(4), ...before]), true);
  assert.equal(indexChanged(before, changed({ subject: "Plans" })), true);
  assert.equal(indexChanged(before, changed({ to: ["Zoë Ortiz <zoe@example.com>"] })), true);
  assert.equal(indexChanged(before, changed({ timestamp: 9_000 })), true);
});

test("typing fast asks the index once, for the text it rests on", async (t) => {
  const { searches, asked, shown, rest } = searchesOf(t, { invo: [1, 2] });
  const mailbox = [mail(1), mail(2), mail(3)];
  for (const text of ["i", "in", "inv", "invo"]) {
    searches.want([text], mailbox);
    t.mock.timers.tick(DELAY - 50);
  }
  await rest();
  assert.deepEqual(asked, ["invo"]);
  assert.deepEqual(shown.map(plain), [{ invo: [1, 2] }]);
});

test("marking mail read or pinned asks nothing again, nor holds up what's asked", async (t) => {
  const { searches, asked, shown, rest } = searchesOf(t, { plans: [2], weekly: [1, 3] });
  const mailbox = [mail(1), mail(2), mail(3)];
  searches.want(["plans", "weekly"], mailbox);
  t.mock.timers.tick(DELAY - 50);
  const read = mailbox.map((m) => ({ ...m, unread: false }));
  searches.want(["plans", "weekly"], read);
  t.mock.timers.tick(50);
  await settle();
  assert.deepEqual(asked, ["plans", "weekly"]);
  searches.want(["plans", "weekly"], read.map((m) => (m.id === 1 ? { ...m, pinned: true } : m)));
  await rest();
  assert.deepEqual(asked, ["plans", "weekly"]);
  assert.deepEqual(shown.map(plain), [{ plans: [2], weekly: [1, 3] }]);
});

test("new mail asks every text again, the old answers showing until then", async (t) => {
  const table = { plans: [2], weekly: [1] };
  const { searches, asked, shown, rest } = searchesOf(t, table);
  const mailbox = [mail(1), mail(2)];
  searches.want(["plans", "weekly"], mailbox);
  await rest();
  table.plans = [2, 3];
  searches.want(["plans", "weekly"], [mail(3), ...mailbox]);
  await settle();
  assert.equal(shown.length, 1);
  await rest();
  assert.deepEqual(asked, ["plans", "weekly", "plans", "weekly"]);
  assert.deepEqual(plain(shown[1]), { plans: [2, 3], weekly: [1] });
});

test("mail that comes while nothing is searched asks again when it is", async (t) => {
  const table = { plans: [2] };
  const { searches, asked, shown, rest } = searchesOf(t, table);
  const mailbox = [mail(1), mail(2)];
  searches.want(["plans"], mailbox);
  await rest();
  searches.want([], mailbox);
  table.plans = [2, 3];
  const more = [mail(3), ...mailbox];
  searches.want([], more);
  searches.want(["plans"], more);
  await rest();
  assert.deepEqual(asked, ["plans", "plans"]);
  assert.deepEqual(shown.map(plain), [{ plans: [2] }, { plans: [2, 3] }]);
});

test("smart folders' answers stay while the search changes", async (t) => {
  const { searches, asked, shown, rest } = searchesOf(t, { invoices: [1], bracket: [2], steel: [3] });
  const mailbox = [mail(1), mail(2), mail(3)];
  searches.want(["bracket", "invoices"], mailbox);
  await rest();
  searches.want(["steel", "invoices"], mailbox);
  await rest();
  // The search cleared: what's left shows at once.
  searches.want(["invoices"], mailbox);
  assert.deepEqual(asked, ["bracket", "invoices", "steel"]);
  assert.deepEqual(shown.map(plain), [
    { bracket: [2], invoices: [1] },
    { steel: [3], invoices: [1] },
    { invoices: [1] },
  ]);
});

test("one search at a time, and a late answer isn't shown for the current text", async (t) => {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  const answer = new Map<string, (ids: number[]) => void>();
  let running = 0;
  let most = 0;
  const shown: SearchHits[] = [];
  const searches = indexSearches({
    ask: (text) => {
      most = Math.max(most, ++running);
      return new Promise((resolve) =>
        answer.set(text, (ids) => {
          running--;
          resolve(ids);
        }),
      );
    },
    show: (hits) => shown.push(hits),
    delay: DELAY,
  });
  const mailbox = [mail(1), mail(2)];
  searches.want(["slow"], mailbox);
  t.mock.timers.tick(DELAY);
  await settle();
  searches.want(["fast"], mailbox);
  t.mock.timers.tick(DELAY);
  await settle();
  assert.deepEqual([...answer.keys()], ["slow"]);

  answer.get("slow")!([1]);
  await settle();
  assert.deepEqual([...answer.keys()], ["slow", "fast"]);
  assert.equal(shown.length, 0);
  answer.get("fast")!([2]);
  await settle();
  assert.deepEqual(shown.map(plain), [{ fast: [2] }]);
  assert.equal(most, 1);

  // The late answer was kept: back to its text, it shows at once.
  searches.want(["slow"], mailbox);
  assert.deepEqual(plain(shown[1]), { slow: [1] });
  assert.equal(answer.size, 2);
});

test("a failed search is asked again when the mail next changes, even just its flags", async (t) => {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  const asked: string[] = [];
  const shown: SearchHits[] = [];
  const searches = indexSearches({
    ask: (text) => {
      asked.push(text);
      return asked.length === 1 ? Promise.reject(new Error("locked")) : Promise.resolve([1]);
    },
    show: (hits) => shown.push(hits),
    delay: DELAY,
  });
  const mailbox = [mail(1), mail(2)];
  searches.want(["plans"], mailbox);
  t.mock.timers.tick(DELAY);
  await settle();
  assert.deepEqual(shown, []);
  searches.want(["plans"], mailbox.map((m) => ({ ...m, unread: false })));
  t.mock.timers.tick(DELAY);
  await settle();
  assert.deepEqual(asked, ["plans", "plans"]);
  assert.deepEqual(shown.map(plain), [{ plans: [1] }]);
});

test("nothing is asked after stop, and without an index nothing is shown", async (t) => {
  const { searches, asked, shown, rest } = searchesOf(t, { plans: null });
  const mailbox = [mail(1)];
  searches.want(["plans"], mailbox);
  searches.stop();
  await rest();
  assert.deepEqual(asked, []);
  // Wanted again (React mounts effects twice in development).
  searches.want(["plans"], mailbox);
  await rest();
  assert.deepEqual(asked, ["plans"]);
  assert.deepEqual(shown, []);
});
