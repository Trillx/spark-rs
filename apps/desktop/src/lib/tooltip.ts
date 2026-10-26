import {
  ArrowDownLeft,
  CalendarClock,
  Check,
  CircleCheck,
  Clock,
  Paperclip,
  Pin,
  Sparkles,
  ThumbsDown,
  ThumbsUp,
  Zap,
  type LucideIcon,
} from "lucide-react";

/**
 * What a hover tooltip says: a label, optionally its shortcut, a second line,
 * or one of Spark's feature cards. A bare string is just the label.
 */
export type Tip = string | { label: string; keys?: string; detail?: string; card?: CardId };

/** Spark's feature cards: a question, the action that answers it, and a line on what it does. */
export const CARDS = {
  done: {
    question: "All set with this one?",
    action: "Mark it as Done",
    body: "It leaves your Inbox but stays searchable. Spark archives it on the server.",
    Icon: Check,
    tint: "text-success",
  },
  priority: {
    question: "Important email?",
    action: "Mark it as Priority",
    body: "Spark will put it on top of your inbox list so it's always at hand.",
    Icon: Zap,
    tint: "text-priority",
  },
  pin: {
    question: "Need it close by?",
    action: "Pin it",
    body: "Pinned emails stay in their own section at the top of your Inbox until you unpin them.",
    Icon: Pin,
    tint: "text-pin",
  },
  aside: {
    question: "Not now, but soon?",
    action: "Set it Aside",
    body: "Clears it from your Inbox without a deadline. Find it under Set Aside when you're ready.",
    Icon: ArrowDownLeft,
    tint: "text-primary",
  },
  snooze: {
    question: "Deal with it later?",
    action: "Snooze it",
    body: "It leaves your Inbox until the time you pick, then comes back on top, unread.",
    Icon: Clock,
    tint: "text-priority",
  },
  summary: {
    question: "Long email?",
    action: "Summarize it with +AI",
    body: "Get the gist, a detailed summary or the action points, using your own AI key.",
    Icon: Sparkles,
    tint: "text-ai-time",
  },
  accept: {
    question: "Looks relevant?",
    action: "Accept the sender",
    body: "Messages from accepted senders go straight to the Inbox.",
    Icon: ThumbsUp,
    tint: "text-primary",
  },
  block: {
    question: "Not for you?",
    action: "Block the sender",
    body: "Their emails skip your Inbox from now on. Nothing is deleted on the server.",
    Icon: ThumbsDown,
    tint: "text-error",
  },
  sendLater: {
    question: "Writing at odd hours?",
    action: "Schedule it",
    body: "Pick when it goes out. Until then you can still cancel it.",
    Icon: CalendarClock,
    tint: "text-primary",
  },
  doneAfter: {
    question: "Reply and move on?",
    action: "Done after sending",
    body: "The email you're answering leaves your Inbox once this goes out.",
    Icon: CircleCheck,
    tint: "text-success",
  },
  attach: {
    question: "Sending a file?",
    action: "Attach it",
    body: "Pick files, or drop them anywhere on the window. Up to 25 MB in total.",
    Icon: Paperclip,
    tint: "text-primary",
  },
} satisfies Record<string, { question: string; action: string; body: string; Icon: LucideIcon; tint: string }>;

export type CardId = keyof typeof CARDS;

export function tipLabel(tip: Tip) {
  return typeof tip === "string" ? tip : tip.label;
}

/** The data attributes the tooltip layer reads; spread them onto the element. */
export function tipAttrs(tip: Tip | undefined) {
  if (tip === undefined) return {};
  const t = typeof tip === "string" ? { label: tip } : tip;
  return {
    "data-tip": t.label,
    "data-tip-keys": t.keys,
    "data-tip-detail": t.detail,
    "data-tip-card": t.card,
  };
}

/** "Ctrl+Shift+R" → ["Ctrl", "Shift", "R"]; "⇧⌘R" → ["⇧", "⌘", "R"]. */
export function keyCaps(keys: string): string[] {
  if (keys.length > 1 && keys.includes("+")) return keys.split("+");
  const caps: string[] = [];
  let rest = keys;
  while (rest.length > 1 && "⇧⌥⌘⌃".includes(rest[0])) {
    caps.push(rest[0]);
    rest = rest.slice(1);
  }
  return [...caps, rest];
}
