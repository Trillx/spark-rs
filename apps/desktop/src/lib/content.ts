import { useEffect, useRef, useState, useSyncExternalStore } from "react";
import { getInlineImages, getMessageContent } from "../api";
import type { InlineImages, InviteAnswer, MessageContent } from "../types";
import { loadCache } from "./cache";
import { reload, type Shown } from "./readerViewport";

// Message bodies load on demand and the most recently used stay cached while
// the app runs, so reopening a thread (or forwarding it) is instant. Failed
// loads aren't cached. Embedded images come from the server the same way, kept
// to a budget of their own: being data URLs, they're big. Sizes are estimated
// as two bytes a character (see `estimateBytes`). A message whose stored
// content changes is forgotten, and readers showing it load it again in
// their turn (`forgetContent`).

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
/** How often `forgetContent` forgot each message, for readers showing it to load it again. */
const versions = new Map<number, number>();
const versionWatchers = new Set<() => void>();

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

/**
 * Forgets what's kept of messages whose stored content changed since it was read, loads under way included: a
 * sync filled in the attachments or invite it stored them without (`staleContent`). Readers showing them load
 * them again in their turn, and show what they have till then (`useMessageContent`). Their embedded images didn't
 * change, so readers keep those.
 */
export function forgetContent(ids: readonly number[]) {
  if (!ids.length) return;
  for (const id of ids) {
    contents.forget(id);
    inline.forget(id);
    versions.set(id, (versions.get(id) ?? 0) + 1);
  }
  for (const watcher of versionWatchers) watcher();
}

function watchVersions(watcher: () => void) {
  versionWatchers.add(watcher);
  return () => {
    versionWatchers.delete(watcher);
  };
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

const LOADING: ContentState = { status: "loading" };

/**
 * A message's content as a mounted body shows it, whether that's stale, and
 * how to try again after a failure. Stale, the body shows what it had, and
 * loads the message again only once the reader gives it a `turn` (see
 * `reload`); a reload that fails keeps the content too. A body never has two
 * loads under way, so the reader's limit counts them all: a new one starts
 * once the last has settled.
 */
export function useMessageContent(id: number, turn: boolean): [ContentState, boolean, () => void] {
  const [attempt, setAttempt] = useState(0);
  const version = useSyncExternalStore(watchVersions, () => versions.get(id) ?? 0);
  const [shown, setShown] = useState<Shown & { id: number }>(() => {
    const cached = contents.get(id);
    return { id, version, state: cached ? { status: "ready", content: cached } : LOADING };
  });
  /** Another message's, from before `id` changed, isn't this one's: this one's loading. */
  const current = shown.id === id ? shown : { id, version, state: LOADING };
  const again = reload(current, version, turn);
  const waiting = again === "waiting";
  /** The last load, settled or not. */
  const last = useRef(Promise.resolve());

  useEffect(() => {
    if (waiting) return;
    let live = true;
    const show = (state: ContentState) => setShown({ id, version, state });
    const cached = contents.get(id);
    if (cached) {
      show({ status: "ready", content: cached });
      return;
    }
    // Loading it again, what shows stays till the new content comes.
    if (!again) show(LOADING);
    const start = () => {
      if (!live) return;
      return loadContent(id).then(
        (content) => {
          if (live) show({ status: "ready", content });
        },
        (error) => {
          if (!live) return;
          const failed: ContentState = { status: "error", message: String(error) };
          setShown((was) => ({ id, version, state: again && was.state.status === "ready" ? was.state : failed }));
        },
      );
    };
    last.current = last.current.then(start, start);
    return () => {
      live = false;
    };
  }, [id, attempt, version, waiting]);

  const retry = () => {
    // At once, stale or not: the user asked.
    setShown({ id, version, state: LOADING });
    setAttempt((n) => n + 1);
  };
  return [current.state, again !== null, retry];
}
