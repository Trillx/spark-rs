import {
  AlignLeft,
  ArrowDownLeft,
  ArrowLeft,
  Ban,
  Check,
  ChevronDown,
  Circle,
  Clock,
  Ellipsis,
  Forward,
  Inbox,
  ListChecks,
  Pin,
  Reply,
  ReplyAll,
  Text,
  Trash2,
  Zap,
  type LucideIcon,
} from "lucide-react";
import { useMemo, useRef, useState, type ReactNode } from "react";
import { SUMMARY_STYLES, type SummaryStyle } from "../ai";
import type { AccountView, Message, MessageContent } from "../types";
import { useAccount } from "../lib/accounts";
import { allowRemoteImages, remoteImagesAllowed, useMessageContent } from "../lib/content";
import { LATER_PRESETS, formatFullDate, formatLater, parseAddress, presetDetail, type LaterPreset } from "../lib/format";
import { hint, shortcutLabel, type ActionId } from "../lib/shortcuts";
import AiMark from "./AiMark";
import Avatar from "./Avatar";
import Dropdown, { MenuItem, Popover } from "./Dropdown";
import { BlockedImagesBar, HtmlBody, TextBody, hasRemoteImages } from "./MessageBody";
import SummaryCard, { type SummaryState } from "./SummaryCard";

export interface ThreadActions {
  close: () => void;
  done: (m: Message) => void;
  trash: (m: Message) => void;
  priority: (m: Message) => void;
  read: (m: Message) => void;
  pin: (m: Message) => void;
  aside: (m: Message) => void;
  snooze: (m: Message, preset: LaterPreset) => void;
  summarize: (m: Message, style: SummaryStyle) => void;
  stopSummary: (m: Message) => void;
  deleteSummary: (m: Message) => void;
  summaryCopied: () => void;
  openAiSettings: () => void;
  reply: (m: Message, body?: string) => void;
  replyAll: (m: Message) => void;
  forward: (m: Message) => void;
  accept: (m: Message) => void;
  block: (m: Message) => void;
}

/** Quick replies. They insert text until AI drafting exists. */
const QUICK_REPLIES = [
  { label: "Interested", body: "Thanks, I'm interested. " },
  { label: "Not interested", body: "Thanks for reaching out, but I'm not interested." },
  { label: "Thanks", body: "Thanks!" },
];

const STYLE_ICONS: Record<SummaryStyle, LucideIcon> = { short: Text, detailed: AlignLeft, actionPoints: ListChecks };
const STYLE_SHORTCUTS: Record<SummaryStyle, ActionId> = {
  short: "summarize",
  detailed: "summarizeDetailed",
  actionPoints: "summarizeActions",
};

export default function ThreadView({
  message: m,
  singleColumn,
  summary,
  aiProvider,
  actions,
}: {
  message: Message;
  singleColumn: boolean;
  summary?: SummaryState;
  /** Names the provider in a refusal, e.g. "Anthropic". */
  aiProvider: string;
  actions: ThreadActions;
}) {
  const screening = m.screening && m.state === "inbox";
  const summaryRef = useRef<HTMLElement>(null);
  const closeTitle = singleColumn ? "Back to list (Esc)" : hint("Close", "close");
  const jumpToSummary = () => summaryRef.current?.scrollIntoView({ block: "nearest", behavior: "smooth" });

  return (
    <>
      {screening ? (
        <GatekeeperToolbar message={m} closeTitle={closeTitle} actions={actions} />
      ) : (
        <Toolbar
          message={m}
          closeTitle={closeTitle}
          summary={summary}
          onJumpToSummary={jumpToSummary}
          actions={actions}
        />
      )}
      <div className="min-h-0 flex-1 overflow-y-auto px-3 pb-8">
        <div className="flex flex-wrap items-center gap-x-2 gap-y-1 px-3 pb-3 pt-2">
          <h1 className="select-text text-sm font-semibold leading-[22px]">{m.subject || "No subject"}</h1>
          {m.priority && (
            <Pill className="bg-priority-fill text-priority">
              <Zap size={12} className="fill-current" /> Priority
            </Pill>
          )}
          {m.state === "snoozed" && m.snoozedUntil !== undefined && (
            <Pill className="bg-priority-fill text-priority">
              <Clock size={12} /> Snoozed until {formatLater(m.snoozedUntil)}
            </Pill>
          )}
          {m.state === "aside" && (
            <Pill className="bg-primary/10 text-primary">
              <ArrowDownLeft size={12} /> Set aside
            </Pill>
          )}
        </div>
        <MessageCard message={m} showReplyBar={!screening} actions={actions} />
        {summary && (
          <SummaryCard
            ref={summaryRef}
            summary={summary}
            provider={aiProvider}
            onStop={() => actions.stopSummary(m)}
            onRegenerate={() => actions.summarize(m, summary.style)}
            onDelete={() => actions.deleteSummary(m)}
            onOpenSettings={actions.openAiSettings}
            onCopied={actions.summaryCopied}
          />
        )}
      </div>
    </>
  );
}

/** Thread toolbar: the `[← │ ✓]` segment, then the message actions. */
function Toolbar({
  message: m,
  closeTitle,
  summary,
  onJumpToSummary,
  actions,
}: {
  message: Message;
  closeTitle: string;
  summary?: SummaryState;
  onJumpToSummary: () => void;
  actions: ThreadActions;
}) {
  const isDone = m.state === "done";
  return (
    <div role="toolbar" aria-label="Email thread toolbar" className="flex h-12 shrink-0 items-center gap-0.5 px-3">
      <div className="mr-1.5 flex h-8 items-center rounded-lg border border-hairline">
        <SegmentButton title={closeTitle} Icon={ArrowLeft} onClick={actions.close} />
        <span className="h-4 w-px bg-hairline" />
        <SegmentButton
          title={isDone ? "Move to Inbox" : hint("Done", "done")}
          Icon={isDone ? Inbox : Check}
          onClick={() => actions.done(m)}
        />
      </div>
      {m.state !== "deleted" && (
        <ToolbarButton title={hint("Move to Trash", "trash")} Icon={Trash2} onClick={() => actions.trash(m)} />
      )}
      <ToolbarButton
        title={hint(m.priority ? "Remove from Priority" : "Mark as Priority", "priority")}
        Icon={Zap}
        active={m.priority}
        iconClass={m.priority ? "fill-current text-priority" : ""}
        onClick={() => actions.priority(m)}
      />
      <ToolbarButton
        title={hint(m.unread ? "Mark as Read" : "Mark as Unread", "read")}
        Icon={Circle}
        active={m.unread}
        iconClass={m.unread ? "fill-current text-primary" : ""}
        onClick={() => actions.read(m)}
      />
      <ToolbarButton
        title={hint(m.pinned ? "Unpin" : "Pin", "pin")}
        Icon={Pin}
        active={m.pinned}
        iconClass={m.pinned ? "fill-current text-pin" : ""}
        onClick={() => actions.pin(m)}
      />
      <ToolbarButton
        title={m.state === "aside" ? "Move back to Inbox" : hint("Set Aside", "aside")}
        Icon={ArrowDownLeft}
        active={m.state === "aside"}
        iconClass={m.state === "aside" ? "text-primary" : ""}
        onClick={() => actions.aside(m)}
      />
      <Dropdown
        trigger={({ toggle, open }) => (
          <button
            className={`btn btn-ghost btn-sm gap-0.5 px-1.5 text-text-2 hover:text-base-content ${open ? "bg-base-content/10" : ""}`}
            title={hint("Snooze", "snooze")}
            aria-label="Snooze"
            aria-haspopup="menu"
            aria-expanded={open}
            onClick={toggle}
          >
            <Clock size={18} />
            <ChevronDown size={12} />
          </button>
        )}
      >
        {LATER_PRESETS.map((p) => (
          <MenuItem key={p.key} label={p.label} detail={presetDetail(p)} onSelect={() => actions.snooze(m, p)} />
        ))}
      </Dropdown>
      <SummarizeButton message={m} summary={summary} onJump={onJumpToSummary} actions={actions} />
      <div className="ml-auto">
        <Dropdown
          align="end"
          trigger={({ toggle, open }) => (
            <button
              className={`btn btn-ghost btn-sm btn-square text-text-2 hover:text-base-content ${open ? "bg-base-content/10" : ""}`}
              title="More"
              aria-label="More"
              aria-haspopup="menu"
              aria-expanded={open}
              onClick={toggle}
            >
              <Ellipsis size={18} />
            </button>
          )}
        >
          <MessageMenuItems message={m} actions={actions} />
        </Dropdown>
      </div>
    </div>
  );
}

/** "+AI Summary" and its "Summarize with" popover. */
function SummarizeButton({
  message: m,
  summary,
  onJump,
  actions,
}: {
  message: Message;
  summary?: SummaryState;
  onJump: () => void;
  actions: ThreadActions;
}) {
  const [style, setStyle] = useState<SummaryStyle>(summary?.style ?? "short");
  const exists = summary?.status === "done" || summary?.status === "stopped" || summary?.status === "generating";
  return (
    <Popover
      label="Summarize with"
      trigger={({ toggle, open }) => (
        <button
          className={`ml-1 inline-flex h-7 items-center gap-1 rounded-full px-2.5 text-xs font-medium leading-5 text-text-2 hover:bg-base-content/10 hover:text-base-content ${
            open ? "bg-base-content/10" : ""
          }`}
          title={hint("+AI Summary", "summarize")}
          aria-label="+AI Summary"
          aria-haspopup="dialog"
          aria-expanded={open}
          onClick={toggle}
        >
          <AiMark className="text-[13px]" />
          Summary
        </button>
      )}
    >
      {(close) => (
        <>
          <div className="mb-3 flex items-center gap-2">
            <span className="text-sm font-semibold">Summarize with</span>
            <span className="rounded-md bg-base-300 px-1.5 py-1">
              <AiMark className="text-xs" />
            </span>
          </div>
          <div role="radiogroup" aria-label="Summary style" className="grid grid-cols-3 gap-2">
            {SUMMARY_STYLES.map(({ key, label }) => {
              const Icon = STYLE_ICONS[key];
              return (
                <button
                  key={key}
                  role="radio"
                  aria-checked={style === key}
                  title={hint(label, STYLE_SHORTCUTS[key])}
                  onClick={() => setStyle(key)}
                  className={`flex flex-col items-center gap-1.5 rounded-xl border px-1 py-2.5 text-xs font-medium ${
                    style === key ? "border-primary bg-primary/5 text-primary" : "border-hairline hover:bg-row-hover"
                  }`}
                >
                  <Icon size={18} />
                  {label}
                </button>
              );
            })}
          </div>
          <div className="my-3 h-px bg-hairline" />
          <button
            className="btn btn-primary btn-sm h-9 w-full rounded-lg"
            onClick={() => {
              close();
              actions.summarize(m, style);
            }}
          >
            {exists ? "Update Summary" : "Generate Summary"}
          </button>
          {exists && (
            <button
              className="btn btn-ghost btn-sm mt-1 h-8 w-full rounded-lg font-medium text-primary"
              onClick={() => {
                close();
                onJump();
              }}
            >
              Jump to Summary
            </button>
          )}
        </>
      )}
    </Popover>
  );
}

function MessageMenuItems({ message: m, actions }: { message: Message; actions: ThreadActions }) {
  return (
    <>
      <MenuItem label="Reply" detail={shortcutLabel("reply")} icon={<Reply size={15} />} onSelect={() => actions.reply(m)} />
      <MenuItem
        label="Reply all"
        detail={shortcutLabel("replyAll")}
        icon={<ReplyAll size={15} />}
        onSelect={() => actions.replyAll(m)}
      />
      <MenuItem
        label="Forward"
        detail={shortcutLabel("forward")}
        icon={<Forward size={15} />}
        onSelect={() => actions.forward(m)}
      />
      <li role="none" className="my-0.5 h-px bg-hairline" />
      <MenuItem label="Block Sender" icon={<Ban size={15} />} danger onSelect={() => actions.block(m)} />
    </>
  );
}

/** Shown instead of the toolbar for messages still in screening. */
function GatekeeperToolbar({
  message: m,
  closeTitle,
  actions,
}: {
  message: Message;
  closeTitle: string;
  actions: ThreadActions;
}) {
  return (
    <div className="shrink-0 px-3 pb-2">
      <div className="flex h-12 items-center">
        <ToolbarButton title={closeTitle} Icon={ArrowLeft} onClick={actions.close} />
      </div>
      <div className="flex flex-wrap items-center gap-4 rounded-2xl border border-hairline px-4 py-3.5">
        <Avatar name={m.from} size="xl" />
        <div className="min-w-0 flex-1">
          <div className="truncate text-base font-semibold leading-6">{m.from}</div>
          <div className="truncate text-[13px] text-text-2">{m.address}</div>
          <span className="mt-1 inline-block rounded-full bg-base-300 px-2 text-[11px] font-medium leading-[18px] text-text-2">
            New sender
          </span>
        </div>
        <div className="flex gap-2">
          <button
            className="btn btn-sm border-0 bg-primary/10 px-5 text-primary shadow-none hover:bg-primary/20"
            onClick={() => actions.accept(m)}
          >
            Accept
          </button>
          <button
            className="btn btn-sm border-0 bg-error/10 px-5 text-error shadow-none hover:bg-error/20"
            onClick={() => actions.block(m)}
          >
            Block
          </button>
        </div>
      </div>
    </div>
  );
}

/** One message: a one-line header, the body, attachments and the bottom bar. */
function MessageCard({
  message: m,
  showReplyBar,
  actions,
}: {
  message: Message;
  showReplyBar: boolean;
  actions: ThreadActions;
}) {
  const account = useAccount(m.account);
  const [state, retry] = useMessageContent(m.id);
  const content = state.status === "ready" ? state.content : null;
  return (
    <article className="rounded-2xl border border-hairline bg-card">
      <header className="flex items-center gap-3 px-5 pb-1.5 pt-3.5">
        <Avatar name={m.from} size="lg" account={m.account} />
        <div className="min-w-0 flex-1 truncate text-sm leading-5">
          <span className="font-bold" title={m.address}>
            {m.from}
          </span>
          {content && <Recipients content={content} account={account} />}
        </div>
        <time className="shrink-0 text-xs leading-5 text-text-2" dateTime={new Date(m.timestamp).toISOString()}>
          {formatFullDate(m.timestamp)}
        </time>
        <Dropdown
          align="end"
          width="w-48"
          trigger={({ toggle, open }) => (
            <button
              className={`grid size-6 place-items-center rounded-full text-text-2 hover:bg-base-content/10 hover:text-base-content ${
                open ? "bg-base-content/10" : ""
              }`}
              title="Message actions"
              aria-label="Message actions"
              aria-haspopup="menu"
              onClick={toggle}
            >
              <Ellipsis size={16} />
            </button>
          )}
        >
          <MessageMenuItems message={m} actions={actions} />
        </Dropdown>
      </header>
      {state.status === "loading" && <BodySkeleton />}
      {state.status === "error" && (
        <div className="px-5 pb-5 pt-2 text-[13px] leading-5">
          <p className="select-text text-error">Couldn't load this email. {state.message}</p>
          <button className="mt-1 font-medium text-primary hover:underline" onClick={retry}>
            Try again
          </button>
        </div>
      )}
      {content && <Body id={m.id} content={content} />}
      {content && content.attachments.length > 0 && (
        <div className="flex flex-wrap gap-2 px-5 pb-4">
          {content.attachments.map((name, i) => (
            <AttachmentTile key={i} name={name} />
          ))}
        </div>
      )}
      {showReplyBar && (
        <footer className="flex flex-wrap items-center justify-end gap-1.5 rounded-b-2xl border-t border-hairline px-4 py-2.5">
          {QUICK_REPLIES.map(({ label, body }) => (
            <button
              key={label}
              className="h-7 rounded-md bg-chip px-3 text-xs font-medium text-base-content/80 hover:brightness-95"
              onClick={() => actions.reply(m, body)}
            >
              {label}
            </button>
          ))}
          <span className="mx-1 h-5 w-px bg-hairline" />
          <ToolbarButton title={hint("Reply", "reply")} Icon={Reply} onClick={() => actions.reply(m)} />
          <ToolbarButton title={hint("Forward", "forward")} Icon={Forward} onClick={() => actions.forward(m)} />
        </footer>
      )}
    </article>
  );
}

/** The email's HTML in the sandboxed frame (remote images on request), else its text. */
function Body({ id, content }: { id: number; content: MessageContent }) {
  const [images, setImages] = useState(() => remoteImagesAllowed(id));
  const html = content.html;
  const blocked = useMemo(() => html !== null && hasRemoteImages(html), [html]);
  if (html === null) return <TextBody paragraphs={content.text} />;
  return (
    <>
      {blocked && !images && (
        <BlockedImagesBar
          onLoad={() => {
            allowRemoteImages(id);
            setImages(true);
          }}
        />
      )}
      <HtmlBody html={html} remoteImages={images} />
    </>
  );
}

/** " to You, Ana Ruiz cc Ben": names only, the full address in each tooltip. */
function Recipients({ content, account }: { content: MessageContent; account?: AccountView }) {
  const own = account?.email.toLowerCase();
  const list = (items: string[]) =>
    items.map((raw, i) => {
      const { name, address } = parseAddress(raw);
      const you = raw === "You" || address.toLowerCase() === own;
      return (
        <span key={i}>
          {i > 0 && <span className="text-text-2">, </span>}
          <span
            className={`font-medium ${you ? "text-primary" : ""}`}
            title={you && account ? `${account.email} (${account.label})` : raw}
          >
            {you ? "You" : name}
          </span>
        </span>
      );
    });
  return (
    <>
      {content.to.length > 0 && <span className="text-text-2"> to </span>}
      {list(content.to)}
      {content.cc.length > 0 && <span className="text-text-2"> cc </span>}
      {list(content.cc)}
    </>
  );
}

function BodySkeleton() {
  return (
    <div className="appear-late space-y-2.5 px-5 pb-6 pt-3" aria-label="Loading email">
      {["w-11/12", "w-4/5", "w-5/6", "w-2/5"].map((w) => (
        <div key={w} className={`skeleton h-2.5 ${w} rounded-full bg-base-content/5`} />
      ))}
    </div>
  );
}

function AttachmentTile({ name }: { name: string }) {
  const ext = name.includes(".") ? name.split(".").pop()!.toUpperCase().slice(0, 4) : "FILE";
  const tint =
    ext === "PDF" ? "bg-error/10 text-error" : ext === "ZIP" ? "bg-warning/15 text-warning" : "bg-primary/10 text-primary";
  return (
    <div className="flex w-60 items-center gap-2.5 rounded-xl border border-hairline p-2" title={name}>
      <span className={`grid size-9 shrink-0 place-items-center rounded-lg text-[10px] font-bold ${tint}`}>{ext}</span>
      <div className="min-w-0">
        <div className="truncate text-[13px] font-medium leading-5">{name}</div>
        <div className="text-[11px] leading-4 text-text-2">{ext === "FILE" ? "Attachment" : `${ext} file`}</div>
      </div>
    </div>
  );
}

function SegmentButton({ title, Icon, onClick }: { title: string; Icon: LucideIcon; onClick: () => void }) {
  return (
    <button
      className="grid h-full w-8 place-items-center text-text-2 first:rounded-l-lg last:rounded-r-lg hover:bg-base-content/5 hover:text-base-content"
      title={title}
      aria-label={title}
      onClick={onClick}
    >
      <Icon size={17} />
    </button>
  );
}

function ToolbarButton({
  title,
  Icon,
  onClick,
  active = false,
  iconClass = "",
}: {
  title: string;
  Icon: LucideIcon;
  onClick: () => void;
  active?: boolean;
  iconClass?: string;
}) {
  return (
    <button
      className="btn btn-ghost btn-sm btn-square text-text-2 hover:text-base-content"
      title={title}
      aria-label={title}
      aria-pressed={active || undefined}
      onClick={onClick}
    >
      <Icon size={18} className={iconClass} />
    </button>
  );
}

function Pill({ className, children }: { className: string; children: ReactNode }) {
  return (
    <span className={`inline-flex h-[22px] items-center gap-1 rounded-full px-2 text-xs font-medium ${className}`}>
      {children}
    </span>
  );
}
