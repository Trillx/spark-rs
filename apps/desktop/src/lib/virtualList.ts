// The message list renders only the blocks near the screen (rows, date
// headers, cards, New senders) and leaves empty space of the same height for
// the rest. This is its arithmetic: where each block sits, which blocks a
// stretch of the list needs, and where the screen ends up when blocks come
// and go. Positions are px down from the top of the first block.

/** Blocks by index, from `start` up to but not including `end`. */
export interface Span {
  start: number;
  end: number;
}

/** A stretch of the list, in px. */
export interface Stretch {
  from: number;
  to: number;
}

/** Where each block starts, then where the last one ends. */
export function blockOffsets(heights: readonly number[]): number[] {
  const offsets = new Array<number>(heights.length + 1);
  offsets[0] = 0;
  for (let i = 0; i < heights.length; i++) offsets[i + 1] = offsets[i] + heights[i];
  return offsets;
}

/** The block at `y`: the first one that ends below it, or the number of blocks past the end. */
export function blockAt(offsets: readonly number[], y: number): number {
  let lo = 0;
  let hi = offsets.length - 1;
  while (lo < hi) {
    const mid = (lo + hi) >> 1;
    if (offsets[mid + 1] > y) hi = mid;
    else lo = mid + 1;
  }
  return lo;
}

/** The blocks that show, even partly, between `from` and `to`. */
export function blocksBetween(offsets: readonly number[], from: number, to: number): Span {
  const start = blockAt(offsets, from);
  let lo = start;
  let hi = offsets.length - 1;
  while (lo < hi) {
    const mid = (lo + hi) >> 1;
    if (offsets[mid] < to) lo = mid + 1;
    else hi = mid;
  }
  return { start, end: lo };
}

/** Spans in order, with overlapping or touching ones joined and empty ones left out. */
export function joinSpans(spans: readonly Span[]): Span[] {
  const sorted = spans.filter((s) => s.end > s.start).sort((a, b) => a.start - b.start);
  const joined: Span[] = [];
  for (const span of sorted) {
    const last = joined.at(-1);
    if (last && span.start <= last.end) last.end = Math.max(last.end, span.end);
    else joined.push({ ...span });
  }
  return joined;
}

/** The empty space before each span, then after the last one. */
export function gaps(offsets: readonly number[], spans: readonly Span[]): number[] {
  const out: number[] = [];
  let at = 0;
  for (const { start, end } of spans) {
    out.push(offsets[start] - at);
    at = offsets[end];
  }
  out.push(offsets[offsets.length - 1] - at);
  return out;
}

/** What to render for a screen at `top`: it, and another screen above and below. */
export function stretchAround(top: number, height: number): Stretch {
  return { from: top - height, to: top + 2 * height };
}

/** Whether the stretch reaches from `from` to `to`, or as far towards them as a list `total` tall goes. */
export function reaches(stretch: Stretch, from: number, to: number, total: number) {
  return stretch.from <= Math.max(0, from) && stretch.to >= Math.min(total, to);
}

/** Whether what's rendered still reaches half a screen past both edges of the screen at `top`. */
export function covers(stretch: Stretch, top: number, height: number, total: number) {
  return reaches(stretch, top - height / 2, top + 1.5 * height, total);
}

/**
 * Where the screen's top goes when the blocks change, if the first block that
 * shows keeps its place on screen, as the browser's scroll anchoring keeps
 * it. `moved` gives a block's new index, or undefined if it's gone; so is
 * the answer then, and when the screen starts above the blocks.
 */
export function anchoredTop(
  before: readonly number[],
  after: readonly number[],
  top: number,
  moved: (index: number) => number | undefined,
): number | undefined {
  const i = blockAt(before, top);
  if (top < 0 || i >= before.length - 1) return undefined;
  const j = moved(i);
  return j === undefined ? undefined : after[j] + top - before[i];
}
