import { invoke, isTauri } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import demo from "../demo/mailbox.json";
import type {
  AccountId,
  AccountView,
  LocalFile,
  Message,
  MessageContent,
  MessagePatch,
  OutgoingDraft,
  Prefs,
  SendStatusEvent,
  SyncStatusEvent,
} from "./types";

// Inside the app, data comes from the Rust side. In a plain browser
// (`npm run dev`) we fall back to the same demo file so the UI can be
// developed without building the app. Anything that needs a real server
// (adding accounts, sync, sending) is a no-op or a clear error there.

// ---- accounts ----

/** The demo accounts while no real account exists, otherwise only real ones. */
export function listAccounts(): Promise<AccountView[]> {
  if (isTauri()) return invoke("list_accounts");
  return Promise.resolve(structuredClone(BROWSER_ACCOUNTS));
}

/** Checks the login before saving; the error string is user-facing. */
export function addGmailAccount(email: string, appPassword: string, label: string): Promise<AccountView> {
  if (isTauri()) return invoke("add_gmail_account", { email, appPassword, label });
  return Promise.reject(DESKTOP_ONLY);
}

export interface ImapAccountForm {
  email: string;
  password: string;
  label: string;
  imapHost: string;
  /** 993 is TLS, 143 STARTTLS. */
  imapPort: number;
  smtpHost: string;
  /** 465 is TLS, 587 STARTTLS. */
  smtpPort: number;
}

export function addImapAccount(form: ImapAccountForm): Promise<AccountView> {
  if (isTauri()) return invoke("add_imap_account", { ...form });
  return Promise.reject(DESKTOP_ONLY);
}

/** Opens the system browser for Microsoft sign-in; resolves once it finishes, which can take minutes. */
export function addMicrosoftAccount(clientId: string, tenant: string, label: string): Promise<AccountView> {
  if (isTauri()) return invoke("add_microsoft_account", { clientId, tenant, label });
  return Promise.reject(DESKTOP_ONLY);
}

/** Abandons a pending Microsoft sign-in; `addMicrosoftAccount` then rejects. */
export function cancelSignIn(): Promise<void> {
  if (isTauri()) return invoke("cancel_sign_in");
  return Promise.resolve();
}

export function removeAccount(id: AccountId): Promise<void> {
  if (isTauri()) return invoke("remove_account", { id });
  return Promise.reject(DESKTOP_ONLY);
}

export function renameAccount(id: AccountId, label: string): Promise<AccountView> {
  if (isTauri()) return invoke("rename_account", { id, label });
  return Promise.reject(DESKTOP_ONLY);
}

/** Returns at once; progress arrives as `sync-status` and `mailbox-changed` events. */
export function syncNow(): Promise<void> {
  if (isTauri()) return invoke("sync_now");
  return Promise.resolve();
}

// ---- messages ----

export function listMessages(): Promise<Message[]> {
  if (isTauri()) return invoke("list_messages");
  wakeSnoozed();
  return Promise.resolve(structuredClone(browserMailbox));
}

export function getMessageContent(id: number): Promise<MessageContent> {
  if (isTauri()) return invoke("get_message_content", { id });
  const m = browserMailbox.find((x) => x.id === id);
  if (!m) return Promise.reject(`No message ${id}.`);
  return Promise.resolve({
    text: m.body ?? [],
    html: null,
    to: ["You"],
    cc: [],
    attachments: m.attachment ? [{ name: m.attachment, mime: "", size: 0 }] : [],
  });
}

// ---- attachments ----

/** Downloads attachment `index` and opens it in its default app. Program files are refused. */
export function openAttachment(id: number, index: number): Promise<void> {
  if (isTauri()) return invoke("open_attachment", { id, index });
  return Promise.reject(DEMO_FILES);
}

/** Asks where to save, then downloads. False if the user cancelled the dialog. */
export function saveAttachment(id: number, index: number): Promise<boolean> {
  if (isTauri()) return invoke("save_attachment", { id, index });
  return Promise.reject(DEMO_FILES);
}

/** Asks for a folder and saves every attachment there. Resolves to how many; 0 if cancelled. */
export function saveAllAttachments(id: number): Promise<number> {
  if (isTauri()) return invoke("save_all_attachments", { id });
  return Promise.reject(DEMO_FILES);
}

/** The system file picker, for attaching. Empty if cancelled. */
export function pickFiles(): Promise<LocalFile[]> {
  if (isTauri()) return invoke("pick_files");
  return Promise.reject("Run the desktop app to attach files.");
}

/** Names and sizes of dropped paths; folders are left out. */
export function describeFiles(paths: string[]): Promise<LocalFile[]> {
  if (isTauri()) return invoke("describe_files", { paths });
  return Promise.resolve([]);
}

export type FileDrag = { type: "over" } | { type: "leave" } | { type: "drop"; paths: string[] };

/**
 * Files dragged over or dropped on the window. The desktop shell handles file
 * drops itself (the page never sees them), so this is how the composer gets
 * their paths. Returns the unsubscribe.
 */
export function onFileDrag(handler: (e: FileDrag) => void): () => void {
  if (!isTauri()) return () => {};
  const stop = getCurrentWebview().onDragDropEvent(({ payload }) => {
    if (payload.type === "drop") handler({ type: "drop", paths: payload.paths });
    else if (payload.type === "leave") handler({ type: "leave" });
    else handler({ type: "over" });
  });
  return () => void stop.then((unlisten) => unlisten());
}

export function updateMessage(id: number, patch: MessagePatch): Promise<Message> {
  if (isTauri()) return invoke("update_message", { id, patch });
  const index = browserMailbox.findIndex((m) => m.id === id);
  if (index < 0) return Promise.reject(new Error(`no message ${id}`));
  browserMailbox[index] = patched(browserMailbox[index], patch);
  return Promise.resolve(structuredClone(browserMailbox[index]));
}

/** Same rules as MessagePatch::apply on the Rust side. Used for optimistic updates too. */
export function patched(message: Message, patch: MessagePatch): Message {
  const next = { ...message };
  if (patch.state !== undefined) {
    next.state = patch.state;
    if (patch.state !== "snoozed") delete next.snoozedUntil;
  }
  if (patch.snoozedUntil !== undefined) next.snoozedUntil = patch.snoozedUntil;
  if (patch.unread !== undefined) next.unread = patch.unread;
  if (patch.pinned !== undefined) next.pinned = patch.pinned;
  if (patch.priority !== undefined) next.priority = patch.priority;
  if (patch.screening !== undefined) next.screening = patch.screening;
  return next;
}

// ---- sending ----

/** Queues a message to go out at `draft.sendAt`. Returns its outbox id. */
export function sendMessage(draft: OutgoingDraft): Promise<number> {
  if (isTauri()) return invoke("send_message", { draft });
  return Promise.resolve(++browserOutbox);
}

/** True if it was cancelled before going out. */
export function cancelSend(id: number): Promise<boolean> {
  if (isTauri()) return invoke("cancel_send", { id });
  return Promise.resolve(true);
}

/** Opens an http(s) or mailto link in the system's handler. */
export function openExternal(url: string): Promise<void> {
  if (isTauri()) return invoke("open_external", { url });
  window.open(url, "_blank", "noopener,noreferrer");
  return Promise.resolve();
}

// ---- notifications and running in the background ----

export function getPrefs(): Promise<Prefs> {
  if (isTauri()) return invoke("get_prefs");
  return Promise.resolve({ ...browserPrefs });
}

/** Returns what was saved. Rejects when Windows' sign-in list can't be changed. */
export function savePrefs(prefs: Prefs): Promise<Prefs> {
  if (isTauri()) return invoke("save_prefs", { prefs });
  browserPrefs = { ...prefs };
  return Promise.resolve({ ...prefs });
}

/** Rejects with a user-facing reason, e.g. notifications are off for the app in Windows. */
export function testNotification(): Promise<void> {
  if (isTauri()) return invoke("test_notification");
  return Promise.reject("Run the desktop app to see notifications.");
}

// ---- events ----
// Each returns its unsubscribe. In the browser nothing is ever emitted.

function on<T>(event: string, handler: (payload: T) => void): () => void {
  if (!isTauri()) return () => {};
  const stop = listen<T>(event, (e) => handler(e.payload));
  return () => void stop.then((unlisten) => unlisten());
}

/** Messages were added, changed or removed: reload `listMessages`. */
export const onMailboxChanged = (handler: () => void) => on<null>("mailbox-changed", handler);

/** An account was added, removed or renamed (demo mode may have ended): reload `listAccounts`. */
export const onAccountsChanged = (handler: () => void) => on<null>("accounts-changed", handler);

export const onSyncStatus = (handler: (event: SyncStatusEvent) => void) => on("sync-status", handler);

export const onSendStatus = (handler: (event: SendStatusEvent) => void) => on("send-status", handler);

/** A new-mail notification was clicked: show this message. */
export const onOpenMessage = (handler: (id: number) => void) => on("open-message", handler);

// ---- browser preview (`npm run dev`) ----

const DESKTOP_ONLY = "Run the desktop app to add accounts.";
const DEMO_FILES = "This is demo mail: its attachments are only names.";

/** Same as the Rust side's demo accounts. */
const BROWSER_ACCOUNTS: AccountView[] = [
  {
    id: 1,
    kind: "gmail",
    email: "me@gmail.com",
    name: "Me",
    label: "Personal",
    color: "#0ea5e9",
    provider: "Gmail",
    demo: true,
    sync: { status: "idle" },
  },
  {
    id: 2,
    kind: "microsoft",
    email: "me@outlook.com",
    name: "Me",
    label: "Work",
    color: "#f59e0b",
    provider: "Microsoft 365",
    demo: true,
    sync: { status: "idle" },
  },
];

const DEMO_ACCOUNT_IDS: Record<string, AccountId> = { personal: 1, work: 2 };

// Like DemoMailbox::load: each entry's age becomes a timestamp relative to now.
const loadedAt = Date.now();
const browserMailbox: Message[] = demo.map(({ ageMinutes, account, ...m }) => ({
  state: "inbox",
  unread: false,
  pinned: false,
  priority: false,
  screening: false,
  ...m,
  account: DEMO_ACCOUNT_IDS[account],
  timestamp: loadedAt - ageMinutes * 60_000,
})) as Message[];

let browserOutbox = 0;

/** The Rust side's defaults. */
let browserPrefs: Prefs = { notify: "people", sound: true, keepRunning: true, openAtLogin: false };

/** Snoozed mail whose time has come goes back to the Inbox, unread, as the Rust side does. */
function wakeSnoozed(now = Date.now()) {
  for (const [i, m] of browserMailbox.entries()) {
    if (m.state === "snoozed" && m.snoozedUntil !== undefined && m.snoozedUntil <= now) {
      browserMailbox[i] = { ...patched(m, { state: "inbox" }), unread: true };
    }
  }
}
