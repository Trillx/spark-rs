import { useEffect, useState } from "react";
import { searchMessages } from "../api";
import type { Message } from "../types";
import { searchText, type SearchHits } from "./layout";

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
