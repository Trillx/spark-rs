// Run with `npm test` (Node's own test runner; it strips the types).
import assert from "node:assert/strict";
import { test } from "node:test";
import type { Message } from "../types.ts";
import { covers, decidableDomain, domain, domainOf, forSenders, predict, sender, settled } from "./gatekeeper.ts";

const mail = (id: number, address: string, state: Message["state"] = "inbox", screening = true): Message => ({
  id,
  account: 1,
  category: "people",
  state,
  unread: true,
  pinned: false,
  priority: false,
  screening,
  from: address,
  address,
  timestamp: id,
  subject: "",
  snippet: "",
});

test("decisions cover one address, or everyone at a domain", () => {
  assert.equal(domainOf("Ana@Parts.COM"), "parts.com");
  assert.equal(domainOf("nobody"), undefined);
  assert.ok(covers(sender("ANA@parts.com"), "ana@Parts.com"));
  assert.ok(!covers(sender("ana@parts.com"), "bob@parts.com"));
  assert.ok(covers(domain("@Parts.com"), "bob@parts.com"));
  assert.ok(!covers(domain("parts.com"), "bob@sub.parts.com"));
  assert.equal(decidableDomain("ana@Parts.com"), "parts.com");
  assert.equal(decidableDomain("ana@gmail.com"), undefined);
  assert.equal(decidableDomain("ana@hotmail.co.uk"), undefined);
});

test("mail follows its sender's decision", () => {
  const waiting = mail(1, "a@x.com");
  assert.deepEqual(settled(waiting, "blocked"), { ...waiting, state: "blocked", screening: false });
  assert.deepEqual(settled(waiting, "accepted"), { ...waiting, screening: false });
  const blocked = mail(2, "a@x.com", "blocked", false);
  assert.deepEqual(settled(blocked, "accepted"), { ...blocked, state: "inbox" });
  assert.deepEqual(settled(blocked, null), { ...blocked, state: "inbox", screening: true });
  const done = mail(3, "a@x.com", "done", false);
  assert.deepEqual(settled(done, "blocked"), done, "only inbox mail goes to Blocked");
});

test("one change per sender, and only single senders are predicted", () => {
  const items = [mail(1, "a@x.com"), mail(2, "A@x.com"), mail(3, "b@x.com")];
  assert.deepEqual(forSenders(items, "accepted"), [
    { screen: sender("a@x.com"), decision: "accepted" },
    { screen: sender("b@x.com"), decision: "accepted" },
  ]);
  const after = predict(items, [{ screen: sender("a@x.com"), decision: "blocked" }, { screen: domain("x.com"), decision: "blocked" }]);
  assert.deepEqual(after.map((m) => m.state), ["blocked", "blocked", "inbox"]);
});
