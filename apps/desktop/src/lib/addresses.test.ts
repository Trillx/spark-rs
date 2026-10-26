// Run with `npm test` (Node's own test runner; it strips the types).
import assert from "node:assert/strict";
import { test } from "node:test";
import {
  completeMention,
  entryFor,
  joinAddresses,
  mentionAt,
  parseEntry,
  replyRecipients,
  splitAddresses,
  takeFinished,
} from "./addresses.ts";

test("Reply uses Reply-To recipients instead of a no-reply sender", () => {
  assert.deepEqual(replyRecipients({
    address: "no-reply@example.test",
    replyTo: ["Help Desk <help@example.test>"],
    to: ["me@example.test"],
  }, "me@example.test"), { to: ["Help Desk <help@example.test>"], cc: [] });
});

test("Reply All keeps multiple reply addresses and removes self and duplicates across To and Cc", () => {
  assert.deepEqual(replyRecipients({
    address: "no-reply@example.test",
    replyTo: ["Help Desk <help@example.test>", "alternate@example.test", "Me <ME@example.test>"],
    to: ["me@example.test", "HELP@example.test", "Teammate <team@example.test>", "no-reply@example.test"],
    cc: ["team@example.test", "Other <other@example.test>", "ALTERNATE@example.test", "You", "ME@example.test"],
  }, "me@example.test", true), {
    to: ["Help Desk <help@example.test>", "alternate@example.test", "Teammate <team@example.test>"],
    cc: ["Other <other@example.test>"],
  });
});

test("Reply falls back to From and self-sent mail follows up with the original recipients", () => {
  const received = { address: "sender@example.test", to: ["me@example.test"] };
  assert.deepEqual(replyRecipients(received, "me@example.test"), { to: ["sender@example.test"], cc: [] });
  assert.deepEqual(replyRecipients({ ...received, replyTo: [] }, "me@example.test"), {
    to: ["sender@example.test"], cc: [],
  });
  const sent = {
    address: "ME@example.test",
    replyTo: ["Support <support@example.test>"],
    to: ["Client <client@example.test>", "You"],
    cc: ["CLIENT@example.test", "team@example.test", "me@example.test"],
  };
  assert.deepEqual(replyRecipients(sent, "me@example.test"), { to: ["Client <client@example.test>"], cc: [] });
  assert.deepEqual(replyRecipients(sent, "me@example.test", true), {
    to: ["Client <client@example.test>"], cc: ["team@example.test"],
  });
  assert.deepEqual(replyRecipients({ ...received, replyTo: ["me@example.test"] }, "me@example.test"), {
    to: [], cc: [],
  });
});

test("reply destinations with commas in their names remain one composer recipient", () => {
  const recipients = replyRecipients({
    address: "no-reply@example.test",
    replyTo: ["Ruiz, Ana <ana@example.test>", 'Bo "BJ" Lee <bo@example.test>'],
  }, "me@example.test");
  assert.deepEqual(splitAddresses(joinAddresses(recipients.to)).map(parseEntry), [
    { name: "Ruiz, Ana", address: "ana@example.test" },
    { name: 'Bo "BJ" Lee', address: "bo@example.test" },
  ]);
});

test("a field splits at commas and semicolons, but not inside a quoted name", () => {
  assert.deepEqual(splitAddresses(`ana@x.com; "Ruiz, Ana" <ana.r@x.com>,  Bo <bo@y.com> ,`), [
    "ana@x.com",
    `"Ruiz, Ana" <ana.r@x.com>`,
    "Bo <bo@y.com>",
  ]);
  assert.deepEqual(splitAddresses("  "), []);
});

test("a picked contact becomes an entry that survives the trip through the field", () => {
  const people = [
    { name: "Ana Ruiz", address: "ana@x.com" },
    { name: "Ruiz, Ana", address: "ana.r@x.com" },
    { name: 'Bo "BJ" Lee', address: "bo@y.com" },
    { name: "", address: "ops@z.io" },
  ];
  const entries = people.map(entryFor);
  assert.deepEqual(entries, ["Ana Ruiz <ana@x.com>", '"Ruiz, Ana" <ana.r@x.com>', '"Bo \\"BJ\\" Lee" <bo@y.com>', "ops@z.io"]);
  assert.deepEqual(splitAddresses(joinAddresses(entries)).map(parseEntry), people);
  assert.deepEqual(parseEntry("half-typed"), { name: "", address: "half-typed" });
});

test("a comma or semicolon finishes what's typed before it", () => {
  assert.deepEqual(takeFinished("ana@x.com, bo"), { finished: ["ana@x.com"], rest: "bo" });
  assert.deepEqual(takeFinished("a@x.com; b@y.com;"), { finished: ["a@x.com", "b@y.com"], rest: "" });
  assert.deepEqual(takeFinished('"Ruiz, An'), { finished: [], rest: '"Ruiz, An' }, "still inside the quotes");
  assert.deepEqual(takeFinished("bo"), { finished: [], rest: "bo" });
});

test("an @ starting a word begins a mention, up to the caret", () => {
  const at = (text: string, caret = text.length) => mentionAt(text, caret);
  assert.deepEqual(at("Thanks @an"), { start: 7, query: "an" });
  assert.deepEqual(at("@José"), { start: 0, query: "José" });
  assert.deepEqual(at("cc (@bo"), { start: 4, query: "bo" });
  assert.deepEqual(at("Hi @ana maria ruiz"), { start: 3, query: "ana maria ruiz" });
  assert.deepEqual(at("Hi @ana there", 6), { start: 3, query: "an" }, "only what's before the caret");
  assert.deepEqual(at("Hi @"), { start: 3, query: "" });
  assert.equal(at("write to ana@x.com"), null, "an address isn't a mention");
  assert.equal(at("meet @ 5pm"), null);
  assert.equal(at("@ana\nthanks"), null, "a mention stays on its line");
  assert.equal(at("@ana and then a whole sentence"), null, "a mention is a name, not a sentence");
});

test("picking someone puts their name where the mention was", () => {
  const pick = (text: string, caret: number) => completeMention(text, mentionAt(text, caret)!, caret, "Ana Ruiz");
  assert.deepEqual(pick("Thanks @an", 10), { text: "Thanks Ana Ruiz ", caret: 16 });
  assert.deepEqual(pick("Hi @an, see you", 6), { text: "Hi Ana Ruiz, see you", caret: 11 });
  assert.deepEqual(pick("Hi @ana there", 6), { text: "Hi Ana Ruiz there", caret: 11 }, "the rest of the word goes too");
});
