import { useEffect, useState } from "react";
import { getInlineImages, getMessageContent } from "../api";
import type { InlineImages, InviteAnswer, MessageContent } from "../types";

// Message bodies load on demand and stay cached while the app runs, so
// reopening a thread (or forwarding it) is instant. Failed loads aren't cached.
// Embedded images come from the server the same way; being data URLs, only
// the most recently shown INLINE_CACHE_MAX messages' are kept.

const cache = new Map<number, MessageContent>();
const pending = new Map<number, Promise<MessageContent>>();
const inlineCache = new Map<number, InlineImages>();
const inlinePending = new Map<number, Promise<InlineImages>>();
const INLINE_CACHE_MAX = 100;
const NO_IMAGES: InlineImages = Object.freeze({});
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

/** An answer sent to the invitation a message carries, kept in its cached content for next time. */
export function cacheInviteAnswer(id: number, answer: InviteAnswer) {
  const content = cache.get(id);
  if (content?.invite) cache.set(id, { ...content, invite: { ...content.invite, answer } });
}

/** After the account set changes, ids may now name other messages. */
export function clearContent() {
  generation++;
  cache.clear();
  pending.clear();
  inlineCache.clear();
  inlinePending.clear();
}

/** Cached embedded images, marked as just used. */
function cachedInline(id: number): InlineImages | null {
  const images = inlineCache.get(id);
  if (!images) return null;
  inlineCache.delete(id);
  inlineCache.set(id, images);
  return images;
}

function loadInlineImages(id: number): Promise<InlineImages> {
  let request = inlinePending.get(id);
  if (!request) {
    const started = generation;
    const settle = () => {
      if (started === generation) inlinePending.delete(id);
    };
    request = getInlineImages(id).then(
      (images) => {
        settle();
        if (started === generation) {
          inlineCache.set(id, images);
          for (const oldest of inlineCache.keys()) {
            if (inlineCache.size <= INLINE_CACHE_MAX) break;
            inlineCache.delete(oldest);
          }
        }
        return images;
      },
      (error: unknown) => {
        settle();
        throw error;
      },
    );
    inlinePending.set(id, request);
  }
  return request;
}

/**
 * The images a message's HTML embeds, fetched when `wanted`: null while
 * they load. A failed fetch shows none and is tried again next time.
 */
export function useInlineImages(id: number, wanted: boolean): InlineImages | null {
  const [images, setImages] = useState<InlineImages | null>(() => (wanted ? cachedInline(id) : NO_IMAGES));

  useEffect(() => {
    if (!wanted) {
      setImages(NO_IMAGES);
      return;
    }
    const cached = cachedInline(id);
    setImages(cached);
    if (cached) return;
    let live = true;
    loadInlineImages(id).then(
      (loaded) => live && setImages(loaded),
      () => live && setImages(NO_IMAGES),
    );
    return () => {
      live = false;
    };
  }, [id, wanted]);

  return images;
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
