// Run with `npm test` (Node's own test runner; it strips the types).
import assert from "node:assert/strict";
import { test } from "node:test";
import type { ScheduledView } from "../types";
import { draftLeft, goneNotice, markTrying, stuckNotice } from "./outbox.ts";

const send = (id: number, subject: string, failed?: string): ScheduledView => ({
  id,
  account: 1,
  to: "ana@example.com",
  subject,
  snippet: "",
  sendAt: 0,
  failed,
});

const offline = "Couldn't reach the mail server: it closed the connection.";

test("One send that couldn't go yet is named, with why", () => {
  const items = [send(1, "Later"), send(2, "Fwd: Files", offline)];
  assert.equal(
    stuckNotice(items, { id: 2, message: offline }),
    "Couldn't send “Fwd: Files” yet: Couldn't reach the mail server: it closed the connection. It'll keep trying.",
  );
  assert.equal(
    stuckNotice([send(2, " ", offline)], { id: 2 }),
    "Couldn't send “(no subject)” yet: the server can't be reached. It'll keep trying.",
  );
});

test("Several that couldn't go yet are told of together", () => {
  const items = [send(1, "Fwd: Files", offline), send(2, "Later"), send(3, "Re: Lunch", offline)];
  assert.equal(stuckNotice(items, { id: 3, message: offline }), "2 emails couldn't be sent yet. They'll keep trying.");
});

test("A send tried again is marked as being sent, and only it", () => {
  const items = [send(1, "Later"), send(2, "Fwd: Files", offline)];
  const marked = markTrying(items, 2);
  assert.deepEqual(
    marked.map((s) => [s.id, s.sending ?? false]),
    [
      [1, false],
      [2, true],
    ],
  );
  assert.equal(marked[0], items[0]);
  assert.equal(markTrying(items, 3), items, "not listed: nothing changes");
});

test("A change that found its send refused for good opens it in Drafts, and says nothing over the failure", () => {
  assert.equal(draftLeft({ status: "failed", draftId: 7 }), 7);
  assert.equal(goneNotice({ status: "failed", draftId: 7 }), undefined);
  assert.equal(draftLeft({ status: "failed" }), undefined);
  assert.equal(goneNotice({ status: "failed" }), undefined);
  assert.equal(draftLeft({ status: "takenBack", draftId: 8 }), 8);
});

test("Only a send that went is already sent", () => {
  assert.equal(goneNotice({ status: "sent" }), "Already sent");
  assert.equal(draftLeft({ status: "sent" }), undefined);
  assert.equal(goneNotice({ status: "gone" }), "It's no longer waiting to be sent");
  assert.equal(goneNotice({ status: "moved" }), undefined);
});
