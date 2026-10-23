import type { FolderView, Message } from "../types";
import { isDraft } from "./drafts";

// A conversation is the messages that share `conversation`: the Rust side
// links replies by Message-ID, References and the server's conversation id.
// The list shows one as a single row, and the thread view as cards, oldest
// first, the way Spark does.

/** Each message's whole conversation, oldest first, by message id. Drafts are conversations of their own. */
export type Conversations = ReadonlyMap<number, readonly Message[]>;

export function indexConversations(messages: Message[], folders: FolderView[]): Conversations {
  const byKey = new Map<string, Message[]>();
  for (const m of messages) {
    const key = isDraft(m, folders) ? `draft ${m.id}` : `${m.account} ${m.conversation ?? m.id}`;
    const list = byKey.get(key);
    if (list) list.push(m);
    else byKey.set(key, [m]);
  }
  const index = new Map<number, Message[]>();
  for (const list of byKey.values()) {
    list.sort((a, b) => a.timestamp - b.timestamp || a.id - b.id);
    for (const m of list) index.set(m.id, list);
  }
  return index;
}

/** `m`'s conversation, oldest first. Mail in Trash or Blocked only shows in a conversation opened from there. */
export function conversationOf(conversations: Conversations, m: Message): Message[] {
  const elsewhere = (x: Message) => (x.state === "deleted" || x.state === "blocked") && x.state !== m.state;
  return (conversations.get(m.id) ?? [m]).filter((x) => x.id === m.id || !elsewhere(x));
}

/**
 * The messages of the conversation that move with `m` when the thread is
 * marked Done, trashed, set aside or snoozed: those in the same place, so
 * your replies stay in Sent while the thread leaves the Inbox.
 */
export function movesWith(conversation: Message[], m: Message): Message[] {
  const together = conversation.filter(
    (x) => x.state === m.state && x.folder === m.folder && x.screening === m.screening,
  );
  return together.some((x) => x.id === m.id) ? together : [m];
}

/**
 * `m` as its list row and the thread toolbar show it: unread, pinned or
 * priority if any email of its conversation is, with an attachment if any
 * has one. The same object when nothing differs, so memoized rows keep it.
 */
export function asThread(m: Message, conversation: readonly Message[]): Message {
  const unread = conversation.some((x) => x.unread);
  const pinned = conversation.some((x) => x.pinned);
  const priority = conversation.some((x) => x.priority);
  const attachment = m.attachment ?? conversation.find((x) => x.attachment)?.attachment;
  return unread === m.unread && pinned === m.pinned && priority === m.priority && attachment === m.attachment
    ? m
    : { ...m, unread, pinned, priority, attachment };
}

/**
 * Who wrote in a conversation, in order of their first email: one sender's
 * full name, or first names together ("Sam, Hannah, me"). `own` holds the
 * user's addresses, in lowercase.
 */
export function participantNames(conversation: readonly Message[], own: ReadonlySet<string>): string {
  const names = new Map<string, string>();
  for (const m of conversation) {
    const address = m.address.toLowerCase();
    if (!names.has(address)) names.set(address, own.has(address) ? "me" : m.from.trim() || m.address);
  }
  const all = [...names.values()];
  if (all.length === 1) return all[0];
  return all.map((name) => (name.includes("@") ? name : name.split(/\s+/)[0])).join(", ");
}

/**
 * Spark's "minimize old emails": reading starts at the first message that
 * was unread when the thread opened, or at the one opened if that's earlier.
 * Messages before it start collapsed; it and everything after are expanded.
 */
export function readingStart(conversation: Message[], opened: number, unread: ReadonlySet<number>): number {
  const firstUnread = conversation.findIndex((m) => unread.has(m.id));
  const at = conversation.findIndex((m) => m.id === opened);
  if (firstUnread < 0) return Math.max(at, 0);
  return at < 0 ? firstUnread : Math.min(at, firstUnread);
}

const MINIMIZE_KEY = "spark-rs.minimizeOldEmails";

/** Settings → Appearance → Minimize old emails. On unless turned off; like the theme, a preference of this machine. */
export function loadMinimize() {
  return localStorage.getItem(MINIMIZE_KEY) !== "off";
}

export function saveMinimize(on: boolean) {
  localStorage.setItem(MINIMIZE_KEY, on ? "on" : "off");
}
