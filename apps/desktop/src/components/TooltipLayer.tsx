import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { CARDS, keyCaps, type CardId } from "../lib/tooltip";

/** Before a tooltip appears; feature cards wait a little longer. */
const SHOW_DELAY = 400;
const CARD_DELAY = 600;
/** Right after one hides, the next shows at once (moving along a toolbar). */
const WARM_FOR = 400;
const GAP = 8;
const MARGIN = 8;

interface Shown {
  el: HTMLElement;
  label: string;
  keys?: string;
  detail?: string;
  card?: CardId;
  /** `data-tip-side="right"`: beside the element (the sidebar rail), not below it. */
  right: boolean;
  /** Shown straight after another one: swap instead of popping in. */
  warm: boolean;
}

interface Placed {
  top: number;
  left: number;
  /** Where the tooltip grows from, toward its element. */
  origin: string;
}

const tipTarget = (target: EventTarget | null) =>
  target instanceof Element ? target.closest<HTMLElement>("[data-tip], [data-tip-card]") : null;

/**
 * Tooltips for the whole app. Elements opt in with `tipAttrs` (data-tip and
 * friends) rather than `title`, whose native tooltips are slow and plain. One
 * listener at the document serves every element, so buttons need no wrappers.
 */
export default function TooltipLayer() {
  const [shown, setShown] = useState<Shown | null>(null);
  const [placed, setPlaced] = useState<Placed | null>(null);
  const box = useRef<HTMLDivElement>(null);

  useEffect(() => {
    let timer = 0;
    let current: HTMLElement | null = null;
    let visible = false;
    let hiddenAt = 0;
    // Pressed: no tooltip on it again until the pointer leaves it.
    let pressed: HTMLElement | null = null;

    function read(el: HTMLElement, warm: boolean): Shown | null {
      const label = el.dataset.tip ?? "";
      const card = el.dataset.tipCard as CardId | undefined;
      if (!label && !card) return null;
      return {
        el,
        label,
        keys: el.dataset.tipKeys || undefined,
        detail: el.dataset.tipDetail || undefined,
        card: card && card in CARDS ? card : undefined,
        right: el.dataset.tipSide === "right",
        warm,
      };
    }

    function hide() {
      window.clearTimeout(timer);
      if (visible) hiddenAt = Date.now();
      visible = false;
      current = null;
      setShown(null);
      setPlaced(null);
    }

    function aim(el: HTMLElement) {
      window.clearTimeout(timer);
      const warm = visible || Date.now() - hiddenAt < WARM_FOR;
      if (visible) hiddenAt = Date.now();
      visible = false;
      current = el;
      setShown(null);
      setPlaced(null);
      const delay = warm ? 0 : el.dataset.tipCard ? CARD_DELAY : SHOW_DELAY;
      timer = window.setTimeout(() => {
        if (current !== el || !el.isConnected) return;
        const next = read(el, warm);
        visible = next !== null;
        setShown(next);
      }, delay);
    }

    const onOver = (e: PointerEvent) => {
      if (e.pointerType === "touch") return;
      const el = tipTarget(e.target);
      if (el === current) return;
      if (pressed && el !== pressed) pressed = null;
      if (!el || el === pressed) {
        if (current) hide();
        return;
      }
      aim(el);
    };
    const onOut = (e: PointerEvent) => {
      // Left the window.
      if (e.relatedTarget === null && current) hide();
    };
    const onPress = () => {
      pressed = current;
      if (current) hide();
    };
    const onFocus = (e: FocusEvent) => {
      const el = tipTarget(e.target);
      if (el && el === e.target && el.matches(":focus-visible")) aim(el);
    };
    const onBlur = () => {
      if (current) hide();
    };

    document.addEventListener("pointerover", onOver);
    document.addEventListener("pointerout", onOut);
    document.addEventListener("pointerdown", onPress, true);
    document.addEventListener("keydown", onPress, true);
    document.addEventListener("wheel", onPress, { capture: true, passive: true });
    document.addEventListener("scroll", onBlur, true);
    document.addEventListener("focusin", onFocus);
    document.addEventListener("focusout", onBlur);
    window.addEventListener("blur", onBlur);
    // Elements can vanish under a tooltip (a closed composer, a re-render).
    const check = window.setInterval(() => {
      if (current && !current.isConnected) hide();
    }, 300);
    return () => {
      window.clearTimeout(timer);
      window.clearInterval(check);
      document.removeEventListener("pointerover", onOver);
      document.removeEventListener("pointerout", onOut);
      document.removeEventListener("pointerdown", onPress, true);
      document.removeEventListener("keydown", onPress, true);
      document.removeEventListener("wheel", onPress, true);
      document.removeEventListener("scroll", onBlur, true);
      document.removeEventListener("focusin", onFocus);
      document.removeEventListener("focusout", onBlur);
      window.removeEventListener("blur", onBlur);
    };
  }, []);

  // Below the element if it fits, else above (or to its right); kept inside the window.
  useLayoutEffect(() => {
    if (!shown || !box.current) return;
    const r = shown.el.getBoundingClientRect();
    const { width: w, height: h } = box.current.getBoundingClientRect();
    if (shown.right) {
      const top = Math.min(Math.max(r.top + r.height / 2 - h / 2, MARGIN), window.innerHeight - MARGIN - h);
      setPlaced({ top, left: r.right + GAP, origin: "left center" });
      return;
    }
    let top = r.bottom + GAP;
    let vertical = "top";
    if (top + h > window.innerHeight - MARGIN && r.top - GAP - h >= MARGIN) {
      top = r.top - GAP - h;
      vertical = "bottom";
    }
    const center = r.left + r.width / 2;
    const left = Math.min(Math.max(center - w / 2, MARGIN), window.innerWidth - MARGIN - w);
    setPlaced({ top, left, origin: `${Math.round(center - left)}px ${vertical}` });
  }, [shown]);

  if (!shown) return null;
  const card = shown.card ? CARDS[shown.card] : undefined;
  return createPortal(
    <div
      ref={box}
      role="tooltip"
      style={{
        top: placed?.top ?? 0,
        left: placed?.left ?? 0,
        transformOrigin: placed?.origin,
        visibility: placed ? "visible" : "hidden",
      }}
      className={`pointer-events-none fixed z-[3000] ${placed ? (shown.warm ? "tip-swap" : "tip-pop") : ""}`}
    >
      {card ? (
        <div className="w-[300px] rounded-2xl border border-hairline bg-popover p-4 text-base-content shadow-popup">
          <div className="text-[17px] font-bold leading-6">{card.question}</div>
          <div className="flex items-center gap-2 text-[17px] font-bold leading-6 text-primary">
            {card.action}
            <span className={`grid size-6 place-items-center rounded-md bg-base-content/5 ring-1 ring-hairline ${card.tint}`}>
              <card.Icon size={14} strokeWidth={2.25} />
            </span>
          </div>
          <p className="mt-1.5 text-[13px] leading-5 text-text-2">{card.body}</p>
          {shown.keys && (
            <div className="mt-3 flex items-center gap-2 border-t border-hairline pt-2.5 text-xs text-text-2">
              {shown.label}
              <span className="ml-auto flex gap-1">
                {keyCaps(shown.keys).map((k, i) => (
                  <kbd key={i} className="tip-card-key">
                    {k}
                  </kbd>
                ))}
              </span>
            </div>
          )}
        </div>
      ) : (
        <div className="tip-simple max-w-[320px] rounded-lg px-2.5 py-1.5 text-xs font-medium leading-[18px] shadow-lg">
          <div className="flex items-center gap-2">
            <span className="min-w-0 break-words">{shown.label}</span>
            {shown.keys && (
              <span className="flex shrink-0 gap-0.5">
                {keyCaps(shown.keys).map((k, i) => (
                  <kbd key={i} className="tip-key">
                    {k}
                  </kbd>
                ))}
              </span>
            )}
          </div>
          {shown.detail && <div className="mt-0.5 break-words text-[11px] font-normal opacity-70">{shown.detail}</div>}
        </div>
      )}
    </div>,
    document.body,
  );
}
