import type { Later } from "./lib/format";

// Mirrors the serde types on the Rust side (camelCase JSON).

export type AccountId = number;
export type AccountKind = "gmail" | "microsoft" | "imap";
export type SyncState = "idle" | "syncing" | "error";

export interface SyncStatus {
  status: SyncState;
  message?: string;
  /** Unix ms of the last finished sync. */
  lastSync?: number;
  /** It failed on a server certificate the user may trust. */
  certificate?: Untrusted;
}

/** A server certificate the system doesn't trust (an IMAP account's IMAP or SMTP server). */
export interface Untrusted {
  service: "imap" | "smtp";
  host: string;
  /** SHA-256, as colon-separated hex pairs; trusting the certificate records it. */
  fingerprint: string;
  /** Why the system refused it; it finishes "…isn't trusted: ". */
  reason: string;
}

/** How some commands fail: the message, and the certificate if that's what it was. */
export interface Failure {
  message: string;
  certificate?: Untrusted;
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
  /** The server has inbox mail older than what's synced. */
  hasOlder: boolean;
}

export type Category = "people" | "notifications" | "newsletters";

/** Settings → About, also used for the sidebar's update dot. */
export interface UpdatesView {
  version: string;
  build: number | null;
  commit: string | null;
  canUpdate: boolean;
  checking: boolean;
  installing: boolean;
  checkedAt: number | null;
  available: {
    build: number;
    behind: { count: number; estimated: boolean; commits: { sha: string; subject: string }[] };
  } | null;
  error: string | null;
}

export interface UpdateProgress {
  downloaded: number;
  total: number | null;
}
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
  /** Reply-To header recipients; absent or empty falls back to From. */
  replyTo?: string[];
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
  /** Messages of one conversation share it; without one, a message is a conversation of its own. */
  conversation?: number;
  /** When it came back from Snooze: the Inbox sorts it there, at the top. */
  returnedAt?: number;
  /** Its conversation is muted: new mail in it doesn't notify. */
  muted?: boolean;
  /** A reminder that came due on it (`at`): it's in the Inbox until Done. */
  reminder?: { id: number; at: number };
  /** The calendar invite it carries, by kind, for the Invitations and Responses cards. */
  inviteKind?: "invitation" | "reply" | "cancellation" | "proposal";
}

/** Mail scheduled with Send Later; `id` is its outbox id. */
export interface ScheduledView {
  id: number;
  account: AccountId;
  to: string;
  subject: string;
  snippet: string;
  sendAt: number;
}

/** A "Remind me" on mail you sent: still to come, or `due` and back in the Inbox as `message`. */
export interface ReminderView {
  id: number;
  account: AccountId;
  to: string;
  subject: string;
  remindAt: number;
  ifNoReply: boolean;
  due: boolean;
  message?: number;
}

/** The Gatekeeper's decision about a sender or a whole domain, for every account. */
export type SenderDecision = "accepted" | "blocked";

/** Who a decision is about: one address, or everyone at a domain. */
export type Screen = { sender: string } | { domain: string };

/** A decision to make, or (`null`) to forget, making the sender new again. */
export interface SenderChange {
  screen: Screen;
  decision: SenderDecision | null;
}

/** What's decided about an address and its domain; its own decision wins. */
export interface SenderStatus {
  sender: SenderDecision | null;
  domain: SenderDecision | null;
  /** A free mail provider's (gmail.com…): it can't be decided as a whole. */
  publicDomain: boolean;
}

/** What `prioritizeSenders` changed: these senders and emails go back to `priority`. */
export interface PriorityUndo {
  senders: string[];
  messages: number[];
  priority: boolean;
}

/** Where new senders wait: New Senders above the Inbox, in the Inbox asking when opened, or nowhere. */
export type GatekeeperMode = "beforeInbox" | "insideInbox" | "off";

/** How Unsubscribe went: done by the list's one-click link, an email sent to the list, or a page to finish on. */
export type Unsubscribed = { kind: "done" } | { kind: "email"; to: string } | { kind: "page"; url: string };

/** What a server folder is for. User folders and labels have none. */
export type FolderRole = "sent" | "drafts" | "archive" | "trash" | "spam" | "flagged" | "blocked";

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
  /** It has messages older than the ones synced. */
  hasOlder: boolean;
}

/** Which new mail shows a notification. `people` is Spark's smart notifications. */
export type NotifyFor = "people" | "all" | "priority" | "off";

/** What the app icon's badge counts: unread conversations in the Inbox, all of them, or nothing. */
export type BadgeCount = "new" | "inbox" | "off";

/** Someone to suggest in a recipient field or an @mention. */
export interface Contact {
  /** "" when their mail gave no name. */
  name: string;
  address: string;
}

/** Settings → Notifications and Composer. */
export interface Prefs {
  notify: NotifyFor;
  /** Windows' New Mail sound with each notification. */
  sound: boolean;
  /** A notification when snoozed mail comes back. */
  snoozeAlert: boolean;
  badge: BadgeCount;
  /** Accounts the badge leaves out. */
  badgeExcluded: AccountId[];
  /** Closing the window leaves the app running in the notification area. */
  keepRunning: boolean;
  /** Start in the background when the user signs in to Windows. */
  openAtLogin: boolean;
  /** A notification at each event's alert time. */
  eventAlerts: boolean;
  /** The meeting pop-up before each meeting. */
  meetingPopup: boolean;
  /** How many minutes before a meeting the pop-up shows: 0 (as it starts), 1, 2, 3, 5, 10 or 15. */
  meetingPopupMinutes: number;
  /** Leave events with no one else and no call out of the pop-up. */
  meetingPopupGuestsOnly: boolean;
  /** A chime as a meeting appears in the pop-up. */
  meetingPopupSound: boolean;
  /** Suggest people from the user's mail while they type in To, Cc and Bcc. */
  suggestRecipients: boolean;
  /** How long Undo can stop a message after Send, in seconds. */
  undoSendSeconds: number;
  /** Replies start with "Mark as Done after sending" on. */
  sendAndDone: boolean;
  /** Addresses every new message from an account gets in Cc and Bcc, by account id. */
  autoCc: Record<string, string>;
  autoBcc: Record<string, string>;
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
  Pick<Message, "state" | "unread" | "pinned" | "priority" | "snoozedUntil" | "folder">
>;

export interface MessageContent {
  /** Plain-text paragraphs, always present (derived from the HTML if needed). */
  text: string[];
  /** The email's own HTML. Untrusted: only ever rendered in the sandboxed frame. */
  html: string | null;
  /** Display strings, e.g. "Ana Ruiz <ana@x.com>". */
  to: string[];
  cc: string[];
  replyTo?: string[];
  attachments: Attachment[];
  /** The calendar invite it carries; the thread shows it in place of the body. */
  invite?: InviteCard;
}

/** What an invite email's calendar part is for (its METHOD). `event` is one to look at (PUBLISH). */
export type InviteKind = "invitation" | "reply" | "cancellation" | "proposal" | "event";
export type InviteAnswer = "accepted" | "tentative" | "declined";

export interface InvitePerson {
  /** Their name, else the address. */
  name: string;
  address: string;
  /** Absent until they answer. */
  answer?: InviteAnswer;
}

export interface InviteCard {
  kind: InviteKind;
  summary: string;
  /** Unix ms. All-day and floating times are the times as written, read as UTC. */
  start: number;
  end?: number;
  allDay: boolean;
  /** No time zone: shown as written, wherever the reader is. */
  floating: boolean;
  /** A recurring event; answering answers the series. */
  repeats: boolean;
  location?: string;
  description?: string;
  /** A video call to join. */
  callLink?: string;
  organizer?: InvitePerson;
  /** Everyone else invited, the user included. */
  attendees: InvitePerson[];
  canceled: boolean;
  /** The user's answer: sent from here, else what the invitation records. */
  answer?: InviteAnswer;
  /** Someone else's invitation, still on. */
  canAnswer: boolean;
}

/** Whether an account's calendar can show: `signIn` means its sign-in lacks calendar permission; `unsupported` is Gmail and IMAP for now. */
export type CalendarStatus = "ready" | "signIn" | "unsupported" | "error";

export interface CalendarAccountState {
  account: AccountId;
  status: CalendarStatus;
  /** Why, for `error`. */
  message?: string;
}

/** One calendar of an account. */
export interface CalendarInfo {
  id: number;
  account: AccountId;
  name: string;
  /** Hex color, e.g. "#0ea5e9". Applied with inline styles. */
  color: string;
  isDefault: boolean;
}

export interface CalendarList {
  calendars: CalendarInfo[];
  /** Every account, in order, with whether its calendar can show. */
  accounts: CalendarAccountState[];
}

/** The user's answer to an event; `organizer` for their own events, `none` before they answer. */
export type EventResponse = "organizer" | "accepted" | "tentative" | "declined" | "none";

export interface CalendarEvent {
  id: number;
  calendar: number;
  account: AccountId;
  title: string;
  /** Unix ms; [start, end). All-day events give their days as written, read as UTC midnights. */
  start: number;
  end: number;
  allDay: boolean;
  location?: string;
  /** A video call to join (Teams, Meet, Zoom, …). */
  callLink?: string;
  /** The description, as plain text. */
  notes?: string;
  /** The event in Outlook on the web. */
  webLink?: string;
  organizer?: InvitePerson;
  /** Everyone else invited, rooms left out. */
  attendees: InvitePerson[];
  response: EventResponse;
  canceled: boolean;
  /** An occurrence of a recurring event. */
  repeats: boolean;
  showAs: "free" | "tentative" | "busy" | "oof" | "workingElsewhere" | "unknown";
  private: boolean;
  /**
   * Outlook reminds the user this many minutes before the start (0: as it starts); absent for no reminder.
   * All-day events count back from the midnight their day starts.
   */
  reminder?: number;
}

/** A meeting in the pop-up (src-tauri/src/meetings.rs). */
export interface PopupMeeting {
  /** The event's; negative for the sample from Settings. */
  id: number;
  title: string;
  /** Unix ms. */
  start: number;
  end: number;
  callLink?: string;
  /** Who's in it besides the user, the organizer first. */
  people: InvitePerson[];
  /** Joined from the pop-up: the card stays, with Open meeting instead of Join. */
  joined: boolean;
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
  /** The chosen schedule while editing; saving keeps sendAt at zero. */
  sendLater?: Later;
  attachments: OutgoingAttachment[];
  /** The saved draft this was opened from or saved as; sending deletes it. */
  draftId?: number;
  /** Threading for a reply whose original isn't stored (a draft from the server). */
  inReplyTo?: string;
  references?: string;
  /** The signature below the body, as HTML; absent sends without one. */
  signature?: string;
  /** The saved signature it started as; sending makes it the address's most recent. */
  signatureId?: number;
  /** A forward's original, below the signature. Plain text. */
  quote?: string;
  /** "Remind me", set once it's sent. */
  remind?: { at: number; ifNoReply: boolean };
}

/** A saved signature. Mirrors `Signature` on the Rust side. */
export interface Signature {
  id: number;
  /** Cleaned HTML. */
  html: string;
  /** As plain text, line by line; empty for one that's only an image. */
  text: string;
}

/** Which signature an address starts new mail with. */
export type DefaultSignature = { type: "mostRecent" } | { type: "none" } | { type: "custom"; id: number };

export interface SignaturesView {
  /** Oldest first. */
  signatures: Signature[];
  /** Each sending address's default, in account order. */
  defaults: { mailbox: string; signature: DefaultSignature }[];
}

/** Payload of the `sync-status` event. */
export interface SyncStatusEvent extends SyncStatus {
  account: AccountId;
}

/** Payload of the `send-status` event; `id` is the outbox id `sendMessage` returned. */
export interface SendStatusEvent {
  id: number;
  /** The account it was sent from. */
  account: AccountId;
  /** The exact failed email recovered in Drafts, also for agent sends. */
  draftId?: number;
  status: "sent" | "failed";
  message?: string;
  /** It failed on the SMTP server's certificate, which the user may trust. */
  certificate?: Untrusted;
}
