import type { FolderView } from "../types";
import { parseAddress } from "./format";
import { folderIdOf, type ListKey } from "./sidebar";

/** Names a row shows before "+N". */
const SHOWN = 3;

/** Sent and Drafts (shared or one account's) name who mail is to; other lists show the sender. */
export function showsRecipients(key: ListKey, folders: FolderView[]) {
  if (key === "sent" || key === "drafts") return true;
  const id = folderIdOf(key);
  const role = id === undefined ? undefined : folders.find((f) => f.id === id)?.role;
  return role === "sent" || role === "drafts";
}

/**
 * Who a Sent or Drafts row is to: the full name of a single recipient, first
 * names of several, the address where there's no name.
 */
export function recipientNames(to: string[] = []) {
  const people = to.map(parseAddress);
  const label = (p: { name: string; address: string }) =>
    people.length === 1 || p.name === p.address ? p.name : p.name.split(/\s+/)[0];
  return {
    shown: people.slice(0, SHOWN).map(label),
    more: Math.max(0, people.length - SHOWN),
    /** For the avatar. */
    first: people[0]?.name,
  };
}
