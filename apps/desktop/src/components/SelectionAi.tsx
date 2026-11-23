import { ArrowUp, MessageCircleQuestion, Reply, Sparkles } from "lucide-react";
import { useEffect, useLayoutEffect, useRef, useState, type ReactNode, type RefObject } from "react";
import { createPortal } from "react-dom";
import { askQuestion, explainQuestion, replyInstructions } from "../lib/selectionAi";
import { SELECT_END, SELECTING_KEYS } from "./MessageBody";

// Selecting text in an email offers AI for it: a small bar over the selection
// that explains it or answers questions about it in the AI Assistant, or opens
// a reply that AI writes about it. Spark's composer has a menu like it for a
// draft's selection; for mail being read, this is our own.

/** Text picked in an email. */
interface Picked {
  /** The email it's in. */
  message: number;
  /** Its text, with runs of white space made one. */
  text: string;
  /** Where it is in the window now, or null once it's gone from the page. */
  place: () => DOMRect | null;
}

/** The selection in an email of `container`, if there is one: in an email's frame (`target`) or a text body. */
function readSelection(container: HTMLElement, target: EventTarget | null): Picked | null {
  if (target instanceof HTMLIFrameElement) {
    if (!container.contains(target)) return null;
    const sel = target.contentDocument?.getSelection();
    const range = sel && !sel.isCollapsed && sel.rangeCount ? sel.getRangeAt(0).cloneRange() : null;
    const card = target.closest<HTMLElement>("[data-message]");
    if (!sel || !range || !card) return null;
    // The range's box is in the frame's own viewport.
    const place = () => {
      if (!target.isConnected) return null;
      const r = range.getBoundingClientRect();
      const f = target.getBoundingClientRect();
      return new DOMRect(f.left + r.left, f.top + r.top, r.width, r.height);
    };
    return picked(card, sel.toString(), place);
  }
  const sel = window.getSelection();
  if (!sel || sel.isCollapsed || !sel.rangeCount) return null;
  const bodyOf = (node: Node | null) =>
    (node instanceof Element ? node : node?.parentElement)?.closest<HTMLElement>("[data-body]") ?? null;
  const body = bodyOf(sel.anchorNode);
  if (!body || body !== bodyOf(sel.focusNode) || !container.contains(body)) return null;
  const card = body.closest<HTMLElement>("[data-message]");
  const range = sel.getRangeAt(0).cloneRange();
  return card && picked(card, sel.toString(), () => (body.isConnected ? range.getBoundingClientRect() : null));
}

function picked(card: HTMLElement, raw: string, place: () => DOMRect | null): Picked | null {
  const text = raw.replace(/\s+/g, " ").trim();
  const message = Number(card.dataset.message);
  return text.length >= 2 && Number.isFinite(message) ? { message, text, place } : null;
}

/**
 * The AI bar over text selected in the reader's emails. `onAsk` is absent where there's no AI
 * Assistant (a conversation's own window), and the bar then only offers the reply.
 */
export default function SelectionAi({
  container,
  onAsk,
  onReply,
}: {
  /** The reader's scrolling element, which holds the emails. */
  container: RefObject<HTMLElement | null>;
  onAsk?: (message: number, question: string) => void;
  onReply: (message: number, instructions: string) => void;
}) {
  const [pick, setPick] = useState<Picked | null>(null);
  const [asking, setAsking] = useState(false);
  const [question, setQuestion] = useState("");
  const [pos, setPos] = useState<{ left: number; top: number } | null>(null);
  /** Bumped when the page scrolls or resizes, so the bar follows the selection. */
  const [moved, setMoved] = useState(0);
  const bar = useRef<HTMLDivElement>(null);

  const close = () => {
    setPick(null);
    setAsking(false);
    setQuestion("");
  };

  useEffect(() => {
    const within = (target: EventTarget | null) => target instanceof Node && !!bar.current?.contains(target);
    let timer = 0;
    // After the event, so a click that ends a selection has collapsed it by then.
    const check = (target: EventTarget | null) => {
      window.clearTimeout(timer);
      timer = window.setTimeout(() => {
        const root = container.current;
        const next = root ? readSelection(root, target) : null;
        if (next) {
          setPick(next);
          setAsking(false);
          setQuestion("");
        } else if (!bar.current?.contains(document.activeElement)) close();
      });
    };
    const onEnd = (e: Event) => {
      if (within(e.target)) return;
      if (e instanceof KeyboardEvent && !SELECTING_KEYS.test(e.key)) return;
      check(e.target);
    };
    const onDown = (e: PointerEvent) => {
      if (!within(e.target)) close();
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape" || !bar.current) return;
      e.preventDefault();
      e.stopPropagation();
      close();
    };
    const onMove = () => setMoved((n) => n + 1);
    document.addEventListener("pointerup", onEnd);
    document.addEventListener("keyup", onEnd);
    document.addEventListener(SELECT_END, onEnd);
    document.addEventListener("pointerdown", onDown, true);
    window.addEventListener("keydown", onKey, true);
    window.addEventListener("resize", onMove);
    document.addEventListener("scroll", onMove, true);
    return () => {
      window.clearTimeout(timer);
      document.removeEventListener("pointerup", onEnd);
      document.removeEventListener("keyup", onEnd);
      document.removeEventListener(SELECT_END, onEnd);
      document.removeEventListener("pointerdown", onDown, true);
      window.removeEventListener("keydown", onKey, true);
      window.removeEventListener("resize", onMove);
      document.removeEventListener("scroll", onMove, true);
    };
  }, [container]);

  // Above the selection, or under it when there's no room above; hidden while it's scrolled out of view.
  useLayoutEffect(() => {
    const root = container.current;
    const el = bar.current;
    const rect = pick?.place();
    if (!root || !el || !rect || (rect.width === 0 && rect.height === 0)) return setPos(null);
    const view = root.getBoundingClientRect();
    if (rect.bottom < view.top || rect.top > view.bottom) return setPos(null);
    const { width, height } = el.getBoundingClientRect();
    const gap = 8;
    const left = Math.min(Math.max(rect.left + rect.width / 2 - width / 2, view.left + gap), view.right - width - gap);
    const above = rect.top - height - gap;
    const top = above >= view.top + 4 ? above : Math.min(rect.bottom + gap, view.bottom - height - 4);
    setPos((p) => (p?.left === left && p.top === top ? p : { left, top }));
  }, [pick, asking, moved, container]);

  // The selection's email went away (the conversation changed, or the card collapsed).
  useEffect(() => {
    if (pick && pick.place() === null) close();
  }, [pick, moved]);

  if (!pick) return null;
  const run = (f: () => void) => {
    f();
    close();
    window.getSelection()?.removeAllRanges();
  };
  const ask = () => {
    if (!onAsk || !question.trim()) return;
    run(() => onAsk(pick.message, askQuestion(pick.text, question)));
  };

  return createPortal(
    <div
      ref={bar}
      role="toolbar"
      aria-label="AI for the selected text"
      className="fixed z-40 rounded-xl border border-hairline bg-base-100 p-1 text-[13px] shadow-popup"
      style={pos ? { left: pos.left, top: pos.top } : { left: 0, top: 0, visibility: "hidden" }}
      // Clicking the bar keeps the text selected (except in its own field).
      onMouseDown={(e) => {
        if (!(e.target instanceof HTMLInputElement)) e.preventDefault();
      }}
    >
      {asking && onAsk ? (
        <form
          className="flex w-80 items-center gap-1.5 pl-1.5"
          onSubmit={(e) => {
            e.preventDefault();
            ask();
          }}
        >
          <Sparkles size={15} className="shrink-0 text-secondary" aria-hidden />
          <input
            autoFocus
            aria-label="Ask AI about the selected text"
            className="min-w-0 flex-1 bg-transparent py-1 outline-none placeholder:text-text-3"
            placeholder="Ask AI about this…"
            value={question}
            onChange={(e) => setQuestion(e.target.value)}
            // Typing here mustn't drive the reader's shortcuts.
            onKeyDown={(e) => {
              if (e.key !== "Escape") e.stopPropagation();
            }}
          />
          <button
            type="submit"
            aria-label="Ask"
            disabled={!question.trim()}
            className="grid size-7 shrink-0 place-items-center rounded-lg bg-primary text-primary-content disabled:opacity-40"
          >
            <ArrowUp size={15} />
          </button>
        </form>
      ) : (
        <div className="flex items-center gap-0.5">
          {onAsk && (
            <BarButton
              icon={<Sparkles size={15} className="text-secondary" />}
              onClick={() => run(() => onAsk(pick.message, explainQuestion(pick.text)))}
            >
              Explain
            </BarButton>
          )}
          <BarButton
            icon={<Reply size={15} className="text-secondary" />}
            onClick={() => run(() => onReply(pick.message, replyInstructions(pick.text)))}
          >
            Reply to this
          </BarButton>
          {onAsk && (
            <BarButton icon={<MessageCircleQuestion size={15} className="text-secondary" />} onClick={() => setAsking(true)}>
              Ask AI…
            </BarButton>
          )}
        </div>
      )}
    </div>,
    document.body,
  );
}

function BarButton({ icon, onClick, children }: { icon: ReactNode; onClick: () => void; children: ReactNode }) {
  return (
    <button
      className="flex h-7 items-center gap-1.5 whitespace-nowrap rounded-lg px-2 font-medium hover:bg-secondary hover:text-secondary-content [&:hover_svg]:text-current"
      onClick={onClick}
    >
      {icon}
      {children}
    </button>
  );
}
