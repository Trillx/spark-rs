import {
  ALargeSmall,
  Baseline,
  Bold,
  Check,
  Heading,
  Highlighter,
  IndentDecrease,
  IndentIncrease,
  Italic,
  Link,
  List,
  ListOrdered,
  RemoveFormatting,
  Strikethrough,
  TextQuote,
  Type,
  Underline,
  type LucideIcon,
} from "lucide-react";
import {
  forwardRef,
  useEffect,
  useImperativeHandle,
  useLayoutEffect,
  useRef,
  useState,
  type ReactNode,
  type RefObject,
} from "react";
import { mentionAt } from "../lib/addresses";
import {
  COLORS,
  DEFAULT_FONT_SIZE,
  FONT_FAMILIES,
  FONT_SIZES,
  IMAGE_SIZES,
  bodyText,
  escapeHtml,
  linkUrl,
  loadRecentFonts,
  noteFont,
  textOffset,
  type ImageSize,
} from "../lib/richText";
import { IS_MAC } from "../lib/shortcuts";
import { undoDarkMode } from "../lib/signatures";
import { useDarkTheme } from "../lib/theme";
import { tipAttrs, type Tip } from "../lib/tooltip";

// The body is HTML anyone may have pasted from anywhere, so like a
// signature it's edited in a frame sandboxed without `allow-scripts`, under
// its own CSP. `allow-same-origin` lets this side size it, read what's typed
// and run the editing commands; with scripts off, the content can't use it.
// Unlike a signature it takes the app's colors: what's typed has none of its
// own, so the recipient reads it in theirs.

/** Shortcuts the editor keeps; other ones with Ctrl/Cmd, and Esc, go to the app. */
const EDITING_KEYS = new Set(["a", "b", "c", "i", "u", "v", "x", "y", "z"]);
/** How a quote looks wherever it's read: the same as a reply's quoted original. */
const QUOTE_STYLE = "margin:0 0 0 .8ex;border-left:1px solid #ccc;padding-left:1ex";
const MOD = IS_MAC ? "⌘" : "Ctrl+";
const SHIFT_MOD = IS_MAC ? "⇧⌘" : "Ctrl+Shift+";

/** Pictures put in the body, as they came, for going back to Actual Size. */
const originals = new Map<string, string>();

/** The @mention being typed, and where its "@" is on screen. */
export interface MentionQuery {
  query: string;
  rect: DOMRect;
}

export interface RichEditorHandle {
  focus: (at?: "start" | "end") => void;
  /** The selection as offsets in the body's text (see `bodyText`); null with nothing selected. */
  selection: () => [number, number] | null;
  /** Replaces the @mention being typed with `name`, linked to `address`, in one undo step. */
  completeMention: (name: string, address: string) => void;
  /** How many links to `address` the body has. */
  mentions: (address: string) => number;
  /** Puts pictures (data URLs) in at the caret, or where `at` (window px) is. */
  insertImages: (urls: string[], at?: { x: number; y: number }) => Promise<void>;
  /** Sizes the body's pictures again. */
  resizeImages: (size: ImageSize) => Promise<void>;
  /** Whether a window point (px) is over the body. */
  covers: (x: number, y: number) => boolean;
}

type Block = "div" | "h1" | "h2";
type Panel = "link" | "color" | "highlight" | "heading" | "font" | "size";

interface Bar {
  /** The selection's middle, and its top and bottom, in window px. */
  x: number;
  top: number;
  bottom: number;
  active: ReadonlySet<string>;
  block: string;
  /** The link the selection is in, if any. */
  href: string | null;
}

/**
 * The composer's body: rich text in a sandboxed frame that grows with it,
 * with Spark's formatting bar over a selection and its shortcuts. A new
 * `html` that isn't what was typed replaces what's there.
 */
const RichEditor = forwardRef<
  RichEditorHandle,
  {
    html: string;
    /** The HTML as it changes ("" when emptied), its text, and the input's type (historyUndo…). */
    onChange: (html: string, text: string, inputType: string) => void;
    placeholder?: string;
    readOnly?: boolean;
    /** Smallest height, in px. */
    minHeight?: number;
    /** Where the caret goes once it's loaded, if anywhere. */
    autoFocus?: "start" | "end";
    /** The area the editor scrolls in, kept on the caret. */
    scroller?: RefObject<HTMLElement | null>;
    /** How big new pictures go in. */
    imageSize: ImageSize;
    onMention?: (mention: MentionQuery | null) => void;
    /** Keys, first: true takes the key (the mention list's arrows and Enter). */
    onKey?: (e: KeyboardEvent) => boolean;
    onError?: (text: string) => void;
  }
>(function RichEditor(
  { html, onChange, placeholder = "", readOnly = false, minHeight = 0, autoFocus, scroller, imageSize, onMention, onKey, onError },
  handle,
) {
  const frameRef = useRef<HTMLIFrameElement>(null);
  const [height, setHeight] = useState(0);
  const dark = useDarkTheme();
  /** What the editor holds: the HTML it was given, then what's typed. */
  const held = useRef<string | null>(null);
  const latest = useRef({ html, onChange, onMention, onKey, onError, readOnly, imageSize, scroller, placeholder });
  latest.current = { html, onChange, onMention, onKey, onError, readOnly, imageSize, scroller, placeholder };
  const [wired, setWired] = useState<HTMLElement | null>(null);
  const mention = useRef<{ node: Text; start: number; caret: number } | null>(null);
  const [bar, setBar] = useState<Bar | null>(null);
  const [panel, setPanel] = useState<Panel | null>(null);
  const panelRef = useRef<Panel | null>(null);
  panelRef.current = panel;
  /** The selection while a panel has focus, to format once it's chosen. */
  const saved = useRef<Range | null>(null);
  const plainPaste = useRef(false);

  const root = () => frameRef.current?.contentDocument?.getElementById("body") ?? null;
  const doc = () => frameRef.current?.contentDocument ?? null;

  function selectionIn(r: HTMLElement) {
    const sel = r.ownerDocument.getSelection();
    if (!sel?.rangeCount) return null;
    const range = sel.getRangeAt(0);
    return r.contains(range.commonAncestorContainer) ? { sel, range } : null;
  }

  function empty(r: HTMLElement) {
    return !r.textContent?.trim() && !r.querySelector("img, li, hr, table");
  }

  function emit(inputType = "") {
    const r = root();
    if (!r) return;
    const blank = empty(r);
    r.toggleAttribute("data-empty", blank);
    // An emptied editor keeps a stray <br>; it's empty.
    const next = blank ? "" : r.innerHTML;
    held.current = next;
    latest.current.onChange(next, blank ? "" : bodyText(r), inputType);
  }

  /** The @mention just before the caret, if one's being typed. */
  function trackMention() {
    const r = root();
    const found = r && !latest.current.readOnly ? selectionIn(r) : null;
    const node = found?.sel.isCollapsed ? found.sel.focusNode : null;
    const m =
      r && node instanceof r.ownerDocument.defaultView!.Text && !node.parentElement?.closest("a")
        ? mentionAt(node.data, found!.sel.focusOffset)
        : null;
    if (!r || !m || !node) {
      mention.current = null;
      latest.current.onMention?.(null);
      return;
    }
    mention.current = { node: node as Text, start: m.start, caret: found!.sel.focusOffset };
    const at = r.ownerDocument.createRange();
    at.setStart(node, m.start);
    at.setEnd(node, m.start + 1);
    latest.current.onMention?.({ query: m.query, rect: toWindow(at.getBoundingClientRect()) });
  }

  /** The element a range starts in. */
  function startElement(range: Range) {
    const start = range.startContainer;
    return start.nodeType === 1 ? (start as Element) : start.parentElement;
  }

  /** Where a range is on screen; a caret on an empty line has no box, so its line's. */
  function rectOf(range: Range) {
    const rect = range.getBoundingClientRect();
    return toWindow(rect.height ? rect : (startElement(range)?.getBoundingClientRect() ?? rect));
  }

  function toWindow(rect: DOMRect) {
    const frame = frameRef.current!.getBoundingClientRect();
    return new DOMRect(frame.left + rect.left, frame.top + rect.top, rect.width, rect.height);
  }

  /** The formatting bar over a selection, with what's on in it. */
  function trackBar() {
    if (panelRef.current) return;
    const r = root();
    const found = r && !latest.current.readOnly && r.ownerDocument.hasFocus() ? selectionIn(r) : null;
    if (!found || found.sel.isCollapsed) return setBar(null);
    showBar(found.range);
  }

  function showBar(range: Range) {
    const d = range.startContainer.ownerDocument!;
    const rect = rectOf(range);
    const active = new Set(
      ["bold", "italic", "underline", "strikeThrough", "insertUnorderedList", "insertOrderedList"].filter((c) =>
        d.queryCommandState(c),
      ),
    );
    const element = startElement(range);
    if (element?.closest("blockquote")) active.add("quote");
    setBar({
      x: rect.left + rect.width / 2,
      top: rect.top,
      bottom: rect.bottom,
      active,
      block: d.queryCommandValue("formatBlock").toLowerCase(),
      href: element?.closest("a")?.getAttribute("href") ?? null,
    });
  }

  /** Keeps the caret in view as the frame grows inside the composer's scrolling area. */
  function followCaret() {
    const r = root();
    const area = latest.current.scroller?.current;
    const found = r && area ? selectionIn(r) : null;
    if (!found || !found.sel.isCollapsed) return;
    const caret = rectOf(found.range);
    if (!caret.height) return;
    const box = area!.getBoundingClientRect();
    if (caret.bottom > box.bottom - 8) area!.scrollTop += caret.bottom - box.bottom + 8;
    else if (caret.top < box.top + 8) area!.scrollTop -= box.top + 8 - caret.top;
  }

  /** Runs an editing command on the selection (or the one saved while a panel was open). */
  function run(command: string, value?: string) {
    const r = root();
    const d = doc();
    if (!r || !d) return;
    d.defaultView?.focus();
    if (saved.current) {
      const sel = d.getSelection();
      sel?.removeAllRanges();
      sel?.addRange(saved.current);
      saved.current = null;
    }
    d.execCommand("styleWithCSS", false, "true");
    d.execCommand(command, false, value);
    emit();
    trackBar();
  }

  /** Takes a style off what's selected: a color, a font. */
  function clearStyle(property: string, attribute: string) {
    const r = root();
    const range = saved.current ?? (r && selectionIn(r)?.range);
    if (!r || !range) return;
    for (const el of r.querySelectorAll<HTMLElement>("[style], font")) {
      if (!range.intersectsNode(el)) continue;
      el.style.removeProperty(property);
      if (!el.getAttribute("style")?.trim()) el.removeAttribute("style");
      if (el.tagName === "FONT" && attribute) el.removeAttribute(attribute);
    }
    emit();
    restore();
  }

  /** The frame back in focus, with the selection a panel saved. */
  function restore() {
    const d = doc();
    const range = saved.current;
    saved.current = null;
    if (!d || !range) return;
    d.defaultView?.focus();
    d.getSelection()?.removeAllRanges();
    d.getSelection()?.addRange(range);
  }

  function fontSize(px: number) {
    const r = root();
    if (!r) return;
    // The browser sizes in seven steps; the largest one stands in for the size.
    run("fontSize", "7");
    for (const el of r.querySelectorAll<HTMLElement>('[style*="xxx-large"], font[size="7"]')) {
      el.removeAttribute("size");
      el.style.fontSize = `${px}px`;
    }
    emit();
    setRecent((recent) => noteFont(recent, { size: px }));
  }

  function toggleQuote() {
    const r = root();
    const range = saved.current ?? (r && selectionIn(r)?.range);
    if (!r || !range) return;
    const quote = startElement(range)?.closest("blockquote");
    if (quote && r.contains(quote)) {
      run("outdent");
      return;
    }
    run("formatBlock", "<blockquote>");
    for (const q of r.querySelectorAll("blockquote:not([style])")) q.setAttribute("style", QUOTE_STYLE);
    emit();
  }

  function setBlock(block: Block) {
    run("formatBlock", `<${block}>`);
  }

  function toggleBlock(block: Block) {
    setBlock(doc()?.queryCommandValue("formatBlock").toLowerCase() === block ? "div" : block);
  }

  function openPanel(next: Panel) {
    const r = root();
    const found = r && selectionIn(r);
    if (!found) return;
    saved.current = found.range.cloneRange();
    // A link can go in at the caret: the bar comes up there for it.
    showBar(found.range);
    panelRef.current = next;
    setPanel(next);
  }

  /** Closes the open panel; `back` returns to the text as it was selected. */
  function closePanel(back = false) {
    panelRef.current = null;
    setPanel(null);
    if (back) restore();
    saved.current = null;
    trackBar();
  }

  /** Links the selection, or with none, puts the link in as its own text; a link it's in changes. */
  function link(typed: string) {
    const url = linkUrl(typed);
    const r = root();
    const range = saved.current;
    if (!url || !r || !range) {
      if (typed.trim() && !url) latest.current.onError?.("That isn't a web, email or phone link.");
      closePanel(true);
      return;
    }
    panelRef.current = null;
    setPanel(null);
    const existing = startElement(range)?.closest("a");
    if (existing && r.contains(existing)) {
      existing.setAttribute("href", url);
      emit();
      restore();
    } else if (range.collapsed) {
      run("insertHTML", `<a href="${escapeHtml(url)}">${escapeHtml(url)}</a>`);
    } else {
      run("createLink", url);
    }
  }

  function unlink() {
    const r = root();
    const existing = saved.current && startElement(saved.current)?.closest("a");
    if (r && existing && r.contains(existing)) {
      const whole = r.ownerDocument.createRange();
      whole.selectNodeContents(existing);
      saved.current = whole;
    }
    panelRef.current = null;
    setPanel(null);
    run("unlink");
  }

  const [recent, setRecent] = useState(loadRecentFonts);

  /** A picture as it goes in: as it came, or no bigger than the size chosen. */
  async function sized(original: string, size: ImageSize) {
    const pixels = IMAGE_SIZES.find((s) => s.key === size)!.pixels;
    const img = await loadImage(original);
    const longest = Math.max(img.naturalWidth, img.naturalHeight);
    if (longest <= pixels) return { url: original, width: null };
    const scale = pixels / longest;
    const canvas = document.createElement("canvas");
    canvas.width = Math.max(1, Math.round(img.naturalWidth * scale));
    canvas.height = Math.max(1, Math.round(img.naturalHeight * scale));
    canvas.getContext("2d")!.drawImage(img, 0, 0, canvas.width, canvas.height);
    // Screenshots and logos stay sharp as PNG; photos are smaller as JPEG.
    const png = /^data:image\/(png|gif)/i.test(original);
    return { url: canvas.toDataURL(png ? "image/png" : "image/jpeg", 0.9), width: canvas.width };
  }

  async function insertImages(urls: string[], at?: { x: number; y: number }) {
    const r = root();
    const d = doc();
    if (!r || !d) return;
    const pictures = await Promise.all(
      urls.map(async (url) => {
        const key = crypto.randomUUID();
        originals.set(key, url);
        const fitted = await sized(url, latest.current.imageSize);
        const width = fitted.width ? ` width="${fitted.width}"` : "";
        return `<img src="${escapeHtml(fitted.url)}" data-original="${key}"${width}>`;
      }),
    );
    d.defaultView?.focus();
    const sel = d.getSelection();
    const frame = frameRef.current!.getBoundingClientRect();
    const dropped = at && d.caretRangeFromPoint(at.x - frame.left, at.y - frame.top);
    if (dropped && r.contains(dropped.startContainer)) {
      sel?.removeAllRanges();
      sel?.addRange(dropped);
    } else if (!selectionIn(r)) {
      const end = d.createRange();
      end.selectNodeContents(r);
      end.collapse(false);
      sel?.removeAllRanges();
      sel?.addRange(end);
    }
    run("insertHTML", pictures.join(""));
  }

  async function resizeImages(size: ImageSize) {
    const r = root();
    if (!r) return;
    for (const img of r.querySelectorAll("img")) {
      const original = originals.get(img.dataset.original ?? "") ?? img.getAttribute("src") ?? "";
      if (!original.startsWith("data:image/")) continue;
      try {
        const fitted = await sized(original, size);
        img.setAttribute("src", fitted.url);
        if (fitted.width) img.setAttribute("width", String(fitted.width));
        else img.removeAttribute("width");
        img.removeAttribute("height");
      } catch {
        // One that doesn't load stays as it is.
      }
    }
    emit();
  }

  useImperativeHandle(handle, () => ({
    focus(at = "end") {
      const r = root();
      const d = doc();
      if (!r || !d) return;
      d.defaultView?.focus();
      r.focus();
      const range = d.createRange();
      range.selectNodeContents(r);
      range.collapse(at === "start");
      d.getSelection()?.removeAllRanges();
      d.getSelection()?.addRange(range);
    },
    selection() {
      const r = root();
      const found = r && selectionIn(r);
      if (!found || found.sel.isCollapsed || !found.range.toString().trim()) return null;
      const start = textOffset(r!, found.range.startContainer, found.range.startOffset);
      const end = textOffset(r!, found.range.endContainer, found.range.endOffset);
      return end > start ? [start, end] : null;
    },
    completeMention(name, address) {
      const m = mention.current;
      const d = doc();
      if (!m || !d || !m.node.isConnected) return;
      const data = m.node.data;
      const end = m.caret + (/^[\p{L}\p{N}._-]*/u.exec(data.slice(m.caret))?.[0].length ?? 0);
      const gap = /^[\s\p{P}]/u.test(data.slice(end)) ? "" : "&nbsp;";
      const range = d.createRange();
      range.setStart(m.node, m.start);
      range.setEnd(m.node, end);
      d.defaultView?.focus();
      d.getSelection()?.removeAllRanges();
      d.getSelection()?.addRange(range);
      mention.current = null;
      latest.current.onMention?.(null);
      run("insertHTML", `<a href="mailto:${escapeHtml(address)}">${escapeHtml(name)}</a>${gap}`);
    },
    mentions(address) {
      const wanted = `mailto:${address}`.toLowerCase();
      return [...(root()?.querySelectorAll("a[href]") ?? [])].filter(
        (a) => a.getAttribute("href")!.toLowerCase() === wanted,
      ).length;
    },
    insertImages,
    resizeImages,
    covers(x, y) {
      const rect = frameRef.current?.getBoundingClientRect();
      return !!rect && x >= rect.left && x <= rect.right && y >= rect.top && y <= rect.bottom;
    },
  }));

  const shortcuts: Record<string, () => void> = {
    "shift+KeyX": () => run("strikeThrough"),
    "shift+Digit8": () => run("insertUnorderedList"),
    "shift+Digit7": () => run("insertOrderedList"),
    "shift+Digit9": toggleQuote,
    "shift+Comma": () => toggleBlock("h1"),
    "shift+Period": () => toggleBlock("h2"),
    "shift+KeyK": () => openPanel("link"),
    Backslash: () => run("removeFormat"),
    "shift+BracketLeft": () => run("outdent"),
    "shift+BracketRight": () => run("indent"),
  };
  const commands = useRef(shortcuts);
  commands.current = shortcuts;

  // The theme's colors, as the frame can't read the app's.
  useEffect(() => {
    const d = wired?.ownerDocument;
    const style = d?.getElementById("theme");
    if (!style || !frameRef.current) return;
    const color = getComputedStyle(frameRef.current).color;
    style.textContent =
      `:root { color-scheme: ${dark ? "dark" : "light"}; } body { color: ${color}; }` +
      ` a { color: ${dark ? "#4da3ff" : "#007cff"}; }`;
  }, [wired, dark]);

  useEffect(() => {
    const r = wired;
    if (!r) return;
    if (html !== held.current) {
      held.current = html;
      r.innerHTML = html;
      r.toggleAttribute("data-empty", empty(r));
    }
  }, [wired, html]);

  useEffect(() => {
    if (!wired) return;
    wired.contentEditable = readOnly ? "false" : "true";
    if (readOnly) setBar(null);
  }, [wired, readOnly]);

  useEffect(() => {
    wired?.setAttribute("data-placeholder", placeholder);
  }, [wired, placeholder]);

  useEffect(() => {
    const frameEl = frameRef.current;
    if (!frameEl) return;
    let done: Document | null = null;
    let unwire = () => {};
    let raf = 0;
    let polls = 0;

    // As soon as the srcdoc has a body: `load` waits for every image.
    function wire() {
      const d = frameEl!.contentDocument;
      const r = d?.getElementById("body");
      if (!d || d === done || d.URL !== "about:srcdoc" || !r) return;
      unwire();
      done = d;
      const host = frameEl!;
      const view = d.defaultView!;
      held.current = latest.current.html;
      r.innerHTML = latest.current.html;
      r.toggleAttribute("data-empty", empty(r));
      r.setAttribute("data-placeholder", latest.current.placeholder);
      const measure = () => setHeight(Math.ceil(r.getBoundingClientRect().height));
      const observer = new ResizeObserver(measure);
      observer.observe(r);
      measure();

      const onKey = (e: KeyboardEvent) => {
        if (e.key === "Escape" && panelRef.current) {
          e.preventDefault();
          closePanel(true);
          return;
        }
        if (latest.current.onKey?.(e)) {
          e.preventDefault();
          e.stopPropagation();
          return;
        }
        const mod = IS_MAC ? e.metaKey : e.ctrlKey;
        const key = e.key.toLowerCase();
        const command = mod && !e.altKey && commands.current[`${e.shiftKey ? "shift+" : ""}${e.code}`];
        if (command) {
          e.preventDefault();
          command();
          return;
        }
        if (mod && e.shiftKey && key === "v") {
          // The paste comes during this key's default action.
          plainPaste.current = true;
          setTimeout(() => (plainPaste.current = false));
          return;
        }
        // Undo and redo stay; the composer's own shortcuts (Send, Cc, Bcc, Attach…) go to it.
        const kept = e.shiftKey ? key === "z" : EDITING_KEYS.has(key);
        if (e.key !== "Escape" && !(mod && !kept)) return;
        const forwarded = new KeyboardEvent("keydown", {
          key: e.key,
          code: e.code,
          ctrlKey: e.ctrlKey,
          metaKey: e.metaKey,
          altKey: e.altKey,
          shiftKey: e.shiftKey,
          bubbles: true,
          cancelable: true,
        });
        if (!host.dispatchEvent(forwarded)) e.preventDefault();
      };
      const onInput = (e: Event) => {
        const type = (e as InputEvent).inputType ?? "";
        // What's pasted may be from Outlook in dark mode.
        if (type.startsWith("insertFrom")) undoDarkMode(r);
        emit(type);
        trackMention();
        trackBar();
        followCaret();
      };
      const onSelection = () => {
        trackMention();
        trackBar();
        followCaret();
      };
      const onPaste = (e: ClipboardEvent) => {
        const plain = plainPaste.current;
        plainPaste.current = false;
        const data = e.clipboardData;
        if (!data) return;
        const images = [...data.files].filter((f) => f.type.startsWith("image/"));
        if (images.length && !plain) {
          e.preventDefault();
          Promise.all(images.map(fileUrl))
            .then((urls) => insertImages(urls))
            .catch((error: Error) => latest.current.onError?.(error.message));
          return;
        }
        // Paste and Match Style: the text alone, in the body's own style.
        if (plain) {
          e.preventDefault();
          d.execCommand("insertText", false, data.getData("text/plain"));
        }
      };
      // Below the text is still the body's; and a link would open in the frame.
      const onPointerDown = (e: MouseEvent) => {
        host.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true }));
        if (e.target === d.documentElement || e.target === d.body) {
          e.preventDefault();
          view.focus();
          r.focus();
          const end = d.createRange();
          end.selectNodeContents(r);
          end.collapse(false);
          d.getSelection()?.removeAllRanges();
          d.getSelection()?.addRange(end);
        }
      };
      const onClick = (e: MouseEvent) => {
        if ((e.target as Element | null)?.closest?.("a[href]")) e.preventDefault();
      };
      const onBlur = () => {
        if (!panelRef.current) setBar(null);
        mention.current = null;
        latest.current.onMention?.(null);
      };
      const onScroll = () => trackBar();
      d.addEventListener("keydown", onKey);
      d.addEventListener("input", onInput);
      d.addEventListener("selectionchange", onSelection);
      d.addEventListener("paste", onPaste);
      d.addEventListener("mousedown", onPointerDown);
      d.addEventListener("click", onClick);
      r.addEventListener("blur", onBlur);
      window.addEventListener("scroll", onScroll, true);
      unwire = () => {
        observer.disconnect();
        d.removeEventListener("keydown", onKey);
        d.removeEventListener("input", onInput);
        d.removeEventListener("selectionchange", onSelection);
        d.removeEventListener("paste", onPaste);
        d.removeEventListener("mousedown", onPointerDown);
        d.removeEventListener("click", onClick);
        r.removeEventListener("blur", onBlur);
        window.removeEventListener("scroll", onScroll, true);
      };
      r.contentEditable = latest.current.readOnly ? "false" : "true";
      setWired(r);
      if (autoFocus) {
        view.focus();
        r.focus();
        const range = d.createRange();
        range.selectNodeContents(r);
        range.collapse(autoFocus === "start");
        d.getSelection()?.removeAllRanges();
        d.getSelection()?.addRange(range);
      }
    }
    const poll = () => {
      wire();
      if (!done && ++polls < 600) raf = requestAnimationFrame(poll);
    };
    poll();
    frameEl.addEventListener("load", wire);
    return () => {
      cancelAnimationFrame(raf);
      frameEl.removeEventListener("load", wire);
      unwire();
    };
  }, []);

  // Panels close on a click outside them.
  useEffect(() => {
    if (!panel) return;
    const onPointer = (e: PointerEvent) => {
      if (!(e.target as Element | null)?.closest?.("[data-format-bar]")) closePanel();
    };
    // Esc in the panel's own field closes it, back to the text.
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape" || !(e.target as Element | null)?.closest?.("[data-format-bar]")) return;
      e.preventDefault();
      e.stopPropagation();
      closePanel(true);
    };
    window.addEventListener("keydown", onKey, true);
    window.addEventListener("pointerdown", onPointer);
    return () => {
      window.removeEventListener("pointerdown", onPointer);
      window.removeEventListener("keydown", onKey, true);
    };
  }, [panel]);

  return (
    <>
      <iframe
        ref={frameRef}
        title="Message"
        sandbox="allow-same-origin"
        referrerPolicy="no-referrer"
        srcDoc={SRC_DOC}
        className="block w-full border-0"
        style={{ height: Math.max(height, minHeight) }}
      />
      {bar && (
        <FormatBar
          bar={bar}
          panel={panel}
          recent={recent}
          onPanel={(p) => (p === panel ? closePanel(true) : openPanel(p))}
          onClose={() => closePanel(true)}
          run={run}
          clearStyle={clearStyle}
          toggleQuote={toggleQuote}
          setBlock={setBlock}
          fontFamily={(family) => {
            run("fontName", family);
            setRecent((r) => noteFont(r, { family }));
            closePanel();
          }}
          fontSize={(px) => {
            fontSize(px);
            closePanel();
          }}
          link={link}
          unlink={unlink}
        />
      )}
    </>
  );
});

export default RichEditor;

const SRC_DOC =
  "<!doctype html><html><head>" +
  `<meta http-equiv="Content-Security-Policy" content="default-src 'none'; img-src data: cid: https: http:; style-src 'unsafe-inline'; font-src data:">` +
  `<meta name="referrer" content="no-referrer"><style>
html, body { margin: 0; background: transparent; }
body { font: 14px/1.57 -apple-system, BlinkMacSystemFont, "Segoe UI", system-ui, Roboto, sans-serif; overflow-wrap: break-word; }
#body { position: relative; padding: 12px 16px 8px; min-height: 1.57em; outline: none; }
#body[data-empty]::before { content: attr(data-placeholder); position: absolute; opacity: .45; pointer-events: none; }
img { max-width: 100%; height: auto; }
p { margin: 0; }
ul, ol { margin: 0; padding-left: 25px; }
h1 { font-size: 1.6em; line-height: 1.3; margin: 0; }
h2 { font-size: 1.25em; line-height: 1.35; margin: 0; }
</style><style id="theme"></style></head><body><div id="body" spellcheck="true"></div></body></html>`;

function loadImage(url: string): Promise<HTMLImageElement> {
  return new Promise((resolve, reject) => {
    const img = new Image();
    img.onload = () => resolve(img);
    img.onerror = () => reject(new Error("Couldn't read the picture."));
    img.src = url;
  });
}

function fileUrl(file: Blob): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => resolve(reader.result as string);
    reader.onerror = () => reject(new Error("Couldn't read the picture."));
    reader.readAsDataURL(file);
  });
}

/** Spark's bar over a selection: Bold through Clear Formatting, in its order. */
function FormatBar({
  bar,
  panel,
  recent,
  onPanel,
  onClose,
  run,
  clearStyle,
  toggleQuote,
  setBlock,
  fontFamily,
  fontSize,
  link,
  unlink,
}: {
  bar: Bar;
  panel: Panel | null;
  recent: { families: string[]; size?: number };
  onPanel: (panel: Panel) => void;
  onClose: () => void;
  run: (command: string, value?: string) => void;
  clearStyle: (property: string, attribute: string) => void;
  toggleQuote: () => void;
  setBlock: (block: Block) => void;
  fontFamily: (family: string) => void;
  fontSize: (px: number) => void;
  link: (url: string) => void;
  unlink: () => void;
}) {
  const ref = useRef<HTMLDivElement>(null);
  const [style, setStyle] = useState<{ left: number; top: number }>({ left: -9999, top: -9999 });

  // Over the selection, centered, kept in the window; under it when there's no room above.
  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    const { width, height } = el.getBoundingClientRect();
    const left = Math.max(8, Math.min(bar.x - width / 2, window.innerWidth - width - 8));
    const top = bar.top - height - 8 >= 8 ? bar.top - height - 8 : bar.bottom + 8;
    setStyle({ left, top });
  }, [bar.x, bar.top, bar.bottom]);

  const button = (key: string, tip: Tip, Icon: LucideIcon, onClick: () => void, on = bar.active.has(key)) => (
    <button
      key={key}
      {...tipAttrs(tip)}
      aria-label={typeof tip === "string" ? tip : tip.label}
      aria-pressed={on}
      // The frame keeps its selection: the button doesn't take focus.
      onMouseDown={(e) => e.preventDefault()}
      onClick={onClick}
      className={`grid size-7 shrink-0 place-items-center rounded-md ${
        on ? "bg-primary/10 text-primary" : "text-text-2 hover:bg-base-content/10 hover:text-base-content"
      }`}
    >
      <Icon size={15} />
    </button>
  );

  return (
    <div
      ref={ref}
      data-format-bar
      role="toolbar"
      aria-label="Formatting"
      className="fixed z-[2010] flex items-center gap-px rounded-lg border border-hairline bg-popover p-1 shadow-popup"
      style={style}
    >
      {button("bold", { label: "Bold", keys: `${MOD}B` }, Bold, () => run("bold"))}
      {button("italic", { label: "Italic", keys: `${MOD}I` }, Italic, () => run("italic"))}
      {button("underline", { label: "Underline", keys: `${MOD}U` }, Underline, () => run("underline"))}
      {button("strikeThrough", { label: "Strikethrough", keys: `${SHIFT_MOD}X` }, Strikethrough, () => run("strikeThrough"))}
      <Anchor open={panel === "link"} panel={<LinkPanel href={bar.href} onSave={link} onRemove={unlink} onCancel={onClose} />}>
        {button("link", { label: "Hyperlink", keys: `${SHIFT_MOD}K` }, Link, () => onPanel("link"), bar.href !== null || panel === "link")}
      </Anchor>
      <Anchor
        open={panel === "color"}
        panel={
          <Colors
            onPick={(color) => {
              if (color) run("foreColor", color);
              else clearStyle("color", "color");
              onClose();
            }}
          />
        }
      >
        {button("color", "Text color", Baseline, () => onPanel("color"), panel === "color")}
      </Anchor>
      <Anchor
        open={panel === "highlight"}
        panel={
          <Colors
            onPick={(color) => {
              if (color) run("hiliteColor", color);
              else clearStyle("background-color", "");
              onClose();
            }}
          />
        }
      >
        {button("highlight", "Highlight color", Highlighter, () => onPanel("highlight"), panel === "highlight")}
      </Anchor>
      <Divider />
      {button("insertUnorderedList", { label: "Bullet list", keys: `${SHIFT_MOD}8` }, List, () => run("insertUnorderedList"))}
      {button("insertOrderedList", { label: "Numbered list", keys: `${SHIFT_MOD}7` }, ListOrdered, () => run("insertOrderedList"))}
      {button("quote", { label: "Quote", keys: `${SHIFT_MOD}9` }, TextQuote, toggleQuote)}
      {button("outdent", { label: "Decrease indent", keys: `${SHIFT_MOD}[` }, IndentDecrease, () => run("outdent"), false)}
      {button("indent", { label: "Increase indent", keys: `${SHIFT_MOD}]` }, IndentIncrease, () => run("indent"), false)}
      <Divider />
      <Anchor
        open={panel === "heading"}
        panel={
          <Choices
            items={[
              { key: "div", label: "Paragraph" },
              { key: "h1", label: "Heading", keys: `${SHIFT_MOD},` },
              { key: "h2", label: "Subheading", keys: `${SHIFT_MOD}.` },
            ]}
            current={bar.block === "h1" || bar.block === "h2" ? bar.block : "div"}
            onPick={(block) => {
              setBlock(block as Block);
              onClose();
            }}
          />
        }
      >
        {button("heading", "Heading", Heading, () => onPanel("heading"), bar.block === "h1" || bar.block === "h2")}
      </Anchor>
      <Anchor
        open={panel === "font"}
        panel={
          <Choices
            items={[
              ...recent.families.map((f) => ({ key: `recent:${f}`, label: f, font: f })),
              ...FONT_FAMILIES.map((f) => ({ key: f, label: f, font: f, divided: recent.families.length > 0 && f === FONT_FAMILIES[0] })),
              { key: "", label: "Default font", divided: true },
            ]}
            onPick={(key) => {
              if (key) fontFamily(key.replace(/^recent:/, ""));
              else {
                clearStyle("font-family", "face");
                onClose();
              }
            }}
          />
        }
      >
        {button("font", "Font", Type, () => onPanel("font"), panel === "font")}
      </Anchor>
      <Anchor
        open={panel === "size"}
        panel={
          <Choices
            items={FONT_SIZES.map((px) => ({
              key: String(px),
              label: px === DEFAULT_FONT_SIZE ? `${px} (Default)` : String(px),
              detail: px === recent.size ? "Recent" : undefined,
            }))}
            onPick={(px) => fontSize(Number(px))}
          />
        }
      >
        {button("size", "Font size", ALargeSmall, () => onPanel("size"), panel === "size")}
      </Anchor>
      {button("removeFormat", { label: "Clear Formatting", keys: `${MOD}\\` }, RemoveFormatting, () => run("removeFormat"), false)}
    </div>
  );
}

function Divider() {
  return <span role="separator" className="mx-0.5 h-5 w-px shrink-0 bg-hairline" />;
}

/** A bar button with its panel under it. */
function Anchor({ open, panel, children }: { open: boolean; panel: ReactNode; children: ReactNode }) {
  return (
    <div className="relative">
      {children}
      {open && <div className="absolute left-1/2 top-full z-10 mt-2 -translate-x-1/2">{panel}</div>}
    </div>
  );
}

function Colors({ onPick }: { onPick: (color: string | null) => void }) {
  return (
    <div className="w-[172px] rounded-xl border border-hairline bg-popover p-2 shadow-popup">
      <button
        className="mb-2 flex w-full items-center gap-2 rounded-md px-1.5 py-1 text-left text-[13px] hover:bg-row-hover"
        onMouseDown={(e) => e.preventDefault()}
        onClick={() => onPick(null)}
      >
        <span className="size-4 rounded-full border border-hairline bg-[linear-gradient(135deg,transparent_45%,#e64d4d_45%,#e64d4d_55%,transparent_55%)]" />
        Default color
      </button>
      <div className="grid grid-cols-5 gap-1.5">
        {COLORS.map((c) => (
          <button
            key={c.color}
            aria-label={c.label}
            data-tip={c.label}
            onMouseDown={(e) => e.preventDefault()}
            onClick={() => onPick(c.color)}
            className="size-6 rounded-full border border-base-content/15 hover:scale-110"
            style={{ backgroundColor: c.color }}
          />
        ))}
      </div>
    </div>
  );
}

function Choices({
  items,
  current,
  onPick,
}: {
  items: { key: string; label: string; keys?: string; detail?: string; font?: string; divided?: boolean }[];
  current?: string;
  onPick: (key: string) => void;
}) {
  return (
    <ul role="menu" className="max-h-64 w-48 overflow-y-auto rounded-xl border border-hairline bg-popover p-1 text-[13px] shadow-popup">
      {items.map((item) => (
        <li key={item.key} role="none" className={item.divided ? "mt-1 border-t border-hairline pt-1" : undefined}>
          <button
            role="menuitemradio"
            aria-checked={current === item.key}
            onMouseDown={(e) => e.preventDefault()}
            onClick={() => onPick(item.key)}
            className="flex w-full items-center gap-2 rounded-md px-2 py-1 text-left hover:bg-row-hover"
          >
            <Check size={13} className={current === item.key ? "" : "invisible"} />
            <span className="min-w-0 flex-1 truncate" style={item.font ? { fontFamily: item.font } : undefined}>
              {item.label}
            </span>
            {(item.keys ?? item.detail) && <span className="text-xs text-text-2">{item.keys ?? item.detail}</span>}
          </button>
        </li>
      ))}
    </ul>
  );
}

function LinkPanel({
  href,
  onSave,
  onRemove,
  onCancel,
}: {
  href: string | null;
  onSave: (url: string) => void;
  onRemove: () => void;
  onCancel: () => void;
}) {
  const [url, setUrl] = useState(href ?? "");
  return (
    <form
      className="flex w-80 items-center gap-1.5 rounded-xl border border-hairline bg-popover p-2 shadow-popup"
      onSubmit={(e) => {
        e.preventDefault();
        onSave(url);
      }}
    >
      <input
        autoFocus
        aria-label="Link"
        className="input input-sm min-w-0 flex-1"
        placeholder="Paste your link here"
        value={url}
        onChange={(e) => setUrl(e.target.value)}
      />
      {href !== null ? (
        <button type="button" className="btn btn-ghost btn-sm px-2" onClick={onRemove}>
          Remove
        </button>
      ) : (
        <button type="button" className="btn btn-ghost btn-sm px-2" onClick={onCancel}>
          Cancel
        </button>
      )}
      <button type="submit" className="btn btn-primary btn-sm px-3" disabled={!url.trim()}>
        Save
      </button>
    </form>
  );
}
