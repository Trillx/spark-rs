import type { ScheduledView } from "../types";

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
