import { useEffect, useState, type PointerEvent as ReactPointerEvent } from "react";

// Pane geometry: sidebar, message list and reader widths.
export const SIDEBAR_RAIL = 72;
export const SIDEBAR_DEFAULT = 240;
export const SIDEBAR_MIN = 150;
export const SIDEBAR_MAX = 335;
export const LIST_MIN = 362;
export const LIST_MAX = 464;
export const READER_MIN = 590;
/** Two columns drop to one below this reader width (10px hysteresis against READER_MIN). */
export const READER_COLLAPSE = 580;

export function clamp(value: number, min: number, max: number) {
  return Math.max(min, Math.min(max, value));
}

export function useWindowWidth() {
  const [width, setWidth] = useState(window.innerWidth);
  useEffect(() => {
    const onResize = () => setWidth(window.innerWidth);
    window.addEventListener("resize", onResize);
    return () => window.removeEventListener("resize", onResize);
  }, []);
  return width;
}

/** The list width actually used: never more than the container minus the reader minimum. */
export function effectiveListWidth(preferred: number, container: number) {
  return clamp(Math.min(preferred, container - READER_MIN), LIST_MIN, LIST_MAX);
}

/** Whether list and reader fit side by side, with hysteresis so resizing doesn't flicker. */
export function useTwoColumns(container: number, listWidth: number) {
  const [two, setTwo] = useState(() => container - listWidth >= READER_MIN);
  const reader = container - listWidth;
  const next = two ? reader >= READER_COLLAPSE : reader >= READER_MIN;
  if (next !== two) setTwo(next);
  return next;
}

/** Tracks a horizontal pointer drag, reporting the offset from where it started. */
export function startHorizontalDrag(e: ReactPointerEvent, onMove: (dx: number) => void) {
  e.preventDefault();
  const startX = e.clientX;
  const move = (ev: PointerEvent) => onMove(ev.clientX - startX);
  const stop = () => {
    window.removeEventListener("pointermove", move);
    window.removeEventListener("pointerup", stop);
    document.body.style.removeProperty("cursor");
  };
  window.addEventListener("pointermove", move);
  window.addEventListener("pointerup", stop);
  document.body.style.cursor = "col-resize";
}
