import type { Message } from "../types";

/**
 * Whether Unsubscribe is offered on `m`: its List-Unsubscribe header gives a way to leave the list, whatever its
 * category, as in Spark. Mail whose header wasn't read for it (stored before, or from Microsoft 365, whose listings
 * leave headers out) goes by its category instead.
 */
export function canUnsubscribe(m: Message) {
  return m.unsubscribe ?? m.category === "newsletters";
}

/**
 * What unsubscribing from the conversations `threads` goes through: of each sender, the newest email that can be
 * unsubscribed from, so a list is asked once however many of its emails are picked. Your own emails are left out.
 */
export function unsubscribeTargets(threads: Message[][], fromYou: (m: Message) => boolean): Message[] {
  const newest = new Map<string, Message>();
  for (const m of threads.flat()) {
    if (fromYou(m) || !canUnsubscribe(m)) continue;
    const sender = m.address.toLowerCase();
    const kept = newest.get(sender);
    if (!kept || m.timestamp > kept.timestamp) newest.set(sender, m);
  }
  return [...newest.values()];
}

/** "Ana", or "Ana and 2 more", as Spark names the senders of a batch. */
export function andMore(names: string[]) {
  return names.length > 1 ? `${names[0]} and ${names.length - 1} more` : (names[0] ?? "");
}

/** Runs `work` on each of `items`, at most `limit` at a time, and settles each as `Promise.allSettled` does. */
export async function settleEach<T, R>(
  items: T[],
  limit: number,
  work: (item: T) => Promise<R>,
): Promise<PromiseSettledResult<R>[]> {
  const results: PromiseSettledResult<R>[] = new Array(items.length);
  let next = 0;
  async function worker() {
    while (next < items.length) {
      const i = next++;
      try {
        results[i] = { status: "fulfilled", value: await work(items[i]) };
      } catch (reason) {
        results[i] = { status: "rejected", reason };
      }
    }
  }
  await Promise.all(Array.from({ length: Math.min(limit, items.length) }, worker));
  return results;
}
