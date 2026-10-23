import type { FolderView, Message } from "../types";
import { isDraft } from "./drafts";

// A conversation is the messages that share `conversation`: the Rust side
// links replies by Message-ID, References and the server's conversation id.
// The thread view shows one as cards, oldest first, the way Spark does.

/**
 * `m`'s conversation, oldest first. Drafts are left out, and mail in Trash or
 * Blocked only shows in a conversation opened from there.
 */
export function conversationOf(messages: Message[], m: Message, folders: FolderView[]): Message[] {
  const key = m.conversation ?? m.id;
  const elsewhere = (x: Message) => (x.state === "deleted" || x.state === "blocked") && x.state !== m.state;
  return messages
    .filter(
      (x) =>
        x.id === m.id ||
        (x.account === m.account && (x.conversation ?? x.id) === key && !elsewhere(x) && !isDraft(x, folders)),
    )
    .sort((a, b) => a.timestamp - b.timestamp || a.id - b.id);
}

/**
 * The messages of the conversation that move with `m` when the thread is
 * marked Done, trashed, set aside or snoozed: those in the same place, so
 * your replies stay in Sent while the thread leaves the Inbox.
 */
export function movesWith(conversation: Message[], m: Message): Message[] {
  const together = conversation.filter((x) => x.state === m.state && x.folder === m.folder);
  return together.some((x) => x.id === m.id) ? together : [m];
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
