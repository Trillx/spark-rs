// Run with `npm test` (Node's own test runner; it strips the types).
import assert from "node:assert/strict";
import { test } from "node:test";
import type { Message } from "../types.ts";
import { CODE_FRESH_MS, copiedText, rowCode } from "./signInCode.ts";

const NOW = 1_790_000_000_000;

const mail = (extra: Partial<Message> = {}): Message => ({
  id: 1,
  account: 1,
  category: "notifications",
  state: "inbox",
  unread: true,
  pinned: false,
  priority: false,
  screening: false,
  from: "Fernway",
  address: "no-reply@fernway.example",
  timestamp: NOW - 60_000,
  subject: "Your sign-in code",
  snippet: "",
  signInCode: "482913",
  ...extra,
});

test("a row offers a fresh code wherever its email is kept", () => {
  assert.equal(rowCode(mail(), NOW), "482913");
  assert.equal(rowCode(mail({ state: "done", unread: false }), NOW), "482913");
  assert.equal(rowCode(mail({ screening: true }), NOW), "482913");
});

test("a row leaves out old codes, trashed and blocked mail, and mail without one", () => {
  assert.equal(rowCode(mail({ timestamp: NOW - CODE_FRESH_MS }), NOW), undefined);
  assert.equal(rowCode(mail({ state: "deleted" }), NOW), undefined);
  assert.equal(rowCode(mail({ state: "blocked" }), NOW), undefined);
  assert.equal(rowCode(mail({ signInCode: undefined }), NOW), undefined);
});

test("the toast says what copying did", () => {
  assert.equal(copiedText("482913", null), "Copied 482913");
  assert.equal(copiedText("482913", "read"), "Copied 482913 and marked as read");
  assert.equal(copiedText("482913", "done"), "Copied 482913 and marked as Done");
});
