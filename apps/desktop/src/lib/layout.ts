import {
  Archive,
  ArrowDownLeft,
  Bell,
  Clock,
  FileText,
  MailOpen,
  Newspaper,
  Pin,
  Send,
  Trash2,
  UserPlus,
  Users,
  Zap,
  type LucideIcon,
} from "lucide-react";
import type { Account, Message } from "../types";
import { ACCOUNTS, datePeriod } from "./format";
import type { ActionId } from "./shortcuts";

export type Folder = "inbox" | "pins" | "snoozed" | "aside" | "archive" | "sent" | "drafts" | "trash";
export type InboxMode = "focused" | "cards" | "simple";
export type AccountFilter = Account | "all";
export type CardKey = "priority" | "pinned" | "people" | "notifications" | "newsletters" | "seen";
export type GroupKey = "notifications" | "newsletters";
export type SubList = { kind: "card"; key: CardKey } | { kind: "group"; key: GroupKey } | { kind: "gatekeeper" };

export interface ListView {
  folder: Folder;
  /** Only narrows the Inbox; folders are unified like Spark's. */
  account: AccountFilter;
  mode: InboxMode;
  /** A card's "Show all" or a Focused group row opened as its own list. */
  sub: SubList | null;
  /** null while the search field is closed. */
  search: string | null;
  /** The list toolbar's Done switch (Mod+E): Done mail stays in the Inbox, dimmed. */
  showDone: boolean;
}

export type ListBlock =
  | { kind: "card"; key: CardKey; items: Message[]; total: number }
  /** New senders: a strip of cards above the list, or the full grid of its own view. */
  | { kind: "gatekeeper"; items: Message[]; layout: "strip" | "grid" }
  | { kind: "period"; title: string }
  | { kind: "row"; message: Message }
  | { kind: "group"; key: GroupKey; items: Message[] };

export interface ListModel {
  title: string;
  blocks: ListBlock[];
  /** Ids of the visible rows in display order, for keyboard navigation. */
  order: number[];
  /** What a sub-list's "Done all" applies to. */
  bulk: Message[];
  /** Set while a search query is active. */
  found?: number;
}

export const FOLDERS: { key: Exclude<Folder, "inbox">; label: string; Icon: LucideIcon }[] = [
  { key: "pins", label: "Pins", Icon: Pin },
  { key: "snoozed", label: "Snoozed", Icon: Clock },
  { key: "aside", label: "Set Aside", Icon: ArrowDownLeft },
  { key: "archive", label: "Archive", Icon: Archive },
  { key: "sent", label: "Sent", Icon: Send },
  { key: "drafts", label: "Drafts", Icon: FileText },
  { key: "trash", label: "Trash", Icon: Trash2 },
];

export const INBOX_MODES: { key: InboxMode; label: string; action: ActionId }[] = [
  { key: "focused", label: "Focused list", action: "modeFocused" },
  { key: "cards", label: "Unread cards", action: "modeCards" },
  { key: "simple", label: "Simple list", action: "modeSimple" },
];

/** Card header icon colors. */
export const CARDS: Record<CardKey, { label: string; Icon: LucideIcon; iconClass: string }> = {
  priority: { label: "Priority", Icon: Zap, iconClass: "bg-[#ffaa3b]" },
  pinned: { label: "Pinned", Icon: Pin, iconClass: "bg-[#ffaa3b]" },
  people: { label: "People", Icon: Users, iconClass: "bg-[#007cff]" },
  notifications: { label: "Notifications", Icon: Bell, iconClass: "bg-[#48c725]" },
  newsletters: { label: "Newsletters", Icon: Newspaper, iconClass: "bg-[#a06ad0]" },
  seen: { label: "Seen", Icon: MailOpen, iconClass: "bg-[#868d97]" },
};

export const GATEKEEPER = { label: "New senders", Icon: UserPlus, iconClass: "bg-[#868d97]" };

/** Rows shown per card before "Show all". */
export const CARD_LIMIT = 3;

const CARD_ORDER: CardKey[] = ["priority", "pinned", "people", "notifications", "newsletters", "seen"];

export function folderLabel(folder: Folder) {
  return folder === "inbox" ? "Inbox" : FOLDERS.find((f) => f.key === folder)!.label;
}

const newestFirst = (a: Message, b: Message) => b.timestamp - a.timestamp;
const isLive = (m: Message) => m.state !== "deleted" && m.state !== "blocked";

/**
 * Inbox messages for the account filter, split into accepted mail and
 * Gatekeeper screening. With `showDone`, Done mail stays in `mail`.
 */
export function inboxMessages(messages: Message[], account: AccountFilter, showDone = false) {
  const inbox = messages
    .filter(
      (m) =>
        (m.state === "inbox" || (showDone && m.state === "done" && !m.screening)) &&
        (account === "all" || m.account === account),
    )
    .sort(newestFirst);
  return {
    mail: inbox.filter((m) => !m.screening),
    screening: inbox.filter((m) => m.screening && m.state === "inbox"),
  };
}

/**
 * Sorts accepted inbox mail into Smart Inbox cards. `sticky` holds messages read
 * during this visit: they keep their card until the list changes, so opening a
 * message doesn't make it jump to Seen under the cursor.
 */
export function smartCards(mail: Message[], sticky: ReadonlySet<number>): Record<CardKey, Message[]> {
  const unread = (m: Message) => m.unread || sticky.has(m.id);
  const cards: Record<CardKey, Message[]> = {
    priority: [],
    pinned: [],
    people: [],
    notifications: [],
    newsletters: [],
    seen: [],
  };
  for (const m of mail) {
    if (m.state === "done") cards.seen.push(m);
    else if (m.priority && unread(m)) cards.priority.push(m);
    else if (m.pinned) cards.pinned.push(m);
    else if (!unread(m)) cards.seen.push(m);
    else cards[m.category].push(m);
  }
  return cards;
}

/** Rows (and Focused group rows) under "Today", "Yesterday", … headers. */
function withPeriods(items: (Message | Extract<ListBlock, { kind: "group" }>)[]): ListBlock[] {
  const blocks: ListBlock[] = [];
  let last = "";
  for (const item of items) {
    const newest = "kind" in item ? item.items[0] : item;
    const period = datePeriod(newest.timestamp);
    if (period !== last) blocks.push({ kind: "period", title: period });
    last = period;
    blocks.push("kind" in item ? item : { kind: "row", message: item });
  }
  return blocks;
}

function rowIds(blocks: ListBlock[]) {
  return blocks.flatMap((b) =>
    b.kind === "row" ? [b.message.id] : b.kind === "card" ? b.items.slice(0, CARD_LIMIT).map((m) => m.id) : [],
  );
}

function model(title: string, blocks: ListBlock[], bulk: Message[] = []): ListModel {
  return { title, blocks, order: rowIds(blocks), bulk };
}

export function buildList(messages: Message[], view: ListView, sticky: ReadonlySet<number>): ListModel {
  if (view.search !== null) {
    if (!view.search.trim()) return model("Search", []);
    const hits = messages.filter((m) => isLive(m) && matchesQuery(m, view.search!)).sort(newestFirst);
    return { ...model("Search", hits.map((m) => ({ kind: "row", message: m }))), found: hits.length };
  }

  if (view.folder !== "inbox") {
    const live = messages.filter(isLive);
    const filters: Record<Exclude<Folder, "inbox">, (m: Message) => boolean> = {
      pins: (m) => m.pinned && !m.screening,
      snoozed: (m) => m.state === "snoozed",
      aside: (m) => m.state === "aside",
      archive: (m) => m.state === "done",
      sent: () => false,
      drafts: () => false,
      trash: () => false,
    };
    const items =
      view.folder === "trash"
        ? messages.filter((m) => m.state === "deleted").sort(newestFirst)
        : live.filter(filters[view.folder]).sort(newestFirst);
    return model(folderLabel(view.folder), withPeriods(items));
  }

  const { mail, screening } = inboxMessages(messages, view.account, view.showDone);
  const title = view.account === "all" ? "Inbox" : ACCOUNTS[view.account].name;
  const gatekeeper: ListBlock[] = screening.length ? [{ kind: "gatekeeper", items: screening, layout: "strip" }] : [];

  if (view.sub?.kind === "gatekeeper") {
    return model(GATEKEEPER.label, screening.length ? [{ kind: "gatekeeper", items: screening, layout: "grid" }] : []);
  }

  const sub = view.sub;
  if (sub) {
    const items = sub.kind === "card" ? smartCards(mail, sticky)[sub.key] : mail.filter((m) => m.category === sub.key);
    const label = CARDS[sub.key].label;
    return model(label, items.map((m) => ({ kind: "row", message: m })), items.filter((m) => m.state !== "done"));
  }

  if (view.mode === "cards") {
    const cards = smartCards(mail, sticky);
    const blocks: ListBlock[] = [];
    for (const key of CARD_ORDER) {
      if (cards[key].length) blocks.push({ kind: "card", key, items: cards[key], total: cards[key].length });
      // New senders sit right under Priority.
      if (key === "priority") blocks.push(...gatekeeper);
    }
    return model(title, blocks);
  }

  if (view.mode === "focused") {
    // Unread Priority mail sits in a card on top; the rest is grouped by date,
    // with notifications and newsletters folded into one row each.
    const priority = smartCards(mail, sticky).priority;
    const rest = mail.filter((m) => !priority.includes(m));
    const items: (Message | Extract<ListBlock, { kind: "group" }>)[] = [];
    const grouped = new Set<GroupKey>();
    for (const m of rest) {
      if (m.category === "notifications" || m.category === "newsletters") {
        if (grouped.has(m.category)) continue;
        grouped.add(m.category);
        items.push({ kind: "group", key: m.category, items: rest.filter((x) => x.category === m.category) });
      } else {
        items.push(m);
      }
    }
    const top: ListBlock[] = priority.length
      ? [{ kind: "card", key: "priority", items: priority, total: priority.length }]
      : [];
    return model(title, [...top, ...gatekeeper, ...withPeriods(items)]);
  }

  return model(title, [...gatekeeper, ...withPeriods(mail)]);
}

/** Free text plus `from:`, `has:attachment` and `is:unread`. */
export function matchesQuery(m: Message, query: string) {
  let rest = query;
  const take = (pattern: RegExp) => {
    const hit = rest.match(pattern);
    if (hit) rest = rest.replace(pattern, " ");
    return hit;
  };
  const from = take(/(?:^|\s)from:(\S+)/i)?.[1]?.toLowerCase();
  const hasAttachment = take(/(?:^|\s)has:attachments?(?=\s|$)/i) !== null;
  const unreadOnly = take(/(?:^|\s)is:unread(?=\s|$)/i) !== null;
  const text = rest.trim().toLowerCase();

  if (from && !`${m.from} ${m.address}`.toLowerCase().includes(from)) return false;
  if (hasAttachment && !m.attachment) return false;
  if (unreadOnly && !m.unread) return false;
  return !text || `${m.from} ${m.address} ${m.subject} ${m.snippet} ${m.body.join(" ")}`.toLowerCase().includes(text);
}

export type SidebarCounts = ReturnType<typeof sidebarCounts>;

/** Sidebar counters: unread accepted inbox mail, plus totals for parked folders. */
export function sidebarCounts(messages: Message[]) {
  const unreadInbox = (account?: Account) =>
    messages.filter((m) => m.state === "inbox" && !m.screening && m.unread && (!account || m.account === account))
      .length;
  return {
    inbox: unreadInbox(),
    personal: unreadInbox("personal"),
    work: unreadInbox("work"),
    snoozed: messages.filter((m) => m.state === "snoozed").length,
    aside: messages.filter((m) => m.state === "aside").length,
  };
}
