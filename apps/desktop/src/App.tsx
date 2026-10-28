import {
  AlignLeft,
  ArrowDownLeft,
  AppWindow,
  Ban,
  Bell,
  BellOff,
  Bot,
  Check,
  CheckCheck,
  Circle,
  CircleAlert,
  CircleCheck,
  Clock,
  Copy,
  Download,
  FileText,
  Folder,
  FolderInput,
  FolderPlus,
  FolderSearch,
  Forward,
  Group,
  House,
  Inbox,
  LayoutGrid,
  List,
  ListChecks,
  Mail,
  MailOpen,
  MailX,
  PanelLeft,
  Pen,
  Pin,
  RefreshCw,
  Reply,
  ReplyAll,
  Rows3,
  Scaling,
  Search,
  Send,
  Settings as SettingsIcon,
  ShieldAlert,
  ShieldCheck,
  Signature,
  Sparkles,
  Tags,
  Text,
  TextSearch,
  ThumbsDown,
  ThumbsUp,
  Trash2,
  UserPlus,
  Zap,
  ZoomIn,
  ZoomOut,
} from "lucide-react";
import { useEffect, useLayoutEffect, useMemo, useRef, useState, type KeyboardEvent as ReactKeyboardEvent } from "react";
import {
  QUICK_REPLIES,
  cancelCompose,
  cancelSummary,
  composeWithAi,
  getAiSettings,
  presetLabel,
  summarizeMessage,
  type AiEvent,
  type ComposeAction,
  type QuickReply,
  type Reference,
  type SummaryStyle,
} from "./ai";
import {
  cancelSend,
  createFolder,
  decideSenders,
  deleteFolder,
  getGatekeeperMode,
  deleteForever,
  discardDraft,
  emptyFolders,
  focusReminder,
  getPrefs,
  onWindowFocus,
  otherWindowFocused,
  getPrioritySenders,
  listAccounts,
  listFolders,
  loadOlder,
  messageChanges,
  onAccountsChanged,
  onAgentActivity,
  onAgentSending,
  onFoldersChanged,
  onMailboxChanged,
  onOpenCalendar,
  onOpenMessage,
  onOpenSettings,
  onSendStatus,
  onSyncStatus,
  openAttachment,
  finishReminder,
  listReminders,
  listScheduled,
  getInlineImages,
  onMailEdited,
  onInviteAnswered,
  onMailto,
  openThreadWindow,
  closeWindow,
  editingDraft,
  draftOpenElsewhere,
  onSearchInMain,
  onSettingsChanged,
  onThreadThen,
  onWindowClose,
  onQuitting,
  searchInMain,
  openDraft,
  patched,
  prioritizeSenders,
  refreshFolders,
  renameFolder,
  saveAllAttachments,
  saveAttachment,
  rescheduleSend,
  saveDraft,
  saveGatekeeperMode,
  sendMessage,
  openExternal,
  prefetchAttachments,
  saveEml,
  setBadge,
  setAutoLabel,
  setMuted,
  setReminder,
  setSenderCategory,
  syncFolders,
  syncNow,
  takeMailto,
  trustCertificate,
  undoPrioritize,
  unscheduleSend,
  unsubscribe,
  updateMessage,
  watchFolders,
} from "./api";
import BulkPanel, { type BulkActions, type Selection } from "./components/BulkPanel";
import PlannedList, { type ReminderActions, type ScheduledActions } from "./components/PlannedList";
import type { AiRequest } from "./components/AiMenu";
import CalendarScreen from "./components/CalendarScreen";
import CommandCenter, { type Command } from "./components/CommandCenter";
import HomeScreen from "./components/HomeScreen";
import Composer, {
  draftHasContent,
  draftKey,
  emptyDraft,
  outgoingDraft,
  savedDraft,
  type Draft,
  type Writing,
} from "./components/Composer";
import { CertificateDialog, ConfirmDialog, DateTimeDialog, PromptDialog } from "./components/Dialog";
import MessageList, { Splitter, type GatekeeperActions } from "./components/MessageList";
import type { CardSettings } from "./components/SmartInboxCard";
import type { RowActions } from "./components/MessageRow";
import Settings, { type SettingsPage } from "./components/Settings";
import type { TemplateDraft } from "./components/TemplatesPage";
import { useUpdates } from "./lib/updates";
import Sidebar, { type SidebarActions, type SidebarMode } from "./components/Sidebar";
import { reduceSummary, startSummary, type SummaryState } from "./components/SummaryCard";
import ThreadView, { type ThreadActions } from "./components/ThreadView";
import UndoToast, { TOAST_MS, type ToastData, type ToastIcon } from "./components/UndoToast";
import { CaptionButtons, DragStrip } from "./components/WindowChrome";
import AssistantPanel, {
  ASSISTANT_MIN_WIDTH,
  ASSISTANT_SPACE,
  AssistantPill,
  assistantSuggestions,
  type Ask,
} from "./components/AssistantPanel";
import type { DraftCard, EmailRef } from "./assistant";
import { AccountsContext, isDemoMode, isFromYou } from "./lib/accounts";
import { joinAddresses, parseEntry, replyRecipients, splitAddresses } from "./lib/addresses";
import { badgeIcon } from "./lib/badge";
import { clearContent, forgetContent, loadContent } from "./lib/content";
import { remoteImagesAllowed } from "./lib/remoteImages";
import {
  asThread,
  conversationOf,
  indexConversations,
  loadMinimize,
  movesWith,
  saveMinimize,
} from "./lib/conversation";
import { isDraft } from "./lib/drafts";
import { draftLeft, goneNotice, markTrying, stuckNotice } from "./lib/outbox";
import {
  activate as activateHome,
  clearAll as clearHomeFeed,
  deactivate as deactivateHome,
  dismiss as dismissFromHome,
  endSession,
  feed as homeFeed,
  homeCounts,
  homeShows,
  launch as launchHome,
  loadHomeSession,
  loadHomeSettings,
  newSince,
  reconfigure as reconfigureHome,
  saveHomeSession,
  saveHomeSettings,
  startSession,
  tick as tickHome,
  type HomeSession,
  type HomeSettings,
  type PreviewFilter,
} from "./lib/homeScreen";
import { decidableDomain, domain, forSenders, predict } from "./lib/gatekeeper";
import { coalesce, mailboxCopy, staleContent } from "./lib/mailboxCopy";
import { loadPriorityLines, savePriorityLines } from "./lib/priority";
import {
  formatFullDate,
  formatLater,
  laterPresets,
  laterTime,
  plural,
  presetDetail,
  presetOf,
  type LaterPreset,
} from "./lib/format";
import { applyTheme, loadTheme, saveTheme, type ThemePref } from "./lib/theme";
import {
  CARDS,
  INBOX_MODES,
  badgeCount,
  buildList,
  inboxMessages,
  loadCardPrefs,
  loadGrouped,
  loadInboxMode,
  saveCardPrefs,
  saveGrouped,
  saveInboxMode,
  sidebarCounts,
  type AccountFilter,
  type CardKey,
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
  folderPath,
  hasAccountRows,
  inSidebar,
  isPlannedList,
  isSystemList,
  listFolderIds,
  listInfo,
  loadLayout,
  loadSeen,
  offerAutoLabels,
  pruneLayout,
  removeSmartFolder,
  renameSmartFolder,
  saveLayout,
  saveSeen,
  shownKeys,
  sidebarKeys,
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
import { setLinkHandlers } from "./lib/links";
import { THREAD_THEN, THREAD_WINDOW, loadPopOut, savePopOut, summaryStyle } from "./lib/threadWindow";
import { messageMarkdown, type MarkdownHeader } from "./lib/markdown";
import { printEmails } from "./lib/print";
import { parseMailto } from "./lib/mailto";
import { dropPlaceholders, hasPlaceholders, htmlText, replaceText, resendHtml, textToHtml } from "./lib/richText";
import { pageList, pageReader, scrollToEnd } from "./lib/scroll";
import { forgetSearch, loadRecentSearches, rememberSearch, useSearchHits, useServerSearch } from "./lib/search";
import { applyZoom, loadZoom, stepZoom } from "./lib/zoom";
import { autoDraftReply, chooseAutoDraftOption, onAutoDraftWriting } from "./automation";
import { AUTO_LABEL_ORDER } from "./lib/autoLabels";
import type {
  AccountId,
  AccountView,
  AgentSendingEvent,
  AutoLabel,
  Category,
  FolderView,
  GatekeeperMode,
  Message,
  MessagePatch,
  Prefs,
  QueuedChange,
  ReminderView,
  ScheduledView,
  SendStatusEvent,
  SenderChange,
  SenderDecision,
  SyncStatusEvent,
  Untrusted,
} from "./types";

/** `targetId` points the actions at one row (its ⋯) rather than the current message. */
type Palette =
  | { kind: "actions"; targetId?: number }
  | { kind: "snooze"; targetIds: number[] }
  | { kind: "move"; targetIds: number[] };

/** Folders moved to lately, newest first, per account: the Move picker offers them first. */
const MOVE_RECENT_KEY = "spark-rs.move-recent";

function loadMoveRecent(): Record<string, number[]> {
  try {
    const saved = JSON.parse(localStorage.getItem(MOVE_RECENT_KEY) ?? "{}");
    return typeof saved === "object" && saved ? saved : {};
  } catch {
    return {};
  }
}

function rememberMove(account: AccountId, folder: number) {
  const saved = loadMoveRecent();
  const list = [folder, ...(saved[String(account)] ?? []).filter((id) => id !== folder)].slice(0, 5);
  localStorage.setItem(MOVE_RECENT_KEY, JSON.stringify({ ...saved, [String(account)]: list }));
}

const CATEGORY_LABELS: Record<Category, string> = {
  people: "People",
  notifications: "Notifications",
  newsletters: "Newsletters",
};

/** A question before something that can't be undone, or a big batch (`ConfirmDialog`). */
interface Confirm {
  title: string;
  message: string;
  confirmLabel: string;
  cancelLabel?: string;
  /** The composer session it's about: it closes when that one does. */
  session?: number;
  danger?: boolean;
  run: () => Promise<void> | void;
}

/** Keys that move in the list, which a conversation's own window doesn't have. */
const LIST_ACTIONS = new Set<ActionId>([
  "next",
  "prev",
  "open",
  "sidebar",
  "search",
  "modeFocused",
  "modeCards",
  "modeSimple",
  "selectAll",
  "selectNext",
  "selectPrev",
  "toggleDone",
  "acceptAll",
  "blockAll",
]);

/** Batch actions on more emails than this ask first, as Spark's do. */
const HEAVY_BATCH = 100;

/** A day and time to ask for (`DateTimeDialog`), and what to do with it. */
interface PickTime {
  title: string;
  confirmLabel: string;
  initial: number;
  run: (at: number) => Promise<void> | void;
}

type AppPrefs = Pick<
  Prefs,
  | "badge"
  | "badgeExcluded"
  | "undoSendSeconds"
  | "sendAndDone"
  | "replyAttachments"
  | "autoCc"
  | "autoBcc"
  | "autoDownloadAttachments"
>;

/** Until the saved settings load: the Rust side's defaults. */
const DEFAULT_APP_PREFS: AppPrefs = {
  badge: "new",
  badgeExcluded: [],
  undoSendSeconds: 5,
  sendAndDone: false,
  replyAttachments: "never",
  autoCc: {},
  autoBcc: {},
  autoDownloadAttachments: true,
};

/** What AI does in a composer (by `composerKey`), and its body (text and HTML) before AI started. */
type ComposerWriting = Writing & { composer: number; request: AiRequest; before: Pick<Draft, "body" | "bodyHtml"> };

/** `body` around `range`, and the trimmed text in it; the range's own outer whitespace stays outside. */
function splitAround(body: string, [start, end]: [number, number]): [string, string, string] {
  const picked = body.slice(start, end);
  const lead = picked.length - picked.trimStart().length;
  const trail = picked.length - picked.trimEnd().length;
  return [body.slice(0, start + lead), picked.trim(), body.slice(end - trail)];
}

const INITIAL_VIEW: ListView = {
  folder: "inbox",
  account: "all",
  mode: "cards",
  sub: null,
  search: null,
  showDone: false,
};

/** Toast icons per action, in the action's color. */
/** Spark's Done, as a patch. */
const DONE: MessagePatch = { state: "done" };

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
  spam: { Icon: ShieldAlert, className: "text-error" },
  mute: { Icon: BellOff, className: "text-text-2" },
  unsubscribe: { Icon: MailX, className: "text-primary" },
  zoom: { Icon: ZoomIn, className: "text-primary" },
  agent: { Icon: Bot, className: "text-primary" },
} satisfies Record<string, ToastIcon>;

/** How often the Home Screen's timers run: the inactivity timeout, focus times, a new day. */
const HOME_TICK_MS = 15_000;
/** Spark waits this long after a window loses focus before calling spark-rs inactive. */
const BLUR_SETTLE_MS = 150;
/** On the Home Screen, with no email open over it, only these shortcuts act. */
/** The floating composer goes beside the assistant's panel only if it can be this wide there. */
const COMPOSER_MIN_BESIDE = 440;
const HOME_KEYS = new Set<ActionId>(["compose", "search", "sidebar", "command", "settings", "close", "assistant"]);
/** Shortcuts that move around the list, which the Home Screen hides. */
const LIST_KEYS = new Set<ActionId>([
  "next",
  "prev",
  "open",
  "toggleDone",
  "modeFocused",
  "modeCards",
  "modeSimple",
  "selectAll",
  "selectNext",
  "selectPrev",
  "acceptAll",
  "blockAll",
]);

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
  const [view, setView] = useState<ListView>(() => ({ ...INITIAL_VIEW, mode: loadInboxMode("all") }));
  /**
   * Messages read during this visit to the list. They keep their place until the list changes, as
   * in Spark: their Smart Inbox card, an `is:unread` search or smart folder, the Home Screen feed.
   */
  const [sticky, setSticky] = useState<ReadonlySet<number>>(new Set());
  /** The list cursor. In split view it follows the open thread. */
  const [selectedId, setSelectedId] = useState<number | null>(null);
  const [openId, setOpenId] = useState<number | null>(null);
  /** In a conversation's own window: it's open, once the mail's loaded (and it's there). */
  const threadOpened = useRef(false);
  const [threadLoaded, setThreadLoaded] = useState(false);
  /** Which emails of the open conversation were unread when it opened: reading starts there. */
  const [openedUnread, setOpenedUnread] = useState<ReadonlySet<number>>(new Set());
  /** The stored messages as last read, brought up to date by `reloadMessages` from what changed since. */
  const [mailbox] = useState(mailboxCopy);
  const shownMessages = useRef(messages);
  shownMessages.current = messages;
  /** One read at a time; asked meanwhile, it reads once more after. Resolves to the list as shown. */
  const [refreshMessages] = useState(() =>
    coalesce(async () => {
      const changes = await messageChanges(mailbox.mark);
      // Bodies read before a sync filled in their attachments or invite are read again.
      forgetContent(staleContent(changes, mailbox));
      const next = mailbox.apply(changes, shownMessages.current);
      setMessages(next);
      setOpenId((id) => (id !== null && mailbox.get(id) ? id : null));
      setSelectedId((id) => (id !== null && mailbox.get(id) ? id : null));
      return next;
    }),
  );
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
  /** The AI Assistant's panel at the right, and a question for it from the pill. */
  const [assistantOpen, setAssistantOpen] = useState(false);
  const [assistantAsk, setAssistantAsk] = useState<Ask | null>(null);
  /** `draftKey` of the composer's draft as it is in Drafts (opened or saved), if it is. */
  const [draftSaved, setDraftSaved] = useState<string | null>(null);
  /** The saved draft each composer session (by `composerKey`) saved to. */
  const draftIds = useRef(new Map<number, number>());
  /** Saves run one after another, so saving again updates the same draft. */
  const draftSaves = useRef<Promise<unknown>>(Promise.resolve());
  /** Drafts deleted while their Undo is up: what deletes them, by timer. */
  const pendingDiscards = useRef(new Map<number, () => Promise<void>>());
  /** The composer as it stands, for callbacks that outlive a render (undo, autosave). */
  const composer = useRef({ draft, saved: draftSaved, key: composerKey });
  composer.current = { draft, saved: draftSaved, key: composerKey };
  const [writing, setWriting] = useState<ComposerWriting | null>(null);
  /** Counts AI runs in the composer, so a stopped or replaced run's late events are dropped. */
  const writingRun = useRef(0);
  /** Drafts deleted while their Undo toast is up: hidden now, deleted when it's gone. */
  const [discarding, setDiscarding] = useState<ReadonlySet<number>>(new Set());
  /** Emails a reply is being written for by "Auto-Draft a reply". */
  const [drafting, setDrafting] = useState<ReadonlySet<number>>(new Set());
  const [palette, setPalette] = useState<Palette | null>(null);
  /** List rows (conversations) selected for a batch action. */
  const [picked, setPicked] = useState<ReadonlySet<number>>(new Set());
  /** Where Shift-click and Shift+arrow selections reach from. */
  const pickAnchor = useRef<number | null>(null);
  const [confirm, setConfirm] = useState<Confirm | null>(null);
  /** A day and time being picked: a custom snooze, a new send time, a reminder's. */
  const [pickTime, setPickTime] = useState<PickTime | null>(null);
  /** Send Later's queue and the reminders, for their lists and the sidebar. */
  const [scheduled, setScheduled] = useState<ScheduledView[]>([]);
  const [reminders, setReminders] = useState<ReminderView[]>([]);
  const [recentSearches, setRecentSearches] = useState(loadRecentSearches);
  /** Cards settings, and the senders and domains grouped into one row. */
  const [cardPrefs, setCardPrefs] = useState(loadCardPrefs);
  const [grouped, setGrouped] = useState(loadGrouped);
  /** A server certificate to ask about, from a sync or send that failed on it. */
  const [certificate, setCertificate] = useState<{ account: AccountId; certificate: Untrusted } | null>(null);
  /** The settings App acts on: the app icon badge, and how the composer starts and sends. */
  const [appPrefs, setAppPrefs] = useState<AppPrefs>(DEFAULT_APP_PREFS);
  /** How long Undo can stop a message after Send. */
  const undoMs = appPrefs.undoSendSeconds * 1000;
  /** +AI summaries by message id, kept while the app runs. */
  const [summaries, setSummaries] = useState<ReadonlyMap<number, SummaryState>>(new Map());
  const summaryRun = useRef(0);
  const [settingsPage, setSettingsPage] = useState<SettingsPage | null>(null);
  /** Goes up each time Find in Conversation is asked for. */
  const [findRequest, setFindRequest] = useState(0);
  /** A template made from the open draft, for Settings → Templates to start with. */
  const [newTemplate, setNewTemplate] = useState<TemplateDraft | null>(null);
  const [gatekeeperMode, setGatekeeperMode] = useState<GatekeeperMode>("beforeInbox");
  /** The calendar shows in place of the list and the reader. */
  const [calendarOpen, setCalendarOpen] = useState(false);
  /** A day and event the calendar was asked to show, by a calendar alert or the meeting pop-up. */
  const [calendarFocus, setCalendarFocus] = useState<{ at: number; event?: number } | undefined>();
  /** Settings → Home Screen, and the Home Screen's Preview Emails filter. */
  const [homeSettings, setHomeSettings] = useState<HomeSettings>(loadHomeSettings);
  /** Whether the user is in a session (the Inbox) or on the Home Screen, from launch on. */
  const [homeSession, setHomeSession] = useState<HomeSession>(() =>
    launchHome(loadHomeSession(), loadHomeSettings(), Date.now()),
  );
  /** The session as last changed, for listeners and timers that outlive a render. */
  const homeRef = useRef(homeSession);
  const lastSettingsPage = useRef<SettingsPage>("accounts");
  const [theme, setTheme] = useState<ThemePref>(loadTheme);
  /** Settings → Appearance → Minimize old emails. */
  const [minimize, setMinimize] = useState(loadMinimize);
  /** Settings → Appearance: in the one-column layout, conversations open in windows of their own. */
  const [alwaysPopOut, setAlwaysPopOut] = useState(loadPopOut);
  /** Settings → Priority: rows the Priority card shows; 0 shows them all. */
  const [priorityLines, setPriorityLines] = useState(loadPriorityLines);
  /** The Priority senders, as stored, whose mail is all Priority. */
  const [prioritySenders, setPrioritySenders] = useState<string[]>([]);
  /** The configured provider's name, for "Anthropic declined…". */
  const [aiProvider, setAiProvider] = useState("The AI provider");
  const [toast, setToast] = useState<ToastData | null>(null);
  const toastNow = useRef<ToastData | null>(null);
  const toastTimer = useRef<number | undefined>(undefined);
  const updates = useUpdates((build) => showToast(`Updated to build ${build}`));
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
  // The assistant's panel takes its room from the mail beside it.
  const assistantSpace = assistantOpen && THREAD_WINDOW === null ? ASSISTANT_SPACE : 0;
  const container = windowWidth - sidebarSpace - assistantSpace;
  const listWidthUsed = effectiveListWidth(listWidth, container);
  const wide = useTwoColumns(container, listWidthUsed);
  // A conversation's own window has no list beside it: finishing one closes it rather than
  // opening the next.
  const twoColumns = THREAD_WINDOW === null && wide;

  const hits = useSearchHits([view.search ?? "", ...layout.smartFolders.map((f) => f.query)], messages);
  const searchingServers = useServerSearch(
    isDemoMode(accounts) ? null : view.search,
    reloadMessages,
    view.account === "all" ? undefined : view.account,
  );
  const ctx: ListContext = useMemo(
    () => ({
      accounts,
      folders,
      smartFolders: layout.smartFolders,
      seen,
      hits,
      gatekeeper: gatekeeperMode,
      priorityLines,
      cards: cardPrefs,
      grouped,
    }),
    [accounts, folders, layout.smartFolders, seen, hits, gatekeeperMode, priorityLines, cardPrefs, grouped],
  );
  const listed = useMemo(
    () => (discarding.size ? messages.filter((m) => !discarding.has(m.id)) : messages),
    [messages, discarding],
  );
  const model = useMemo(() => buildList(listed, view, sticky, ctx), [listed, view, sticky, ctx]);
  const counts = useMemo(() => {
    const found = sidebarCounts(listed, shownKeys(layout, folders), ctx);
    found.lists.scheduled = scheduled.length;
    found.lists.reminders = reminders.filter((r) => !r.due).length;
    return found;
  }, [listed, layout, folders, ctx, scheduled, reminders]);
  /** Planned items of the account the list is narrowed to. */
  const inAccount = <T extends { account: AccountId }>(items: T[]) =>
    view.account === "all" ? items : items.filter((x) => x.account === view.account);
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
  /** The Home Screen shows in place of the list and the reader; the calendar covers it. */
  const homeShown = THREAD_WINDOW === null && homeShows(homeSettings, homeSession) && !calendarOpen;
  /** What message shortcuts and commands act on. The Home Screen hides the list's cursor. */
  const current = openMessage ?? (homeShown ? null : selected);
  const searching = view.search !== null;
  const syncing = accounts.some((a) => a.sync.status === "syncing");
  const demo = isDemoMode(accounts);
  const byId = useMemo(() => new Map(messages.map((m) => [m.id, m])), [messages]);
  /** The selected rows still listed, in list order. */
  const pickedRows = useMemo(() => model.order.filter((id) => picked.has(id)), [model.order, picked]);
  /** The selected conversations; a selected group row stands for all of its. */
  const pickedMessages = pickedRows.flatMap((id) => {
    const group = model.groups.get(id);
    return group ? group.items.flatMap((m) => byId.get(m.id) ?? []) : (byId.get(id) ?? []);
  });
  /** A group row under the cursor, with nothing open: single keys act on all of its conversations. */
  const cursorGroup = openId === null && selectedId !== null ? model.groups.get(selectedId) : undefined;
  /** What batch actions and single keys work on: the selection, else the group at the cursor. */
  const batchRows = pickedRows.length
    ? pickedMessages
    : (cursorGroup?.items.flatMap((m) => byId.get(m.id) ?? []) ?? []);
  const badge = useMemo(
    () => (demo ? 0 : badgeCount(listed, appPrefs.badge, appPrefs.badgeExcluded, gatekeeperMode)),
    [demo, listed, appPrefs, gatekeeperMode],
  );

  // ---- Home Screen ----

  /** The Inbox's mail, New Senders left out: what the Home Screen counts and previews. */
  const inboxMail = useMemo(
    () => (homeShown ? inboxMessages(listed, "all", false, gatekeeperMode).mail : []),
    [homeShown, listed, gatekeeperMode],
  );
  const homeNew = useMemo(() => homeCounts(inboxMail, newSince(homeSession)), [inboxMail, homeSession]);
  const homeItems = useMemo(
    () => homeFeed(inboxMail, homeSession, homeSettings.filter, sticky),
    [inboxMail, homeSession, homeSettings.filter, sticky],
  );
  /** Writing or reading an email holds off the inactivity timeout. Split view always shows one, so only one column counts. */
  const homePaused = draft !== null || (openMessage !== null && !twoColumns);
  /** What the focus listeners and the timer need from the latest render. */
  const homeLatest = useRef({ settings: homeSettings, paused: homePaused, listed, gatekeeperMode });
  homeLatest.current = { settings: homeSettings, paused: homePaused, listed, gatekeeperMode };

  /** Changes the session, and saves it. Only its last-seen time changing doesn't re-render. */
  function updateHome(change: (s: HomeSession) => HomeSession) {
    const before = homeRef.current;
    const after = change(before);
    if (after === before) return;
    homeRef.current = after;
    saveHomeSession(after);
    if (JSON.stringify({ ...after, lastSeen: 0 }) !== JSON.stringify({ ...before, lastSeen: 0 })) setHomeSession(after);
  }
  const homeUpdates = useRef(updateHome);
  homeUpdates.current = updateHome;

  useEffect(() => {
    if (THREAD_WINDOW === null) saveHomeSession(homeRef.current);
  }, []);

  // spark-rs is active while one of its windows has focus (Spark's
  // SparkFocusMonitor): a change counts once it still holds 150 ms later, so
  // a click into a message's frame doesn't. The backend reports focus
  // changes of every window, the meeting pop-up's too. While inactive, the
  // timers run the timeout.
  useEffect(() => {
    // The main window keeps the Home Screen's session; a conversation's own window has none.
    if (THREAD_WINDOW !== null) return;
    const here = () => document.visibilityState === "visible" && document.hasFocus();
    let active = here();
    if (!active) homeUpdates.current((s) => deactivateHome(s, Date.now()));
    let settle: number | undefined;
    let disposed = false;
    /** Counts focus changes: an answer only applies if none came after the change it was asked for. */
    let checks = 0;
    /** A focus change is being looked into: the timers wait for the answer. */
    let checking = false;
    const evaluate = async (check: number) => {
      const now = here() || (await otherWindowFocused());
      if (disposed || check !== checks) return;
      checking = false;
      if (now === active) return;
      active = now;
      const { settings, paused } = homeLatest.current;
      homeUpdates.current((s) => (now ? activateHome(s, settings, Date.now(), paused) : deactivateHome(s, Date.now())));
    };
    const check = () => {
      if (disposed) return;
      const id = ++checks;
      checking = true;
      window.clearTimeout(settle);
      settle = window.setTimeout(() => void evaluate(id), BLUR_SETTLE_MS);
    };
    const run = () => {
      if (checking) return;
      const { settings, paused, listed, gatekeeperMode } = homeLatest.current;
      let remind = false;
      homeUpdates.current((s) => {
        const result = tickHome(s, settings, Date.now(), { active, paused });
        remind = result.remind;
        return result.session;
      });
      if (!remind) return;
      const mail = inboxMessages(listed, "all", false, gatekeeperMode).mail;
      const n = homeCounts(mail, newSince(homeRef.current));
      void focusReminder(n.people + n.notifications + n.newsletters).catch(() => {});
    };
    window.addEventListener("focus", check);
    window.addEventListener("blur", check);
    document.addEventListener("visibilitychange", check);
    // Once the backend's focus reports are heard, and right away: the pop-up may have focus already.
    const stopWindowFocus = onWindowFocus(check, check);
    check();
    const timer = window.setInterval(run, HOME_TICK_MS);
    return () => {
      disposed = true;
      window.clearTimeout(settle);
      window.clearInterval(timer);
      window.removeEventListener("focus", check);
      window.removeEventListener("blur", check);
      document.removeEventListener("visibilitychange", check);
      stopWindowFocus();
    };
  }, []);

  // Arriving on the Home Screen closes what was open in the Inbox, and drops its cursor.
  useEffect(() => {
    if (!homeShown) return;
    setOpenId(null);
    setSelectedId(null);
    setPicked(new Set());
  }, [homeShown]);

  // The Home Screen and the Inbox are separate visits: what was read in one goes where it belongs
  // in the other. Before paint, so the first frame already shows it there.
  useLayoutEffect(() => setSticky((s) => (s.size ? new Set() : s)), [homeShown]);

  /** The user is in the Inbox now: a list, a search or a notification opened. */
  function startHomeSession() {
    if (homeRef.current.state !== "sessionActive") updateHome((s) => startSession(s, Date.now()));
  }

  /** The sidebar's Home Screen: the session ends. */
  function goHome() {
    setCalendarOpen(false);
    setSettingsPage(null);
    setPalette(null);
    closeSidebar();
    updateHome((s) => endSession(s, homeSettings, Date.now()));
  }

  /** Check Inbox and Open Inbox. */
  function checkInbox() {
    startHomeSession();
    selectList("inbox", "all");
  }

  /** New emails only: the Inbox narrowed to what came while away. */
  function showNewEmails() {
    const since = newSince(homeRef.current);
    startHomeSession();
    setCalendarOpen(false);
    changeView({ folder: "inbox", account: "all", mode: loadInboxMode("all"), sub: { kind: "new", since }, search: null });
  }

  function changeHomeSettings(next: HomeSettings) {
    saveHomeSettings(next);
    updateHome((s) => reconfigureHome(s, homeSettings, next, Date.now()));
    setHomeSettings(next);
  }

  const changeHomeFilter = (filter: PreviewFilter) => changeHomeSettings({ ...homeSettings, filter });

  useEffect(() => {
    void reloadFolders();
    void getGatekeeperMode().then(setGatekeeperMode, () => {});
    void reloadPrioritySenders();
    void Promise.all([listAccounts(), refreshMessages()]).then(
      ([loadedAccountList, loaded]) => {
        loadedAccounts.current = accountSet(loadedAccountList);
        setAccounts(loadedAccountList);
        const first = buildList(loaded, { ...INITIAL_VIEW, mode: loadInboxMode("all") }, new Set()).order[0];
        // Not on the Home Screen: opening an email there would show it over it, and read it.
        const home = homeShows(homeSettings, homeRef.current);
        if (THREAD_WINDOW !== null) {
          // This window's conversation, which it closes with.
          const own = loaded.find((x) => x.id === THREAD_WINDOW);
          threadOpened.current = own !== undefined;
          setThreadLoaded(true);
          if (own) {
            openThread(own.id, loaded);
            // Opened to summarize it, too.
            const style = summaryStyle(THREAD_THEN);
            if (style) summarize(own, style);
          }
        } else if (first !== undefined && twoColumns && !home) {
          openThread(first, loaded);
        }
      },
      (error) => showToast(`Couldn't load your mail: ${String(error)}`, undefined, ICONS.error),
    );
  }, []);

  // Backend events. They call through a ref so the listeners, registered once, see current state.
  const events = useRef({
    reloadMessages,
    reloadPlanned,
    reloadAccounts,
    reloadFolders,
    syncStatus,
    sendStatus,
    openFromNotification,
    agentSending,
    openCalendarAt,
    openSettings,
    otherWindowEdited,
    searchFromWindow,
    summarizeOpen,
    reloadAppPrefs,
  });
  events.current = {
    reloadMessages,
    reloadPlanned,
    reloadAccounts,
    reloadFolders,
    syncStatus,
    sendStatus,
    openFromNotification,
    agentSending,
    openCalendarAt,
    openSettings,
    otherWindowEdited,
    searchFromWindow,
    summarizeOpen,
    reloadAppPrefs,
  };
  useEffect(() => {
    const stops = [
      onMailboxChanged(() => {
        void events.current.reloadMessages();
        events.current.reloadPlanned();
      }),
      onFoldersChanged(() => void events.current.reloadFolders()),
      onAccountsChanged(() => void events.current.reloadAccounts()),
      onSyncStatus((e) => events.current.syncStatus(e)),
      onSendStatus((e) => events.current.sendStatus(e)),
      onMailEdited(() => events.current.otherWindowEdited()),
      // Its card, read again, shows the answer.
      onInviteAnswered((id) => forgetContent([id])),
      // Settings saved in another window (an automatic Bcc, the Undo Send delay, …).
      onSettingsChanged(() => events.current.reloadAppPrefs()),
      // A conversation's own window, asked to summarize it.
      ...(THREAD_WINDOW !== null ? [onThreadThen((style) => events.current.summarizeOpen(style))] : []),
      // What clicks on notifications and the meeting pop-up ask for is the main window's.
      ...(THREAD_WINDOW === null
        ? [
            onOpenMessage((id) => events.current.openFromNotification(id)),
            onAgentActivity((e) => showToast(e.text, undefined, ICONS.agent)),
            onAgentSending((e) => events.current.agentSending(e)),
            onOpenCalendar((focus) => events.current.openCalendarAt(focus)),
            onOpenSettings((page) => events.current.openSettings(page)),
            onSearchInMain((query) => events.current.searchFromWindow(query)),
          ]
        : []),
    ];
    return () => stops.forEach((stop) => stop());
  }, []);

  // Also when the list comes back from behind the Home Screen with a search open.
  useEffect(() => {
    if (searching && !homeShown) searchRef.current?.focus();
  }, [searching, homeShown]);

  // The sidebar's layout is the main window's: a conversation's own window has none to save.
  useEffect(() => {
    if (THREAD_WINDOW === null) saveLayout(layout);
  }, [layout]);

  // An Auto-Label's list joins the sidebar once mail has it, as Spark adds them.
  const labelsInUse = AUTO_LABEL_ORDER.filter((l) => messages.some((m) => m.autoLabel === l)).join(",");
  useEffect(() => {
    if (THREAD_WINDOW !== null || !labelsInUse) return;
    const labels = labelsInUse.split(",") as AutoLabel[];
    setLayout((l) => (labels.every((x) => l.autoOffered.includes(x)) ? l : offerAutoLabels(l, labels)));
  }, [labelsInUse]);

  // "Auto-Draft a reply" from another window: this one shows it under way too.
  useEffect(
    () =>
      onAutoDraftWriting((id, writing) =>
        setDrafting((s) => (writing ? new Set([...s, id]) : new Set([...s].filter((x) => x !== id)))),
      ),
    [],
  );
  useEffect(() => saveCardPrefs(cardPrefs), [cardPrefs]);
  useEffect(() => saveGrouped(grouped), [grouped]);
  useEffect(() => saveSeen(seen), [seen]);

  // The backend keeps the sidebar's folders and the open one in sync.
  const watched = watchedFolderIds(layout, folders, view.folder).join(",");
  useEffect(() => {
    if (THREAD_WINDOW !== null) return;
    void watchFolders(watched ? watched.split(",").map(Number) : []).catch(() => {});
  }, [watched]);

  // As in Spark, the window coming to the front fetches the open list.
  // The backend checks the inboxes.
  const open = listFolderIds(view.folder, folders, accountOf(view.account)).join(",");
  useEffect(() => {
    if (!open || THREAD_WINDOW !== null) return;
    const fetchOpen = () => void syncFolders(open.split(",").map(Number)).catch(() => {});
    window.addEventListener("focus", fetchOpen);
    return () => window.removeEventListener("focus", fetchOpen);
  }, [open]);

  useEffect(() => {
    if (settingsPage !== null) return;
    reloadAppPrefs();
    reloadPlanned();
  }, [settingsPage]);

  /** The settings this window acts on: the AI provider's name, and how the composer starts and sends. */
  function reloadAppPrefs() {
    void getAiSettings().then(
      (view) => view && setAiProvider(presetLabel(view.preset)),
      () => {},
    );
    void getPrefs().then(
      ({ badge, badgeExcluded, undoSendSeconds, sendAndDone, replyAttachments, autoCc, autoBcc, autoDownloadAttachments }) =>
        setAppPrefs({
          badge,
          badgeExcluded,
          undoSendSeconds,
          sendAndDone,
          replyAttachments,
          autoCc,
          autoBcc,
          autoDownloadAttachments,
        }),
      () => {},
    );
  }

  useEffect(() => {
    if (THREAD_WINDOW === null) void setBadge(badge, badge > 0 ? badgeIcon(badge) : null).catch(() => {});
  }, [badge]);

  // Email links (mailto:) open a new email: the one spark-rs was started with, and any after,
  // once the accounts to send from are known.
  const latestCompose = useRef(compose);
  latestCompose.current = compose;
  const latestSend = useRef(send);
  latestSend.current = send;

  // A conversation's own window closes when it's done with it: closed, or another one open
  // in its place (Done takes the reader to the next email). Its email's conversation is
  // looked at again each time, as a sync can join two conversations into one.
  const ownMessage = THREAD_WINDOW === null ? undefined : byId.get(THREAD_WINDOW);
  const ownConversation = ownMessage ? (ownMessage.conversation ?? ownMessage.id) : null;
  // While its Undo is up (Send and Done, Done), it waits, still showing the conversation.
  useEffect(() => {
    if (THREAD_WINDOW === null || !threadOpened.current) return;
    const here = openMessage ? (openMessage.conversation ?? openMessage.id) : null;
    if (here !== null && here === ownConversation) return;
    if (toast?.undo) return;
    void closeThreadWindow();
  }, [openMessage, ownConversation, toast]);

  /** Work a window mustn't close in the middle of: sends being queued, drafts being saved. */
  const inFlight = useRef(new Set<Promise<unknown>>());
  function track<T>(work: Promise<T>): Promise<T> {
    inFlight.current.add(work);
    void work.finally(() => inFlight.current.delete(work)).catch(() => {});
    return work;
  }

  /**
   * Gets this conversation's window ready to close: its draft saved, sends queued, drafts
   * deleted. False if it should stay open: a draft didn't save (its error offers it back), or
   * with `asked` off, a send failed and put its email back in the composer to see.
   */
  async function settleWindow(asked: boolean): Promise<boolean> {
    const session = composer.current.key;
    /** Composer sessions this closed: one still showing is waiting for React to catch up. */
    const closed = new Set<number>();
    for (let round = 0; round < 4; round++) {
      const { draft: open, key } = composer.current;
      if (open && !closed.has(key)) {
        if (!asked && key !== session) return false;
        if (!(await track(latestCloseComposer.current(true)))) return false;
        closed.add(key);
      }
      const results = await Promise.allSettled([...inFlight.current]);
      // A composer close that was already saving, and failed: its error offers the draft back.
      if (results.some((r) => r.status === "fulfilled" && r.value === false)) return false;
      await draftSaves.current.catch(() => {});
      for (const [timer, discard] of [...pendingDiscards.current]) {
        window.clearTimeout(timer);
        pendingDiscards.current.delete(timer);
        await discard();
      }
      const now = composer.current;
      if ((!now.draft || closed.has(now.key)) && inFlight.current.size === 0) return true;
    }
    return false;
  }
  const latestSettle = useRef(settleWindow);
  latestSettle.current = settleWindow;
  const latestCloseComposer = useRef(closeComposer);
  latestCloseComposer.current = closeComposer;

  async function closeThreadWindow() {
    if (!(await latestSettle.current(false))) return;
    // A send queued meanwhile put up its Undo: the window waits for it.
    if (toastNow.current?.undo) return;
    closeWindow();
  }

  // The window's own close button: the draft is saved and a send queued first; one that
  // couldn't be saved keeps the window open.
  useEffect(() => {
    if (THREAD_WINDOW === null) return;
    return onWindowClose(() => latestSettle.current(true));
  }, []);

  // Quitting, the main window does the same; a draft it couldn't save keeps spark-rs open.
  useEffect(() => {
    if (THREAD_WINDOW !== null) return;
    return onQuitting(() => latestSettle.current(true));
  }, []);

  /** Another window changed mail: read the changes, coalesced. */
  const editedTimer = useRef(0);
  function otherWindowEdited() {
    clearTimeout(editedTimer.current);
    editedTimer.current = window.setTimeout(() => {
      void reloadMessages();
      void reloadPrioritySenders();
      void getGatekeeperMode().then(setGatekeeperMode, () => {});
      reloadPlanned();
    }, 150);
  }

  // Another window saved a preference this one shows (grouping, cards, …): take it, so neither
  // writes its old copy back over the other's.
  useEffect(() => {
    const same = <T,>(current: T, next: T) => (JSON.stringify(current) === JSON.stringify(next) ? current : next);
    const onStorage = () => {
      setGrouped((g) => same(g, loadGrouped()));
      setCardPrefs((c) => same(c, loadCardPrefs()));
      setSeen((s) => same(s, loadSeen()));
      setHomeSettings((h) => same(h, loadHomeSettings()));
      setMinimize(loadMinimize());
      setAlwaysPopOut(loadPopOut());
      setPriorityLines(loadPriorityLines());
      // The theme and zoom are the app's, every window's.
      const nextTheme = loadTheme();
      setTheme(nextTheme);
      applyTheme(nextTheme);
      applyZoom(loadZoom());
    };
    window.addEventListener("storage", onStorage);
    return () => window.removeEventListener("storage", onStorage);
  }, []);

  // Links in email: mailto: starts a new email here, as in Spark; an app's link asks first.
  useEffect(() => {
    setLinkHandlers({
      mailto: (link) => {
        const m = parseMailto(link);
        if (!m) return;
        latestCompose.current({
          to: m.to,
          cc: m.cc,
          showCc: m.cc !== "",
          bcc: m.bcc,
          showBcc: m.bcc !== "",
          subject: m.subject,
          body: m.body,
        });
      },
      external: (url) => void openExternal(url).catch(fileError),
      ask: (url, scheme) =>
        setConfirm({
          title: "Potentially Unsafe Link",
          message: `spark-rs has detected a suspicious URL scheme: ${scheme} This link could expose your device and data to security risks, including unauthorized access, malicious code execution, or device compromise. Proceed only if you trust the source.`,
          confirmLabel: "Open Anyway",
          run: () => openExternal(url),
        }),
    });
    return () => setLinkHandlers(null);
  }, []);

  // A question about one email goes once its composer closes or another opens.
  const composing = draft !== null;
  useEffect(() => {
    setConfirm((c) => (c?.session !== undefined && (c.session !== composerKey || !composing) ? null : c));
  }, [composerKey, composing]);
  const accountsKnown = accounts.length > 0;
  useEffect(() => {
    if (!accountsKnown || THREAD_WINDOW !== null) return;
    const open = () =>
      void takeMailto().then((link) => {
        const m = link ? parseMailto(link) : null;
        if (!m) return;
        latestCompose.current({
          to: m.to,
          cc: m.cc,
          showCc: m.cc !== "",
          bcc: m.bcc,
          showBcc: m.bcc !== "",
          subject: m.subject,
          body: m.body,
        });
      }, () => {});
    // Listening first, so a link that comes in meanwhile isn't missed; then the one waiting.
    let stop: (() => void) | null = null;
    let gone = false;
    void onMailto(open).then((unlisten) => {
      if (gone) return unlisten();
      stop = unlisten;
      open();
    });
    return () => {
      gone = true;
      stop?.();
    };
  }, [accountsKnown]);

  // Rows that left the list leave the selection.
  useEffect(() => {
    if (pickedRows.length !== picked.size) setPicked(new Set(pickedRows));
  }, [pickedRows, picked]);

  // ---- data ----

  /**
   * The list is scrolled near its end: bring in older mail, as Spark does. The accounts and folders aren't read
   * again: a round that changes what they show (`hasOlder`, a folder's first sync) says so with its events, before
   * `loadOlder` resolves. The sidebar counts come from the messages.
   */
  function reachBack() {
    if (!older || reaching.current) return;
    reaching.current = true;
    setLoadingOlder(true);
    void loadOlder(older.accounts, older.folders)
      .then(reloadMessages)
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

  /** `update` for many messages at once, e.g. a batch action: one re-render before saving and one after. */
  async function updateAll(changes: { id: number; patch: MessagePatch }[]) {
    if (changes.length === 1) return update(changes[0].id, changes[0].patch);
    const patches = new Map(changes.map((c) => [c.id, c.patch]));
    setMessages((ms) => ms.map((m) => (patches.has(m.id) ? patched(m, patches.get(m.id)!) : m)));
    const results = await Promise.allSettled(changes.map((c) => updateMessage(c.id, c.patch)));
    const saved = new Map<number, Message>();
    let failure: unknown;
    results.forEach((r, i) => {
      if (r.status === "fulfilled") saved.set(changes[i].id, r.value);
      else failure ??= r.reason;
    });
    setMessages((ms) => ms.map((m) => saved.get(m.id) ?? m));
    if (failure !== undefined) {
      showToast(`Couldn't update the messages: ${String(failure)}`);
      void reloadMessages();
    }
  }

  /** Reloads Send Later's queue and the reminders. */
  function reloadPlanned() {
    void listScheduled().then(setScheduled, () => {});
    void listReminders().then(setReminders, () => {});
  }

  /**
   * Brings the list up to date with the store from what changed since it was read, keeping the cursor and open
   * thread while their messages still exist.
   */
  function reloadMessages() {
    return refreshMessages().then(
      () => {},
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
        // The demo keeps its own mode.
        if (demoChanged) void getGatekeeperMode().then(setGatekeeperMode, () => {});
        void reloadPrioritySenders();
      },
      (error) => showToast(`Couldn't load accounts: ${String(error)}`, undefined, ICONS.error),
    );
  }

  /** Settings → Gatekeeper. Turning it off accepts everyone waiting, so the list reloads. */
  function changeGatekeeper(mode: GatekeeperMode) {
    const before = gatekeeperMode;
    setGatekeeperMode(mode);
    void saveGatekeeperMode(mode).then(
      () => void reloadMessages(),
      (error) => {
        setGatekeeperMode(before);
        showToast(`Couldn't change the Gatekeeper: ${String(error)}`, undefined, ICONS.error);
      },
    );
  }

  /** Bumped by each load and each change shown before the store answers; a load answered after either is stale. */
  const prioritySendersVersion = useRef(0);

  function reloadPrioritySenders() {
    const version = ++prioritySendersVersion.current;
    return getPrioritySenders().then(
      (list) => {
        if (version === prioritySendersVersion.current) setPrioritySenders(list);
      },
      (error) => showToast(`Couldn't load Priority senders: ${String(error)}`, undefined, ICONS.error),
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
    const untrusted = e.certificate;
    showToast(
      `${label}: ${e.message ?? "Couldn't check for new mail."}`,
      untrusted ? () => setCertificate({ account: e.account, certificate: untrusted }) : () => openSettings("accounts"),
      ICONS.error,
      { actionLabel: untrusted ? "Review" : "Settings", duration: ERROR_TOAST_MS },
    );
  }

  function sendStatus(e: SendStatusEvent) {
    // Tried again: only its row in the Scheduled list changes.
    if (e.status === "trying") return setScheduled((items) => markTrying(items, e.id));
    reloadPlanned();
    // Queued again: the Scheduled list shows it, and it may be told of later.
    if (e.status === "retrying") return;
    const sent = outbox.current.get(e.id);
    outbox.current.delete(e.id);
    // A saved draft it came from is gone now, or back in Drafts if it
    // failed; and the people it went to are through the Gatekeeper.
    void reloadMessages();
    if (e.status !== "failed") return;
    if (e.queued) {
      // Long after "Message sent", and maybe with others: the Scheduled list has each that couldn't go yet.
      void listScheduled()
        .catch(() => [])
        .then((items) =>
          showToast(stuckNotice(items, e), () => selectList("scheduled"), ICONS.error, {
            actionLabel: "Show",
            duration: ERROR_TOAST_MS,
          }),
        );
      return;
    }
    const recovered = e.draftId;
    const untrusted = e.certificate;
    if (untrusted) {
      showToast(
        `Couldn't send: ${e.message ?? "the server's certificate isn't trusted."} It's back in Drafts.`,
        () => setCertificate({ account: e.account, certificate: untrusted }),
        ICONS.error,
        { actionLabel: "Review", duration: ERROR_TOAST_MS },
      );
      return;
    }
    showToast(
      `Couldn't send: ${e.message ?? "the server refused the message."}`,
      recovered !== undefined ? () => void editDraft({ id: recovered }) : sent && (() => openComposer(sent)),
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
    undo?: () => unknown,
    icon?: ToastIcon,
    options: Pick<ToastData, "actionLabel" | "duration"> = {},
  ) {
    window.clearTimeout(toastTimer.current);
    const next = { id: Date.now(), text, undo, icon, ...options };
    setToast(next);
    // At once, for a window deciding whether it can close (an Undo keeps it open).
    toastNow.current = next;
    toastTimer.current = window.setTimeout(() => {
      setToast((t) => (t?.id === next.id ? null : t));
      if (toastNow.current?.id === next.id) toastNow.current = null;
    }, options.duration ?? TOAST_MS);
  }

  /** Takes toast `id` down if it's still showing. */
  function closeToast(id: number) {
    if (toastNow.current?.id !== id) return;
    window.clearTimeout(toastTimer.current);
    setToast(null);
    toastNow.current = null;
  }

  function undoToast() {
    window.clearTimeout(toastTimer.current);
    // Undo that's still putting things back (a send cancelled, its draft reopened) keeps a
    // conversation's own window open till it's done.
    const work = toast?.undo?.();
    if (work instanceof Promise) void track(work);
    setToast(null);
    toastNow.current = null;
    // A conversation's own window, undoing what took it off its conversation, shows it again.
    if (THREAD_WINDOW !== null) setOpenId((id) => id ?? THREAD_WINDOW);
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
    const putBack = leave(after, changes.map((c) => c.id));
    void updateAll(changes);
    return () => {
      void updateAll(changes.map((c) => ({ id: c.id, patch: c.previous })));
      putBack();
    };
  }

  /** Moves the cursor off what `after` takes out of the list (of `changed`, if given). Returns how to put it back. */
  function leave(after: Message[], changed?: number[]): () => void {
    const before = listedIds(model);
    const remaining = listedIds(buildList(after, view, sticky, ctx));
    const leaving =
      THREAD_WINDOW !== null
        ? movedOn(after, changed)
        : (changed ?? [...before]).filter((id) => before.has(id) && !remaining.has(id));
    const cursor = { selectedId, openId };
    moveOffMessages(leaving);
    return () => {
      if (leaving.length) {
        setSelectedId(cursor.selectedId);
        setOpenId(cursor.openId);
      }
    };
  }

  /**
   * In a conversation's own window, which has no list to leave: what a change takes away from
   * where it was (Done, Trash, Snooze, Set Aside, a folder), so the window moves on, wherever the
   * conversation was. Moving it to the Inbox doesn't.
   */
  function movedOn(after: Message[], changed?: number[]) {
    const ids = changed ? new Set(changed) : null;
    return after
      .filter((a) => {
        const b = byId.get(a.id);
        if (!b || (ids && !ids.has(a.id))) return false;
        return (a.state !== b.state && a.state !== "inbox") || a.folder !== b.folder;
      })
      .map((a) => a.id);
  }

  /**
   * Accepts or blocks senders or domains for every account, with an undo:
   * their mail follows, stored mail too, so the list reloads once it's done.
   * `done` is mail to mark Done with it (Accept and Done).
   */
  function decide(changes: SenderChange[], text: string, icon?: ToastIcon, done: Message[] = []) {
    if (!changes.length) return;
    const finishing = done.map((m) => ({ id: m.id, patch: DONE, previous: previousValues(m, DONE) }));
    const after = predict(messages, changes).map((m) => (done.some((x) => x.id === m.id) ? patched(m, DONE) : m));
    const putBack = leave(after);
    setMessages(after);
    const failed = (error: unknown) => {
      showToast(`Couldn't change the sender: ${String(error)}`, undefined, ICONS.error);
      putBack();
      void reloadMessages();
    };
    const undoAll = (undo: SenderChange[]) => async () => {
      if (finishing.length) await updateAll(finishing.map((f) => ({ id: f.id, patch: f.previous })));
      await decideSenders(undo);
    };
    // Tracked: a conversation's own window waits for it, and then for its Undo.
    void track(
      decideSenders(changes)
        .then(async (undo) => {
          if (finishing.length) await updateAll(finishing);
          return undo;
        })
        .then((undo) => {
          void reloadMessages();
          showToast(text, () => void undoAll(undo)().then(reloadMessages, failed).finally(putBack), icon);
        }, failed),
    );
  }

  /** Everyone at `m`'s domain, unless anyone can have an address there. */
  function decideDomain(m: Message, decision: SenderDecision) {
    const at = decidableDomain(m.address);
    if (!at) return;
    const accepted = decision === "accepted";
    decide(
      [{ screen: domain(at), decision }],
      accepted ? "Domain accepted" : "Domain blocked",
      accepted ? ICONS.accept : ICONS.block,
    );
  }

  /** `m`'s sender's mail waiting in the Gatekeeper. */
  const waitingFrom = (m: Message) =>
    messages.filter((x) => x.screening && x.state === "inbox" && x.address.toLowerCase() === m.address.toLowerCase());

  const prioritySet = useMemo(() => new Set(prioritySenders.map((a) => a.toLowerCase())), [prioritySenders]);
  const isOwnAddress = (address: string) => accounts.some((a) => a.email.toLowerCase() === address.toLowerCase());
  /** Whether `m`'s sender is a Priority sender; null for the user's own mail, which can't be. */
  const senderPriority = (m: Message) =>
    !m.address.includes("@") || isOwnAddress(m.address) ? null : prioritySet.has(m.address.toLowerCase());

  /**
   * Adds senders to the Priority senders, or takes them off, with an undo:
   * all their mail turns Priority or not, so the list reloads once it's done.
   */
  function prioritize(addresses: string[], priority: boolean) {
    const byKey = new Map(addresses.map((a) => [a.trim().toLowerCase(), a.trim()]));
    if (!byKey.size) return;
    const after = messages.map((m) =>
      byKey.has(m.address.toLowerCase()) && m.priority !== priority ? { ...m, priority } : m,
    );
    const putBack = leave(after);
    setMessages(after);
    // The list changes at once too, so the menus offer the opposite right away.
    showPrioritySenders([...byKey.values()], priority);
    const text = `${byKey.size === 1 ? "Sender is" : "Senders are"} ${priority ? "Priority" : "Not Priority"}`;
    const reload = () => void Promise.all([reloadMessages(), reloadPrioritySenders()]);
    const failed = (error: unknown) => {
      showToast(`Couldn't change Priority senders: ${String(error)}`, undefined, ICONS.error);
      putBack();
      reload();
    };
    void prioritizeSenders([...byKey.values()], priority).then((undo) => {
      reload();
      // Nothing changed (it was so already): keep the toast whose undo does something.
      if (!undo.senders.length && !undo.messages.length) return;
      showToast(
        text,
        () => {
          const changed = new Set(undo.messages);
          setMessages((ms) => ms.map((m) => (changed.has(m.id) ? { ...m, priority: undo.priority } : m)));
          showPrioritySenders(undo.senders, undo.priority);
          void undoPrioritize(undo).then(reload, failed).finally(putBack);
        },
        ICONS.priority,
      );
    }, failed);
  }

  /** Puts senders on the Priority senders shown, or takes them off, before the store answers. */
  function showPrioritySenders(addresses: string[], priority: boolean) {
    prioritySendersVersion.current++;
    const keys = new Set(addresses.map((a) => a.toLowerCase()));
    setPrioritySenders((list) => {
      const rest = list.filter((a) => !keys.has(a.toLowerCase()));
      return priority ? [...rest, ...addresses].sort((a, b) => a.toLowerCase().localeCompare(b.toLowerCase())) : rest;
    });
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
    // A conversation's own window has no next one to move to: keys act on nothing it doesn't show.
    if (THREAD_WINDOW !== null) {
      setOpenId(null);
      setSelectedId(null);
      return;
    }
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
    // Over the Home Screen, the email closes rather than opening the next one.
    if (twoColumns && openId !== null && next !== null && !homeShown) follow(next);
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
      holdRows(unread);
      for (const x of unread) void update(x.id, { unread: false });
    }
  }

  /** Mail being read stays where it shows for the rest of the visit (see `sticky`). */
  function holdRows(read: Message[]) {
    setSticky((s) => new Set([...s, ...read.map((m) => m.id)]));
  }

  /** Opens a message: a draft in the composer, other mail in the reader. */
  function openListed(id: number) {
    const group = model.groups.get(id);
    if (group) return changeView({ sub: { kind: "group", key: group.key } }, true);
    const m = messages.find((x) => x.id === id);
    if (m && isDraft(m, folders)) void editDraft(m);
    else openConversation(id);
  }

  /** Opens a conversation the way it's asked to be, in a window of its own or in the reader. */
  function openConversation(id: number, list = messages) {
    const m = list.find((x) => x.id === id);
    if (m && alwaysPopOut && !twoColumns && THREAD_WINDOW === null && !isDraft(m, folders)) popOut(m);
    else openThread(id, list);
  }

  /**
   * Spark's "Open Thread in a new Window": the conversation leaves the reader for a window of its
   * own; with `then`, the window summarizes it in that style.
   */
  function popOut(m: Message, then?: SummaryStyle) {
    void openThreadWindow(m.id, m.subject || "No subject", then).then(() => {
      // The reader lets go of what it showed (in split view, maybe another conversation), and the
      // list's cursor goes to this one, as opening it here would: keys act on it, not the one before.
      setOpenId(null);
      setSelectedId(m.id);
    }, fileError);
  }

  /** Summarizes a conversation where it's read: its own window, if it opens in one, else the reader. */
  function summarizeConversation(m: Message, style: SummaryStyle) {
    if (openId !== m.id) {
      if (alwaysPopOut && !twoColumns && THREAD_WINDOW === null && !isDraft(m, folders)) return popOut(m, style);
      openThread(m.id);
    }
    summarize(m, style);
  }

  /** In a conversation's own window: summarize what it shows. */
  function summarizeOpen(style: string) {
    const s = summaryStyle(style);
    if (s && openMessage) summarize(openMessage, s);
  }

  /** The cursor moved: split view shows the message there, but a draft only opens when asked. */
  function follow(id: number) {
    const m = messages.find((x) => x.id === id);
    // A group row only opens when asked, like a draft.
    if (model.groups.has(id) || (m && isDraft(m, folders))) {
      setOpenId(null);
      setSelectedId(id);
    } else {
      openThread(id);
    }
  }

  /** A notification was clicked: show its message in the Inbox, closing what covers it. */
  function openFromNotification(id: number) {
    void refreshMessages().then(
      (loaded) => {
        const m = mailbox.get(id);
        if (!m) return;
        setSettingsPage(null);
        setPalette(null);
        setCalendarOpen(false);
        closeSidebar();
        startHomeSession();
        const account = view.account === m.account ? m.account : "all";
        changeView({ folder: "inbox", mode: loadInboxMode(account), sub: null, search: null, account }, true);
        openConversation(id, loaded);
      },
      (error) => showToast(`Couldn't load your mail: ${String(error)}`, undefined, ICONS.error),
    );
  }

  function moveCursor(step: 1 | -1) {
    clearPicked();
    const order = model.order;
    if (!order.length) return;
    const anchor = openId ?? selectedId;
    const i = anchor === null ? -1 : order.indexOf(rowOf(anchor));
    const next = i < 0 ? order[0] : order[clamp(i + step, 0, order.length - 1)];
    if (twoColumns || openId !== null) follow(next);
    else setSelectedId(next);
  }

  function changeView(next: Partial<ListView>, keepThread = false) {
    clearPicked();
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
    setCalendarOpen(false);
    startHomeSession();
    const narrowed = hasAccountRows(folder) ? account : "all";
    // Each account keeps the Inbox mode it was last shown in.
    const mode = folder === "inbox" ? loadInboxMode(narrowed) : view.mode;
    changeView({ folder, account: narrowed, mode, sub: null, search: null });
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
    setRecentSearches((r) => rememberSearch(r, query));
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

  function setMode(mode: InboxMode) {
    saveInboxMode(view.account, mode);
    changeView({ folder: "inbox", mode, sub: null, search: null }, true);
  }

  function openSearch() {
    startHomeSession();
    if (searching) searchRef.current?.select();
    else changeView({ search: "", sub: null }, true);
  }

  const closeSearch = () => changeView({ search: null }, true);

  /**
   * Esc: the folder picker and sidebar overlay, then the selection, then the thread, then a sub-list, then
   * search, then the assistant's panel.
   */
  function closeTopmost() {
    if (pickerOpen || sidebarMode === "overlay") closeSidebar();
    else if (pickedRows.length) clearPicked();
    else if (openId !== null) setOpenId(null);
    else if (view.sub) changeView({ sub: null });
    else if (searching) closeSearch();
    else if (assistantOpen) setAssistantOpen(false);
  }

  function onSearchKeyDown(e: ReactKeyboardEvent<HTMLInputElement>) {
    if (e.key === "Escape") {
      e.preventDefault();
      closeSearch();
    } else if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      moveCursor(e.key === "ArrowDown" ? 1 : -1);
    } else if (e.key === "Enter") {
      if (view.search) setRecentSearches((r) => rememberSearch(r, view.search!));
      if (selectedId !== null) openListed(selectedId);
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

  /** The account's Spam folder (Junk Email on Microsoft 365). */
  const spamFolder = (account: AccountId) => folders.find((f) => f.account === account && f.role === "spam");
  const inSpam = (m: Message) => m.state === "filed" && m.folder !== undefined && m.folder === spamFolder(m.account)?.id;

  const actions = {
    /** On an email a reminder brought back, Done ends the reminder and leaves the email where it's kept. */
    done: (m: Message) =>
      own(m).reminder
        ? finishReminders([own(m)])
        : m.state === "done"
          ? act(together(m), { state: "inbox" }, "Moved to Inbox", ICONS.inbox)
          : act(together(m), { state: "done" }, "Marked as Done", ICONS.done),
    /** Mail already in Trash is deleted for good, after asking. */
    trash: (m: Message) => {
      if (isDraft(m, folders)) discardListed([m]);
      else if (m.state === "deleted") deleteForeverAsk(together(m));
      else act(together(m), { state: "deleted" }, "Moved to Trash", ICONS.trash);
    },
    /** Just this email of its conversation, from its card's menu. */
    trashOne: (m: Message) => {
      const one = own(m);
      if (isDraft(one, folders)) discardListed([one]);
      else if (one.state === "deleted") deleteForeverAsk([one]);
      else act([one], { state: "deleted" }, "Moved to Trash", ICONS.trash);
    },
    spam: (m: Message) => spam([m]),
    deleteForever: (m: Message) => deleteForeverAsk(together(m)),
    pin: (m: Message) => flag(m, "pinned", "Pinned", "Unpinned", ICONS.pin),
    priority: (m: Message) => flag(m, "priority", "Thread is Priority", "Thread is Not Priority", ICONS.priority),
    /** Reading marks the whole conversation read; unread marks `m`. */
    read: (m: Message) => {
      const unread = threadOf(m).filter((x) => x.unread);
      if (unread.length) {
        holdRows(unread);
        act(unread, { unread: false }, "Marked as Read", ICONS.read);
      } else act([own(m)], { unread: true }, "Marked as Unread", ICONS.read);
    },
    aside: (m: Message) =>
      m.state === "aside"
        ? act(together(m), { state: "inbox" }, "Moved to Inbox", ICONS.inbox)
        : act(together(m), { state: "aside" }, "Set aside", ICONS.aside),
    snooze: (m: Message, p: LaterPreset) => snoozeUntil([m], laterTime(p), p.label.toLowerCase()),
    snoozeCustom: (m: Message) => askSnoozeTime([m]),
    sendAgain,
    sendDraft: (m: Message) => void track(sendDraftNow(m)),
    move: (m: Message) => openMove([own(m)]),
    changeCategory: (m: Message, category: Category) => void changeCategory(m, category),
    groupSender,
    groupDomain,
    grouping: (m: Message) => ({
      sender: grouped.senders.includes(m.address.toLowerCase()),
      domain: decidableDomain(m.address),
      domainGrouped: grouped.domains.includes(decidableDomain(m.address) ?? ""),
    }),
    /** Spark's Discard Snooze: back in the Inbox now. */
    unsnooze: (m: Message) => act(together(m), { state: "inbox" }, "Snooze discarded", ICONS.inbox),
    mute,
    unsubscribe: unsubscribeFrom,
    // A conversation's own window has no list: the main window searches.
    searchSender: (m: Message) =>
      THREAD_WINDOW === null ? searchFor(`from:${m.address}`) : void searchInMain(`from:${m.address}`),
    accept: (m: Message) => decide(forSenders([m], "accepted"), "Sender accepted", ICONS.accept),
    acceptAndOpen: (m: Message) => {
      decide(forSenders([m], "accepted"), "Sender accepted", ICONS.accept);
      if (view.folder !== "inbox" || view.sub || view.search !== null) {
        changeView({ folder: "inbox", sub: null, search: null }, true);
      }
      openConversation(m.id);
    },
    acceptAndDone: (m: Message) => {
      const waiting = waitingFrom(m);
      const done = waiting.length ? waiting : [own(m)];
      decide(
        forSenders([m], "accepted"),
        `Sender accepted. ${plural(done.length, "email")} marked as Done`,
        ICONS.accept,
        done,
      );
    },
    acceptDomain: (m: Message) => decideDomain(m, "accepted"),
    block: (m: Message) => decide(forSenders([m], "blocked"), "Sender blocked", ICONS.block),
    blockDomain: (m: Message) => decideDomain(m, "blocked"),
  };

  /** Move to Spam, or for mail in Spam, Not Spam: back to the Inbox. Rows stand for their conversations. */
  function spam(rows: Message[]) {
    if (rows.length && rows.every(inSpam)) {
      act(rows.flatMap(together), { state: "inbox" }, "Marked as Not Spam", ICONS.inbox);
      return;
    }
    const movable = rows.filter((m) => !inSpam(m) && !isDraft(m, folders) && spamFolder(m.account));
    if (!movable.length) {
      showToast("This account has no Spam folder", undefined, ICONS.error);
      return;
    }
    act(movable.flatMap(together), (m) => ({ folder: spamFolder(m.account)!.id }), "Moved to Spam", ICONS.spam);
  }

  /** Deletes emails for good, after asking: there's no undo, as in Spark. */
  function deleteForeverAsk(emails: Message[]) {
    const ids = emails.map((m) => m.id);
    if (!ids.length) return;
    const one = ids.length === 1;
    setConfirm({
      title: one ? "Delete email" : "Delete emails",
      message: one
        ? "Do you want to permanently delete this email?"
        : `Do you want to permanently delete these ${ids.length} emails?`,
      confirmLabel: "Delete",
      run: async () => {
        await deleteForever(ids);
        moveOffMessages(ids);
        setMessages((ms) => ms.filter((m) => !ids.includes(m.id)));
        showToast(one ? "Email deleted" : `${plural(ids.length, "email")} deleted`, undefined, ICONS.trash);
      },
    });
  }

  /** The server folders behind a list that can be emptied: Trash, Spam, or a folder that isn't a Gmail label. */
  function emptiable(key: ListKey, account: AccountFilter = "all"): FolderView[] {
    if (key !== "trash" && key !== "spam" && folderIdOf(key) === undefined) return [];
    const ids = listFolderIds(key, folders, accountOf(account));
    const gmail = (f: FolderView) => accounts.find((a) => a.id === f.account)?.kind === "gmail";
    return folders.filter(
      (f) => ids.includes(f.id) && (f.role === "trash" || f.role === "spam" || (f.role === undefined && !gmail(f))),
    );
  }

  /** Deletes everything in a list's server folders for good, after asking; also mail not downloaded here. */
  function emptyList(key: ListKey, account: AccountFilter = "all") {
    const targets = emptiable(key, account);
    if (!targets.length) return;
    const name = isSystemList(key) ? SYSTEM_LISTS[key].label : targets[0].name;
    setConfirm({
      title: `Empty ${name}`,
      message: `Do you want to permanently delete all emails inside the ${name} folder? This can't be undone.`,
      confirmLabel: "Empty",
      run: async () => {
        const deleted = await emptyFolders(targets.map((f) => f.id));
        await reloadMessages();
        showToast(`${plural(deleted, "email")} deleted`, undefined, ICONS.trash);
      },
    });
  }

  /** Mark all as Read (or Unread): every email a list shows, the open one by default. */
  function markAll(unread: boolean, key?: ListKey) {
    const list = key === undefined ? model : buildList(listed, { ...INITIAL_VIEW, folder: key, mode: "simple" }, new Set(), ctx);
    const ids = listedIds(list);
    const targets = listed.filter((m) => ids.has(m.id) && m.unread !== unread && !isDraft(m, folders));
    if (!targets.length) {
      showToast(unread ? "Everything here is unread" : "Everything here is read", undefined, ICONS.read);
      return;
    }
    act(targets, { unread }, `${plural(targets.length, "email")} marked as ${unread ? "unread" : "read"}`, ICONS.read);
  }

  // ---- selection ----

  function clearPicked() {
    if (picked.size) setPicked(new Set());
    pickAnchor.current = null;
  }

  /**
   * Ctrl/⌘-click puts a row in the selection or takes it out; Shift-click
   * adds the rows from where the selection started. The cursor's row is in
   * a new selection too.
   */
  function pickRow(id: number, range: boolean) {
    const order = model.order;
    const cursor = selectedId === null ? null : rowOf(selectedId);
    const next = new Set(pickedRows.length ? pickedRows : cursor !== null && order.includes(cursor) ? [cursor] : []);
    const anchor = pickAnchor.current ?? cursor ?? id;
    const [from, to] = [order.indexOf(anchor), order.indexOf(id)];
    if (range && from >= 0 && to >= 0) {
      for (const x of order.slice(Math.min(from, to), Math.max(from, to) + 1)) next.add(x);
    } else if (next.has(id)) {
      next.delete(id);
    } else {
      next.add(id);
    }
    pickAnchor.current = range ? anchor : id;
    setPicked(next);
  }

  /** Shift+↓ / Shift+↑: the row below or above the cursor joins the selection, and the cursor moves there. */
  function pickNext(step: 1 | -1) {
    const order = model.order;
    if (!order.length) return;
    const cursor = selectedId === null ? null : rowOf(selectedId);
    const i = cursor === null ? -1 : order.indexOf(cursor);
    const next = order[clamp(i + step, 0, order.length - 1)];
    const set = new Set(pickedRows.length ? pickedRows : i >= 0 ? [cursor!] : []);
    set.add(next);
    pickAnchor.current ??= cursor ?? next;
    setPicked(set);
    setSelectedId(next);
  }

  /** A batch action on the selected conversations (or `rows`); a big one asks first. */
  function batch(run: (rows: Message[]) => void, rows = batchRows) {
    if (!rows.length) return;
    const emails = rows.reduce((n, m) => n + threadOf(m).length, 0);
    if (emails <= HEAVY_BATCH) return run(rows);
    setConfirm({
      title: "Proceed with a batch action?",
      message: `This acts on ${plural(emails, "email")} at once.`,
      confirmLabel: "Proceed",
      danger: false,
      run: () => run(rows),
    });
  }

  const conversationCount = (rows: Message[]) => plural(rows.length, "conversation");
  const pinnedThread = (m: Message) => threadOf(m).some((x) => x.pinned);
  const priorityThread = (m: Message) => threadOf(m).some((x) => x.priority);

  const selection: Selection = {
    count: pickedMessages.length,
    allDone: pickedMessages.every((m) => m.state === "done"),
    anyUnread: pickedMessages.some((m) => threadOf(m).some((x) => x.unread)),
    allPinned: pickedMessages.every(pinnedThread),
    allPriority: pickedMessages.every(priorityThread),
    allAside: pickedMessages.every((m) => m.state === "aside"),
    allSnoozed: pickedMessages.every((m) => m.state === "snoozed"),
    allSpam: pickedMessages.every(inSpam),
    allDeleted: pickedMessages.every((m) => m.state === "deleted"),
  };

  const bulk: BulkActions = {
    done: () =>
      batch((rows) =>
        rows.every((m) => m.state === "done")
          ? act(rows.flatMap(together), { state: "inbox" }, `${conversationCount(rows)} moved to Inbox`, ICONS.inbox)
          : doneAll(rows),
      ),
    trash: () =>
      batch((rows) => {
        if (rows.every((m) => m.state === "deleted")) return deleteForeverAsk(rows.flatMap(together));
        const drafts = rows.filter((m) => isDraft(m, folders)).map((m) => m.id);
        const mail = rows.filter((m) => !isDraft(m, folders) && m.state !== "deleted").flatMap(together);
        const undo = mail.length ? apply(mail, () => ({ state: "deleted" })) : undefined;
        const text = `${conversationCount(rows)} moved to Trash`;
        if (drafts.length) {
          moveOffMessages(drafts);
          discardLater(drafts, undo, text);
        } else showToast(text, undo, ICONS.trash);
      }),
    read: () =>
      batch((rows) => {
        const unread = rows.flatMap(threadOf).filter((x) => x.unread);
        if (unread.length) {
          holdRows(unread);
          act(unread, { unread: false }, `${conversationCount(rows)} marked as read`, ICONS.read);
        } else act(rows.map(own), { unread: true }, `${conversationCount(rows)} marked as unread`, ICONS.read);
      }),
    pin: () =>
      batch((rows) =>
        rows.every(pinnedThread)
          ? act(rows.flatMap(threadOf).filter((x) => x.pinned), { pinned: false }, `${conversationCount(rows)} unpinned`, ICONS.pin)
          : act(rows.filter((m) => !pinnedThread(m)).map(own), { pinned: true }, `${conversationCount(rows)} pinned`, ICONS.pin),
      ),
    priority: () =>
      batch((rows) =>
        rows.every(priorityThread)
          ? act(
              rows.flatMap(threadOf).filter((x) => x.priority),
              { priority: false },
              `${conversationCount(rows)} removed from Priority`,
              ICONS.priority,
            )
          : act(
              rows.filter((m) => !priorityThread(m)).map(own),
              { priority: true },
              `${conversationCount(rows)} marked as Priority`,
              ICONS.priority,
            ),
      ),
    aside: () =>
      batch((rows) =>
        rows.every((m) => m.state === "aside")
          ? act(rows.flatMap(together), { state: "inbox" }, `${conversationCount(rows)} moved to Inbox`, ICONS.inbox)
          : act(
              rows.filter((m) => m.state !== "aside").flatMap(together),
              { state: "aside" },
              `${conversationCount(rows)} set aside`,
              ICONS.aside,
            ),
      ),
    snooze: (p) => batch((rows) => snoozeRows(rows, p)),
    snoozeCustom: () => askSnoozeTime(batchRows),
    unsnooze: () =>
      batch((rows) =>
        act(rows.flatMap(together), { state: "inbox" }, `${conversationCount(rows)} back in the Inbox`, ICONS.inbox),
      ),
    spam: () => batch(spam),
    move: () => openMove(batchRows),
    clear: clearPicked,
  };

  function snoozeRows(rows: Message[], p: LaterPreset) {
    snoozeUntil(rows, laterTime(p), p.label.toLowerCase());
  }

  /**
   * Snoozes conversations until `at`; `until` says when in the toast
   * ("tomorrow"). An email a reminder brought back has its reminder moved
   * there instead: it stays where it's kept.
   */
  function snoozeUntil(rows: Message[], at: number, until = formatLater(at)) {
    const reminded = rows.filter((m) => own(m).reminder);
    const mail = rows.filter((m) => !own(m).reminder);
    const undoReminders = moveReminders(reminded.map(own), at);
    const undoMail = mail.length
      ? apply(mail.flatMap(together), () => ({ state: "snoozed", snoozedUntil: at }))
      : undefined;
    const what = rows.length === 1 ? "Snoozed" : `${conversationCount(rows)} snoozed`;
    showToast(
      `${what} until ${until}`,
      () => {
        undoMail?.();
        undoReminders?.();
      },
      ICONS.snooze,
    );
  }

  // ---- organising ----

  /** Spark's Move to folder: the conversations' account's folders, in a picker. */
  function openMove(rows: Message[]) {
    const movable = rows.filter((m) => !isDraft(m, folders));
    if (!movable.length) return;
    if (new Set(movable.map((m) => m.account)).size > 1) {
      showToast("Select emails from the same account to move them", undefined, ICONS.folder);
      return;
    }
    setPalette({ kind: "move", targetIds: movable.map((m) => m.id) });
  }

  /** Where emails can be moved: the Inbox, Archive, Trash, Spam and the account's own folders, recent ones first. */
  function moveCommands(targets: Message[]): Command[] {
    if (!targets.length) return [];
    const account = targets[0].account;
    const recent = loadMoveRecent()[String(account)] ?? [];
    const own = folders.filter((f) => f.account === account && f.selectable && f.role === undefined);
    const spamFolder = folders.find((f) => f.account === account && f.role === "spam");
    const rank = (f: FolderView) => (recent.includes(f.id) ? recent.indexOf(f.id) : recent.length);
    const sorted = [...own].sort((a, b) => rank(a) - rank(b) || folderPath(a, folders).localeCompare(folderPath(b, folders)));
    const moved = targets.flatMap(together);
    return [
      { id: "inbox", label: "Inbox", Icon: Inbox, run: () => act(moved, { state: "inbox" }, "Moved to Inbox", ICONS.inbox) },
      // Done and Delete, as everywhere else: the account needn't have listed those folders yet.
      {
        id: "archive",
        label: SYSTEM_LISTS.archive.label,
        keywords: "done all mail",
        Icon: SYSTEM_LISTS.archive.Icon,
        run: () => act(moved, { state: "done" }, "Marked as Done", ICONS.done),
      },
      {
        id: "trash",
        label: SYSTEM_LISTS.trash.label,
        keywords: "delete bin",
        Icon: SYSTEM_LISTS.trash.Icon,
        run: () => act(moved, { state: "deleted" }, "Moved to Trash", ICONS.trash),
      },
      ...(spamFolder
        ? [
            {
              id: "spam",
              label: SYSTEM_LISTS.spam.label,
              keywords: "junk",
              Icon: SYSTEM_LISTS.spam.Icon,
              run: () => act(moved, { folder: spamFolder.id }, "Moved to Spam", ICONS.spam),
            },
          ]
        : []),
      ...sorted.map((f) => ({
        id: `folder-${f.id}`,
        label: folderPath(f, folders),
        keywords: f.name,
        Icon: Folder,
        detail: recent.includes(f.id) ? "Recent" : undefined,
        run: () => moveTo(targets, f),
      })),
    ];
  }

  function moveTo(targets: Message[], f: FolderView) {
    rememberMove(f.account, f.id);
    act(targets.flatMap(together), { folder: f.id }, `Moved to ${f.name}`, ICONS.folder);
  }

  /** The Move picker's last line: a name that isn't a folder yet creates one, and moves there. */
  function createAndMove(targets: Message[], name: string): Command | null {
    if (!targets.length) return null;
    const account = targets[0].account;
    const exists = folders.some((f) => f.account === account && f.name.toLowerCase() === name.toLowerCase());
    if (exists) return null;
    return {
      id: "create",
      label: `Create folder “${name}” and move`,
      Icon: FolderPlus,
      run: async () => {
        try {
          const created = await createFolder(account, undefined, name);
          await reloadFolders();
          moveTo(targets, created);
        } catch (error) {
          showToast(`Couldn't create the folder: ${String(error)}`, undefined, ICONS.error);
        }
      },
    };
  }

  /** Spark's Change category: the sender's mail, now and later, in every account. */
  async function changeCategory(m: Message, category: Category) {
    const address = m.address;
    const set = async (to: Category | null) => {
      if (to) {
        setMessages((ms) =>
          ms.map((x) => (x.address.toLowerCase() === address.toLowerCase() ? { ...x, category: to } : x)),
        );
      }
      const before = await setSenderCategory(address, to);
      await reloadMessages();
      return before;
    };
    try {
      const before = await set(category);
      showToast(`${m.from || address} moved to ${CATEGORY_LABELS[category]}`, () => void set(before), ICONS.folder);
    } catch (error) {
      showToast(`Couldn't change the category: ${String(error)}`, undefined, ICONS.error);
      void reloadMessages();
    }
  }

  /** Spark's Group by Sender / Domain: their mail folds into one row in the Inbox; again to ungroup. */
  function toggleGroup(kind: "senders" | "domains", value: string, label: string) {
    const key = value.toLowerCase();
    const on = !grouped[kind].includes(key);
    const set = (add: boolean) =>
      setGrouped((g) => ({ ...g, [kind]: add ? [...g[kind].filter((x) => x !== key), key] : g[kind].filter((x) => x !== key) }));
    set(on);
    showToast(on ? `Emails from ${label} grouped` : `Emails from ${label} ungrouped`, () => set(!on), ICONS.folder);
  }

  function groupSender(m: Message) {
    toggleGroup("senders", m.address, m.from || m.address);
  }
  function groupDomain(m: Message) {
    const domain = decidableDomain(m.address);
    if (!domain) return showToast("Grouping by domain is only for custom domains", undefined, ICONS.folder);
    toggleGroup("domains", domain, domain);
  }

  /** A card's menu, and Settings → Appearance: its rows, or turning it off (People takes its mail). */
  function cardSettings(key: CardKey): CardSettings {
    return {
      setRows: (rows) => setCardPrefs((p) => ({ ...p, rows: { ...p.rows, [key]: rows } })),
      hide: () => {
        const set = (off: boolean) =>
          setCardPrefs((p) => ({ ...p, hidden: off ? [...p.hidden.filter((k) => k !== key), key] : p.hidden.filter((k) => k !== key) }));
        set(true);
        showToast(`${CARDS[key].label} card turned off`, () => set(false), ICONS.folder);
      },
    };
  }

  /** Asks for a day and time to snooze conversations until. */
  function askSnoozeTime(rows: Message[]) {
    setPickTime({
      title: "Snooze until",
      confirmLabel: "Snooze",
      initial: laterTime(presetOf("tomorrow")),
      run: (at) => batch((targets) => snoozeUntil(targets, at), rows),
    });
  }

  /** Done on emails reminders brought back: the reminders end, with Undo. */
  function finishReminders(emails: Message[]) {
    const count = emails.filter((m) => m.reminder).length;
    const undo = endReminders(emails);
    if (undo) showToast(count === 1 ? "Reminder done" : `${plural(count, "reminder")} done`, undo, ICONS.done);
  }

  /** Ends the reminders on `emails` (moving the cursor off them as they leave); returns the undo. */
  function endReminders(emails: Message[]): (() => void) | undefined {
    const due = new Map(emails.flatMap((m) => (m.reminder ? [[m.id, m.reminder] as const] : [])));
    if (!due.size) return undefined;
    const back = leaveFor(messages.map((m) => (due.has(m.id) ? { ...m, reminder: undefined } : m)));
    const set = (done: boolean) => {
      setMessages((ms) => ms.map((x) => (due.has(x.id) ? { ...x, reminder: done ? undefined : due.get(x.id) } : x)));
      Promise.all([...due.values()].map((r) => finishReminder(r.id, done))).then(reloadPlanned, (error) => {
        showToast(`Couldn't change the reminder: ${String(error)}`, undefined, ICONS.error);
        void reloadMessages();
      });
    };
    set(true);
    return () => {
      set(false);
      back();
    };
  }

  /** Snoozing an email a reminder brought back moves the reminder there instead; returns the undo. */
  function moveReminders(emails: Message[], at: number): (() => void) | undefined {
    const due = new Map(emails.flatMap((m) => (m.reminder ? [[m.id, m.reminder] as const] : [])));
    if (!due.size) return undefined;
    const back = leaveFor(messages.map((m) => (due.has(m.id) ? { ...m, reminder: undefined } : m)));
    const move = (to: (r: { id: number; at: number }) => number, removed: boolean) => {
      setMessages((ms) => ms.map((x) => (due.has(x.id) ? { ...x, reminder: removed ? undefined : due.get(x.id) } : x)));
      Promise.all([...due.values()].map((r) => setReminder(r.id, to(r)))).then(reloadPlanned, (error) => {
        showToast(`Couldn't move the reminder: ${String(error)}`, undefined, ICONS.error);
        void reloadMessages();
      });
    };
    move(() => at, true);
    return () => {
      // Back to when it came due: it's in the Inbox again on the next check.
      move((r) => r.at, false);
      back();
    };
  }

  /** Moves the cursor off emails that leave the open list once the mail is `after`; returns putting it back. */
  function leaveFor(after: Message[]) {
    const before = listedIds(model);
    const remaining = listedIds(buildList(after, view, sticky, ctx));
    const leaving = [...before].filter((id) => !remaining.has(id));
    const cursor = { selectedId, openId };
    moveOffMessages(leaving);
    return () => {
      if (!leaving.length) return;
      setSelectedId(cursor.selectedId);
      setOpenId(cursor.openId);
    };
  }

  /** Your sent email, as a new one to send again: its To and Cc, subject, text and files. */
  async function sendAgain(m: Message) {
    const content = await contentToAnswer(m, "send it again", () => void sendAgain(m));
    if (!content) return;
    const people = (list?: string[]) => (list ?? []).filter((r) => r !== "You").join(", ");
    const cc = people(content.cc);
    // Its formatting and pictures go again too.
    const html = content.html;
    const images = html && /["'=(]cid:/i.test(html) ? await getInlineImages(m.id).catch(() => ({})) : {};
    const bodyHtml = html ? resendHtml(html, images) : undefined;
    compose({
      title: "Send again",
      to: people(content.to),
      cc,
      showCc: cc !== "",
      subject: m.subject,
      body: bodyHtml !== undefined ? htmlText(bodyHtml) : content.text.join("\n\n"),
      bodyHtml,
      from: m.account,
      attachments: content.attachments.map((a, index) => ({
        kind: "message" as const,
        message: m.id,
        index,
        name: a.name,
        size: a.size,
      })),
    });
  }

  /**
   * A conversation's Auto-Label, as the user picks it: on its newest email that had one (else its
   * newest), and off the others, so it's in one label's list. Null takes it off.
   */
  async function setConversationLabel(thread: Message[], label: AutoLabel | null) {
    const labeled = thread.filter((x) => x.autoLabel);
    const keep = labeled[labeled.length - 1] ?? thread[thread.length - 1];
    try {
      for (const x of labeled) if (x !== keep || label === null) await setAutoLabel(x.id, null);
      if (label && keep) await setAutoLabel(keep.id, label);
    } catch (error) {
      showToast(`Couldn't change the label: ${String(error)}`, undefined, ICONS.error);
    }
    void reloadMessages();
  }

  /** Spark's "Auto-Draft a reply": AI writes one now, which shows in the conversation. */
  async function autoDraftFor(m: Message) {
    setDrafting((s) => new Set([...s, m.id]));
    try {
      await autoDraftReply(m.id);
      await reloadMessages();
    } catch (error) {
      showToast(String(error), undefined, ICONS.error, { duration: ERROR_TOAST_MS });
    } finally {
      setDrafting((s) => new Set([...s].filter((id) => id !== m.id)));
    }
  }

  /**
   * Sends a saved draft from the list, without opening it; Undo opens it in the composer.
   * Tracked (`track`): a conversation's window showing the draft waits for it to be queued.
   */
  async function sendDraftNow(m: Message) {
    // Open in the composer: what's there is the latest, and sending closes it.
    const open = composer.current;
    if (open.draft && (open.draft.draftId === m.id || draftIds.current.get(open.key) === m.id)) return send();
    // Open in another window's composer: that window comes forward, to send from there.
    if (await draftOpenElsewhere(m.id)) {
      showToast("This draft is open in another window");
      return;
    }
    let d: Draft;
    try {
      d = savedDraft(await openDraft(m.id));
    } catch (error) {
      showToast(`Couldn't open the draft: ${String(error)}`, undefined, ICONS.error);
      return;
    }
    if (![d.to, d.cc, d.bcc].some((field) => field.trim())) {
      openComposer(d, draftKey(d));
      showToast("Add at least one recipient");
      return;
    }
    // Placeholders nobody filled in: in the composer, where Send asks about them.
    if (hasPlaceholders(d.bodyHtml)) {
      openComposer(d, draftKey(d));
      showToast("This email has empty placeholders");
      return;
    }
    moveOffMessages([m.id]);
    let id: number;
    try {
      id = await sendMessage(outgoingDraft(d, d.sendLater?.at ?? Date.now() + undoMs));
    } catch (error) {
      showToast(`Couldn't send: ${String(error)}`, () => reopenComposer(d), ICONS.error, {
        actionLabel: "Edit",
        duration: ERROR_TOAST_MS,
      });
      return;
    }
    outbox.current.set(id, d);
    void reloadMessages();
    reloadPlanned();
    showToast(d.sendLater ? "Message scheduled" : "Message sent", () => unsend(id, d), ICONS.send, {
      duration: d.sendLater ? TOAST_MS : undoMs,
    });
  }

  // ---- Scheduled and Reminders ----

  /**
   * Changes a queued send, once a try of it under way is over: says it's waiting if that takes a moment, and why
   * if the change failed (undefined then).
   */
  async function changeQueued<T>(change: () => Promise<T>): Promise<T | undefined> {
    let waiting: number | undefined;
    const slow = window.setTimeout(() => {
      showToast("Waiting for the try under way to end…", undefined, ICONS.send, { duration: 5 * 60_000 });
      waiting = toastNow.current?.id;
    }, 600);
    try {
      return await change();
    } catch (error) {
      showToast(String(error), undefined, ICONS.error, { duration: ERROR_TOAST_MS });
      return undefined;
    } finally {
      window.clearTimeout(slow);
      if (waiting !== undefined) closeToast(waiting);
      reloadPlanned();
    }
  }

  /** Says what became of a queued send a change found gone, unless the notice of its failure does. */
  function sayGone(change: QueuedChange) {
    const notice = goneNotice(change);
    if (notice !== undefined) showToast(notice, undefined, ICONS.send);
  }

  /**
   * Takes a queued send into Drafts exactly as queued, then opens it there: nothing is lost if editing stops. One the
   * server refused for good meanwhile is in Drafts too, and opens there.
   */
  async function editQueued(outboxId: number) {
    const change = await changeQueued(() => unscheduleSend(outboxId, true));
    if (change === undefined) return;
    const id = draftLeft(change);
    if (id === undefined) return sayGone(change);
    await reloadMessages();
    void editDraft({ id });
  }

  const scheduledActions: ScheduledActions = {
    edit: (item) => editQueued(item.id),
    sendNow: async (item) => {
      const change = await changeQueued(() => rescheduleSend(item.id, null));
      if (change?.status === "moved") showToast("Sending now", undefined, ICONS.send);
      else if (change !== undefined) sayGone(change);
    },
    reschedule: (item) =>
      setPickTime({
        title: "Send at",
        confirmLabel: "Schedule",
        initial: item.sendAt,
        run: async (at) => {
          const change = await changeQueued(() => rescheduleSend(item.id, at));
          if (change?.status === "moved") showToast(`Scheduled for ${formatLater(at)}`, undefined, ICONS.send);
          else if (change !== undefined) sayGone(change);
        },
      }),
    cancel: async (item) => {
      const change = await changeQueued(() => unscheduleSend(item.id, false));
      if (change === undefined) return;
      if (change.status !== "takenBack") return sayGone(change);
      showToast("Not sent: it's in Drafts", undefined, ICONS.draft);
      void reloadMessages();
    },
  };

  const reminderActions: ReminderActions = {
    open: (r) => r.message !== undefined && openConversation(r.message),
    change: (r) =>
      setPickTime({
        title: "Remind me at",
        confirmLabel: "Remind me",
        initial: Math.max(r.remindAt, Date.now() + 60 * 60_000),
        run: async (at) => {
          await setReminder(r.id, at);
          showToast(`Reminder moved to ${formatLater(at)}`, undefined, ICONS.snooze);
          reloadPlanned();
          void reloadMessages();
        },
      }),
    remove: async (r) => {
      await setReminder(r.id, null).catch(() => {});
      showToast("Reminder removed", undefined, ICONS.snooze);
      reloadPlanned();
      void reloadMessages();
    },
  };

  /** Mutes a conversation, or unmutes a muted one: new mail in it doesn't notify. */
  function mute(m: Message) {
    const conversation = m.conversation ?? m.id;
    const set = (muted: boolean) => {
      setMessages((ms) => ms.map((x) => ((x.conversation ?? x.id) === conversation ? { ...x, muted } : x)));
      setMuted(conversation, muted).catch((error) => {
        showToast(`Couldn't change that: ${String(error)}`, undefined, ICONS.error);
        void reloadMessages();
      });
    };
    const muting = !m.muted;
    set(muting);
    showToast(muting ? "Conversation muted" : "Conversation unmuted", () => set(!muting), ICONS.mute);
  }

  /** Asks the mailing list to stop, after asking the user: its one-click link, an email to it, or its page. */
  function unsubscribeFrom(m: Message) {
    setConfirm({
      title: "Unsubscribe",
      message: `Stop getting emails like this from ${m.from || m.address}? spark-rs asks the mailing list to take you off it.`,
      confirmLabel: "Unsubscribe",
      danger: false,
      run: async () => {
        const result = await unsubscribe(m.id);
        if (result.kind === "page") {
          await openExternal(result.url);
          showToast("Finish unsubscribing on the list's page", undefined, ICONS.unsubscribe);
        } else if (result.kind === "email") {
          showToast(`Unsubscribe request sent to ${result.to}`, undefined, ICONS.unsubscribe);
        } else {
          showToast(`Unsubscribed from ${m.address}`, undefined, ICONS.unsubscribe);
        }
      },
    });
  }

  /** Opens a search for `query`, keeping the thread open; it counts as a recent search. */
  /** A search a conversation's own window asked for: shown here, over whatever covered the list. */
  function searchFromWindow(query: string) {
    setSettingsPage(null);
    setPalette(null);
    setCalendarOpen(false);
    closeSidebar();
    // In one column the reader covers the list the results are in.
    if (!twoColumns) setOpenId(null);
    searchFor(query);
  }

  function searchFor(query: string) {
    startHomeSession();
    setRecentSearches((r) => rememberSearch(r, query));
    changeView({ search: query, sub: null }, true);
  }

  function zoom(step: 1 | -1 | 0) {
    const level = stepZoom(step);
    showToast(`Zoom ${Math.round(level * 100)}%`, undefined, ICONS.zoom);
  }

  /** A card's or sub-list's rows, each with its conversation. */
  function doneAll(items: Message[]) {
    // Emails a reminder brought back end their reminders instead; they stay where they're kept.
    const reminded = items.filter((m) => own(m).reminder).map(own);
    const rows = items.filter((m) => m.state !== "done" && !own(m).reminder);
    if (!reminded.length && !rows.length) return;
    const undoReminders = endReminders(reminded);
    const undoMail = rows.length ? apply(rows.flatMap(together), () => ({ state: "done" })) : undefined;
    const count = reminded.length + rows.length;
    showToast(
      count === 1 ? "Marked as Done" : `${count} emails marked as Done`,
      () => {
        undoMail?.();
        undoReminders?.();
      },
      ICONS.done,
    );
  }

  /** "Sender accepted" for one sender's mail, "N senders accepted" for several. */
  const senders = (items: Message[], verb: string) => {
    const count = new Set(items.map((m) => m.address)).size;
    return count === 1 ? `Sender ${verb}` : `${plural(count, "sender")} ${verb}`;
  };

  /** Everyone waiting in New senders, after asking as Spark does. */
  function decideAll(items: Message[], decision: SenderDecision) {
    if (!items.length) return;
    const accepting = decision === "accepted";
    const run = () =>
      decide(forSenders(items, decision), senders(items, accepting ? "accepted" : "blocked"), ICONS[accepting ? "accept" : "block"]);
    setConfirm({
      title: accepting ? "Accept all" : "Block all",
      message: accepting
        ? "Do you want to accept all new senders?"
        : "Do you want to block all new senders? Their mail goes to the Blocked folder.",
      confirmLabel: accepting ? "Accept all" : "Block all",
      danger: !accepting,
      run,
    });
  }

  const gatekeeper: GatekeeperActions = {
    acceptSender: (items) => decide(forSenders(items, "accepted"), senders(items, "accepted"), ICONS.accept),
    blockSender: (items) => decide(forSenders(items, "blocked"), senders(items, "blocked"), ICONS.block),
    acceptAll: (items) => decideAll(items, "accepted"),
    blockAll: (items) => decideAll(items, "blocked"),
  };

  const snoozeMenu = (m: Message) => setPalette({ kind: "snooze", targetIds: [m.id] });

  /**
   * A row dragged onto a sidebar list: its conversation filed in a folder, or
   * done, trashed or back in the Inbox; or the email pinned.
   */
  function dropMessage(dragged: Message, key: ListKey, account?: number) {
    const m = messages.find((x) => x.id === dragged.id);
    const patch = m && dropPatch(m, key, folders, account);
    if (!m || !patch) return;
    const moved = together(m);
    if (isDraft(m, folders)) discardListed([m]);
    else if (key === "spam") act(moved, patch, "Moved to Spam", ICONS.spam);
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

  /**
   * A draft back in the composer (Undo, Edit), unless another window has opened its saved copy
   * meanwhile: it's edited there, and that window comes forward.
   */
  async function reopenComposer(d: Draft) {
    if (d.draftId !== undefined && (await draftOpenElsewhere(d.draftId))) {
      showToast("This draft is open in another window");
      return;
    }
    openComposer(d);
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
    showToast(`Couldn't save the draft: ${String(error)}`, () => reopenComposer(d), ICONS.error, {
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

  // The saved draft the composer has open (each save says which, as one can replace it), so
  // that no other window opens it too and saves its own copy over this one's.
  const editingId = draft ? (draftIds.current.get(composerKey) ?? draft.draftId ?? null) : null;
  useEffect(() => editingDraft(editingId), [editingId]);

  /**
   * A draft from Drafts, in the composer. Already open there, it stays as it is; open in
   * another window's, that window comes forward.
   */
  async function editDraft(m: Pick<Message, "id">) {
    setSelectedId(m.id);
    const open = composer.current;
    if (open.draft && (draftIds.current.get(open.key) ?? open.draft.draftId) === m.id) return;
    if (await draftOpenElsewhere(m.id)) {
      showToast("This draft is open in another window");
      return;
    }
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
  function discardLater(ids: number[], undo?: () => unknown, text = "Draft discarded") {
    if (!ids.length) {
      if (undo) showToast(text, undo, ICONS.trash);
      return;
    }
    setDiscarding((s) => new Set([...s, ...ids]));
    const keep = () => setDiscarding((s) => new Set([...s].filter((x) => !ids.includes(x))));
    const discard = () =>
      Promise.all(ids.map((id) => discardDraft(id))).then(
        () => reloadMessages().then(keep),
        (error) => {
          keep();
          void reloadMessages();
          showToast(`Couldn't delete the draft: ${String(error)}`, undefined, ICONS.error);
        },
      );
    const timer = window.setTimeout(() => {
      pendingDiscards.current.delete(timer);
      void discard();
    }, TOAST_MS);
    // A window closing now deletes them at once, rather than never.
    pendingDiscards.current.set(timer, discard);
    showToast(
      text,
      () => {
        window.clearTimeout(timer);
        pendingDiscards.current.delete(timer);
        keep();
        return undo?.();
      },
      ICONS.trash,
    );
  }

  /** Move to Trash on drafts in the list. */
  function discardListed(ms: Message[]) {
    const ids = ms.map((m) => m.id);
    moveOffMessages(ids);
    discardLater(ids, undefined, ids.length === 1 ? "Draft discarded" : `${plural(ids.length, "draft")} discarded`);
  }

  /** The composer's Delete Draft: it closes, and its saved draft goes too. Undo brings both back. */
  async function discardComposer() {
    if (!draft) return;
    stopWriting();
    const closed = draft;
    const session = composerKey;
    setDraft(null);
    const id = await savedDraftId(closed, session);
    discardLater(id === undefined ? [] : [id], draftHasContent(closed) ? () => reopenComposer({ ...closed, draftId: id }) : undefined);
  }

  /** From the account the Inbox is narrowed to, else the first. */
  /** From the account the Inbox is narrowed to, else the first; with the account's automatic Cc and Bcc. */
  function compose(seed: Partial<Draft> = {}) {
    const from = seed.from ?? (view.account !== "all" ? view.account : (accounts[0]?.id ?? 0));
    openComposer(withAutoRecipients({ ...emptyDraft(from), ...seed }, from));
  }

  /**
   * The draft from `from`, with that account's automatic Cc and Bcc in place
   * of the ones the draft got from its account before; what was typed stays.
   */
  function withAutoRecipients(d: Draft, from: AccountId): Draft {
    const address = (entry: string) => parseEntry(entry).address.toLowerCase();
    const swap = (field: string, before: string[], addresses: Record<string, string>) => {
      const gone = new Set(before.map(address));
      const kept = splitAddresses(field).filter((e) => !gone.has(address(e)));
      const have = new Set(kept.map(address));
      const added = splitAddresses(addresses[String(from)] ?? "").filter((e) => !have.has(address(e)));
      return { field: joinAddresses([...kept, ...added]), added };
    };
    const cc = swap(d.cc, d.auto?.cc ?? [], appPrefs.autoCc);
    const bcc = swap(d.bcc, d.auto?.bcc ?? [], appPrefs.autoBcc);
    return {
      ...d,
      from,
      cc: cc.field,
      bcc: bcc.field,
      showCc: d.showCc || cc.field !== "",
      showBcc: d.showBcc || bcc.field !== "",
      auto: { cc: cc.added, bcc: bcc.added },
    };
  }

  const replySubject = (m: Message) => (/^re:/i.test(m.subject) ? m.subject : `Re: ${m.subject}`);

  const fromYou = (m: Message) => isFromYou(m, accounts.find((a) => a.id === m.account));

  /** To Reply-To (or From); on sent mail, follow up with its recipients. */
  function reply(m: Message) {
    compose({
      title: "Reply",
      to: replyRecipients(m, accounts.find((a) => a.id === m.account)?.email).to.join(", "),
      subject: replySubject(m),
      from: m.account,
      replyTo: m.id,
      doneAfter: appPrefs.sendAndDone,
    });
    withReplied(m);
  }

  /**
   * Spark's Attachments with Replies: the files of the email being answered
   * go in the reply too, always or when you say so. They're added once the
   * list is read, unless the composer has moved on to another email.
   */
  function withReplied(m: Message) {
    if (appPrefs.replyAttachments === "never" || !m.attachment) return;
    // The reply just opened; another composer by then isn't this one's to change.
    const session = composer.current.key;
    const current = () => composer.current.key === session && composer.current.draft !== null;
    void loadContent(m.id).then((content) => {
      if (!current()) return;
      const files = (content?.attachments ?? []).map((a, index) => ({
        kind: "message" as const,
        message: m.id,
        index,
        name: a.name,
        size: a.size,
      }));
      if (!files.length) return;
      const add = () =>
        setDraft((d) => {
          if (!d || !current() || d.replyTo !== m.id || d.forward) return d;
          const fresh = files.filter(
            (f) => !d.attachments.some((a) => a.kind === "message" && a.message === f.message && a.index === f.index),
          );
          return { ...d, attachments: [...d.attachments, ...fresh] };
        });
      if (appPrefs.replyAttachments === "always") return add();
      setConfirm({
        title: files.length === 1 ? "Include the attachment?" : "Include the attachments?",
        message: `The email you're replying to has ${plural(files.length, "attachment")}: ${files.map((f) => f.name).join(", ")}.`,
        confirmLabel: "Include",
        cancelLabel: "Don't Include",
        danger: false,
        session,
        run: add,
      });
    }, (error) => {
      // Older mail that couldn't be downloaded: its files would be left out without a word.
      if (!current()) return;
      showToast(`Couldn't add the attachments: ${String(error)}`, () => withReplied(m), ICONS.error, {
        actionLabel: "Try again",
        duration: ERROR_TOAST_MS,
      });
    });
  }

  /** A quick-reply chip: a reply your AI provider writes, or without one set up, a stock line. */
  function quickReply(m: Message, chip: QuickReply) {
    reply(m);
    writeWithAi({ type: "quickReply", reply: chip });
  }

  /** The email a draft replies to or forwards, and which, for AI. */
  function draftReference(d: Draft): Reference | null {
    if (d.replyTo === undefined) return null;
    if (d.forward) return { id: d.replyTo, relation: "forward" };
    const m = messages.find((x) => x.id === d.replyTo);
    return { id: d.replyTo, relation: m && fromYou(m) ? "followUp" : "reply" };
  }

  /**
   * Runs a +AI request in the open composer. A draft streams in place of the
   * empty body; a rewrite replaces the request's range once its text starts
   * arriving. If it fails, the body goes back to how it was.
   */
  function writeWithAi(request: AiRequest) {
    const { draft: d, key: session } = composer.current;
    if (!d) return;
    const before = { body: d.body, bodyHtml: d.bodyHtml };
    // The text as the editor reads it, which the selection's offsets count in: a draft
    // reopened from the server keeps the list markers of its text version in `body`.
    const bodyText = d.bodyHtml === undefined ? d.body : htmlText(d.bodyHtml);
    // All of it is all of the editor's text, whatever `body`'s length.
    const range: [number, number] | null =
      "range" in request
        ? request.range[0] === 0 && request.range[1] >= d.body.length
          ? [0, bodyText.length]
          : request.range
        : null;
    const [head, text, tail] = range ? splitAround(bodyText, range) : ["", "", ""];
    // What's rewritten, as offsets in that text: the rest keeps its formatting and pictures.
    const span: [number, number] = [head.length, bodyText.length - tail.length];
    // A new draft goes above any pictures already there.
    const pictures = d.bodyHtml !== undefined && /<img\b/i.test(d.bodyHtml) ? d.bodyHtml : "";
    const action: ComposeAction =
      request.type === "rewrite"
        ? { type: "rewrite", text, how: request.how }
        : request.type === "refine"
          ? { type: "refine", text, instructions: request.instructions }
          : request;
    const run = ++writingRun.current;
    const base = { composer: session, request, before };
    const settle = (next: Writing | null) => setWriting(next && { ...base, ...next });
    const show = (written: string) =>
      setDraft(
        (d) =>
          d && {
            ...d,
            body: head + written + tail,
            bodyHtml:
              before.bodyHtml === undefined
                ? undefined
                : "range" in request
                  ? replaceText(before.bodyHtml, span, written)
                  : textToHtml(written) + pictures,
          },
      );
    const fail = (next: Writing) => {
      setDraft((d) => d && { ...d, ...before });
      settle(next);
    };
    let written = "";
    const onEvent = (event: AiEvent) => {
      if (writingRun.current !== run) return;
      switch (event.event) {
        case "delta":
          written += event.data.text;
          return show(written.trimStart());
        case "done": {
          show(written.trim());
          const { truncated } = event.data;
          // Replaced text can come back with Undo; a new draft just stays.
          if (!text && !truncated) return settle(null);
          return settle({ status: "done", message: truncated ? "Cut off at the length limit" : "Rewritten" });
        }
        case "notConfigured":
          if (request.type === "quickReply") {
            show(QUICK_REPLIES.find((q) => q.key === request.reply)!.text);
            return settle(null);
          }
          return fail({ status: "setup", message: event.data.message });
        case "refused":
          return fail({ status: "error", message: `${aiProvider} declined this request.` });
        case "error":
          return fail({ status: "error", message: event.data.message });
      }
    };
    settle({ status: "writing" });
    if (!text) show("");
    const signed = d.signature !== null && !d.signatureHidden;
    composeWithAi(session, action, d.from, draftReference(d), signed, onEvent).catch((error) =>
      onEvent({ event: "error", data: { message: String(error) } }),
    );
  }

  /** Stops AI writing in the composer, keeping what it wrote so far. */
  function stopWriting() {
    writingRun.current++;
    if (writing?.status === "writing") void cancelCompose(writing.composer);
    setWriting(null);
  }

  /** The Stop button: like stopWriting, but a stopped rewrite can still be undone. */
  function stopAi() {
    if (writing?.status !== "writing") return;
    stopWriting();
    if ("range" in writing.request) setWriting({ ...writing, status: "done", message: "Stopped" });
  }

  function undoAi() {
    if (!writing) return;
    const { before } = writing;
    setDraft((d) => d && { ...d, ...before });
    setWriting(null);
  }

  /**
   * The content of an email being answered, forwarded or sent again, or `null` once the user is told why it can't be
   * had, with Try again (`retry`). Older mail downloads its body first: offline, a forward would quote only its
   * preview and go without its files, Reply All would leave out its Cc, and Send Again would send only the preview.
   */
  async function contentToAnswer(m: Message, what: string, retry: () => void) {
    try {
      return await loadContent(m.id);
    } catch (error) {
      showToast(`Couldn't ${what}: ${String(error)}`, retry, ICONS.error, {
        actionLabel: "Try again",
        duration: ERROR_TOAST_MS,
      });
      return null;
    }
  }

  /**
   * The sender plus everyone else on To and Cc, minus this account's own
   * address. On an email you sent, its own To and Cc.
   */
  async function replyAll(m: Message) {
    const content = await contentToAnswer(m, "reply to all", () => void replyAll(m));
    if (!content) return;
    const recipients = replyRecipients(
      // The refreshed list owns Reply-To; the cached body may predate a sync.
      { ...m, to: content.to, cc: content.cc },
      accounts.find((a) => a.id === m.account)?.email,
      true,
    );
    const to = recipients.to.join(", ");
    const cc = recipients.cc.join(", ");
    compose({
      title: "Reply all",
      to,
      cc,
      showCc: cc !== "",
      subject: replySubject(m),
      from: m.account,
      replyTo: m.id,
      doneAfter: appPrefs.sendAndDone,
    });
    withReplied(m);
  }

  async function forward(m: Message) {
    const content = await contentToAnswer(m, "forward", () => void forward(m));
    if (!content) return;
    const text = content.text;
    const to = content.to.filter((r) => r !== "You");
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
      // Below the note and the signature, as Spark lays it out.
      quote: `${header.join("\n")}\n\n${text.join("\n\n")}`,
      from: m.account,
      replyTo: m.id,
      forward: true,
      // The original's files go along; they're fetched from the server when it's sent.
      attachments: content.attachments.map((a, index) => ({
        kind: "message" as const,
        message: m.id,
        index,
        name: a.name,
        size: a.size,
      })),
    });
  }

  /**
   * Closes the composer, keeping its draft in Drafts (one emptied out is deleted there). False if
   * saving it failed: `draftError` offers it back, and with `keep` it's back in the composer too,
   * as a conversation's own window keeps it in view, and so does quitting.
   */
  async function closeComposer(keep = THREAD_WINDOW !== null): Promise<boolean> {
    if (!draft) return true;
    stopWriting();
    const closed = draft;
    const session = composerKey;
    const unchanged = draftKey(closed) === draftSaved;
    setDraft(null);
    if (!draftHasContent(closed)) {
      const id = await savedDraftId(closed, session);
      discardLater(id === undefined ? [] : [id]);
      return true;
    }
    if (unchanged) {
      if (draftIds.current.has(session)) showToast("Draft saved", undefined, ICONS.draft);
      return true;
    }
    try {
      await storeDraft(closed, session);
    } catch (error) {
      draftError(error, closed);
      // Back as it was, saving to the copy it saved before, if any.
      if (keep) openComposer({ ...closed, draftId: draftIds.current.get(session) ?? closed.draftId });
      return false;
    }
    showToast("Draft saved", undefined, ICONS.draft);
    void reloadMessages();
    return true;
  }

  /**
   * Queues the message to go out when the undo window ends (or at the Send
   * Later time). Undo cancels it in the outbox and reopens the composer.
   */
  async function send(anyway = false) {
    if (!draft || writing?.status === "writing") return;
    // Cc or Bcc alone will do, as for a mailing list sent in Bcc.
    if (![draft.to, draft.cc, draft.bcc].some((field) => field.trim())) {
      showToast("Add at least one recipient");
      return;
    }
    // A template's placeholders nobody filled in: Spark asks, and they're left out.
    const unfilled = hasPlaceholders(draft.bodyHtml);
    if (unfilled && !anyway) {
      const asked = composerKey;
      setConfirm({
        title: "An empty placeholder",
        message: "This email has empty placeholders. Send it anyway?",
        confirmLabel: "Send Anyway",
        cancelLabel: "Edit Placeholder",
        danger: false,
        session: asked,
        // The email as it is then, if it's still the one asked about.
        run: () => {
          if (composer.current.key === asked) void track(latestSend.current(true));
        },
      });
      return;
    }
    const filled = unfilled ? dropPlaceholders(draft.bodyHtml!) : null;
    const session = composerKey;
    const repliedTo = draft.doneAfter ? messages.find((m) => m.id === draft.replyTo && m.state !== "done") : undefined;
    setDraft(null);
    const undoDone = repliedTo ? apply([repliedTo], () => ({ state: "done" })) : undefined;
    // A saved draft goes with it: deleted once it's sent, back in Drafts on Undo.
    const sent = {
      ...draft,
      ...(filled && { body: filled.text, bodyHtml: filled.html }),
      draftId: await savedDraftId(draft, session),
    };
    let id: number;
    try {
      id = await sendMessage(outgoingDraft(sent, sent.sendLater?.at ?? Date.now() + undoMs));
    } catch (error) {
      undoDone?.();
      openComposer(sent);
      showToast(`Couldn't send: ${String(error)}`, undefined, ICONS.error, { duration: ERROR_TOAST_MS });
      return;
    }
    outbox.current.set(id, sent);
    if (sent.draftId !== undefined) void reloadMessages();
    if (sent.sendLater) reloadPlanned();
    showToast(sent.sendLater ? "Message scheduled" : "Message sent", () => unsend(id, sent, undoDone), ICONS.send, {
      duration: sent.sendLater ? TOAST_MS : undoMs,
    });
  }

  async function unsend(id: number, sent: Draft, undoDone?: () => void) {
    const cancelled = await cancelSend(id).catch(() => false);
    if (!cancelled) {
      showToast("Already sent", undefined, ICONS.send);
      return;
    }
    outbox.current.delete(id);
    undoDone?.();
    await reopenComposer(sent);
    if (sent.draftId !== undefined) void reloadMessages();
    reloadPlanned();
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

  /** A calendar alert, or the meeting pop-up's calendar button: the day, and the event there. */
  function openCalendarAt(focus: { at: number; event?: number }) {
    setSettingsPage(null);
    setPalette(null);
    setCalendarFocus({ ...focus });
    setCalendarOpen(true);
    closeSidebar();
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

  function changePopOut(on: boolean) {
    savePopOut(on);
    setAlwaysPopOut(on);
  }

  function changePriorityLines(lines: number) {
    savePriorityLines(lines);
    setPriorityLines(lines);
  }

  // ---- keyboard ----

  function run(action: ActionId) {
    // A conversation's own window has no list to move in.
    if (THREAD_WINDOW !== null && LIST_ACTIONS.has(action)) return;
    switch (action) {
      case "openInNewWindow":
        if (THREAD_WINDOW === null && current) popOut(current);
        return;
      case "next":
        return moveCursor(1);
      case "prev":
        return moveCursor(-1);
      case "open":
        if (selectedId !== null && !pickedRows.length) openListed(selectedId);
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
      case "assistant":
        if (THREAD_WINDOW === null) setAssistantOpen((o) => !o);
        return;
      case "findInThread":
        if (openMessage) setFindRequest((n) => n + 1);
        return;
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
      case "selectAll":
        return setPicked(new Set(model.order));
      case "selectNext":
        return pickNext(1);
      case "selectPrev":
        return pickNext(-1);
      case "acceptAll":
        return gatekeeper.acceptAll(waitingInList());
      case "blockAll":
        return gatekeeper.blockAll(waitingInList());
      case "zoomIn":
        return zoom(1);
      case "zoomOut":
        return zoom(-1);
      case "zoomReset":
        return zoom(0);
    }
    if (batchRows.length && !homeShown) {
      switch (action) {
        case "done":
        case "trash":
        case "read":
        case "pin":
        case "priority":
        case "aside":
          return bulk[action]();
        case "snooze":
          return setPalette({ kind: "snooze", targetIds: batchRows.map((m) => m.id) });
        case "move":
          return openMove(batchRows);
      }
      return;
    }
    const m = current;
    if (!m) return;
    const waiting = m.screening && m.state === "inbox";
    switch (action) {
      // For an email from a new sender, E is the Gatekeeper's Accept and Done.
      case "done":
        return waiting ? actions.acceptAndDone(m) : actions.done(m);
      case "acceptSender":
        return waiting || m.state === "blocked" ? actions.accept(m) : undefined;
      case "acceptAndOpen":
        return waiting ? actions.acceptAndOpen(m) : undefined;
      case "acceptDomain":
        return waiting || m.state === "blocked" ? actions.acceptDomain(m) : undefined;
      case "blockSender":
        return m.state !== "blocked" ? actions.block(m) : undefined;
      case "blockDomain":
        return m.state !== "blocked" ? actions.blockDomain(m) : undefined;
      case "snooze":
        return snoozeMenu(m);
      case "move":
        return openMove([own(m)]);
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
        return summarizeConversation(m, SUMMARY_ACTIONS[action]);
    }
  }

  useEffect(() => {
    function onKey(e: KeyboardEvent) {
      if (e.defaultPrevented) return;
      // Zoom works everywhere, the composer and dialogs included.
      const zooming = matchShortcut(e);
      if (zooming === "zoomIn" || zooming === "zoomOut" || zooming === "zoomReset") {
        e.preventDefault();
        return run(zooming);
      }
      if (palette || settingsPage || isInsideOverlay(e.target)) return;
      // The assistant's panel is like a text field: its buttons don't take mail shortcuts.
      const typing = isTypingTarget(e.target) || (e.target instanceof Element && e.target.closest("[data-assistant]") !== null);
      if (isModShortcut(e) && !e.altKey && !e.shiftKey && e.key.toLowerCase() === "z") {
        if (!typing && toast?.undo && !toast.actionLabel) {
          e.preventDefault();
          undoToast();
        }
        return;
      }
      const action = matchShortcut(e);
      if (!action) return;
      // The Home Screen hides the list: only its own keys, or the open email's, act.
      if (homeShown && (openId === null ? !HOME_KEYS.has(action) : LIST_KEYS.has(action))) return;
      // Cmd/Ctrl+K, +F and +, also work from the search field; single keys never fire while typing.
      if (typing && action !== "command" && action !== "search" && action !== "settings" && action !== "assistant") return;
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

  /** Mail waiting in the Gatekeeper, in the accounts the Inbox shows. */
  function waitingInList() {
    return messages.filter(
      (x) => x.screening && x.state === "inbox" && (view.account === "all" || x.account === view.account),
    );
  }

  /** Accepting a new or blocked sender, or blocking one, and the same for everyone at their domain. */
  function gatekeeperCommands(m: Message): Command[] {
    const at = decidableDomain(m.address);
    const waiting = m.screening && m.state === "inbox";
    const commands: Command[] = [];
    if (waiting || m.state === "blocked") {
      commands.push({
        id: "accept-sender",
        label: "Accept Sender",
        keywords: "gatekeeper new unblock",
        Icon: ThumbsUp,
        shortcut: shortcutLabel("acceptSender"),
        run: () => actions.accept(m),
      });
    }
    if (waiting) {
      commands.push(
        {
          id: "accept-open",
          label: "Accept and Open",
          keywords: "gatekeeper new",
          Icon: ThumbsUp,
          shortcut: shortcutLabel("acceptAndOpen"),
          run: () => actions.acceptAndOpen(m),
        },
        {
          id: "accept-done",
          label: "Accept and Done",
          keywords: "gatekeeper new archive",
          Icon: ThumbsUp,
          shortcut: shortcutLabel("done"),
          run: () => actions.acceptAndDone(m),
        },
      );
    }
    if (at && (waiting || m.state === "blocked")) {
      commands.push({
        id: "accept-domain",
        label: `Accept all from ${at}`,
        keywords: "gatekeeper domain unblock",
        Icon: ThumbsUp,
        shortcut: shortcutLabel("acceptDomain"),
        run: () => actions.acceptDomain(m),
      });
    }
    if (m.state !== "blocked") {
      commands.push({
        id: "block-sender",
        label: "Block Sender",
        keywords: "gatekeeper spam",
        Icon: Ban,
        shortcut: shortcutLabel("blockSender"),
        run: () => actions.block(m),
      });
      if (at) {
        commands.push({
          id: "block-domain",
          label: `Block all from ${at}`,
          keywords: "gatekeeper spam domain",
          Icon: Ban,
          shortcut: shortcutLabel("blockDomain"),
          run: () => actions.blockDomain(m),
        });
      }
    }
    return commands;
  }

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
          ...(m.state === "snoozed"
            ? [{ id: "unsnooze", label: "Discard Snooze", keywords: "unsnooze inbox", Icon: Inbox, run: () => actions.unsnooze(m) }]
            : []),
          {
            id: "snooze-tomorrow",
            label: "Snooze until tomorrow",
            keywords: "later",
            Icon: Clock,
            detail: presetDetail(presetOf("tomorrow")),
            run: () => actions.snooze(m, presetOf("tomorrow")),
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
          ...(senderPriority(m) === null
            ? []
            : [
                {
                  id: "priority-sender",
                  label: senderPriority(m)
                    ? `Remove "${m.from.trim() || m.address}" from Priority senders`
                    : `Mark "${m.from.trim() || m.address}" as priority sender`,
                  keywords: "important vip contact person",
                  Icon: Zap,
                  run: () => prioritize([m.address], !senderPriority(m)),
                },
              ]),
          {
            id: "trash",
            label: m.state === "deleted" ? "Delete Forever" : "Move to Trash",
            keywords: "delete remove permanently",
            Icon: Trash2,
            shortcut: shortcutLabel("trash"),
            run: () => actions.trash(m),
          },
          {
            id: "mute",
            label: m.muted ? "Unmute Conversation" : "Mute Conversation",
            keywords: "silence notifications quiet",
            Icon: m.muted ? Bell : BellOff,
            run: () => actions.mute(m),
          },
          ...(m.category === "newsletters"
            ? [{ id: "unsubscribe", label: "Unsubscribe", keywords: "newsletter list stop", Icon: MailX, run: () => actions.unsubscribe(m) }]
            : []),
          ...(fromYou(m) && !isDraft(m, folders)
            ? [{ id: "send-again", label: "Send Again", keywords: "resend new copy", Icon: Send, run: () => void sendAgain(m) }]
            : []),
          ...(isDraft(m, folders)
            ? [{ id: "send-draft", label: "Send Draft", keywords: "send now", Icon: Send, run: () => void track(sendDraftNow(m)) }]
            : []),
          {
            id: "move",
            label: "Move to Folder…",
            keywords: "file folder label",
            Icon: FolderInput,
            shortcut: shortcutLabel("move"),
            run: () => actions.move(m),
          },
          ...(Object.keys(CATEGORY_LABELS) as Category[])
            .filter((c) => c !== m.category)
            .map((c) => ({
              id: `category-${c}`,
              label: `Change Category to ${CATEGORY_LABELS[c]}`,
              detail: m.address,
              keywords: "type sender personal",
              Icon: Tags,
              run: () => actions.changeCategory(m, c),
            })),
          {
            id: "group-sender",
            label: actions.grouping(m).sender ? "Ungroup Emails from Sender" : "Group Emails by Sender",
            detail: m.address,
            keywords: "fold together",
            Icon: Group,
            run: () => actions.groupSender(m),
          },
          ...(actions.grouping(m).domain
            ? [
                {
                  id: "group-domain",
                  label: actions.grouping(m).domainGrouped ? "Ungroup Emails from Domain" : "Group Emails by Domain",
                  detail: actions.grouping(m).domain,
                  keywords: "fold together company",
                  Icon: Group,
                  run: () => actions.groupDomain(m),
                },
              ]
            : []),
          ...(THREAD_WINDOW === null
            ? [
                {
                  id: "pop-out",
                  label: "Open in a New Window",
                  keywords: "window separate pop out",
                  Icon: AppWindow,
                  shortcut: shortcutLabel("openInNewWindow"),
                  run: () => popOut(m),
                },
              ]
            : []),
          ...(openMessage
            ? [
                {
                  id: "find-in-thread",
                  label: "Find in Conversation",
                  keywords: "search text words",
                  Icon: TextSearch,
                  shortcut: shortcutLabel("findInThread"),
                  run: () => setFindRequest((n) => n + 1),
                },
              ]
            : []),
          {
            id: "search-sender",
            label: "Search Emails by Sender",
            detail: m.address,
            keywords: "from find",
            Icon: Search,
            run: () => actions.searchSender(m),
          },
          {
            id: "spam",
            label: inSpam(m) ? "Not Spam" : "Move to Spam",
            keywords: "junk",
            Icon: inSpam(m) ? ShieldCheck : ShieldAlert,
            run: () => actions.spam(m),
          },
          ...(inSpam(m)
            ? [
                {
                  id: "delete-forever",
                  label: "Delete Forever",
                  keywords: "delete remove permanently",
                  Icon: Trash2,
                  run: () => actions.deleteForever(m),
                },
              ]
            : []),
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
            run: () => summarizeConversation(m, SUMMARY_ACTIONS[action]),
          })),
        ]
      : [];
    const pickedSenders = pickedMessages.filter((x) => senderPriority(x) !== null).map((x) => x.address);
    const allPrioritySenders = pickedSenders.every((a) => prioritySet.has(a.toLowerCase()));
    const selectionCommands: Command[] =
      pickedMessages.length > 1 && pickedSenders.length
        ? [
            {
              id: "priority-senders",
              label: allPrioritySenders ? "Remove senders from Priority senders" : "Mark senders as priority senders",
              keywords: "important vip contacts people",
              Icon: Zap,
              run: () => prioritize(pickedSenders, !allPrioritySenders),
            },
          ]
        : [];
    // A conversation's own window acts on that conversation only: it has no list.
    if (THREAD_WINDOW !== null) return [...messageCommands, ...(m ? gatekeeperCommands(m) : [])];
    const waiting = waitingInList();
    return [
      ...messageCommands,
      ...(m ? gatekeeperCommands(m) : []),
      // The Home Screen hides the list, so nothing acts on all of it, or on a selection in it.
      ...(homeShown ? [] : selectionCommands),
      ...(waiting.length && !homeShown
        ? [
            {
              id: "accept-all",
              label: "Accept all new senders",
              keywords: "gatekeeper",
              Icon: ThumbsUp,
              shortcut: shortcutLabel("acceptAll"),
              run: () => gatekeeper.acceptAll(waiting),
            },
            {
              id: "block-all",
              label: "Block all new senders",
              keywords: "gatekeeper",
              Icon: ThumbsDown,
              shortcut: shortcutLabel("blockAll"),
              run: () => gatekeeper.blockAll(waiting),
            },
          ]
        : []),
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
      ...(homeShown
        ? []
        : [
            {
              id: "select-all",
              label: "Select All",
              keywords: "pick choose emails batch",
              Icon: CheckCheck,
              shortcut: shortcutLabel("selectAll"),
              run: () => setPicked(new Set(model.order)),
            },
            { id: "mark-all-read", label: "Mark All as Read", keywords: "seen", Icon: MailOpen, run: () => markAll(false) },
            { id: "mark-all-unread", label: "Mark All as Unread", keywords: "seen", Icon: Mail, run: () => markAll(true) },
          ]),
      ...(emptiable(view.folder, view.account).length && !searching && !homeShown
        ? [
            {
              id: "empty",
              label: `Empty ${model.title}`,
              keywords: "delete all permanently trash spam junk folder",
              Icon: Trash2,
              run: () => emptyList(view.folder, view.account),
            },
          ]
        : []),
      { id: "go-inbox", label: "Go to Inbox", keywords: "folder", Icon: Inbox, run: () => selectList("inbox") },
      ...(homeSettings.enabled
        ? [
            homeShown
              ? { id: "check-inbox", label: "Check Inbox", keywords: "home screen session", Icon: Inbox, run: checkInbox }
              : { id: "home-screen", label: "Home Screen", keywords: "home focus away welcome", Icon: House, run: goHome },
          ]
        : []),
      ...SYSTEM_ORDER.map((key) => ({
        id: `go-${key}`,
        label: `Go to ${SYSTEM_LISTS[key].label}`,
        keywords: "folder",
        Icon: SYSTEM_LISTS[key].Icon,
        run: () => selectList(key),
      })),
      ...sidebarKeys(layout)
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
      // The list's own views, which the Home Screen hides.
      ...(homeShown
        ? []
        : [
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
          ]),
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
        id: "home-settings",
        label: "Home Screen settings",
        keywords: "home focus schedule inactivity timeout away",
        Icon: House,
        run: () => openSettings("home"),
      },
      {
        id: "notification-settings",
        label: "Notification settings",
        keywords: "notify alerts sound tray background close startup sign in login",
        Icon: Bell,
        run: () => openSettings("notifications"),
      },
      {
        id: "signature-settings",
        label: "Signature settings",
        keywords: "signatures sign off email footer composer",
        Icon: Signature,
        run: () => openSettings("signatures"),
      },
      {
        id: "agent-settings",
        label: "Agent settings",
        keywords: "ai agents claude code codex hermes mcp cli access",
        Icon: Bot,
        run: () => openSettings("agents"),
      },
      ...(THREAD_WINDOW === null
        ? [
            {
              id: "assistant",
              label: "AI Assistant",
              keywords: "ask ai chat question",
              Icon: Sparkles,
              shortcut: shortcutLabel("assistant"),
              run: () => openAssistant(),
            },
          ]
        : []),
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
      { id: "zoom-in", label: "Zoom In", keywords: "bigger larger", Icon: ZoomIn, shortcut: shortcutLabel("zoomIn"), run: () => zoom(1) },
      { id: "zoom-out", label: "Zoom Out", keywords: "smaller", Icon: ZoomOut, shortcut: shortcutLabel("zoomOut"), run: () => zoom(-1) },
      { id: "zoom-reset", label: "Actual Size", keywords: "zoom reset", Icon: Scaling, shortcut: shortcutLabel("zoomReset"), run: () => zoom(0) },
    ];
  }

  const snoozeTargets = palette?.kind === "snooze" ? palette.targetIds.flatMap((id) => byId.get(id) ?? []) : [];
  const moveTargets = palette?.kind === "move" ? palette.targetIds.flatMap((id) => byId.get(id) ?? []) : [];
  const snoozeCommands: Command[] = snoozeTargets.length
    ? [
        ...(snoozeTargets.every((m) => m.state === "snoozed")
          ? [
              {
                id: "unsnooze",
                label: "Discard Snooze",
                Icon: Inbox,
                run: () =>
                  batch((rows) => act(rows.flatMap(together), { state: "inbox" }, "Snooze discarded", ICONS.inbox), snoozeTargets),
              },
            ]
          : []),
        ...laterPresets().map((p) => ({
          id: p.key,
          label: p.label,
          Icon: Clock,
          detail: presetDetail(p),
          run: () => batch((rows) => snoozeRows(rows, p), snoozeTargets),
        })),
        {
          id: "custom",
          label: "Pick date & time…",
          keywords: "custom date day",
          Icon: Clock,
          run: () => askSnoozeTime(snoozeTargets),
        },
      ]
    : [];

  // ---- layout ----

  // Stable, so typing in the composer doesn't re-render every (memoized) row.
  const rows = useLatest<RowActions>({
    open: (id) => {
      clearPicked();
      if (view.search?.trim()) setRecentSearches((r) => rememberSearch(r, view.search!));
      openListed(id);
    },
    pick: pickRow,
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
    setAutoLabel: (thread, label) => void setConversationLabel(thread, label),
    autoDraft: (m) => void autoDraftFor(m),
    drafting: (id) => drafting.has(id),
    draftOf: (id) => (discarding.has(id) ? undefined : messages.find((x) => x.id === id)),
    autoDrafts: {
      edit: (d) => void editDraft(d),
      send: (d) => void sendDraftNow(d),
      remove: (d) => discardLater([d.id]),
      choose: (d, index) =>
        chooseAutoDraftOption(d.id, index).then(
          () => reloadMessages(),
          (error) => showToast(`Couldn't change the draft: ${String(error)}`, undefined, ICONS.error),
        ),
    },
    // A conversation's own window doesn't open another.
    popOut: THREAD_WINDOW === null ? popOut : undefined,
    inSpam,
    senderPriority,
    prioritySender: (m) => prioritize([m.address], !senderPriority(m)),
    close: () => setOpenId(null),
    summarize,
    stopSummary,
    deleteSummary,
    summaryCopied: () => showToast("Summary Copied", undefined, ICONS.copy),
    openAiSettings: () => openSettings("ai"),
    openImageSettings: () => openSettings("appearance"),
    reply: (m) => reply(m),
    quickReply,
    replyAll: (m) => void replyAll(m),
    forward: (m) => void forward(m),
    openAttachment: async (m, index) => {
      try {
        const opened = await openAttachment(m.id, index);
        if (opened.kind !== "risky") return;
        // Spark's question before a file that could run a program.
        setConfirm({
          title: `Are you sure you want to open ${opened.name}?`,
          message: "This file from an unidentified developer might harm your device.",
          confirmLabel: "Open Anyway",
          run: async () => void (await openAttachment(m.id, index, true)),
        });
      } catch (error) {
        fileError(error);
      }
    },
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
    print: (m, all) =>
      void readEmails(all ? conversation : [m]).then(
        async (emails) =>
          printEmails(
            m.subject || "No subject",
            await Promise.all(
              emails.map(async ({ message: x, header, content }) => ({
                header,
                html: content.html,
                text: content.text,
                remoteImages: remoteImagesAllowed(x),
                inlineImages:
                  content.html && /["'=(]cid:/i.test(content.html) ? await getInlineImages(x.id).catch(() => null) : null,
              })),
            ),
          ),
        fileError,
      ),
    copyMarkdown: (m, all) =>
      void readEmails(all ? conversation : [m])
        .then((emails) =>
          navigator.clipboard.writeText(
            emails.map(({ header, content }) => messageMarkdown(header, content.html, content.text)).join("\n\n---\n\n"),
          ),
        )
        .then(() => showToast("Copied as Markdown", undefined, ICONS.copy), fileError),
    copyAddresses: (m) =>
      void readEmails([m])
        .then(([{ header }]) => {
          // Everyone on it, as they'd go back in To: the sender, then To and Cc.
          const people = [header.from, ...header.to, ...header.cc];
          const seen = new Set<string>();
          const unique = people.filter((p) => {
            const address = parseEntry(p).address.toLowerCase();
            return address && !seen.has(address) && seen.add(address);
          });
          return navigator.clipboard.writeText(unique.join(", "));
        })
        .then(() => showToast("Email addresses copied", undefined, ICONS.copy), fileError),
    prefetchAttachments: (m) => {
      if (appPrefs.autoDownloadAttachments) void prefetchAttachments(m.id).catch(() => {});
    },
    saveEml: (m) =>
      void saveEml(m.id, m.subject || "Email").then(
        (saved) => saved && showToast("Email saved", undefined, ICONS.download),
        fileError,
      ),
  };

  /** Emails with their content, and who, when and what as Copy and Print show it. */
  async function readEmails(list: Message[]) {
    return Promise.all(
      list.map(async (x) => {
        const content = await loadContent(x.id);
        const header: MarkdownHeader = {
          from: x.from && x.from !== x.address ? `${x.from} <${x.address}>` : x.address,
          to: (content.to ?? []).filter((r) => r !== "You"),
          cc: (content.cc ?? []).filter((r) => r !== "You"),
          date: formatFullDate(x.timestamp),
          subject: x.subject || "No subject",
        };
        return { message: x, header, content };
      }),
    );
  }

  /**
   * The floating composer moves left of the assistant's panel when there's room for it there, and
   * goes over the panel when there isn't.
   */
  const besideAssistant = windowWidth - assistantSpace - 32;
  const composerBesideAssistant =
    assistantSpace && besideAssistant >= COMPOSER_MIN_BESIDE
      ? { right: assistantSpace + 16, maxWidth: besideAssistant }
      : undefined;

  /** Opens the assistant's panel, asking `question` if there is one. */
  function openAssistant(question?: string) {
    setAssistantOpen(true);
    if (question) setAssistantAsk({ text: question, nonce: Date.now() });
  }

  /** An email the assistant cited: in the reader if it's here, else in a window of its own. */
  function openAssistantEmail(email: EmailRef) {
    setCalendarOpen(false);
    if (messages.some((m) => m.id === email.id)) openConversation(email.id);
    else void openThreadWindow(email.id, email.subject || "No subject").catch(fileError);
  }

  /** A draft the assistant wrote, in the composer to edit and send. */
  function openAssistantDraft(d: DraftCard) {
    compose({
      title: d.replyTo === undefined ? "New message" : "Reply",
      to: d.to,
      cc: d.cc,
      showCc: d.cc !== "",
      subject: d.subject,
      body: d.body,
      from: d.account,
      replyTo: d.replyTo,
      doneAfter: d.replyTo !== undefined && appPrefs.sendAndDone,
    });
  }

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
    settings: () => openSettings(updates.view?.available ? "about" : lastSettingsPage.current),
    togglePicker,
    closePicker: () => setPickerOpen(false),
    newSmartFolder,
    createFolder: newFolder,
    renameList,
    deleteList,
    drop: dropMessage,
    markAllRead: (key) => markAll(false, key),
    canEmpty: (key) => emptiable(key).length > 0,
    empty: (key) => emptyList(key),
    calendar: () => {
      setCalendarFocus(undefined);
      setCalendarOpen(true);
      closeSidebar();
    },
    home: goHome,
  };

  // A conversation's own window shows its conversation, even while an Undo keeps it open after Done.
  const windowShown = THREAD_WINDOW === null ? null : (openMessage ?? (toast?.undo ? ownMessage : undefined) ?? null);
  const windowConversation =
    windowShown && windowShown !== openMessage ? conversationOf(conversations, windowShown) : conversation;
  const showList = twoColumns || !openMessage;
  const showReader = twoColumns || openMessage !== null;
  const readerLeft = THREAD_WINDOW !== null ? 0 : sidebarSpace + (twoColumns && !homeShown ? listWidthUsed : 0);

  return (
    <AccountsContext value={accounts}>
      <div className="flex h-full bg-base-100 text-base-content">
        {THREAD_WINDOW === null ? (
          <>
        <Sidebar
          mode={sidebarMode}
          width={sidebarWidth}
          layout={layout}
          folders={folders}
          current={view.folder}
          account={view.account}
          counts={counts}
          syncing={syncing}
          updateAvailable={Boolean(updates.view?.available)}
          pickerOpen={pickerOpen}
          demo={demo}
          calendarOpen={calendarOpen}
          home={homeSettings.enabled ? { open: homeShown } : null}
          actions={sidebarActions}
        />

        <main
          className="relative flex min-w-0 flex-1"
          style={assistantSpace ? { paddingRight: assistantSpace } : undefined}
        >
          <DragStrip />
          <CaptionButtons />
          {calendarOpen && (
            <CalendarScreen demo={isDemoMode(accounts)} focus={calendarFocus} onClose={() => setCalendarOpen(false)} />
          )}
          {homeShown && (
            <HomeScreen
              settings={homeSettings}
              session={homeSession}
              name={accounts.find((a) => !a.demo)?.name ?? accounts[0]?.name ?? ""}
              counts={homeNew}
              items={homeItems}
              onCheckInbox={checkInbox}
              onNewEmails={showNewEmails}
              onFilter={changeHomeFilter}
              onOpen={(m) => {
                setPicked(new Set());
                openConversation(m.id);
              }}
              onDismiss={(m) => updateHome((s) => dismissFromHome(s, m.id))}
              onClearAll={() => updateHome((s) => clearHomeFeed(s, Date.now()))}
              onSearch={openSearch}
              onCompose={() => compose()}
            />
          )}
          {homeShown && openMessage && (
            // An email from the feed opens over the Home Screen, as in Spark.
            <div
              // Under the composer (z-50), so Reply and Forward write over it.
              className="absolute inset-0 z-40 flex justify-center bg-black/30 px-6 pb-6 pt-10"
              onClick={() => setOpenId(null)}
            >
              <section
                aria-label="Conversation"
                className="flex w-full max-w-[920px] flex-col overflow-hidden rounded-2xl bg-base-100 shadow-popup"
                onClick={(e) => e.stopPropagation()}
                onPointerDownCapture={() => (scrollPane.current = "reader")}
              >
                <ThreadView
                  key={openMessage.id}
                  message={asThread(openMessage, conversation)}
                  conversation={conversation}
                  unread={openedUnread}
                  minimize={minimize}
                  singleColumn
                  summary={summaries.get(openMessage.id)}
                  aiProvider={aiProvider}
                  actions={threadActions}
                  scrollRef={readerScroll}
                  findRequest={findRequest}
                />
              </section>
            </div>
          )}
          {showList && !calendarOpen && !homeShown && (
            <section
              aria-label="Message list"
              className={`relative flex min-w-0 flex-col pt-8 ${twoColumns ? "shrink-0 border-r border-hairline" : "flex-1"}`}
              style={twoColumns ? { width: listWidthUsed } : undefined}
              onPointerDownCapture={() => (scrollPane.current = "list")}
            >
              {isPlannedList(view.folder) && view.search === null ? (
                view.folder === "scheduled" ? (
                  <PlannedList kind="scheduled" items={inAccount(scheduled)} actions={scheduledActions} />
                ) : (
                  <PlannedList kind="reminders" items={inAccount(reminders)} actions={reminderActions} />
                )
              ) : (
                <MessageList
                  model={model}
                  view={view}
                  selectedId={selectedId}
                  openId={openId}
                  picked={picked}
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
                  cardSettings={cardSettings}
                  onOpenGatekeeper={() => changeView({ sub: { kind: "gatekeeper" } }, true)}
                  searchRef={searchRef}
                  demo={isDemoMode(accounts)}
                  onAddAccounts={() => openSettings("accounts")}
                  loading={loading}
                  onSaveSearch={(query) => setSavingSearch(query)}
                  scrollRef={listScroll}
                  older={older && { loading: loadingOlder, onNearEnd: reachBack }}
                  searchingServers={searchingServers}
                  onEmpty={emptiable(view.folder, view.account).length ? () => emptyList(view.folder, view.account) : undefined}
                  recentSearches={recentSearches}
                  onForgetSearch={(q) => setRecentSearches((r) => forgetSearch(r, q))}
                  searchScope={view.account === "all" ? null : (accounts.find((a) => a.id === view.account)?.label ?? null)}
                  onSearchAllAccounts={() => setView((v) => ({ ...v, account: "all" }))}
                />
              )}
              {!twoColumns && pickedRows.length > 0 && <BulkPanel compact selection={selection} actions={bulk} />}
              {twoColumns && (
                <Splitter
                  width={listWidthUsed}
                  onResize={(w) => setListWidth(clamp(w, LIST_MIN, Math.min(LIST_MAX, container - READER_MIN)))}
                />
              )}
            </section>
          )}
          {showReader && !calendarOpen && !homeShown && (
            <section
              aria-label="Conversation"
              className="flex min-w-0 flex-1 flex-col pt-8"
              onPointerDownCapture={() => (scrollPane.current = "reader")}
            >
              {twoColumns && pickedRows.length > 0 ? (
                <BulkPanel selection={selection} actions={bulk} />
              ) : openMessage ? (
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
                  findRequest={findRequest}
                />
              ) : (
                <EmptyReader />
              )}
            </section>
          )}
          {!assistantOpen && !draft && !homeShown && !calendarOpen && !pickedRows.length && windowWidth >= ASSISTANT_MIN_WIDTH && (
            <AssistantPill suggestions={assistantSuggestions(openMessage)} onAsk={openAssistant} />
          )}
          <AssistantPanel
            open={assistantOpen}
            ask={assistantAsk}
            screen={{ openEmail: openMessage?.id }}
            name={(accounts.find((a) => !a.demo)?.name ?? accounts[0]?.name ?? "").split(/\s+/)[0]}
            suggestions={assistantSuggestions(openMessage)}
            onClose={() => setAssistantOpen(false)}
            onOpenEmail={openAssistantEmail}
            onOpenDraft={openAssistantDraft}
            onOpenSettings={() => openSettings("ai")}
            onCopied={() => showToast("Copied", undefined, ICONS.copy)}
          />
        </main>
          </>
        ) : (
          // A conversation in a window of its own: the reader, filling it.
          <main className="relative flex min-w-0 flex-1 flex-col">
            <DragStrip />
            <CaptionButtons />
            <section
              aria-label="Conversation"
              className="flex min-h-0 flex-1 flex-col pt-8"
              onPointerDownCapture={() => (scrollPane.current = "reader")}
            >
              {windowShown ? (
                <ThreadView
                  key={windowShown.id}
                  message={asThread(windowShown, windowConversation)}
                  conversation={windowConversation}
                  unread={openedUnread}
                  minimize={minimize}
                  singleColumn={false}
                  summary={summaries.get(windowShown.id)}
                  aiProvider={aiProvider}
                  actions={threadActions}
                  scrollRef={readerScroll}
                  findRequest={findRequest}
                />
              ) : (
                <div className="grid flex-1 place-items-center text-[13px] text-text-2">
                  {threadLoaded ? "This email isn't here anymore." : "Loading…"}
                </div>
              )}
            </section>
          </main>
        )}

        {draft && (
          <Composer
            key={composerKey}
            draft={draft}
            saved={draftSaved === draftKey(draft)}
            full={composerFull}
            fullStyle={{ left: readerLeft, right: assistantSpace }}
            floatStyle={composerBesideAssistant}
            onChange={(patch) =>
              setDraft(
                (d) =>
                  d &&
                  (patch.from !== undefined && patch.from !== d.from
                    ? withAutoRecipients({ ...d, ...patch }, patch.from)
                    : { ...d, ...patch }),
              )
            }
            onToggleFull={() => setComposerFull((f) => !f)}
            onClose={() => void track(closeComposer())}
            onDiscard={() => void discardComposer()}
            onSend={() => void track(send())}
            onError={(text) => showToast(text, undefined, ICONS.error, { duration: ERROR_TOAST_MS })}
            onSignatureSettings={() => openSettings("signatures")}
            onTemplateFromDraft={() => {
              setNewTemplate({
                id: null,
                name: draft.subject.trim(),
                subject: draft.subject.trim(),
                html: draft.bodyHtml ?? textToHtml(draft.body),
              });
              openSettings("templates");
            }}
            onTemplateSettings={() => openSettings("templates")}
            ai={{
              writing: writing?.composer === composerKey ? writing : null,
              quickReplies: draftReference(draft)?.relation === "reply",
              run: writeWithAi,
              stop: stopAi,
              retry: () => writing && writeWithAi(writing.request),
              undo: undoAi,
              dismiss: () => setWriting(null),
              setUp: () => {
                setWriting(null);
                openSettings("ai");
              },
            }}
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
        {palette?.kind === "move" && (
          <CommandCenter
            key="move"
            title="Move to folder"
            placeholder="Folder name…"
            commands={moveCommands(moveTargets)}
            fallback={(name) => createAndMove(moveTargets, name)}
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
            title={
              snoozeTargets.length > 1
                ? `Snooze · ${conversationCount(snoozeTargets)}`
                : snoozeTargets.length
                  ? `Snooze · ${snoozeTargets[0].subject}`
                  : "Snooze"
            }
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
            updates={updates}
            newTemplate={newTemplate}
            onNewTemplateTaken={() => setNewTemplate(null)}
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
            popOut={alwaysPopOut}
            onPopOut={changePopOut}
            cards={cardPrefs}
            onCards={setCardPrefs}
            priorityLines={priorityLines}
            onPriorityLines={changePriorityLines}
            prioritySenders={prioritySenders}
            onPrioritize={prioritize}
            onAccountsChanged={() => void reloadAccounts()}
            gatekeeper={gatekeeperMode}
            onGatekeeper={changeGatekeeper}
            home={homeSettings}
            onHome={changeHomeSettings}
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

        {confirm && (
          <ConfirmDialog
            title={confirm.title}
            message={confirm.message}
            confirmLabel={confirm.confirmLabel}
            cancelLabel={confirm.cancelLabel}
            danger={confirm.danger}
            onConfirm={confirm.run}
            onClose={() => setConfirm(null)}
          />
        )}
        {pickTime && (
          <DateTimeDialog
            title={pickTime.title}
            confirmLabel={pickTime.confirmLabel}
            initial={pickTime.initial}
            onSubmit={pickTime.run}
            onClose={() => setPickTime(null)}
          />
        )}
        {certificate && (
          <CertificateDialog
            certificate={certificate.certificate}
            onTrust={() => trustCertificate(certificate.account, certificate.certificate)}
            onClose={() => setCertificate(null)}
          />
        )}

        {toast && <UndoToast toast={toast} left={THREAD_WINDOW !== null ? 16 : sidebarSpace + 16} onUndo={undoToast} />}
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
    if ((view.mode === "cards" && !view.sub) || view.sub?.kind === "gatekeeper" || view.sub?.kind === "new") return null;
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
