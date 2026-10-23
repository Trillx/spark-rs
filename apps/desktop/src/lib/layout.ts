import { Bell, MailOpen, Newspaper, Pin, UserPlus, Users, Zap, type LucideIcon } from "lucide-react";
import type { AccountId, AccountView, FolderView, Message } from "../types";
import { asThread, conversationOf, indexConversations, participantNames } from "./conversation";
import { datePeriod } from "./format";
import { showsRecipients } from "./recipients";
import type { ActionId } from "./shortcuts";
import { folderIdOf, hasAccountRows, listFolderIds, listInfo, smartIdOf, type ListKey, type SmartFolder } from "./sidebar";

export type InboxMode = "focused" | "cards" | "simple";
export type AccountFilter = AccountId | "all";
export type CardKey = "priority" | "pinned" | "people" | "notifications" | "newsletters" | "seen";
export type GroupKey = "notifications" | "newsletters";
export type SubList = { kind: "card"; key: CardKey } | { kind: "group"; key: GroupKey } | { kind: "gatekeeper" };

export interface ListView {
  folder: ListKey;
  /**
   * Narrows the Inbox or a shared list (Sent, Archive…) to one account, from
   * its account rows. Folders belong to one account already; smart folders
   * stay unified.
   */
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
  /** Rows name who mail is to rather than the sender (Sent, Drafts). */
  recipients?: boolean;
  /** The row each listed email shows in. A row stands for a conversation, so several can share one. */
  rowOf: ReadonlyMap<number, number>;
  /** Each row's conversation, by row id. */
  threads: ReadonlyMap<number, ThreadInfo>;
}

export interface ThreadInfo {
  /** Its emails, as the thread view shows them. */
  count: number;
  /** Who wrote them, e.g. "Sam, Hannah, me". */
  names: string;
}

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

/** What lists other than the Inbox are made of. */
export interface ListContext {
  accounts: AccountView[];
  folders: FolderView[];
  smartFolders: SmartFolder[];
  /** Recently opened message ids, newest first. */
  seen: number[];
}

const EMPTY_CONTEXT: ListContext = { accounts: [], folders: [], smartFolders: [], seen: [] };

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

/**
 * Turns a list's emails into its rows, one per conversation, Spark-style:
 * the newest of a conversation's emails in the list stands for it, with the
 * flags of the whole conversation (see `asThread`). `read` holds the emails
 * read during this visit to the list.
 */
function listRows(messages: Message[], read: ReadonlySet<number>, ctx: ListContext) {
  const conversations = indexConversations(messages, ctx.folders);
  const own = new Set(ctx.accounts.map((a) => a.email.toLowerCase()));
  /** The emails each row stands for, by row id. */
  const members = new Map<number, number[]>();
  const threads = new Map<number, ThreadInfo>();
  /** Rows of conversations read during this visit, for `smartCards`. */
  const sticky = new Set<number>();

  /** `items` as rows, in the order of each conversation's first email there. */
  function of(items: Message[]): Message[] {
    const rows: Message[] = [];
    const byConversation = new Map<readonly Message[], number[]>();
    for (const m of items) {
      const key = conversations.get(m.id) ?? [m];
      const listed = byConversation.get(key);
      if (listed) {
        listed.push(m.id);
        continue;
      }
      const conversation = conversationOf(conversations, m);
      const ids = [m.id];
      byConversation.set(key, ids);
      members.set(m.id, ids);
      threads.set(m.id, { count: conversation.length, names: participantNames(conversation, own) });
      if (conversation.some((x) => read.has(x.id))) sticky.add(m.id);
      rows.push(asThread(m, conversation));
    }
    return rows;
  }

  function model(title: string, blocks: ListBlock[], bulk: Message[] = []): ListModel {
    const rowOf = new Map<number, number>();
    for (const block of blocks) {
      const shown = block.kind === "row" ? [block.message] : block.kind === "period" ? [] : block.items;
      for (const m of shown) for (const id of members.get(m.id) ?? [m.id]) rowOf.set(id, m.id);
    }
    return { title, blocks, order: rowIds(blocks), bulk, rowOf, threads };
  }

  return { of, model, sticky };
}

/** Messages in any of these server folders. */
const inFolders = (ids: number[]) => (m: Message) => m.folders?.some((id) => ids.includes(id)) ?? false;

/** A list other than the Inbox, newest first (Recently Seen: most recently opened first). */
export function listItems(messages: Message[], key: Exclude<ListKey, "inbox">, ctx: ListContext): Message[] {
  const live = messages.filter(isLive);
  switch (key) {
    case "pins":
      return live.filter((m) => m.pinned && !m.screening).sort(newestFirst);
    case "snoozed":
    case "aside":
      return live.filter((m) => m.state === key).sort(newestFirst);
    case "archive":
      return live.filter((m) => m.state === "done").sort(newestFirst);
    case "trash":
    case "blocked":
      return messages.filter((m) => m.state === (key === "trash" ? "deleted" : "blocked")).sort(newestFirst);
    case "seen":
      return ctx.seen.flatMap((id) => live.filter((m) => m.id === id));
  }
  const smartId = smartIdOf(key);
  if (smartId !== undefined) {
    const smart = ctx.smartFolders.find((f) => f.id === smartId);
    return smart ? live.filter((m) => matchesQuery(m, smart.query)).sort(newestFirst) : [];
  }
  return live.filter(inFolders(listFolderIds(key, ctx.folders))).sort(newestFirst);
}

export function buildList(
  messages: Message[],
  view: ListView,
  sticky: ReadonlySet<number>,
  ctx: ListContext = EMPTY_CONTEXT,
): ListModel {
  const accounts = ctx.accounts;
  const rows = listRows(messages, sticky, ctx);
  if (view.search !== null) {
    if (!view.search.trim()) return rows.model("Search", []);
    const hits = rows.of(messages.filter((m) => isLive(m) && matchesQuery(m, view.search!)).sort(newestFirst));
    return { ...rows.model("Search", hits.map((m) => ({ kind: "row", message: m }))), found: hits.length };
  }

  if (view.folder !== "inbox") {
    const all = listItems(messages, view.folder, ctx);
    const items = rows.of(view.account === "all" ? all : all.filter((m) => m.account === view.account));
    const label = listInfo(view.folder, ctx.folders, ctx.smartFolders)?.label ?? "Folder";
    const account = view.account !== "all" && accounts.find((a) => a.id === view.account)?.label;
    const title = account ? `${label} · ${account}` : label;
    // Recently Seen keeps the order they were opened in.
    const blocks: ListBlock[] =
      view.folder === "seen" ? items.map((m) => ({ kind: "row", message: m })) : withPeriods(items);
    return { ...rows.model(title, blocks), recipients: showsRecipients(view.folder, ctx.folders) };
  }

  const inbox = inboxMessages(messages, view.account, view.showDone);
  const mail = rows.of(inbox.mail);
  const screening = inbox.screening;
  const title = (view.account !== "all" && accounts.find((a) => a.id === view.account)?.label) || "Inbox";
  const gatekeeper: ListBlock[] = screening.length ? [{ kind: "gatekeeper", items: screening, layout: "strip" }] : [];

  if (view.sub?.kind === "gatekeeper") {
    return rows.model(
      GATEKEEPER.label,
      screening.length ? [{ kind: "gatekeeper", items: screening, layout: "grid" }] : [],
    );
  }

  const sub = view.sub;
  if (sub) {
    const items =
      sub.kind === "card" ? smartCards(mail, rows.sticky)[sub.key] : mail.filter((m) => m.category === sub.key);
    const label = CARDS[sub.key].label;
    return rows.model(
      label,
      items.map((m) => ({ kind: "row", message: m })),
      items.filter((m) => m.state !== "done"),
    );
  }

  if (view.mode === "cards") {
    const cards = smartCards(mail, rows.sticky);
    const blocks: ListBlock[] = [];
    for (const key of CARD_ORDER) {
      if (cards[key].length) blocks.push({ kind: "card", key, items: cards[key], total: cards[key].length });
      // New senders sit right under Priority.
      if (key === "priority") blocks.push(...gatekeeper);
    }
    return rows.model(title, blocks);
  }

  if (view.mode === "focused") {
    // Unread Priority mail sits in a card on top; the rest is grouped by date,
    // with notifications and newsletters folded into one row each.
    const priority = smartCards(mail, rows.sticky).priority;
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
    return rows.model(title, [...top, ...gatekeeper, ...withPeriods(items)]);
  }

  return rows.model(title, [...gatekeeper, ...withPeriods(mail)]);
}

/** Free text plus `from:`, `has:attachment`, `is:unread` and `date:today`. */
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
  const todayOnly = take(/(?:^|\s)date:today(?=\s|$)/i) !== null;
  const text = rest.trim().toLowerCase();

  if (from && !`${m.from} ${m.address}`.toLowerCase().includes(from)) return false;
  if (hasAttachment && !m.attachment) return false;
  if (unreadOnly && !m.unread) return false;
  if (todayOnly && m.timestamp < new Date().setHours(0, 0, 0, 0)) return false;
  const haystack = `${m.from} ${m.address} ${m.subject} ${m.snippet} ${m.body?.join(" ") ?? ""}`;
  return !text || haystack.toLowerCase().includes(text);
}

export interface SidebarCounts {
  /** Unread accepted inbox mail, in all accounts and per account. */
  inbox: number;
  accounts: Partial<Record<AccountId, number>>;
  /** Per sidebar list: totals for Snoozed, Set Aside and Drafts, unread mail elsewhere. */
  lists: Partial<Record<ListKey, number>>;
  /** The same per account, for a shared list's account rows. */
  listAccounts: Partial<Record<ListKey, Partial<Record<AccountId, number>>>>;
}

/** Sidebar counters, for the Inbox and the lists in `keys`. Like the lists, they count conversations. */
export function sidebarCounts(messages: Message[], keys: ListKey[], ctx: ListContext = EMPTY_CONTEXT): SidebarCounts {
  const unread = oneEach(messages.filter((m) => m.state === "inbox" && !m.screening && m.unread));
  const lists: Partial<Record<ListKey, number>> = {};
  const listAccounts: SidebarCounts["listAccounts"] = {};
  for (const key of keys) {
    if (key === "inbox") continue;
    let counted: Message[];
    if (key === "drafts") {
      counted = listItems(messages, key, ctx);
    } else if (key === "snoozed" || key === "aside") {
      counted = oneEach(listItems(messages, key, ctx));
    } else if (key === "spam" || folderIdOf(key) !== undefined) {
      counted = oneEach(listItems(messages, key, ctx).filter((m) => m.unread && !m.screening));
    } else continue;
    lists[key] = counted.length;
    if (hasAccountRows(key)) listAccounts[key] = perAccount(counted);
  }
  return { inbox: unread.length, accounts: perAccount(unread), lists, listAccounts };
}

/** One email per conversation. */
function oneEach(messages: Message[]) {
  const seen = new Set<string>();
  return messages.filter((m) => {
    const key = `${m.account} ${m.conversation ?? m.id}`;
    if (seen.has(key)) return false;
    seen.add(key);
    return true;
  });
}

function perAccount(messages: Message[]) {
  const counts: Partial<Record<AccountId, number>> = {};
  for (const m of messages) counts[m.account] = (counts[m.account] ?? 0) + 1;
  return counts;
}
