import { ImageOff } from "lucide-react";
import { useEffect, useMemo, useRef, useState, type MouseEvent as ReactMouseEvent, type ReactNode } from "react";
import { openExternal } from "../api";

// Email HTML is untrusted. It renders in an iframe sandboxed without
// `allow-scripts` (so nothing in it runs, submits or opens windows) under its
// own CSP that blocks every fetch except inline images until the user asks for
// remote ones. `allow-same-origin` only lets this side size the frame and
// catch link clicks; with scripts off, the email itself can't use it.

const OPENABLE = new Set(["http:", "https:", "mailto:"]);
/** A runaway layout (e.g. `100vh` content) stops growing here. */
const MAX_FRAME_HEIGHT = 50_000;

/** Opens an absolute http(s) or mailto link outside the app; anything else is ignored. */
export function openLink(href: string | null | undefined) {
  if (!href) return;
  let url: URL;
  try {
    url = new URL(href.trim());
  } catch {
    return; // relative or malformed
  }
  if (OPENABLE.has(url.protocol)) void openExternal(url.href);
}

/** Whether the HTML loads anything from the network: img src/srcset, background="", CSS url(). */
export function hasRemoteImages(html: string) {
  return /(?:\bsrc(?:set)?|\bbackground)\s*=\s*["']?\s*(?:https?:)?\/\/|url\(\s*["']?\s*(?:https?:)?\/\//i.test(html);
}

/** The bar above a body whose remote images are blocked. */
export function BlockedImagesBar({ onLoad }: { onLoad: () => void }) {
  return (
    <div className="mx-5 mb-1 mt-2 flex items-center gap-2 rounded-lg bg-base-200 px-3 py-1.5 text-xs leading-5 text-text-2">
      <ImageOff size={14} className="shrink-0" />
      <span className="min-w-0 flex-1">Images are blocked to protect your privacy.</span>
      <button className="shrink-0 font-medium text-primary hover:underline" onClick={onLoad}>
        Load images
      </button>
    </div>
  );
}

/** Plain-text paragraphs, with web addresses as links. */
export function TextBody({ paragraphs }: { paragraphs: string[] }) {
  return (
    <div className="select-text space-y-3 break-words px-5 pb-5 pt-2 text-sm leading-[1.57]">
      {paragraphs.map((paragraph, i) => (
        <p key={i} className="whitespace-pre-line">
          {linkify(paragraph)}
        </p>
      ))}
    </div>
  );
}

function linkify(text: string): ReactNode[] {
  const parts: ReactNode[] = [];
  let last = 0;
  for (const match of text.matchAll(/https?:\/\/[^\s<>"]+/g)) {
    // Trailing punctuation usually ends the sentence, not the URL.
    const url = match[0].replace(/[.,;:!?)\]'"]+$/, "");
    parts.push(text.slice(last, match.index));
    const open = (e: ReactMouseEvent) => {
      e.preventDefault();
      openLink(url);
    };
    parts.push(
      <a key={match.index} href={url} title={url} onClick={open} onAuxClick={open} className="text-primary hover:underline">
        {url}
      </a>,
    );
    last = match.index + url.length;
  }
  parts.push(text.slice(last));
  return parts;
}

/** The email's HTML in a sandboxed frame sized to its content. */
export function HtmlBody({ html, remoteImages }: { html: string; remoteImages: boolean }) {
  const frameRef = useRef<HTMLIFrameElement>(null);
  /** Kept across the reload that "Load images" causes, so the card doesn't jump. */
  const [height, setHeight] = useState(0);
  const srcDoc = useMemo(() => frameDocument(html, remoteImages), [html, remoteImages]);

  useEffect(() => {
    const frame = frameRef.current;
    if (!frame) return;
    let wired: Document | null = null;
    let unwire = () => {};
    let measure = () => {};
    let raf = 0;
    let polls = 0;

    // Wire up as soon as the srcdoc document has a body, not at `load`,
    // which waits for every image.
    function wire() {
      const doc = frame!.contentDocument;
      if (!doc || doc === wired || doc.URL !== "about:srcdoc" || !doc.body) return;
      unwire();
      wired = doc;
      [measure, unwire] = wireFrame(doc, (h) => setHeight(Math.min(h, MAX_FRAME_HEIGHT)));
    }
    const poll = () => {
      wire();
      if (!wired && ++polls < 600) raf = requestAnimationFrame(poll);
    };
    const onLoad = () => {
      wire();
      measure();
    };
    poll();
    frame.addEventListener("load", onLoad);
    return () => {
      cancelAnimationFrame(raf);
      frame.removeEventListener("load", onLoad);
      unwire();
    };
  }, [srcDoc]);

  return (
    // Light mode: the sheet blends into the card. Dark mode: a white inset sheet.
    <div className="mx-5 mb-5 mt-2 overflow-hidden rounded-lg bg-white p-[var(--spark-email-inset)]">
      <iframe
        // A new frame per document, so a reload never wires up the old one.
        key={remoteImages ? "remote" : "blocked"}
        ref={frameRef}
        title="Email content"
        sandbox="allow-same-origin"
        referrerPolicy="no-referrer"
        srcDoc={srcDoc}
        className="block w-full border-0"
        style={{ height }}
      />
    </div>
  );
}

/**
 * The frame's document: our CSP, base and styles first in <head>, then the
 * email. Its own CSP, base, refresh and <link> tags are dropped (a policy can
 * only be tightened, but a refresh or prefetch would still reach the network).
 */
function frameDocument(html: string, remoteImages: boolean) {
  const doc = new DOMParser().parseFromString(html, "text/html");
  for (const el of doc.querySelectorAll("script, meta[http-equiv], base, link, iframe, frame, frameset, object, embed")) {
    el.remove();
  }
  const imgSrc = remoteImages ? "data: cid: https: http:" : "data: cid:";
  doc.head.insertAdjacentHTML(
    "afterbegin",
    `<meta http-equiv="Content-Security-Policy" content="default-src 'none'; img-src ${imgSrc}; style-src 'unsafe-inline'; font-src data:">` +
      `<meta name="referrer" content="no-referrer">` +
      `<meta name="color-scheme" content="light">` +
      `<base target="_blank">` +
      `<style>${FRAME_CSS}</style>`,
  );
  return `<!doctype html>${doc.documentElement.outerHTML}`;
}

// `:root:root` outranks the email's own html/body rules, so the document's
// height is its content's and the frame can size to it.
const FRAME_CSS = `
:root:root { height: auto !important; min-height: 0 !important; overflow-y: hidden !important; overflow-x: auto; color-scheme: light; }
:root:root > body { height: auto !important; min-height: 0 !important; }
body { margin: 0; background: #fff; color: #141414; font: 14px/1.5 -apple-system, BlinkMacSystemFont, "Segoe UI", system-ui, Roboto, sans-serif; overflow-wrap: break-word; word-wrap: break-word; }
img { max-width: 100%; height: auto; }
table { max-width: 100%; }
pre { white-space: pre-wrap; }
a { color: #007cff; }
`;

/**
 * Sizes the frame to its document and routes its clicks and keys to the app.
 * Returns [measure, unwire]. Listeners added from here run even though the
 * frame can't run scripts of its own.
 */
function wireFrame(doc: Document, setHeight: (height: number) => void): [() => void, () => void] {
  const root = doc.documentElement;
  /** Forwarded events start at the <iframe>, as if the frame were part of the page. */
  const host = doc.defaultView?.frameElement ?? document.body;
  const measure = () => {
    const win = doc.defaultView;
    // A wide email scrolls sideways; leave room for that scrollbar.
    const scrollbar = win && root.scrollWidth > root.clientWidth ? Math.max(0, win.innerHeight - root.clientHeight) : 0;
    setHeight(Math.ceil(root.getBoundingClientRect().height) + scrollbar);
  };

  const observer = new ResizeObserver(measure);
  observer.observe(root);

  const onClick = (e: MouseEvent) => {
    if (e.button > 1) return;
    const link = (e.target as Element | null)?.closest?.("a[href]");
    if (!link) return;
    e.preventDefault();
    openLink(link.getAttribute("href"));
  };
  // Keys typed while the email has focus still drive the app (E for Done, Esc, …).
  const onKey = (e: KeyboardEvent) => {
    if ((e.target as Element | null)?.closest?.("input, textarea, select, [contenteditable]")) return;
    const forwarded = new KeyboardEvent("keydown", {
      key: e.key,
      code: e.code,
      ctrlKey: e.ctrlKey,
      metaKey: e.metaKey,
      altKey: e.altKey,
      shiftKey: e.shiftKey,
      repeat: e.repeat,
      bubbles: true,
      cancelable: true,
    });
    if (!host.dispatchEvent(forwarded)) e.preventDefault();
  };
  // Open menus close on an outside pointerdown, which a click in the frame wouldn't send.
  const onPointer = () => host.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true }));

  // Image load/error events don't bubble, but they do pass the document while capturing.
  doc.addEventListener("load", measure, true);
  doc.addEventListener("error", measure, true);
  doc.addEventListener("click", onClick);
  doc.addEventListener("auxclick", onClick);
  doc.addEventListener("keydown", onKey);
  doc.addEventListener("pointerdown", onPointer);
  measure();

  return [
    measure,
    () => {
      observer.disconnect();
      doc.removeEventListener("load", measure, true);
      doc.removeEventListener("error", measure, true);
      doc.removeEventListener("click", onClick);
      doc.removeEventListener("auxclick", onClick);
      doc.removeEventListener("keydown", onKey);
      doc.removeEventListener("pointerdown", onPointer);
    },
  ];
}
