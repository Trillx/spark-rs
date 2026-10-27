import { invoke, isTauri } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import demo from "../demo/mailbox.json";
import { announceMailEdited, announceSettingsChanged } from "./api";
import type { AccountId, AutoLabel } from "./types";

// Mirrors src-tauri/src/automation.rs: Spark's Auto-Labels and Auto-Drafts,
// run in the app with the user's own AI provider. They need a real account,
// so outside the app (the browser preview) there's nothing to set up.

export interface DraftInstruction {
  id: number;
  text: string;
  /** Only for this account's mail. */
  accountId?: AccountId;
  createdAt: number;
}

/** Settings → Inbox Automation as saved. */
export interface InboxAutomation {
  labelAccounts: AccountId[];
  /** Labels AI may pick but that aren't applied. */
  labelsOff: AutoLabel[];
  draftAccounts: AccountId[];
  /** Untouched auto-drafts are deleted after this many days; null keeps them. */
  cleanupDays: number | null;
  /** Drafts for senders still waiting in the Gatekeeper. */
  gatekeeperDrafts: boolean;
  /** Used instead of Settings → AI's model. */
  model?: string;
  excluded: string[];
  instructions: DraftInstruction[];
  /** Why it isn't working, if it isn't (e.g. no API key). */
  problem?: string;
}

/**
 * What a save changes. What it leaves out stays as saved, so a page open in another window can't put
 * back what this one changed. `model: null` goes back to Settings → AI's.
 */
export type InboxAutomationChange = Partial<
  Pick<InboxAutomation, "labelAccounts" | "labelsOff" | "draftAccounts" | "cleanupDays" | "gatekeeperDrafts"> & {
    model: string | null;
  }
>;

/** One of the replies an auto-draft offers. */
export interface DraftOption {
  /** A few words for the picker; empty when there's only one. */
  title: string;
  body: string;
}

export interface AutoDraftView {
  /** The email it replies to. */
  messageId: number;
  options: DraftOption[];
  /** The option the draft says now. */
  chosen: number;
}

const DESKTOP_ONLY = "Inbox automation works in the desktop app.";

export function getInboxAutomation(): Promise<InboxAutomation> {
  if (isTauri()) return invoke("get_inbox_automation");
  return Promise.resolve({
    labelAccounts: [],
    labelsOff: [],
    draftAccounts: [],
    cleanupDays: 7,
    gatekeeperDrafts: false,
    excluded: [],
    instructions: [],
  });
}

export function saveInboxAutomation(change: InboxAutomationChange): Promise<InboxAutomation> {
  if (!isTauri()) return Promise.reject(DESKTOP_ONLY);
  return invoke<InboxAutomation>("save_inbox_automation", { change }).then((view) => {
    announceSettingsChanged();
    return view;
  });
}

/** Spark's "Auto-Draft a reply": a reply to `id`, written now. Resolves to the draft's id. */
export function autoDraftReply(id: number): Promise<number> {
  if (!isTauri()) return Promise.reject(DESKTOP_ONLY);
  return invoke<number>("auto_draft_reply", { id }).then((draft) => {
    announceMailEdited();
    return draft;
  });
}

/** The options an auto-draft was written with; null if it isn't one. */
export function getAutoDraft(id: number): Promise<AutoDraftView | null> {
  if (isTauri()) return invoke("get_auto_draft", { id });
  // The demo's one auto-draft, as demo.rs has it.
  const entries = demo as { id: number; autoDraft?: number; autoDraftOptions?: DraftOption[] }[];
  const options = entries.find((e) => e.id === id)?.autoDraftOptions;
  const repliesTo = entries.find((e) => e.autoDraft === id)?.id;
  return Promise.resolve(options && repliesTo !== undefined ? { messageId: repliesTo, options, chosen: 0 } : null);
}

/** The auto-draft says option `index` now. */
export function chooseAutoDraftOption(id: number, index: number): Promise<void> {
  if (!isTauri()) return Promise.reject(DESKTOP_ONLY);
  return invoke<void>("choose_auto_draft_option", { id, index }).then(announceMailEdited);
}

/** `value`, once the app's other windows are told the settings changed (an open Settings reads them again). */
function announced<T>(value: T): T {
  announceSettingsChanged();
  return value;
}

/** Leaves a sender's mail alone (`excluded`), or no longer does. Resolves to the list. */
export function setAutoDraftExcluded(address: string, excluded: boolean): Promise<string[]> {
  if (!isTauri()) return Promise.reject(DESKTOP_ONLY);
  return invoke<string[]>("set_auto_draft_excluded", { address, excluded }).then(announced);
}

export function addDraftInstruction(text: string, account?: AccountId): Promise<DraftInstruction[]> {
  if (!isTauri()) return Promise.reject(DESKTOP_ONLY);
  return invoke<DraftInstruction[]>("add_draft_instruction", { text, account: account ?? null }).then(announced);
}

export function removeDraftInstruction(id: number): Promise<DraftInstruction[]> {
  if (!isTauri()) return Promise.reject(DESKTOP_ONLY);
  return invoke<DraftInstruction[]>("remove_draft_instruction", { id }).then(announced);
}

/** Why inbox automation isn't working changed (`null`: it works now). Returns the unsubscribe. */
export function onAutomationProblem(handler: (problem: string | null) => void): () => void {
  if (!isTauri()) return () => {};
  const off = listen<string | null>("inbox-automation-problem", (e) => handler(e.payload));
  return () => void off.then((unlisten) => unlisten());
}

/** A reply to email `id` started or stopped being written. Returns the unsubscribe. */
export function onAutoDraftWriting(handler: (id: number, writing: boolean) => void): () => void {
  if (!isTauri()) return () => {};
  const off = listen<{ id: number; writing: boolean }>("auto-draft-writing", (e) =>
    handler(e.payload.id, e.payload.writing),
  );
  return () => void off.then((unlisten) => unlisten());
}
