// Run with `npm test` (Node's own test runner; it strips the types).
import assert from "node:assert/strict";
import { test } from "node:test";
import type { Message, MessageChanges, MessageMark } from "../types.ts";
import { loadCache } from "./cache.ts";
import { coalesce, mailboxCopy, sameMessage, staleContent } from "./mailboxCopy.ts";

const mail = (id: number, timestamp = 1_000 + id): Message => ({
  id,
  account: 1,
  category: "people",
  state: "inbox",
  unread: true,
  pinned: false,
  priority: false,
  screening: false,
  from: "Ana Ruiz",
  address: "ana@parts.com",
  to: ["Me <me@x.com>"],
  timestamp,
  subject: `Message ${id}`,
  snippet: "",
  conversation: id,
});

const mark = (seq: number): MessageMark => ({ log: 1, seq, elsewhere: 0 });

/** The store's order, as `Store::messages` gives it. */
const ordered = (list: Message[]) => [...list].sort((a, b) => b.timestamp - a.timestamp || a.id - b.id);

const full = (messages: Message[], seq = 1): MessageChanges => ({ full: true, messages, mark: mark(seq) });
const changed = (seq: number, messages: Message[], gone: number[] = []): MessageChanges => ({
  full: false,
  messages,
  gone,
  mark: mark(seq),
});

test("the first read is every message, and gives the mark to ask from", () => {
  const copy = mailboxCopy();
  assert.equal(copy.mark, null);
  const read = ordered([mail(1), mail(2), mail(3)]);
  const shown = copy.apply(full(read, 4), []);
  assert.deepEqual(shown, read);
  assert.deepEqual(copy.mark, mark(4));
  assert.equal(copy.get(2), read[1]);
  // The demo has no mark: it's read whole every time.
  copy.apply({ full: true, messages: read }, shown);
  assert.equal(copy.mark, null);
});

test("changes merge by id: new mail in order, changed mail in place, gone mail out", () => {
  const copy = mailboxCopy();
  const shown = copy.apply(full(ordered([mail(1), mail(2), mail(3), mail(4)])), []);
  const read = { ...mail(2), unread: false };
  const older = mail(9, 500);
  const tied = mail(7, 1_003);
  const next = copy.apply(changed(2, [mail(10), read, older, tied], [3, 42]), shown);
  assert.deepEqual(next, ordered([mail(1), read, mail(4), mail(10), older, tied]));
  assert.deepEqual(
    next.map((m) => m.id),
    [10, 4, 7, 2, 1, 9],
  );
  // Mail that didn't change is the same object, so memoized rows skip it.
  assert.equal(next.find((m) => m.id === 1), shown.find((m) => m.id === 1));
  assert.equal(next.find((m) => m.id === 4), shown.find((m) => m.id === 4));
  assert.equal(copy.get(3), undefined);
  assert.deepEqual(copy.mark, mark(2));
});

test("a message moved in time moves in the list", () => {
  const copy = mailboxCopy();
  const shown = copy.apply(full(ordered([mail(1), mail(2), mail(3)])), []);
  // A draft saved again has a new date.
  const saved = { ...mail(1), timestamp: 5_000, snippet: "Edited" };
  const next = copy.apply(changed(2, [saved]), shown);
  assert.deepEqual(
    next.map((m) => m.id),
    [1, 3, 2],
  );
  assert.equal(next[0], saved);
});

test("nothing changed gives back what's shown", () => {
  const copy = mailboxCopy();
  const shown = copy.apply(full(ordered([mail(1), mail(2)])), []);
  assert.equal(copy.apply(changed(2, []), shown), shown);
  // Written again as it was: the shown object stays.
  assert.equal(copy.apply(changed(3, [structuredClone(shown[0])]), shown), shown);
  // Read again whole, as it was.
  assert.equal(copy.apply(full(structuredClone(shown), 4), shown), shown);
});

test("what's shown is kept where it reads the same as the store, and put back where it doesn't", () => {
  const copy = mailboxCopy();
  const read = copy.apply(full(ordered([mail(1), mail(2), mail(3)])), []);
  // The UI showed changes before the store had them: 1 was saved, 2 failed, 3 is gone here only.
  const saved = { ...read[2], pinned: true };
  const failed = { ...read[1], state: "done" as const };
  const shown = [failed, saved];
  const next = copy.apply(changed(2, [{ ...mail(1), pinned: true }]), shown);
  assert.deepEqual(next, ordered([{ ...mail(1), pinned: true }, mail(2), mail(3)]));
  assert.equal(next[2], saved, "the store has it as shown");
  assert.equal(next[1], read[1], "the store didn't make that change");
  assert.equal(next[0], read[0]);
  // A field set to undefined reads as one that isn't there.
  const cleared = { ...next[0], reminder: undefined };
  assert.equal(copy.apply(changed(3, []), [cleared, next[1], next[2]])[0], cleared);
});

test("a full read replaces everything, keeping the objects that read the same", () => {
  const copy = mailboxCopy();
  const shown = copy.apply(full(ordered([mail(1), mail(2), mail(3)])), []);
  // spark-cli wrote, or the app restarted: everything comes again.
  const again = ordered([structuredClone(mail(3)), { ...mail(2), unread: false }, mail(5)]);
  const next = copy.apply(full(again, 9), shown);
  assert.deepEqual(next, again);
  assert.equal(next.find((m) => m.id === 3), shown.find((m) => m.id === 3));
  assert.equal(copy.get(1), undefined);
  assert.deepEqual(copy.mark, mark(9));
});

test("following changes gives what reading everything gives", () => {
  let seed = 7;
  const random = (n: number) => {
    seed = (seed * 48_271) % 2_147_483_647;
    return seed % n;
  };
  let store = ordered(Array.from({ length: 40 }, (_, i) => mail(i + 1, 1_000 + random(20))));
  const copy = mailboxCopy();
  let shown = copy.apply(full(store), []);
  let nextId = 41;
  for (let round = 2; round < 200; round++) {
    const touched = new Map<number, Message | null>();
    for (let n = random(4); n >= 0; n--) {
      const kind = random(4);
      if (kind === 0 || !store.length) {
        const m = mail(nextId++, 1_000 + random(30));
        touched.set(m.id, m);
      } else {
        const m = store[random(store.length)];
        if (kind === 1) touched.set(m.id, null);
        else if (kind === 2) touched.set(m.id, { ...m, unread: !m.unread });
        else touched.set(m.id, { ...m, timestamp: 1_000 + random(30) });
      }
    }
    store = ordered([...store.filter((m) => !touched.has(m.id)), ...[...touched.values()].filter((m) => m !== null)]);
    const listed = [...touched.values()].filter((m): m is Message => m !== null).map((m) => structuredClone(m));
    const gone = [...touched].filter(([, m]) => m === null).map(([id]) => id);
    const before = new Map(shown.map((m) => [m.id, m]));
    shown = copy.apply(changed(round, listed, gone), shown);
    assert.deepEqual(shown, store, `round ${round}`);
    for (const m of shown) if (!touched.has(m.id)) assert.equal(m, before.get(m.id), `round ${round}: ${m.id}`);
  }
});

test("messages read the same field by field", () => {
  const m = { ...mail(1), reminder: { id: 4, at: 9 }, folders: [2, 3] };
  assert.ok(sameMessage(m, structuredClone(m)));
  assert.ok(!sameMessage(m, { ...m, folders: [3, 2] }));
  assert.ok(!sameMessage(m, { ...m, reminder: { id: 4, at: 10 } }));
  assert.ok(!sameMessage(m, { ...m, reminder: undefined }));
  assert.ok(sameMessage({ ...mail(1), muted: undefined }, mail(1)));
  assert.ok(!sameMessage({ ...mail(1), muted: true }, mail(1)));
});

test("a body goes stale as a sync fills in its message's files or invite, not as it's read or pinned", () => {
  const copy = mailboxCopy();
  const shown = copy.apply(full(ordered([mail(1), mail(2), mail(3), mail(4)])), []);
  const changes = changed(2, [
    { ...mail(1), unread: false },
    { ...mail(2), pinned: true, state: "done" },
    { ...mail(3), attachment: "plan.pdf" },
    { ...mail(4), inviteKind: "invitation" },
    { ...mail(5), attachment: "new.pdf" },
  ]);
  assert.deepEqual(staleContent(changes, copy), [3, 4], "5 is new: nothing of it was read");
  const next = copy.apply(changes, shown);
  assert.deepEqual(staleContent(changes, copy), [], "taken in");
  // A full read, after a restart, finds them too.
  const repaired = next.map((m) => (m.id === 1 ? { ...m, attachment: "late.pdf" } : m));
  assert.deepEqual(staleContent(full(repaired, 3), copy), [1]);
  assert.deepEqual(staleContent(full(next, 3), copy), []);
});

test("a reader's body loaded before its files were filled in is read again, and the old load isn't kept", async () => {
  // The store's n2: stored without its file, which a later round fills in.
  let stored = { attachments: [] as string[] };
  const answers: (() => void)[] = [];
  const bodies = loadCache({
    fetch: () => {
      const read = stored;
      return new Promise<typeof stored>((resolve) => answers.push(() => resolve(read)));
    },
    budget: 1_000_000,
    entryMax: 100_000,
  });
  const copy = mailboxCopy();
  const shown = copy.apply(full([mail(2)]), []);
  // The reader opens n2 as the round runs.
  const opening = bodies.load(2);
  stored = { attachments: ["n2.pdf"] };
  const changes = changed(2, [{ ...mail(2), attachment: "n2.pdf" }]);
  for (const id of staleContent(changes, copy)) bodies.forget(id);
  copy.apply(changes, shown);
  // Loaded again, as the reader does: the new body is kept, and the old
  // one, answering last, doesn't take its place.
  const again = bodies.load(2);
  assert.notEqual(again, opening);
  answers[1]();
  assert.deepEqual((await again).attachments, ["n2.pdf"]);
  answers[0]();
  assert.deepEqual((await opening).attachments, []);
  assert.deepEqual(bodies.get(2)?.attachments, ["n2.pdf"]);
  assert.equal(answers.length, 2);
});

/** A read that answers when told, counting how many started. */
function reads() {
  const answers: { resolve: (n: number) => void; reject: (e: Error) => void }[] = [];
  let started = 0;
  const run = () => {
    started++;
    return new Promise<number>((resolve, reject) => answers.push({ resolve, reject }));
  };
  return { run, answers, started: () => started };
}

const settle = () => new Promise((resolve) => setImmediate(resolve));

test("reads run one at a time, and those asked for meanwhile share one more", async () => {
  const { run, answers, started } = reads();
  const refresh = coalesce(run);
  const first = refresh();
  const second = refresh();
  const third = refresh();
  assert.equal(started(), 1);
  assert.equal(second, third);
  answers[0].resolve(1);
  assert.equal(await first, 1);
  await settle();
  assert.equal(started(), 2, "the queued one starts after");
  // Asked while the queued one runs: one more after it.
  const fourth = refresh();
  answers[1].resolve(2);
  assert.equal(await second, 2);
  await settle();
  assert.equal(started(), 3);
  answers[2].resolve(3);
  assert.equal(await fourth, 3);
  await settle();
  assert.equal(started(), 3, "nothing more was asked");
  // Idle again: the next starts at once.
  const fifth = refresh();
  assert.equal(started(), 4);
  answers[3].resolve(4);
  assert.equal(await fifth, 4);
});

test("a failed read fails its callers, and the next still runs", async () => {
  const { run, answers, started } = reads();
  const refresh = coalesce(run);
  const first = refresh();
  const second = refresh();
  answers[0].reject(new Error("offline"));
  await assert.rejects(first, /offline/);
  await settle();
  assert.equal(started(), 2);
  answers[1].resolve(5);
  assert.equal(await second, 5);
});
