import type { ContentState } from "./content";

// A long conversation opens and scrolls as quickly as a short one: the reader
// keeps every email's card, but mounts a card's body (and so loads it) only
// while the card is near the screen, a few at a time. A body that scrolls far
// off becomes a placeholder of its last height, so nothing on screen moves.
// This is the bookkeeping, without the DOM: ThreadView's observers say where
// cards are, mounted bodies say what they're loading, and ThreadView mounts
// the bodies `mounted` names. A mounted body whose message changed (a sync
// filled in its files) loads it again the same way, in its turn, showing
// what it had till then (`reload`).

/** Bodies mount this many screens ahead, above and below the reader's. */
export const OVERSCAN = 1;
/** At most this many bodies load at once, content or embedded images; the rest wait their turn. */
export const LOADING_LIMIT = 4;
/** Spark leaves about this much of the email before the one reading starts at in view. */
export const READING_CONTEXT = 100;

/** A box's top and bottom edges, or the screen's, in the same coordinates. */
export interface Span {
  top: number;
  bottom: number;
}

/** Whether `box` comes within `margin` of `screen`. Touching counts, as it does for IntersectionObserver. */
export function overlaps(box: Span, screen: Span, margin = 0) {
  return box.bottom >= screen.top - margin && box.top <= screen.bottom + margin;
}

/**
 * The order bodies get their turn in: the cards on screen, top down, then
 * outwards from them a card at a time, the one below first since reading
 * goes down. With none on screen, conversation order.
 */
export function mountOrder(cards: readonly number[], visible: ReadonlySet<number>): number[] {
  const shown = cards.flatMap((id, i) => (visible.has(id) ? [i] : []));
  if (shown.length === 0) return [...cards];
  const [first, last] = [shown[0], shown[shown.length - 1]];
  const distance = (i: number) => (i < first ? first - i + 0.5 : i > last ? i - last : 0);
  return cards
    .map((id, i) => ({ id, i, d: distance(i) }))
    .sort((a, b) => a.d - b.d || a.i - b.i)
    .map((x) => x.id);
}

/** What the reader knows about its cards. */
export interface Places {
  /** Cards within OVERSCAN of the screen. */
  near: ReadonlySet<number>;
  /** Cards on screen. */
  visible: ReadonlySet<number>;
  /** Cards whose place has been seen at all. */
  known: ReadonlySet<number>;
  /** Mounted bodies loading, or yet to say whether they are. */
  loading: ReadonlySet<number>;
  /** Whether a body would show from the cache now, loading nothing: then it doesn't need a turn. */
  cached: (id: number) => boolean;
  /** Mounted bodies that are stale (see `reload`), loading again or waiting to. */
  stale?: ReadonlySet<number>;
}

/**
 * Which bodies are mounted next. A mounted one stays while it's near, while
 * it loads (so `limit` counts every load in flight, even one scrolled past)
 * and until its place is known. Of `cards` (the expanded ones, oldest first),
 * a near one mounts if it would show from the cache, or in its turn while
 * fewer than `limit` load. A stale body on screen waiting to load again
 * (`reloadTurns`) keeps a turn from bodies off it: what's read comes before
 * what Find or scrolling might reach. When nothing changes, `mounted` itself
 * comes back.
 */
export function mountedBodies(
  cards: readonly number[],
  mounted: ReadonlySet<number>,
  { near, visible, known, loading, cached, stale = new Set() }: Places,
  limit = LOADING_LIMIT,
): ReadonlySet<number> {
  const next = new Set<number>();
  let inFlight = 0;
  for (const id of mounted) {
    if (loading.has(id)) inFlight++;
    if (near.has(id) || loading.has(id) || !known.has(id)) next.add(id);
  }
  const waiting = [...next].filter((id) => visible.has(id) && stale.has(id) && !loading.has(id)).length;
  for (const id of mountOrder(cards, visible)) {
    if (next.has(id) || !near.has(id)) continue;
    if (cached(id)) next.add(id);
    else if (inFlight + (visible.has(id) ? 0 : waiting) < limit) {
      next.add(id);
      inFlight++;
    }
  }
  return same(next, mounted);
}

/** What a mounted body shows (see `useMessageContent`), and from which version of its message (`forgetContent`). */
export interface Shown {
  state: ContentState;
  version: number;
}

/**
 * What a mounted body does as its message changes, `version` being the
 * message's now: nothing while it shows that version, or while it's still
 * loading, as it loads the new version in the turn it has. Showing what it
 * loaded of a version before, or that it couldn't, it's stale: "waiting" for
 * a turn, then "loading" in the one the reader gives it (`turn`, see
 * `reloadTurns`). It shows what it had till the new content comes.
 */
export function reload(shown: Shown, version: number, turn: boolean): "waiting" | "loading" | null {
  if (shown.state.status === "loading" || shown.version === version) return null;
  return turn ? "loading" : "waiting";
}

/**
 * Which stale bodies load their content again (see `reload`). Loading again
 * is one of the `limit` loads, as mounting is, and a body keeps its turn till
 * it's no longer stale. Bodies on screen yet to show anything had their turns
 * first (`mountedBodies`), as a stale one shows what it had meanwhile; bodies
 * off screen wait for stale ones on it. Then, in
 * turn, a near one that isn't loading something else gets one while fewer
 * than `limit` load. When nothing changes, `reloading` itself comes back.
 */
export function reloadTurns(
  cards: readonly number[],
  mounted: ReadonlySet<number>,
  reloading: ReadonlySet<number>,
  { near, visible, loading, stale = new Set() }: Places,
  limit = LOADING_LIMIT,
): ReadonlySet<number> {
  const next = new Set([...reloading].filter((id) => mounted.has(id) && stale.has(id)));
  // Every stale body has its turn, or none is stale.
  if (next.size === stale.size) return same(next, reloading);
  let inFlight = [...mounted].filter((id) => loading.has(id) || next.has(id)).length;
  for (const id of mountOrder(cards, visible)) {
    if (inFlight >= limit) break;
    if (!stale.has(id) || !mounted.has(id) || !near.has(id) || loading.has(id) || next.has(id)) continue;
    next.add(id);
    inFlight++;
  }
  return same(next, reloading);
}

/** `next`, or `was` itself when they hold the same ids. */
function same(next: Set<number>, was: ReadonlySet<number>): ReadonlySet<number> {
  return next.size === was.size && [...next].every((id) => was.has(id)) ? was : next;
}

/** `set` without `id`: `set` itself when it doesn't hold it. */
function without(set: ReadonlySet<number>, id: number): ReadonlySet<number> {
  return set.has(id) ? new Set([...set].filter((x) => x !== id)) : set;
}

/** Where a card is: near the screen or not, on it or not. Either can be left out. */
export interface Place {
  id: number;
  near?: boolean;
  visible?: boolean;
}

export type BodyScheduler = ReturnType<typeof bodyScheduler>;

/**
 * The reader's bodies as reports come in, calling `onChange` with the new
 * mounted set, and the stale bodies that may load again, whenever either
 * changes. The card reading starts at is mounted from the start, without
 * waiting for its place. `cached` says whether a body would show from the
 * cache now; left out, none would.
 */
export function bodyScheduler(
  start: number | undefined,
  onChange: (mounted: ReadonlySet<number>, reloading: ReadonlySet<number>) => void,
  limit = LOADING_LIMIT,
  cached: (id: number) => boolean = () => false,
) {
  let cards: readonly number[] = [];
  /** Every expanded body counts as near (see `cards`). */
  let all = false;
  let mounted: ReadonlySet<number> = new Set(start === undefined ? [] : [start]);
  /** Stale bodies given their turn to load again (see `reloadTurns`): they count as loading. */
  let reloading: ReadonlySet<number> = new Set();
  /** What `onChange` last heard of. */
  let told = { mounted, reloading };
  const near = new Set<number>();
  const visible = new Set<number>();
  const known = new Set<number>();
  /**
   * Cards that left the page or collapsed (see `forget`): whatever `cards`
   * still says, their bodies don't mount until they're placed again or the
   * user expands them.
   */
  const gone = new Set<number>();
  /** Bodies mounted, but not yet rendered to say whether they're loading: they count as loading. */
  const pending = new Set<number>(mounted);
  const loadingContent = new Set<number>();
  const fetchingImages = new Set<number>();
  /** Bodies that said they're stale, loading again or waiting to. */
  const stale = new Set<number>();
  const heights = new Map<number, number>();

  /** A body that can't be showing: it's not mounted, and nothing it was loading counts. */
  const letGo = (id: number) => {
    pending.delete(id);
    loadingContent.delete(id);
    fetchingImages.delete(id);
    stale.delete(id);
    reloading = without(reloading, id);
    mounted = without(mounted, id);
  };

  const tell = () => {
    if (mounted === told.mounted && reloading === told.reloading) return;
    told = { mounted, reloading };
    onChange(mounted, reloading);
  };

  const run = () => {
    const present = cards.filter((id) => !gone.has(id));
    const loading = new Set([...pending, ...loadingContent, ...fetchingImages, ...reloading]);
    const nearNow = all ? new Set([...near, ...present]) : near;
    const places = { near: nearNow, visible, known, loading, cached, stale };
    const next = mountedBodies(present, mounted, places, limit);
    for (const id of next) {
      if (mounted.has(id) || cached(id)) continue;
      pending.add(id);
      loading.add(id);
    }
    mounted = next;
    reloading = reloadTurns(present, mounted, reloading, places, limit);
    tell();
  };

  return {
    get mounted() {
      return mounted;
    },
    /** Stale bodies that may load again now (see `reloadTurns`). */
    get reloading() {
      return reloading;
    },
    /**
     * The expanded cards, oldest first; any other body goes, loading or not.
     * With `everyOne`, each counts as near, so all their bodies mount in turn
     * and none unmounts: Find in Conversation searches them all.
     */
    cards(expanded: readonly number[], everyOne = false) {
      cards = expanded;
      all = everyOne;
      const open = new Set(expanded);
      const bodies = [...mounted, ...pending, ...loadingContent, ...fetchingImages, ...stale];
      for (const id of bodies) if (!open.has(id)) letGo(id);
      run();
    },
    /** Where cards on the page are. */
    place(places: Iterable<Place>) {
      for (const { id, near: isNear, visible: isVisible } of places) {
        gone.delete(id);
        if (isNear !== undefined) {
          mark(near, id, isNear);
          known.add(id);
        }
        if (isVisible !== undefined) mark(visible, id, isVisible);
      }
      run();
    },
    /**
     * A card that left the page, or collapsed: its body goes at once, with
     * the turn it held, and its place is unknown until it's seen again.
     */
    forget(id: number) {
      gone.add(id);
      near.delete(id);
      visible.delete(id);
      known.delete(id);
      letGo(id);
    },
    /**
     * A mounted body's state, all in one report: its content loading, ready
     * or failed, with `images`, ready but fetching its embedded images, and
     * with `isStale`, ready or failed but stale (see `reload`). A stale body
     * waits for its turn to load again, and keeps it till it says it's no
     * longer stale. Null once the body unmounts.
     */
    content(id: number, status: ContentState["status"] | null, images = false, isStale = false) {
      pending.delete(id);
      mark(loadingContent, id, status === "loading");
      mark(fetchingImages, id, status === "ready" && images);
      mark(stale, id, isStale && (status === "ready" || status === "error"));
      if (!stale.has(id)) reloading = without(reloading, id);
      // Only a mounted body reports, so one that does counts, even just after
      // its card was forgotten: Strict Mode remounts a new card to try it.
      if (status !== null && !mounted.has(id)) {
        gone.delete(id);
        mounted = new Set(mounted).add(id);
      }
      run();
    },
    /** Mounts a body now, without waiting its turn: for an email the user expands. */
    mount(id: number) {
      gone.delete(id);
      if (mounted.has(id)) return;
      if (!cached(id)) pending.add(id);
      mounted = new Set(mounted).add(id);
      tell();
    },
    /** A mounted body's height. A hidden reader's zero doesn't count. */
    measure(id: number, height: number) {
      if (height > 0) heights.set(id, height);
    },
    /** A placeholder's height: the body's when it was last mounted, if it has been. */
    height(id: number): number | undefined {
      return heights.get(id);
    },
  };
}

function mark(set: Set<number>, id: number, on: boolean) {
  if (on) set.add(id);
  else set.delete(id);
}

/**
 * Where the reader scrolls to put a card `offset` below its top edge, the
 * card's top being `top` below that edge now. Never above the start.
 */
export function scrollToPlace(scrollTop: number, top: number, offset: number) {
  return Math.max(0, scrollTop + top - offset);
}

/**
 * Whether the reader moved from where it was placed because of the user. The
 * browser also moves it when content above changes size; only a move with
 * the same content is the user's.
 */
export function movedByUser(placed: { top: number; height: number }, now: { top: number; height: number }) {
  return now.height === placed.height && Math.abs(now.top - placed.top) > 1;
}
