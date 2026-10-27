import { ChevronLeft, ChevronRight, X, ZoomIn, ZoomOut } from "lucide-react";
import { useEffect, useLayoutEffect, useRef, useState, type MouseEvent as ReactMouseEvent } from "react";
import { createPortal } from "react-dom";
import { tipAttrs } from "../lib/tooltip";

/** A picture from an email: its loaded URL (data: or the web) and its alt text. */
export interface ViewedImage {
  src: string;
  alt: string;
}

/**
 * Pictures from an email over the whole window, opened by double-clicking one.
 * A picture bigger than the window fits it; clicking it (or Z) shows it at its
 * own size, centred on the spot clicked, to scroll around. ← and → step
 * through the email's other pictures. Esc, × or a click beside the picture
 * closes it. Focus stays in the viewer while it's open, so the app's
 * shortcuts don't act on the email behind.
 */
export function ImageViewer({ images, start, onClose }: { images: ViewedImage[]; start: number; onClose: () => void }) {
  const [index, setIndex] = useState(start);
  /** At its own size rather than fitted to the window. */
  const [actual, setActual] = useState(false);
  /** Fitted, it shows smaller than it is, so zooming in shows more. */
  const [shrunk, setShrunk] = useState(false);
  const rootRef = useRef<HTMLDivElement>(null);
  const boxRef = useRef<HTMLDivElement>(null);
  const imgRef = useRef<HTMLImageElement>(null);
  /** Where the click that zoomed in was: as a fraction of the picture, and on screen. */
  const anchor = useRef<{ fx: number; fy: number; x: number; y: number } | null>(null);
  const image = images[index];
  const zoomable = actual || shrunk;

  const step = (by: number) => {
    setIndex((i) => (i + by + images.length) % images.length);
    setActual(false);
    setShrunk(false);
  };
  const zoom = (at?: { x: number; y: number }) => {
    if (!zoomable) return;
    const img = imgRef.current;
    if (!actual && img) {
      const r = img.getBoundingClientRect();
      const x = at?.x ?? r.left + r.width / 2;
      const y = at?.y ?? r.top + r.height / 2;
      anchor.current = { fx: (x - r.left) / r.width, fy: (y - r.top) / r.height, x, y };
    }
    setActual(!actual);
  };
  const measure = () => {
    const img = imgRef.current;
    if (!img || actual || !img.naturalWidth) return;
    setShrunk(img.naturalWidth > img.clientWidth + 1 || img.naturalHeight > img.clientHeight + 1);
  };

  /** Tab goes round the buttons and never to the app behind. */
  const cycleFocus = (by: number) => {
    const buttons = [...rootRef.current!.querySelectorAll("button")];
    const i = buttons.indexOf(document.activeElement as HTMLButtonElement);
    buttons[i < 0 ? (by > 0 ? 0 : buttons.length - 1) : (i + by + buttons.length) % buttons.length]?.focus();
  };

  // Takes focus when it opens, and back from a button that goes away (zoom, for a picture that fits).
  useEffect(() => {
    const root = rootRef.current;
    if (root && !root.contains(document.activeElement)) root.focus();
  });

  useEffect(() => {
    // Capture phase, so these keys act here and not on the email behind.
    const onKey = (e: KeyboardEvent) => {
      if (e.ctrlKey || e.metaKey || e.altKey) return;
      if (e.key === "Escape") onClose();
      else if (e.key === "ArrowLeft" && images.length > 1) step(-1);
      else if (e.key === "ArrowRight" && images.length > 1) step(1);
      else if (e.key.toLowerCase() === "z") zoom();
      else if (e.key === "Tab") cycleFocus(e.shiftKey ? -1 : 1);
      else return;
      e.preventDefault();
      e.stopPropagation();
    };
    window.addEventListener("keydown", onKey, true);
    window.addEventListener("resize", measure);
    return () => {
      window.removeEventListener("keydown", onKey, true);
      window.removeEventListener("resize", measure);
    };
  });

  // Zoomed in, the spot clicked stays under the pointer.
  useLayoutEffect(() => {
    const at = anchor.current;
    const box = boxRef.current;
    const img = imgRef.current;
    anchor.current = null;
    if (!actual || !at || !box || !img) return;
    const r = img.getBoundingClientRect();
    box.scrollLeft += r.left + at.fx * r.width - at.x;
    box.scrollTop += r.top + at.fy * r.height - at.y;
  }, [actual]);

  const closeBeside = (e: ReactMouseEvent) => {
    if (e.target === e.currentTarget) onClose();
  };
  const button = "grid size-9 place-items-center rounded-full bg-black/50 text-white hover:bg-black/70";

  return createPortal(
    <div
      ref={rootRef}
      role="dialog"
      aria-modal
      aria-label={image.alt || "Image"}
      tabIndex={-1}
      className="fixed inset-0 z-[2950] bg-black/85 outline-none animate-[appear_120ms_ease-out]"
    >
      <div ref={boxRef} className="absolute inset-0 flex overflow-auto p-14" onClick={closeBeside}>
        <img
          ref={imgRef}
          key={index}
          src={image.src}
          alt={image.alt}
          referrerPolicy="no-referrer"
          draggable={false}
          className={`m-auto select-none bg-white shadow-popup ${actual ? "max-w-none" : "max-h-full max-w-full"} ${
            zoomable ? (actual ? "cursor-zoom-out" : "cursor-zoom-in") : ""
          }`}
          onLoad={measure}
          onClick={(e) => zoom({ x: e.clientX, y: e.clientY })}
        />
      </div>
      <div className="absolute right-3 top-3 flex items-center gap-2">
        {images.length > 1 && (
          <span className="mr-1 rounded-full bg-black/50 px-2.5 py-1 text-[13px] tabular-nums text-white/80">
            {index + 1} of {images.length}
          </span>
        )}
        {zoomable && (
          <button
            className={button}
            {...tipAttrs({ label: actual ? "Fit to window" : "Actual size", keys: "Z" })}
            aria-label={actual ? "Fit to window" : "Actual size"}
            onClick={() => zoom()}
          >
            {actual ? <ZoomOut size={18} /> : <ZoomIn size={18} />}
          </button>
        )}
        <button className={button} {...tipAttrs({ label: "Close", keys: "Esc" })} aria-label="Close" onClick={onClose}>
          <X size={18} />
        </button>
      </div>
      {images.length > 1 && (
        <>
          <button
            className={`${button} absolute left-3 top-1/2 -translate-y-1/2`}
            {...tipAttrs({ label: "Previous image", keys: "←" })}
            aria-label="Previous image"
            onClick={() => step(-1)}
          >
            <ChevronLeft size={20} />
          </button>
          <button
            className={`${button} absolute right-3 top-1/2 -translate-y-1/2`}
            {...tipAttrs({ label: "Next image", keys: "→" })}
            aria-label="Next image"
            onClick={() => step(1)}
          >
            <ChevronRight size={20} />
          </button>
        </>
      )}
    </div>,
    document.body,
  );
}
