// What's typed in a recipient field and the email body's @mentions, as text.
// Nothing here imports: Node runs the tests on this file as it is.

/** A field's addresses: split at commas and semicolons outside quotes and angle brackets. */
export function splitAddresses(field: string): string[] {
  const parts: string[] = [];
  let start = 0;
  for (const i of separators(field)) {
    parts.push(field.slice(start, i));
    start = i + 1;
  }
  parts.push(field.slice(start));
  return parts.map((p) => p.trim()).filter(Boolean);
}

/** Splits typed text at its last separator: the addresses before it are finished. */
export function takeFinished(text: string): { finished: string[]; rest: string } {
  const last = separators(text).at(-1);
  if (last === undefined) return { finished: [], rest: text };
  return { finished: splitAddresses(text.slice(0, last)), rest: text.slice(last + 1).trimStart() };
}

/** Where the commas and semicolons between addresses are. */
function separators(field: string): number[] {
  const found: number[] = [];
  let quoted = false;
  let angle = false;
  for (let i = 0; i < field.length; i++) {
    const c = field[i];
    if (c === "\\" && quoted) i++;
    else if (c === '"') quoted = !quoted;
    else if (c === "<" && !quoted) angle = true;
    else if (c === ">" && !quoted) angle = false;
    else if ((c === "," || c === ";") && !quoted && !angle) found.push(i);
  }
  return found;
}

export interface Person {
  /** "" when there's none. */
  name: string;
  address: string;
}

/** A field entry for someone: `Name <address>`, the name quoted if it has a comma or quotes in it. */
export function entryFor({ name, address }: Person): string {
  if (!name) return address;
  const safe = /[",;<>@\\]/.test(name) ? `"${name.replace(/["\\]/g, "\\$&")}"` : name;
  return `${safe} <${address}>`;
}

export function joinAddresses(entries: string[]): string {
  return entries.join(", ");
}

/** Who an entry names; a half-typed one is all address. */
export function parseEntry(entry: string): Person {
  const match = /^\s*(.*?)\s*<([^<>]*)>\s*$/.exec(entry);
  if (!match) return { name: "", address: entry.trim() };
  let name = match[1];
  if (name.length >= 2 && name.startsWith('"') && name.endsWith('"')) name = name.slice(1, -1).replace(/\\(.)/g, "$1");
  return { name, address: match[2].trim() };
}

interface ReplyEnvelope {
  address: string;
  replyTo?: string[];
  to?: string[];
  cc?: string[];
}

/** Recipients for Reply or Reply All; sent mail follows up with its recipients. */
export function replyRecipients(message: ReplyEnvelope, ownAddress?: string, all = false) {
  const own = ownAddress?.trim().toLowerCase();
  const fromYou = own !== undefined && message.address.toLowerCase() === own;
  const sender = message.address.toLowerCase();
  const seen = new Set(["", "you", ...(own ? [own] : [])]);
  const take = (list: string[], excludeSender = false) => list
    .filter((entry) => {
      const address = parseEntry(entry).address.toLowerCase();
      if (seen.has(address) || (excludeSender && address === sender)) return false;
      seen.add(address);
      return true;
    })
    .map((entry) => entryFor(parseEntry(entry)));
  const targets = message.replyTo?.length ? message.replyTo : [message.address];
  const to = take(fromYou ? message.to ?? [] : targets);
  if (all && !fromYou) to.push(...take(message.to ?? [], true));
  return { to, cc: all ? take(message.cc ?? [], true) : [] };
}

export interface Mention {
  /** Where its "@" is. */
  start: number;
  /** The name typed after the "@" so far. */
  query: string;
}

/** Longest name a mention looks up, in words. */
const MENTION_WORDS = 3;

/**
 * The @mention being typed just before `caret`: an "@" at the start of a word, then a name of a
 * few words on the same line. An "@" inside a word is an email address.
 */
export function mentionAt(text: string, caret: number): Mention | null {
  const match = /(?:^|[\s(["'])@([^\s@][^@\n]*)?$/u.exec(text.slice(0, caret));
  if (!match) return null;
  const query = match[1] ?? "";
  if (query.trim().split(/\s+/).length > MENTION_WORDS) return null;
  return { start: caret - query.length - 1, query };
}

/** Replaces a mention, and the rest of the word the caret is in, with `name`; a space follows it. */
export function completeMention(text: string, mention: Mention, caret: number, name: string) {
  const end = caret + (/^[\p{L}\p{N}._-]*/u.exec(text.slice(caret))?.[0].length ?? 0);
  const after = text.slice(end);
  const gap = /^[\s\p{P}]/u.test(after) ? "" : " ";
  return { text: text.slice(0, mention.start) + name + gap + after, caret: mention.start + name.length + gap.length };
}
