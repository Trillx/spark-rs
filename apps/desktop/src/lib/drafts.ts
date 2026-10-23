import type { FolderView, Message } from "../types";

/** In its account's Drafts folder: opening it edits it in the composer. */
export function isDraft(m: Message, folders: FolderView[]) {
  return m.folders?.some((id) => folders.some((f) => f.id === id && f.role === "drafts")) ?? false;
}
