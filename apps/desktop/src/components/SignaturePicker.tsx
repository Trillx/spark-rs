import { Check, Settings } from "lucide-react";
import { useEffect, useLayoutEffect, useRef, useState, type CSSProperties, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { listSignatures } from "../api";
import type { Signature } from "../types";
import SignatureFrame from "./SignatureFrame";

const WIDTH = 320;
const MARGIN = 8;

/**
 * Spark's signature picker: each signature as it looks, then Hide Signature
 * and a way to Settings. It floats over the page (the composer would clip
 * it) beside or above its trigger.
 */
export default function SignaturePicker({
  trigger,
  direction = "down",
  currentId,
  onPick,
  onHide,
  onSettings,
}: {
  trigger: (props: { open: boolean; toggle: () => void }) => ReactNode;
  direction?: "down" | "up";
  currentId: number | null;
  onPick: (signature: Signature) => void;
  /** While a signature shows. */
  onHide?: () => void;
  onSettings: () => void;
}) {
  const [open, setOpen] = useState(false);
  const [signatures, setSignatures] = useState<Signature[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [style, setStyle] = useState<CSSProperties>({});
  const anchorRef = useRef<HTMLSpanElement>(null);
  const panelRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    setError(null);
    listSignatures().then(
      (view) => setSignatures(view.signatures),
      (e) => setError(String(e)),
    );
    const onPointer = (e: PointerEvent) => {
      const target = e.target as Node;
      if (!anchorRef.current?.contains(target) && !panelRef.current?.contains(target)) setOpen(false);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      e.stopPropagation();
      setOpen(false);
    };
    window.addEventListener("pointerdown", onPointer);
    window.addEventListener("keydown", onKey, true);
    return () => {
      window.removeEventListener("pointerdown", onPointer);
      window.removeEventListener("keydown", onKey, true);
    };
  }, [open]);

  useLayoutEffect(() => {
    // The anchor is `display: contents`, with no box of its own: measure the trigger.
    const trigger = anchorRef.current?.firstElementChild;
    if (!open || !trigger) return;
    const r = trigger.getBoundingClientRect();
    const left = Math.max(MARGIN, Math.min(r.left, window.innerWidth - WIDTH - MARGIN));
    setStyle(
      direction === "up"
        ? { left, bottom: window.innerHeight - r.top + 4, maxHeight: r.top - 4 - MARGIN }
        : { left, top: r.bottom + 4, maxHeight: window.innerHeight - r.bottom - 4 - MARGIN },
    );
  }, [open, direction]);

  const close = (then: () => void) => {
    setOpen(false);
    then();
  };

  return (
    <>
      <span ref={anchorRef} className="contents">
        {trigger({ open, toggle: () => setOpen((o) => !o) })}
      </span>
      {open &&
        createPortal(
          <div
            ref={panelRef}
            role="dialog"
            aria-label="Choose signature"
            style={{ ...style, width: WIDTH }}
            className="fixed z-[2020] flex flex-col overflow-hidden rounded-2xl border border-hairline bg-popover text-[13px] text-base-content shadow-popup"
          >
            <div className="min-h-0 flex-1 space-y-1 overflow-y-auto p-1.5" role="listbox" aria-label="Signatures">
              {error ? (
                <p className="px-2 py-3 text-error">{error}</p>
              ) : signatures === null ? (
                <p className="px-2 py-3 text-text-2">Loading…</p>
              ) : signatures.length === 0 ? (
                <div className="px-2 py-3 text-text-2">
                  <p>No signatures yet.</p>
                  <button className="btn btn-primary btn-sm mt-2" onClick={() => close(onSettings)}>
                    Create signature
                  </button>
                </div>
              ) : (
                signatures.map((s) => (
                  <button
                    key={s.id}
                    role="option"
                    aria-selected={s.id === currentId}
                    onClick={() => close(() => onPick(s))}
                    className={`relative block w-full rounded-xl border p-2 pr-7 text-left hover:border-primary/60 ${
                      s.id === currentId ? "border-primary/60 bg-primary/5" : "border-hairline"
                    }`}
                  >
                    {/* A preview: clicks go to the button. */}
                    <div className="pointer-events-none max-h-28 overflow-hidden">
                      <SignatureFrame html={s.html} title={`Signature ${s.id}`} />
                    </div>
                    {s.id === currentId && <Check size={14} className="absolute right-2 top-2 text-primary" />}
                  </button>
                ))
              )}
            </div>
            <div className="flex shrink-0 items-center gap-1 border-t border-hairline px-2 py-1.5">
              {onHide && (
                <button className="btn btn-ghost btn-xs h-7 px-2 font-medium" onClick={() => close(onHide)}>
                  Hide Signature
                </button>
              )}
              <button
                className="btn btn-ghost btn-xs btn-square ml-auto h-7 w-7 text-text-2"
                aria-label="Signature settings"
                data-tip="Signature settings"
                onClick={() => close(onSettings)}
              >
                <Settings size={15} />
              </button>
            </div>
          </div>,
          document.body,
        )}
    </>
  );
}
