import assert from "node:assert/strict";
import { test } from "node:test";
import { draftHasContent, draftKey, emptyDraft, outgoingDraft, savedDraft } from "./composeDraft.ts";

test("Send Later survives saving and reopening without queueing delivery", () => {
  const sendLater = { key: "tomorrow" as const, at: 2_000_000_000_000 };
  const draft = { ...emptyDraft(1), to: "recipient@example.com", body: "Keep this", sendLater };
  const saved = JSON.parse(JSON.stringify(outgoingDraft(draft, 0, 7)));
  assert.equal(saved.sendAt, 0, "saving is independent of the selected delivery time");
  assert.deepEqual(savedDraft(saved).sendLater, sendLater);
  assert.equal(outgoingDraft(savedDraft(saved), sendLater.at).sendAt, sendLater.at);
});

test("choosing, changing only the time, and clearing a schedule are draft edits", () => {
  const draft = { ...emptyDraft(1), body: "Already saved" };
  const scheduled = { ...draft, sendLater: { key: "tomorrow" as const, at: 2_000_000_000_000 } };
  assert.notEqual(draftKey(draft), draftKey(scheduled));
  assert.notEqual(draftKey(scheduled), draftKey({
    ...scheduled,
    sendLater: { ...scheduled.sendLater, at: scheduled.sendLater.at + 60_000 },
  }));
  const cleared = { ...scheduled, sendLater: null };
  assert.notEqual(draftKey(scheduled), draftKey(cleared));
  assert.equal(savedDraft(outgoingDraft(cleared, 0)).sendLater, null);
});

test("the rich body goes out and comes back with its text; a picture alone is content", () => {
  const draft = { ...emptyDraft(1), body: "Hi Bo", bodyHtml: "<div>Hi <b>Bo</b></div>" };
  const saved = outgoingDraft(draft, 0);
  assert.equal(saved.bodyHtml, "<div>Hi <b>Bo</b></div>");
  assert.equal(savedDraft(saved).bodyHtml, draft.bodyHtml);
  assert.notEqual(draftKey(draft), draftKey({ ...draft, bodyHtml: "<div>Hi Bo</div>" }), "formatting is an edit");
  assert.equal(draftHasContent({ ...emptyDraft(1), bodyHtml: "<div><br></div>" }), false);
  assert.equal(draftHasContent({ ...emptyDraft(1), bodyHtml: '<div><img src="data:image/png;base64,x"></div>' }), true);
});

test("drafts saved before Send Later metadata open without a schedule", () => {
  assert.equal(savedDraft(outgoingDraft(emptyDraft(1), 0)).sendLater, null);
});

test("Remind me goes out with the message, as its time and whether to wait for a reply", () => {
  const remind = { key: "custom" as const, at: 2_000_000_000_000, ifNoReply: true };
  const draft = { ...emptyDraft(1), to: "ana@example.com", remind };
  assert.deepEqual(outgoingDraft(draft, 5).remind, { at: remind.at, ifNoReply: true });
  assert.equal(outgoingDraft(emptyDraft(1), 5).remind, undefined);
  // Back in the composer from Drafts or the Scheduled list, it's still set.
  assert.deepEqual(savedDraft(JSON.parse(JSON.stringify(outgoingDraft(draft, 0)))).remind, remind);
});
