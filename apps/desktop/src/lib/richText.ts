/**
 * The composer's rich text as plain text: what AI rewrites and @mentions read,
 * and the edits AI makes back in the HTML. A text offset counts characters the
 * way `bodyText` reads the HTML: each text node as it is, a line break for a
 * `<br>`, and one more where a block starts or ends on a line that has text.
 */

/** Elements that sit on lines of their own. */
const BLOCKS = new Set([
  "ADDRESS", "ARTICLE", "ASIDE", "BLOCKQUOTE", "CENTER", "DD", "DIV", "DL", "DT", "FIGCAPTION", "FIGURE",
  "FOOTER", "H1", "H2", "H3", "H4", "H5", "H6", "HEADER", "HR", "LI", "MAIN", "NAV", "OL", "P", "PRE",
  "SECTION", "TABLE", "TR", "UL",
]);
/** Elements whose text doesn't show. */
const UNSEEN = new Set(["HEAD", "SCRIPT", "STYLE", "TEMPLATE", "TITLE"]);
/** Formatting that's nothing without text in it. */
const INLINE = "a, b, big, code, em, font, i, s, small, span, strike, strong, sub, sup, u";

const TEXT_NODE = 3;
const ELEMENT_NODE = 1;
const FOLLOWING = 4;

/** A text node or a `<br>`, and where its text starts. */
interface Piece {
  node: Node;
  start: number;
  length: number;
}

function read(root: Node) {
  let text = "";
  const pieces: Piece[] = [];
  /** The text ends in a block's break rather than something in it. */
  let broken = false;
  const breakLine = () => {
    if (!text || text.endsWith("\n")) return;
    text += "\n";
    broken = true;
  };
  const visit = (node: Node) => {
    if (node.nodeType === TEXT_NODE) {
      const data = (node as Text).data;
      pieces.push({ node, start: text.length, length: data.length });
      text += data;
      broken &&= !data;
      return;
    }
    if (node.nodeType !== ELEMENT_NODE) return;
    const tag = (node as Element).tagName;
    if (tag === "BR") {
      pieces.push({ node, start: text.length, length: 1 });
      text += "\n";
      broken = false;
      return;
    }
    if (UNSEEN.has(tag)) return;
    const block = BLOCKS.has(tag);
    if (block) breakLine();
    for (const child of node.childNodes) visit(child);
    if (block) breakLine();
  };
  for (const child of root.childNodes) visit(child);
  // The last block's end isn't a line of its own.
  if (broken) text = text.slice(0, -1);
  return { text, pieces };
}

/** The block `node` is in, short of `root`. */
function blockOf(node: Node, root: Node) {
  for (let at: Node | null = node; at && at !== root; at = at.parentNode) {
    if (at.nodeType === ELEMENT_NODE && BLOCKS.has((at as Element).tagName)) return at as Element;
  }
  return null;
}

/** What's in `root`, as plain text. */
export function bodyText(root: Node) {
  return read(root).text;
}

/** Where a DOM point (as a selection has them) is in `root`'s text. */
export function textOffset(root: Node, container: Node, offset: number) {
  const { text, pieces } = read(root);
  if (container.nodeType === TEXT_NODE) {
    const piece = pieces.find((p) => p.node === container);
    if (piece) return piece.start + Math.min(offset, piece.length);
  }
  // Between children: where the first piece at or after the point starts.
  const next = container.childNodes[offset] ?? null;
  const at = pieces.find((p) =>
    next
      ? p.node === next || next.compareDocumentPosition(p.node) & FOLLOWING
      : !container.contains(p.node) && container.compareDocumentPosition(p.node) & FOLLOWING,
  );
  return at?.start ?? text.length;
}

/** The text of some HTML, as the editor reads it (`bodyText`). */
export function htmlText(html: string) {
  const doc = new DOMParser().parseFromString("<!doctype html><body></body>", "text/html");
  doc.body.innerHTML = html;
  return bodyText(doc.body);
}

/** A `cid:` URL's Content-ID as `InlineImages` keys it: decoded, without angle brackets, lowercase. */
export function cidKey(url: string) {
  let id = url.trim().slice(4);
  try {
    id = decodeURIComponent(id);
  } catch {
    // A stray `%`: keep it as written.
  }
  return id.trim().replace(/^<+/, "").replace(/>+$/, "").toLowerCase();
}

/**
 * A sent email's HTML as the body of a new one (Send Again): the body spark-rs wrote, without
 * its signature and quote, or all of another app's; its `cid:` pictures from `images` (by
 * Content-ID) as data, and ones that aren't there left out.
 */
export function resendHtml(html: string, images: Record<string, string>) {
  // Parsed inert, and only ever shown in the editor's sandboxed frame.
  const doc = new DOMParser().parseFromString(html, "text/html");
  const section = doc.querySelector('[name="messageBodySection"]');
  const root = section ?? doc.body;
  for (const el of root.querySelectorAll("script, style, link, meta, title, base, iframe, object, embed, form")) el.remove();
  for (const el of root.querySelectorAll("*")) {
    for (const attr of [...el.attributes]) if (/^on/i.test(attr.name)) el.removeAttribute(attr.name);
  }
  for (const img of root.querySelectorAll("img")) {
    const src = img.getAttribute("src") ?? "";
    if (!/^\s*cid:/i.test(src)) continue;
    const data = images[cidKey(src)];
    if (data) img.setAttribute("src", data);
    else img.remove();
  }
  const out = root.innerHTML.trim();
  // What the HTML version adds after a body isn't typed.
  return section ? out.replace(/(<br>){2}$/i, "") : out;
}

/** Puts one end of `range` at text offset `at`: a start before that character, an end after the one before it. */
function place(range: Range, pieces: Piece[], at: number, edge: "start" | "end") {
  const texts = pieces.filter((p) => p.node.nodeType === TEXT_NODE);
  if (edge === "start") {
    const inside = texts.find((p) => at >= p.start && at < p.start + p.length);
    if (inside) return range.setStart(inside.node, at - inside.start);
    const after = pieces.find((p) => p.start >= at);
    if (after) range.setStartBefore(after.node);
    return;
  }
  const inside = [...texts].reverse().find((p) => at > p.start && at <= p.start + p.length);
  if (inside) return range.setEnd(inside.node, at - inside.start);
  const before = [...pieces].reverse().find((p) => p.start + p.length <= at);
  if (before) range.setEndAfter(before.node);
}

/**
 * `html` with the text between offsets `start` and `end` replaced by `text`, its lines broken
 * with `<br>`. The new text takes the formatting the old one began in, and a block the old text
 * ended in joins the one it began in, as when the text is selected and typed over.
 */
export function replaceText(html: string, [start, end]: [number, number], text: string) {
  // A parsed document is inert: nothing in it loads or runs.
  const doc = new DOMParser().parseFromString("<!doctype html><body></body>", "text/html");
  const root = doc.body;
  root.innerHTML = html;
  const { pieces } = read(root);
  const range = doc.createRange();
  range.selectNodeContents(root);
  place(range, pieces, start, "start");
  place(range, pieces, end, "end");
  const first = blockOf(range.startContainer, root);
  const last = blockOf(range.endContainer, root);
  const lines = doc.createDocumentFragment();
  text.split("\n").forEach((line, i) => {
    if (i) lines.append(doc.createElement("br"));
    if (line) lines.append(line);
  });
  const written = lines.lastChild;
  const at = range.cloneRange();
  at.collapse(true);
  at.insertNode(lines);
  if (written) range.setStartAfter(written);
  range.deleteContents();
  if (first && last && first !== last && last.isConnected && !first.contains(last) && !last.contains(first)) {
    while (last.firstChild) first.append(last.firstChild);
    // The emptied block goes, with a list or quote it was all that was left of.
    let gone: Element = last;
    while (gone.parentElement && gone.parentElement !== root && gone.parentElement.childNodes.length === 1) {
      gone = gone.parentElement;
    }
    gone.remove();
  }
  for (const el of root.querySelectorAll(INLINE)) {
    if (el.isConnected && !el.textContent && !el.querySelector("img, br")) el.remove();
  }
  return root.innerHTML;
}

/** A template placeholder as the editor holds it: a chip with its label, typed over to fill it. */
export function placeholderHtml(kind: string, label: string) {
  const name = escapeHtml(label);
  return `<span data-placeholder="${escapeHtml(kind)}" data-label="${name}" contenteditable="false">${name}</span>`;
}

/** A template's HTML with the placeholders that have a value written in; the others stay to be filled. */
export function fillPlaceholders(html: string, values: Partial<Record<string, string>>) {
  const doc = new DOMParser().parseFromString("<!doctype html><body></body>", "text/html");
  doc.body.innerHTML = html;
  for (const chip of doc.body.querySelectorAll<HTMLElement>("span[data-placeholder]")) {
    const value = values[chip.dataset.placeholder ?? ""]?.trim();
    if (value) chip.replaceWith(value);
    else chip.setAttribute("contenteditable", "false");
  }
  return doc.body.innerHTML;
}

/** Whether a body still has placeholders to fill. */
export function hasPlaceholders(html: string | undefined) {
  return /\bdata-placeholder=/.test(html ?? "");
}

/** A body with its unfilled placeholders taken out, and its text: what "Send Anyway" sends. */
export function dropPlaceholders(html: string) {
  const doc = new DOMParser().parseFromString("<!doctype html><body></body>", "text/html");
  doc.body.innerHTML = html;
  for (const chip of doc.body.querySelectorAll("span[data-placeholder]")) chip.remove();
  return { html: doc.body.innerHTML, text: bodyText(doc.body) };
}

/**
 * Plain text as the editor's HTML: a line to a `<div>`, as the editor writes them, with its
 * runs of spaces kept (HTML would show them as one).
 */
export function textToHtml(text: string) {
  if (!text) return "";
  return text
    .split(/\r?\n/)
    .map((line) => `<div>${escapeHtml(line).replace(/^ | (?= )/g, "\u00a0") || "<br>"}</div>`)
    .join("");
}

export function escapeHtml(text: string) {
  return text.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");
}

/** Spark's image sizes, and the longest side each allows ("actual" is as it came). */
export type ImageSize = "small" | "medium" | "large" | "actual";
export const IMAGE_SIZES: { key: ImageSize; label: string; pixels: number }[] = [
  { key: "small", label: "Small", pixels: 320 },
  { key: "medium", label: "Medium", pixels: 640 },
  { key: "large", label: "Large", pixels: 1280 },
  { key: "actual", label: "Actual", pixels: Infinity },
];

/** Spark's text and highlight colors (CKEditor's palette), in hex, which every mail client reads. */
export const COLORS: { label: string; color: string }[] = [
  { label: "Black", color: "#000000" },
  { label: "Dim grey", color: "#4d4d4d" },
  { label: "Grey", color: "#999999" },
  { label: "Light grey", color: "#e6e6e6" },
  { label: "White", color: "#ffffff" },
  { label: "Red", color: "#e64d4d" },
  { label: "Orange", color: "#e6994d" },
  { label: "Yellow", color: "#e6e64d" },
  { label: "Light green", color: "#99e64d" },
  { label: "Green", color: "#4de64d" },
  { label: "Aquamarine", color: "#4de699" },
  { label: "Turquoise", color: "#4de6e6" },
  { label: "Light blue", color: "#4d99e6" },
  { label: "Blue", color: "#4d4de6" },
  { label: "Purple", color: "#994de6" },
];

/** Spark's font sizes, in px; 14 is the body's own. */
export const FONT_SIZES = [9, 10, 11, 12, 14, 16, 18, 20, 22, 24, 26, 28, 32, 36, 48, 64, 72, 96, 144, 288];
export const DEFAULT_FONT_SIZE = 14;

/** Fonts that look the same on most computers that read the mail. */
export const FONT_FAMILIES = [
  "Arial",
  "Calibri",
  "Courier New",
  "Georgia",
  "Helvetica",
  "Segoe UI",
  "Tahoma",
  "Times New Roman",
  "Trebuchet MS",
  "Verdana",
];

const FONTS_KEY = "spark-rs.fonts";
/** How many recently used fonts the font menu offers first. */
const RECENT_FONTS = 3;

/** The fonts and size used last, as Spark remembers them. */
export interface RecentFonts {
  families: string[];
  size?: number;
}

export function loadRecentFonts(): RecentFonts {
  try {
    const saved = JSON.parse(localStorage.getItem(FONTS_KEY) ?? "{}") as Partial<RecentFonts>;
    return {
      families: Array.isArray(saved.families) ? saved.families.filter((f) => typeof f === "string").slice(0, RECENT_FONTS) : [],
      size: typeof saved.size === "number" ? saved.size : undefined,
    };
  } catch {
    return { families: [] };
  }
}

/** Notes a font family or size as used. */
export function noteFont(recent: RecentFonts, used: { family?: string; size?: number }): RecentFonts {
  const next = {
    families: used.family
      ? [used.family, ...recent.families.filter((f) => f !== used.family)].slice(0, RECENT_FONTS)
      : recent.families,
    size: used.size ?? recent.size,
  };
  try {
    localStorage.setItem(FONTS_KEY, JSON.stringify(next));
  } catch {
    // Not remembered; nothing else depends on it.
  }
  return next;
}

/**
 * A link as typed: one without a scheme is https, as Spark defaults, and an
 * address is a mailto link. Only web, mail and phone links are taken ("" for others).
 */
export function linkUrl(typed: string) {
  const url = typed.trim();
  if (!url) return "";
  if (/^(https?|mailto|tel):/i.test(url)) return url;
  // Another scheme; "host:port" isn't one.
  if (/^[a-z][a-z\d+.-]*:(?!\d)/i.test(url)) return "";
  if (/^[^\s@/]+@[^\s@/]+\.[^\s@/]+$/.test(url)) return `mailto:${url}`;
  return `https://${url}`;
}
