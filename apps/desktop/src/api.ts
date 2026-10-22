import { invoke, isTauri } from "@tauri-apps/api/core";
import demo from "../demo/mailbox.json";
import type { Message, MessagePatch } from "./types";

// Inside the app, data comes from the Rust side. In a plain browser
// (`npm run dev`) we fall back to the same demo file so the UI can be
// developed without building the app.

export function listMessages(): Promise<Message[]> {
  if (isTauri()) return invoke("list_messages");
  return Promise.resolve(structuredClone(browserMailbox));
}

export function updateMessage(id: number, patch: MessagePatch): Promise<Message> {
  if (isTauri()) return invoke("update_message", { id, patch });
  const index = browserMailbox.findIndex((m) => m.id === id);
  if (index < 0) return Promise.reject(new Error(`no message ${id}`));
  browserMailbox[index] = patched(browserMailbox[index], patch);
  return Promise.resolve(structuredClone(browserMailbox[index]));
}

/** Same rules as MessagePatch::apply in mailbox.rs. Used for optimistic updates too. */
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

// Like DemoMailbox::load: each entry's age becomes a timestamp relative to now.
const loadedAt = Date.now();
const browserMailbox: Message[] = demo.map(({ ageMinutes, ...m }) => ({
  state: "inbox",
  unread: false,
  pinned: false,
  priority: false,
  screening: false,
  ...m,
  timestamp: loadedAt - ageMinutes * 60_000,
})) as Message[];
