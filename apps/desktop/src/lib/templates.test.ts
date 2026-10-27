import assert from "node:assert/strict";
import { test } from "node:test";
import { nameParts, placeholderValues, recentTemplates } from "./templates.ts";

test("names split into first and last, either way round", () => {
  assert.deepEqual(nameParts("Ana María Ruiz"), { first: "Ana", last: "Ruiz", full: "Ana María Ruiz" });
  assert.deepEqual(nameParts('"Ruiz, Ana"'), { first: "Ana", last: "Ruiz", full: "Ana Ruiz" });
  assert.deepEqual(nameParts("Cher"), { first: "Cher", last: "", full: "Cher" });
  assert.deepEqual(nameParts(""), { first: "", last: "", full: "" });
});

test("placeholders take the first recipient's name and your own", () => {
  assert.deepEqual(placeholderValues("Bo Lind", "Ana Ruiz"), {
    recipientFirstName: "Bo",
    recipientLastName: "Lind",
    recipientFullName: "Bo Lind",
    myFirstName: "Ana",
    myLastName: "Ruiz",
    myFullName: "Ana Ruiz",
  });
});

test("Recent lists the templates used last, at most three", () => {
  const t = (id: number, usedAt?: number) => ({ id, name: `T${id}`, subject: "", html: "", text: "", usedAt });
  assert.deepEqual(
    recentTemplates([t(1, 5), t(2), t(3, 9), t(4, 1), t(5, 7)]).map((x) => x.id),
    [3, 5, 1],
  );
});
