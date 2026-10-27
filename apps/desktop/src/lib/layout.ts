import {
  Bell,
  CalendarCheck,
  CalendarDays,
  Globe,
  MailOpen,
  Newspaper,
  Pin,
  User,
  UserPlus,
  Users,
  Zap,
  type LucideIcon,
} from "lucide-react";
import type { AccountId, AccountView, BadgeCount, FolderView, GatekeeperMode, Message } from "../types";
import { asThread, conversationOf, indexConversations, participantNames } from "./conversation";
import { datePeriod, futurePeriod } from "./format";
import { showsRecipients } from "./recipients";
import type { ActionId } from "./shortcuts";
import {
  autoLabelOf,
  folderIdOf,
  hasAccountRows,
  listFolderIds,
  listInfo,
  smartIdOf,
  type ListKey,
  type SmartFolder,
} from "./sidebar";

export type InboxMode = "focused" | "cards" | "simple";
export type AccountFilter = AccountId | "all";
export type CardKey =
  | "priority"
  | "pinned"
  | "invitations"
  | "people"
  | "responses"
  | "notifications"
  | "newsletters"
  | "seen";
/** A composite row: Focused's notifications, newsletters and invitations, or a sender or domain the user grouped. */
export type GroupKey = "notifications" | "newsletters" | "invitations" | `sender:${string}` | `domain:${string}`;
export type SubList =
  | { kind: "card"; key: CardKey }
  | { kind: "group"; key: GroupKey }
  | { kind: "gatekeeper" }
  /** The Home Screen's "New emails only": Inbox mail that came after `since`. */
  | { kind: "new"; since: number };

export interface ListView {
  folder: ListKey;
  /**
   * Narrows the Inbox or a shared list (Sent, Archive…) to one account, from
   * its account rows. Folders belong to one account already; smart folders
   * stay unified.
   */
  account: AccountFilter;
  mode: InboxMode;
  /** A card's "Show all", a Focused group row, or the Home Screen's new emails, as its own list. */
  sub: SubList | null;
  /** null while the search field is closed. */
  search: string | null;
  /** The list toolbar's Done switch (Mod+E): Done mail stays in the Inbox, dimmed. */
  showDone: boolean;
}

export type ListBlock =
  /** `shown` rows, then "Show all". */
  | { kind: "card"; key: CardKey; items: Message[]; total: number; shown: number }
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
  /**
   * Group rows by the id they take in `order` (their newest email's): the
   * cursor and the selection reach them like rows, and act on all of them.
   */
  groups: ReadonlyMap<number, GroupBlock>;
}

export type GroupBlock = Extract<ListBlock, { kind: "group" }>;

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

/** The Inbox's mode for each account filter, as Spark keeps an inbox type per account. */
const MODES_KEY = "spark-rs.inbox-modes";

export function loadInboxMode(account: AccountFilter): InboxMode {
  try {
    const saved = JSON.parse(localStorage.getItem(MODES_KEY) ?? "{}")[String(account)];
    return INBOX_MODES.some((m) => m.key === saved) ? saved : "cards";
  } catch {
    return "cards";
  }
}

export function saveInboxMode(account: AccountFilter, mode: InboxMode) {
  let saved: Record<string, InboxMode> = {};
  try {
    saved = JSON.parse(localStorage.getItem(MODES_KEY) ?? "{}");
  } catch {
    // A damaged entry is replaced.
  }
  localStorage.setItem(MODES_KEY, JSON.stringify({ ...saved, [String(account)]: mode }));
}

/** Card header icon colors. */
export const CARDS: Record<CardKey, { label: string; Icon: LucideIcon; iconClass: string }> = {
  priority: { label: "Priority", Icon: Zap, iconClass: "bg-[#ffaa3b]" },
  pinned: { label: "Pinned", Icon: Pin, iconClass: "bg-[#ffaa3b]" },
  invitations: { label: "Invitations", Icon: CalendarDays, iconClass: "bg-[#f05a5a]" },
  people: { label: "People", Icon: Users, iconClass: "bg-[#007cff]" },
  responses: { label: "Responses", Icon: CalendarCheck, iconClass: "bg-[#f05a5a]" },
  notifications: { label: "Notifications", Icon: Bell, iconClass: "bg-[#48c725]" },
  newsletters: { label: "Newsletters", Icon: Newspaper, iconClass: "bg-[#a06ad0]" },
  seen: { label: "Seen", Icon: MailOpen, iconClass: "bg-[#868d97]" },
};

export const GATEKEEPER = { label: "New senders", Icon: UserPlus, iconClass: "bg-[#868d97]" };

/** Rows shown per card before "Show all". */
export const CARD_LIMIT = 3;

export const CARD_ORDER: CardKey[] = [
  "priority",
  "pinned",
  "invitations",
  "people",
  "responses",
  "notifications",
  "newsletters",
  "seen",
];

/**
 * Cards settings, kept on this machine like the sidebar: the cards turned
 * off (their mail goes to its category's card, or People) and how many rows
 * each shows. People can't be turned off; Priority's size is Settings → Priority's.
 */
export interface CardPrefs {
  hidden: CardKey[];
  rows: Partial<Record<CardKey, number>>;
}

const CARDS_KEY = "spark-rs.cards";

export function loadCardPrefs(): CardPrefs {
  try {
    const saved = JSON.parse(localStorage.getItem(CARDS_KEY) ?? "{}");
    const known = (k: unknown): k is CardKey => typeof k === "string" && k in CARDS && k !== "people";
    return {
      hidden: Array.isArray(saved.hidden) ? saved.hidden.filter(known) : [],
      rows: typeof saved.rows === "object" && saved.rows ? saved.rows : {},
    };
  } catch {
    return { hidden: [], rows: {} };
  }
}

export function saveCardPrefs(prefs: CardPrefs) {
  localStorage.setItem(CARDS_KEY, JSON.stringify(prefs));
}

/** Senders and custom domains whose mail folds into one row (Spark's Group by Sender / Domain), lowercased. */
export interface Grouped {
  senders: string[];
  domains: string[];
}

const GROUPED_KEY = "spark-rs.grouped";

export function loadGrouped(): Grouped {
  try {
    const saved = JSON.parse(localStorage.getItem(GROUPED_KEY) ?? "{}");
    const list = (v: unknown) => (Array.isArray(v) ? v.filter((x): x is string => typeof x === "string") : []);
    return { senders: list(saved.senders), domains: list(saved.domains) };
  } catch {
    return { senders: [], domains: [] };
  }
}

export function saveGrouped(grouped: Grouped) {
  localStorage.setItem(GROUPED_KEY, JSON.stringify(grouped));
}

/** `Grouped` as sets, for looking up every email of a big Inbox. */
interface GroupSets {
  senders: ReadonlySet<string>;
  domains: ReadonlySet<string>;
}

const groupSets = (grouped: Grouped | undefined): GroupSets => ({
  senders: new Set(grouped?.senders),
  domains: new Set(grouped?.domains),
});

/** The sender or domain group an email folds into, if the user grouped either. */
function groupOf(m: Message, sets: GroupSets): GroupKey | undefined {
  if (!sets.senders.size && !sets.domains.size) return undefined;
  const address = m.address.toLowerCase();
  if (sets.senders.has(address)) return `sender:${address}`;
  const domain = address.split("@")[1];
  return domain && sets.domains.has(domain) ? `domain:${domain}` : undefined;
}

/** A group row's name and look: Focused's built-in groups, or the grouped sender or domain. */
export function groupInfo(key: GroupKey, items: Message[]): { label: string; Icon: LucideIcon } {
  if (key.startsWith("sender:")) return { label: items[0]?.from || key.slice("sender:".length), Icon: User };
  if (key.startsWith("domain:")) return { label: key.slice("domain:".length), Icon: Globe };
  const { label, Icon } = CARDS[key as "notifications" | "newsletters" | "invitations"];
  return { label, Icon };
}

/** What lists other than the Inbox are made of. */
export interface ListContext {
  accounts: AccountView[];
  folders: FolderView[];
  smartFolders: SmartFolder[];
  /** Recently opened message ids, newest first. */
  seen: number[];
  /** The search index's answers, for `matchesQuery`. */
  hits?: SearchHits;
  /** Where new senders wait; their mail is inbox mail unless that's before the Inbox. */
  gatekeeper?: GatekeeperMode;
  /** Rows the Priority card shows before "Show all" (Settings → Priority); 0 shows them all. */
  priorityLines?: number;
  /** Cards settings. */
  cards?: CardPrefs;
  grouped?: Grouped;
}

/** Mail waiting in New Senders, out of the lists until its sender is accepted. */
const waiting = (m: Message, mode: GatekeeperMode = "beforeInbox") => m.screening && mode === "beforeInbox";

/** Message ids the search index found, per free text (`searchText`). */
export type SearchHits = ReadonlyMap<string, ReadonlySet<number>>;

const EMPTY_CONTEXT: ListContext = { accounts: [], folders: [], smartFolders: [], seen: [] };

/** Where a message sorts in the Inbox: when it arrived, or when it came back from Snooze or a reminder if later. */
export const inboxTime = (m: Message) => Math.max(m.timestamp, m.returnedAt ?? 0, m.reminder?.at ?? 0);
const newestFirst = (a: Message, b: Message) => b.timestamp - a.timestamp;
const newestInInbox = (a: Message, b: Message) => inboxTime(b) - inboxTime(a);
const soonestBack = (a: Message, b: Message) => (a.snoozedUntil ?? 0) - (b.snoozedUntil ?? 0);
const isLive = (m: Message) => m.state !== "deleted" && m.state !== "blocked";

/**
 * Inbox messages for the account filter, split into the list's mail and
 * New Senders, which is empty unless the Gatekeeper screens before the
 * Inbox. With `showDone`, Done mail stays in `mail`.
 */
export function inboxMessages(
  messages: Message[],
  account: AccountFilter,
  showDone = false,
  mode: GatekeeperMode = "beforeInbox",
) {
  const inbox = messages
    .filter(
      (m) =>
        // A reminder that came due brings its email to the Inbox, wherever it's kept,
        // unless it was deleted, blocked or snoozed since.
        (m.state === "inbox" ||
          (m.reminder !== undefined && m.state !== "deleted" && m.state !== "blocked" && m.state !== "snoozed") ||
          (showDone && m.state === "done" && !waiting(m, mode))) &&
        (account === "all" || m.account === account),
    )
    .sort(newestInInbox);
  return {
    mail: inbox.filter((m) => !waiting(m, mode)),
    screening: inbox.filter((m) => waiting(m, mode) && m.state === "inbox"),
  };
}

/**
 * Sorts accepted inbox mail into Smart Inbox cards. `sticky` holds messages read
 * during this visit: they keep their card until the list changes, so opening a
 * message doesn't make it jump to Seen under the cursor. Priority mail stays
 * in its card once read, until it's Done (Spark's `priorityCardShowsRead`
 * defaults to on).
 */
export function smartCards(
  mail: Message[],
  sticky: ReadonlySet<number>,
  hidden: readonly CardKey[] = [],
): Record<CardKey, Message[]> {
  const unread = (m: Message) => m.unread || sticky.has(m.id);
  const on = (key: CardKey) => !hidden.includes(key);
  const cards = Object.fromEntries(CARD_ORDER.map((key) => [key, []])) as unknown as Record<CardKey, Message[]>;
  const card = (m: Message): CardKey => {
    if (m.state === "done") return "seen";
    if (m.priority && on("priority")) return "priority";
    if (m.pinned && on("pinned")) return "pinned";
    if (!unread(m)) return "seen";
    if (m.inviteKind === "invitation") return "invitations";
    if (m.inviteKind === "reply") return "responses";
    return m.category;
  };
  for (const m of mail) {
    // A card turned off gives its mail to People.
    const key = card(m);
    cards[on(key) ? key : "people"].push(m);
  }
  return cards;
}

/** Rows (and Focused group rows) under "Today", "Yesterday", … headers; the Inbox goes by `inboxTime`. */
function withPeriods(
  items: (Message | Extract<ListBlock, { kind: "group" }>)[],
  time: (m: Message) => number = (m) => m.timestamp,
): ListBlock[] {
  const blocks: ListBlock[] = [];
  let last = "";
  for (const item of items) {
    const newest = "kind" in item ? item.items[0] : item;
    const period = datePeriod(time(newest));
    if (period !== last) blocks.push({ kind: "period", title: period });
    last = period;
    blocks.push("kind" in item ? item : { kind: "row", message: item });
  }
  return blocks;
}

/** Snoozed rows under "Later today", "Tomorrow", … for when they come back. */
function withReturnPeriods(items: Message[]): ListBlock[] {
  const blocks: ListBlock[] = [];
  let last = "";
  for (const m of items) {
    const period = futurePeriod(m.snoozedUntil ?? m.timestamp);
    if (period !== last) blocks.push({ kind: "period", title: period });
    last = period;
    blocks.push({ kind: "row", message: m });
  }
  return blocks;
}

function rowIds(blocks: ListBlock[]) {
  return blocks.flatMap((b) =>
    b.kind === "row"
      ? [b.message.id]
      : b.kind === "card"
        ? b.items.slice(0, b.shown).map((m) => m.id)
        : b.kind === "group"
          ? [b.items[0].id]
          : [],
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
    const groups = new Map(blocks.flatMap((b) => (b.kind === "group" ? [[b.items[0].id, b] as const] : [])));
    return { title, blocks, order: rowIds(blocks), bulk, rowOf, threads, groups };
  }

  return { of, model, sticky };
}

/** Messages in any of these server folders. */
const inFolders = (ids: number[]) => (m: Message) => m.folders?.some((id) => ids.includes(id)) ?? false;

/**
 * A list other than the Inbox, newest first (Recently Seen: most recently opened first).
 * `read` holds emails read during this visit, which an `is:unread` smart folder keeps.
 */
export function listItems(
  messages: Message[],
  key: Exclude<ListKey, "inbox">,
  ctx: ListContext,
  read?: ReadonlySet<number>,
): Message[] {
  const live = messages.filter(isLive);
  switch (key) {
    case "pins":
      return live.filter((m) => m.pinned && !waiting(m, ctx.gatekeeper)).sort(newestFirst);
    case "snoozed":
      return live.filter((m) => m.state === key).sort(soonestBack);
    case "aside":
      return live.filter((m) => m.state === key).sort(newestFirst);
    case "archive":
      return live.filter((m) => m.state === "done").sort(newestFirst);
    case "trash":
    case "blocked":
      return messages.filter((m) => m.state === (key === "trash" ? "deleted" : "blocked")).sort(newestFirst);
    case "seen":
      return ctx.seen.flatMap((id) => live.filter((m) => m.id === id));
    // Not mail: App shows these lists itself.
    case "scheduled":
    case "reminders":
      return [];
  }
  const smartId = smartIdOf(key);
  if (smartId !== undefined) {
    const smart = ctx.smartFolders.find((f) => f.id === smartId);
    return smart ? live.filter((m) => matchesQuery(m, smart.query, ctx.hits, read)).sort(newestFirst) : [];
  }
  const label = autoLabelOf(key);
  if (label) return live.filter((m) => m.autoLabel === label && !waiting(m, ctx.gatekeeper)).sort(newestFirst);
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
    // Narrowed to an account, a search looks in that account only.
    const inScope = (m: Message) => view.account === "all" || m.account === view.account;
    const hits = rows.of(
      messages
        .filter((m) => isLive(m) && inScope(m) && matchesQuery(m, view.search!, ctx.hits, sticky))
        .sort(newestFirst),
    );
    return { ...rows.model("Search", hits.map((m) => ({ kind: "row", message: m }))), found: hits.length };
  }

  if (view.folder !== "inbox") {
    const all = listItems(messages, view.folder, ctx, sticky);
    const items = rows.of(view.account === "all" ? all : all.filter((m) => m.account === view.account));
    const label = listInfo(view.folder, ctx.folders, ctx.smartFolders)?.label ?? "Folder";
    const account = view.account !== "all" && accounts.find((a) => a.id === view.account)?.label;
    const title = account ? `${label} · ${account}` : label;
    // Recently Seen keeps the order they were opened in; Snoozed goes by when mail comes back.
    const blocks: ListBlock[] =
      view.folder === "seen"
        ? items.map((m) => ({ kind: "row", message: m }))
        : view.folder === "snoozed"
          ? withReturnPeriods(items)
          : withPeriods(items);
    return { ...rows.model(title, blocks), recipients: showsRecipients(view.folder, ctx.folders) };
  }

  const inbox = inboxMessages(messages, view.account, view.showDone, ctx.gatekeeper);
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

  const hidden = ctx.cards?.hidden ?? [];
  const sets = groupSets(ctx.grouped);
  const focused = view.mode === "focused";
  const grouped = (m: Message) => groupOf(m, sets) !== undefined;
  /** The group a row folds into: a grouped sender or domain, and in Focused, invitations, notifications and newsletters. */
  const groupKey = (m: Message): GroupKey | undefined =>
    groupOf(m, sets) ??
    (!focused
      ? undefined
      : m.inviteKind === "invitation"
        ? "invitations"
        : m.category === "notifications" || m.category === "newsletters"
          ? m.category
          : undefined);
  /** Rows, each group folded into one row where its newest email is. */
  const fold = (items: Message[]) => {
    const out: (Message | GroupBlock)[] = [];
    const groups = new Map<GroupKey, GroupBlock>();
    for (const m of items) {
      const key = groupKey(m);
      if (!key) {
        out.push(m);
        continue;
      }
      let group = groups.get(key);
      if (!group) {
        group = { kind: "group", key, items: [] };
        groups.set(key, group);
        out.push(group);
      }
      group.items.push(m);
    }
    return out;
  };

  // What each part of the Inbox holds, the same for its block and the list it opens:
  // in Unread cards grouped mail leaves the cards; in Focused, Priority mail leaves the groups.
  const cardMail = view.mode === "cards" ? mail.filter((m) => !grouped(m)) : mail;
  const cards = smartCards(cardMail, rows.sticky, hidden);
  const priority = new Set(focused ? cards.priority : []);
  const groupMail =
    view.mode === "cards" ? mail.filter(grouped) : focused ? mail.filter((m) => !priority.has(m)) : mail;

  const sub = view.sub;
  if (sub?.kind === "new") {
    const items = mail.filter((m) => inboxTime(m) > sub.since);
    return rows.model("New Emails", withPeriods(items, inboxTime), items.filter((m) => m.state !== "done"));
  }
  if (sub) {
    const items = sub.kind === "card" ? cards[sub.key] : groupMail.filter((m) => groupKey(m) === sub.key);
    const label = sub.kind === "card" ? CARDS[sub.key].label : groupInfo(sub.key, items).label;
    return rows.model(
      label,
      items.map((m) => ({ kind: "row", message: m })),
      items.filter((m) => m.state !== "done"),
    );
  }

  const priorityLines = ctx.priorityLines ?? CARD_LIMIT;
  /** A card's block: the Priority card shows as many rows as Settings → Priority says. */
  const card = (key: CardKey, items: Message[]): ListBlock => ({
    kind: "card",
    key,
    items,
    total: items.length,
    shown: key !== "priority" ? (ctx.cards?.rows[key] ?? CARD_LIMIT) : priorityLines || items.length,
  });

  if (view.mode === "cards") {
    // Grouped senders and domains are rows of their own, after the cards with unread mail.
    const groups = fold(groupMail) as GroupBlock[];
    const blocks: ListBlock[] = [];
    for (const key of CARD_ORDER) {
      if (key === "seen") blocks.push(...groups);
      if (cards[key].length) blocks.push(card(key, cards[key]));
      // New senders sit right under Priority.
      if (key === "priority") blocks.push(...gatekeeper);
    }
    return rows.model(title, blocks);
  }

  if (focused) {
    // Priority mail sits in a card on top; the rest is grouped by date,
    // with invitations, notifications and newsletters folded into one row each.
    const top: ListBlock[] = priority.size ? [card("priority", cards.priority)] : [];
    return rows.model(title, [...top, ...gatekeeper, ...withPeriods(fold(groupMail), inboxTime)]);
  }

  return rows.model(title, [...gatekeeper, ...withPeriods(fold(mail), inboxTime)]);
}

/** A search's `from:`, `has:attachment`, `is:unread` and `date:today`, and the free text left. */
function parseQuery(query: string) {
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
  return { from, hasAttachment, unreadOnly, todayOnly, text: rest.trim().toLowerCase() };
}

/** A search's free text, what the search index is asked. */
export function searchText(query: string) {
  return parseQuery(query).text;
}

/**
 * Whether a search goes to the mail servers too: it has words or a sender, and isn't for
 * today's mail, which is all here.
 */
export function searchesServers(query: string) {
  const { from, todayOnly, text } = parseQuery(query);
  return !todayOnly && (text !== "" || from !== undefined);
}

/**
 * Free text plus `from:`, `has:attachment`, `is:unread` and `date:today`. The free text
 * matches what the search index found for it, in `hits`, and until it answers, the
 * message's sender, subject and snippet. `is:unread` also keeps the emails in `read`,
 * read during this visit, so they don't leave the list under the cursor.
 */
export function matchesQuery(m: Message, query: string, hits?: SearchHits, read?: ReadonlySet<number>) {
  const { from, hasAttachment, unreadOnly, todayOnly, text } = parseQuery(query);
  if (from && !`${m.from} ${m.address}`.toLowerCase().includes(from)) return false;
  if (hasAttachment && !m.attachment) return false;
  if (unreadOnly && !m.unread && !read?.has(m.id)) return false;
  if (todayOnly && m.timestamp < new Date().setHours(0, 0, 0, 0)) return false;
  if (!text) return true;
  const found = hits?.get(text);
  if (found) return found.has(m.id);
  const haystack = `${m.from} ${m.address} ${m.subject} ${m.snippet} ${m.body?.join(" ") ?? ""}`;
  return haystack.toLowerCase().includes(text);
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
  const unread = oneEach(messages.filter((m) => m.state === "inbox" && !waiting(m, ctx.gatekeeper) && m.unread));
  const lists: Partial<Record<ListKey, number>> = {};
  const listAccounts: SidebarCounts["listAccounts"] = {};
  for (const key of keys) {
    if (key === "inbox") continue;
    let counted: Message[];
    if (key === "drafts") {
      counted = listItems(messages, key, ctx);
    } else if (key === "snoozed" || key === "aside") {
      counted = oneEach(listItems(messages, key, ctx));
    } else if (key === "spam" || folderIdOf(key) !== undefined || autoLabelOf(key)) {
      counted = oneEach(listItems(messages, key, ctx).filter((m) => m.unread && !waiting(m, ctx.gatekeeper)));
    } else continue;
    lists[key] = counted.length;
    if (hasAccountRows(key)) listAccounts[key] = perAccount(counted);
  }
  return { inbox: unread.length, accounts: perAccount(unread), lists, listAccounts };
}

/**
 * The app icon badge's number: the Inbox's conversations, all of them or only
 * unread ones, as the sidebar counts them, leaving out `excluded` accounts.
 */
export function badgeCount(
  messages: Message[],
  badge: BadgeCount,
  excluded: AccountId[],
  mode: GatekeeperMode = "beforeInbox",
): number {
  if (badge === "off") return 0;
  return oneEach(
    messages.filter(
      (m) =>
        m.state === "inbox" && !waiting(m, mode) && (badge === "inbox" || m.unread) && !excluded.includes(m.account),
    ),
  ).length;
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
