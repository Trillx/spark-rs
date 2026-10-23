import { forwardRef, useEffect, useImperativeHandle, useMemo, useRef, useState } from "react";
import { useDarkTheme } from "../lib/theme";
import { fitImage, resetFonts } from "../lib/signatures";

// A signature is HTML the user pasted from anywhere, so it's shown and
// edited like an email: in a frame sandboxed without `allow-scripts`, under
// its own CSP. `allow-same-origin` lets this side size the frame, read what's
// typed and run the editing commands; with scripts off, the signature can't
// use it. It stays on a light sheet in the dark theme, the way the
// recipient will see it.

const FRAME_CSS = `
:root { color-scheme: light; }
html, body { margin: 0; }
body { padding: 0; min-height: 1.5em; color: #141414; background: #fff; font: 14px/1.5 -apple-system, BlinkMacSystemFont, "Segoe UI", system-ui, Roboto, sans-serif; overflow-wrap: break-word; outline: none; }
body[contenteditable]:empty::before { content: attr(data-placeholder); color: #9aa3ad; pointer-events: none; }
img { max-width: 100%; height: auto; }
a { color: #007cff; }
p { margin: 0; }
`;

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
  const latest = useRef({ onInput, onError });
  latest.current = { onInput, onError };

  useEffect(() => {
    if (html === held.current) return;
    held.current = html;
    setLoaded(html);
  }, [html]);

  const srcDoc = useMemo(
    () =>
      "<!doctype html><html><head>" +
      `<meta http-equiv="Content-Security-Policy" content="default-src 'none'; img-src data: cid: https: http:; style-src 'unsafe-inline'; font-src data:">` +
      `<meta name="referrer" content="no-referrer"><style>${FRAME_CSS}</style></head>` +
      `<body${editable ? ' contenteditable="true"' : ""} data-placeholder="${escapeAttribute(placeholder)}">${loaded}</body></html>`,
    [loaded, editable, placeholder],
  );

  const doc = () => frameRef.current?.contentDocument ?? null;

  function emit() {
    const body = doc()?.body;
    if (!body) return;
    // An emptied editor holds a stray <br>; it's empty.
    const next = body.textContent?.trim() || body.querySelector("img") ? body.innerHTML : "";
    held.current = next;
    latest.current.onInput?.(next);
  }

  async function insertImage(file: Blob) {
    const fitted = await fitImage(file);
    const d = doc();
    d?.defaultView?.focus();
    d?.execCommand("insertImage", false, fitted.url);
    emit();
    return { resized: fitted.resized };
  }

  useImperativeHandle(handle, () => ({
    exec(command, value) {
      const d = doc();
      d?.defaultView?.focus();
      d?.execCommand(command, false, value);
      emit();
    },
    link(url) {
      const d = doc();
      if (!d) return;
      d.defaultView?.focus();
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
        d.addEventListener("input", emit);
        d.addEventListener("paste", onPaste);
      }
      unwire = () => {
        observer.disconnect();
        d.removeEventListener("load", measure, true);
        d.removeEventListener("error", measure, true);
        d.removeEventListener("keydown", onKey);
        d.removeEventListener("click", onClick);
        d.removeEventListener("pointerdown", onPointer);
        d.removeEventListener("input", emit);
        d.removeEventListener("paste", onPaste);
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
    <div className={dark ? "rounded-md bg-white px-2 py-1.5" : undefined}>
      <iframe
        ref={frameRef}
        title={title}
        sandbox="allow-same-origin"
        referrerPolicy="no-referrer"
        srcDoc={srcDoc}
        className="block w-full border-0"
        style={{ height: Math.max(height, minHeight) }}
      />
    </div>
  );
});

export default SignatureFrame;

function escapeAttribute(text: string) {
  return text.replace(/&/g, "&amp;").replace(/"/g, "&quot;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
}
