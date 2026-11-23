// Run with `npm test` (Node's own test runner; it strips the types).
import assert from "node:assert/strict";
import { test } from "node:test";
import { askQuestion, explainQuestion, quoteOf, replyInstructions } from "./selectionAi.ts";

test("The selection goes into each request quoted", () => {
  assert.equal(explainQuestion("DFM methods here?"), 'What does this part of the email mean: "DFM methods here?"');
  assert.equal(
    askQuestion("RFQ 85028", "  Which parts are on it? "),
    'About this part of the email, "RFQ 85028": Which parts are on it?',
  );
  assert.equal(replyInstructions("Continue to quote and produce?"), 'respond to this part of it, "Continue to quote and produce?"');
});

test("A long selection is cut short", () => {
  const quote = quoteOf(`${"word ".repeat(200)}end`);
  assert.ok(quote.length <= 603, `${quote.length} characters`);
  assert.ok(quote.endsWith('word…"'));
  assert.equal(quoteOf("short"), '"short"');
});
