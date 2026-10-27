import type { AutoLabel, Message } from "../types";

// Spark's Auto-Labels: the names Gmail and Outlook get too, and the colors
// the chips and sidebar lists show them in.

export const AUTO_LABELS: Record<AutoLabel, { name: string; color: string }> = {
  toRespond: { name: "To respond", color: "#ff5b5b" },
  fyi: { name: "FYI", color: "#3d8bff" },
  comment: { name: "Comment", color: "#9a6bff" },
  notification: { name: "Notification", color: "#ff9f2e" },
  meetingUpdate: { name: "Meeting update", color: "#2cc5d2" },
  actioned: { name: "Actioned", color: "#3ccb6e" },
  marketing: { name: "Marketing", color: "#ff6bc6" },
  news: { name: "News", color: "#e0b000" },
};

/** As Settings and the menus list them. */
export const AUTO_LABEL_ORDER = Object.keys(AUTO_LABELS) as AutoLabel[];

/** The label a conversation shows: its newest email's that has one. */
export function conversationLabel(conversation: Message[]): AutoLabel | undefined {
  for (let i = conversation.length - 1; i >= 0; i--) {
    const label = conversation[i].autoLabel;
    if (label) return label;
  }
  return undefined;
}
