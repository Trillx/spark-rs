import { test } from "node:test";
import assert from "node:assert/strict";
import { linkUrl, loadRecentFonts, noteFont, textToHtml } from "./richText.ts";

test("plain text becomes a line per div", () => {
  assert.equal(textToHtml(""), "");
  assert.equal(textToHtml("Hi <Bo>\n\nThanks & bye"), "<div>Hi &lt;Bo&gt;</div><div><br></div><div>Thanks &amp; bye</div>");
  assert.equal(textToHtml("  a   b\r\nc"), "<div>\u00a0 a\u00a0\u00a0 b</div><div>c</div>");
});

test("typed links get a scheme, and only safe ones are kept", () => {
  assert.equal(linkUrl(" example.com/a "), "https://example.com/a");
  assert.equal(linkUrl("http://x.org"), "http://x.org");
  assert.equal(linkUrl("ana@x.com"), "mailto:ana@x.com");
  assert.equal(linkUrl("mailto:ana@x.com"), "mailto:ana@x.com");
  assert.equal(linkUrl("localhost:8080"), "https://localhost:8080");
  assert.equal(linkUrl("tel:+15551234"), "tel:+15551234");
  assert.equal(linkUrl("javascript:alert(1)"), "");
  assert.equal(linkUrl("  "), "");
});

test("recent fonts put the last used first", () => {
  const store = new Map<string, string>();
  (globalThis as { localStorage?: unknown }).localStorage = {
    getItem: (k: string) => store.get(k) ?? null,
    setItem: (k: string, v: string) => void store.set(k, v),
  };
  let recent = loadRecentFonts();
  assert.deepEqual(recent, { families: [], size: undefined });
  for (const family of ["Arial", "Georgia", "Verdana", "Arial", "Tahoma"]) recent = noteFont(recent, { family });
  recent = noteFont(recent, { size: 18 });
  assert.deepEqual(loadRecentFonts(), { families: ["Tahoma", "Arial", "Verdana"], size: 18 });
});
