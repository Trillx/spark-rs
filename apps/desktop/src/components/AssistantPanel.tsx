import { ArrowUp, Check, ChevronDown, Copy, History, Sparkles, Square, SquarePen, Trash2, X } from "lucide-react";
import { Fragment, useEffect, useLayoutEffect, useMemo, useRef, useState, type ReactNode } from "react";
import {
  assistantDecide,
  assistantDeleteSession,
  assistantSend,
  assistantSession,
  assistantSessions,
  assistantStop,
  emptyAnswer,
  reduceAnswer,
  type Answer,
  type AssistantEvent,
  type Card,
  type DraftCard,
  type EmailRef,
  type Entry,
  type Screen,
  type SessionInfo,
} from "../assistant";
import { citedIds, parseAnswer, plainAnswer, revealTo, type Inline, type TextBlock } from "../lib/assistantText";
import { formatAgo, formatListDate } from "../lib/format";
import { openLink } from "../lib/links";
import Avatar from "./Avatar";

/** The panel's 364px and the margin around it: what the mail beside it gives up. */
export const ASSISTANT_SPACE = 380;
/** Below this window width there's no room for the pill. */
export const ASSISTANT_MIN_WIDTH = 700;

/** Characters an answer shows per tick, and the tick: about 1,000 a second, however they arrive. */
const PACE_CHARS = 30;
const PACE_MS = 30;
/** The status line shows once nothing new has come for this long, so it doesn't flicker between bursts. */
const QUIET_MS = 500;

/** What the pill and the empty chat suggest asking. */
export function assistantSuggestions(open: { subject: string } | null): string[] {
  const general = ["Find emails waiting for my reply", "Summarize my unread emails", "What's on my calendar today?"];
  return open ? ["Summarize this email", "Draft a reply to this email", general[0]] : general;
}

/**
 * The floating "Ask me anything..." button at the bottom right of the mail. Hovering it raises
 * a few questions to ask; clicking one asks it.
 */
export function AssistantPill({ suggestions, onAsk }: { suggestions: string[]; onAsk: (question?: string) => void }) {
  const [hover, setHover] = useState(false);
  return (
    <div
      className="absolute bottom-[18px] right-[18px] z-30 flex flex-col items-end gap-3"
      onPointerEnter={() => setHover(true)}
      onPointerLeave={() => setHover(false)}
    >
      {hover && (
        <ul aria-label="Try asking" className="ai-hints flex flex-col items-end gap-1">
          {suggestions.map((s) => (
            <li key={s}>
              <button
                className="rounded-2xl bg-base-100 px-3 py-1.5 text-left text-[13px] text-secondary shadow-popup hover:bg-base-200"
                onClick={() => onAsk(s)}
              >
                <span aria-hidden className="mr-1.5">
                  →
                </span>
                {s}
              </button>
            </li>
          ))}
        </ul>
      )}
      <button
        aria-label="Open AI Assistant"
        className={`rounded-3xl p-px shadow-[0_4px_15px_rgba(0,0,0,0.1)] transition-[width] duration-150 ease-linear ${
          hover ? "w-[300px] bg-ai-gradient" : "w-[240px] bg-ai-gradient-soft"
        }`}
        onClick={() => onAsk()}
      >
        <span className="flex h-10 items-center gap-2 rounded-3xl bg-base-100 pl-3 pr-2 text-sm text-text-2">
          <Sparkles size={18} className="shrink-0 text-secondary" />
          Ask me anything...
        </span>
      </button>
    </div>
  );
}

/** A question asked from outside the panel (the pill); `nonce` tells two asks of the same apart. */
export interface Ask {
  text: string;
  nonce: number;
}

/**
 * The AI Assistant: a chat at the right of the window over the user's mail and calendar,
 * answered by their own AI provider. It stays mounted while closed, so a chat goes on where it
 * was; New chat starts over and History brings back an earlier one.
 */
export default function AssistantPanel({
  open,
  ask,
  screen,
  name,
  onClose,
  onOpenEmail,
  onOpenDraft,
  onOpenSettings,
  onCopied,
  suggestions,
}: {
  open: boolean;
  ask: Ask | null;
  screen: Screen;
  /** The user's first name, for the greeting. */
  name: string;
  onClose: () => void;
  onOpenEmail: (email: EmailRef) => void;
  onOpenDraft: (draft: DraftCard) => void;
  onOpenSettings: () => void;
  onCopied: () => void;
  suggestions: string[];
}) {
  const [chat, setChat] = useState<{ id: string | null; entries: Entry[] }>({ id: null, entries: [] });
  /** The answer being written arrived here, so it types out; a chat from History shows at once. */
  const [live, setLive] = useState(false);
  const [input, setInput] = useState("");
  const [historyOpen, setHistoryOpen] = useState(false);
  /** Bumped by every send and switch, so events from an earlier one are dropped. */
  const run = useRef(0);
  const chatId = useRef<string | null>(null);
  chatId.current = chat.id;
  const inputRef = useRef<HTMLTextAreaElement>(null);
  const scrollRef = useRef<HTMLDivElement>(null);

  const last = chat.entries.at(-1);
  const working = last?.role === "assistant" && last.answer.status === "working";
  /** Every email the chat has seen: a later answer can cite one an earlier answer found. */
  const known = useMemo(() => {
    const all = new Map<number, EmailRef>();
    for (const e of chat.entries) if (e.role === "assistant") for (const m of e.answer.emails) all.set(m.id, m);
    return all;
  }, [chat.entries]);

  function updateAnswer(token: number, f: (a: Answer) => Answer) {
    if (run.current !== token) return;
    setChat((c) => {
      const entries = [...c.entries];
      const i = entries.length - 1;
      const e = entries[i];
      if (e?.role !== "assistant") return c;
      entries[i] = { role: "assistant", answer: f(e.answer) };
      return { ...c, entries };
    });
  }

  async function send(question: string) {
    const text = question.trim();
    if (!text) return;
    if (working && chat.id) await stop();
    const token = ++run.current;
    setLive(true);
    setInput("");
    setChat((c) => ({
      ...c,
      entries: [...c.entries, { role: "user", text }, { role: "assistant", answer: emptyAnswer() }],
    }));
    const onEvent = (event: AssistantEvent) => updateAnswer(token, (a) => reduceAnswer(a, event));
    try {
      const id = await assistantSend(chatId.current, text, screen, onEvent);
      if (run.current === token && id) setChat((c) => ({ ...c, id }));
    } catch (error) {
      updateAnswer(token, (a) => ({ ...a, status: "failed", message: String(error) }));
    }
  }

  async function stop() {
    const id = chatId.current;
    if (!id) return;
    // Events the stopped answer sent before it stopped can still be on their way: they mustn't
    // land on the answer as saved.
    const token = ++run.current;
    const answer = await assistantStop(id);
    if (answer) updateAnswer(token, () => answer);
    else updateAnswer(token, (a) => (a.status === "working" ? { ...a, status: "stopped" } : a));
  }

  async function newChat() {
    if (working) await stop();
    run.current++;
    setChat({ id: null, entries: [] });
    setLive(false);
    inputRef.current?.focus();
  }

  async function restore(id: string) {
    setHistoryOpen(false);
    // The chat on screen is already here, with its answer still arriving.
    if (id === chat.id) return inputRef.current?.focus();
    if (working) await stop();
    const token = ++run.current;
    try {
      const view = await assistantSession(id);
      if (run.current !== token) return;
      setChat({ id: view.id, entries: view.entries });
      setLive(false);
    } catch (error) {
      setChat({ id: null, entries: [{ role: "assistant", answer: { ...emptyAnswer(), status: "failed", message: String(error) } }] });
    }
    inputRef.current?.focus();
  }

  // A question from the pill.
  const asked = useRef<number | null>(null);
  useEffect(() => {
    if (!ask || asked.current === ask.nonce) return;
    asked.current = ask.nonce;
    void send(ask.text);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [ask]);

  useEffect(() => {
    if (open) inputRef.current?.focus();
  }, [open]);

  // New text and cards keep the newest in view.
  const scrollToEnd = () => {
    const el = scrollRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  };
  useLayoutEffect(scrollToEnd, [chat.entries.length]);

  const decide = (card: string, approve: boolean) => {
    if (chat.id) void assistantDecide(chat.id, card, approve).catch(() => {});
  };

  return (
    <aside
      aria-label="AI Assistant"
      data-assistant
      hidden={!open}
      className={`${open ? "flex" : "hidden"} assistant-in absolute bottom-2 right-2 top-8 z-30 w-[364px] flex-col overflow-hidden rounded-[20px] border border-hairline bg-base-100 shadow-popup`}
      onKeyDown={(e) => {
        if (e.key !== "Escape") return;
        e.stopPropagation();
        if (historyOpen) setHistoryOpen(false);
        else onClose();
      }}
    >
      <div aria-hidden className="pointer-events-none absolute inset-0 bg-ai-tint" />
      <header className="relative flex h-11 shrink-0 items-center border-b border-hairline px-1">
        <HeaderButton label="Close" onClick={onClose}>
          <X size={18} />
        </HeaderButton>
        <h2 className="flex flex-1 items-center justify-center gap-1.5 text-[13px] font-semibold">
          <Sparkles size={15} className="text-secondary" />
          AI Assistant
        </h2>
        <HeaderButton label="Start new chat" onClick={() => void newChat()} disabled={chat.entries.length === 0}>
          <SquarePen size={17} />
        </HeaderButton>
        <HeaderButton label="View past chats" onClick={() => setHistoryOpen((o) => !o)} active={historyOpen}>
          <History size={17} />
        </HeaderButton>
        {historyOpen && (
          <HistoryPopup
            current={chat.id}
            onPick={(id) => void restore(id)}
            onDelete={(id) => {
              if (id === chat.id) void newChat();
            }}
            onClose={() => setHistoryOpen(false)}
          />
        )}
      </header>

      <div ref={scrollRef} role="log" aria-live="polite" className="relative min-h-0 flex-1 overflow-y-auto px-4 pb-2 pt-4">
        {chat.entries.length === 0 ? (
          <Greeting name={name} suggestions={suggestions} onAsk={(s) => void send(s)} />
        ) : (
          <div className="flex flex-col gap-6">
            {chat.entries.map((e, i) =>
              e.role === "user" ? (
                <p
                  key={i}
                  className="ml-12 self-end whitespace-pre-wrap break-words rounded-[20px] bg-secondary/10 px-3.5 py-[7px] text-sm"
                >
                  {e.text}
                </p>
              ) : (
                <AnswerView
                  key={i}
                  answer={e.answer}
                  known={known}
                  animate={live && i === chat.entries.length - 1}
                  onGrow={scrollToEnd}
                  onOpenEmail={onOpenEmail}
                  onOpenDraft={onOpenDraft}
                  onOpenSettings={onOpenSettings}
                  onDecide={decide}
                  onCopied={onCopied}
                />
              ),
            )}
          </div>
        )}
      </div>

      <form
        className="relative m-3 mt-1"
        onSubmit={(e) => {
          e.preventDefault();
          void send(input);
        }}
      >
        <InputBox
          ref={inputRef}
          value={input}
          working={working}
          onChange={setInput}
          onSubmit={() => void send(input)}
          onStop={() => void stop()}
        />
      </form>
    </aside>
  );
}

function HeaderButton({
  label,
  onClick,
  disabled,
  active,
  children,
}: {
  label: string;
  onClick: () => void;
  disabled?: boolean;
  active?: boolean;
  children: ReactNode;
}) {
  return (
    <button
      type="button"
      data-tip={label}
      aria-label={label}
      disabled={disabled}
      onClick={onClick}
      className={`grid size-9 place-items-center rounded-lg text-text-2 hover:bg-base-content/10 hover:text-base-content disabled:opacity-40 disabled:hover:bg-transparent ${
        active ? "bg-base-content/10 text-base-content" : ""
      }`}
    >
      {children}
    </button>
  );
}

function Greeting({ name, suggestions, onAsk }: { name: string; suggestions: string[]; onAsk: (s: string) => void }) {
  return (
    <div className="flex flex-col">
      <h3 className="text-base font-semibold leading-6">
        {name ? `Hi ${name}, welcome to your AI Assistant!` : "Hi, welcome to your AI Assistant!"}
      </h3>
      <p className="mb-3 text-sm leading-[22px]">
        I can help you find important information and organize your inbox. Try asking me to:
      </p>
      <ul className="flex flex-col items-start gap-1">
        {suggestions.map((s) => (
          <li key={s}>
            <button
              className="ai-hint rounded-2xl px-3 py-1.5 text-left text-sm leading-5"
              onClick={() => onAsk(s)}
            >
              {s}
            </button>
          </li>
        ))}
      </ul>
    </div>
  );
}

/**
 * The question box: two lines, growing to five; Enter sends, Shift+Enter starts a line. Its
 * border lights up while it has focus. It doesn't shrink on blur, so the chat above stays put
 * while its buttons are clicked.
 */
function InputBox({
  ref,
  value,
  working,
  onChange,
  onSubmit,
  onStop,
}: {
  ref: React.Ref<HTMLTextAreaElement>;
  value: string;
  working: boolean;
  onChange: (v: string) => void;
  onSubmit: () => void;
  onStop: () => void;
}) {
  const [focused, setFocused] = useState(false);
  const own = useRef<HTMLTextAreaElement | null>(null);
  useLayoutEffect(() => {
    const el = own.current;
    if (!el) return;
    el.style.height = "auto";
    const line = 20;
    el.style.height = `${Math.max(2, Math.min(5, Math.round(el.scrollHeight / line))) * line}px`;
  }, [value]);
  return (
    <div className={`rounded-2xl p-px ${focused ? "bg-ai-gradient shadow-popup" : "bg-base-content/15"}`}>
      <div className="relative rounded-[15px] bg-base-100 px-3 pb-10 pt-3">
        <textarea
          ref={(el) => {
            own.current = el;
            if (typeof ref === "function") ref(el);
            else if (ref) (ref as React.RefObject<HTMLTextAreaElement | null>).current = el;
          }}
          rows={2}
          value={value}
          spellCheck
          aria-label="Ask me anything"
          placeholder="Ask me anything..."
          className="block w-full resize-none bg-transparent text-sm leading-5 outline-none placeholder:text-text-2"
          onFocus={() => setFocused(true)}
          onBlur={() => setFocused(false)}
          onChange={(e) => onChange(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) {
              e.preventDefault();
              if (!working) onSubmit();
            }
          }}
        />
        <button
          type={working ? "button" : "submit"}
          aria-label={working ? "Stop" : "Send"}
          data-tip={working ? "Stop" : undefined}
          disabled={!working && !value.trim()}
          onClick={(e) => {
            if (working) {
              e.preventDefault();
              onStop();
            }
          }}
          className={`absolute bottom-2 right-2 grid size-6 place-items-center rounded-full text-white transition-colors ${
            working || value.trim() ? "bg-ai-gradient hover:brightness-90" : "bg-base-content/25"
          }`}
        >
          {working ? <Square size={10} fill="currentColor" /> : <ArrowUp size={15} strokeWidth={2.5} />}
        </button>
      </div>
    </div>
  );
}

/**
 * One answer: its text typing out at an even pace, its cards once the text before them is out,
 * what it did ("Worked on 3 steps"), and Sources and Copy when it's finished.
 */
function AnswerView({
  answer,
  known,
  animate,
  onGrow,
  onOpenEmail,
  onOpenDraft,
  onOpenSettings,
  onDecide,
  onCopied,
}: {
  answer: Answer;
  known: Map<number, EmailRef>;
  animate: boolean;
  onGrow: () => void;
  onOpenEmail: (email: EmailRef) => void;
  onOpenDraft: (draft: DraftCard) => void;
  onOpenSettings: () => void;
  onDecide: (card: string, approve: boolean) => void;
  onCopied: () => void;
}) {
  const text = answer.blocks.map((b) => (b.type === "text" ? b.text : "")).join("");
  const complete = answer.status !== "working";
  const [shown, setShown] = useState(0);
  // A refusal takes back what streamed.
  const visible = animate ? Math.min(shown, text.length) : text.length;
  const done = complete && visible >= text.length;
  // A steady tick that reads the latest text, so text arriving faster than
  // the tick can't keep putting it off.
  const latest = useRef({ text, complete });
  latest.current = { text, complete };
  useEffect(() => {
    if (!animate || done) return;
    const timer = window.setInterval(() => {
      const { text, complete } = latest.current;
      setShown((n) => (n >= text.length ? n : revealTo(text, n, PACE_CHARS, complete)));
    }, PACE_MS);
    return () => window.clearInterval(timer);
  }, [animate, done]);
  useLayoutEffect(onGrow, [visible, answer.blocks.length, answer.status]);
  const [quiet, setQuiet] = useState(true);
  useEffect(() => {
    setQuiet(false);
    const timer = window.setTimeout(() => setQuiet(true), QUIET_MS);
    return () => window.clearTimeout(timer);
  }, [text.length, answer.blocks.length]);

  const caughtUp = visible >= text.length;
  // While a card waits for Approve or Skip, the card is what's happening.
  const awaiting = answer.blocks.some((b) => b.type === "card" && b.card.type === "action" && b.card.state === "waiting");
  const finished = complete && caughtUp;
  const emails = useMemo(
    () => new Map([...known, ...answer.emails.map((e) => [e.id, e] as const)]),
    [known, answer.emails],
  );
  const cited = citedIds(text).filter((id) => emails.has(id));

  let budget = visible;
  const parts: ReactNode[] = [];
  for (const [i, block] of answer.blocks.entries()) {
    if (block.type === "text") {
      const piece = block.text.slice(0, Math.max(0, budget));
      budget -= block.text.length;
      if (piece.trim())
        parts.push(<AnswerText key={i} text={piece} cited={cited} emails={emails} onOpenEmail={onOpenEmail} />);
    } else if (budget >= 0) {
      parts.push(<CardView key={i} card={block.card} onDecide={onDecide} onOpenDraft={onOpenDraft} />);
    }
  }

  const failed = answer.status === "failed" || answer.status === "refused";
  return (
    <article aria-busy={!finished} className="flex flex-col gap-3 text-sm leading-5">
      {parts}
      {!complete && caughtUp && (quiet || !text) && !awaiting && <Progress label={answer.steps.at(-1) ?? "Thinking"} />}
      {finished && answer.steps.length > 0 && <Steps steps={answer.steps} />}
      {failed && (
        <p role="alert" className="text-[13px] text-error">
          {answer.message ?? "Something went wrong. Please try again."}{" "}
          {/AI|provider|API key|model/i.test(answer.message ?? "") && answer.status === "failed" && (
            <button className="font-medium text-secondary hover:underline" onClick={onOpenSettings}>
              Open Settings
            </button>
          )}
        </p>
      )}
      {answer.status === "stopped" && <p className="text-xs text-text-2">Stopped</p>}
      {answer.status === "truncated" && <p className="text-xs text-warning">Cut off at the length limit</p>}
      {finished && text.trim() && (
        <Footer text={text} sources={cited.map((id) => emails.get(id)!)} onOpenEmail={onOpenEmail} onCopied={onCopied} />
      )}
    </article>
  );
}

function AnswerText({
  text,
  cited,
  emails,
  onOpenEmail,
}: {
  text: string;
  cited: number[];
  emails: Map<number, EmailRef>;
  onOpenEmail: (email: EmailRef) => void;
}) {
  const blocks = useMemo(() => parseAnswer(text), [text]);
  const inline = (items: Inline[]): ReactNode =>
    items.map((x, i) => {
      switch (x.kind) {
        case "text":
          return <Fragment key={i}>{x.text}</Fragment>;
        case "strong":
          return <strong key={i}>{inline(x.children)}</strong>;
        case "em":
          return <em key={i}>{inline(x.children)}</em>;
        case "code":
          return (
            <code key={i} className="rounded bg-base-200 px-1 text-[13px]">
              {x.text}
            </code>
          );
        case "link":
          return (
            <a
              key={i}
              href={x.href}
              className="text-secondary underline"
              onClick={(e) => {
                // The app opens links the way it opens them from emails: the browser, or a new email.
                e.preventDefault();
                openLink(x.href);
              }}
            >
              {inline(x.children)}
            </a>
          );
        case "cite": {
          const email = emails.get(x.id);
          if (!email) return null;
          const n = cited.indexOf(x.id) + 1;
          return (
            <button
              key={i}
              type="button"
              data-tip={`${email.from || email.address} · ${email.subject || "No subject"}`}
              aria-label={`Source ${n}: ${email.subject || "No subject"}`}
              className="mx-0.5 inline-grid h-4 min-w-4 -translate-y-px place-items-center rounded bg-base-content/10 px-1 align-middle text-[10px] font-semibold text-text-2 hover:bg-secondary/20 hover:text-secondary"
              onClick={() => onOpenEmail(email)}
            >
              {n}
            </button>
          );
        }
      }
    });
  const render = (bs: TextBlock[]): ReactNode =>
    bs.map((b, i) => {
      switch (b.kind) {
        case "p":
          return (
            <p key={i} className={b.strong ? "font-semibold" : undefined}>
              {b.lines.map((line, j) => (
                <Fragment key={j}>
                  {j > 0 && <br />}
                  {inline(line)}
                </Fragment>
              ))}
            </p>
          );
        case "list": {
          const items = b.items.map((item, j) => (
            <li key={j} className="pl-0.5">
              {render(item)}
            </li>
          ));
          return b.ordered ? (
            <ol key={i} start={b.start} className="flex list-decimal flex-col gap-1 pl-5">
              {items}
            </ol>
          ) : (
            <ul key={i} className="flex list-disc flex-col gap-1 pl-5">
              {items}
            </ul>
          );
        }
        case "quote":
          return (
            <blockquote key={i} className="rounded bg-base-200 px-2 py-1">
              {render(b.blocks)}
            </blockquote>
          );
        case "code":
          return (
            <pre key={i} className="whitespace-pre-wrap rounded bg-base-200 px-2 py-1 text-[13px]">
              {b.text}
            </pre>
          );
      }
    });
  return <div className="flex select-text flex-col gap-2 break-words">{render(blocks)}</div>;
}

/** The orb and a line saying what's happening, which slides when it changes. */
function Progress({ label }: { label: string }) {
  return (
    <output aria-live="polite" className="flex items-center gap-2 text-xs">
      <span aria-hidden className="ai-orb">
        <span />
      </span>
      <span key={label} className="ai-line-in ai-status">
        {label}
      </span>
    </output>
  );
}

function Steps({ steps }: { steps: string[] }) {
  return (
    <details className="group text-xs text-text-2">
      <summary className="flex cursor-pointer list-none items-center gap-1 hover:text-base-content">
        Worked on {steps.length === 1 ? "1 step" : `${steps.length} steps`}
        <ChevronDown size={13} className="transition-transform group-open:rotate-180" />
      </summary>
      <ol className="mt-1.5 flex flex-col">
        {steps.map((s, i) => (
          <li key={i} className="relative flex items-center gap-2 py-0.5 pl-1">
            <span className="size-[7px] rounded-full bg-text-2/70" />
            {s}
          </li>
        ))}
      </ol>
    </details>
  );
}

function Footer({
  text,
  sources,
  onOpenEmail,
  onCopied,
}: {
  text: string;
  sources: EmailRef[];
  onOpenEmail: (email: EmailRef) => void;
  onCopied: () => void;
}) {
  const [open, setOpen] = useState(false);
  const copy = (
    <button
      type="button"
      data-tip="Copy"
      aria-label="Copy"
      className="grid size-6 place-items-center rounded-md text-secondary/70 hover:bg-secondary/10 hover:text-secondary"
      onClick={() => void navigator.clipboard.writeText(plainAnswer(text, sources.map((s) => s.id))).then(onCopied)}
    >
      <Copy size={14} />
    </button>
  );
  if (!sources.length) return <div className="flex">{copy}</div>;
  return (
    <div className="flex flex-col gap-1.5">
      <div className="flex items-center">
        <button
          type="button"
          aria-expanded={open}
          className="flex items-center gap-1 text-xs font-medium text-secondary hover:underline"
          onClick={() => setOpen((o) => !o)}
        >
          Sources ({sources.length})
          <ChevronDown size={13} className={`transition-transform ${open ? "rotate-180" : ""}`} />
        </button>
        {!open && <span className="ml-auto">{copy}</span>}
      </div>
      {open && (
        <>
          <ul className="flex flex-col rounded-xl border border-hairline bg-base-100">
            {sources.map((s, i) => (
              <li key={s.id}>
                <button
                  type="button"
                  className="flex w-full items-center gap-2.5 px-3 py-2 text-left hover:bg-base-200"
                  onClick={() => onOpenEmail(s)}
                >
                  <Avatar name={s.from || s.address} />
                  <span className="min-w-0 flex-1">
                    <span className="block truncate text-[13px] font-semibold">{s.from || s.address}</span>
                    <span className="block truncate text-xs text-text-2">{s.subject || "No subject"}</span>
                  </span>
                  <span className="text-[11px] text-text-2">{s.date ? formatListDate(s.date) : ""}</span>
                  <span className="grid h-4 min-w-4 place-items-center rounded bg-base-content/10 px-1 text-[10px] font-semibold text-text-2">
                    {i + 1}
                  </span>
                </button>
              </li>
            ))}
          </ul>
          <div className="flex">{copy}</div>
        </>
      )}
    </div>
  );
}

function CardView({
  card,
  onDecide,
  onOpenDraft,
}: {
  card: Card;
  onDecide: (card: string, approve: boolean) => void;
  onOpenDraft: (draft: DraftCard) => void;
}) {
  return (
    <div className="ai-card-in rounded-2xl bg-ai-gradient p-px">
      <div className="flex flex-col gap-2 rounded-[15px] bg-base-100 p-3">
        {card.type === "draft" ? (
          <DraftView draft={card.draft} onOpen={() => onOpenDraft(card.draft)} />
        ) : (
          <ActionView card={card} onDecide={(approve) => onDecide(card.id, approve)} />
        )}
      </div>
    </div>
  );
}

const SHOWN_EMAILS = 5;

function ActionView({ card, onDecide }: { card: Extract<Card, { type: "action" }>; onDecide: (approve: boolean) => void }) {
  const [deciding, setDeciding] = useState(false);
  const more = card.emails.length - SHOWN_EMAILS;
  return (
    <>
      <p className="text-sm font-semibold">{card.title}</p>
      {card.emails.length > 0 && (
        <ul className="flex flex-col gap-1">
          {card.emails.slice(0, SHOWN_EMAILS).map((e) => (
            <li key={e.id} className="flex min-w-0 items-baseline gap-1.5 text-[13px]">
              <span className="max-w-[40%] shrink-0 truncate font-medium">{e.from || e.address}</span>
              <span className="truncate text-text-2">{e.subject || "No subject"}</span>
            </li>
          ))}
          {more > 0 && <li className="text-xs text-text-2">and {more} more</li>}
        </ul>
      )}
      {card.senders.length > 0 && (
        <ul className="flex flex-col gap-0.5 text-[13px] text-text-2">
          {card.senders.map((s) => (
            <li key={s} className="truncate">
              {s}
            </li>
          ))}
        </ul>
      )}
      {card.state === "waiting" ? (
        <div className="mt-1 flex gap-2">
          <button
            type="button"
            disabled={deciding}
            className="h-7 rounded-full bg-ai-gradient px-4 text-[13px] font-medium text-white hover:brightness-90 disabled:opacity-60"
            onClick={() => {
              setDeciding(true);
              onDecide(true);
            }}
          >
            Approve
          </button>
          <button
            type="button"
            disabled={deciding}
            className="h-7 rounded-full bg-base-content/10 px-4 text-[13px] font-medium hover:bg-base-content/15 disabled:opacity-60"
            onClick={() => {
              setDeciding(true);
              onDecide(false);
            }}
          >
            Skip
          </button>
        </div>
      ) : (
        <p
          className={`flex items-center gap-1 text-xs font-medium ${
            card.state === "done" ? "text-success" : card.state === "failed" ? "text-error" : "text-text-2"
          }`}
        >
          {card.state === "done" && <Check size={13} />}
          {card.state === "done" ? "Done" : card.state === "skipped" ? "Skipped" : `Couldn't do it: ${card.note ?? ""}`}
        </p>
      )}
    </>
  );
}

function DraftView({ draft, onOpen }: { draft: DraftCard; onOpen: () => void }) {
  return (
    <>
      <p className="text-sm font-semibold">{draft.subject || "No subject"}</p>
      <dl className="grid grid-cols-[auto_1fr] gap-x-1.5 text-xs">
        <dt className="text-text-2">From:</dt>
        <dd className="truncate text-secondary">{draft.from}</dd>
        <dt className="text-text-2">To:</dt>
        <dd className="truncate text-secondary">{draft.to || "—"}</dd>
        {draft.cc && (
          <>
            <dt className="text-text-2">Cc:</dt>
            <dd className="truncate text-secondary">{draft.cc}</dd>
          </>
        )}
      </dl>
      <p className="line-clamp-[8] whitespace-pre-wrap text-[13px]">{draft.body}</p>
      <button
        type="button"
        className="mt-1 h-7 self-start rounded-full bg-ai-gradient px-4 text-[13px] font-medium text-white hover:brightness-90"
        onClick={onOpen}
      >
        Open Draft
      </button>
    </>
  );
}

/** Past chats, newest first: pick one to go on with it, or delete it. */
function HistoryPopup({
  current,
  onPick,
  onDelete,
  onClose,
}: {
  current: string | null;
  onPick: (id: string) => void;
  onDelete: (id: string) => void;
  onClose: () => void;
}) {
  const [sessions, setSessions] = useState<SessionInfo[] | null>(null);
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    void assistantSessions().then(setSessions, () => setSessions([]));
  }, []);
  useEffect(() => {
    const away = (e: PointerEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node) && !(e.target as Element).closest?.("[aria-label='View past chats']"))
        onClose();
    };
    document.addEventListener("pointerdown", away);
    return () => document.removeEventListener("pointerdown", away);
  }, [onClose]);
  return (
    <div
      ref={ref}
      role="dialog"
      aria-label="Past chats"
      className="absolute right-2 top-10 z-10 flex max-h-[60vh] w-[300px] flex-col overflow-hidden rounded-xl border border-hairline bg-base-100 shadow-popup"
    >
      <header className="px-3 pb-1 pt-2.5 text-xs font-semibold text-text-2">Past chats</header>
      <ul className="overflow-y-auto pb-1.5">
        {sessions === null ? (
          <li className="px-3 py-2 text-[13px] text-text-2">Loading…</li>
        ) : sessions.length === 0 ? (
          <li className="px-3 py-2 text-[13px] text-text-2">No chats yet.</li>
        ) : (
          sessions.map((s) => (
            <li key={s.id} className="group flex items-center">
              <button
                type="button"
                className={`flex min-w-0 flex-1 flex-col px-3 py-1.5 text-left hover:bg-base-200 ${
                  s.id === current ? "bg-base-200" : ""
                }`}
                onClick={() => onPick(s.id)}
              >
                <span className="truncate text-[13px]">{s.title}</span>
                <span className="text-[11px] text-text-2">{formatAgo(s.updated)}</span>
              </button>
              <button
                type="button"
                aria-label={`Delete “${s.title}”`}
                data-tip="Delete"
                className="mr-1.5 hidden size-7 shrink-0 place-items-center rounded-md text-text-2 hover:bg-base-content/10 hover:text-error group-hover:grid"
                onClick={() => {
                  void assistantDeleteSession(s.id).then(() => {
                    setSessions((all) => all?.filter((x) => x.id !== s.id) ?? null);
                    onDelete(s.id);
                  });
                }}
              >
                <Trash2 size={14} />
              </button>
            </li>
          ))
        )}
      </ul>
    </div>
  );
}
