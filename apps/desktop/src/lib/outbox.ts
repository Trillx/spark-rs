import type { QueuedChange, ScheduledView } from "../types";

// Sends that couldn't go yet, as they fail on the network: they stay queued
// and are tried again, and the Scheduled list shows each with why
// (`Engine::waiting_sends`), so none is reachable only through a toast.

/**
 * What the notice says once a send has kept failing a while (`told`, its
 * send-status event): which send, when it's the only one that couldn't go
 * yet, or how many couldn't (`items`, the Scheduled list).
 */
export function stuckNotice(items: ScheduledView[], told: { id: number; message?: string }) {
  const stuck = items.filter((s) => s.failed !== undefined).length;
  if (stuck > 1) return `${stuck} emails couldn't be sent yet. They'll keep trying.`;
  const item = items.find((s) => s.id === told.id);
  const subject = item ? ` “${item.subject.trim() || "(no subject)"}”` : "";
  const why = (told.message ?? "the server can't be reached").replace(/\.$/, "");
  return `Couldn't send${subject} yet: ${why}. It'll keep trying.`;
}

/** Send `id` that couldn't go yet is tried again (its send-status event): its row says so till the try is over. */
export function markTrying(items: ScheduledView[], id: number) {
  if (!items.some((s) => s.id === id)) return items;
  return items.map((s) => (s.id === id ? { ...s, sending: true } : s));
}

/** The draft a change to a queued send leaves in Drafts: it taken back, or refused for good by the server first. */
export function draftLeft(change: QueuedChange) {
  return change.status === "takenBack" || change.status === "failed" ? change.draftId : undefined;
}

/**
 * What to say when a change to a queued send found it gone. Nothing once the server refused it: the failure's own
 * notice says so, and is left up.
 */
export function goneNotice(change: QueuedChange) {
  if (change.status === "sent") return "Already sent";
  if (change.status === "gone") return "It's no longer waiting to be sent";
  return undefined;
}
