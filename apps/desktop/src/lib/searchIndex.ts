import type { Message } from "../types";
import type { SearchHits } from "./layout";

// What the search index finds for each search's free text, asked as seldom as
// the mail allows. Only type imports, so `npm test` can run it outside the app
// (see searchIndex.test.ts).

/** The ids the index found for a text; null outside the app, where there's no index. */
type Answer = ReadonlySet<number> | null;

const sameList = (a: string[] = [], b: string[] = []) => a.length === b.length && a.every((x, i) => x === b[i]);

/**
 * Whether two versions of a message read the same to the index: sender, recipients, subject and text. The list
 * doesn't have the text or Cc, but only drafts change those, and a draft saved again has a new date.
 */
const indexedSame = (a: Message, b: Message) =>
  a === b ||
  (a.timestamp === b.timestamp &&
    a.subject === b.subject &&
    a.snippet === b.snippet &&
    a.from === b.from &&
    a.address === b.address &&
    sameList(a.to, b.to));

/**
 * Whether the index may find other messages of `after` than it did of `before`: there's mail new to the list, or
 * a message's sender, recipients, subject or text changed. New flags or places, and mail that's gone, can't.
 */
export function indexChanged(before: readonly Message[], after: readonly Message[]): boolean {
  if (before === after) return false;
  // Usually the same messages in the same order, some with new flags.
  if (before.length === after.length && after.every((m, i) => m.id === before[i].id && indexedSame(before[i], m))) {
    return false;
  }
  const old = new Map(before.map((m) => [m.id, m]));
  return after.some((m) => {
    const was = old.get(m.id);
    return !was || !indexedSame(was, m);
  });
}

export interface IndexSearches {
  /**
   * The free texts the lists need now, and the mail they list. Answers known for this mail show at once; the
   * rest are asked once texts and mail have rested, after the asking under way, and show together. Mail that
   * changes nothing for the index (`indexChanged`) asks nothing again.
   */
  want(texts: string[], messages: readonly Message[]): void;
  /** Asks nothing more and shows nothing for what was wanted, until the next `want`. */
  stop(): void;
}

/**
 * Puts the free texts of searches to the index (`ask`), one at a time and `delay` ms after they last changed,
 * and `show`s the answers for the texts wanted. Until new texts are answered what's shown stays, and an answer
 * that comes after other texts were wanted is kept for them but never shown in place of theirs.
 */
export function indexSearches({
  ask,
  show,
  delay,
}: {
  ask: (text: string) => Promise<number[] | null>;
  show: (hits: SearchHits) => void;
  delay: number;
}): IndexSearches {
  /** The answers for the mail last wanted, by text. A new map when the mail changes for the index. */
  let answers = new Map<string, Answer>();
  let mail: readonly Message[] = [];
  /** The texts last wanted, one per line; null after `stop` or a failed ask, so the next `want` asks again. */
  let wanted: string | null = null;
  let shown: SearchHits = new Map();
  let timer: ReturnType<typeof setTimeout> | undefined;
  /** The asking under way: the next starts after it. */
  let asking = Promise.resolve();
  /** Bumped when something else is wanted: asking for what was wanted before stops, and isn't shown. */
  let generation = 0;

  function cancel() {
    generation++;
    clearTimeout(timer);
    timer = undefined;
  }

  /** Shows the answers in `known` for `texts`, unless that's what's shown already. */
  function showAnswers(texts: string[], known: ReadonlyMap<string, Answer>) {
    const hits = new Map<string, ReadonlySet<number>>();
    for (const text of texts) {
      const found = known.get(text);
      if (found) hits.set(text, found);
    }
    if (hits.size === shown.size && [...hits].every(([text, found]) => shown.get(text) === found)) return;
    shown = hits;
    show(hits);
  }

  return {
    want(texts, messages) {
      const changed = indexChanged(mail, messages);
      mail = messages;
      const key = texts.join("\n");
      if (!changed && key === wanted) return;
      wanted = key;
      cancel();
      if (changed) answers = new Map();
      else for (const text of answers.keys()) if (!texts.includes(text)) answers.delete(text);
      if (!texts.length) return;
      const known = answers;
      if (texts.every((text) => known.has(text))) return showAnswers(texts, known);
      const current = generation;
      timer = setTimeout(() => {
        timer = undefined;
        asking = asking
          .then(async () => {
            for (const text of texts) {
              if (current !== generation) return;
              if (known.has(text)) continue;
              const found = await ask(text);
              known.set(text, found && new Set(found));
            }
            if (current === generation) showAnswers(texts, known);
          })
          .catch(() => {
            if (current === generation) wanted = null;
          });
      }, delay);
    },
    stop() {
      cancel();
      wanted = null;
    },
  };
}
