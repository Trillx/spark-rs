import { useEffect, useState } from "react";
import { getMessageContent } from "../api";
import type { MessageContent } from "../types";

// Message bodies load on demand and stay cached while the app runs, so
// reopening a thread (or forwarding it) is instant. Failed loads aren't cached.

const cache = new Map<number, MessageContent>();
const pending = new Map<number, Promise<MessageContent>>();
/** Messages whose remote images the user chose to load. */
const imagesAllowed = new Set<number>();
/** Bumped by `clearContent`, so a load that started before it isn't cached. */
let generation = 0;

export function loadContent(id: number): Promise<MessageContent> {
  const cached = cache.get(id);
  if (cached) return Promise.resolve(cached);
  let request = pending.get(id);
  if (!request) {
    const started = generation;
    const settle = () => {
      if (started === generation) pending.delete(id);
    };
    request = getMessageContent(id).then(
      (content) => {
        settle();
        if (started === generation) cache.set(id, content);
        return content;
      },
      (error: unknown) => {
        settle();
        throw error;
      },
    );
    pending.set(id, request);
  }
  return request;
}

/** After the account set changes, ids may now name other messages. */
export function clearContent() {
  generation++;
  cache.clear();
  pending.clear();
  imagesAllowed.clear();
}

export function remoteImagesAllowed(id: number) {
  return imagesAllowed.has(id);
}

export function allowRemoteImages(id: number) {
  imagesAllowed.add(id);
}

export type ContentState =
  | { status: "loading" }
  | { status: "ready"; content: MessageContent }
  | { status: "error"; message: string };

export function useMessageContent(id: number): [ContentState, () => void] {
  const [attempt, setAttempt] = useState(0);
  const [state, setState] = useState<ContentState>(() => {
    const cached = cache.get(id);
    return cached ? { status: "ready", content: cached } : { status: "loading" };
  });

  useEffect(() => {
    let live = true;
    const cached = cache.get(id);
    if (cached) {
      setState({ status: "ready", content: cached });
      return;
    }
    setState({ status: "loading" });
    loadContent(id).then(
      (content) => live && setState({ status: "ready", content }),
      (error) => live && setState({ status: "error", message: String(error) }),
    );
    return () => {
      live = false;
    };
  }, [id, attempt]);

  return [state, () => setAttempt((n) => n + 1)];
}
