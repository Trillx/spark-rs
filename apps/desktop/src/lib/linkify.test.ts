import assert from "node:assert/strict";
import { test } from "node:test";
import { findLinks } from "./linkify.ts";

/** Each address found, as written, and where it goes. */
const links = (text: string) => findLinks(text).map(({ start, end, href }) => [text.slice(start, end), href]);

test("a web address written out in the text is a link", () => {
  const url = "https://erp.example.com/ui/annotate/item/5f0c2a/9b1e4d";
  assert.deepEqual(links(`Approve it here: ${url}\n17 checkpoints, 11 critical.`), [[url, url]]);
  assert.deepEqual(links("see http://x.com/a?b=1&c=2#d and HTTPS://Y.ORG"), [
    ["http://x.com/a?b=1&c=2#d", "http://x.com/a?b=1&c=2#d"],
    ["HTTPS://Y.ORG", "HTTPS://Y.ORG"],
  ]);
  assert.deepEqual(links("nothing to see here: 2.5 mm, 112.88° ±0.5°"), []);
});

test("punctuation that ends the sentence isn't part of the address", () => {
  assert.deepEqual(links("Go to https://x.com/a."), [["https://x.com/a", "https://x.com/a"]]);
  assert.deepEqual(links('"https://x.com/a", then'), [["https://x.com/a", "https://x.com/a"]]);
  assert.deepEqual(links("(see https://x.com/a)"), [["https://x.com/a", "https://x.com/a"]]);
  assert.deepEqual(links("(https://en.wikipedia.org/wiki/Mercury_(planet))"), [
    ["https://en.wikipedia.org/wiki/Mercury_(planet)", "https://en.wikipedia.org/wiki/Mercury_(planet)"],
  ]);
  assert.deepEqual(links("just https://."), []);
});

test("www addresses open over http, as in Gmail and Outlook", () => {
  assert.deepEqual(links("Visit www.example.com/path, or www.x.co.uk."), [
    ["www.example.com/path", "http://www.example.com/path"],
    ["www.x.co.uk", "http://www.x.co.uk"],
  ]);
  assert.deepEqual(links("www.nothing, http://www.x.com"), [["http://www.x.com", "http://www.x.com"]]);
});

test("email addresses open a new email", () => {
  assert.deepEqual(links("Write to ana.p+qc@shop.example.com."), [
    ["ana.p+qc@shop.example.com", "mailto:ana.p+qc@shop.example.com"],
  ]);
  // One inside a web address stays part of it; a handle isn't an address.
  assert.deepEqual(links("https://x.com/?to=a@b.com and @someone"), [
    ["https://x.com/?to=a@b.com", "https://x.com/?to=a@b.com"],
  ]);
});
