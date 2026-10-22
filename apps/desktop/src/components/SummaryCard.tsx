import { Copy, Ellipsis, RotateCcw, Settings2, Square, Trash2, X } from "lucide-react";
import { forwardRef, type ReactNode } from "react";
import { SUMMARY_STYLES, type SummaryEvent, type SummaryStyle } from "../ai";
import { formatFullDate } from "../lib/format";
import AiMark from "./AiMark";
import Dropdown, { MenuItem } from "./Dropdown";

export type SummaryStatus = "generating" | "done" | "stopped" | "refused" | "error" | "notConfigured";

/** One message's summary, as App keeps it. `run` tells a replaced run's late events apart. */
export interface SummaryState {
  run: number;
  style: SummaryStyle;
  status: SummaryStatus;
  text: string;
  startedAt: number;
  model?: string;
  truncated?: boolean;
  category?: string | null;
  message?: string;
}

export function startSummary(run: number, style: SummaryStyle): SummaryState {
  return { run, style, status: "generating", text: "", startedAt: Date.now() };
}

/** Folds one streamed event into the card's state. A refusal discards the partial text. */
export function reduceSummary(state: SummaryState, event: SummaryEvent): SummaryState {
  switch (event.event) {
    case "delta":
      return { ...state, text: state.text + event.data.text };
    case "done":
      return { ...state, status: "done", model: event.data.model, truncated: event.data.truncated };
    case "refused":
      return { ...state, status: "refused", text: "", category: event.data.category };
    case "error":
      return { ...state, status: "error", text: "", message: event.data.message };
    case "notConfigured":
      return { ...state, status: "notConfigured", text: "", message: event.data.message };
  }
}

/**
 * The +AI Summary card: "+ai", the style's title, a purple timestamp,
 * Copy and ⋯ (Update / Delete). Placed after the message.
 */
const SummaryCard = forwardRef<
  HTMLElement,
  {
    summary: SummaryState;
    provider: string;
    onStop: () => void;
    onRegenerate: () => void;
    onDelete: () => void;
    onOpenSettings: () => void;
    onCopied: () => void;
  }
>(function SummaryCard({ summary: s, provider, onStop, onRegenerate, onDelete, onOpenSettings, onCopied }, ref) {
  const generating = s.status === "generating";
  const hasText = s.status === "done" || s.status === "stopped" || (generating && s.text !== "");
  const title = SUMMARY_STYLES.find((x) => x.key === s.style)!.title;

  function copy() {
    void navigator.clipboard.writeText(s.text.trim()).then(onCopied);
  }

  return (
    <section
      ref={ref}
      aria-label={title}
      aria-busy={generating}
      className="mt-3 scroll-mt-3 rounded-2xl border border-secondary/20 bg-ai-tint px-5 pb-3.5 pt-3"
    >
      <header className="flex h-7 items-center gap-2 text-[13px]">
        <AiMark className={`text-sm ${generating ? "animate-pulse" : ""}`} />
        <span className="font-semibold">{title}</span>
        {generating && <span className="text-xs text-text-2">Generating…</span>}
        <div className="ml-auto flex items-center gap-0.5 text-text-2">
          {s.status === "done" && (
            <time
              className="mr-1.5 text-xs font-medium text-ai-time"
              dateTime={new Date(s.startedAt).toISOString()}
            >
              {formatFullDate(s.startedAt)}
            </time>
          )}
          {generating ? (
            <button className="btn btn-ghost btn-xs h-6 gap-1 px-2 font-medium" onClick={onStop}>
              <Square size={10} className="fill-current" />
              Stop
            </button>
          ) : hasText ? (
            <>
              <IconButton label="Copy summary" onClick={copy}>
                <Copy size={14} />
              </IconButton>
              <Dropdown
                align="end"
                width="w-40"
                trigger={({ toggle, open }) => (
                  <IconButton label="More" onClick={toggle} active={open}>
                    <Ellipsis size={15} />
                  </IconButton>
                )}
              >
                <MenuItem label="Update Summary" icon={<RotateCcw size={14} />} onSelect={onRegenerate} />
                <MenuItem label="Delete Summary" icon={<Trash2 size={14} />} danger onSelect={onDelete} />
              </Dropdown>
            </>
          ) : (
            <IconButton label="Dismiss" onClick={onDelete}>
              <X size={14} />
            </IconButton>
          )}
        </div>
      </header>

      <div aria-live="polite" className="mt-1">
        {generating && s.text === "" && (
          <div className="space-y-2 py-1.5" aria-label="Summarizing">
            {["w-11/12", "w-3/4", "w-5/6", "w-1/2"].map((w) => (
              <div key={w} className={`skeleton h-2.5 ${w} rounded-full bg-secondary/15`} />
            ))}
          </div>
        )}
        {hasText && <SummaryText text={s.text} caret={generating} />}

        {s.status === "refused" && (
          <Notice
            text={`${provider} declined to summarize this email${s.category ? ` (${s.category})` : ""}.`}
            actions={<SmallButton onClick={onRegenerate}>Try again</SmallButton>}
          />
        )}
        {s.status === "error" && (
          <Notice
            text={s.message ?? "Something went wrong."}
            tone="error"
            actions={
              <>
                <SmallButton onClick={onRegenerate}>Try again</SmallButton>
                <SmallButton onClick={onOpenSettings}>
                  <Settings2 size={13} /> AI settings
                </SmallButton>
              </>
            }
          />
        )}
        {s.status === "notConfigured" && (
          <Notice
            text={`Summaries run on your own AI provider. ${s.message ?? ""}`}
            actions={
              <button className="btn btn-primary btn-xs h-7 rounded-md px-3" onClick={onOpenSettings}>
                Set up AI
              </button>
            }
          />
        )}
      </div>

      {(s.status === "done" || s.status === "stopped") && (
        <footer className="mt-2 flex flex-wrap items-center gap-x-1.5 text-[11px] leading-4 text-text-2">
          <span>{s.status === "stopped" ? "Stopped" : s.model}</span>
          {s.truncated && <span className="text-warning">· Cut off at the length limit</span>}
        </footer>
      )}
    </section>
  );
});

export default SummaryCard;

/** Plain text from the model: "- " lines become a list, the rest paragraphs. */
function SummaryText({ text, caret }: { text: string; caret: boolean }) {
  const blocks: ({ kind: "p"; text: string } | { kind: "ul"; items: string[] })[] = [];
  for (const raw of text.split("\n")) {
    const line = raw.trim();
    if (!line) continue;
    const bullet = /^[-•*]\s+(.*)$/.exec(line);
    const last = blocks[blocks.length - 1];
    if (bullet) {
      if (last?.kind === "ul") last.items.push(bullet[1]);
      else blocks.push({ kind: "ul", items: [bullet[1]] });
    } else {
      blocks.push({ kind: "p", text: line });
    }
  }
  const cursor = caret ? (
    <span className="ml-0.5 inline-block h-3.5 w-[2px] translate-y-0.5 animate-pulse bg-secondary" />
  ) : null;
  return (
    <div className="select-text space-y-1.5 text-[13px] leading-5">
      {blocks.map((b, i) => {
        const end = i === blocks.length - 1 ? cursor : null;
        return b.kind === "p" ? (
          <p key={i}>
            {b.text}
            {end}
          </p>
        ) : (
          <ul key={i} className="list-disc space-y-0.5 pl-5 marker:text-secondary">
            {b.items.map((item, j) => (
              <li key={j}>
                {item}
                {j === b.items.length - 1 && end}
              </li>
            ))}
          </ul>
        );
      })}
      {blocks.length === 0 && cursor}
    </div>
  );
}

function Notice({ text, actions, tone }: { text: string; actions: ReactNode; tone?: "error" }) {
  return (
    <div className="py-1">
      <p className={`select-text text-[13px] leading-5 ${tone === "error" ? "text-error" : "text-text-2"}`}>{text}</p>
      <div className="mt-2 flex flex-wrap gap-1.5">{actions}</div>
    </div>
  );
}

function SmallButton({ onClick, children }: { onClick: () => void; children: ReactNode }) {
  return (
    <button
      className="btn btn-xs h-7 gap-1 rounded-md border-hairline bg-base-100/70 px-3 font-medium shadow-none hover:bg-base-100"
      onClick={onClick}
    >
      {children}
    </button>
  );
}

function IconButton({
  label,
  onClick,
  active = false,
  children,
}: {
  label: string;
  onClick: () => void;
  active?: boolean;
  children: ReactNode;
}) {
  return (
    <button
      className={`grid size-6 place-items-center rounded-full hover:bg-base-content/10 hover:text-base-content ${
        active ? "bg-base-content/10" : ""
      }`}
      title={label}
      aria-label={label}
      aria-haspopup={label === "More" ? "menu" : undefined}
      onClick={onClick}
    >
      {children}
    </button>
  );
}
