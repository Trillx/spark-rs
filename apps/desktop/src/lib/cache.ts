// A cache of values loaded by id, held to a byte budget. No imports, so
// `npm test` can run it outside the app (see cache.test.ts).

/** Allowed per string, array, object or other value for its header and the slot holding it. */
const VALUE_OVERHEAD = 16;

/**
 * A conservative guess at the bytes a JSON-like value holds: two per UTF-16
 * code unit of every string, keys included (engines often store one), plus
 * `VALUE_OVERHEAD` per value.
 */
export function estimateBytes(value: unknown): number {
  if (typeof value === "string") return VALUE_OVERHEAD + 2 * value.length;
  if (Array.isArray(value)) return value.reduce((sum: number, item) => sum + estimateBytes(item), VALUE_OVERHEAD);
  if (value && typeof value === "object") {
    let sum = VALUE_OVERHEAD;
    for (const [key, item] of Object.entries(value)) sum += estimateBytes(key) + estimateBytes(item);
    return sum;
  }
  return VALUE_OVERHEAD;
}

export interface LoadCache<V> {
  /** The kept value, marked as just used. */
  get(id: number): V | undefined;
  /** The kept value, else `fetch`'s: asks while one is loading share it. */
  load(id: number): Promise<V>;
  /** Replaces a kept value with `change`'s version of it; one that isn't kept stays that way. */
  update(id: number, change: (value: V) => V): void;
  /** Forgets one value, and a load of it under way: that still answers, but isn't kept. The next ask fetches it. */
  forget(id: number): void;
  /** Forgets everything, including loads under way: they still answer, but aren't kept. */
  clear(): void;
  /** The estimated bytes kept. */
  weight(): number;
}

/**
 * Values `fetch`ed by id and kept while their estimated bytes fit `budget`,
 * the least recently used going first. One over `entryMax` is handed to
 * whoever asked but not kept, so one huge value can't push out the rest.
 * Failed loads aren't kept, so the next ask tries again.
 */
export function loadCache<V>({
  fetch,
  budget,
  entryMax,
  weigh = estimateBytes,
}: {
  fetch: (id: number) => Promise<V>;
  budget: number;
  entryMax: number;
  weigh?: (value: V) => number;
}): LoadCache<V> {
  /** In use order, the most recent last. */
  const kept = new Map<number, { value: V; bytes: number }>();
  /** The load under way for each id, standing for its generation: one `forget` or `clear` let go isn't kept. */
  const pending = new Map<number, Promise<V>>();
  let total = 0;

  function discard(id: number) {
    const entry = kept.get(id);
    if (!entry) return;
    kept.delete(id);
    total -= entry.bytes;
  }

  function keep(id: number, value: V) {
    discard(id);
    const bytes = weigh(value);
    if (bytes > entryMax) return;
    kept.set(id, { value, bytes });
    total += bytes;
    for (const [oldest, entry] of kept) {
      if (total <= budget) break;
      kept.delete(oldest);
      total -= entry.bytes;
    }
  }

  function get(id: number) {
    const entry = kept.get(id);
    if (!entry) return undefined;
    kept.delete(id);
    kept.set(id, entry);
    return entry.value;
  }

  return {
    get,
    load(id) {
      const cached = get(id);
      if (cached !== undefined) return Promise.resolve(cached);
      let request = pending.get(id);
      if (!request) {
        /** Whether this is still the load under way for `id`, so what it brings is kept. */
        const current = () => pending.get(id) === request;
        request = fetch(id).then(
          (value) => {
            if (current()) {
              pending.delete(id);
              keep(id, value);
            }
            return value;
          },
          (error: unknown) => {
            if (current()) pending.delete(id);
            throw error;
          },
        );
        pending.set(id, request);
      }
      return request;
    },
    update(id, change) {
      const entry = kept.get(id);
      if (entry) keep(id, change(entry.value));
    },
    forget(id) {
      discard(id);
      pending.delete(id);
    },
    clear() {
      kept.clear();
      pending.clear();
      total = 0;
    },
    weight: () => total,
  };
}
