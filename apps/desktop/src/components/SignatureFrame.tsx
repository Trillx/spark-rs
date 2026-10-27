import { forwardRef, useEffect, useImperativeHandle, useMemo, useRef, useState } from "react";
import { backdropColor, cssColor, darkColors, type Rgb } from "../lib/darkColors";
import { fitImage, resetFonts, undoDarkMode } from "../lib/signatures";
import { useDarkTheme } from "../lib/theme";

// A signature is HTML the user pasted from anywhere, so it's shown and
// edited like an email: in a frame sandboxed without `allow-scripts`, under
// its own CSP. `allow-same-origin` lets this side size the frame, read what's
// typed and run the editing commands; with scripts off, the signature can't
// use it. It takes the app's colors, like the body above it; in the dark
// theme the colors it sets itself are adapted the way the reader adapts mail
// (`adapt`). What's kept and sent has them as they are.

const FRAME_CSS = `
html, body { margin: 0; background: transparent; }
body { padding: 0; min-height: 1.5em; font: 14px/1.5 -apple-system, BlinkMacSystemFont, "Segoe UI", system-ui, Roboto, sans-serif; overflow-wrap: break-word; outline: none; }
body[contenteditable]:empty::before { content: attr(data-placeholder); color: #9aa3ad; pointer-events: none; }
img { max-width: 100%; height: auto; }
p { margin: 0; }
`;

const LIGHT_CSS = `:root { color-scheme: light; } body { color: #141414; } a { color: #007cff; }`;
const DARK_CSS = `:root { color-scheme: dark; } body { color: #dee3eb; } a { color: #4da3ff; }`;

/** Marks on the elements whose colors are adapted, which the HTML that's kept leaves out. */
const COLOR_MARK = "data-spark-color";
const FILL_MARK = "data-spark-fill";
const MARKED = `[${COLOR_MARK}], [${FILL_MARK}]`;

/** Shortcuts the editor keeps; other ones with Ctrl/Cmd, and Esc, go to the app. */
const EDITING_KEYS = new Set(["a", "b", "c", "i", "u", "v", "x", "y", "z"]);

export interface SignatureFrameHandle {
  /** Runs an editing command (bold, italic, …) on the selection. */
  exec: (command: string, value?: string) => void;
  /** Links the selected text to `url`, or with nothing selected, inserts the link. */
  link: (url: string) => void;
  insertImage: (file: Blob) => Promise<{ resized: boolean }>;
  resetFonts: () => void;
  focus: () => void;
}

/**
 * Signature HTML in a sandboxed frame sized to its content: a preview, or
 * with `onInput`, an editor. A new `html` that isn't what was typed loads
 * in place of it.
 */
const SignatureFrame = forwardRef<
  SignatureFrameHandle,
  {
    html: string;
    /** Makes it editable; called with the HTML as it changes. */
    onInput?: (html: string) => void;
    placeholder?: string;
    /** Smallest height, in px. */
    minHeight?: number;
    title?: string;
    /** An image couldn't go in, in words. */
    onError?: (text: string) => void;
  }
>(function SignatureFrame({ html, onInput, placeholder = "", minHeight = 0, title = "Signature", onError }, handle) {
  const frameRef = useRef<HTMLIFrameElement>(null);
  const [height, setHeight] = useState(0);
  const dark = useDarkTheme();
  const editable = onInput !== undefined;
  /** What the frame holds: its first HTML, then what's typed. */
  const held = useRef(html);
  const [loaded, setLoaded] = useState(html);
  /** What the signature is shown on, which its colors are adapted to; null in the light theme. */
  const card = useRef<Rgb | null>(null);
  const latest = useRef({ onInput, onError });
  latest.current = { onInput, onError };

  useEffect(() => {
    if (html === held.current) return;
    held.current = html;
    setLoaded(html);
  }, [html]);

  // What's held, loaded again for a new `html` (`loaded`) or theme. In the
  // dark theme it's hidden until `adapt` has run, so it doesn't flash.
  const srcDoc = useMemo(
    () =>
      "<!doctype html><html><head>" +
      `<meta http-equiv="Content-Security-Policy" content="default-src 'none'; img-src data: cid: https: http:; style-src 'unsafe-inline'; font-src data:">` +
      `<meta name="referrer" content="no-referrer"><style>${FRAME_CSS}${dark ? DARK_CSS : LIGHT_CSS}</style>` +
      `<style id="spark-adapted">${dark ? ":root { visibility: hidden; }" : ""}</style></head>` +
      `<body${editable ? ' contenteditable="true"' : ""} data-placeholder="${escapeAttribute(placeholder)}">${held.current}</body></html>`,
    [loaded, editable, placeholder, dark],
  );

  const doc = () => frameRef.current?.contentDocument ?? null;

  /** After a change: adapts what's new, and passes on the HTML. */
  function emit() {
    const d = doc();
    if (!d?.body) return;
    const body = d.body;
    adapt(d, card.current);
    // An emptied editor holds a stray <br>; it's empty.
    const next = body.textContent?.trim() || body.querySelector("img") ? keptHtml(body) : "";
    held.current = next;
    latest.current.onInput?.(next);
  }

  /**
   * Turns the adapted colors off for an edit or a copy. The browser writes
   * the colors it sees into what it adds (typing over a colored word, a
   * paste) and what it copies, and those have to be the signature's own.
   * `emit` adapts again after an edit; this does before the next paint
   * after one that changed nothing, or a copy, which leaves out the marks.
   */
  function original(d: Document | null, copying = false) {
    const sheet = d?.getElementById("spark-adapted") as HTMLStyleElement | null | undefined;
    if (!d || !sheet || !card.current) return;
    sheet.disabled = true;
    if (copying) for (const el of d.querySelectorAll(MARKED)) unmark(el);
    requestAnimationFrame(() => {
      if (sheet.disabled) adapt(d, card.current);
    });
  }

  async function insertImage(file: Blob) {
    const fitted = await fitImage(file);
    const d = doc();
    d?.defaultView?.focus();
    original(d);
    d?.execCommand("insertImage", false, fitted.url);
    emit();
    return { resized: fitted.resized };
  }

  useImperativeHandle(handle, () => ({
    exec(command, value) {
      const d = doc();
      d?.defaultView?.focus();
      original(d);
      d?.execCommand(command, false, value);
      emit();
    },
    link(url) {
      const d = doc();
      if (!d) return;
      d.defaultView?.focus();
      original(d);
      if (d.getSelection()?.isCollapsed === false) d.execCommand("createLink", false, url);
      else d.execCommand("insertHTML", false, `<a href="${escapeAttribute(url)}">${escapeAttribute(url)}</a>`);
      emit();
    },
    insertImage,
    resetFonts() {
      const body = doc()?.body;
      if (!body) return;
      resetFonts(body);
      emit();
    },
    focus() {
      const d = doc();
      if (!d?.body) return;
      d.defaultView?.focus();
      d.body.focus();
      // At the end, as Spark puts the caret after choosing a signature.
      const range = d.createRange();
      range.selectNodeContents(d.body);
      range.collapse(false);
      d.getSelection()?.removeAllRanges();
      d.getSelection()?.addRange(range);
    },
  }));

  useEffect(() => {
    const frameEl = frameRef.current;
    if (!frameEl) return;
    let wired: Document | null = null;
    let unwire = () => {};
    let raf = 0;
    let polls = 0;

    // As soon as the srcdoc has a body: `load` waits for every image.
    function wire() {
      const d = frameEl!.contentDocument;
      if (!d || d === wired || d.URL !== "about:srcdoc" || !d.body) return;
      unwire();
      wired = d;
      card.current = dark ? backdropColor(frameEl!) : null;
      // One copied from Outlook in dark mode, saved before it was turned back.
      if (undoDarkMode(d.body) && editable) emit();
      else adapt(d, card.current);
      // Once all of it is there.
      if (d.readyState === "loading") {
        d.addEventListener("DOMContentLoaded", () => adapt(d, card.current), { once: true });
      }
      const root = d.documentElement;
      const measure = () => setHeight(Math.ceil(root.getBoundingClientRect().height));
      const observer = new ResizeObserver(measure);
      observer.observe(root);
      measure();
      const host = frameEl!;
      const onKey = (e: KeyboardEvent) => {
        const mod = e.ctrlKey || e.metaKey;
        if (e.key !== "Escape" && !(mod && !EDITING_KEYS.has(e.key.toLowerCase()))) return;
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
      // A link would open inside the frame; in the editor a click only places the caret.
      const onClick = (e: MouseEvent) => {
        if ((e.target as Element | null)?.closest?.("a[href]")) e.preventDefault();
      };
      // Open menus close on an outside pointerdown, which a click in the frame wouldn't send.
      const onPointer = () => host.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true }));
      // The editor's own commands; the toolbar's run through execCommand, which doesn't send this.
      const onBeforeInput = () => original(d);
      const onCopy = () => original(d, true);
      // What's pasted may be from Outlook in dark mode.
      const onInput = () => {
        undoDarkMode(d.body);
        emit();
      };
      const onPaste = (e: ClipboardEvent) => {
        const image = [...(e.clipboardData?.files ?? [])].find((f) => f.type.startsWith("image/"));
        if (!image) return;
        e.preventDefault();
        insertImage(image).catch((error: Error) => latest.current.onError?.(error.message));
      };
      d.addEventListener("load", measure, true);
      d.addEventListener("error", measure, true);
      d.addEventListener("keydown", onKey);
      d.addEventListener("click", onClick);
      d.addEventListener("pointerdown", onPointer);
      if (editable) {
        d.addEventListener("beforeinput", onBeforeInput);
        d.addEventListener("input", onInput);
        d.addEventListener("paste", onPaste);
        d.addEventListener("copy", onCopy);
        d.addEventListener("cut", onCopy);
        d.addEventListener("dragstart", onCopy);
      }
      unwire = () => {
        observer.disconnect();
        d.removeEventListener("load", measure, true);
        d.removeEventListener("error", measure, true);
        d.removeEventListener("keydown", onKey);
        d.removeEventListener("click", onClick);
        d.removeEventListener("pointerdown", onPointer);
        d.removeEventListener("beforeinput", onBeforeInput);
        d.removeEventListener("input", onInput);
        d.removeEventListener("paste", onPaste);
        d.removeEventListener("copy", onCopy);
        d.removeEventListener("cut", onCopy);
        d.removeEventListener("dragstart", onCopy);
      };
    }
    const poll = () => {
      wire();
      if (!wired && ++polls < 600) raf = requestAnimationFrame(poll);
    };
    poll();
    frameEl.addEventListener("load", wire);
    return () => {
      cancelAnimationFrame(raf);
      frameEl.removeEventListener("load", wire);
      unwire();
    };
  }, [srcDoc]);

  return (
    <iframe
      ref={frameRef}
      title={title}
      sandbox="allow-same-origin"
      referrerPolicy="no-referrer"
      srcDoc={srcDoc}
      className="block w-full border-0"
      style={{ height: Math.max(height, minHeight) }}
    />
  );
});

export default SignatureFrame;

/**
 * Shows the signature's colors adapted to the dark theme's `card`, or as
 * they are with none. The adapted ones are rules in the `spark-adapted`
 * sheet for marks on the elements, so the elements keep their own colors.
 * Shows the document, which the sheet hid until now.
 */
function adapt(doc: Document, card: Rgb | null) {
  const sheet = doc.getElementById("spark-adapted") as HTMLStyleElement | null;
  if (!sheet) return;
  // Adapted afresh from the colors as they are.
  sheet.disabled = true;
  const adapted = card ? darkColors(doc, card) : [];
  sheet.disabled = false;
  const rules = new Set<string>();
  const marked = new Set<Element>();
  for (const { el, color, background } of adapted) {
    marked.add(el);
    mark(el, COLOR_MARK, color && cssColor(color));
    mark(el, FILL_MARK, background && cssColor(background));
    if (color) rules.add(`[${COLOR_MARK}="${cssColor(color)}"] { color: ${cssColor(color)} !important; }`);
    if (background) {
      rules.add(`[${FILL_MARK}="${cssColor(background)}"] { background-color: ${cssColor(background)} !important; }`);
    }
  }
  for (const el of doc.querySelectorAll(MARKED)) {
    if (!marked.has(el)) unmark(el);
  }
  sheet.textContent = [...rules].join("\n");
}

function mark(el: Element, name: string, value: string | undefined) {
  if (value === undefined) el.removeAttribute(name);
  else if (el.getAttribute(name) !== value) el.setAttribute(name, value);
}

function unmark(el: Element) {
  el.removeAttribute(COLOR_MARK);
  el.removeAttribute(FILL_MARK);
}

/** The signature's HTML as it's kept and sent: without the marks `adapt` adds. */
function keptHtml(body: HTMLElement) {
  if (!body.querySelector(MARKED)) return body.innerHTML;
  const copy = body.cloneNode(true) as HTMLElement;
  for (const el of copy.querySelectorAll(MARKED)) unmark(el);
  return copy.innerHTML;
}

function escapeAttribute(text: string) {
  return text.replace(/&/g, "&amp;").replace(/"/g, "&quot;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
}
