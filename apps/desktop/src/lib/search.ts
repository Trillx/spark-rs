import { useEffect, useRef, useState } from "react";
import { searchMessages, searchServers } from "../api";
import type { AccountId, Message } from "../types";
import { searchesServers, searchText, type SearchHits } from "./layout";
import { indexSearches } from "./searchIndex";

/** How long a search rests before it goes to the mail servers, as in Spark. */
const SERVER_DELAY_MS = 1000;
/** How long search texts and mail rest before the search index is asked: a word typed fast is asked once. */
const LOCAL_DELAY_MS = 150;

/**
 * The search index's answers for the free text of `queries`, asked again when the mail
 * changes what it finds (new mail, a draft saved), not when it's only read or pinned. Until
 * a new text is answered the old answers stay, and outside the app, where there's no index,
 * there are none: searches then match the loaded messages.
 */
export function useSearchHits(queries: string[], messages: Message[]): SearchHits {
  const [hits, setHits] = useState<SearchHits>(() => new Map());
  const [searches] = useState(() => indexSearches({ ask: searchMessages, show: setHits, delay: LOCAL_DELAY_MS }));
  const texts = [...new Set(queries.map(searchText).filter(Boolean))].join("\n");

  useEffect(() => {
    searches.want(texts ? texts.split("\n") : [], messages);
  }, [searches, texts, messages]);
  useEffect(() => () => searches.stop(), [searches]);

  return hits;
}

/**
 * Puts `query` (null: none) to the mail servers once it has rested, for mail that isn't stored
 * yet, and calls `reload` when they found some. True from the first keystroke until they've
 * answered, so the list says it's still searching rather than that nothing was found.
 */
export function useServerSearch(
  query: string | null,
  reload: () => Promise<void>,
  account?: AccountId,
): boolean {
  const [asking, setAsking] = useState(false);
  const reloadRef = useRef(reload);
  reloadRef.current = reload;
  const wanted = query !== null && searchesServers(query) ? query.trim() : "";

  useEffect(() => {
    if (!wanted) return;
    let live = true;
    setAsking(true);
    const timer = setTimeout(() => {
      searchServers(wanted, account)
        .then((found) => (found > 0 ? reloadRef.current() : undefined))
        .catch(() => {})
        .finally(() => {
          if (live) setAsking(false);
        });
    }, SERVER_DELAY_MS);
    return () => {
      live = false;
      clearTimeout(timer);
      setAsking(false);
    };
  }, [wanted, account]);

  return asking;
}

/** Spark's recent searches: the last few, newest first, kept on this machine. */
const RECENT_KEY = "spark-rs.recent-searches";
const RECENT_MAX = 8;

export function loadRecentSearches(): string[] {
  try {
    const saved: unknown = JSON.parse(localStorage.getItem(RECENT_KEY) ?? "[]");
    return Array.isArray(saved) ? saved.filter((q): q is string => typeof q === "string").slice(0, RECENT_MAX) : [];
  } catch {
    return [];
  }
}

/** Puts `query` first; returns the new list. */
export function rememberSearch(recent: string[], query: string): string[] {
  const q = query.trim();
  if (!q) return recent;
  const next = [q, ...recent.filter((x) => x !== q)].slice(0, RECENT_MAX);
  localStorage.setItem(RECENT_KEY, JSON.stringify(next));
  return next;
}

export function forgetSearch(recent: string[], query: string): string[] {
  const next = recent.filter((x) => x !== query);
  localStorage.setItem(RECENT_KEY, JSON.stringify(next));
  return next;
}
