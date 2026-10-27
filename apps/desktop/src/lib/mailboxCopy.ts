import type { Message, MessageChanges, MessageMark } from "../types";

// The window's copy of the stored message list, brought up to date from what changed since it was read
// (`messageChanges`) instead of read again whole. Only type imports, so `npm test` can run it outside the app (see
// mailboxCopy.test.ts).

/** The store's order: newest first, then in the order stored. */
const storeOrder = (a: Message, b: Message) => b.timestamp - a.timestamp || a.id - b.id;

function sameValue(a: unknown, b: unknown): boolean {
  if (a === b) return true;
  if (typeof a !== "object" || typeof b !== "object" || a === null || b === null) return false;
  if (Array.isArray(a) || Array.isArray(b)) {
    return Array.isArray(a) && Array.isArray(b) && a.length === b.length && a.every((x, i) => sameValue(x, b[i]));
  }
  return sameFields(a as Record<string, unknown>, b as Record<string, unknown>);
}

/** Field by field; a field set to undefined is one that isn't there. */
function sameFields(a: Record<string, unknown>, b: Record<string, unknown>): boolean {
  for (const key in a) if (!sameValue(a[key], b[key])) return false;
  for (const key in b) if (!(key in a) && b[key] !== undefined) return false;
  return true;
}

/** Whether two versions of a message read the same, so either can be shown. */
export const sameMessage = (a: Message, b: Message) =>
  a === b || sameFields(a as unknown as Record<string, unknown>, b as unknown as Record<string, unknown>);

/** Two lists in `storeOrder` as one. */
function mergeSorted(a: readonly Message[], b: readonly Message[]): Message[] {
  const out = new Array<Message>(a.length + b.length);
  let i = 0;
  let j = 0;
  let k = 0;
  while (i < a.length && j < b.length) out[k++] = storeOrder(a[i], b[j]) <= 0 ? a[i++] : b[j++];
  while (i < a.length) out[k++] = a[i++];
  while (j < b.length) out[k++] = b[j++];
  return out;
}

export interface MailboxCopy {
  /** Where the copy has got to, to ask for changes from; null before the first read and in the demo. */
  readonly mark: MessageMark | null;
  /** The message with this id, as last read; undefined if it isn't stored (or listed). */
  get(id: number): Message | undefined;
  /**
   * Takes in an answer of `messageChanges` and returns the list to show in place of `shown`: what reading every
   * message would give, in the store's order. A message that reads the same as the one shown is the one shown,
   * so memoized rows and searches see nothing new. A message changed only where it's shown (a change the store
   * didn't make, or hasn't yet) goes back to what the store has, as on a full read. `shown` itself comes back
   * when nothing changed.
   */
  apply(changes: MessageChanges, shown: readonly Message[]): Message[];
}

export function mailboxCopy(): MailboxCopy {
  let mark: MessageMark | null = null;
  /** What `apply` last returned, and by id. */
  let list: Message[] = [];
  let byId = new Map<number, Message>();

  return {
    get mark() {
      return mark;
    },
    get: (id) => byId.get(id),
    apply(changes, shown) {
      // What's shown by id: the list given out last, or (if it was changed since) the list shown.
      const showing = shown === list ? byId : new Map(shown.map((m) => [m.id, m]));
      const pick = (m: Message) => {
        const was = showing.get(m.id);
        return was !== undefined && was !== m && sameMessage(was, m) ? was : m;
      };
      let next: Message[];
      if (changes.full) {
        next = changes.messages.map(pick);
      } else {
        const named = new Set(changes.gone);
        for (const m of changes.messages) named.add(m.id);
        // Messages that didn't change are as read before, or as shown if that reads the same.
        const kept = named.size ? list.filter((m) => !named.has(m.id)) : list;
        const unchanged = shown === list ? kept : kept.map(pick);
        const fresh = changes.messages.map(pick).sort(storeOrder);
        next = fresh.length ? mergeSorted(unchanged, fresh) : unchanged;
      }
      mark = changes.mark ?? null;
      if (next !== shown && next.length === shown.length && next.every((m, i) => m === shown[i])) {
        next = shown as Message[];
      }
      if (next !== list) {
        list = next;
        byId = new Map(next.map((m) => [m.id, m]));
      }
      return next;
    },
  };
}

/**
 * Whether a message's stored content changed between two reads of it, so a body kept from before is stale: older
 * mail got its body, or a sync filled in the attachments or invite it was stored without, as the server couldn't
 * give them then (`contentRev`). Its being read, pinned or moved doesn't change it.
 */
const contentChanged = (was: Message, now: Message) =>
  was.attachment !== now.attachment ||
  was.inviteKind !== now.inviteKind ||
  (was.contentRev ?? 0) !== (now.contentRev ?? 0);

/**
 * The ids of messages in `changes` whose stored content changed since `copy` read them (`contentChanged`), for their
 * kept bodies to be forgotten. Ask before `copy.apply` takes the changes in.
 */
export function staleContent(changes: MessageChanges, copy: MailboxCopy): number[] {
  const stale: number[] = [];
  for (const m of changes.messages) {
    const was = copy.get(m.id);
    if (was !== undefined && contentChanged(was, m)) stale.push(m.id);
  }
  return stale;
}

/**
 * `run`, once at a time: asked while it runs, it runs once more when it's done, for everyone who asked meanwhile.
 * Each caller gets the outcome of a run that started after they asked.
 */
export function coalesce<T>(run: () => Promise<T>): () => Promise<T> {
  let running: Promise<T> | null = null;
  let queued: Promise<T> | null = null;
  const start = () => {
    const current = run();
    running = current;
    const done = () => {
      if (running === current) running = null;
    };
    current.then(done, done);
    return current;
  };
  return () => {
    if (queued) return queued;
    if (!running) return start();
    const after = running.then(
      () => {},
      () => {},
    );
    queued = after.then(() => {
      queued = null;
      return start();
    });
    return queued;
  };
}
