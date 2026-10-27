import type { ContentState } from "./content";

// A long conversation opens and scrolls as quickly as a short one: the reader
// keeps every email's card, but mounts a card's body (and so loads it) only
// while the card is near the screen, a few at a time. A body that scrolls far
// off becomes a placeholder of its last height, so nothing on screen moves.
// This is the bookkeeping, without the DOM: ThreadView's observers say where
// cards are, mounted bodies say what they're loading, and ThreadView mounts
// the bodies `mounted` names.

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
}

/**
 * Which bodies are mounted next. A mounted one stays while it's near, while
 * it loads (so `limit` counts every load in flight, even one scrolled past)
 * and until its place is known. Of `cards` (the expanded ones, oldest first),
 * a near one mounts if it would show from the cache, or in its turn while
 * fewer than `limit` load. When nothing changes, `mounted` itself comes back.
 */
export function mountedBodies(
  cards: readonly number[],
  mounted: ReadonlySet<number>,
  { near, visible, known, loading, cached }: Places,
  limit = LOADING_LIMIT,
): ReadonlySet<number> {
  const next = new Set<number>();
  let inFlight = 0;
  for (const id of mounted) {
    if (loading.has(id)) inFlight++;
    if (near.has(id) || loading.has(id) || !known.has(id)) next.add(id);
  }
  for (const id of mountOrder(cards, visible)) {
    if (next.has(id) || !near.has(id)) continue;
    if (cached(id)) next.add(id);
    else if (inFlight < limit) {
      next.add(id);
      inFlight++;
    }
  }
  return next.size === mounted.size && [...next].every((id) => mounted.has(id)) ? mounted : next;
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
 * mounted set whenever it changes. The card reading starts at is mounted
 * from the start, without waiting for its place. `cached` says whether a
 * body would show from the cache now; left out, none would.
 */
export function bodyScheduler(
  start: number | undefined,
  onChange: (mounted: ReadonlySet<number>) => void,
  limit = LOADING_LIMIT,
  cached: (id: number) => boolean = () => false,
) {
  let cards: readonly number[] = [];
  /** Every expanded body counts as near (see `cards`). */
  let all = false;
  let mounted: ReadonlySet<number> = new Set(start === undefined ? [] : [start]);
  /** The mounted set `onChange` last heard of. */
  let told = mounted;
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
  const heights = new Map<number, number>();

  /** A body that can't be showing: it's not mounted, and nothing it was loading counts. */
  const letGo = (id: number) => {
    pending.delete(id);
    loadingContent.delete(id);
    fetchingImages.delete(id);
    if (mounted.has(id)) mounted = new Set([...mounted].filter((x) => x !== id));
  };

  const run = () => {
    const present = cards.filter((id) => !gone.has(id));
    const loading = new Set([...pending, ...loadingContent, ...fetchingImages]);
    const nearNow = all ? new Set([...near, ...present]) : near;
    const next = mountedBodies(present, mounted, { near: nearNow, visible, known, loading, cached }, limit);
    for (const id of next) if (!mounted.has(id) && !cached(id)) pending.add(id);
    mounted = next;
    if (mounted !== told) onChange((told = mounted));
  };

  return {
    get mounted() {
      return mounted;
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
      for (const id of [...mounted, ...pending, ...loadingContent, ...fetchingImages]) if (!open.has(id)) letGo(id);
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
     * or failed, and with `images`, ready but fetching its embedded images.
     * Null once the body unmounts.
     */
    content(id: number, status: ContentState["status"] | null, images = false) {
      pending.delete(id);
      mark(loadingContent, id, status === "loading");
      mark(fetchingImages, id, status === "ready" && images);
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
      onChange((told = mounted));
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
