// Run with `npm test` (Node's own test runner; it strips the types).
import assert from "node:assert/strict";
import { test } from "node:test";
import {
  completeMention,
  entryFor,
  joinAddresses,
  mentionAt,
  parseEntry,
  splitAddresses,
  takeFinished,
} from "./addresses.ts";

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
