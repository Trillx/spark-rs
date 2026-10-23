import { useEffect, useRef, useState } from "react";
import { searchMessages, searchServers } from "../api";
import type { Message } from "../types";
import { searchesServers, searchText, type SearchHits } from "./layout";

/** How long a search rests before it goes to the mail servers, as in Spark. */
const SERVER_DELAY_MS = 1000;

/**
 * The search index's answers for the free text of `queries`, asked again when the mail
 * changes. Until a new text is answered the old answers stay, and outside the app, where
 * there's no index, there are none: searches then match the loaded messages.
 */
export function useSearchHits(queries: string[], messages: Message[]): SearchHits {
  const [hits, setHits] = useState<SearchHits>(() => new Map());
  const texts = [...new Set(queries.map(searchText).filter(Boolean))].join("\n");

  useEffect(() => {
    if (!texts) return;
    const asked = texts.split("\n");
    let live = true;
    Promise.all(asked.map(searchMessages)).then(
      (answers) => {
        if (!live) return;
        setHits(new Map(asked.flatMap((text, i) => (answers[i] ? [[text, new Set(answers[i])] as const] : []))));
      },
      () => {},
    );
    return () => {
      live = false;
    };
  }, [texts, messages]);

  return hits;
}

/**
 * Puts `query` (null: none) to the mail servers once it has rested, for mail that isn't stored
 * yet, and calls `reload` when they found some. True from the first keystroke until they've
 * answered, so the list says it's still searching rather than that nothing was found.
 */
export function useServerSearch(query: string | null, reload: () => Promise<void>): boolean {
  const [asking, setAsking] = useState(false);
  const reloadRef = useRef(reload);
  reloadRef.current = reload;
  const wanted = query !== null && searchesServers(query) ? query.trim() : "";

  useEffect(() => {
    if (!wanted) return;
    let live = true;
    setAsking(true);
    const timer = setTimeout(() => {
      searchServers(wanted)
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
  }, [wanted]);

  return asking;
}
