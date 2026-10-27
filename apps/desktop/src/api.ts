import { invoke, isTauri } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { Window, getCurrentWindow } from "@tauri-apps/api/window";
import demo from "../demo/mailbox.json";
import demoFolders from "../demo/folders.json";
import { version as appVersion } from "../package.json";
import type {
  AccountId,
  CalendarEvent,
  CalendarList,
  Contact,
  AgentAccess,
  AgentSendingEvent,
  AgentsView,
  AccountView,
  Category,
  Failure,
  FolderRole,
  FolderView,
  GatekeeperMode,
  InlineImages,
  LocalFile,
  InviteAnswer,
  InviteCard,
  InvitePerson,
  Message,
  MessageContent,
  MessagePatch,
  OutgoingDraft,
  PopupMeeting,
  Prefs,
  PriorityUndo,
  ReminderView,
  ScheduledView,
  SendStatusEvent,
  SenderChange,
  SenderDecision,
  SenderStatus,
  Signature,
  SignaturesView,
  DefaultSignature,
  SyncStatusEvent,
  Unsubscribed,
  Untrusted,
  UpdatesView,
  UpdateProgress,
} from "./types";
import { covers, domainOf, settled } from "./lib/gatekeeper";
import { htmlText } from "./lib/signatures";

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

/** The certificates the user chose to trust while adding an account, by fingerprint. */
export interface TrustedCerts {
  trustedImap?: string;
  trustedSmtp?: string;
}

/**
 * Checks the login before saving. Rejects with a `Failure`, whose certificate
 * is set when a server's certificate needs the user's trust (`TrustedCerts`).
 */
export function addGmailAccount(
  email: string,
  appPassword: string,
  label: string,
  trusted: TrustedCerts = {},
): Promise<AccountView> {
  if (isTauri()) return invoke("add_gmail_account", { email, appPassword, label, ...noTrust, ...trusted });
  return Promise.reject(DESKTOP_ONLY);
}

const noTrust = { trustedImap: null, trustedSmtp: null };

/** A rejection's text: a `Failure`'s message, or the error itself. */
export function failureText(error: unknown): string {
  return failureOf(error).message;
}

export function failureOf(error: unknown): Failure {
  if (typeof error === "object" && error !== null && "message" in error) {
    const { message, certificate } = error as Failure;
    return { message: String(message), certificate };
  }
  return { message: String(error) };
}

/** Trusts the certificate an IMAP account's server showed, then checks for mail with it. */
export function trustCertificate(account: AccountId, certificate: Untrusted): Promise<void> {
  if (isTauri()) {
    return invoke("trust_certificate", {
      account,
      service: certificate.service,
      fingerprint: certificate.fingerprint,
    });
  }
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

/** Rejects as `addGmailAccount` does. */
export function addImapAccount(form: ImapAccountForm, trusted: TrustedCerts = {}): Promise<AccountView> {
  if (isTauri()) return invoke("add_imap_account", { ...form, ...noTrust, ...trusted });
  return Promise.reject(DESKTOP_ONLY);
}

/** Opens the system browser for Microsoft sign-in; resolves once it finishes, which can take minutes. */
export function addMicrosoftAccount(clientId: string, tenant: string, label: string): Promise<AccountView> {
  if (isTauri()) return invoke("add_microsoft_account", { clientId, tenant, label });
  return Promise.reject(DESKTOP_ONLY);
}

/**
 * Signs in to Microsoft again for an existing account, in the system
 * browser, to grant the calendar permission. Resolves once it's done, which
 * can take minutes; `cancelSignIn` abandons it.
 */
export function reconnectAccount(id: AccountId): Promise<AccountView> {
  if (isTauri()) return invoke("reconnect_account", { id });
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

/**
 * Ids of the stored messages whose words start with those of `query`, newest first, from the
 * search index. Null outside the app, where the caller matches the messages it has.
 */
export function searchMessages(query: string): Promise<number[] | null> {
  if (isTauri()) return invoke("search_messages", { query });
  return Promise.resolve(null);
}

/**
 * Puts a search to every account's server, which stores the mail they find that isn't stored
 * yet. Resolves to how many messages were new, so the caller knows to reload; 0 outside the app.
 */
/** `account` keeps it to one account's server. */
export function searchServers(query: string, account?: AccountId): Promise<number> {
  if (isTauri()) return invoke("search_servers", { query, account: account ?? null });
  return Promise.resolve(0);
}

/** The people from the user's mail to suggest for what's typed in a recipient field, best first. */
export function suggestContacts(query: string): Promise<Contact[]> {
  if (isTauri()) return invoke("suggest_contacts", { query });
  const q = query.trim().toLowerCase();
  if (!q) return Promise.resolve([]);
  // The browser preview just matches the demo's senders.
  const people = new Map<string, Contact>();
  for (const m of browserMailbox) people.set(m.address.toLowerCase(), { name: m.from, address: m.address });
  return Promise.resolve(
    [...people.values()].filter((c) => !browserContactExceptions.has(c.address.toLowerCase())
      && `${c.name} ${c.address}`.toLowerCase().includes(q)).slice(0, 20),
  );
}

/** Stops suggesting `address`. */
export function neverSuggestContact(address: string): Promise<void> {
  if (isTauri()) return invoke("never_suggest_contact", { address });
  browserContactExceptions.add(address.toLowerCase());
  return Promise.resolve();
}

export function getMessageContent(id: number): Promise<MessageContent> {
  if (isTauri()) return invoke("get_message_content", { id });
  const m = browserMailbox.find((x) => x.id === id);
  if (!m) return Promise.reject(`No message ${id}.`);
  return Promise.resolve({
    text: m.body ?? [],
    html: browserHtml.get(id) ?? null,
    to: m.to?.length ? m.to : ["You"],
    cc: [],
    attachments: m.attachment ? [{ name: m.attachment, mime: "", size: 0 }] : [],
    invite: browserInvite(m),
  });
}

/** Answers the invitation a message carries, by email to its organizer; resolves once it's sent. */
export function answerInvite(id: number, answer: InviteAnswer): Promise<void> {
  if (isTauri()) return invoke("answer_invite", { id, answer });
  browserAnswers.set(id, answer);
  return new Promise((resolve) => setTimeout(resolve, 400));
}

// ---- calendar ----
// Real accounts only: the demo calendar lives in the UI (lib/demoCalendar),
// which knows the local time zone its events are placed in.

/** The stored calendars, and each account's calendar status. */
export function listCalendars(): Promise<CalendarList> {
  if (isTauri()) return invoke("list_calendars");
  return Promise.resolve({ calendars: [], accounts: [] });
}

/** Stored events overlapping [start, end) (Unix ms), from every calendar. */
export function listEvents(start: number, end: number): Promise<CalendarEvent[]> {
  if (isTauri()) return invoke("list_events", { start, end });
  return Promise.resolve([]);
}

/**
 * Fetches the calendars and their events in [start, end) from the servers.
 * Resolves once stored; then list them again. Per-account failures show in
 * `listCalendars`' account states rather than rejecting.
 */
export function refreshCalendars(start: number, end: number): Promise<void> {
  if (isTauri()) return invoke("refresh_calendars", { start, end });
  return Promise.resolve();
}

/** Answers an event through its calendar; the organizer is told. Resolves once the server has it. */
export function answerEvent(id: number, answer: InviteAnswer): Promise<void> {
  if (isTauri()) return invoke("answer_event", { id, answer });
  return Promise.resolve();
}

// ---- meeting pop-up ----
// Its own window (popup.html); the Rust side decides what it shows.

/** What the pop-up shows now. In the browser, a sample meeting starting in a minute. */
export function meetingPopupMeetings(): Promise<PopupMeeting[]> {
  if (isTauri()) return invoke("meeting_popup_meetings");
  const start = Date.now() + 60_000;
  return Promise.resolve([
    {
      id: -1,
      title: "Design review: Inbox v3",
      start,
      end: start + 30 * 60_000,
      callLink: "https://teams.microsoft.com/l/meetup-join/preview",
      people: ["Hannah Cole", "Priya Raman", "Sam Okafor", "Kai Tanaka", "Alex Moreno"].map((name) => ({
        name,
        address: `${name.split(" ")[0].toLowerCase()}@acme.example`,
      })),
      joined: false,
    },
  ]);
}

export type PopupAct =
  | { action: "join" | "snooze" | "dismiss" | "openCalendar"; id: number }
  | { action: "dismissAll" | "openSettings" };

/** Join opens the call; the card stays, with Open meeting in place of Join. */
export function meetingPopupAct(act: PopupAct): Promise<void> {
  if (isTauri()) return invoke("meeting_popup_act", { act });
  return Promise.resolve();
}

/** Sizes the pop-up's window to its cards (CSS px). */
export function fitMeetingPopup(width: number, height: number): Promise<void> {
  if (isTauri()) return invoke("fit_meeting_popup", { width, height });
  return Promise.resolve();
}

/** Moves the pop-up's window with the mouse, from a mousedown. */
export function dragWindow() {
  if (isTauri()) void getCurrentWindow().startDragging();
}

/** Shows a sample meeting in the pop-up, for Settings. */
export function previewMeetingPopup(): Promise<void> {
  if (isTauri()) return invoke("preview_meeting_popup");
  return Promise.reject("Run the desktop app to see the pop-up.");
}

// ---- folders ----

/** Every account's server folders and labels, accounts in order. */
export function listFolders(): Promise<FolderView[]> {
  if (isTauri()) return invoke("list_folders");
  return Promise.resolve(structuredClone(BROWSER_FOLDERS));
}

/** The folders to keep in sync: those on the sidebar or open. New ones sync at once. */
export function watchFolders(ids: number[]): Promise<void> {
  if (isTauri()) return invoke("watch_folders", { ids });
  return Promise.resolve();
}

/** Syncs these folders now, e.g. the one just opened. Results arrive as `mailbox-changed`. */
export function syncFolders(ids: number[]): Promise<void> {
  if (isTauri()) return invoke("sync_folders", { ids });
  return Promise.resolve();
}

/**
 * Reaches further back on the servers: 100 more messages in these accounts'
 * inboxes and in these folders. Resolves once they're stored; reload the
 * messages, accounts and folders (`hasOlder`) then.
 */
export function loadOlder(accounts: AccountId[], folders: number[]): Promise<void> {
  if (isTauri()) return invoke("load_older", { accounts, folders });
  return Promise.resolve();
}

/** Lists the folders on the servers again; changes arrive as `folders-changed`. */
export function refreshFolders(): Promise<void> {
  if (isTauri()) return invoke("refresh_folders");
  return Promise.resolve();
}

/** Creates a folder in `account`, inside `parent` or at the top. */
export function createFolder(account: AccountId, parent: number | undefined, name: string): Promise<FolderView> {
  if (isTauri()) return invoke("create_folder", { account, parent: parent ?? null, name });
  return Promise.reject(DEMO_FOLDERS);
}

export function renameFolder(id: number, name: string): Promise<void> {
  if (isTauri()) return invoke("rename_folder", { id, name });
  return Promise.reject(DEMO_FOLDERS);
}

/** Deletes the folder, its subfolders and their mail on the server. */
export function deleteFolder(id: number): Promise<void> {
  if (isTauri()) return invoke("delete_folder", { id });
  return Promise.reject(DEMO_FOLDERS);
}

// ---- scheduled sends and reminders (the desktop app only) ----

export function listScheduled(): Promise<ScheduledView[]> {
  if (isTauri()) return invoke("list_scheduled");
  return Promise.resolve([]);
}

/** Sends a scheduled message at `at` instead, or (null) now. False once it's gone out. */
export function rescheduleSend(id: number, at: number | null): Promise<boolean> {
  if (isTauri()) return invoke("reschedule_send", { id, at });
  return Promise.resolve(false);
}

/**
 * Takes a scheduled message out of the queue into Drafts, exactly as it was
 * queued (with its time, given `keepSchedule`). Resolves to the draft's id;
 * null once it's gone out.
 */
export function unscheduleSend(id: number, keepSchedule: boolean): Promise<number | null> {
  if (isTauri()) return invoke("unschedule_send", { id, keepSchedule });
  return Promise.resolve(null);
}

export function listReminders(): Promise<ReminderView[]> {
  if (isTauri()) return invoke("list_reminders");
  return Promise.resolve([]);
}

/** Moves a reminder to `at`, or (null) removes it. */
export function setReminder(id: number, at: number | null): Promise<void> {
  if (isTauri()) return invoke("set_reminder", { id, at });
  return Promise.resolve();
}

/** Done on a reminder in the Inbox, or (false) its undo. */
export function finishReminder(id: number, done: boolean): Promise<void> {
  if (isTauri()) return invoke("finish_reminder", { id, done });
  return Promise.resolve();
}

/**
 * Files a sender's mail under `category`, now and as it arrives, in every
 * account; null goes back to what the headers say. Resolves to the sender's
 * category before, for undo.
 */
export function setSenderCategory(address: string, category: Category | null): Promise<Category | null> {
  if (isTauri()) return invoke("set_sender_category", { address, category });
  // Like the store: the sender's choice, over each email's own category.
  const key = address.toLowerCase();
  const before = browserCategories.get(key) ?? null;
  if (category) browserCategories.set(key, category);
  else browserCategories.delete(key);
  for (const [i, m] of browserMailbox.entries()) {
    if (m.address.toLowerCase() !== key) continue;
    browserMailbox[i] = { ...m, category: category ?? browserOwnCategory.get(m.id) ?? m.category };
  }
  return Promise.resolve(before);
}

/** Mutes or unmutes a conversation (`Message.conversation`, or the message's id). */
export function setMuted(conversation: number, muted: boolean): Promise<void> {
  if (isTauri()) return invoke("set_muted", { conversation, muted });
  for (const [i, m] of browserMailbox.entries()) {
    if ((m.conversation ?? m.id) === conversation) browserMailbox[i] = { ...m, muted };
  }
  return Promise.resolve();
}

/** Asks the mailing list a message came from to stop sending. */
export function unsubscribe(id: number): Promise<Unsubscribed> {
  if (isTauri()) return invoke("unsubscribe", { id });
  return Promise.resolve({ kind: "done" });
}

/** Deletes messages for good on their servers; resolves to how many went. */
export function deleteForever(ids: number[]): Promise<number> {
  if (isTauri()) return invoke("delete_forever", { ids });
  const before = browserMailbox.length;
  for (let i = browserMailbox.length - 1; i >= 0; i--) {
    if (ids.includes(browserMailbox[i].id)) browserMailbox.splice(i, 1);
  }
  return Promise.resolve(before - browserMailbox.length);
}

/**
 * Deletes everything in these folders (of any accounts) for good on their
 * servers, including mail not downloaded here; resolves to how many emails went.
 */
export function emptyFolders(ids: number[]): Promise<number> {
  if (isTauri()) return invoke("empty_folders", { ids });
  const before = browserMailbox.length;
  for (let i = browserMailbox.length - 1; i >= 0; i--) {
    const m = browserMailbox[i];
    const inside = BROWSER_FOLDERS.some(
      (f) =>
        ids.includes(f.id) &&
        f.account === m.account &&
        (f.role === "trash" ? m.state === "deleted" : m.state === "filed" && m.folder === f.id),
    );
    if (inside) browserMailbox.splice(i, 1);
  }
  return Promise.resolve(before - browserMailbox.length);
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

/** Fetches the images the email's HTML embeds from its server. None for demo mail. */
export function getInlineImages(id: number): Promise<InlineImages> {
  if (isTauri()) return invoke("inline_images", { id });
  return Promise.resolve({});
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

/** Where in the window (CSS px) files are dragged to. */
export type DragPoint = { x: number; y: number };
export type FileDrag =
  | { type: "over"; at: DragPoint }
  | { type: "leave" }
  | { type: "drop"; paths: string[]; at: DragPoint };

/**
 * Files dragged over or dropped on the window. The desktop shell handles file
 * drops itself (the page never sees them), so this is how the composer gets
 * their paths. Returns the unsubscribe.
 */
export function onFileDrag(handler: (e: FileDrag) => void): () => void {
  if (!isTauri()) return () => {};
  const stop = getCurrentWebview().onDragDropEvent(({ payload }) => {
    if (payload.type === "leave") return handler({ type: "leave" });
    // The shell gives physical pixels.
    const at = { x: payload.position.x / devicePixelRatio, y: payload.position.y / devicePixelRatio };
    if (payload.type === "drop") handler({ type: "drop", paths: payload.paths, at });
    else handler({ type: "over", at });
  });
  return () => void stop.then((unlisten) => unlisten());
}

/** A picture file dropped on the composer's body, as a data URL. */
export function readImage(path: string): Promise<string> {
  if (isTauri()) return invoke("read_image", { path });
  return Promise.reject("Run the desktop app to add pictures from files.");
}

export function updateMessage(id: number, patch: MessagePatch): Promise<Message> {
  if (isTauri()) return invoke("update_message", { id, patch });
  const index = browserMailbox.findIndex((m) => m.id === id);
  if (index < 0) return Promise.reject(new Error(`no message ${id}`));
  const folder = BROWSER_FOLDERS.find((f) => f.id === patch.folder);
  if (patch.folder !== undefined && folder?.account !== browserMailbox[index].account) {
    return Promise.reject(`No folder ${patch.folder} in this account.`);
  }
  browserMailbox[index] = patched(browserMailbox[index], patch);
  return Promise.resolve(structuredClone(browserMailbox[index]));
}

/** Same rules as MessagePatch::apply on the Rust side. Used for optimistic updates too. */
export function patched(message: Message, patch: MessagePatch): Message {
  const next = { ...message };
  if (patch.state !== undefined) {
    next.state = patch.state;
    if (patch.state !== "snoozed") delete next.snoozedUntil;
    // Out of its folder, e.g. to the Archive or back to the Inbox.
    if (patch.state !== "filed" && message.folder !== undefined) {
      next.folders = message.folders?.filter((id) => id !== message.folder);
      delete next.folder;
    }
  }
  if (patch.folder !== undefined) {
    next.state = "filed";
    delete next.snoozedUntil;
    const others = next.folders?.filter((id) => id !== next.folder && id !== patch.folder) ?? [];
    next.folders = [...others, patch.folder];
    next.folder = patch.folder;
  }
  if (patch.snoozedUntil !== undefined) next.snoozedUntil = patch.snoozedUntil;
  if (patch.unread !== undefined) next.unread = patch.unread;
  if (patch.pinned !== undefined) next.pinned = patch.pinned;
  if (patch.priority !== undefined) next.priority = patch.priority;
  return next;
}

// ---- Gatekeeper ----

/**
 * Accepts or blocks senders or whole domains for every account (`null`
 * makes them new again), moving their mail to match. Resolves to the
 * changes that undo it. A free mail provider's domain is refused.
 */
export function decideSenders(changes: SenderChange[]): Promise<SenderChange[]> {
  if (isTauri()) return invoke("decide_senders", { changes });
  const undo: SenderChange[] = [];
  for (const change of changes) {
    const isDomain = "domain" in change.screen;
    const key = isDomain
      ? `d:${(change.screen as { domain: string }).domain.replace(/^@/, "").toLowerCase()}`
      : `s:${(change.screen as { sender: string }).sender.toLowerCase()}`;
    if (isDomain && BROWSER_PUBLIC.has(key.slice(2))) {
      return Promise.reject(`Anyone can have an address at ${key.slice(2)}, so its senders can only be accepted or blocked one by one.`);
    }
    undo.unshift({ screen: change.screen, decision: browserDecisions.get(key) ?? null });
    if (change.decision) browserDecisions.set(key, change.decision);
    else browserDecisions.delete(key);
    browserMailbox.forEach((m, i) => {
      if (!covers(change.screen, m.address)) return;
      const status = browserStatus(m.address);
      const decision = status.sender ?? status.domain;
      const next = settled(m, decision ?? (browserMode === "off" ? "accepted" : null));
      browserMailbox[i] = next;
    });
  }
  return Promise.resolve(undo);
}

export function senderStatus(address: string): Promise<SenderStatus> {
  if (isTauri()) return invoke("sender_status", { address });
  return Promise.resolve(browserStatus(address));
}

export function getGatekeeperMode(): Promise<GatekeeperMode> {
  if (isTauri()) return invoke("gatekeeper_mode");
  return Promise.resolve(browserMode);
}

/** Turning the Gatekeeper off accepts everyone waiting in it. */
export function saveGatekeeperMode(mode: GatekeeperMode): Promise<void> {
  if (isTauri()) return invoke("set_gatekeeper_mode", { mode });
  browserMode = mode;
  if (mode === "off") {
    browserMailbox.forEach((m, i) => {
      if (!m.screening) return;
      browserDecisions.set(`s:${m.address.toLowerCase()}`, "accepted");
      browserMailbox[i] = { ...m, screening: false };
    });
  }
  return Promise.resolve();
}

// ---- Priority senders ----

/** The Priority senders, whose mail is all Priority. */
export function getPrioritySenders(): Promise<string[]> {
  if (isTauri()) return invoke("priority_senders");
  return Promise.resolve([...browserPriority].sort());
}

/** Adds senders to the Priority senders, or takes them off, and marks their mail Priority or not. */
export function prioritizeSenders(addresses: string[], priority: boolean): Promise<PriorityUndo> {
  if (isTauri()) return invoke("prioritize_senders", { addresses, priority });
  const keys = addresses.map((a) => a.trim().toLowerCase());
  // All or nothing, as the store's transaction is.
  if (keys.some((a) => !a.includes("@"))) return Promise.reject("That isn't an email address.");
  if (priority && keys.some((a) => BROWSER_ACCOUNTS.some((x) => x.email.toLowerCase() === a))) {
    return Promise.reject("That's one of your own addresses.");
  }
  const undo: PriorityUndo = { senders: [], messages: [], priority: !priority };
  for (const address of keys) {
    if (browserPriority.has(address) !== priority) undo.senders.push(address);
    if (priority) browserPriority.add(address);
    else browserPriority.delete(address);
    browserMailbox.forEach((m, i) => {
      if (m.priority === priority || m.address.toLowerCase() !== address) return;
      undo.messages.push(m.id);
      browserMailbox[i] = { ...m, priority };
    });
  }
  return Promise.resolve(undo);
}

export function undoPrioritize(undo: PriorityUndo): Promise<void> {
  if (isTauri()) return invoke("undo_prioritize", { undo });
  for (const address of undo.senders) {
    if (undo.priority) browserPriority.add(address);
    else browserPriority.delete(address);
  }
  browserMailbox.forEach((m, i) => {
    if (undo.messages.includes(m.id)) browserMailbox[i] = { ...m, priority: undo.priority };
  });
  return Promise.resolve();
}

function browserStatus(address: string): SenderStatus {
  const domain = domainOf(address) ?? "";
  return {
    sender: browserDecisions.get(`s:${address.toLowerCase()}`) ?? null,
    domain: browserDecisions.get(`d:${domain}`) ?? null,
    publicDomain: BROWSER_PUBLIC.has(domain),
  };
}

// ---- sending ----

/** Queues a message to go out at `draft.sendAt`. Returns its outbox id. A saved draft it came from leaves Drafts. */
export function sendMessage(draft: OutgoingDraft): Promise<number> {
  if (isTauri()) return invoke("send_message", { draft });
  const address = BROWSER_ACCOUNTS.find((a) => a.id === draft.account)?.email.toLowerCase();
  if (address && draft.signature && draft.signatureId !== undefined) browserSignatureUsage.set(address, draft.signatureId);
  const id = ++browserOutbox;
  const i = browserMailbox.findIndex((m) => m.id === draft.draftId && isBrowserDraft(m));
  if (i >= 0) browserSending.set(id, browserMailbox.splice(i, 1)[0]);
  return Promise.resolve(id);
}

/** True if it was cancelled before going out; a saved draft it came from is back in Drafts. */
export function cancelSend(id: number): Promise<boolean> {
  if (isTauri()) return invoke("cancel_send", { id });
  const draft = browserSending.get(id);
  browserSending.delete(id);
  if (draft) browserMailbox.push(draft);
  return Promise.resolve(true);
}

// ---- drafts ----

/** Saves the composer's draft in its account's Drafts: here at once, on the server with the next sync. Resolves to its id. */
export function saveDraft(draft: OutgoingDraft): Promise<number> {
  if (isTauri()) return invoke("save_draft", { draft });
  return Promise.resolve(saveBrowserDraft(draft));
}

/** A draft from Drafts, for the composer. */
export function openDraft(id: number): Promise<OutgoingDraft> {
  if (isTauri()) return invoke("open_draft", { id });
  const saved = browserDrafts.get(id);
  if (saved) return Promise.resolve(structuredClone(saved));
  const m = browserMailbox.find((x) => x.id === id && isBrowserDraft(x));
  if (!m) return Promise.reject("That draft is gone.");
  return Promise.resolve({
    account: m.account,
    to: (m.to ?? []).join(", "),
    cc: "",
    bcc: "",
    subject: m.subject,
    body: (m.body ?? []).join("\n\n"),
    sendAt: 0,
    attachments: [],
    draftId: id,
  });
}

/** Deletes a draft, here and on the server. */
export function discardDraft(id: number): Promise<void> {
  if (isTauri()) return invoke("discard_draft", { id });
  const i = browserMailbox.findIndex((m) => m.id === id && isBrowserDraft(m));
  if (i >= 0) browserMailbox.splice(i, 1);
  browserDrafts.delete(id);
  return Promise.resolve();
}

// ---- signatures ----

/** Saved signatures, and each sending address's default. */
export function listSignatures(): Promise<SignaturesView> {
  if (isTauri()) return invoke("list_signatures");
  return Promise.resolve(
    structuredClone({
      signatures: browserSignatures,
      defaults: BROWSER_ACCOUNTS.map((a) => ({ mailbox: a.email, signature: browserDefault(a.email) })),
    }),
  );
}

/** Saves signature `id`, or a new one. The HTML is cleaned first; resolves to the signature as kept. Rejects with a user-facing reason. */
export function saveSignature(id: number | null, html: string): Promise<Signature> {
  if (isTauri()) return invoke("save_signature", { id, html });
  const doc = new DOMParser().parseFromString(html, "text/html");
  for (const el of doc.querySelectorAll("script, style, iframe, object, embed, form, input, button, link, meta")) el.remove();
  for (const el of doc.body.querySelectorAll("*")) {
    for (const attr of [...el.attributes]) if (/^on/i.test(attr.name)) el.removeAttribute(attr.name);
  }
  const clean = doc.body.innerHTML.trim();
  const text = htmlText(clean);
  if (!text && !clean.includes("<img")) return Promise.reject("Write a signature first.");
  const saved = { id: id ?? Math.max(0, ...browserSignatures.map((x) => x.id)) + 1, html: clean, text };
  const i = browserSignatures.findIndex((x) => x.id === id);
  if (i >= 0) browserSignatures[i] = saved;
  else browserSignatures.push(saved);
  return Promise.resolve(structuredClone(saved));
}

/** Addresses that defaulted to it go back to Most Recent. */
export function deleteSignature(id: number): Promise<void> {
  if (isTauri()) return invoke("delete_signature", { id });
  browserSignatures = browserSignatures.filter((x) => x.id !== id);
  for (const [address, d] of browserSignatureDefaults) if (d.type === "custom" && d.id === id) browserSignatureDefaults.delete(address);
  return Promise.resolve();
}

export function setDefaultSignature(address: string, signature: DefaultSignature): Promise<void> {
  if (isTauri()) return invoke("set_default_signature", { address, signature });
  browserSignatureDefaults.set(address.toLowerCase(), signature);
  return Promise.resolve();
}

/** The signature the composer starts with for `account`; for a reply, the one used last in that conversation. */
export function resolveSignature(account: AccountId, replyTo?: number): Promise<Signature | null> {
  if (isTauri()) return invoke("resolve_signature", { account, replyTo: replyTo ?? null });
  const address = BROWSER_ACCOUNTS.find((a) => a.id === account)?.email ?? "";
  const d = browserDefault(address);
  const used = browserSignatureUsage.get(address.toLowerCase());
  const pick =
    d.type === "none"
      ? undefined
      : d.type === "custom"
        ? browserSignatures.find((x) => x.id === d.id)
        : (browserSignatures.find((x) => x.id === used) ?? browserSignatures.at(-1));
  return Promise.resolve(pick ? structuredClone(pick) : null);
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

/**
 * Shows `count` on the app's taskbar button, 0 for none. On Windows that's
 * an overlay icon, `icon` (32×32 RGBA, see `lib/badge`).
 */
export function setBadge(count: number, icon: number[] | null): Promise<void> {
  if (isTauri()) return invoke("set_badge", { count, icon });
  return Promise.resolve();
}

/** Rejects with a user-facing reason, e.g. notifications are off for the app in Windows. */
export function testNotification(): Promise<void> {
  if (isTauri()) return invoke("test_notification");
  return Promise.reject("Run the desktop app to see notifications.");
}

/** Whether the meeting pop-up has focus: it's spark-rs too, so the user hasn't left it. */
export async function otherWindowFocused(): Promise<boolean> {
  if (!isTauri()) return false;
  try {
    const popup = await Window.getByLabel("meeting-popup");
    return popup ? await popup.isFocused() : false;
  } catch {
    return false;
  }
}

/** "It's time for Email Focus!", unless the window is in front (Settings → Home Screen → Remind me). */
export function focusReminder(newEmails: number): Promise<void> {
  if (isTauri()) return invoke("focus_reminder", { newEmails });
  return Promise.resolve();
}

// ---- agents ----

export function getAgents(): Promise<AgentsView> {
  if (isTauri()) return invoke("get_agents");
  return Promise.resolve({ accounts: [], cli: null, computer: "This computer" });
}

export function setAgentAccess(account: AccountId, access: AgentAccess): Promise<AgentsView> {
  if (isTauri()) return invoke("set_agent_access", { account, access });
  return Promise.reject(DESKTOP_ONLY);
}

// ---- updates ----

export function getUpdates(): Promise<UpdatesView> {
  if (isTauri()) return invoke("get_updates");
  return Promise.resolve({
    version: appVersion, build: null, commit: null, canUpdate: false,
    checking: false, installing: false, checkedAt: null, available: null, error: null,
  });
}

export function checkForUpdates(): Promise<UpdatesView> {
  return isTauri() ? invoke("check_for_updates") : getUpdates();
}

export function installUpdate(): Promise<void> {
  return isTauri() ? invoke("install_update") : Promise.reject(DESKTOP_ONLY);
}

export const onUpdatesChanged = (handler: (view: UpdatesView) => void) => on("updates-changed", handler);
export const onUpdateProgress = (handler: (progress: UpdateProgress) => void) => on("update-progress", handler);

// ---- events ----
// Each returns its unsubscribe. In the browser nothing is ever emitted.
// `ready` runs once the listener is in place, for state that may have
// changed before then.

function on<T>(event: string, handler: (payload: T) => void, ready?: () => void): () => void {
  if (!isTauri()) return () => {};
  const stop = listen<T>(event, (e) => handler(e.payload));
  if (ready) void stop.then(ready);
  return () => void stop.then((unlisten) => unlisten());
}

/** Messages were added, changed or removed: reload `listMessages`. */
export const onMailboxChanged = (handler: () => void) => on<null>("mailbox-changed", handler);

/** The folder list changed, or a folder synced for the first time: reload `listFolders`. */
export const onFoldersChanged = (handler: () => void) => on<null>("folders-changed", handler);

/** An account was added, removed or renamed (demo mode may have ended): reload `listAccounts`. */
export const onAccountsChanged = (handler: () => void) => on<null>("accounts-changed", handler);

export const onSyncStatus = (handler: (event: SyncStatusEvent) => void) => on("sync-status", handler);

export const onSendStatus = (handler: (event: SendStatusEvent) => void) => on("send-status", handler);

/** An agent changed mail; `text` says what, e.g. "Codex marked 3 emails Done". */
export const onAgentActivity = (handler: (event: { text: string }) => void) => on("agent-activity", handler);

/** An agent queued an email; it can be cancelled until it goes out. */
export const onAgentSending = (handler: (event: AgentSendingEvent) => void) => on("agent-sending", handler);

/** A new-mail notification was clicked: show this message. */
export const onOpenMessage = (handler: (id: number) => void) => on("open-message", handler);

/** A calendar alert, or the meeting pop-up's calendar button: show the calendar on the day of `at` (Unix ms), with `event` open. */
export const onOpenCalendar = (handler: (focus: { at: number; event?: number }) => void) =>
  on("open-calendar", handler);

/** A spark-rs window, this one or the meeting pop-up, gained or lost focus. */
export const onWindowFocus = (handler: (focused: boolean) => void, ready?: () => void) =>
  on("window-focus", handler, ready);

/** The meeting pop-up's settings button: open this Settings page. */
export const onOpenSettings = (handler: (page: "notifications") => void) => on("open-settings", handler);

/** For the pop-up's window: what it shows changed. */
export const onMeetingPopupChanged = (handler: (meetings: PopupMeeting[]) => void) =>
  on("meeting-popup-changed", handler);

// ---- browser preview (`npm run dev`) ----

const DESKTOP_ONLY = "Run the desktop app to add accounts.";
const DEMO_FILES = "This is demo mail: its attachments are only names.";
const DEMO_FOLDERS = "These are demo folders: add an account to manage your own.";

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
    hasOlder: false,
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
    hasOlder: false,
  },
];

const DEMO_ACCOUNT_IDS: Record<string, AccountId> = { personal: 1, work: 2 };

/** Like demo::folders on the Rust side. */
const BROWSER_FOLDERS: FolderView[] = demoFolders.map((f) => ({
  id: f.id,
  account: DEMO_ACCOUNT_IDS[f.account],
  name: f.name,
  parent: "parent" in f ? f.parent : undefined,
  role: "role" in f ? (f.role as FolderRole) : undefined,
  selectable: true,
  synced: true,
  hasOlder: false,
}));

// Like DemoMailbox::load: each entry's age becomes a timestamp relative to now.
const loadedAt = Date.now();
const browserMailbox: Message[] = demo.map(({ ageMinutes, account, html: _html, invite, ...m }) => ({
  state: "inbox",
  unread: false,
  pinned: false,
  priority: false,
  screening: false,
  ...m,
  account: DEMO_ACCOUNT_IDS[account],
  timestamp: loadedAt - ageMinutes * 60_000,
  folders: demoFolders.filter((f) => (f.members as number[]).includes(m.id)).map((f) => f.id),
  inviteKind: invite?.kind,
})) as Message[];

/** Each demo email's own category, and the senders' chosen ones, for `setSenderCategory`. */
const browserOwnCategory = new Map(browserMailbox.map((m) => [m.id, m.category]));
const browserCategories = new Map<string, Category>();

/** Gatekeeper decisions in the preview, by `s:<address>` and `d:<domain>`. Senders not waiting were accepted, as after a first sync. */
const browserDecisions = new Map<string, SenderDecision>(
  browserMailbox.filter((m) => !m.screening).map((m) => [`s:${m.address.toLowerCase()}`, "accepted"]),
);
let browserMode: GatekeeperMode = "beforeInbox";
/** Priority senders in the preview, lowercased. */
const browserPriority = new Set<string>();
/** A few free mail providers, standing in for the store's list. */
const BROWSER_PUBLIC = new Set(["gmail.com", "googlemail.com", "outlook.com", "hotmail.com", "yahoo.com", "icloud.com"]);

/** The few demo messages with HTML of their own, like demo::html. */
const browserHtml = new Map(demo.flatMap((m) => (m.html ? [[m.id, m.html] as const] : [])));

const browserAnswers = new Map<number, InviteAnswer>();
const browserContactExceptions = new Set<string>();

/** Like demo::invite: the demo message's invite, starting on the half hour `startMinutes` from now. */
function browserInvite(m: Message): InviteCard | undefined {
  const invite = demo.find((e) => e.id === m.id)?.invite;
  if (!invite) return undefined;
  const me = BROWSER_ACCOUNTS.find((a) => a.id === m.account)?.email;
  const halfHour = 30 * 60_000;
  const start = Math.floor((Date.now() + invite.startMinutes * 60_000) / halfHour) * halfHour;
  const answer = browserAnswers.get(m.id);
  const invitation = invite.kind === "invitation";
  return {
    kind: invite.kind as InviteCard["kind"],
    summary: invite.summary,
    start,
    end: start + invite.minutes * 60_000,
    allDay: false,
    floating: false,
    repeats: false,
    location: invite.location,
    description: invite.description,
    callLink: invite.callLink,
    organizer: invite.organizer as InvitePerson,
    attendees: invite.attendees.map((a) => {
      const person = a as InvitePerson;
      return a.address === me ? { ...person, answer: answer ?? person.answer } : person;
    }),
    canceled: invite.kind === "cancellation",
    answer: invitation ? answer : undefined,
    canAnswer: invitation && invite.organizer.address !== me,
  };
}

let browserOutbox = 0;

/** Drafts saved in the preview, as the composer had them. */
const browserDrafts = new Map<number, OutgoingDraft>();
/** Drafts out of Drafts while their send can be undone, by outbox id. */
const browserSending = new Map<number, Message>();

/** Signatures in the preview; defaults and the last one sent, by lowercased address. */
let browserSignatures: Signature[] = [];
const browserSignatureDefaults = new Map<string, DefaultSignature>();
const browserSignatureUsage = new Map<string, number>();
const browserDefault = (address: string): DefaultSignature =>
  browserSignatureDefaults.get(address.toLowerCase()) ?? { type: "mostRecent" };

const isBrowserDraft = (m: Message) =>
  m.folders?.some((id) => BROWSER_FOLDERS.some((f) => f.id === id && f.role === "drafts")) ?? false;

/** Like DemoMailbox::save_draft. */
function saveBrowserDraft(draft: OutgoingDraft): number {
  const folder = BROWSER_FOLDERS.find((f) => f.account === draft.account && f.role === "drafts");
  const i = browserMailbox.findIndex((m) => m.id === draft.draftId && isBrowserDraft(m));
  const id = i >= 0 ? draft.draftId! : Math.max(0, ...browserMailbox.map((m) => m.id)) + 1;
  const saved: Message = {
    id,
    account: draft.account,
    category: "people",
    state: "filed",
    unread: false,
    pinned: false,
    priority: false,
    screening: false,
    from: "Me",
    address: BROWSER_ACCOUNTS.find((a) => a.id === draft.account)?.email ?? "",
    to: draft.to
      .split(/[,;]/)
      .map((r) => r.trim())
      .filter(Boolean),
    timestamp: Date.now(),
    subject: draft.subject.trim(),
    snippet: draft.body.replace(/\s+/g, " ").trim().slice(0, 200),
    body: draft.body.split("\n\n"),
    attachment: draft.attachments[0]?.name,
    folders: folder ? [folder.id] : [],
  };
  if (i >= 0) browserMailbox.splice(i, 1);
  browserMailbox.push(saved);
  browserDrafts.set(id, { ...structuredClone(draft), draftId: id });
  return id;
}

/** The Rust side's defaults. */
let browserPrefs: Prefs = {
  notify: "people",
  sound: true,
  snoozeAlert: true,
  badge: "new",
  badgeExcluded: [],
  keepRunning: true,
  openAtLogin: false,
  eventAlerts: true,
  meetingPopup: true,
  meetingPopupMinutes: 1,
  meetingPopupGuestsOnly: true,
  meetingPopupSound: true,
  suggestRecipients: true,
  undoSendSeconds: 5,
  sendAndDone: false,
  autoCc: {},
  autoBcc: {},
};

/** Snoozed mail whose time has come goes back to the Inbox, unread, as the Rust side does. */
function wakeSnoozed(now = Date.now()) {
  for (const [i, m] of browserMailbox.entries()) {
    if (m.state === "snoozed" && m.snoozedUntil !== undefined && m.snoozedUntil <= now) {
      browserMailbox[i] = { ...patched(m, { state: "inbox" }), unread: true, returnedAt: m.snoozedUntil };
    }
  }
}
