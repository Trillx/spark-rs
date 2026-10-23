import {
  AlignLeft,
  ArrowDownLeft,
  Bell,
  Bot,
  Check,
  CircleAlert,
  CircleCheck,
  Circle,
  Clock,
  Copy,
  Download,
  FileText,
  Folder,
  FolderSearch,
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
  QUICK_REPLIES,
  cancelQuickReply,
  cancelSummary,
  draftQuickReply,
  getAiSettings,
  presetLabel,
  summarizeMessage,
  type AiEvent,
  type QuickReply,
  type SummaryStyle,
} from "./ai";
import {
  cancelSend,
  createFolder,
  deleteFolder,
  discardDraft,
  listAccounts,
  listFolders,
  listMessages,
  loadOlder,
  onAccountsChanged,
  onAgentActivity,
  onAgentSending,
  onFoldersChanged,
  onMailboxChanged,
  onOpenMessage,
  onSendStatus,
  onSyncStatus,
  openAttachment,
  openDraft,
  patched,
  refreshFolders,
  renameFolder,
  saveAllAttachments,
  saveAttachment,
  saveDraft,
  sendMessage,
  syncFolders,
  syncNow,
  updateMessage,
  watchFolders,
} from "./api";
import CommandCenter, { type Command } from "./components/CommandCenter";
import Composer, {
  draftHasContent,
  draftKey,
  emptyDraft,
  outgoingDraft,
  savedDraft,
  type Draft,
  type Writing,
} from "./components/Composer";
import { PromptDialog } from "./components/Dialog";
import MessageList, { Splitter, type GatekeeperActions } from "./components/MessageList";
import type { RowActions } from "./components/MessageRow";
import Settings, { type SettingsPage } from "./components/Settings";
import Sidebar, { type SidebarActions, type SidebarMode } from "./components/Sidebar";
import { reduceSummary, startSummary, type SummaryState } from "./components/SummaryCard";
import ThreadView, { type ThreadActions } from "./components/ThreadView";
import UndoToast, { TOAST_MS, type ToastData, type ToastIcon } from "./components/UndoToast";
import { CaptionButtons, DragStrip } from "./components/WindowChrome";
import { AccountsContext, isDemoMode, isFromYou } from "./lib/accounts";
import { clearContent, loadContent } from "./lib/content";
import {
  asThread,
  conversationOf,
  indexConversations,
  loadMinimize,
  movesWith,
  saveMinimize,
} from "./lib/conversation";
import { isDraft } from "./lib/drafts";
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
  INBOX_MODES,
  buildList,
  sidebarCounts,
  type AccountFilter,
  type InboxMode,
  type ListContext,
  type ListModel,
  type ListView,
} from "./lib/layout";
import {
  LIST_MAX,
  LIST_MIN,
  READER_MIN,
  SIDEBAR_RAIL,
  clamp,
  effectiveListWidth,
  useTwoColumns,
  useWindowWidth,
} from "./lib/panes";
import {
  SEEN_MAX,
  SYSTEM_LISTS,
  SYSTEM_ORDER,
  addSmartFolder,
  addToSidebar,
  dropPatch,
  folderIdOf,
  folderKey,
  hasAccountRows,
  inSidebar,
  isSystemList,
  listFolderIds,
  listInfo,
  loadLayout,
  loadSeen,
  pruneLayout,
  removeSmartFolder,
  renameSmartFolder,
  saveLayout,
  saveSeen,
  smartIdOf,
  visit,
  watchedFolderIds,
  type ListKey,
  type SidebarLayout,
} from "./lib/sidebar";
import {
  isInsideOverlay,
  isModShortcut,
  isTypingTarget,
  matchShortcut,
  shortcutLabel,
  type ActionId,
} from "./lib/shortcuts";
import { pageList, pageReader, scrollToEnd } from "./lib/scroll";
import type {
  AccountView,
  AgentSendingEvent,
  FolderView,
  Message,
  MessagePatch,
  SendStatusEvent,
  SyncStatusEvent,
} from "./types";

/** `targetId` points the actions at one row (its ⋯) rather than the current message. */
type Palette = { kind: "actions"; targetId?: number } | { kind: "snooze"; targetId: number };

/** A quick reply AI is writing in the composer it opened (by `composerKey`). */
type ReplyWriting = Writing & { composer: number; repliedTo: Message; chip: QuickReply };

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
  draft: { Icon: FileText, className: "text-primary" },
  copy: { Icon: Copy, className: "text-primary" },
  folder: { Icon: Folder, className: "text-primary" },
  download: { Icon: Download, className: "text-primary" },
  error: { Icon: CircleAlert, className: "text-error" },
  agent: { Icon: Bot, className: "text-primary" },
} satisfies Record<string, ToastIcon>;

/** Failures stay up longer than undo toasts. */
const ERROR_TOAST_MS = 10_000;
/** The composer saves its draft after this long without typing. */
const AUTOSAVE_MS = 4_000;

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
  /** Which emails of the open conversation were unread when it opened: reading starts there. */
  const [openedUnread, setOpenedUnread] = useState<ReadonlySet<number>>(new Set());
  const [folders, setFolders] = useState<FolderView[]>([]);
  const [layout, setLayout] = useState<SidebarLayout>(loadLayout);
  const [sidebarMode, setSidebarMode] = useState<SidebarMode>(() => (layout.locked ? "locked" : "rail"));
  const [pickerOpen, setPickerOpen] = useState(false);
  /** Recently opened messages, newest first. */
  const [seen, setSeen] = useState<number[]>(loadSeen);
  /** A search being saved as a smart folder, while its name is asked for. */
  const [savingSearch, setSavingSearch] = useState<string | null>(null);
  const [listWidth, setListWidth] = useState(LIST_MIN);
  const [draft, setDraft] = useState<Draft | null>(null);
  const [composerKey, setComposerKey] = useState(0);
  const [composerFull, setComposerFull] = useState(false);
  /** `draftKey` of the composer's draft as it is in Drafts (opened or saved), if it is. */
  const [draftSaved, setDraftSaved] = useState<string | null>(null);
  /** The saved draft each composer session (by `composerKey`) saved to. */
  const draftIds = useRef(new Map<number, number>());
  /** Saves run one after another, so saving again updates the same draft. */
  const draftSaves = useRef<Promise<unknown>>(Promise.resolve());
  /** The composer as it stands, for callbacks that outlive a render (undo, autosave). */
  const composer = useRef({ draft, saved: draftSaved, key: composerKey });
  composer.current = { draft, saved: draftSaved, key: composerKey };
  const [writing, setWriting] = useState<ReplyWriting | null>(null);
  /** Drafts deleted while their Undo toast is up: hidden now, deleted when it's gone. */
  const [discarding, setDiscarding] = useState<ReadonlySet<number>>(new Set());
  const [palette, setPalette] = useState<Palette | null>(null);
  /** +AI summaries by message id, kept while the app runs. */
  const [summaries, setSummaries] = useState<ReadonlyMap<number, SummaryState>>(new Map());
  const summaryRun = useRef(0);
  const [settingsPage, setSettingsPage] = useState<SettingsPage | null>(null);
  const lastSettingsPage = useRef<SettingsPage>("accounts");
  const [theme, setTheme] = useState<ThemePref>(loadTheme);
  /** Settings → Appearance → Minimize old emails. */
  const [minimize, setMinimize] = useState(loadMinimize);
  /** The configured provider's name, for "Anthropic declined…". */
  const [aiProvider, setAiProvider] = useState("The AI provider");
  const [toast, setToast] = useState<ToastData | null>(null);
  const toastTimer = useRef<number | undefined>(undefined);
  const searchRef = useRef<HTMLInputElement>(null);
  /** The list's and the reader's scrolling elements, for Page Up/Down, Home and End. */
  const listScroll = useRef<HTMLDivElement>(null);
  const readerScroll = useRef<HTMLDivElement>(null);
  /** The pane clicked last: keyboard scrolling moves it. */
  const scrollPane = useRef<"list" | "reader">("list");
  /** Older mail is on its way for the open list. */
  const [loadingOlder, setLoadingOlder] = useState(false);
  const reaching = useRef(false);

  const windowWidth = useWindowWidth();
  const sidebarWidth = layout.width;
  const sidebarSpace = sidebarMode === "locked" ? sidebarWidth : SIDEBAR_RAIL;
  const container = windowWidth - sidebarSpace;
  const listWidthUsed = effectiveListWidth(listWidth, container);
  const twoColumns = useTwoColumns(container, listWidthUsed);

  const ctx: ListContext = useMemo(
    () => ({ accounts, folders, smartFolders: layout.smartFolders, seen }),
    [accounts, folders, layout.smartFolders, seen],
  );
  const listed = useMemo(
    () => (discarding.size ? messages.filter((m) => !discarding.has(m.id)) : messages),
    [messages, discarding],
  );
  const model = useMemo(() => buildList(listed, view, sticky, ctx), [listed, view, sticky, ctx]);
  const counts = useMemo(
    () => sidebarCounts(listed, [...layout.favorites, ...layout.folders, ...layout.recents], ctx),
    [listed, layout, ctx],
  );
  /** The open list's folders haven't synced yet, so an empty list may only be loading. */
  const loading = listFolderIds(view.folder, folders, accountOf(view.account)).some(
    (id) => !folders.find((f) => f.id === id)?.synced,
  );
  const conversations = useMemo(() => indexConversations(messages, folders), [messages, folders]);
  const older = olderSources(view, accounts, folders);
  const openMessage = messages.find((m) => m.id === openId) ?? null;
  const conversation = useMemo(
    () => (openMessage ? conversationOf(conversations, openMessage) : []),
    [conversations, openMessage],
  );
  /** The list row an email shows in, or the email itself if none does. */
  const rowOf = (id: number) => model.rowOf.get(id) ?? id;
  const selected = messages.find((m) => m.id === selectedId && model.order.includes(rowOf(m.id))) ?? null;
  /** What message shortcuts and commands act on. */
  const current = openMessage ?? selected;
  const searching = view.search !== null;
  const syncing = accounts.some((a) => a.sync.status === "syncing");

  useEffect(() => {
    void reloadFolders();
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
  const events = useRef({
    reloadMessages,
    reloadAccounts,
    reloadFolders,
    syncStatus,
    sendStatus,
    openFromNotification,
    agentSending,
  });
  events.current = {
    reloadMessages,
    reloadAccounts,
    reloadFolders,
    syncStatus,
    sendStatus,
    openFromNotification,
    agentSending,
  };
  useEffect(() => {
    const stops = [
      onMailboxChanged(() => void events.current.reloadMessages()),
      onFoldersChanged(() => void events.current.reloadFolders()),
      onAccountsChanged(() => void events.current.reloadAccounts()),
      onSyncStatus((e) => events.current.syncStatus(e)),
      onSendStatus((e) => events.current.sendStatus(e)),
      onOpenMessage((id) => events.current.openFromNotification(id)),
      onAgentActivity((e) => showToast(e.text, undefined, ICONS.agent)),
      onAgentSending((e) => events.current.agentSending(e)),
    ];
    return () => stops.forEach((stop) => stop());
  }, []);

  useEffect(() => {
    if (searching) searchRef.current?.focus();
  }, [searching]);

  useEffect(() => saveLayout(layout), [layout]);
  useEffect(() => saveSeen(seen), [seen]);

  // The backend keeps the sidebar's folders and the open one in sync.
  const watched = watchedFolderIds(layout, folders, view.folder).join(",");
  useEffect(() => {
    void watchFolders(watched ? watched.split(",").map(Number) : []).catch(() => {});
  }, [watched]);

  useEffect(() => {
    if (settingsPage !== null) return;
    void getAiSettings().then(
      (view) => view && setAiProvider(presetLabel(view.preset)),
      () => {},
    );
  }, [settingsPage]);

  // ---- data ----

  /** The list is scrolled near its end: bring in older mail, as Spark does. */
  function reachBack() {
    if (!older || reaching.current) return;
    reaching.current = true;
    setLoadingOlder(true);
    void loadOlder(older.accounts, older.folders)
      .then(() => Promise.all([reloadMessages(), reloadAccounts(), reloadFolders()]))
      .catch(() => {})
      .finally(() => {
        reaching.current = false;
        setLoadingOlder(false);
      });
  }

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
          setSeen([]);
        }
        void reloadMessages();
        void reloadFolders();
      },
      (error) => showToast(`Couldn't load accounts: ${String(error)}`, undefined, ICONS.error),
    );
  }

  /** Reloads the folder list; folders that are gone leave the sidebar, and the list closes if it was open. */
  function reloadFolders() {
    return listFolders().then(
      (loaded) => {
        setFolders(loaded);
        setLayout((l) => pruneLayout(l, loaded));
        const ids = new Set(loaded.map((f) => f.id));
        setView((v) => {
          const id = folderIdOf(v.folder);
          return id !== undefined && !ids.has(id) ? { ...v, folder: "inbox", sub: null } : v;
        });
      },
      (error) => showToast(`Couldn't load your folders: ${String(error)}`, undefined, ICONS.error),
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
    // A saved draft it came from is gone now, or back in Drafts if it failed.
    if (sent?.draftId !== undefined) void reloadMessages();
    if (e.status !== "failed") return;
    showToast(
      `Couldn't send: ${e.message ?? "the server refused the message."}`,
      sent && (() => openComposer(sent)),
      ICONS.error,
      { actionLabel: "Edit", duration: ERROR_TOAST_MS },
    );
  }

  /** An agent's email waits in the outbox; the toast's Undo stops it until `sendAt`. */
  function agentSending(e: AgentSendingEvent) {
    const subject = e.subject.trim() || "(no subject)";
    showToast(
      `${e.agent} is sending “${subject}” to ${e.to}`,
      () =>
        void cancelSend(e.id).then(
          (cancelled) => showToast(cancelled ? "Not sent" : "Already sent", undefined, ICONS.send),
          (error) => showToast(`Couldn't stop it: ${String(error)}`, undefined, ICONS.error),
        ),
      ICONS.agent,
      { duration: Math.max(e.sendAt - Date.now(), TOAST_MS) },
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
    const remaining = listedIds(buildList(after, view, sticky, ctx));
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
    const i = order.indexOf(rowOf(anchor));
    const next =
      i < 0
        ? null
        : (order.slice(i + 1).find((id) => !gone.has(id)) ??
          order
            .slice(0, i)
            .reverse()
            .find((id) => !gone.has(id)) ??
          null);
    if (twoColumns && openId !== null && next !== null) follow(next);
    else {
      setOpenId(null);
      setSelectedId(next);
    }
  }

  // ---- navigation ----

  function openThread(id: number, list = messages) {
    setSelectedId(id);
    setOpenId(id);
    setSeen((s) => (s[0] === id ? s : [id, ...s.filter((x) => x !== id)].slice(0, SEEN_MAX)));
    const m = list.find((x) => x.id === id);
    // Opening a conversation reads all of it. Opening it again (clicking its
    // row while it's open) keeps where reading started.
    const index = list === messages ? conversations : indexConversations(list, folders);
    const unread = m ? conversationOf(index, m).filter((x) => x.unread) : [];
    if (id !== openId) setOpenedUnread(new Set(unread.map((x) => x.id)));
    if (unread.length) {
      setSticky((s) => new Set([...s, ...unread.map((x) => x.id)]));
      for (const x of unread) void update(x.id, { unread: false });
    }
  }

  /** Opens a message: a draft in the composer, other mail in the reader. */
  function openListed(id: number) {
    const m = messages.find((x) => x.id === id);
    if (m && isDraft(m, folders)) void editDraft(m);
    else openThread(id);
  }

  /** The cursor moved: split view shows the message there, but a draft only opens when asked. */
  function follow(id: number) {
    const m = messages.find((x) => x.id === id);
    if (m && isDraft(m, folders)) {
      setOpenId(null);
      setSelectedId(id);
    } else {
      openThread(id);
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
    const i = anchor === null ? -1 : order.indexOf(rowOf(anchor));
    const next = i < 0 ? order[0] : order[clamp(i + step, 0, order.length - 1)];
    if (twoColumns || openId !== null) follow(next);
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

  /** Opens a list, narrowed to an account if it has account rows. Server-backed lists sync now. */
  function selectList(folder: ListKey, account: AccountFilter = "all") {
    const narrowed = hasAccountRows(folder) ? account : "all";
    changeView({ folder, account: narrowed, sub: null, search: null });
    closeSidebar();
    const ids = listFolderIds(folder, folders, accountOf(narrowed));
    if (ids.length) void syncFolders(ids).catch(() => {});
  }

  /** Closes the folder picker, and the sidebar if it's showing over the list. */
  function closeSidebar() {
    setPickerOpen(false);
    setSidebarMode((m) => (m === "overlay" ? "rail" : m));
  }

  function toggleSidebar() {
    if (sidebarMode === "locked") return;
    setPickerOpen(false);
    setSidebarMode((m) => (m === "rail" ? "overlay" : "rail"));
  }

  function toggleLock() {
    const locked = sidebarMode !== "locked";
    setSidebarMode(locked ? "locked" : "rail");
    setLayout((l) => ({ ...l, locked }));
    if (!locked) setPickerOpen(false);
  }

  function togglePicker() {
    if (!pickerOpen) void refreshFolders().catch(() => {});
    setPickerOpen(!pickerOpen);
  }

  /** From the folder picker: open the list (it shows under Recents), or with `add`, put it in the sidebar. */
  function pick(key: ListKey, add: boolean) {
    if (add) {
      if (inSidebar(layout, key)) showToast("This folder is already in the sidebar");
      else setLayout((l) => addToSidebar(l, key));
      return;
    }
    setLayout((l) => visit(l, key));
    selectList(key);
  }

  /** The folder picker's +: search, then save the search from the search field. */
  function newSmartFolder() {
    closeSidebar();
    openSearch();
  }

  function saveSmartFolder(name: string, query: string) {
    const [next, key] = addSmartFolder(layout, name, query);
    setLayout(next);
    selectList(key);
  }

  async function newFolder(account: number, parent: number | undefined, name: string) {
    const created = await createFolder(account, parent, name);
    await reloadFolders();
    setLayout((l) => addToSidebar(l, folderKey(created.id)));
  }

  async function renameList(key: ListKey, name: string) {
    const smart = smartIdOf(key);
    if (smart !== undefined) {
      setLayout((l) => renameSmartFolder(l, smart, name));
      return;
    }
    const id = folderIdOf(key);
    if (id === undefined) return;
    await renameFolder(id, name);
    await reloadFolders();
  }

  async function deleteList(key: ListKey) {
    const smart = smartIdOf(key);
    if (smart !== undefined) setLayout((l) => removeSmartFolder(l, smart));
    else {
      const id = folderIdOf(key);
      if (id === undefined) return;
      await deleteFolder(id);
      await reloadFolders();
    }
    if (view.folder === key) selectList("inbox");
  }

  const setMode = (mode: InboxMode) => changeView({ folder: "inbox", mode, sub: null, search: null }, true);

  function openSearch() {
    if (searching) searchRef.current?.select();
    else changeView({ search: "", sub: null }, true);
  }

  const closeSearch = () => changeView({ search: null }, true);

  /** Esc: the folder picker and sidebar overlay, then the thread, then a sub-list, then search. */
  function closeTopmost() {
    if (pickerOpen || sidebarMode === "overlay") closeSidebar();
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
      openListed(selectedId);
    }
  }

  // ---- message actions ----

  // Rows and the thread toolbar act on whole conversations, as Spark does.

  /** The conversation of `m`, which may be a list row's copy (`asThread`). */
  const threadOf = (m: Message) => conversationOf(conversations, m);
  /** What moving `m` moves: its conversation's emails in the same place (see `movesWith`). */
  const together = (m: Message) => movesWith(threadOf(m), m);
  /** `m` as stored, not a row's copy with the conversation's flags, so undo puts back its own. */
  const own = (m: Message) => messages.find((x) => x.id === m.id) ?? m;

  /** Pin and Priority mark the conversation: off for every email that has it, on for `m`. */
  function flag(m: Message, key: "pinned" | "priority", on: string, off: string, icon: ToastIcon) {
    const flagged = threadOf(m).filter((x) => x[key]);
    if (flagged.length) act(flagged, { [key]: false }, off, icon);
    else act([own(m)], { [key]: true }, on, icon);
  }

  const actions = {
    done: (m: Message) =>
      m.state === "done"
        ? act(together(m), { state: "inbox" }, "Moved to Inbox", ICONS.inbox)
        : act(together(m), { state: "done" }, "Marked as Done", ICONS.done),
    trash: (m: Message) => {
      if (isDraft(m, folders)) discardListed(m);
      else if (m.state !== "deleted") act(together(m), { state: "deleted" }, "Moved to Trash", ICONS.trash);
    },
    pin: (m: Message) => flag(m, "pinned", "Pinned", "Unpinned", ICONS.pin),
    priority: (m: Message) => flag(m, "priority", "Marked as Priority", "Removed from Priority", ICONS.priority),
    /** Reading marks the whole conversation read; unread marks `m`. */
    read: (m: Message) => {
      const unread = threadOf(m).filter((x) => x.unread);
      if (unread.length) act(unread, { unread: false }, "Marked as Read", ICONS.read);
      else act([own(m)], { unread: true }, "Marked as Unread", ICONS.read);
    },
    aside: (m: Message) =>
      m.state === "aside"
        ? act(together(m), { state: "inbox" }, "Moved to Inbox", ICONS.inbox)
        : act(together(m), { state: "aside" }, "Set aside", ICONS.aside),
    snooze: (m: Message, p: LaterPreset) =>
      act(
        together(m),
        { state: "snoozed", snoozedUntil: laterTime(p) },
        `Snoozed until ${p.label.toLowerCase()}`,
        ICONS.snooze,
      ),
    accept: (m: Message) => act([m], { screening: false }, "Sender accepted", ICONS.accept),
    block: (m: Message) => act([m], { state: "blocked" }, "Sender blocked", ICONS.block),
  };

  /** A card's or sub-list's rows, each with its conversation. */
  function doneAll(items: Message[]) {
    const rows = items.filter((m) => m.state !== "done");
    act(
      rows.flatMap(together),
      { state: "done" },
      rows.length === 1 ? "Marked as Done" : `${rows.length} emails marked as Done`,
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

  /**
   * A row dragged onto a sidebar list: its conversation filed in a folder, or
   * done, trashed or back in the Inbox; or the email pinned.
   */
  function dropMessage(dragged: Message, key: ListKey, account?: number) {
    const m = messages.find((x) => x.id === dragged.id);
    const patch = m && dropPatch(m, key, folders, account);
    if (!m || !patch) return;
    const moved = together(m);
    if (isDraft(m, folders)) discardListed(m);
    else if (patch.folder !== undefined) {
      const name = folders.find((f) => f.id === patch.folder)?.name ?? "the folder";
      act(moved, patch, `Moved to ${name}`, ICONS.folder);
    } else if (key === "archive") act(moved, patch, "Marked as Done", ICONS.done);
    else if (key === "trash") act(moved, patch, "Moved to Trash", ICONS.trash);
    else if (key === "pins") act([m], patch, "Pinned", ICONS.pin);
    else act(moved, patch, "Moved to Inbox", ICONS.inbox);
  }

  // ---- composer ----

  /** `saved` is the new draft's `draftKey` if it's in Drafts as it is. An open composer's changes are saved. */
  function openComposer(next: Draft, saved: string | null = null) {
    stopWriting();
    const open = composer.current;
    if (open.draft && draftHasContent(open.draft) && draftKey(open.draft) !== open.saved) {
      void storeDraft(open.draft, open.key).then(reloadMessages, (error) => draftError(error, open.draft!));
    }
    const key = open.key + 1;
    composer.current = { draft: next, saved, key };
    setDraft(next);
    setDraftSaved(saved);
    setComposerKey(key);
  }

  /** Saves a composer session's draft to its account's Drafts, after any save still running. Resolves to its id. */
  function storeDraft(d: Draft, session: number): Promise<number> {
    const run = draftSaves.current.then(async () => {
      const id = await saveDraft(outgoingDraft(d, 0, draftIds.current.get(session) ?? d.draftId));
      draftIds.current.set(session, id);
      return id;
    });
    draftSaves.current = run.catch(() => {});
    return run;
  }

  /** The saved draft a composer session edits, once its saves are done. */
  async function savedDraftId(d: Draft, session: number) {
    await draftSaves.current;
    return draftIds.current.get(session) ?? d.draftId;
  }

  function draftError(error: unknown, d: Draft) {
    showToast(`Couldn't save the draft: ${String(error)}`, () => openComposer(d), ICONS.error, {
      actionLabel: "Edit",
      duration: ERROR_TOAST_MS,
    });
  }

  // Changes are saved to Drafts after a pause in typing.
  useEffect(() => {
    if (!draft || !draftHasContent(draft)) return;
    const key = draftKey(draft);
    if (key === draftSaved) return;
    const session = composerKey;
    const timer = window.setTimeout(() => {
      storeDraft(draft, session).then(
        () => {
          if (composer.current.key === session) setDraftSaved(key);
          void reloadMessages();
        },
        () => {}, // Closing the composer saves again, and says if it fails.
      );
    }, AUTOSAVE_MS);
    return () => window.clearTimeout(timer);
  }, [draft, draftSaved, composerKey]);

  /** A draft from Drafts, in the composer. Already open there, it stays as it is. */
  async function editDraft(m: Message) {
    setSelectedId(m.id);
    const open = composer.current;
    if (open.draft && (draftIds.current.get(open.key) ?? open.draft.draftId) === m.id) return;
    try {
      const next = savedDraft(await openDraft(m.id));
      openComposer(next, draftKey(next));
    } catch (error) {
      showToast(`Couldn't open the draft: ${String(error)}`, undefined, ICONS.error);
    }
  }

  /**
   * Deletes a saved draft once the Undo toast is gone (sooner if another toast
   * replaces it); until then it's out of the lists. `undo` also runs on Undo.
   */
  function discardLater(id: number | undefined, undo?: () => void) {
    if (id === undefined) {
      if (undo) showToast("Draft discarded", undo, ICONS.trash);
      return;
    }
    setDiscarding((s) => new Set(s).add(id));
    const keep = () => setDiscarding((s) => new Set([...s].filter((x) => x !== id)));
    const timer = window.setTimeout(() => {
      discardDraft(id).then(
        () => reloadMessages().then(keep),
        (error) => {
          keep();
          showToast(`Couldn't delete the draft: ${String(error)}`, undefined, ICONS.error);
        },
      );
    }, TOAST_MS);
    showToast(
      "Draft discarded",
      () => {
        window.clearTimeout(timer);
        keep();
        undo?.();
      },
      ICONS.trash,
    );
  }

  /** Move to Trash on a draft in the list. */
  function discardListed(m: Message) {
    moveOffMessages([m.id]);
    discardLater(m.id);
  }

  /** The composer's Delete Draft: it closes, and its saved draft goes too. Undo brings both back. */
  async function discardComposer() {
    if (!draft) return;
    stopWriting();
    const closed = draft;
    const session = composerKey;
    setDraft(null);
    const id = await savedDraftId(closed, session);
    discardLater(id, draftHasContent(closed) ? () => openComposer({ ...closed, draftId: id }) : undefined);
  }

  /** From the account the Inbox is narrowed to, else the first. */
  function compose(seed: Partial<Draft> = {}) {
    const from = view.account !== "all" ? view.account : (accounts[0]?.id ?? 0);
    openComposer({ ...emptyDraft(from), ...seed });
  }

  const replySubject = (m: Message) => (/^re:/i.test(m.subject) ? m.subject : `Re: ${m.subject}`);

  const fromYou = (m: Message) => isFromYou(m, accounts.find((a) => a.id === m.account));

  /** Addresses on `list` other than the account's own. */
  function othersOn(m: Message, list: string[]) {
    const own = accounts.find((a) => a.id === m.account)?.email.toLowerCase();
    return list.filter((r) => r !== "You" && parseAddress(r).address.toLowerCase() !== own);
  }

  /** To the sender; on an email you sent, to the people you sent it to. */
  const reply = (m: Message) =>
    compose({
      title: "Reply",
      to: fromYou(m) ? othersOn(m, m.to ?? []).join(", ") : m.address,
      subject: replySubject(m),
      from: m.account,
      replyTo: m.id,
    });

  /** A quick-reply chip: a reply your AI provider writes, or without one set up, a stock line. */
  function quickReply(m: Message, chip: QuickReply) {
    reply(m);
    writeReply(m, chip, composer.current.key);
  }

  /** Streams a quick reply into composer `session`'s body; events after it closes are dropped. */
  function writeReply(m: Message, chip: QuickReply, session: number) {
    const fail = (message: string) => {
      setDraft((d) => d && { ...d, body: "" });
      setWriting((w) => w && { ...w, status: "error", message });
    };
    const onEvent = (event: AiEvent) => {
      if (composer.current.key !== session) return;
      switch (event.event) {
        case "delta":
          return setDraft((d) => d && { ...d, body: d.body + event.data.text });
        case "done":
          setDraft((d) => d && { ...d, body: d.body.trim() });
          return setWriting(null);
        case "notConfigured":
          setDraft((d) => d && { ...d, body: QUICK_REPLIES.find((q) => q.key === chip)!.text });
          return setWriting(null);
        case "refused":
          return fail(`${aiProvider} declined to write this reply.`);
        case "error":
          return fail(event.data.message);
      }
    };
    setDraft((d) => d && { ...d, body: "" });
    setWriting({ status: "writing", composer: session, repliedTo: m, chip });
    draftQuickReply(m, chip, session, onEvent).catch((error) =>
      onEvent({ event: "error", data: { message: String(error) } }),
    );
  }

  /** Stops AI writing in the composer, keeping what it wrote so far. */
  function stopWriting() {
    if (writing?.status === "writing") void cancelQuickReply(writing.composer);
    setWriting(null);
  }

  /**
   * The sender plus everyone else on To and Cc, minus this account's own
   * address. On an email you sent, its own To and Cc.
   */
  async function replyAll(m: Message) {
    const content = await loadContent(m.id).catch(() => null);
    const sender = m.address.toLowerCase();
    const others = (list: string[]) =>
      othersOn(m, list).filter((r) => parseAddress(r).address.toLowerCase() !== sender);
    const to = (
      fromYou(m) ? othersOn(m, content?.to ?? m.to ?? []) : [m.address, ...others(content?.to ?? [])]
    ).join(", ");
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

  /** Closing keeps the draft in Drafts; one emptied out is deleted there. */
  async function closeComposer() {
    if (!draft) return;
    stopWriting();
    const closed = draft;
    const session = composerKey;
    const unchanged = draftKey(closed) === draftSaved;
    setDraft(null);
    if (!draftHasContent(closed)) {
      discardLater(await savedDraftId(closed, session));
      return;
    }
    if (unchanged) {
      if (draftIds.current.has(session)) showToast("Draft saved", undefined, ICONS.draft);
      return;
    }
    try {
      await storeDraft(closed, session);
    } catch (error) {
      draftError(error, closed);
      return;
    }
    showToast("Draft saved", undefined, ICONS.draft);
    void reloadMessages();
  }

  /**
   * Queues the message to go out when the undo window ends (or at the Send
   * Later time). Undo cancels it in the outbox and reopens the composer.
   */
  async function send() {
    if (!draft || writing?.status === "writing") return;
    if (!draft.to.trim()) {
      showToast("Add at least one recipient");
      return;
    }
    const session = composerKey;
    const repliedTo = draft.doneAfter ? messages.find((m) => m.id === draft.replyTo && m.state !== "done") : undefined;
    setDraft(null);
    const undoDone = repliedTo ? apply([repliedTo], () => ({ state: "done" })) : undefined;
    // A saved draft goes with it: deleted once it's sent, back in Drafts on Undo.
    const sent = { ...draft, draftId: await savedDraftId(draft, session) };
    let id: number;
    try {
      id = await sendMessage(outgoingDraft(sent, sent.sendLater?.at ?? Date.now() + TOAST_MS));
    } catch (error) {
      undoDone?.();
      openComposer(sent);
      showToast(`Couldn't send: ${String(error)}`, undefined, ICONS.error, { duration: ERROR_TOAST_MS });
      return;
    }
    outbox.current.set(id, sent);
    if (sent.draftId !== undefined) void reloadMessages();
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
    if (sent.draftId !== undefined) void reloadMessages();
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
    const onEvent = (event: AiEvent) => setSummary(m.id, (s) => (s?.run === run ? reduceSummary(s, event) : s));
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

  function changeMinimize(on: boolean) {
    saveMinimize(on);
    setMinimize(on);
  }

  // ---- keyboard ----

  function run(action: ActionId) {
    switch (action) {
      case "next":
        return moveCursor(1);
      case "prev":
        return moveCursor(-1);
      case "open":
        if (selectedId !== null) openListed(selectedId);
        return;
      case "close":
        return closeTopmost();
      case "compose":
        return compose();
      case "search":
        return openSearch();
      case "sidebar":
        return toggleSidebar();
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
      case "pageDown":
      case "pageUp":
      case "top":
      case "bottom":
        return scrollByKey(action);
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

  /** Moves the pane clicked last: the list, or the reader (also when it's all that shows). */
  function scrollByKey(action: "pageDown" | "pageUp" | "top" | "bottom") {
    const list = listScroll.current;
    const reader = readerScroll.current;
    const inReader = reader !== null && (scrollPane.current === "reader" || list === null);
    const el = inReader ? reader : list;
    if (!el) return;
    if (action === "top" || action === "bottom") return scrollToEnd(el, action);
    const dir = action === "pageDown" ? 1 : -1;
    if (inReader) pageReader(el, dir);
    else pageList(el, dir);
  }

  function toggleDone() {
    changeView({ showDone: !view.showDone }, true);
  }

  // ---- Command Center ----

  function actionCommands(targetId?: number): Command[] {
    const target = targetId === undefined ? current : messages.find((x) => x.id === targetId);
    const m = target && asThread(target, threadOf(target));
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
      { id: "go-inbox", label: "Go to Inbox", keywords: "folder", Icon: Inbox, run: () => selectList("inbox") },
      ...SYSTEM_ORDER.map((key) => ({
        id: `go-${key}`,
        label: `Go to ${SYSTEM_LISTS[key].label}`,
        keywords: "folder",
        Icon: SYSTEM_LISTS[key].Icon,
        run: () => selectList(key),
      })),
      ...[...layout.favorites, ...layout.folders, ...layout.recents]
        .filter((key) => !isSystemList(key))
        .flatMap((key) => {
          const info = listInfo(key, folders, layout.smartFolders, (id) => accounts.find((a) => a.id === id)?.label ?? "");
          return info
            ? [{ id: `go-${key}`, label: `Go to ${info.label}`, keywords: "folder label smart", Icon: info.Icon, detail: info.detail, run: () => selectList(key) }]
            : [];
        }),
      {
        id: "new-smart-folder",
        label: "New Smart Folder",
        keywords: "saved search folder",
        Icon: FolderSearch,
        run: newSmartFolder,
      },
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
        id: "agent-settings",
        label: "Agent settings",
        keywords: "ai agents claude code codex hermes mcp cli access",
        Icon: Bot,
        run: () => openSettings("agents"),
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
        run: toggleSidebar,
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

  // Stable, so typing in the composer doesn't re-render every (memoized) row.
  const rows = useLatest<RowActions>({
    open: openListed,
    done: actions.done,
    pin: actions.pin,
    snooze: snoozeMenu,
    aside: actions.aside,
    trash: actions.trash,
    priority: actions.priority,
    read: actions.read,
    more: (m) => setPalette({ kind: "actions", targetId: m.id }),
  });

  const threadActions: ThreadActions = {
    ...actions,
    close: () => setOpenId(null),
    summarize,
    stopSummary,
    deleteSummary,
    summaryCopied: () => showToast("Summary Copied", undefined, ICONS.copy),
    openAiSettings: () => openSettings("ai"),
    reply: (m) => reply(m),
    quickReply,
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

  const sidebarActions: SidebarActions = {
    select: selectList,
    pick,
    layout: setLayout,
    open: () => setSidebarMode("overlay"),
    close: closeSidebar,
    toggleLock,
    resize: (width) => setLayout((l) => ({ ...l, width })),
    sync: checkMail,
    settings: () => openSettings(),
    togglePicker,
    closePicker: () => setPickerOpen(false),
    newSmartFolder,
    createFolder: newFolder,
    renameList,
    deleteList,
    drop: dropMessage,
  };

  const showList = twoColumns || !openMessage;
  const showReader = twoColumns || openMessage !== null;
  const readerLeft = sidebarSpace + (twoColumns ? listWidthUsed : 0);

  return (
    <AccountsContext value={accounts}>
      <div className="flex h-full bg-base-100 text-base-content">
        <Sidebar
          mode={sidebarMode}
          width={sidebarWidth}
          layout={layout}
          folders={folders}
          current={view.folder}
          account={view.account}
          counts={counts}
          syncing={syncing}
          pickerOpen={pickerOpen}
          demo={isDemoMode(accounts)}
          actions={sidebarActions}
        />

        <main className="relative flex min-w-0 flex-1">
          <DragStrip />
          <CaptionButtons />
          {showList && (
            <section
              aria-label="Message list"
              className={`relative flex min-w-0 flex-col pt-8 ${twoColumns ? "shrink-0 border-r border-hairline" : "flex-1"}`}
              style={twoColumns ? { width: listWidthUsed } : undefined}
              onPointerDownCapture={() => (scrollPane.current = "list")}
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
                loading={loading}
                onSaveSearch={(query) => setSavingSearch(query)}
                scrollRef={listScroll}
                older={older && { loading: loadingOlder, onNearEnd: reachBack }}
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
            <section
              aria-label="Conversation"
              className="flex min-w-0 flex-1 flex-col pt-8"
              onPointerDownCapture={() => (scrollPane.current = "reader")}
            >
              {openMessage ? (
                <ThreadView
                  key={openMessage.id}
                  message={asThread(openMessage, conversation)}
                  conversation={conversation}
                  unread={openedUnread}
                  minimize={minimize}
                  singleColumn={!twoColumns}
                  summary={summaries.get(openMessage.id)}
                  aiProvider={aiProvider}
                  actions={threadActions}
                  scrollRef={readerScroll}
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
            saved={draftSaved === draftKey(draft)}
            full={composerFull}
            fullStyle={{ left: readerLeft, right: 0 }}
            onChange={(patch) => setDraft((d) => d && { ...d, ...patch })}
            onToggleFull={() => setComposerFull((f) => !f)}
            onClose={() => void closeComposer()}
            onDiscard={() => void discardComposer()}
            onSend={send}
            onError={(text) => showToast(text, undefined, ICONS.error, { duration: ERROR_TOAST_MS })}
            writing={writing?.composer === composerKey ? writing : null}
            onStopWriting={stopWriting}
            onRetryWriting={() => writing && writeReply(writing.repliedTo, writing.chip, writing.composer)}
            onDismissWriting={() => setWriting(null)}
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
            minimize={minimize}
            onMinimize={changeMinimize}
            onAccountsChanged={() => void reloadAccounts()}
          />
        )}

        {savingSearch !== null && (
          <PromptDialog
            title="New Smart Folder"
            label={`Emails matching “${savingSearch}”, from all your accounts.`}
            placeholder="Smart Folder Name"
            confirmLabel="Save"
            onSubmit={(name) => saveSmartFolder(name, savingSearch)}
            onClose={() => setSavingSearch(null)}
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
  const moves = patch.state !== undefined || patch.folder !== undefined;
  // Filing changes the state; undo puts back the state, and the folder it was filed in.
  if (moves) {
    previous.state = m.state;
    if (m.state === "filed" && m.folder !== undefined) previous.folder = m.folder;
    else delete previous.folder;
  }
  if (moves && m.state === "snoozed") previous.snoozedUntil = m.snoozedUntil;
  return previous;
}

/** An account filter as an account id, or undefined for all of them. */
const accountOf = (account: AccountFilter) => (account === "all" ? undefined : account);

/**
 * Where the list can reach further back on the servers: its accounts' inboxes,
 * or its server folders; null when none has older mail. Search, the Unread
 * cards and New senders don't reach back.
 */
function olderSources(view: ListView, accounts: AccountView[], folders: FolderView[]) {
  if (view.search !== null) return null;
  if (view.folder === "inbox") {
    if ((view.mode === "cards" && !view.sub) || view.sub?.kind === "gatekeeper") return null;
    const ids = accounts
      .filter((a) => a.hasOlder && (view.account === "all" || a.id === view.account))
      .map((a) => a.id);
    return ids.length ? { accounts: ids, folders: [] } : null;
  }
  const ids = listFolderIds(view.folder, folders, accountOf(view.account)).filter(
    (id) => folders.find((f) => f.id === id)?.hasOlder,
  );
  return ids.length ? { accounts: [], folders: ids } : null;
}

/** Every message a list shows: rows and the emails they stand for, card rows past the limit, Gatekeeper senders. */
function listedIds(list: ListModel) {
  return new Set(list.rowOf.keys());
}

/**
 * `fns` with an identity that lasts across renders, each function calling
 * the latest render's version, so memoized children skip renders that would
 * only hand them new closures.
 */
function useLatest<T extends object>(fns: T): T {
  const latest = useRef(fns);
  latest.current = fns;
  const [stable] = useState(() => {
    const call =
      (key: string) =>
      (...args: unknown[]) =>
        (latest.current as Record<string, (...args: unknown[]) => unknown>)[key](...args);
    return Object.fromEntries(Object.keys(fns).map((key) => [key, call(key)])) as T;
  });
  return stable;
}
