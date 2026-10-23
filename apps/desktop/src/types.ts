// Mirrors the serde types on the Rust side (camelCase JSON).

export type AccountId = number;
export type AccountKind = "gmail" | "microsoft" | "imap";
export type SyncState = "idle" | "syncing" | "error";

export interface SyncStatus {
  status: SyncState;
  message?: string;
  /** Unix ms of the last finished sync. */
  lastSync?: number;
}

export interface AccountView {
  id: AccountId;
  kind: AccountKind;
  email: string;
  /** The user's own name, used in From. */
  name: string;
  /** Short label shown in the sidebar, e.g. "Work". */
  label: string;
  /** Hex color, e.g. "#0ea5e9". Applied with inline styles. */
  color: string;
  /** "Gmail", "Microsoft 365" or "IMAP". */
  provider: string;
  /** The built-in demo accounts, shown while no real account exists. */
  demo: boolean;
  sync: SyncStatus;
}

export type Category = "people" | "notifications" | "newsletters";
/**
 * `done` is Spark's Done (archive); `aside` is Set Aside; `filed` lives in
 * another server folder (Sent, Spam, or one of the user's folders or labels).
 */
export type MessageState = "inbox" | "done" | "snoozed" | "aside" | "deleted" | "blocked" | "filed";

export interface Message {
  id: number;
  account: AccountId;
  category: Category;
  state: MessageState;
  unread: boolean;
  pinned: boolean;
  priority: boolean;
  /** From a sender not yet accepted through the Gatekeeper. */
  screening: boolean;
  /** When a snoozed message comes back, in Unix milliseconds. */
  snoozedUntil?: number;
  from: string;
  address: string;
  /** Who it's to, e.g. "Ana Ruiz <ana@x.com>"; Sent and Drafts show them instead of the sender. */
  to?: string[];
  /** When the message arrived, in Unix milliseconds. */
  timestamp: number;
  subject: string;
  snippet: string;
  /** Demo messages only; real ones load through `getMessageContent`. */
  body?: string[];
  attachment?: string;
  /** Ids of the server folders that hold it (a Gmail message can be in several). */
  folders?: number[];
  /** The folder it was filed in, while `state` is `filed`. */
  folder?: number;
}

/** What a server folder is for. User folders and labels have none. */
export type FolderRole = "sent" | "drafts" | "archive" | "trash" | "spam" | "flagged";

/** A server folder or label of one account. */
export interface FolderView {
  id: number;
  account: AccountId;
  name: string;
  parent?: number;
  role?: FolderRole;
  /** False for folders that only hold other folders. */
  selectable: boolean;
  /** Its messages have been synced at least once. */
  synced: boolean;
}

/** Which new mail shows a notification. `people` is Spark's smart notifications. */
export type NotifyFor = "people" | "all" | "off";

/** Settings → Notifications. */
export interface Prefs {
  notify: NotifyFor;
  /** Windows' New Mail sound with each notification. */
  sound: boolean;
  /** Closing the window leaves the app running in the notification area. */
  keepRunning: boolean;
  /** Start in the background when the user signs in to Windows. */
  openAtLogin: boolean;
}

/** What agents may do in an account; each level includes the ones before. */
export type AgentAccess = "none" | "read" | "triage" | "send";

/** Settings → Agents. */
export interface AgentsView {
  accounts: { id: AccountId; email: string; label: string; color: string; access: AgentAccess }[];
  /** spark-cli beside the app, which agents run; null if it's missing. */
  cli: string | null;
  /** This computer's name. */
  computer: string;
}

/** An agent's email waits in the outbox until `sendAt` (Unix ms). */
export interface AgentSendingEvent {
  id: number;
  agent: string;
  to: string;
  subject: string;
  sendAt: number;
}

/** `folder` files the message in that folder of its own account, setting `state` to `filed`. */
export type MessagePatch = Partial<
  Pick<Message, "state" | "unread" | "pinned" | "priority" | "screening" | "snoozedUntil" | "folder">
>;

export interface MessageContent {
  /** Plain-text paragraphs, always present (derived from the HTML if needed). */
  text: string[];
  /** The email's own HTML. Untrusted: only ever rendered in the sandboxed frame. */
  html: string | null;
  /** Display strings, e.g. "Ana Ruiz <ana@x.com>". */
  to: string[];
  cc: string[];
  attachments: Attachment[];
}

/** The images an email's HTML embeds (`cid:` URLs): data URLs by Content-ID, lowercase without angle brackets. */
export type InlineImages = Record<string, string>;

export interface Attachment {
  name: string;
  /** e.g. "application/pdf"; empty when unknown. */
  mime: string;
  /** Bytes; 0 when unknown. */
  size: number;
}

/** A file picked or dropped to attach. */
export interface LocalFile {
  path: string;
  name: string;
  size: number;
}

/** A file on an outgoing message. Mirrors `OutgoingAttachment` on the Rust side. */
export type OutgoingAttachment =
  | { kind: "file"; path: string; name: string; size: number }
  /** Attachment `index` of a stored message, fetched from the server when sending (forwards). */
  | { kind: "message"; message: number; index: number; name: string; size: number };

export interface OutgoingDraft {
  account: AccountId;
  /** Comma- or semicolon-separated "Name <addr>" or bare addresses. */
  to: string;
  cc: string;
  bcc: string;
  subject: string;
  /** Plain text. */
  body: string;
  /** The message being replied to, so the reply is threaded. */
  replyTo?: number;
  /** Unix ms: the end of the undo window for a normal send, or the Send Later time. */
  sendAt: number;
  attachments: OutgoingAttachment[];
  /** The saved draft this was opened from or saved as; sending deletes it. */
  draftId?: number;
  /** Threading for a reply whose original isn't stored (a draft from the server). */
  inReplyTo?: string;
  references?: string;
}

/** Payload of the `sync-status` event. */
export interface SyncStatusEvent extends SyncStatus {
  account: AccountId;
}

/** Payload of the `send-status` event; `id` is the outbox id `sendMessage` returned. */
export interface SendStatusEvent {
  id: number;
  status: "sent" | "failed";
  message?: string;
}
