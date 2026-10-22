// Mirrors the serde types in src-tauri/src/mailbox.rs.

export type Account = "personal" | "work";
export type Category = "people" | "notifications" | "newsletters";
/** `done` is Spark's Done (archive); `aside` is Set Aside. */
export type MessageState = "inbox" | "done" | "snoozed" | "aside" | "deleted" | "blocked";

export interface Message {
  id: number;
  account: Account;
  category: Category;
  state: MessageState;
  unread: boolean;
  pinned: boolean;
  priority: boolean;
  /** From a sender not yet accepted through the Gatekeeper. */
  screening: boolean;
  snoozedUntil?: string;
  from: string;
  address: string;
  /** When the message arrived, in Unix milliseconds. */
  timestamp: number;
  subject: string;
  snippet: string;
  body: string[];
  attachment?: string;
}

export type MessagePatch = Partial<
  Pick<Message, "state" | "unread" | "pinned" | "priority" | "screening" | "snoozedUntil">
>;
