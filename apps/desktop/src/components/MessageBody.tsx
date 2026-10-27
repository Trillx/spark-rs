import { Ellipsis, ImageOff } from "lucide-react";
import { useEffect, useMemo, useRef, useState, type MouseEvent as ReactMouseEvent, type ReactNode } from "react";
import { cardColor, cssColor, darkColors, type Rgb } from "../lib/darkColors";
import { openLink } from "../lib/links";
import { cssIsRemote, isRemoteUrl, srcsetIsRemote } from "../lib/remoteImages";
import { cidKey } from "../lib/richText";
import { useDarkTheme } from "../lib/theme";
import type { InlineImages } from "../types";
import { ImageViewer, type ViewedImage } from "./ImageViewer";

// Email HTML is untrusted. It renders in an iframe sandboxed without
// `allow-scripts` (so nothing in it runs, submits or opens windows) under its
// own CSP that blocks every fetch except inline images, and remote ones when
// they're allowed (`remoteImages.ts`); tracking pixels are gone by then.
// `allow-same-origin` only lets this side size the frame and catch link
// clicks; with scripts off, the email itself can't use it.
//
// In the dark theme an email is shown the way Spark shows it (see
// `emailScheme`): in its own dark design if it has one, on a white sheet if
// it's built to stay light, and otherwise with its colors adapted to the dark
// card (`adaptColors`). A reply's quote of earlier mail folds behind "•••".
// Double-clicking a picture opens it in the viewer (`ImageViewer`).

/** A runaway layout (e.g. `100vh` content) stops growing here. */
const MAX_FRAME_HEIGHT = 50_000;

/** Whether the HTML embeds images with `cid:` URLs (the same test as `cid_refs` on the Rust side). */
export function hasInlineImages(html: string) {
  return /["'=(]cid:/i.test(html);
}

/**
 * A body's state that outlives it. The reader unmounts bodies that scroll far
 * out of view and hands the same object back when one remounts, so its quote
 * and its size come back as they were.
 */
export interface KeptBody {
  quoteShown?: boolean;
  /** An HTML body's frame height. */
  height?: number;
}

/** Spark's bar above a body whose remote images are off: Show images for this email, or Settings to turn them on. */
function BlockedImagesBar({ onShow, onSettings }: { onShow: () => void; onSettings: () => void }) {
  return (
    <div className="mx-5 mb-1 mt-2 flex items-center gap-2 rounded-lg bg-base-200 px-3 py-1.5 text-xs leading-5 text-text-2">
      <ImageOff size={14} className="shrink-0" />
      <span className="min-w-0 flex-1">
        Images aren't shown, to protect your privacy. To always show them, turn on Load remote images in{" "}
        <button className="font-medium text-primary hover:underline" onClick={onSettings}>
          Settings
        </button>
        .
      </span>
      <button className="btn btn-primary btn-xs shrink-0 rounded-md px-2.5" onClick={onShow}>
        Show images
      </button>
    </div>
  );
}

/** The "•••" under a reply that shows or hides its quote of earlier mail. */
function QuoteToggle({ shown, onToggle }: { shown: boolean; onToggle: () => void }) {
  const label = shown ? "Hide quoted text" : "Show quoted text";
  return (
    <button
      className="mx-5 mb-4 grid h-4 w-7 place-items-center rounded-full bg-base-content/10 text-text-2 hover:bg-base-content/15 hover:text-base-content"
      data-tip={label}
      aria-label={label}
      aria-expanded={shown}
      onClick={onToggle}
    >
      <Ellipsis size={14} />
    </button>
  );
}

/** Plain-text paragraphs, with web addresses as links. A reply's quote folds away unless `quotes` is off. */
export function TextBody({
  paragraphs,
  quotes = true,
  kept,
}: {
  paragraphs: string[];
  quotes?: boolean;
  kept?: KeptBody;
}) {
  const [shown, setShown] = useState(kept?.quoteShown ?? false);
  const toggle = () => {
    if (kept) kept.quoteShown = !shown;
    setShown(!shown);
  };
  const cut = quotes ? quoteStart(paragraphs) : -1;
  const visible = cut < 0 || shown ? paragraphs : paragraphs.slice(0, cut);
  return (
    <>
      <div className={`select-text space-y-3 break-words px-5 pt-2 text-sm leading-[1.57] ${cut < 0 ? "pb-5" : "pb-3"}`}>
        {visible.map((paragraph, i) => (
          <p key={i} className={`whitespace-pre-line ${cut >= 0 && i >= cut ? "text-text-2" : ""}`}>
            {linkify(paragraph)}
          </p>
        ))}
      </div>
      {cut >= 0 && <QuoteToggle shown={shown} onToggle={toggle} />}
    </>
  );
}

/** How a plain-text reply's quote starts: an attribution line, Outlook's header, or "> " lines. */
const TEXT_QUOTE = [
  /^(?:On\b[^]{0,300}?|[^\n]{1,200}\s)wrote:\s*(?:\n|$)/,
  /^-{2,}\s*Original Message\s*-{2,}/i,
  /^From:[^\n]*\n(?:Sent|Date):/,
  /^>/,
];

/** The paragraph a reply's quote starts at; -1 if there's none, or nothing before it. */
function quoteStart(paragraphs: string[]) {
  const i = paragraphs.findIndex((p) => TEXT_QUOTE.some((pattern) => pattern.test(p.trimStart())));
  return i > 0 ? i : -1;
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
      <a key={match.index} href={url} data-tip={url} onClick={open} onAuxClick={open} className="text-primary hover:underline">
        {url}
      </a>,
    );
    last = match.index + url.length;
  }
  parts.push(text.slice(last));
  return parts;
}

/**
 * The email's HTML in a sandboxed frame sized to its content. Its embedded
 * images show once `inlineImages` has them; until then they're blank. While
 * `remoteImages` is off, an email that loads anything from the web gets the
 * bar to show it or open the setting. A reply's quote folds away unless
 * `quotes` is off. Double-clicking a picture opens it over the window.
 */
export function HtmlBody({
  html,
  remoteImages,
  inlineImages,
  quotes = true,
  onShowImages,
  onImageSettings,
  kept,
}: {
  html: string;
  remoteImages: boolean;
  inlineImages: InlineImages | null;
  quotes?: boolean;
  onShowImages: () => void;
  onImageSettings: () => void;
  kept?: KeptBody;
}) {
  const frameRef = useRef<HTMLIFrameElement>(null);
  /**
   * Kept across the reloads that "Show images" and arriving embedded images
   * cause, and in `kept` across remounts, so the card doesn't jump.
   */
  const [height, setHeight] = useState(kept?.height ?? 0);
  const [quoteShown, setQuoteShown] = useState(kept?.quoteShown ?? false);
  const toggleQuote = () => {
    if (kept) kept.quoteShown = !quoteShown;
    setQuoteShown(!quoteShown);
  };
  const dark = useDarkTheme();
  const frame = useMemo(
    () => frameDocument(html, { remoteImages, inlineImages, quotes, card: dark ? cardColor() : null }),
    [html, remoteImages, inlineImages, quotes, dark],
  );
  /** What the laid-out frame shows from the web (`showsRemote`); until it's laid out, `frame.remote`. */
  const [shown, setShown] = useState<{ frame: Frame; remote: boolean } | null>(null);
  const remoteShown = shown?.frame === frame ? shown.remote : frame.remote;
  /** The quote's state for a document that loads later. */
  const quoteShownRef = useRef(quoteShown);
  quoteShownRef.current = quoteShown;
  const wiredRef = useRef<{ doc: Document; measure: () => void } | null>(null);
  const [viewing, setViewing] = useState<{ images: ViewedImage[]; start: number } | null>(null);

  useEffect(() => {
    const frameEl = frameRef.current;
    if (!frameEl) return;
    let wired: Document | null = null;
    let unwire = () => {};
    let measure = () => {};
    let raf = 0;
    let polls = 0;

    // Wire up as soon as the srcdoc document has a body, not at `load`,
    // which waits for every image.
    function wire() {
      const doc = frameEl!.contentDocument;
      if (!doc || doc === wired || doc.URL !== "about:srcdoc" || !doc.body) return;
      unwire();
      wired = doc;
      // Once it's parsed (and its colors adapted, which hides it meanwhile), and again as its
      // layout changes: a media query or the quote can show or hide a picture.
      let laidOut = false;
      const check = () => {
        if (!laidOut || remoteImages || !frame.remote) return;
        const remote = showsRemote(doc);
        setShown((s) => (s?.frame === frame && s.remote === remote ? s : { frame, remote }));
      };
      [measure, unwire] = wireFrame(
        doc,
        (h) => {
          const capped = Math.min(h, MAX_FRAME_HEIGHT);
          if (kept) kept.height = capped;
          setHeight(capped);
          check();
        },
        (images, start) => setViewing({ images, start }),
      );
      wiredRef.current = { doc, measure };
      showQuote(doc, quoteShownRef.current);
      const card = frame.adapt;
      whenParsed(doc, () => {
        if (card) {
          adaptColors(doc, card);
          measure();
        }
        laidOut = true;
        check();
      });
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
    frameEl.addEventListener("load", onLoad);
    return () => {
      cancelAnimationFrame(raf);
      frameEl.removeEventListener("load", onLoad);
      unwire();
      wiredRef.current = null;
    };
  }, [frame]);

  useEffect(() => {
    const wired = wiredRef.current;
    if (!wired) return;
    showQuote(wired.doc, quoteShown);
    wired.measure();
  }, [quoteShown]);

  return (
    <>
      {!remoteImages && remoteShown && <BlockedImagesBar onShow={onShowImages} onSettings={onImageSettings} />}
      {/* The light theme's sheet blends into the card. In the dark theme, emails
          that stay light sit on a white inset sheet; the rest take the card's color. */}
      <div
        className={`mx-5 mt-2 overflow-hidden rounded-lg ${frame.quoted ? "mb-2" : "mb-5"} ${
          frame.sheet ? "bg-white p-[var(--spark-email-inset)]" : ""
        }`}
      >
        <iframe
          // A new frame per document, so a reload never wires up the old one.
          key={remoteImages ? "remote" : "blocked"}
          ref={frameRef}
          title="Email content"
          sandbox="allow-same-origin"
          referrerPolicy="no-referrer"
          srcDoc={frame.srcDoc}
          className="block w-full border-0"
          style={{ height }}
        />
      </div>
      {frame.quoted && <QuoteToggle shown={quoteShown} onToggle={toggleQuote} />}
      {viewing && (
        <ImageViewer
          images={viewing.images}
          start={viewing.start}
          onClose={() => {
            setViewing(null);
            // Back to the email, in the frame that's there now: images allowed from another window replace it.
            frameRef.current?.focus();
          }}
        />
      )}
    </>
  );
}

interface Frame {
  srcDoc: string;
  /** On a white sheet: in the light theme, and emails that stay light in the dark one. */
  sheet: boolean;
  /** The card color to adapt the email's colors to, for emails without a dark design of their own. */
  adapt: Rgb | null;
  /** A reply's quote of earlier mail is folded into `#spark-quoted`. */
  quoted: boolean;
  /** It loads something from the web (`loadsRemote`), shown or not. */
  remote: boolean;
}

/**
 * The frame's document: our CSP, base and styles first in <head>, then the
 * email. Its own CSP, base, refresh and <link> tags are dropped (a policy can
 * only be tightened, but a refresh or prefetch would still reach the network).
 * Embedded images become data URLs; ones not (yet) fetched stay hidden by
 * FRAME_CSS rather than showing as broken. `card` is the dark theme's card
 * color, null in the light theme.
 */
function frameDocument(
  html: string,
  {
    remoteImages,
    inlineImages,
    quotes,
    card,
  }: { remoteImages: boolean; inlineImages: InlineImages | null; quotes: boolean; card: Rgb | null },
): Frame {
  const doc = new DOMParser().parseFromString(html, "text/html");
  // Before its <meta http-equiv> tags go: they count.
  const scheme = card ? emailScheme(doc) : "light";
  const dark = scheme !== "light";
  for (const el of doc.querySelectorAll("script, meta[http-equiv], base, link, iframe, frame, frameset, object, embed")) {
    el.remove();
  }
  if (inlineImages) {
    for (const img of doc.querySelectorAll("img[src]")) {
      const src = img.getAttribute("src")!;
      const data = /^\s*cid:/i.test(src) ? inlineImages[cidKey(src)] : undefined;
      if (data) img.setAttribute("src", data);
    }
  }
  preferColorScheme(doc, dark);
  const remote = loadsRemote(doc);
  const quoted = quotes && foldQuote(doc);
  const imgSrc = remoteImages ? "data: cid: https: http:" : "data: cid:";
  const css = card && dark ? FRAME_CSS + darkFrameCss(card, scheme === "adapt") : FRAME_CSS;
  doc.head.insertAdjacentHTML(
    "afterbegin",
    `<meta http-equiv="Content-Security-Policy" content="default-src 'none'; img-src ${imgSrc}; style-src 'unsafe-inline'; font-src data:">` +
      `<meta name="referrer" content="no-referrer">` +
      `<meta name="color-scheme" content="${dark ? "dark" : "light"}">` +
      `<base target="_blank">` +
      `<style>${css}</style>`,
  );
  return {
    srcDoc: `<!doctype html>${doc.documentElement.outerHTML}`,
    sheet: !dark,
    adapt: card && scheme === "adapt" ? card : null,
    quoted,
    remote,
  };
}

/**
 * The elements that load a picture from the web: by src, srcset, background, a video's poster or
 * inline style. Video and audio themselves never load (the frame's policy has no media-src).
 * Read from the parsed document, so an attribute's entities count as the frame reads them.
 * Tracking pixels aren't among them: the backend has taken them out, so a newsletter with only a
 * tracker gets no bar, as in Spark.
 */
function remoteElements(doc: Document) {
  return [...doc.querySelectorAll("[src], [srcset], [background], [poster], [style]")].filter((el) => {
    const attr = (name: string) => el.getAttribute(name) ?? "";
    const tag = el.tagName;
    const media = tag === "VIDEO" || tag === "AUDIO" || (tag === "SOURCE" && el.parentElement?.tagName !== "PICTURE");
    return (
      (!media && (isRemoteUrl(attr("src")) || srcsetIsRemote(attr("srcset")))) ||
      isRemoteUrl(attr("background")) ||
      isRemoteUrl(attr("poster")) ||
      cssIsRemote(attr("style"))
    );
  });
}

/** Whether a style sheet loads anything from the web; what it applies to isn't worked out. */
function sheetsLoadRemote(doc: Document) {
  return [...doc.querySelectorAll("style")].some((style) => cssIsRemote(style.textContent ?? ""));
}

/** Whether an email loads anything from the web, shown or not. */
function loadsRemote(doc: Document) {
  return remoteElements(doc).length > 0 || sheetsLoadRemote(doc);
}

/**
 * Whether CSS squeezes an element to nothing, as hybrid newsletters hide what's for phones:
 * `max-height:0` or `max-width:0` on it, or on an ancestor that clips. Computed limits, not
 * rendered sizes: a blocked picture with `alt=""` and no height is 0×0 until it loads.
 */
function squeezed(el: Element) {
  const win = el.ownerDocument.defaultView;
  for (let node: Element | null = el; node && win; node = node.parentElement) {
    const style = win.getComputedStyle(node);
    const zero = style.maxHeight === "0px" || style.maxWidth === "0px";
    if (zero && (node === el || /hidden|clip/.test(style.overflowX + style.overflowY))) return true;
  }
  return false;
}

/**
 * Whether the laid-out email shows anything from the web. A hidden picture, like a newsletter's
 * banner for phones on a wide screen, doesn't count: Show images wouldn't show it. Nor does a
 * `<picture>`'s web alternative for another screen size: an image counts by the source it picked
 * (`currentSrc`, set once it has; its `src` until then).
 */
function showsRemote(doc: Document) {
  const visible = (el: Element) =>
    el.checkVisibility({ visibilityProperty: true, opacityProperty: true }) && !squeezed(el);
  return (
    sheetsLoadRemote(doc) ||
    [...doc.images].some((img) => isRemoteUrl(img.currentSrc || (img.getAttribute("src") ?? "")) && visible(img)) ||
    remoteElements(doc).some((el) => el.tagName !== "IMG" && el.tagName !== "SOURCE" && visible(el))
  );
}

// `:root:root` outranks the email's own html/body rules, so the document's
// height is its content's and the frame can size to it.
const FRAME_CSS = `
:root:root { height: auto !important; min-height: 0 !important; overflow-y: hidden !important; overflow-x: auto; color-scheme: light; }
:root:root > body { height: auto !important; min-height: 0 !important; }
body { margin: 0; background: #fff; color: #141414; font: 14px/1.5 -apple-system, BlinkMacSystemFont, "Segoe UI", system-ui, Roboto, sans-serif; overflow-wrap: break-word; word-wrap: break-word; }
img { max-width: 100%; height: auto; }
img[src^="cid:" i] { visibility: hidden; }
table { max-width: 100%; }
pre { white-space: pre-wrap; }
a { color: #007cff; }
#spark-quoted[hidden] { display: none !important; }
`;

/**
 * The dark theme's defaults: the card's color and light text. An email being
 * adapted stays hidden until `adaptColors` has run, so it doesn't flash white.
 */
function darkFrameCss(card: Rgb, adapting: boolean) {
  return `
:root:root { color-scheme: dark; ${adapting ? "visibility: hidden;" : ""} }
body { background: ${cssColor(card)}; color: #dee3eb; }
a { color: #439fff; }
`;
}

/** How an email is shown in the dark theme: its own dark design, kept light, or adapted. */
type Scheme = "native" | "light" | "adapt";

/**
 * Spark's test. An email that lists dark in `supported-color-schemes` has a
 * dark design of its own. One built to stay light keeps a white sheet: its
 * styles only apply for one color scheme (`media` on a <style>), it's an
 * Outlook-style template (X-UA-Compatible) that doesn't declare dark, or it's
 * from Amazon or Apple, whose layouts break when recolored. The rest adapt.
 */
function emailScheme(doc: Document): Scheme {
  const meta = (name: string) => doc.querySelector(`meta[name="${name}" i]`)?.getAttribute("content")?.toLowerCase() ?? "";
  if (meta("supported-color-schemes").includes("dark")) return "native";
  const styles = [...doc.querySelectorAll("style")].map((s) => s.textContent ?? "").join("\n");
  const declaresDark = meta("color-scheme").includes("dark") || /(?:supported-)?color-scheme\s*:[^;}]*dark/i.test(styles);
  const light =
    [...doc.querySelectorAll("style[media], link[media]")].some((el) =>
      /prefers-color-scheme/i.test(el.getAttribute("media")!),
    ) ||
    (doc.querySelector('meta[http-equiv="X-UA-Compatible" i]') !== null && !declaresDark) ||
    doc.querySelector('img[src*="ssl-images-amazon"]') !== null ||
    doc.getElementById("aapl-desktop") !== null ||
    doc.getElementById("aapl-mobile") !== null;
  return light ? "light" : "adapt";
}

/**
 * Answers the email's `prefers-color-scheme` queries with the app's theme
 * rather than the system's: Spark renders mail inside its own page, where
 * they follow the app.
 */
function preferColorScheme(doc: Document, dark: boolean) {
  const query = /\(\s*prefers-color-scheme\s*:\s*(dark|light)\s*\)/gi;
  // Always-true and always-false queries.
  const answer = (_: string, scheme: string) => ((scheme.toLowerCase() === "dark") === dark ? "(color)" : "(monochrome)");
  for (const style of doc.querySelectorAll("style")) {
    const text = style.textContent ?? "";
    const answered = text.replace(query, answer);
    if (answered !== text) style.textContent = answered;
    const media = style.getAttribute("media");
    if (media) style.setAttribute("media", media.replace(query, answer));
  }
}

/** Where mail apps' replies start their quote of earlier mail. */
const QUOTE_MARKERS = [
  // Gmail
  ".gmail_quote_container",
  ".gmail_quote",
  // Outlook on the web and new Outlook, then Outlook for Windows' header block
  "#divRplyFwdMsg",
  "#appendonsend",
  "[id^='mail-editor-reference-message-container']",
  "div[style*='border-top:solid #E1E1E1' i]",
  "div[style*='border-top:solid #B5C4DF' i]",
  // Thunderbird and Apple Mail
  ".moz-cite-prefix",
  "blockquote[type='cite' i]",
  // Yahoo, Proton, Zoho
  ".yahoo_quoted",
  ".protonmail_quote",
  ".zmail_extra",
].join(", ");

/**
 * Moves a reply's quote of earlier mail, from its marker to the end, into a
 * hidden `#spark-quoted` at the end of the body. Only when something comes
 * before it, so a reply that's all quote still shows. Returns whether it did.
 */
function foldQuote(doc: Document) {
  const body = doc.body;
  let start = body.querySelector(QUOTE_MARKERS);
  if (!start || !body.lastChild) return false;
  // Outlook rules a line above its header block.
  if (start.previousElementSibling?.tagName === "HR") start = start.previousElementSibling;
  const before = doc.createRange();
  before.setStart(body, 0);
  before.setEndBefore(start);
  if (!before.toString().trim() && !before.cloneContents().querySelector("img")) return false;
  const quote = doc.createRange();
  quote.setStartBefore(start);
  quote.setEndAfter(body.lastChild);
  const folded = doc.createElement("div");
  folded.id = "spark-quoted";
  folded.hidden = true;
  folded.append(quote.extractContents());
  body.append(folded);
  return true;
}

function showQuote(doc: Document, shown: boolean) {
  const folded = doc.getElementById("spark-quoted");
  if (folded) folded.hidden = !shown;
}

/** Runs `then` once the document is fully parsed, so all of its styles apply. */
function whenParsed(doc: Document, then: () => void) {
  if (doc.readyState === "loading") doc.addEventListener("DOMContentLoaded", then, { once: true });
  else then();
}

/** Applies `darkColors`, then shows the document, which `darkFrameCss` hid until now. */
function adaptColors(doc: Document, card: Rgb) {
  try {
    for (const { el, color, background } of darkColors(doc, card)) {
      if (color) el.style.setProperty("color", cssColor(color), "important");
      if (background) el.style.setProperty("background-color", cssColor(background), "important");
    }
  } finally {
    doc.documentElement.style.setProperty("visibility", "visible");
  }
}

/**
 * A picture the viewer can open: loaded (an embedded one that hasn't arrived,
 * or a blocked one from the web, hasn't), shown (not squeezed away like a
 * newsletter's picture for phones), more than a spacer, and not a link, which
 * a click opens instead.
 */
function viewable(img: HTMLImageElement) {
  return (
    img.complete &&
    img.naturalWidth > 2 &&
    img.naturalHeight > 2 &&
    /^(?:data|https?):/i.test(img.currentSrc) &&
    !img.closest("a[href]") &&
    img.checkVisibility({ visibilityProperty: true, opacityProperty: true }) &&
    !squeezed(img)
  );
}

/**
 * Sizes the frame to its document and routes its clicks and keys to the app,
 * and a double-clicked picture to `openImage` with the others it shows.
 * Returns [measure, unwire]. Listeners added from here run even though the
 * frame can't run scripts of its own.
 */
function wireFrame(
  doc: Document,
  setHeight: (height: number) => void,
  openImage: (images: ViewedImage[], start: number) => void,
): [() => void, () => void] {
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
  const onDoubleClick = (e: MouseEvent) => {
    const target = e.target as Element | null;
    if (target?.tagName !== "IMG" || !viewable(target as HTMLImageElement)) return;
    e.preventDefault();
    // The double click selected the picture.
    doc.getSelection()?.removeAllRanges();
    const images = [...doc.images].filter(viewable);
    openImage(
      images.map((img) => ({ src: img.currentSrc, alt: img.alt.trim() })),
      images.indexOf(target as HTMLImageElement),
    );
  };

  // Image load/error events don't bubble, but they do pass the document while capturing.
  doc.addEventListener("load", measure, true);
  doc.addEventListener("error", measure, true);
  doc.addEventListener("click", onClick);
  doc.addEventListener("auxclick", onClick);
  doc.addEventListener("keydown", onKey);
  doc.addEventListener("pointerdown", onPointer);
  doc.addEventListener("dblclick", onDoubleClick);
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
      doc.removeEventListener("dblclick", onDoubleClick);
    },
  ];
}
