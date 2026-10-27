// Run with `npm test` (Node's own test runner; it strips the types).
import assert from "node:assert/strict";
import { test } from "node:test";
import type { AutoLabel, Message } from "../types.ts";
import { AUTO_LABELS, AUTO_LABEL_ORDER, conversationLabel, inLabelList } from "./autoLabels.ts";

const mail = (id: number, autoLabel?: AutoLabel): Message => ({
  id,
  account: 1,
  category: "people",
  state: "inbox",
  unread: false,
  pinned: false,
  priority: false,
  screening: false,
  from: "Dana",
  address: "dana@example.com",
  timestamp: id,
  subject: "",
  snippet: "",
  autoLabel,
});

test("a conversation shows its newest email's label", () => {
  assert.equal(conversationLabel([mail(1, "toRespond"), mail(2, "actioned"), mail(3)]), "actioned");
  assert.equal(conversationLabel([mail(1, "fyi"), mail(2)]), "fyi");
  assert.equal(conversationLabel([mail(1), mail(2)]), undefined);
  assert.equal(conversationLabel([]), undefined);
});

test("a thread is in the list of the label its row shows", () => {
  const own = new Set(["me@example.com"]);
  const mine = (id: number): Message => ({ ...mail(id), address: "Me@example.com" });
  // A request, then "never mind, handled it", then the user's own reply.
  const thread = [mail(1, "toRespond"), mail(2, "actioned"), mine(3)];
  const listed = (label: AutoLabel, emails: Message[]) =>
    emails.filter((m) => inLabelList(label, m, emails, own)).map((m) => m.id);
  assert.deepEqual(listed("toRespond", thread), []);
  assert.deepEqual(listed("actioned", thread), [1, 2]);
  // A reply without a label of its own (labels turned off since) is listed with it, so its row is the
  // newest email's; the user's own isn't.
  assert.deepEqual(listed("fyi", [mail(4, "fyi"), mail(5), mine(6)]), [4, 5]);
  assert.deepEqual(listed("fyi", [mail(7)]), []);
});

test("every label has a name and a color, in Spark's order", () => {
  assert.deepEqual(AUTO_LABEL_ORDER, [
    "toRespond",
    "fyi",
    "comment",
    "notification",
    "meetingUpdate",
    "actioned",
    "marketing",
    "news",
  ]);
  for (const label of AUTO_LABEL_ORDER) {
    assert.ok(AUTO_LABELS[label].name);
    assert.match(AUTO_LABELS[label].color, /^#[0-9a-f]{6}$/);
  }
  assert.equal(AUTO_LABELS.toRespond.name, "To respond");
});
