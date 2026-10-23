import { useCallback, useEffect, useRef, type HTMLAttributes, type ReactNode, type Ref } from "react";

// A scroll area whose scrollbar floats over the content, like Spark's: a thin
// thumb that shows while the pointer is over the area or it scrolls, and fades
// a second after. The native scrollbar is hidden, so rows keep their width.

/** How long the thumb stays once the pointer leaves and scrolling stops. */
const HIDE_DELAY = 1000;
/** Space around the thumb inside its 10px track. */
const INSET = 2;
const MIN_THUMB = 24;

interface Props extends HTMLAttributes<HTMLDivElement> {
  /** The element that scrolls, for callers that read or set its position. */
  viewportRef?: Ref<HTMLDivElement>;
  /** The area's own layout, e.g. `min-h-0 flex-1`. */
  className?: string;
  /** Classes of the element that scrolls, e.g. its padding. */
  viewportClassName?: string;
  /** Where the track sits, `right-0` unless something else claims the edge. */
  barClassName?: string;
  children: ReactNode;
}

/** Other props (role, aria-*, onScroll…) go to the element that scrolls. */
export default function OverlayScroll({
  viewportRef,
  className = "",
  viewportClassName = "",
  barClassName = "right-0",
  children,
  ...rest
}: Props) {
  const area = useRef<HTMLDivElement>(null);
  const viewport = useRef<HTMLDivElement | null>(null);
  const content = useRef<HTMLDivElement>(null);
  const track = useRef<HTMLDivElement>(null);
  const thumb = useRef<HTMLDivElement>(null);

  const setViewport = useCallback(
    (el: HTMLDivElement | null) => {
      viewport.current = el;
      if (typeof viewportRef === "function") viewportRef(el);
      else if (viewportRef) viewportRef.current = el;
    },
    [viewportRef],
  );

  useEffect(() => {
    const box = area.current!;
    const el = viewport.current!;
    const bar = track.current!;
    const knob = thumb.current!;
    let hovering = false;
    let dragging = false;
    let hideTimer: number | undefined;
    let dragStart = { y: 0, top: 0 };

    const geometry = () => {
      const { scrollTop, scrollHeight, clientHeight } = el;
      const range = scrollHeight - clientHeight;
      const room = clientHeight - INSET * 2;
      const size = Math.min(room, Math.max(MIN_THUMB, (room * clientHeight) / scrollHeight));
      const travel = room - size;
      return { range, size, travel, top: range > 0 ? INSET + (scrollTop / range) * travel : INSET };
    };
    const place = () => {
      const g = geometry();
      bar.hidden = g.range <= 1;
      knob.style.height = `${g.size}px`;
      knob.style.transform = `translateY(${g.top}px)`;
    };
    const show = () => {
      window.clearTimeout(hideTimer);
      bar.dataset.visible = "";
    };
    const hideLater = () => {
      window.clearTimeout(hideTimer);
      if (hovering || dragging) return;
      hideTimer = window.setTimeout(() => delete bar.dataset.visible, HIDE_DELAY);
    };

    const onScroll = () => {
      place();
      show();
      hideLater();
    };
    const onEnter = () => {
      hovering = true;
      show();
    };
    const onLeave = () => {
      hovering = false;
      hideLater();
    };
    // The track sits beside the scrolling element, so wheel over it is passed on.
    const onWheel = (e: WheelEvent) => {
      el.scrollTop += e.deltaY;
    };
    const onDown = (e: PointerEvent) => {
      if (e.button !== 0) return;
      e.preventDefault();
      const g = geometry();
      if (e.target !== knob) {
        // A click on the track brings the thumb's middle there.
        const y = e.clientY - bar.getBoundingClientRect().top - INSET - g.size / 2;
        el.scrollTop = g.travel > 0 ? (y / g.travel) * g.range : 0;
      }
      dragging = true;
      dragStart = { y: e.clientY, top: el.scrollTop };
      bar.dataset.dragging = "";
      bar.setPointerCapture(e.pointerId);
    };
    const onMove = (e: PointerEvent) => {
      if (!dragging) return;
      const g = geometry();
      if (g.travel > 0) el.scrollTop = dragStart.top + ((e.clientY - dragStart.y) * g.range) / g.travel;
    };
    const onUp = () => {
      if (!dragging) return;
      dragging = false;
      delete bar.dataset.dragging;
      hideLater();
    };

    const resized = new ResizeObserver(place);
    resized.observe(el);
    resized.observe(content.current!);
    el.addEventListener("scroll", onScroll, { passive: true });
    box.addEventListener("pointerenter", onEnter);
    box.addEventListener("pointerleave", onLeave);
    bar.addEventListener("wheel", onWheel, { passive: true });
    bar.addEventListener("pointerdown", onDown);
    bar.addEventListener("pointermove", onMove);
    bar.addEventListener("pointerup", onUp);
    bar.addEventListener("lostpointercapture", onUp);
    place();
    return () => {
      window.clearTimeout(hideTimer);
      resized.disconnect();
      el.removeEventListener("scroll", onScroll);
      box.removeEventListener("pointerenter", onEnter);
      box.removeEventListener("pointerleave", onLeave);
      bar.removeEventListener("wheel", onWheel);
      bar.removeEventListener("pointerdown", onDown);
      bar.removeEventListener("pointermove", onMove);
      bar.removeEventListener("pointerup", onUp);
      bar.removeEventListener("lostpointercapture", onUp);
    };
  }, []);

  return (
    <div ref={area} className={`relative ${className}`}>
      <div
        ref={setViewport}
        {...rest}
        className={`absolute inset-0 overflow-y-auto [scrollbar-width:none] ${viewportClassName}`}
      >
        <div ref={content}>{children}</div>
      </div>
      <div
        ref={track}
        aria-hidden
        className={`group/bar pointer-events-none absolute inset-y-0 z-10 w-2.5 opacity-0 transition-opacity duration-200 data-[dragging]:pointer-events-auto data-[visible]:pointer-events-auto data-[dragging]:opacity-100 data-[visible]:opacity-100 ${barClassName}`}
      >
        <div
          ref={thumb}
          className="absolute right-[2px] top-0 w-1.5 rounded-full bg-[#8a8a8a80] transition-colors hover:bg-[#8a8a8a] group-data-[dragging]/bar:bg-[#8a8a8a]"
        />
      </div>
    </div>
  );
}
