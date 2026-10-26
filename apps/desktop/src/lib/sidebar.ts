import {
  Archive,
  ArrowDownLeft,
  Ban,
  Clock,
  FileText,
  Folder,
  FolderSearch,
  History,
  Pin,
  Send,
  ShieldAlert,
  Trash2,
  type LucideIcon,
} from "lucide-react";
import type { AccountId, FolderRole, FolderView, Message, MessagePatch } from "../types";
import { isDraft } from "./drafts";
import { SIDEBAR_DEFAULT } from "./panes";

// What the sidebar holds, like Spark's: a top section without a label, a
// "Folders" section, Recents (lists opened from the folder picker), and the
// smart folders (saved searches). A display preference of this machine, so
// it lives in localStorage like the theme.

/** Lists every account shares. Server-backed ones gather each account's folder with `role`. */
export type SystemList =
  | "pins"
  | "snoozed"
  | "aside"
  | "drafts"
  | "sent"
  | "archive"
  | "spam"
  | "trash"
  | "blocked"
  | "seen";

/** Any list the message list can show: the Inbox, a shared list, a server folder or a smart folder. */
export type ListKey = "inbox" | SystemList | `folder:${number}` | `smart:${string}`;

export const SYSTEM_LISTS: Record<SystemList, { label: string; Icon: LucideIcon; role?: FolderRole }> = {
  pins: { label: "Pins", Icon: Pin },
  snoozed: { label: "Snoozed", Icon: Clock },
  aside: { label: "Set Aside", Icon: ArrowDownLeft },
  drafts: { label: "Drafts", Icon: FileText, role: "drafts" },
  sent: { label: "Sent", Icon: Send, role: "sent" },
  archive: { label: "Archive", Icon: Archive, role: "archive" },
  spam: { label: "Spam", Icon: ShieldAlert, role: "spam" },
  trash: { label: "Trash", Icon: Trash2, role: "trash" },
  blocked: { label: "Blocked", Icon: Ban, role: "blocked" },
  seen: { label: "Recently Seen", Icon: History },
};

/** "All Folders" order in the folder picker. */
export const SYSTEM_ORDER = Object.keys(SYSTEM_LISTS) as SystemList[];

export interface SmartFolder {
  id: string;
  name: string;
  /** Search syntax, as typed in the search field. */
  query: string;
}

export interface SidebarLayout {
  /** The top section, right under the Inbox. */
  favorites: ListKey[];
  /** The "Folders" section. */
  folders: ListKey[];
  /** Lists opened from the folder picker without adding them, newest first. */
  recents: ListKey[];
  smartFolders: SmartFolder[];
  hiddenBadges: ListKey[];
  /** Icon colors chosen for folders and smart folders. */
  colors: Partial<Record<ListKey, string>>;
  /** Lists showing a row per account under them, the Inbox at first. */
  expanded: ListKey[];
  recentsOpen: boolean;
  /** The folder picker's "Use Ctrl to add multiple folders" tip was closed. */
  tipHidden: boolean;
  /** Docked rather than the rail. */
  locked: boolean;
  width: number;
}

export const RECENTS_MAX = 5;

/** Spark's label colors. */
export const FOLDER_COLORS = ["#ff5b5b", "#ff9f2e", "#f5c400", "#3ccb6e", "#2cc5d2", "#3d8bff", "#9a6bff", "#ff6bc6"];

export const DEFAULT_LAYOUT: SidebarLayout = {
  favorites: ["pins", "drafts", "sent", "trash"],
  folders: ["archive"],
  recents: [],
  smartFolders: [
    { id: "attachments", name: "Attachments", query: "has:attachment" },
    { id: "today-unread", name: "Today's Unread", query: "is:unread date:today" },
  ],
  hiddenBadges: [],
  colors: {},
  expanded: ["inbox"],
  recentsOpen: true,
  tipHidden: false,
  locked: false,
  width: SIDEBAR_DEFAULT,
};

const STORAGE_KEY = "spark-rs.sidebar";

/** The saved layout, with defaults for anything missing or damaged. */
export function loadLayout(): SidebarLayout {
  try {
    const parsed = JSON.parse(localStorage.getItem(STORAGE_KEY) ?? "null") as
      | (Partial<SidebarLayout> & { inboxOpen?: boolean })
      | null;
    if (!parsed || typeof parsed !== "object") return DEFAULT_LAYOUT;
    // Before `expanded`, only the Inbox could show its accounts.
    const { inboxOpen, ...saved } = parsed;
    const keys = (value: unknown, fallback: ListKey[]) =>
      Array.isArray(value) ? value.filter((k): k is ListKey => typeof k === "string") : fallback;
    return {
      ...DEFAULT_LAYOUT,
      ...saved,
      expanded: keys(saved.expanded, inboxOpen === false ? [] : DEFAULT_LAYOUT.expanded),
      favorites: keys(saved.favorites, DEFAULT_LAYOUT.favorites),
      folders: keys(saved.folders, DEFAULT_LAYOUT.folders),
      recents: keys(saved.recents, []),
      hiddenBadges: keys(saved.hiddenBadges, []),
      smartFolders: Array.isArray(saved.smartFolders) ? saved.smartFolders : DEFAULT_LAYOUT.smartFolders,
      colors: saved.colors && typeof saved.colors === "object" ? saved.colors : {},
    };
  } catch {
    return DEFAULT_LAYOUT;
  }
}

export function saveLayout(layout: SidebarLayout) {
  localStorage.setItem(STORAGE_KEY, JSON.stringify(layout));
}

export const folderKey = (id: number): ListKey => `folder:${id}`;
export const smartKey = (id: string): ListKey => `smart:${id}`;

export function folderIdOf(key: ListKey): number | undefined {
  return key.startsWith("folder:") ? Number(key.slice(7)) : undefined;
}

export function smartIdOf(key: ListKey): string | undefined {
  return key.startsWith("smart:") ? key.slice(6) : undefined;
}

export function isSystemList(key: ListKey): key is SystemList {
  return key in SYSTEM_LISTS;
}

/** Added to the sidebar (top section or Folders), not just recent. */
export function inSidebar(layout: SidebarLayout, key: ListKey) {
  return layout.favorites.includes(key) || layout.folders.includes(key);
}

const without = (keys: ListKey[], key: ListKey) => keys.filter((k) => k !== key);

/** Adds a list to the end of Folders (out of Recents); no change if it's already there. */
export function addToSidebar(layout: SidebarLayout, key: ListKey): SidebarLayout {
  if (inSidebar(layout, key)) return layout;
  return { ...layout, folders: [...layout.folders, key], recents: without(layout.recents, key) };
}

export function removeFromSidebar(layout: SidebarLayout, key: ListKey): SidebarLayout {
  return {
    ...layout,
    favorites: without(layout.favorites, key),
    folders: without(layout.folders, key),
    recents: without(layout.recents, key),
  };
}

/** Moves a list to the end of the top section. */
export function pinToTop(layout: SidebarLayout, key: ListKey): SidebarLayout {
  const rest = removeFromSidebar(layout, key);
  return { ...rest, favorites: [...rest.favorites, key] };
}

/** Moves a list from the top section to the start of Folders. */
export function removeFromTop(layout: SidebarLayout, key: ListKey): SidebarLayout {
  const rest = removeFromSidebar(layout, key);
  return { ...rest, folders: [key, ...rest.folders] };
}

/** The sections a list can be dragged into. */
export type SidebarSection = "favorites" | "folders";

/**
 * Dragging a list to position `index` of a section, counted before the move.
 * It leaves wherever it was, Recents included.
 */
export function moveInSidebar(layout: SidebarLayout, key: ListKey, section: SidebarSection, index: number): SidebarLayout {
  const from = layout[section].indexOf(key);
  const at = from >= 0 && from < index ? index - 1 : index;
  const rest = removeFromSidebar(layout, key);
  const list = [...rest[section]];
  list.splice(at, 0, key);
  return { ...rest, [section]: list };
}

/** A list opened from the folder picker: it goes to the top of Recents unless it's in the sidebar. */
export function visit(layout: SidebarLayout, key: ListKey): SidebarLayout {
  if (inSidebar(layout, key)) return layout;
  return { ...layout, recents: [key, ...without(layout.recents, key)].slice(0, RECENTS_MAX) };
}

/** The Inbox and the shared lists can show a row per account; folders belong to one already. */
export function hasAccountRows(key: ListKey) {
  return key === "inbox" || (isSystemList(key) && key !== "seen");
}

export function toggleExpanded(layout: SidebarLayout, key: ListKey): SidebarLayout {
  const open = layout.expanded.includes(key);
  return { ...layout, expanded: open ? without(layout.expanded, key) : [...layout.expanded, key] };
}

export function toggleBadge(layout: SidebarLayout, key: ListKey): SidebarLayout {
  const hidden = layout.hiddenBadges.includes(key);
  return { ...layout, hiddenBadges: hidden ? without(layout.hiddenBadges, key) : [...layout.hiddenBadges, key] };
}

export function setColor(layout: SidebarLayout, key: ListKey, color: string | undefined): SidebarLayout {
  const colors = { ...layout.colors };
  if (color) colors[key] = color;
  else delete colors[key];
  return { ...layout, colors };
}

/** Saves a search as a smart folder in the Folders section. Returns its key too. */
export function addSmartFolder(layout: SidebarLayout, name: string, query: string): [SidebarLayout, ListKey] {
  const id = `${Date.now().toString(36)}${Math.random().toString(36).slice(2, 6)}`;
  const key = smartKey(id);
  const next = { ...layout, smartFolders: [...layout.smartFolders, { id, name, query }] };
  return [addToSidebar(next, key), key];
}

export function renameSmartFolder(layout: SidebarLayout, id: string, name: string): SidebarLayout {
  return { ...layout, smartFolders: layout.smartFolders.map((f) => (f.id === id ? { ...f, name } : f)) };
}

export function removeSmartFolder(layout: SidebarLayout, id: string): SidebarLayout {
  const key = smartKey(id);
  return {
    ...forget(layout, new Set([key])),
    smartFolders: layout.smartFolders.filter((f) => f.id !== id),
  };
}

/** Drops server folders that no longer exist (deleted, or their account removed). */
export function pruneLayout(layout: SidebarLayout, folders: FolderView[]): SidebarLayout {
  const exists = new Set(folders.map((f) => folderKey(f.id)));
  const gone = new Set(
    [...layout.favorites, ...layout.folders, ...layout.recents].filter((k) => k.startsWith("folder:") && !exists.has(k)),
  );
  return gone.size ? forget(layout, gone) : layout;
}

function forget(layout: SidebarLayout, keys: Set<ListKey>): SidebarLayout {
  const keep = (list: ListKey[]) => list.filter((k) => !keys.has(k));
  const colors = { ...layout.colors };
  for (const key of keys) delete colors[key];
  return {
    ...layout,
    favorites: keep(layout.favorites),
    folders: keep(layout.folders),
    recents: keep(layout.recents),
    hiddenBadges: keep(layout.hiddenBadges),
    colors,
  };
}

/**
 * The server folders a list shows mail from: a folder itself, or each
 * account's folder for a shared list (only `account`'s when it's narrowed).
 */
export function listFolderIds(key: ListKey, folders: FolderView[], account?: AccountId): number[] {
  const id = folderIdOf(key);
  if (id !== undefined) return [id];
  const role = isSystemList(key) ? SYSTEM_LISTS[key].role : undefined;
  if (!role) return [];
  return folders.filter((f) => f.role === role && (account === undefined || f.account === account)).map((f) => f.id);
}

/** Folders to keep in sync: every list in the sidebar, plus the open one. */
export function watchedFolderIds(layout: SidebarLayout, folders: FolderView[], open: ListKey): number[] {
  const keys = new Set([...layout.favorites, ...layout.folders, ...layout.recents, open]);
  const ids = new Set([...keys].flatMap((k) => listFolderIds(k, folders)));
  return [...ids].sort((a, b) => a - b);
}

export interface ListInfo {
  label: string;
  Icon: LucideIcon;
  /** The account a server folder belongs to. */
  account?: AccountId;
  folder?: FolderView;
  smart?: SmartFolder;
  /** For a row's tooltip, e.g. "Projects / Q4 · Work". */
  detail?: string;
}

/** How to show a list; `undefined` if it names a folder or smart folder that's gone. */
export function listInfo(
  key: ListKey,
  folders: FolderView[],
  smartFolders: SmartFolder[],
  accountLabel: (id: AccountId) => string = () => "",
): ListInfo | undefined {
  if (key === "inbox") return { label: "Inbox", Icon: Folder };
  if (isSystemList(key)) return SYSTEM_LISTS[key];
  const folderId = folderIdOf(key);
  if (folderId !== undefined) {
    const folder = folders.find((f) => f.id === folderId);
    if (!folder) return undefined;
    const path = folderPath(folder, folders);
    const account = accountLabel(folder.account);
    return {
      label: folder.name,
      Icon: Folder,
      account: folder.account,
      folder,
      detail: [path, account].filter(Boolean).join(" · "),
    };
  }
  const smart = smartFolders.find((f) => f.id === smartIdOf(key));
  return smart && { label: smart.name, Icon: FolderSearch, smart, detail: smart.query };
}

/** "Projects / Q4" for a nested folder. */
export function folderPath(folder: FolderView, folders: FolderView[]) {
  const names = [folder.name];
  let parent = folders.find((f) => f.id === folder.parent);
  for (let depth = 0; parent && depth < 20; depth++) {
    names.unshift(parent.name);
    parent = folders.find((f) => f.id === parent!.parent);
  }
  return names.join(" / ");
}

/** Folders the user made: what the folder picker lists per account. Shared lists cover the rest. */
export const isUserFolder = (f: FolderView) => f.role === undefined || f.role === null;

/**
 * What dropping a message on a sidebar list does, or `undefined` if the list
 * doesn't take it: a folder of another account, a list it's already in, or
 * one that can't be moved to (Sent, Snoozed, smart folders…). `account` is
 * an account row's.
 */
export function dropPatch(
  m: Message,
  key: ListKey,
  folders: FolderView[],
  account?: AccountId,
): MessagePatch | undefined {
  if (account !== undefined && account !== m.account) return undefined;
  // A draft can only be thrown away.
  if (isDraft(m, folders)) return key === "trash" ? { state: "deleted" } : undefined;
  switch (key) {
    case "inbox":
      return m.state === "inbox" ? undefined : { state: "inbox" };
    case "archive":
      return m.state === "done" ? undefined : { state: "done" };
    case "trash":
      return m.state === "deleted" ? undefined : { state: "deleted" };
    case "pins":
      return m.pinned ? undefined : { pinned: true };
    case "spam": {
      const spam = folders.find((f) => f.account === m.account && f.role === "spam");
      return !spam || (m.state === "filed" && m.folder === spam.id) ? undefined : { folder: spam.id };
    }
  }
  const folder = folders.find((f) => f.id === folderIdOf(key));
  if (!folder || !isUserFolder(folder) || !folder.selectable || folder.account !== m.account) return undefined;
  return m.state === "filed" && m.folder === folder.id ? undefined : { folder: folder.id };
}

/** Recently opened messages, newest first, for Recently Seen. */
const SEEN_KEY = "spark-rs.seen";
export const SEEN_MAX = 50;

export function loadSeen(): number[] {
  try {
    const saved = JSON.parse(localStorage.getItem(SEEN_KEY) ?? "[]");
    return Array.isArray(saved) ? saved.filter((id): id is number => typeof id === "number") : [];
  } catch {
    return [];
  }
}

export function saveSeen(ids: number[]) {
  localStorage.setItem(SEEN_KEY, JSON.stringify(ids.slice(0, SEEN_MAX)));
}
