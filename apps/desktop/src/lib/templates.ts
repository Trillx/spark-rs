import type { Template } from "../types";

/** Spark's template placeholders (`TemplatePlaceholder`); "custom" ones are named by the user. */
export type PlaceholderKind =
  | "custom"
  | "recipientFirstName"
  | "recipientLastName"
  | "recipientFullName"
  | "myFirstName"
  | "myLastName"
  | "myFullName";

/** The ones filled in from who it's to and who it's from, as Spark names them. */
export const PRESET_PLACEHOLDERS: { kind: Exclude<PlaceholderKind, "custom">; label: string }[] = [
  { kind: "recipientFirstName", label: "Recipient's first name" },
  { kind: "recipientLastName", label: "Recipient's last name" },
  { kind: "recipientFullName", label: "Recipient's full name" },
  { kind: "myFirstName", label: "My first name" },
  { kind: "myLastName", label: "My last name" },
  { kind: "myFullName", label: "My full name" },
];

/** A name's first and last parts: "Ana Ruiz", or "Ruiz, Ana" as address books write it. */
export function nameParts(name: string) {
  const full = name.replace(/^["']|["']$/g, "").trim();
  const comma = full.split(",").map((part) => part.trim());
  if (comma.length === 2 && comma[0] && comma[1]) {
    return { first: comma[1].split(/\s+/)[0], last: comma[0], full: `${comma[1]} ${comma[0]}` };
  }
  const words = full.split(/\s+/).filter(Boolean);
  return { first: words[0] ?? "", last: words.length > 1 ? words[words.length - 1] : "", full };
}

/** What each preset placeholder becomes: from the first recipient's name and the sender's own. Unknown ones stay. */
export function placeholderValues(recipient: string, mine: string): Partial<Record<PlaceholderKind, string>> {
  const them = nameParts(recipient);
  const me = nameParts(mine);
  return {
    recipientFirstName: them.first,
    recipientLastName: them.last,
    recipientFullName: them.full,
    myFirstName: me.first,
    myLastName: me.last,
    myFullName: me.full,
  };
}

/** How many templates the Recent section shows. */
const RECENT = 3;

/** The templates applied last, newest first. */
export function recentTemplates(templates: Template[]) {
  return templates
    .filter((t) => t.usedAt !== undefined)
    .sort((a, b) => b.usedAt! - a.usedAt!)
    .slice(0, RECENT);
}
