// Spark-style shortcuts. Single keys only; no chords.

import type { CardId, Tip } from "./tooltip";

export type ActionId =
  | "next"
  | "prev"
  | "open"
  | "close"
  | "done"
  | "snooze"
  | "pin"
  | "aside"
  | "read"
  | "priority"
  | "replyAll"
  | "reply"
  | "forward"
  | "compose"
  | "search"
  | "sidebar"
  | "command"
  | "settings"
  | "summarize"
  | "summarizeDetailed"
  | "summarizeActions"
  | "toggleDone"
  | "trash"
  | "modeFocused"
  | "modeCards"
  | "modeSimple"
  | "pageDown"
  | "pageUp"
  | "top"
  | "bottom"
  | "selectAll"
  | "selectNext"
  | "selectPrev"
  | "acceptSender"
  | "acceptAndOpen"
  | "acceptAll"
  | "acceptDomain"
  | "blockSender"
  | "blockAll"
  | "blockDomain"
  | "zoomIn"
  | "zoomOut"
  | "zoomReset"
  | "move"
  | "findInThread";

interface Binding {
  /** `KeyboardEvent.key`, lowercased for letters. */
  key?: string;
  /** `KeyboardEvent.code`, for digits under Alt where `key` varies by layout. */
  code?: string;
  /** Cmd on macOS, Ctrl elsewhere. */
  mod?: boolean;
  alt?: boolean;
  shift?: boolean;
}

export const IS_MAC = /Mac|iPhone|iPad/.test(navigator.platform || navigator.userAgent);

const SHORTCUTS: Record<ActionId, Binding[]> = {
  next: [{ key: "ArrowDown" }, { key: "ArrowRight" }],
  prev: [{ key: "ArrowUp" }, { key: "ArrowLeft" }],
  open: [{ key: "Enter" }],
  close: [{ key: "Escape" }],
  done: [{ key: "e" }],
  snooze: [{ key: "s" }],
  pin: [{ key: "d" }],
  aside: [{ key: "g" }],
  read: [{ key: "u" }],
  priority: [{ key: "i" }],
  replyAll: [{ key: "r" }],
  reply: [{ key: "r", shift: true }],
  forward: [{ key: "f" }],
  compose: [{ key: "c" }, { key: "n", mod: true }],
  search: [{ key: "f", mod: true }],
  sidebar: [{ key: "/" }],
  command: [{ key: "k", mod: true }],
  settings: [{ key: ",", mod: true }],
  /** Summarize: Short, Detailed and Action Points. */
  summarize: [{ key: "s", mod: true, shift: true }],
  summarizeDetailed: [{ key: "d", mod: true, shift: true }],
  summarizeActions: [{ key: "a", mod: true, shift: true }],
  toggleDone: [{ key: "e", mod: true }],
  trash: [{ key: "Delete" }, { key: "Backspace" }],
  modeFocused: [{ code: "Digit1", mod: true, alt: true }],
  modeCards: [{ code: "Digit2", mod: true, alt: true }],
  modeSimple: [{ code: "Digit3", mod: true, alt: true }],
  pageDown: [{ key: "PageDown" }],
  pageUp: [{ key: "PageUp" }],
  top: [{ key: "Home" }, { key: "ArrowUp", mod: true }],
  bottom: [{ key: "End" }, { key: "ArrowDown", mod: true }],
  /** Selecting conversations for a batch action: all of the list, or one more below or above. */
  selectAll: [{ key: "a", mod: true }],
  selectNext: [{ key: "ArrowDown", shift: true }],
  selectPrev: [{ key: "ArrowUp", shift: true }],
  /** The Gatekeeper's, as in Spark. E is Accept and Done for an email from a new sender. */
  acceptSender: [{ key: "t", mod: true }],
  acceptAndOpen: [{ key: "t" }],
  acceptAll: [{ key: "t", mod: true, shift: true }],
  acceptDomain: [{ code: "KeyT", mod: true, alt: true, shift: true }],
  blockSender: [{ key: "b", mod: true }],
  blockAll: [{ key: "b", mod: true, shift: true }],
  blockDomain: [{ code: "KeyB", mod: true, alt: true, shift: true }],
  /** Ctrl+= or Ctrl++ (with Shift, or on the keypad), Ctrl+- and Ctrl+0. */
  zoomIn: [{ key: "=", mod: true }, { key: "+", mod: true, shift: true }, { key: "+", mod: true }],
  zoomOut: [{ key: "-", mod: true }],
  zoomReset: [{ key: "0", mod: true }],
  /** Move to folder. */
  move: [{ key: "m" }],
  /** Search in the open conversation. */
  findInThread: [{ key: "f", mod: true, shift: true }],
};

function bindingMatches(b: Binding, e: KeyboardEvent) {
  const mod = IS_MAC ? e.metaKey : e.ctrlKey;
  const otherMod = IS_MAC ? e.ctrlKey : e.metaKey;
  if (Boolean(b.mod) !== mod || otherMod || Boolean(b.alt) !== e.altKey) return false;
  // "/" needs Shift on many non-US layouts, so Shift is ignored for it.
  if (b.key !== "/" && Boolean(b.shift) !== e.shiftKey) return false;
  if (b.code) return e.code === b.code;
  const key = e.key.length === 1 ? e.key.toLowerCase() : e.key;
  return key === b.key;
}

export function matchShortcut(e: KeyboardEvent): ActionId | null {
  for (const [id, bindings] of Object.entries(SHORTCUTS) as [ActionId, Binding[]][]) {
    if (bindings.some((b) => bindingMatches(b, e))) return id;
  }
  return null;
}

/** True for shortcuts with Cmd/Ctrl, which still work while a plain input has focus. */
export function isModShortcut(e: KeyboardEvent) {
  return IS_MAC ? e.metaKey : e.ctrlKey;
}

const KEY_NAMES: Record<string, string> = {
  ArrowDown: "↓",
  ArrowUp: "↑",
  ArrowRight: "→",
  ArrowLeft: "←",
  Escape: "Esc",
  Delete: "Del",
  Backspace: "⌫",
  Enter: "Enter",
};

function bindingLabel(b: Binding) {
  const key = b.code ? b.code.replace(/^(Digit|Key)/, "") : (KEY_NAMES[b.key!] ?? b.key!.toUpperCase());
  if (IS_MAC) return `${b.shift ? "⇧" : ""}${b.alt ? "⌥" : ""}${b.mod ? "⌘" : ""}${key}`;
  return [b.mod && "Ctrl", b.alt && "Alt", b.shift && "Shift", key].filter(Boolean).join("+");
}

/** Display text for an action's primary binding, e.g. "Shift+R" or "Ctrl+K". */
export function shortcutLabel(id: ActionId) {
  return bindingLabel(SHORTCUTS[id][0]);
}

/** A tooltip with the action's shortcut, optionally as a feature card. Spark shows hints in tooltips, not a footer. */
export function hint(label: string, id: ActionId, card?: CardId): Tip {
  return { label, keys: shortcutLabel(id), card };
}

/** Where single-key shortcuts must not fire. */
export function isTypingTarget(target: EventTarget | null) {
  return target instanceof Element && target.closest("input, textarea, select, [contenteditable='true']") !== null;
}

/** The composer and dialogs handle their own keys. */
export function isInsideOverlay(target: EventTarget | null) {
  return target instanceof Element && target.closest("[data-composer], dialog, [role='dialog']") !== null;
}
