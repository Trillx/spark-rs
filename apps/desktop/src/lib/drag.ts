import { useEffect, useRef, useSyncExternalStore, type PointerEvent as ReactPointerEvent } from "react";
import type { AccountId, Message } from "../types";
import type { ListKey } from "./sidebar";

// Dragging with pointer events. The desktop shell handles file drops itself,
// and on Windows that swallows the page's HTML5 drag events, so `draggable`
// can't be used: a press turns into a drag once the pointer has moved a few
// pixels, and one that doesn't move stays a click.

/** How far the pointer moves, in px, before a press becomes a drag. */
const THRESHOLD = 5;

export interface DragHandlers {
  /** Text of the pill that follows the pointer. */
  ghost: string;
  /** The press became a drag. */
  start?: () => void;
  move: (x: number, y: number) => void;
  /** Released (`dropped`), or cancelled with Esc. */
  end: (dropped: boolean) => void;
}

/** Call from `onPointerDown`. Only the primary button drags, so right-click menus are left alone. */
export function trackDrag(e: ReactPointerEvent<HTMLElement>, handlers: DragHandlers) {
  if (e.button !== 0 || e.pointerType === "touch") return;
  const source = e.currentTarget;
  const pointer = e.pointerId;
  const startX = e.clientX;
  const startY = e.clientY;
  let ghost: HTMLElement | null = null;
  let ended = false;

  function finish(dropped: boolean) {
    if (!ghost || ended) return;
    ended = true;
    ghost.remove();
    handlers.end(dropped);
  }

  function move(ev: PointerEvent) {
    if (ev.pointerId !== pointer || ended) return;
    // Released where we didn't hear it, e.g. outside the window before the drag began.
    if (!(ev.buttons & 1)) {
      finish(false);
      stop();
      return;
    }
    if (!ghost) {
      if (Math.hypot(ev.clientX - startX, ev.clientY - startY) < THRESHOLD) return;
      ghost = makeGhost(handlers.ghost);
      // Keeps the pointer's events (and hover styles and tooltips) on the source while dragging.
      try {
        source.setPointerCapture(pointer);
      } catch {
        // Already released.
      }
      handlers.start?.();
    }
    ghost.style.transform = `translate(${ev.clientX + 12}px, ${ev.clientY + 8}px)`;
    handlers.move(ev.clientX, ev.clientY);
  }

  function up(ev: PointerEvent) {
    if (ev.pointerId !== pointer) return;
    // The click that follows a drag (or a drag cancelled with Esc) isn't one.
    if (ghost) swallowClick();
    finish(true);
    stop();
  }

  function cancel(ev: PointerEvent) {
    if (ev.pointerId !== pointer) return;
    finish(false);
    stop();
  }

  function onKey(ev: KeyboardEvent) {
    if (ev.key !== "Escape" || !ghost || ended) return;
    ev.preventDefault();
    ev.stopPropagation();
    finish(false);
  }

  function onBlur() {
    finish(false);
    stop();
  }

  function stop() {
    window.removeEventListener("pointermove", move);
    window.removeEventListener("pointerup", up);
    window.removeEventListener("pointercancel", cancel);
    window.removeEventListener("keydown", onKey, true);
    window.removeEventListener("blur", onBlur);
  }

  window.addEventListener("pointermove", move);
  window.addEventListener("pointerup", up);
  window.addEventListener("pointercancel", cancel);
  // Capture phase, so Esc stops the drag and nothing else.
  window.addEventListener("keydown", onKey, true);
  window.addEventListener("blur", onBlur);
}

function makeGhost(text: string) {
  const el = document.createElement("div");
  el.className =
    "pointer-events-none fixed left-0 top-0 z-[4000] max-w-60 truncate rounded-full bg-primary px-3 py-1 text-[13px] font-medium text-primary-content shadow-popup";
  el.textContent = text;
  document.body.append(el);
  return el;
}

function swallowClick() {
  const swallow = (e: MouseEvent) => {
    e.stopPropagation();
    e.preventDefault();
  };
  window.addEventListener("click", swallow, { capture: true, once: true });
  window.setTimeout(() => window.removeEventListener("click", swallow, true));
}

// ---- messages dragged onto the sidebar ----

/** A sidebar row a message can be dropped on; `account` for an account row. */
export interface DropTarget {
  key: ListKey;
  account?: AccountId;
}

export interface MessageDrag {
  message: Message;
  /** The row under the pointer, if it takes the message. */
  over: DropTarget | null;
}

let messageDrag: MessageDrag | null = null;
const listeners = new Set<() => void>();
let dropHandler: ((m: Message, target: DropTarget) => void) | undefined;

function setMessageDrag(next: MessageDrag | null) {
  messageDrag = next;
  for (const listener of listeners) listener();
}

function subscribe(listener: () => void) {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

/** The message being dragged, or null. */
export function useMessageDrag() {
  return useSyncExternalStore(subscribe, () => messageDrag);
}

/** Starts dragging a list row's message; rows that take it carry `dropAttrs`. */
export function dragMessage(e: ReactPointerEvent<HTMLElement>, message: Message) {
  let at = { x: e.clientX, y: e.clientY };
  trackDrag(e, {
    ghost: message.subject || "No subject",
    start: () => setMessageDrag({ message, over: null }),
    move: (x, y) => {
      at = { x, y };
      const over = dropTargetAt(x, y);
      const was = messageDrag?.over;
      if (over?.key !== was?.key || over?.account !== was?.account) setMessageDrag({ message, over });
    },
    end: (dropped) => {
      // Looked up again: rows only mark themselves as targets once the drag has rendered.
      const over = dropped ? dropTargetAt(at.x, at.y) : null;
      setMessageDrag(null);
      if (over) dropHandler?.(message, over);
    },
  });
}

/** Marks an element as a place to drop the dragged message. */
export function dropAttrs(target: DropTarget | undefined) {
  if (!target) return {};
  return { "data-drop": target.key, "data-drop-account": target.account };
}

function dropTargetAt(x: number, y: number): DropTarget | null {
  const el = document.elementFromPoint(x, y)?.closest<HTMLElement>("[data-drop]");
  if (!el) return null;
  const account = el.dataset.dropAccount;
  return { key: el.dataset.drop as ListKey, account: account ? Number(account) : undefined };
}

/** What happens to a dropped message; the sidebar sets it. */
export function useMessageDrop(handler: (m: Message, target: DropTarget) => void) {
  const latest = useRef(handler);
  latest.current = handler;
  useEffect(() => {
    const handle = (m: Message, target: DropTarget) => latest.current(m, target);
    dropHandler = handle;
    return () => {
      if (dropHandler === handle) dropHandler = undefined;
    };
  }, []);
}
