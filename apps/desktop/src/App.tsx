import {
  AlignLeft,
  ArrowDownLeft,
  Bell,
  Check,
  CircleAlert,
  CircleCheck,
  Circle,
  Clock,
  Copy,
  Download,
  Forward,
  Inbox,
  LayoutGrid,
  List,
  ListChecks,
  MailOpen,
  PanelLeft,
  Pen,
  Pin,
  RefreshCw,
  Reply,
  ReplyAll,
  Rows3,
  Search,
  Send,
  Settings as SettingsIcon,
  Sparkles,
  Text,
  ThumbsDown,
  ThumbsUp,
  Trash2,
  UserPlus,
  Zap,
} from "lucide-react";
import { useEffect, useMemo, useRef, useState, type KeyboardEvent as ReactKeyboardEvent } from "react";
import {
  cancelSummary,
  getAiSettings,
  presetLabel,
  summarizeMessage,
  type SummaryEvent,
  type SummaryStyle,
} from "./ai";
import {
  cancelSend,
  listAccounts,
  listMessages,
  onAccountsChanged,
  onMailboxChanged,
  onOpenMessage,
  onSendStatus,
  onSyncStatus,
  openAttachment,
  patched,
  saveAllAttachments,
  saveAttachment,
  sendMessage,
  syncNow,
  updateMessage,
} from "./api";
import CommandCenter, { type Command } from "./components/CommandCenter";
import Composer, { draftHasContent, emptyDraft, type Draft } from "./components/Composer";
import MessageList, { Splitter, type GatekeeperActions } from "./components/MessageList";
import type { RowActions } from "./components/MessageRow";
import Settings, { type SettingsPage } from "./components/Settings";
import Sidebar, { type SidebarMode } from "./components/Sidebar";
import { reduceSummary, startSummary, type SummaryState } from "./components/SummaryCard";
import ThreadView, { type ThreadActions } from "./components/ThreadView";
import UndoToast, { TOAST_MS, type ToastData, type ToastIcon } from "./components/UndoToast";
import { CaptionButtons, DragStrip } from "./components/WindowChrome";
import { AccountsContext, isDemoMode } from "./lib/accounts";
import { clearContent, loadContent } from "./lib/content";
import {
  LATER_PRESETS,
  formatFullDate,
  laterTime,
  parseAddress,
  plural,
  presetDetail,
  type LaterPreset,
} from "./lib/format";
import { loadTheme, saveTheme, type ThemePref } from "./lib/theme";
import {
  FOLDERS,
  INBOX_MODES,
  buildList,
  sidebarCounts,
  type AccountFilter,
  type Folder,
  type InboxMode,
  type ListModel,
  type ListView,
} from "./lib/layout";
import {
  LIST_MAX,
  LIST_MIN,
  READER_MIN,
  SIDEBAR_DEFAULT,
  SIDEBAR_RAIL,
  clamp,
  effectiveListWidth,
  useTwoColumns,
  useWindowWidth,
} from "./lib/panes";
import {
  isInsideOverlay,
  isModShortcut,
  isTypingTarget,
  matchShortcut,
  shortcutLabel,
  type ActionId,
} from "./lib/shortcuts";
import type { AccountView, Message, MessagePatch, SendStatusEvent, SyncStatusEvent } from "./types";

/** `targetId` points the actions at one row (its ⋯) rather than the current message. */
type Palette = { kind: "actions"; targetId?: number } | { kind: "snooze"; targetId: number };

const INITIAL_VIEW: ListView = {
  folder: "inbox",
  account: "all",
  mode: "cards",
  sub: null,
  search: null,
  showDone: false,
};

/** Toast icons per action, in the action's color. */
const ICONS = {
  done: { Icon: CircleCheck, className: "text-success" },
  inbox: { Icon: Inbox, className: "text-primary" },
  trash: { Icon: Trash2, className: "text-error" },
  pin: { Icon: Pin, className: "fill-current text-pin" },
  priority: { Icon: Zap, className: "fill-current text-priority" },
  read: { Icon: Circle, className: "text-primary" },
  aside: { Icon: ArrowDownLeft, className: "text-success" },
  snooze: { Icon: Clock, className: "text-priority" },
  accept: { Icon: ThumbsUp, className: "text-primary" },
  block: { Icon: ThumbsDown, className: "text-text-2" },
  send: { Icon: Send, className: "text-primary" },
  copy: { Icon: Copy, className: "text-primary" },
  download: { Icon: Download, className: "text-primary" },
  error: { Icon: CircleAlert, className: "text-error" },
} satisfies Record<string, ToastIcon>;

/** Failures stay up longer than undo toasts. */
const ERROR_TOAST_MS = 10_000;

const SUMMARY_ACTIONS: Record<"summarize" | "summarizeDetailed" | "summarizeActions", SummaryStyle> = {
  summarize: "short",
  summarizeDetailed: "detailed",
  summarizeActions: "actionPoints",
};
const MODE_ICONS = { focused: List, cards: LayoutGrid, simple: Rows3 } as const;

/** Changes when accounts come or go (or demo mode ends), not on a rename or sync. */
const accountSet = (accounts: AccountView[]) => accounts.map((a) => `${a.id}${a.demo ? "d" : ""}`).join(",");

export default function App() {
  const [accounts, setAccounts] = useState<AccountView[]>([]);
  const loadedAccounts = useRef("");
  /** The last sync error toasted per account, so a retry that fails the same way stays quiet. */
  const syncErrors = useRef(new Map<number, string | undefined>());
  /** Sent drafts by outbox id until they go out, to reopen one that fails. */
  const outbox = useRef(new Map<number, Draft>());
  const [messages, setMessages] = useState<Message[]>([]);
  const [view, setView] = useState<ListView>(INITIAL_VIEW);
  /** Messages read during this visit to the list; they keep their Smart Inbox card until the list changes. */
  const [sticky, setSticky] = useState<ReadonlySet<number>>(new Set());
  /** The list cursor. In split view it follows the open thread. */
  const [selectedId, setSelectedId] = useState<number | null>(null);
  const [openId, setOpenId] = useState<number | null>(null);
  const [sidebarMode, setSidebarMode] = useState<SidebarMode>("rail");
  const [sidebarWidth, setSidebarWidth] = useState(SIDEBAR_DEFAULT);
  const [listWidth, setListWidth] = useState(LIST_MIN);
  const [draft, setDraft] = useState<Draft | null>(null);
  const [composerKey, setComposerKey] = useState(0);
  const [composerFull, setComposerFull] = useState(false);
  const [palette, setPalette] = useState<Palette | null>(null);
  /** +AI summaries by message id, kept while the app runs. */
  const [summaries, setSummaries] = useState<ReadonlyMap<number, SummaryState>>(new Map());
  const summaryRun = useRef(0);
  const [settingsPage, setSettingsPage] = useState<SettingsPage | null>(null);
  const lastSettingsPage = useRef<SettingsPage>("accounts");
  const [theme, setTheme] = useState<ThemePref>(loadTheme);
  /** The configured provider's name, for "Anthropic declined…". */
  const [aiProvider, setAiProvider] = useState("The AI provider");
  const [toast, setToast] = useState<ToastData | null>(null);
  const toastTimer = useRef<number | undefined>(undefined);
  const searchRef = useRef<HTMLInputElement>(null);

  const windowWidth = useWindowWidth();
  const sidebarSpace = sidebarMode === "locked" ? sidebarWidth : SIDEBAR_RAIL;
  const container = windowWidth - sidebarSpace;
  const listWidthUsed = effectiveListWidth(listWidth, container);
  const twoColumns = useTwoColumns(container, listWidthUsed);

  const model = useMemo(() => buildList(messages, view, sticky, accounts), [messages, view, sticky, accounts]);
  const counts = useMemo(() => sidebarCounts(messages), [messages]);
  const openMessage = messages.find((m) => m.id === openId) ?? null;
  const selected = messages.find((m) => m.id === selectedId && model.order.includes(m.id)) ?? null;
  /** What message shortcuts and commands act on. */
  const current = openMessage ?? selected;
  const searching = view.search !== null;
  const syncing = accounts.some((a) => a.sync.status === "syncing");

  useEffect(() => {
    void Promise.all([listAccounts(), listMessages()]).then(
      ([loadedAccountList, loaded]) => {
        loadedAccounts.current = accountSet(loadedAccountList);
        setAccounts(loadedAccountList);
        setMessages(loaded);
        const first = buildList(loaded, INITIAL_VIEW, new Set()).order[0];
        if (first !== undefined && twoColumns) openThread(first, loaded);
      },
      (error) => showToast(`Couldn't load your mail: ${String(error)}`, undefined, ICONS.error),
    );
  }, []);

  // Backend events. They call through a ref so the listeners, registered once, see current state.
  const events = useRef({ reloadMessages, reloadAccounts, syncStatus, sendStatus, openFromNotification });
  events.current = { reloadMessages, reloadAccounts, syncStatus, sendStatus, openFromNotification };
  useEffect(() => {
    const stops = [
      onMailboxChanged(() => void events.current.reloadMessages()),
      onAccountsChanged(() => void events.current.reloadAccounts()),
      onSyncStatus((e) => events.current.syncStatus(e)),
      onSendStatus((e) => events.current.sendStatus(e)),
      onOpenMessage((id) => events.current.openFromNotification(id)),
    ];
    return () => stops.forEach((stop) => stop());
  }, []);

  useEffect(() => {
    if (searching) searchRef.current?.focus();
  }, [searching]);

  useEffect(() => {
    if (settingsPage !== null) return;
    void getAiSettings().then(
      (view) => view && setAiProvider(presetLabel(view.preset)),
      () => {},
    );
  }, [settingsPage]);

  // ---- data ----

  async function update(id: number, patch: MessagePatch) {
    setMessages((ms) => ms.map((m) => (m.id === id ? patched(m, patch) : m)));
    try {
      const saved = await updateMessage(id, patch);
      setMessages((ms) => ms.map((m) => (m.id === id ? saved : m)));
    } catch (error) {
      showToast(`Couldn't update the message: ${String(error)}`);
      void reloadMessages();
    }
  }

  /** Reloads the list, keeping the cursor and open thread while their messages still exist. */
  function reloadMessages() {
    return listMessages().then(
      (loaded) => {
        const ids = new Set(loaded.map((m) => m.id));
        setMessages(loaded);
        setOpenId((id) => (id !== null && ids.has(id) ? id : null));
        setSelectedId((id) => (id !== null && ids.has(id) ? id : null));
      },
      (error) => showToast(`Couldn't load your mail: ${String(error)}`, undefined, ICONS.error),
    );
  }

  /**
   * Reloads accounts. When accounts come or go, message ids may now name other
   * messages (demo mail ends when the first real account arrives), so cached
   * bodies and summaries go, and an Inbox filter or From on a removed account resets.
   */
  function reloadAccounts() {
    return listAccounts().then(
      (loaded) => {
        setAccounts(loaded);
        const set = accountSet(loaded);
        if (set === loadedAccounts.current) return;
        const demoChanged = isDemoMode(accounts) !== isDemoMode(loaded);
        loadedAccounts.current = set;
        const exists = (id: number) => loaded.some((a) => a.id === id);
        clearContent();
        setSummaries(new Map());
        setSticky(new Set());
        setView((v) => (v.account !== "all" && !exists(v.account) ? { ...v, account: "all", sub: null } : v));
        setDraft((d) => (d && !exists(d.from) && loaded.length ? { ...d, from: loaded[0].id } : d));
        if (demoChanged) {
          setOpenId(null);
          setSelectedId(null);
        }
        void reloadMessages();
      },
      (error) => showToast(`Couldn't load accounts: ${String(error)}`, undefined, ICONS.error),
    );
  }

  function syncStatus(e: SyncStatusEvent) {
    setAccounts((list) =>
      list.map((a) =>
        a.id === e.account
          ? { ...a, sync: { status: e.status, message: e.message, lastSync: e.lastSync ?? a.sync.lastSync } }
          : a,
      ),
    );
    if (e.status === "idle") syncErrors.current.delete(e.account);
    if (e.status !== "error" || syncErrors.current.get(e.account) === e.message) return;
    syncErrors.current.set(e.account, e.message);
    const label = accounts.find((a) => a.id === e.account)?.label ?? "Mail";
    showToast(
      `${label}: ${e.message ?? "Couldn't check for new mail."}`,
      () => openSettings("accounts"),
      ICONS.error,
      { actionLabel: "Settings", duration: ERROR_TOAST_MS },
    );
  }

  function sendStatus(e: SendStatusEvent) {
    const sent = outbox.current.get(e.id);
    outbox.current.delete(e.id);
    if (e.status !== "failed") return;
    showToast(
      `Couldn't send: ${e.message ?? "the server refused the message."}`,
      sent && (() => openComposer(sent)),
      ICONS.error,
      { actionLabel: "Edit", duration: ERROR_TOAST_MS },
    );
  }

  function checkMail() {
    syncNow().then(
      () => void reloadMessages(),
      (error) => showToast(`Couldn't check for new mail: ${String(error)}`, undefined, ICONS.error),
    );
  }

  /** `options.actionLabel` replaces Undo, e.g. "Edit"; such toasts don't answer Ctrl+Z. */
  function showToast(
    text: string,
    undo?: () => void,
    icon?: ToastIcon,
    options: Pick<ToastData, "actionLabel" | "duration"> = {},
  ) {
    window.clearTimeout(toastTimer.current);
    const next = { id: Date.now(), text, undo, icon, ...options };
    setToast(next);
    toastTimer.current = window.setTimeout(
      () => setToast((t) => (t?.id === next.id ? null : t)),
      options.duration ?? TOAST_MS,
    );
  }

  function undoToast() {
    window.clearTimeout(toastTimer.current);
    toast?.undo?.();
    setToast(null);
  }

  /** Applies patches as one step, moving the cursor off messages that leave the list. Returns the undo. */
  function apply(targets: Message[], patchFor: (m: Message) => MessagePatch): () => void {
    const changes = targets.map((m) => {
      const patch = patchFor(m);
      return { id: m.id, patch, previous: previousValues(m, patch) };
    });
    const after = messages.map((m) => {
      const change = changes.find((c) => c.id === m.id);
      return change ? patched(m, change.patch) : m;
    });
    const before = listedIds(model);
    const remaining = listedIds(buildList(after, view, sticky));
    const leaving = changes.map((c) => c.id).filter((id) => before.has(id) && !remaining.has(id));
    const cursor = { selectedId, openId };

    moveOffMessages(leaving);
    for (const c of changes) void update(c.id, c.patch);
    return () => {
      for (const c of changes) void update(c.id, c.previous);
      if (leaving.length) {
        setSelectedId(cursor.selectedId);
        setOpenId(cursor.openId);
      }
    };
  }

  function act(
    targets: Message[],
    patch: MessagePatch | ((m: Message) => MessagePatch),
    text: string,
    icon?: ToastIcon,
  ) {
    if (!targets.length) return;
    showToast(text, apply(targets, typeof patch === "function" ? patch : () => patch), icon);
  }

  /** After a message leaves the list: split view opens its neighbor, one column goes back to the list. */
  function moveOffMessages(ids: number[]) {
    const anchor = openId ?? selectedId;
    if (anchor === null || !ids.includes(anchor)) return;
    const gone = new Set(ids);
    const order = model.order;
    const i = order.indexOf(anchor);
    const next =
      i < 0
        ? null
        : (order.slice(i + 1).find((id) => !gone.has(id)) ??
          order
            .slice(0, i)
            .reverse()
            .find((id) => !gone.has(id)) ??
          null);
    if (twoColumns && openId !== null && next !== null) openThread(next);
    else {
      setOpenId(null);
      setSelectedId(next);
    }
  }

  // ---- navigation ----

  function openThread(id: number, list = messages) {
    setSelectedId(id);
    setOpenId(id);
    const m = list.find((x) => x.id === id);
    if (m?.unread) {
      setSticky((s) => new Set(s).add(id));
      void update(id, { unread: false });
    }
  }

  /** A notification was clicked: show its message in the Inbox, closing what covers it. */
  function openFromNotification(id: number) {
    void listMessages().then(
      (loaded) => {
        setMessages(loaded);
        const m = loaded.find((x) => x.id === id);
        if (!m) return;
        setSettingsPage(null);
        setPalette(null);
        changeView(
          { folder: "inbox", sub: null, search: null, account: view.account === m.account ? m.account : "all" },
          true,
        );
        openThread(id, loaded);
      },
      (error) => showToast(`Couldn't load your mail: ${String(error)}`, undefined, ICONS.error),
    );
  }

  function moveCursor(step: 1 | -1) {
    const order = model.order;
    if (!order.length) return;
    const anchor = openId ?? selectedId;
    const i = anchor === null ? -1 : order.indexOf(anchor);
    const next = i < 0 ? order[0] : order[clamp(i + step, 0, order.length - 1)];
    if (twoColumns || openId !== null) openThread(next);
    else setSelectedId(next);
  }

  function changeView(next: Partial<ListView>, keepThread = false) {
    const merged = { ...view, ...next };
    if (merged.folder !== view.folder || merged.account !== view.account || merged.mode !== view.mode) {
      setSticky(new Set());
    }
    setView(merged);
    if (!keepThread) {
      setOpenId(null);
      setSelectedId(null);
    }
  }

  function selectFolder(folder: Folder, account: AccountFilter = "all") {
    changeView({ folder, account, sub: null, search: null });
    if (sidebarMode === "overlay") setSidebarMode("rail");
  }

  const setMode = (mode: InboxMode) => changeView({ folder: "inbox", mode, sub: null, search: null }, true);

  function openSearch() {
    if (searching) searchRef.current?.select();
    else changeView({ search: "", sub: null }, true);
  }

  const closeSearch = () => changeView({ search: null }, true);

  /** Esc: the sidebar overlay, then the thread, then a sub-list, then search. */
  function closeTopmost() {
    if (sidebarMode === "overlay") setSidebarMode("rail");
    else if (openId !== null) setOpenId(null);
    else if (view.sub) changeView({ sub: null });
    else if (searching) closeSearch();
  }

  function onSearchKeyDown(e: ReactKeyboardEvent<HTMLInputElement>) {
    if (e.key === "Escape") {
      e.preventDefault();
      closeSearch();
    } else if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      moveCursor(e.key === "ArrowDown" ? 1 : -1);
    } else if (e.key === "Enter" && selectedId !== null) {
      openThread(selectedId);
    }
  }

  // ---- message actions ----

  const actions = {
    done: (m: Message) =>
      m.state === "done"
        ? act([m], { state: "inbox" }, "Moved to Inbox", ICONS.inbox)
        : act([m], { state: "done" }, "Marked as Done", ICONS.done),
    trash: (m: Message) => {
      if (m.state !== "deleted") act([m], { state: "deleted" }, "Moved to Trash", ICONS.trash);
    },
    pin: (m: Message) => act([m], { pinned: !m.pinned }, m.pinned ? "Unpinned" : "Pinned", ICONS.pin),
    priority: (m: Message) =>
      act([m], { priority: !m.priority }, m.priority ? "Removed from Priority" : "Marked as Priority", ICONS.priority),
    read: (m: Message) =>
      act([m], { unread: !m.unread }, m.unread ? "Marked as Read" : "Marked as Unread", ICONS.read),
    aside: (m: Message) =>
      m.state === "aside"
        ? act([m], { state: "inbox" }, "Moved to Inbox", ICONS.inbox)
        : act([m], { state: "aside" }, "Set aside", ICONS.aside),
    snooze: (m: Message, p: LaterPreset) =>
      act(
        [m],
        { state: "snoozed", snoozedUntil: laterTime(p) },
        `Snoozed until ${p.label.toLowerCase()}`,
        ICONS.snooze,
      ),
    accept: (m: Message) => act([m], { screening: false }, "Sender accepted", ICONS.accept),
    block: (m: Message) => act([m], { state: "blocked" }, "Sender blocked", ICONS.block),
  };

  function doneAll(items: Message[]) {
    const targets = items.filter((m) => m.state !== "done");
    act(
      targets,
      { state: "done" },
      targets.length === 1 ? "Marked as Done" : `${targets.length} emails marked as Done`,
      ICONS.done,
    );
  }

  /** "Sender accepted" for one sender's mail, "N senders accepted" for several. */
  const senders = (items: Message[], verb: string) => {
    const count = new Set(items.map((m) => m.address)).size;
    return count === 1 ? `Sender ${verb}` : `${plural(count, "sender")} ${verb}`;
  };

  const gatekeeper: GatekeeperActions = {
    accept: actions.accept,
    block: actions.block,
    acceptAll: (items) => act(items, { screening: false }, senders(items, "accepted"), ICONS.accept),
    blockAll: (items) => act(items, { state: "blocked" }, senders(items, "blocked"), ICONS.block),
  };

  const snoozeMenu = (m: Message) => setPalette({ kind: "snooze", targetId: m.id });

  // ---- composer ----

  function openComposer(next: Draft) {
    setDraft(next);
    setComposerKey((k) => k + 1);
  }

  /** From the account the Inbox is narrowed to, else the first. */
  function compose(seed: Partial<Draft> = {}) {
    const from = view.account !== "all" ? view.account : (accounts[0]?.id ?? 0);
    openComposer({ ...emptyDraft(from), ...seed });
  }

  const replySubject = (m: Message) => (/^re:/i.test(m.subject) ? m.subject : `Re: ${m.subject}`);

  const reply = (m: Message, body = "") =>
    compose({ title: "Reply", to: m.address, subject: replySubject(m), body, from: m.account, replyTo: m.id });

  /** The sender plus everyone else on To and Cc, minus this account's own address. */
  async function replyAll(m: Message) {
    const content = await loadContent(m.id).catch(() => null);
    const own = accounts.find((a) => a.id === m.account)?.email.toLowerCase();
    const others = (list: string[]) =>
      list.filter((r) => {
        const address = parseAddress(r).address.toLowerCase();
        return r !== "You" && address !== own && address !== m.address.toLowerCase();
      });
    const to = [m.address, ...others(content?.to ?? [])].join(", ");
    const cc = others(content?.cc ?? []).join(", ");
    compose({
      title: "Reply all",
      to,
      cc,
      showCc: cc !== "",
      subject: replySubject(m),
      from: m.account,
      replyTo: m.id,
    });
  }

  async function forward(m: Message) {
    const content = await loadContent(m.id).catch(() => null);
    const text = content?.text ?? m.body ?? [m.snippet];
    const to = (content?.to ?? []).filter((r) => r !== "You");
    const header = [
      "---------- Forwarded message ----------",
      `From: ${m.from} <${m.address}>`,
      `Date: ${formatFullDate(m.timestamp)}`,
      `Subject: ${m.subject}`,
      ...(to.length ? [`To: ${to.join(", ")}`] : []),
    ];
    compose({
      title: "Forward",
      subject: /^fwd?:/i.test(m.subject) ? m.subject : `Fwd: ${m.subject}`,
      body: `\n\n${header.join("\n")}\n\n${text.join("\n\n")}`,
      from: m.account,
      replyTo: m.id,
      forward: true,
      // The original's files go along; they're fetched from the server when it's sent.
      attachments: (content?.attachments ?? []).map((a, index) => ({
        kind: "message" as const,
        message: m.id,
        index,
        name: a.name,
        size: a.size,
      })),
    });
  }

  function closeComposer() {
    if (!draft) return;
    const closed = draft;
    setDraft(null);
    if (draftHasContent(closed)) showToast("Draft discarded", () => openComposer(closed));
  }

  /**
   * Queues the message to go out when the undo window ends (or at the Send
   * Later time). Undo cancels it in the outbox and reopens the composer.
   */
  async function send() {
    if (!draft) return;
    if (!draft.to.trim()) {
      showToast("Add at least one recipient");
      return;
    }
    const sent = draft;
    const repliedTo = sent.doneAfter ? messages.find((m) => m.id === sent.replyTo && m.state !== "done") : undefined;
    setDraft(null);
    const undoDone = repliedTo ? apply([repliedTo], () => ({ state: "done" })) : undefined;
    let id: number;
    try {
      id = await sendMessage({
        account: sent.from,
        to: sent.to,
        cc: sent.cc,
        bcc: sent.bcc,
        subject: sent.subject,
        body: sent.body,
        // A forward isn't a reply; it only carries replyTo for "Mark as Done after sending".
        replyTo: sent.forward ? undefined : sent.replyTo,
        sendAt: sent.sendLater?.at ?? Date.now() + TOAST_MS,
        attachments: sent.attachments,
      });
    } catch (error) {
      undoDone?.();
      openComposer(sent);
      showToast(`Couldn't send: ${String(error)}`, undefined, ICONS.error, { duration: ERROR_TOAST_MS });
      return;
    }
    outbox.current.set(id, sent);
    showToast(sent.sendLater ? "Message scheduled" : "Message sent", () => void unsend(id, sent, undoDone), ICONS.send);
  }

  async function unsend(id: number, sent: Draft, undoDone?: () => void) {
    const cancelled = await cancelSend(id).catch(() => false);
    if (!cancelled) {
      showToast("Already sent", undefined, ICONS.send);
      return;
    }
    outbox.current.delete(id);
    undoDone?.();
    openComposer(sent);
  }

  // ---- +AI Summary ----

  function setSummary(id: number, next: (s: SummaryState | undefined) => SummaryState | undefined) {
    setSummaries((prev) => {
      const value = next(prev.get(id));
      if (value === prev.get(id)) return prev;
      const map = new Map(prev);
      if (value) map.set(id, value);
      else map.delete(id);
      return map;
    });
  }

  /** Starts (or restarts) a summary. Events from a run that was replaced are dropped. */
  function summarize(m: Message, style: SummaryStyle = "short") {
    const run = ++summaryRun.current;
    setSummary(m.id, () => startSummary(run, style));
    const onEvent = (event: SummaryEvent) => setSummary(m.id, (s) => (s?.run === run ? reduceSummary(s, event) : s));
    summarizeMessage(m, style, onEvent).catch((error) =>
      onEvent({ event: "error", data: { message: String(error) } }),
    );
  }

  function stopSummary(m: Message) {
    void cancelSummary(m.id);
    setSummary(m.id, (s) => (s?.status === "generating" ? { ...s, status: "stopped" } : s));
  }

  function deleteSummary(m: Message) {
    if (summaries.get(m.id)?.status === "generating") void cancelSummary(m.id);
    setSummary(m.id, () => undefined);
  }

  // ---- settings ----

  function openSettings(page = lastSettingsPage.current) {
    lastSettingsPage.current = page;
    setPalette(null);
    setSettingsPage(page);
  }

  function changeTheme(next: ThemePref) {
    saveTheme(next);
    setTheme(next);
  }

  // ---- keyboard ----

  function run(action: ActionId) {
    switch (action) {
      case "next":
        return moveCursor(1);
      case "prev":
        return moveCursor(-1);
      case "open":
        if (selectedId !== null) openThread(selectedId);
        return;
      case "close":
        return closeTopmost();
      case "compose":
        return compose();
      case "search":
        return openSearch();
      case "sidebar":
        return setSidebarMode((m) => (m === "rail" ? "overlay" : "rail"));
      case "command":
        return setPalette({ kind: "actions" });
      case "settings":
        return openSettings();
      case "toggleDone":
        return toggleDone();
      case "modeFocused":
        return setMode("focused");
      case "modeCards":
        return setMode("cards");
      case "modeSimple":
        return setMode("simple");
    }
    const m = current;
    if (!m) return;
    switch (action) {
      case "done":
        return actions.done(m);
      case "snooze":
        return snoozeMenu(m);
      case "pin":
        return actions.pin(m);
      case "aside":
        return actions.aside(m);
      case "read":
        return actions.read(m);
      case "priority":
        return actions.priority(m);
      case "replyAll":
        return void replyAll(m);
      case "reply":
        return reply(m);
      case "forward":
        return void forward(m);
      case "trash":
        return actions.trash(m);
      case "summarize":
      case "summarizeDetailed":
      case "summarizeActions":
        if (openId !== m.id) openThread(m.id);
        return summarize(m, SUMMARY_ACTIONS[action]);
    }
  }

  useEffect(() => {
    function onKey(e: KeyboardEvent) {
      if (e.defaultPrevented || palette || settingsPage || isInsideOverlay(e.target)) return;
      const typing = isTypingTarget(e.target);
      if (isModShortcut(e) && !e.altKey && !e.shiftKey && e.key.toLowerCase() === "z") {
        if (!typing && toast?.undo && !toast.actionLabel) {
          e.preventDefault();
          undoToast();
        }
        return;
      }
      const action = matchShortcut(e);
      if (!action) return;
      // Cmd/Ctrl+K, +F and +, also work from the search field; single keys never fire while typing.
      if (typing && action !== "command" && action !== "search" && action !== "settings") return;
      // Let Enter activate a focused button instead of opening the selected row.
      if (action === "open" && e.target instanceof Element && e.target.closest("button, a, [role='menuitem']")) return;
      e.preventDefault();
      run(action);
    }
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  });

  function toggleDone() {
    changeView({ showDone: !view.showDone }, true);
  }

  // ---- Command Center ----

  function actionCommands(targetId?: number): Command[] {
    const m = targetId === undefined ? current : messages.find((x) => x.id === targetId);
    const messageCommands: Command[] = m
      ? [
          {
            id: "done",
            label: m.state === "done" ? "Move to Inbox" : "Done",
            keywords: "archive",
            Icon: Check,
            shortcut: shortcutLabel("done"),
            run: () => actions.done(m),
          },
          {
            id: "snooze-tomorrow",
            label: "Snooze until tomorrow",
            keywords: "later",
            Icon: Clock,
            detail: presetDetail(LATER_PRESETS[1]),
            run: () => actions.snooze(m, LATER_PRESETS[1]),
          },
          {
            id: "snooze",
            label: "Snooze…",
            keywords: "later",
            Icon: Clock,
            shortcut: shortcutLabel("snooze"),
            run: () => snoozeMenu(m),
          },
          {
            id: "pin",
            label: m.pinned ? "Unpin" : "Pin",
            keywords: "star",
            Icon: Pin,
            shortcut: shortcutLabel("pin"),
            run: () => actions.pin(m),
          },
          {
            id: "aside",
            label: m.state === "aside" ? "Move back to Inbox" : "Set Aside",
            Icon: ArrowDownLeft,
            shortcut: shortcutLabel("aside"),
            run: () => actions.aside(m),
          },
          {
            id: "read",
            label: m.unread ? "Mark as Read" : "Mark as Unread",
            keywords: "seen unread read",
            Icon: Circle,
            shortcut: shortcutLabel("read"),
            run: () => actions.read(m),
          },
          {
            id: "priority",
            label: m.priority ? "Remove from Priority" : "Mark as Priority",
            keywords: "important",
            Icon: Zap,
            shortcut: shortcutLabel("priority"),
            run: () => actions.priority(m),
          },
          {
            id: "trash",
            label: "Move to Trash",
            keywords: "delete",
            Icon: Trash2,
            shortcut: shortcutLabel("trash"),
            run: () => actions.trash(m),
          },
          { id: "reply", label: "Reply", Icon: Reply, shortcut: shortcutLabel("reply"), run: () => reply(m) },
          {
            id: "reply-all",
            label: "Reply all",
            Icon: ReplyAll,
            shortcut: shortcutLabel("replyAll"),
            run: () => void replyAll(m),
          },
          {
            id: "forward",
            label: "Forward",
            Icon: Forward,
            shortcut: shortcutLabel("forward"),
            run: () => void forward(m),
          },
          ...(
            [
              { action: "summarize", label: "+AI Summary: Short", Icon: Text },
              { action: "summarizeDetailed", label: "+AI Summary: Detailed", Icon: AlignLeft },
              { action: "summarizeActions", label: "+AI Summary: Action Points", Icon: ListChecks },
            ] as const
          ).map(({ action, label, Icon }) => ({
            id: action,
            label,
            keywords: "ai summarize summary",
            Icon,
            shortcut: shortcutLabel(action),
            run: () => {
              if (openId !== m.id) openThread(m.id);
              summarize(m, SUMMARY_ACTIONS[action]);
            },
          })),
        ]
      : [];
    return [
      ...messageCommands,
      {
        id: "compose",
        label: "Compose new email",
        keywords: "new write",
        Icon: Pen,
        shortcut: shortcutLabel("compose"),
        run: () => compose(),
      },
      { id: "search", label: "Search", keywords: "find", Icon: Search, shortcut: shortcutLabel("search"), run: openSearch },
      { id: "sync", label: "Check for new mail", keywords: "sync refresh fetch receive", Icon: RefreshCw, run: checkMail },
      { id: "go-inbox", label: "Go to Inbox", keywords: "folder", Icon: Inbox, run: () => selectFolder("inbox") },
      ...FOLDERS.map(({ key, label, Icon }) => ({
        id: `go-${key}`,
        label: `Go to ${label}`,
        keywords: "folder",
        Icon,
        run: () => selectFolder(key),
      })),
      ...INBOX_MODES.map(({ key, label, action }) => ({
        id: `mode-${key}`,
        label: `Switch to ${label}`,
        keywords: "inbox mode view",
        Icon: MODE_ICONS[key],
        shortcut: shortcutLabel(action),
        run: () => setMode(key),
      })),
      {
        id: "toggle-done",
        label: view.showDone ? "Hide emails marked as Done" : "Show emails marked as Done",
        keywords: "done archive toggle",
        Icon: CircleCheck,
        shortcut: shortcutLabel("toggleDone"),
        run: toggleDone,
      },
      {
        id: "settings",
        label: "Settings",
        keywords: "preferences options",
        Icon: SettingsIcon,
        shortcut: shortcutLabel("settings"),
        run: () => openSettings(),
      },
      {
        id: "add-account",
        label: "Add account",
        keywords: "accounts gmail google microsoft outlook office 365 exchange imap sign in login",
        Icon: UserPlus,
        run: () => openSettings("accounts"),
      },
      {
        id: "notification-settings",
        label: "Notification settings",
        keywords: "notify alerts sound tray background close startup sign in login",
        Icon: Bell,
        run: () => openSettings("notifications"),
      },
      {
        id: "ai-settings",
        label: "AI settings",
        keywords: "api key provider anthropic openai model",
        Icon: Sparkles,
        run: () => openSettings("ai"),
      },
      {
        id: "sidebar",
        label: "Toggle sidebar",
        Icon: PanelLeft,
        shortcut: shortcutLabel("sidebar"),
        run: () => setSidebarMode((s) => (s === "rail" ? "overlay" : "rail")),
      },
    ];
  }

  const snoozeTarget = palette?.kind === "snooze" ? messages.find((m) => m.id === palette.targetId) : undefined;
  const snoozeCommands: Command[] = snoozeTarget
    ? LATER_PRESETS.map((p) => ({
        id: p.key,
        label: p.label,
        Icon: Clock,
        detail: presetDetail(p),
        run: () => actions.snooze(snoozeTarget, p),
      }))
    : [];

  // ---- layout ----

  const rows: RowActions = {
    open: (id) => openThread(id),
    done: actions.done,
    pin: actions.pin,
    snooze: snoozeMenu,
    aside: actions.aside,
    trash: actions.trash,
    priority: actions.priority,
    read: actions.read,
    more: (m) => setPalette({ kind: "actions", targetId: m.id }),
  };

  const threadActions: ThreadActions = {
    ...actions,
    close: () => setOpenId(null),
    summarize,
    stopSummary,
    deleteSummary,
    summaryCopied: () => showToast("Summary Copied", undefined, ICONS.copy),
    openAiSettings: () => openSettings("ai"),
    reply: (m, body) => reply(m, body),
    replyAll: (m) => void replyAll(m),
    forward: (m) => void forward(m),
    openAttachment: (m, index) => openAttachment(m.id, index).catch(fileError),
    saveAttachment: async (m, index) => {
      try {
        if (await saveAttachment(m.id, index)) showToast("Attachment saved", undefined, ICONS.download);
      } catch (error) {
        fileError(error);
      }
    },
    saveAllAttachments: async (m) => {
      try {
        const saved = await saveAllAttachments(m.id);
        if (saved > 0) showToast(`${plural(saved, "attachment")} saved`, undefined, ICONS.download);
      } catch (error) {
        fileError(error);
      }
    },
  };

  function fileError(error: unknown) {
    showToast(String(error), undefined, ICONS.error, { duration: ERROR_TOAST_MS });
  }

  const showList = twoColumns || !openMessage;
  const showReader = twoColumns || openMessage !== null;
  const readerLeft = sidebarSpace + (twoColumns ? listWidthUsed : 0);

  return (
    <AccountsContext value={accounts}>
      <div className="flex h-full bg-base-100 text-base-content">
        <Sidebar
          mode={sidebarMode}
          width={sidebarWidth}
          folder={view.folder}
          account={view.account}
          counts={counts}
          syncing={syncing}
          onSelect={selectFolder}
          onOpen={() => setSidebarMode("overlay")}
          onClose={() => setSidebarMode("rail")}
          onToggleLock={() => setSidebarMode((m) => (m === "locked" ? "rail" : "locked"))}
          onResize={setSidebarWidth}
          onSync={checkMail}
          onSettings={() => openSettings()}
        />

        <main className="relative flex min-w-0 flex-1">
          <DragStrip />
          <CaptionButtons />
          {showList && (
            <section
              aria-label="Message list"
              className={`relative flex min-w-0 flex-col pt-8 ${twoColumns ? "shrink-0 border-r border-hairline" : "flex-1"}`}
              style={twoColumns ? { width: listWidthUsed } : undefined}
            >
              <MessageList
                model={model}
                view={view}
                selectedId={selectedId}
                openId={openId}
                rows={rows}
                gatekeeper={gatekeeper}
                onDoneAll={doneAll}
                onShowAll={(key) => changeView({ sub: { kind: "card", key } }, true)}
                onOpenGroup={(key) => changeView({ sub: { kind: "group", key } }, true)}
                onBack={() => changeView({ sub: null }, true)}
                onModeChange={setMode}
                onSearchOpen={openSearch}
                onSearchChange={(search) => setView((v) => ({ ...v, search }))}
                onSearchClose={closeSearch}
                onSearchKeyDown={onSearchKeyDown}
                onCompose={() => compose()}
                onToggleDone={toggleDone}
                onOpenGatekeeper={() => changeView({ sub: { kind: "gatekeeper" } }, true)}
                searchRef={searchRef}
                demo={isDemoMode(accounts)}
                onAddAccounts={() => openSettings("accounts")}
              />
              {twoColumns && (
                <Splitter
                  width={listWidthUsed}
                  onResize={(w) => setListWidth(clamp(w, LIST_MIN, Math.min(LIST_MAX, container - READER_MIN)))}
                />
              )}
            </section>
          )}
          {showReader && (
            <section aria-label="Conversation" className="flex min-w-0 flex-1 flex-col pt-8">
              {openMessage ? (
                <ThreadView
                  key={openMessage.id}
                  message={openMessage}
                  singleColumn={!twoColumns}
                  summary={summaries.get(openMessage.id)}
                  aiProvider={aiProvider}
                  actions={threadActions}
                />
              ) : (
                <EmptyReader />
              )}
            </section>
          )}
        </main>

        {draft && (
          <Composer
            key={composerKey}
            draft={draft}
            full={composerFull}
            fullStyle={{ left: readerLeft, right: 0 }}
            onChange={(patch) => setDraft((d) => d && { ...d, ...patch })}
            onToggleFull={() => setComposerFull((f) => !f)}
            onClose={closeComposer}
            onSend={send}
            onError={(text) => showToast(text, undefined, ICONS.error, { duration: ERROR_TOAST_MS })}
          />
        )}

        {palette?.kind === "actions" && (
          <CommandCenter
            key="actions"
            title="Actions"
            placeholder="Type a command, or go to…"
            commands={actionCommands(palette.targetId)}
            onRun={(c) => {
              setPalette(null);
              c.run();
            }}
            onClose={() => setPalette(null)}
          />
        )}
        {palette?.kind === "snooze" && (
          <CommandCenter
            key="snooze"
            title={snoozeTarget ? `Snooze · ${snoozeTarget.subject}` : "Snooze"}
            placeholder="Snooze until…"
            commands={snoozeCommands}
            onRun={(c) => {
              setPalette(null);
              c.run();
            }}
            onClose={() => setPalette(null)}
          />
        )}

        {settingsPage && (
          <Settings
            page={settingsPage}
            onPage={(page) => {
              lastSettingsPage.current = page;
              setSettingsPage(page);
            }}
            onClose={() => setSettingsPage(null)}
            theme={theme}
            onTheme={changeTheme}
            onAccountsChanged={() => void reloadAccounts()}
          />
        )}

        {toast && <UndoToast toast={toast} left={sidebarSpace + 16} onUndo={undoToast} />}
      </div>
    </AccountsContext>
  );
}

function EmptyReader() {
  return (
    <div className="grid flex-1 place-items-center text-center">
      <div>
        <span className="mx-auto grid size-20 place-items-center rounded-full bg-base-200 text-text-3">
          <MailOpen size={32} />
        </span>
        <p className="mt-3 text-sm text-text-2">No conversation selected</p>
      </div>
    </div>
  );
}

function previousValues(m: Message, patch: MessagePatch): MessagePatch {
  const previous: MessagePatch = {};
  for (const key of Object.keys(patch) as (keyof MessagePatch)[]) Object.assign(previous, { [key]: m[key] });
  if (patch.state !== undefined && m.state === "snoozed") previous.snoozedUntil = m.snoozedUntil;
  return previous;
}

/** Every message a list shows, including card rows past the limit and Gatekeeper senders. */
function listedIds(list: ListModel) {
  const ids = new Set<number>();
  for (const block of list.blocks) {
    if (block.kind === "row") ids.add(block.message.id);
    else if (block.kind !== "period") for (const m of block.items) ids.add(m.id);
  }
  return ids;
}
