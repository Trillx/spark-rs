// Run with `npm test` (Node's own test runner; it strips the types).
import assert from "node:assert/strict";
import { test } from "node:test";
import type { Message } from "../types.ts";
import { andMore, canUnsubscribe, settleEach, unsubscribeTargets } from "./unsubscribe.ts";

const mail = (id: number, address: string, extra: Partial<Message> = {}): Message => ({
  id,
  account: 1,
  category: "people",
  state: "inbox",
  unread: true,
  pinned: false,
  priority: false,
  screening: false,
  from: address,
  address,
  timestamp: id,
  subject: "",
  snippet: "",
  ...extra,
});

test("the header decides, and the category only when it wasn't read", () => {
  assert.ok(canUnsubscribe(mail(1, "noreply@shop.com", { category: "notifications", unsubscribe: true })));
  assert.ok(!canUnsubscribe(mail(2, "news@x.com", { category: "newsletters", unsubscribe: false })));
  assert.ok(canUnsubscribe(mail(3, "news@x.com", { category: "newsletters" })));
  assert.ok(!canUnsubscribe(mail(4, "ana@x.com")));
});

test("each sender is asked once, through its newest email", () => {
  const me = (m: Message) => m.address === "me@x.com";
  const threads = [
    [mail(1, "News@X.com", { unsubscribe: true }), mail(2, "me@x.com", { unsubscribe: true })],
    [mail(5, "news@x.com", { unsubscribe: true })],
    [mail(3, "deals@shop.com", { unsubscribe: true }), mail(4, "ana@x.com")],
  ];
  assert.deepEqual(
    unsubscribeTargets(threads, me).map((m) => m.id),
    [5, 3],
  );
  assert.deepEqual(unsubscribeTargets([[mail(6, "ana@x.com")]], me), []);
});

test("a batch names its first sender and counts the rest", () => {
  assert.equal(andMore([]), "");
  assert.equal(andMore(["Ana"]), "Ana");
  assert.equal(andMore(["Ana", "Bob", "Cat"]), "Ana and 2 more");
});

test("work runs a few at a time and every item settles in order", async () => {
  let running = 0;
  let most = 0;
  const results = await settleEach([1, 2, 3, 4, 5], 2, async (n) => {
    running++;
    most = Math.max(most, running);
    await new Promise((resolve) => setTimeout(resolve, 5));
    running--;
    if (n === 3) throw new Error("three");
    return n * 10;
  });
  assert.equal(most, 2);
  assert.deepEqual(
    results.map((r) => (r.status === "fulfilled" ? r.value : String(r.reason))),
    [10, 20, "Error: three", 40, 50],
  );
  assert.deepEqual(await settleEach([], 3, async () => 1), []);
});
