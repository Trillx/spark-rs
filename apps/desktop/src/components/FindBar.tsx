import { ChevronDown, ChevronUp, Search, X } from "lucide-react";
import { useEffect, useRef, useState, type RefObject } from "react";
import { tipAttrs } from "../lib/tooltip";

/** A match: where it is, and its document (the thread's, or an email's frame). */
interface Match {
  doc: Document;
  range: Range;
  top: number;
  left: number;
}

const ALL = "spark-find";
const CURRENT = "spark-find-current";
/** How matches look, in each email's frame as in the thread around them. */
const HIGHLIGHT_CSS = `::highlight(${ALL}) { background-color: rgba(255, 200, 0, .45); color: inherit; }
::highlight(${CURRENT}) { background-color: #ff9632; color: #000; }`;
/** Elements whose text starts a new run: a match doesn't join the end of one to the start of the next. */
const BLOCKS =
  "address, article, aside, blockquote, center, dd, div, dl, dt, figcaption, figure, footer, h1, h2, h3, h4, h5, h6, header, hr, li, main, nav, ol, p, pre, section, table, td, th, tr, ul";

/** A character as search compares it: lower case, when that's still one character (offsets stay put). */
function fold(c: string) {
  const lower = c.toLowerCase();
  return lower.length === c.length ? lower : c;
}

/** What's searched for: folded, its spaces as one. */
function searchText(query: string) {
  return [...query.trim().replace(/\s+/g, " ")].map(fold).join("");
}

/**
 * The matches of `wanted` (see `searchText`) in the text that shows in `root` of `doc`, with
 * where each is in the window. The text is read as it shows: across formatting, whitespace
 * as one space.
 */
function findIn(doc: Document, root: Node, wanted: string, offset: { top: number; left: number }, out: Match[]) {
  const chars: string[] = [];
  const from: { node: Text; offset: number }[] = [];
  const shows = new Map<Element, boolean>();
  const visible = (el: Element) => {
    let seen = shows.get(el);
    if (seen === undefined) {
      seen = el.checkVisibility?.({ visibilityProperty: true, contentVisibilityAuto: true }) ?? true;
      shows.set(el, seen);
    }
    return seen;
  };
  let block: Element | null = null;
  const walker = doc.createTreeWalker(root, NodeFilter.SHOW_TEXT, {
    acceptNode: (node) =>
      node.parentElement?.closest("script, style, noscript, template, title, [data-find-skip]")
        ? NodeFilter.FILTER_REJECT
        : NodeFilter.FILTER_ACCEPT,
  });
  for (let node = walker.nextNode() as Text | null; node; node = walker.nextNode() as Text | null) {
    const parent = node.parentElement;
    // Text that doesn't show (a folded quote, a hidden preheader) isn't a match to go to.
    if (!parent || !visible(parent)) continue;
    const inBlock = parent.closest(BLOCKS);
    if (inBlock !== block && chars.length && chars[chars.length - 1] !== " ") {
      chars.push(" ");
      from.push({ node, offset: 0 });
    }
    block = inBlock;
    const data = node.data;
    for (let i = 0; i < data.length; i++) {
      let c = data[i];
      if (/\s/.test(c)) {
        if (chars[chars.length - 1] === " ") continue;
        c = " ";
      } else {
        c = fold(c);
      }
      chars.push(c);
      from.push({ node, offset: i });
    }
  }
  const text = chars.join("");
  for (let at = text.indexOf(wanted); at >= 0; at = text.indexOf(wanted, at + wanted.length)) {
    const start = from[at];
    const last = from[at + wanted.length - 1];
    const range = doc.createRange();
    range.setStart(start.node, start.offset);
    range.setEnd(last.node, last.offset + 1);
    const rect = range.getClientRects()[0];
    if (!rect || (!rect.width && !rect.height)) continue;
    out.push({ doc, range, top: offset.top + rect.top, left: offset.left + rect.left });
  }
}

function highlight(doc: Document, all: Range[], current: Range | null) {
  const view = doc.defaultView as (Window & typeof globalThis) | null;
  if (!view?.CSS?.highlights) return;
  if (!doc.getElementById(ALL)) {
    const style = doc.createElement("style");
    style.id = ALL;
    style.textContent = HIGHLIGHT_CSS;
    (doc.head ?? doc.documentElement).append(style);
  }
  view.CSS.highlights.set(ALL, new view.Highlight(...all));
  if (current) view.CSS.highlights.set(CURRENT, new view.Highlight(current));
  else view.CSS.highlights.delete(CURRENT);
}

function unhighlight(doc: Document) {
  const view = doc.defaultView;
  view?.CSS?.highlights?.delete(ALL);
  view?.CSS?.highlights?.delete(CURRENT);
}

/**
 * Spark's search in the open conversation: the matches of what's typed in its emails, the
 * current one in view, Enter for the next and Shift+Enter for the one before.
 */
export default function FindBar({
  root,
  focus,
  onClose,
}: {
  /** What's searched, and scrolls: the thread's emails, their frames included. */
  root: RefObject<HTMLElement | null>;
  /** Changes when Find is asked for again: the field takes focus. */
  focus: number;
  onClose: () => void;
}) {
  const [query, setQuery] = useState("");
  const [matches, setMatches] = useState<Match[]>([]);
  const [index, setIndex] = useState(0);
  const input = useRef<HTMLInputElement>(null);
  /** Documents highlighted, to clear. */
  const marked = useRef(new Set<Document>());
  /** The current match is to come into view: a new search or a move, not the emails changing. */
  const reveal = useRef(false);

  useEffect(() => {
    input.current?.focus();
    input.current?.select();
  }, [focus]);

  // Matches again as the query changes, and as emails load, open, or show or hide a quote.
  useEffect(() => {
    const el = root.current;
    if (!el) return;
    let timer = 0;
    const frames: MutationObserver[] = [];
    const later = (ms: number) => {
      clearTimeout(timer);
      timer = window.setTimeout(search, ms);
    };
    const again = () => later(300);
    function watchFrames() {
      for (const o of frames.splice(0)) o.disconnect();
      for (const frame of el!.querySelectorAll("iframe")) {
        const body = frame.contentDocument?.body;
        if (!body) continue;
        const o = new MutationObserver(again);
        o.observe(body, {
          subtree: true,
          childList: true,
          characterData: true,
          attributes: true,
          attributeFilter: ["hidden", "style", "class", "open"],
        });
        frames.push(o);
      }
    }
    function search() {
      watchFrames();
      const wanted = searchText(query);
      const found: Match[] = [];
      if (wanted) {
        findIn(document, el!, wanted, { top: 0, left: 0 }, found);
        for (const frame of el!.querySelectorAll("iframe")) {
          const doc = frame.contentDocument;
          if (!doc?.body) continue;
          const rect = frame.getBoundingClientRect();
          findIn(doc, doc.body, wanted, { top: rect.top, left: rect.left }, found);
        }
        found.sort((a, b) => a.top - b.top || a.left - b.left);
      }
      setMatches(found);
      setIndex((i) => (found.length ? Math.min(i, found.length - 1) : 0));
    }
    reveal.current = true;
    later(120);
    const observer = new MutationObserver(again);
    observer.observe(el, {
      subtree: true,
      childList: true,
      characterData: true,
      attributes: true,
      attributeFilter: ["hidden"],
    });
    // An email's frame loading doesn't change the thread's own DOM.
    el.addEventListener("load", again, true);
    return () => {
      clearTimeout(timer);
      observer.disconnect();
      for (const o of frames) o.disconnect();
      el.removeEventListener("load", again, true);
    };
  }, [query]);

  // Highlights: every match, and the current one, brought into view when it's new.
  const current = matches[index] ?? null;
  useEffect(() => {
    const docs = new Set(matches.map((m) => m.doc));
    for (const doc of marked.current) if (!docs.has(doc)) unhighlight(doc);
    for (const doc of docs) {
      highlight(
        doc,
        matches.filter((m) => m.doc === doc).map((m) => m.range),
        current?.doc === doc ? current.range : null,
      );
    }
    marked.current = docs;
    const scroller = root.current;
    if (!current || !scroller || !reveal.current) return;
    reveal.current = false;
    // The match itself, which in a long paragraph can be far from where the paragraph starts.
    const frame = current.doc === document ? null : current.doc.defaultView?.frameElement;
    const rect = current.range.getBoundingClientRect();
    const top = (frame?.getBoundingClientRect().top ?? 0) + rect.top;
    const box = scroller.getBoundingClientRect();
    if (top < box.top + 40 || top + rect.height > box.bottom - 40) {
      scroller.scrollBy({ top: top - (box.top + box.height / 2), behavior: "smooth" });
    }
  }, [matches, current]);

  useEffect(
    () => () => {
      for (const doc of marked.current) unhighlight(doc);
    },
    [],
  );

  const go = (by: number) => {
    if (!matches.length) return;
    reveal.current = true;
    setIndex((i) => (i + by + matches.length) % matches.length);
  };

  return (
    <div
      role="search"
      data-find-skip
      className="mx-3 mb-2 flex h-9 shrink-0 items-center gap-2 rounded-lg border border-hairline bg-base-100 px-2.5"
      // Esc closes it from its buttons too, not the conversation.
      onKeyDown={(e) => {
        if (e.key !== "Escape") return;
        e.preventDefault();
        e.stopPropagation();
        onClose();
      }}
    >
      <Search size={15} className="shrink-0 text-text-3" />
      <input
        ref={input}
        aria-label="Search in conversation"
        className="min-w-0 flex-1 bg-transparent text-[13px] outline-none placeholder:text-text-3"
        placeholder="Search in conversation…"
        spellCheck={false}
        value={query}
        onChange={(e) => {
          setQuery(e.target.value);
          setIndex(0);
        }}
        onKeyDown={(e) => {
          if (e.key === "Enter" || e.key === "F3") {
            e.preventDefault();
            go(e.shiftKey ? -1 : 1);
          }
        }}
      />
      {query.trim() !== "" && (
        <span aria-live="polite" className="shrink-0 text-xs text-text-2">
          {matches.length ? `${index + 1} of ${matches.length} results` : "No results"}
        </span>
      )}
      <button
        {...tipAttrs({ label: "Previous", keys: "Shift+Enter" })}
        aria-label="Previous result"
        disabled={!matches.length}
        className="grid size-6 place-items-center rounded-md text-text-2 hover:bg-base-content/10 disabled:opacity-40"
        onClick={() => go(-1)}
      >
        <ChevronUp size={15} />
      </button>
      <button
        {...tipAttrs({ label: "Next", keys: "Enter" })}
        aria-label="Next result"
        disabled={!matches.length}
        className="grid size-6 place-items-center rounded-md text-text-2 hover:bg-base-content/10 disabled:opacity-40"
        onClick={() => go(1)}
      >
        <ChevronDown size={15} />
      </button>
      <button
        {...tipAttrs({ label: "Close", keys: "Esc" })}
        aria-label="Close search"
        className="grid size-6 place-items-center rounded-md text-text-2 hover:bg-base-content/10"
        onClick={onClose}
      >
        <X size={15} />
      </button>
    </div>
  );
}
