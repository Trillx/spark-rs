// Run with `npm test` (Node's own test runner; it strips the types).
import assert from "node:assert/strict";
import { test } from "node:test";
import type { ScheduledView } from "../types";
import { stuckNotice } from "./outbox.ts";

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
