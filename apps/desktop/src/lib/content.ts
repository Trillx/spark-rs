import { useEffect, useState } from "react";
import { getInlineImages, getMessageContent } from "../api";
import type { InlineImages, InviteAnswer, MessageContent } from "../types";
import { loadCache } from "./cache";

// Message bodies load on demand and the most recently used stay cached while
// the app runs, so reopening a thread (or forwarding it) is instant. Failed
// loads aren't cached. Embedded images come from the server the same way, kept
// to a budget of their own: being data URLs, they're big. Sizes are estimated
// as two bytes a character (see `estimateBytes`).

const MiB = 1024 * 1024;
/** A few hundred ordinary bodies (their HTML and text, ~100–300 KiB as estimated): modest on an 8 GB ARM laptop. */
const CONTENT_BUDGET = 64 * MiB;
/** 8M characters, which no ordinary email comes near; a bigger one shows but loads again next time. */
const CONTENT_ENTRY_MAX = CONTENT_BUDGET / 4;
/** More room than bodies, as a miss downloads the message again: a few dozen image-heavy newsletters. */
const INLINE_BUDGET = 96 * MiB;
/** About 9 MB of images as base64; attachment.rs allows 20 MiB a message, and sets that heavy aren't kept. */
const INLINE_ENTRY_MAX = INLINE_BUDGET / 4;

const contents = loadCache({ fetch: getMessageContent, budget: CONTENT_BUDGET, entryMax: CONTENT_ENTRY_MAX });
const inline = loadCache({ fetch: getInlineImages, budget: INLINE_BUDGET, entryMax: INLINE_ENTRY_MAX });
const NO_IMAGES: InlineImages = Object.freeze({});

export function loadContent(id: number): Promise<MessageContent> {
  return contents.load(id);
}

/** An answer sent to the invitation a message carries, kept in its cached content for next time. */
export function cacheInviteAnswer(id: number, answer: InviteAnswer) {
  contents.update(id, (content) => (content.invite ? { ...content, invite: { ...content.invite, answer } } : content));
}

/** After the account set changes, ids may now name other messages. */
export function clearContent() {
  contents.clear();
  inline.clear();
}

/** Cached embedded images, marked as just used. */
function cachedInline(id: number): InlineImages | null {
  return inline.get(id) ?? null;
}

/**
 * Whether a message would show from the caches now, loading nothing: its
 * content is kept and, if `embeds` finds embedded images in its HTML, so are
 * they. It loads nothing, though what it finds counts as just used: the
 * reader asks only of emails near the screen, which read the same to show.
 */
export function shownFromCache(id: number, embeds: (html: string) => boolean): boolean {
  const content = contents.get(id);
  return content !== undefined && (content.html === null || !embeds(content.html) || cachedInline(id) !== null);
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
    inline.load(id).then(
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
    const cached = contents.get(id);
    return cached ? { status: "ready", content: cached } : { status: "loading" };
  });

  useEffect(() => {
    let live = true;
    const cached = contents.get(id);
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
