// Run with `npm test` (Node's own test runner; it strips the types).
import assert from "node:assert/strict";
import { test } from "node:test";
import { citedIds, inline, parseAnswer, plainAnswer, revealTo } from "./assistantText.ts";

test("Answers read as paragraphs, lists and quotes", () => {
  const blocks = parseAnswer(
    "Two emails need you:\n\n- **Ana** wants the contract [[12]]\n- Bob asks about *Friday*\n  - lunch at 1\n\n1. First\n2. Second\n\n> quoted\n\n### Next steps\nDone.",
  );
  assert.deepEqual(blocks[0], { kind: "p", lines: [[{ kind: "text", text: "Two emails need you:" }]] });
  assert.equal(blocks[1].kind, "list");
  if (blocks[1].kind !== "list") return;
  assert.equal(blocks[1].ordered, false);
  assert.equal(blocks[1].items.length, 2);
  assert.deepEqual(blocks[1].items[0], [
    {
      kind: "p",
      lines: [
        [
          { kind: "strong", children: [{ kind: "text", text: "Ana" }] },
          { kind: "text", text: " wants the contract " },
          { kind: "cite", id: 12 },
        ],
      ],
    },
  ]);
  // The indented line is a list inside the second item.
  assert.equal(blocks[1].items[1][1].kind, "list");
  assert.deepEqual(blocks[2], {
    kind: "list",
    ordered: true,
    start: 1,
    items: [[{ kind: "p", lines: [[{ kind: "text", text: "First" }]] }], [{ kind: "p", lines: [[{ kind: "text", text: "Second" }]] }]],
  });
  assert.equal(blocks[3].kind, "quote");
  assert.deepEqual(blocks[4], { kind: "p", lines: [[{ kind: "text", text: "Next steps" }]], strong: true });
});

test("Only web and mail links become links", () => {
  assert.deepEqual(inline("[site](https://example.com) and [x](javascript:alert(1))"), [
    { kind: "link", href: "https://example.com", children: [{ kind: "text", text: "site" }] },
    { kind: "text", text: " and " },
    { kind: "text", text: "x" },
    { kind: "text", text: ")" },
  ]);
});

test("Unclosed marks and HTML stay text", () => {
  assert.deepEqual(inline("2 * 3 and **half"), [{ kind: "text", text: "2 * 3 and **half" }]);
  assert.deepEqual(inline("<img src=x onerror=alert(1)>"), [{ kind: "text", text: "<img src=x onerror=alert(1)>" }]);
  assert.deepEqual(inline("snake_case_name"), [{ kind: "text", text: "snake_case_name" }]);
});

test("Citations number the sources in first-cited order", () => {
  const text = "Due Friday [[7]]. Also [[3, 7]] and [[3]].";
  assert.deepEqual(citedIds(text), [7, 3]);
  assert.equal(plainAnswer("**Due** Friday [[7]]. Also [[3,7]].", [7, 3]), "Due Friday [1]. Also [2][1].");
  // Numbered as the chips are: an email the panel doesn't know has no chip and no number.
  assert.equal(plainAnswer("Unknown [[999]], real [[1]].", [1]), "Unknown, real [1].");
  assert.equal(plainAnswer("Bonjour ! Prêt [[1]] ?", [1]), "Bonjour ! Prêt [1] ?");
  assert.deepEqual(inline("[[3, 7]]"), [
    { kind: "cite", id: 3 },
    { kind: "cite", id: 7 },
  ]);
});

test("Text appears at an even pace without half a citation", () => {
  const text = "See [[1234]] now";
  assert.equal(revealTo(text, 0, 3, false), 3);
  // "See [" would cut into the citation: it shows whole.
  assert.equal(revealTo(text, 3, 3, false), 12);
  assert.equal(revealTo(text, 12, 30, false), text.length);
  // A citation still arriving waits, unless nothing more is coming.
  assert.equal(revealTo("See [[12", 2, 30, false), 4);
  assert.equal(revealTo("See [[12", 2, 30, true), 8);
});
