/**
 * Web and email addresses written out in an email's text, which Gmail and Outlook show as links:
 * `https://…` and `http://…`, `www.…` (opened over http, as they do) and `name@domain`.
 */
const LINK =
  /\bhttps?:\/\/[^\s<>"]+|(?<![\w.@/-])www\.[^\s<>".]+\.[^\s<>"]+|(?<![\w.%+-])[\w.%+-]+@[a-z0-9-]+(?:\.[a-z0-9-]+)*\.[a-z]{2,}\b/gi;

/** A cheap test for whether text might have an address in it at all. */
export const MAYBE_LINK = /https?:\/\/|www\.|@/i;

/** Where an address is in the text (`text.slice(start, end)`), and the link it opens. */
export interface FoundLink {
  start: number;
  end: number;
  href: string;
}

/** The addresses in `text`, in order. */
export function findLinks(text: string): FoundLink[] {
  const links: FoundLink[] = [];
  for (const match of text.matchAll(LINK)) {
    const raw = match[0];
    const start = match.index;
    if (raw.includes("@") && !/^(?:https?:\/\/|www\.)/i.test(raw)) {
      links.push({ start, end: start + raw.length, href: `mailto:${raw}` });
      continue;
    }
    const url = trimEnd(raw);
    // Punctuation is all that followed "https://", or the "www." domain lost its last part.
    if (!/^(?:https?:\/\/.|www\.[^.]+\.[^.])/i.test(url)) continue;
    const href = /^www\./i.test(url) ? `http://${url}` : url;
    links.push({ start, end: start + url.length, href });
  }
  return links;
}

const count = (text: string, char: string) => text.split(char).length - 1;

/**
 * Trailing punctuation usually ends the sentence, not the address. A closing bracket stays when
 * the address opened it, as in `https://en.wikipedia.org/wiki/Mercury_(planet)`.
 */
function trimEnd(url: string) {
  for (;;) {
    const last = url.at(-1)!;
    const open = { ")": "(", "]": "[", "}": "{" }[last];
    if (".,;:!?'\"*".includes(last) || (open && count(url, open) < count(url, last))) url = url.slice(0, -1);
    else return url;
  }
}
