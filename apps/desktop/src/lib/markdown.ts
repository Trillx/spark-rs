/**
 * Email as Markdown, for Spark's "Copy email as Markdown" and "Copy thread as Markdown":
 * headings, emphasis, links, lists, quotes, code and tables as Markdown writes them, and the
 * rest as its text, escaped so that it reads back as the same text.
 */

/** What a message says about itself, above its body. */
export interface MarkdownHeader {
  from: string;
  to: string[];
  cc: string[];
  date: string;
  subject: string;
}

/** Elements whose text doesn't show. */
const UNSEEN = new Set(["HEAD", "SCRIPT", "STYLE", "TEMPLATE", "TITLE", "NOSCRIPT"]);
/** Elements that sit on lines of their own. */
const BLOCKS = new Set([
  "ADDRESS", "ARTICLE", "ASIDE", "CENTER", "DIV", "DL", "DD", "DT", "FIGURE", "FOOTER", "FORM",
  "HEADER", "MAIN", "NAV", "P", "SECTION", "TABLE", "TBODY", "THEAD", "TFOOT", "TR", "TD", "TH",
]);
/** Elements a table cell holding them is laid out with, rather than a cell of data. */
const LAYOUT = "div, p, table, ul, ol, blockquote, h1, h2, h3, h4, h5, h6, pre, img";
/**
 * Starts the Markdown syntax this writes (`# `, `- `, `> `, fences, table rows), so the final
 * pass can tell it from text that only looks like it. Taken out at the end.
 */
const MARK = "\u0001";

/** Characters Markdown would read as formatting, or as HTML, escaped. */
function escape(text: string) {
  return text.replace(/[\\`*_[\]<>&|~]/g, "\\$&");
}

/** A link's address as Markdown takes it: in <…>, with what can't be there encoded. */
function destination(url: string) {
  return `<${url.replace(/[<>\s]/g, (c) => encodeURIComponent(c))}>`;
}

/** Inline code, fenced with more backticks than it holds in a row. */
function code(text: string) {
  const longest = Math.max(0, ...[...text.matchAll(/`+/g)].map((m) => m[0].length));
  const fence = "`".repeat(longest + 1);
  const pad = text.startsWith("`") || text.endsWith("`") ? " " : "";
  return `${fence}${pad}${text}${pad}${fence}`;
}

/** HTML as Markdown. Parsed inert: nothing in it loads or runs. */
export function htmlToMarkdown(html: string) {
  const doc = new DOMParser().parseFromString(html, "text/html");
  return finish(convert(doc.body, { list: [], pre: false }));
}

interface Context {
  /** The lists we're in, innermost last: null for bulleted, else the next number. */
  list: (number | null)[];
  pre: boolean;
}

function children(node: Node, cx: Context) {
  return [...node.childNodes].map((child) => convert(child, cx)).join("");
}

function convert(node: Node, cx: Context): string {
  if (node.nodeType === 3) {
    const text = (node as Text).data;
    return cx.pre ? text : escape(text.replace(/\s+/g, " "));
  }
  if (node.nodeType !== 1) return "";
  const el = node as Element;
  const tag = el.tagName;
  if (UNSEEN.has(tag)) return "";
  switch (tag) {
    case "BR":
      return cx.pre ? "\n" : "  \n";
    case "HR":
      return `\n\n${MARK}---\n\n`;
    case "H1":
    case "H2":
    case "H3":
    case "H4":
    case "H5":
    case "H6":
      return `\n\n${MARK}${"#".repeat(Number(tag[1]))} ${tidy(children(el, cx)).replace(/\n+/g, " ")}\n\n`;
    case "B":
    case "STRONG":
      return wrap("**", children(el, cx));
    case "I":
    case "EM":
      return wrap("*", children(el, cx));
    case "S":
    case "STRIKE":
    case "DEL":
      return wrap("~~", children(el, cx));
    case "CODE":
      return cx.pre ? (el.textContent ?? "") : code(el.textContent ?? "");
    case "PRE": {
      const text = (el.textContent ?? "").replace(/\n+$/, "");
      const longest = Math.max(2, ...[...text.matchAll(/`+/g)].map((m) => m[0].length));
      const fence = "`".repeat(longest + 1);
      return `\n\n${MARK}${fence}\n${text}\n${MARK}${fence}\n\n`;
    }
    case "A": {
      const text = children(el, cx).trim();
      const href = el.getAttribute("href") ?? "";
      if (!/^(https?|mailto|tel):/i.test(href)) return text;
      const shown = href.replace(/^mailto:/i, "");
      return text && text !== escape(shown) && text !== escape(href) ? `[${text}](${destination(href)})` : destination(href);
    }
    case "IMG": {
      const src = el.getAttribute("src") ?? "";
      return /^https?:/i.test(src) ? `![${escape(el.getAttribute("alt") ?? "")}](${destination(src)})` : "";
    }
    case "BLOCKQUOTE": {
      const inner = finish(children(el, cx), false);
      return `\n\n${inner
        .split("\n")
        .map((line) => `${MARK}>${line ? ` ${line}` : ""}`)
        .join("\n")}\n\n`;
    }
    case "UL":
    case "OL": {
      const inner = children(el, { ...cx, list: [...cx.list, tag === "OL" ? Number(el.getAttribute("start") ?? 1) : null] });
      return cx.list.length ? `\n${inner}` : `\n\n${inner}\n\n`;
    }
    case "LI": {
      const lists = cx.list;
      const kind = lists[lists.length - 1];
      const marker = kind === null || kind === undefined ? "-" : `${kind}.`;
      if (typeof kind === "number") lists[lists.length - 1] = kind + 1;
      // What follows the first line lines up under its text, as Markdown nests it.
      const inner = tidy(children(el, cx)).replace(/\n/g, `\n${" ".repeat(marker.length + 1)}`);
      return `${MARK}${marker} ${inner}\n`;
    }
    case "TABLE":
      return table(el, cx) ?? `\n${children(el, cx)}\n`;
    default: {
      const inner = children(el, cx);
      return BLOCKS.has(tag) ? `\n${inner}\n` : inner;
    }
  }
}

/**
 * A table of data as Markdown's: its rows, the first one over the `---` line Markdown needs.
 * Null for one that lays an email out (cells holding blocks, or a single column).
 */
function table(el: Element, cx: Context) {
  const rows = [...el.querySelectorAll(":scope > tr, :scope > thead > tr, :scope > tbody > tr, :scope > tfoot > tr")];
  const cells = rows.map((row) => [...row.querySelectorAll(":scope > td, :scope > th")]);
  const width = Math.max(0, ...cells.map((r) => r.length));
  if (!rows.length || width < 2 || cells.some((r) => r.some((cell) => cell.querySelector(LAYOUT)))) return null;
  const line = (values: string[]) =>
    `${MARK}| ${[...values, ...Array(width - values.length).fill("")].join(" | ")} |`;
  const text = cells.map((r) => r.map((cell) => tidy(children(cell, cx)).replace(/\n+/g, " ")));
  return `\n\n${[line(text[0]), line(Array(width).fill("---")), ...text.slice(1).map(line)].join("\n")}\n\n`;
}

/** `mark` around text, outside its spaces: `**bold** text`, not `** bold**`. */
function wrap(mark: string, text: string) {
  const inner = text.trim();
  if (!inner) return text;
  const lead = text.slice(0, text.length - text.trimStart().length);
  const trail = text.slice(text.trimEnd().length);
  return `${lead}${mark}${inner}${mark}${trail}`;
}

/** Lines without trailing spaces (but Markdown's line breaks), and one blank line at most. */
function tidy(markdown: string) {
  return markdown
    .split("\n")
    .map((line) => (!line.trim() ? "" : line.endsWith("  ") ? `${line.trimEnd()}  ` : line.trimEnd()))
    .join("\n")
    .replace(/\n{3,}/g, "\n\n")
    .trim();
}

/**
 * Tidied, with text that starts a line as Markdown syntax would (`# not a heading`, `1. not a
 * list`) escaped, code blocks left as they are; with `unmark`, the marks this left are gone.
 */
function finish(markdown: string, unmark = true) {
  let fence: string | null = null;
  const lines = tidy(markdown)
    .split("\n")
    .map((line) => {
      const body = line.trimStart();
      if (body.startsWith(MARK)) {
        const opened = /^\u0001(`{3,})/.exec(body)?.[1];
        if (opened) fence = fence === opened ? null : (fence ?? opened);
        return line;
      }
      if (fence !== null) return line;
      return line
        .replace(/^(\s*)(#{1,6}|[-+*])(?=\s|$)/, "$1\\$2")
        .replace(/^(\s*)(\d{1,9})([.)])(?=\s|$)/, "$1$2\\$3")
        .replace(/^(\s*)([=-]+\s*)$/, "$1\\$2");
    })
    .join("\n");
  return unmark ? lines.replaceAll(MARK, "") : lines;
}

/** One email as Markdown: who, when and what, then its body (`html`, or its text paragraphs). */
export function messageMarkdown(header: MarkdownHeader, html: string | null, text: string[]) {
  const lines = [`**From:** ${escape(header.from)}`];
  if (header.to.length) lines.push(`**To:** ${escape(header.to.join(", "))}`);
  if (header.cc.length) lines.push(`**Cc:** ${escape(header.cc.join(", "))}`);
  lines.push(`**Date:** ${escape(header.date)}`, `**Subject:** ${escape(header.subject)}`);
  const body =
    html !== null ? htmlToMarkdown(html) : finish(text.map((p) => escape(p).replace(/\n/g, "  \n")).join("\n\n"));
  return `${lines.join("  \n")}\n\n${body}`.trim();
}
