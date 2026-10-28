/**
 * The assistant's answers: the Markdown models write, read into a tree the panel renders as React
 * elements, so nothing in an answer runs as HTML. Paragraphs, lists (one level of nesting),
 * quotes, code, bold, italics, links and the assistant's citations, `[[1234]]` for email 1234.
 * Headings come out as bold lines, as the panel has no room for big type.
 */

export type Inline =
  | { kind: "text"; text: string }
  | { kind: "strong"; children: Inline[] }
  | { kind: "em"; children: Inline[] }
  | { kind: "code"; text: string }
  | { kind: "link"; href: string; children: Inline[] }
  | { kind: "cite"; id: number };

export type TextBlock =
  /** Lines that break where the text does. */
  | { kind: "p"; lines: Inline[][]; strong?: boolean }
  | { kind: "list"; ordered: boolean; start: number; items: TextBlock[][] }
  | { kind: "quote"; blocks: TextBlock[] }
  | { kind: "code"; text: string };

const BULLET = /^(\s*)[-*•+]\s+(.*)$/;
const NUMBERED = /^(\s*)(\d{1,9})[.)]\s+(.*)$/;
const CITE = /\[\[\s*(\d+(?:\s*,\s*\d+)*)\s*\]\]/g;

export function parseAnswer(text: string): TextBlock[] {
  return blocks(text.replace(/\r\n?/g, "\n").split("\n"));
}

function blocks(lines: string[]): TextBlock[] {
  const out: TextBlock[] = [];
  let i = 0;
  while (i < lines.length) {
    const line = lines[i];
    if (!line.trim()) {
      i++;
      continue;
    }
    const fence = /^\s*(```|~~~)/.exec(line);
    if (fence) {
      const code: string[] = [];
      i++;
      while (i < lines.length && !lines[i].trim().startsWith(fence[1])) code.push(lines[i++]);
      i++;
      out.push({ kind: "code", text: code.join("\n") });
      continue;
    }
    if (/^\s*>/.test(line)) {
      const quoted: string[] = [];
      while (i < lines.length && /^\s*>/.test(lines[i])) quoted.push(lines[i++].replace(/^\s*>\s?/, ""));
      out.push({ kind: "quote", blocks: blocks(quoted) });
      continue;
    }
    if (BULLET.test(line) || NUMBERED.test(line)) {
      const [list, next] = listAt(lines, i);
      out.push(list);
      i = next;
      continue;
    }
    const heading = /^\s*#{1,6}\s+(.*)$/.exec(line);
    if (heading) {
      out.push({ kind: "p", lines: [inline(heading[1])], strong: true });
      i++;
      continue;
    }
    const para: Inline[][] = [];
    while (
      i < lines.length &&
      lines[i].trim() &&
      !BULLET.test(lines[i]) &&
      !NUMBERED.test(lines[i]) &&
      !/^\s*(>|```|~~~|#{1,6}\s)/.test(lines[i])
    ) {
      para.push(inline(lines[i++].trim()));
    }
    out.push({ kind: "p", lines: para });
  }
  return out;
}

/** The list starting at line `i`, and the line after it. */
function listAt(lines: string[], i: number): [TextBlock, number] {
  const first = NUMBERED.exec(lines[i]);
  const ordered = first !== null;
  const indent = (first?.[1] ?? BULLET.exec(lines[i])![1]).length;
  const items: string[][] = [];
  while (i < lines.length) {
    const line = lines[i];
    const item = ordered ? NUMBERED.exec(line) : BULLET.exec(line);
    if (item && item[1].length <= indent) {
      items.push([ordered ? item[3] : item[2]]);
      i++;
      continue;
    }
    // Indented lines belong to the item above: more of its text, or a list inside it.
    const indented = line.length - line.trimStart().length > indent;
    if (items.length && line.trim() && indented) {
      items[items.length - 1].push(line.slice(Math.min(indent + 2, line.length - line.trimStart().length)));
      i++;
      continue;
    }
    // A blank line inside a list keeps it going if the next line carries on.
    if (!line.trim() && i + 1 < lines.length && (ordered ? NUMBERED : BULLET).test(lines[i + 1])) {
      i++;
      continue;
    }
    break;
  }
  return [
    { kind: "list", ordered, start: ordered ? Number(first[2]) : 1, items: items.map((item) => blocks(item)) },
    i,
  ];
}

/** Bold, italics, code, links and citations in one line. Unclosed marks stay as text. */
export function inline(text: string): Inline[] {
  const out: Inline[] = [];
  let plain = "";
  const flush = () => {
    if (plain) out.push({ kind: "text", text: plain });
    plain = "";
  };
  let i = 0;
  while (i < text.length) {
    const rest = text.slice(i);
    CITE.lastIndex = 0;
    const cite = rest.startsWith("[[") ? CITE.exec(rest) : null;
    if (cite && cite.index === 0) {
      flush();
      for (const id of cite[1].split(",")) out.push({ kind: "cite", id: Number(id.trim()) });
      i += cite[0].length;
      continue;
    }
    if (rest[0] === "`") {
      const end = rest.indexOf("`", 1);
      if (end > 1) {
        flush();
        out.push({ kind: "code", text: rest.slice(1, end) });
        i += end + 1;
        continue;
      }
    }
    const strong = /^(\*\*|__)(?=\S)([\s\S]*?\S)\1/.exec(rest);
    if (strong) {
      flush();
      out.push({ kind: "strong", children: inline(strong[2]) });
      i += strong[0].length;
      continue;
    }
    const em = /^([*_])(?=[^\s*_])([\s\S]*?[^\s*_])\1(?![*_\w])/.exec(rest);
    if (em && (rest[0] === "*" || !/\w/.test(text[i - 1] ?? ""))) {
      flush();
      out.push({ kind: "em", children: inline(em[2]) });
      i += em[0].length;
      continue;
    }
    const link = /^\[([^\]]+)\]\(([^)\s]+)\)/.exec(rest);
    if (link) {
      flush();
      const href = safeHref(link[2]);
      if (href) out.push({ kind: "link", href, children: inline(link[1]) });
      else out.push(...inline(link[1]));
      i += link[0].length;
      continue;
    }
    plain += text[i];
    i++;
  }
  flush();
  return out;
}

/** Only web and mail links open; anything else shows as its text. */
function safeHref(href: string): string | null {
  return /^(https?:\/\/|mailto:)/i.test(href) ? href : null;
}

/** The emails an answer cites, in the order it first cites them: the Sources list and its numbers. */
export function citedIds(text: string): number[] {
  const ids: number[] = [];
  for (const m of text.matchAll(CITE)) {
    for (const id of m[1].split(",").map((x) => Number(x.trim()))) if (!ids.includes(id)) ids.push(id);
  }
  return ids;
}

/**
 * Plain text for the clipboard: Markdown marks gone, citations numbered as the chips are, by
 * `order` (the cited emails the panel knows), and ones it doesn't know left out.
 */
export function plainAnswer(text: string, order: number[]): string {
  return text
    .replace(/(\s*)\[\[\s*(\d+(?:\s*,\s*\d+)*)\s*\]\]/g, (_, space: string, ids: string) => {
      const numbers = ids
        .split(",")
        .map((id) => order.indexOf(Number(id.trim())) + 1)
        .filter((n) => n > 0)
        .map((n) => `[${n}]`)
        .join("");
      // A citation left out takes the space before it along.
      return numbers ? space + numbers : "";
    })
    .replace(/(\*\*|__)(.+?)\1/g, "$2")
    .replace(/`([^`]+)`/g, "$1")
    .replace(/\[([^\]]+)\]\(([^)\s]+)\)/g, "$1 ($2)")
    .trim();
}

/**
 * How much of `target` to show once `shown` characters are out, at an even pace: about
 * `perStep` more, without stopping inside a citation. While more is coming (`complete` false),
 * a citation still being written waits until it's whole.
 */
export function revealTo(target: string, shown: number, perStep: number, complete: boolean): number {
  const next = Math.min(target.length, shown + perStep);
  const open = target.lastIndexOf("[[", next - 1);
  if (open < 0) return next;
  const close = target.indexOf("]]", open + 2);
  if (close >= 0) return close + 2 > next ? Math.min(target.length, close + 2) : next;
  return complete ? next : Math.max(shown, open);
}
