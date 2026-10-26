import {
  AlignLeft,
  ArrowDownLeft,
  ArrowLeft,
  Ban,
  Check,
  ChevronDown,
  ChevronsDownUp,
  ChevronsUpDown,
  Circle,
  Clock,
  Ellipsis,
  Forward,
  Inbox,
  ListChecks,
  Pin,
  Reply,
  ReplyAll,
  ShieldAlert,
  ShieldCheck,
  Text,
  Trash2,
  Zap,
  type LucideIcon,
} from "lucide-react";
import { Fragment, useLayoutEffect, useMemo, useRef, useState, type ReactNode, type RefObject } from "react";
import { QUICK_REPLIES, SUMMARY_STYLES, type QuickReply, type SummaryStyle } from "../ai";
import type { AccountView, Message, MessageContent } from "../types";
import { isFromYou, useAccount } from "../lib/accounts";
import { readingStart } from "../lib/conversation";
import { allowRemoteImages, remoteImagesAllowed, useInlineImages, useMessageContent } from "../lib/content";
import {
  LATER_PRESETS,
  formatFullDate,
  formatLater,
  formatListDate,
  parseAddress,
  presetDetail,
  type LaterPreset,
} from "../lib/format";
import { hint, shortcutLabel, type ActionId } from "../lib/shortcuts";
import { tipAttrs, tipLabel, type Tip } from "../lib/tooltip";
import AiMark from "./AiMark";
import OverlayScroll from "./OverlayScroll";
import { AttachmentList } from "./Attachments";
import Avatar from "./Avatar";
import Dropdown, { MenuItem, Popover } from "./Dropdown";
import InviteCard from "./InviteCard";
import { BlockedImagesBar, HtmlBody, TextBody, hasInlineImages, hasRemoteImages } from "./MessageBody";
import SummaryCard, { type SummaryState } from "./SummaryCard";

export interface ThreadActions {
  close: () => void;
  done: (m: Message) => void;
  /** Its conversation to Trash; deleted for good if it's there already. */
  trash: (m: Message) => void;
  /** Just this email, as `trash` does. */
  trashOne: (m: Message) => void;
  /** Move to Spam, or Not Spam for mail in Spam. */
  spam: (m: Message) => void;
  inSpam: (m: Message) => boolean;
  deleteForever: (m: Message) => void;
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
  reply: (m: Message) => void;
  /** Opens a reply that AI writes for the chip. */
  quickReply: (m: Message, chip: QuickReply) => void;
  replyAll: (m: Message) => void;
  forward: (m: Message) => void;
  accept: (m: Message) => void;
  block: (m: Message) => void;
  /** Settle when done (errors are reported by the caller), so tiles can show progress. */
  openAttachment: (m: Message, index: number) => Promise<void>;
  saveAttachment: (m: Message, index: number) => Promise<void>;
  saveAllAttachments: (m: Message) => Promise<void>;
}

const STYLE_ICONS: Record<SummaryStyle, LucideIcon> = { short: Text, detailed: AlignLeft, actionPoints: ListChecks };
const STYLE_SHORTCUTS: Record<SummaryStyle, ActionId> = {
  short: "summarize",
  detailed: "summarizeDetailed",
  actionPoints: "summarizeActions",
};

/**
 * The open conversation, Spark's way: one card per email, oldest first.
 * With "Minimize old emails" on, emails before the first one that was unread
 * when it opened (or before the one opened, if that's earlier) start
 * collapsed to a line; clicking a card's header collapses or expands it.
 */
export default function ThreadView({
  message: m,
  conversation,
  unread,
  minimize,
  singleColumn,
  summary,
  aiProvider,
  actions,
  scrollRef,
}: {
  /** The email opened from the list, with its conversation's flags. Toolbar actions act on its conversation. */
  message: Message;
  /** Its conversation, oldest first, `message` included. */
  conversation: Message[];
  /** Which of them were unread when the conversation opened. */
  unread: ReadonlySet<number>;
  /** Settings → Appearance → Minimize old emails. Off, every email starts expanded. */
  minimize: boolean;
  singleColumn: boolean;
  summary?: SummaryState;
  /** Names the provider in a refusal, e.g. "Anthropic". */
  aiProvider: string;
  actions: ThreadActions;
  /** The element that scrolls, for keyboard paging. */
  scrollRef: RefObject<HTMLDivElement | null>;
}) {
  const screening = m.screening && m.state === "inbox";
  const summaryRef = useRef<HTMLElement>(null);
  /** Stops holding the place reading starts at, once the user moves around. */
  const stopHolding = useRef(() => {});
  /** Scrolled down: the subject shrinks to a line over a border, like Spark's header. */
  const [scrolled, setScrolled] = useState(false);
  const closeTip: Tip = singleColumn ? { label: "Back to list", keys: "Esc" } : hint("Close", "close");
  const jumpToSummary = () => {
    stopHolding.current();
    summaryRef.current?.scrollIntoView({ block: "nearest", behavior: "smooth" });
  };
  const [start] = useState(() => conversation[readingStart(conversation, m.id, unread)]?.id);
  /** Collapsed cards. Emails that arrive while it's open aren't in it, so they show expanded. */
  const [collapsed, setCollapsed] = useState<ReadonlySet<number>>(() => {
    const at = minimize ? conversation.findIndex((x) => x.id === start) : 0;
    return new Set(conversation.slice(0, Math.max(at, 0)).map((x) => x.id));
  });
  const firstNew = conversation.findIndex((x) => unread.has(x.id));
  const several = conversation.length > 1;
  const latest = conversation[conversation.length - 1];

  const toggle = (id: number) =>
    setCollapsed((c) => {
      const next = new Set(c);
      if (!next.delete(id)) next.add(id);
      return next;
    });
  const expandAll = () => {
    stopHolding.current();
    setCollapsed(new Set());
  };
  /** Like Spark, the latest email stays open. */
  const collapseAll = () => {
    stopHolding.current();
    setCollapsed(new Set(conversation.filter((x) => x !== latest).map((x) => x.id)));
  };

  // Reading starts at the first expanded email, below the ones collapsed above
  // it, with a little of the one before still in view. Like Spark, that place
  // holds as bodies load and grow, until the user scrolls or clicks.
  useLayoutEffect(() => {
    const el = scrollRef.current;
    const target =
      start === undefined || start === conversation[0]?.id
        ? null
        : el?.querySelector<HTMLElement>(`[data-message="${start}"]`);
    if (!el || !target) return;
    let placed = { top: -1, height: -1 };
    const place = () => {
      const top = target.getBoundingClientRect().top - el.getBoundingClientRect().top + el.scrollTop;
      el.scrollTop = Math.max(0, top - READING_CONTEXT);
      placed = { top: el.scrollTop, height: el.scrollHeight };
    };
    // The browser moves the page when content above changes size; only a
    // move with the same content is the user's.
    const onScroll = () => {
      if (el.scrollHeight === placed.height && Math.abs(el.scrollTop - placed.top) > 1) stop();
    };
    const resized = new ResizeObserver(place);
    const stop = () => {
      resized.disconnect();
      el.removeEventListener("scroll", onScroll);
      el.removeEventListener("wheel", stop);
      el.removeEventListener("pointerdown", stop);
    };
    stopHolding.current = stop;
    place();
    resized.observe(el.firstElementChild ?? el);
    el.addEventListener("scroll", onScroll, { passive: true });
    el.addEventListener("wheel", stop, { passive: true });
    el.addEventListener("pointerdown", stop);
    return stop;
  }, []); // Only when the conversation opens.

  return (
    <>
      {screening ? (
        <GatekeeperToolbar message={m} closeTip={closeTip} actions={actions} />
      ) : (
        <Toolbar
          message={m}
          closeTip={closeTip}
          summary={summary}
          onJumpToSummary={jumpToSummary}
          onExpandAll={several ? expandAll : undefined}
          onCollapseAll={several ? collapseAll : undefined}
          actions={actions}
        />
      )}
      <div
        className={`mx-3 flex items-center gap-x-2 gap-y-1 border-b px-3 pb-3 pt-2 ${
          scrolled ? "border-hairline" : "flex-wrap border-transparent"
        }`}
      >
        <h1 className={`select-text text-sm font-semibold leading-[22px] ${scrolled ? "min-w-0 truncate" : ""}`}>
          {conversation[0]?.subject || m.subject || "No subject"}
        </h1>
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
      <OverlayScroll
        className="min-h-0 flex-1"
        viewportClassName="px-3 pb-8"
        viewportRef={scrollRef}
        onScroll={(e) => {
          const el = e.currentTarget;
          // Once shrunk, it stays so until the top; it only shrinks when there's
          // more to scroll than it gives back, so it can't flicker.
          setScrolled((was) => el.scrollTop > 0 && (was || el.scrollHeight - el.clientHeight > HEADER_SLACK));
        }}
      >
        <div className="space-y-2">
          {conversation.map((x, i) => (
            <Fragment key={x.id}>
              {i === firstNew && i > 0 && <NewMessagesLine />}
              {collapsed.has(x.id) ? (
                <CollapsedCard message={x} onExpand={() => toggle(x.id)} />
              ) : (
                <MessageCard
                  message={x}
                  showReplyBar={!screening}
                  quickReplies={x === latest}
                  onCollapse={several ? () => toggle(x.id) : undefined}
                  actions={actions}
                />
              )}
            </Fragment>
          ))}
        </div>
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
      </OverlayScroll>
    </>
  );
}

/** Spark leaves about this much of the email before the one reading starts at in view. */
const READING_CONTEXT = 100;
/** More than the subject gives back when it shrinks to a line. */
const HEADER_SLACK = 48;

/** Spark's divider above the emails that were unread when the conversation opened. */
function NewMessagesLine() {
  return (
    <div role="separator" className="flex items-center gap-2 py-1 text-[11px] font-medium leading-4 text-primary">
      <span className="h-px w-20 bg-primary/40" />
      New messages
      <span className="h-px flex-1 bg-primary/40" />
    </div>
  );
}

/** An email collapsed to one line: who, the start of what they wrote, and when. */
function CollapsedCard({ message: m, onExpand }: { message: Message; onExpand: () => void }) {
  return (
    <article data-message={m.id} className="scroll-mt-2 rounded-2xl border border-hairline bg-card">
      <button
        className="flex w-full items-center gap-3 rounded-2xl px-5 py-2.5 text-left hover:bg-base-content/[0.03]"
        aria-expanded={false}
        aria-label={`${m.from}: ${m.snippet}`}
        onClick={onExpand}
      >
        <Avatar name={m.from} size="sm" account={m.account} />
        <span className="shrink-0 text-sm font-bold leading-5">{m.from}</span>
        <span className="min-w-0 flex-1 truncate text-[13px] leading-5 text-text-2">{m.snippet}</span>
        <time className="shrink-0 text-xs leading-5 text-text-2" dateTime={new Date(m.timestamp).toISOString()}>
          {formatListDate(m.timestamp)}
        </time>
      </button>
    </article>
  );
}

/** Thread toolbar: the `[← │ ✓]` segment, then the message actions. */
function Toolbar({
  message: m,
  closeTip,
  summary,
  onJumpToSummary,
  onExpandAll,
  onCollapseAll,
  actions,
}: {
  message: Message;
  closeTip: Tip;
  summary?: SummaryState;
  onJumpToSummary: () => void;
  /** Only for a conversation of several emails. */
  onExpandAll?: () => void;
  onCollapseAll?: () => void;
  actions: ThreadActions;
}) {
  const isDone = m.state === "done";
  return (
    <div role="toolbar" aria-label="Email thread toolbar" className="flex h-12 shrink-0 items-center gap-0.5 px-3">
      <div className="mr-1.5 flex h-8 items-center rounded-lg border border-hairline">
        <SegmentButton tip={closeTip} Icon={ArrowLeft} onClick={actions.close} />
        <span className="h-4 w-px bg-hairline" />
        <SegmentButton
          tip={isDone ? "Move to Inbox" : hint("Done", "done", "done")}
          Icon={isDone ? Inbox : Check}
          onClick={() => actions.done(m)}
        />
      </div>
      <ToolbarButton
        tip={hint(m.state === "deleted" ? "Delete Forever" : "Move to Trash", "trash")}
        Icon={Trash2}
        onClick={() => actions.trash(m)}
      />
      <ToolbarButton
        tip={m.priority ? hint("Remove from Priority", "priority") : hint("Mark as Priority", "priority", "priority")}
        Icon={Zap}
        active={m.priority}
        iconClass={m.priority ? "fill-current text-priority" : ""}
        onClick={() => actions.priority(m)}
      />
      <ToolbarButton
        tip={hint(m.unread ? "Mark as Read" : "Mark as Unread", "read")}
        Icon={Circle}
        active={m.unread}
        iconClass={m.unread ? "fill-current text-primary" : ""}
        onClick={() => actions.read(m)}
      />
      <ToolbarButton
        tip={m.pinned ? hint("Unpin", "pin") : hint("Pin", "pin", "pin")}
        Icon={Pin}
        active={m.pinned}
        iconClass={m.pinned ? "fill-current text-pin" : ""}
        onClick={() => actions.pin(m)}
      />
      <ToolbarButton
        tip={m.state === "aside" ? "Move back to Inbox" : hint("Set Aside", "aside", "aside")}
        Icon={ArrowDownLeft}
        active={m.state === "aside"}
        iconClass={m.state === "aside" ? "text-primary" : ""}
        onClick={() => actions.aside(m)}
      />
      <Dropdown
        trigger={({ toggle, open }) => (
          <button
            className={`btn btn-ghost btn-sm gap-0.5 px-1.5 text-text-2 hover:text-base-content ${open ? "bg-base-content/10" : ""}`}
            {...tipAttrs(hint("Snooze", "snooze", "snooze"))}
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
              data-tip="More actions"
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
          {onExpandAll && onCollapseAll && (
            <>
              <li role="none" className="my-0.5 h-px bg-hairline" />
              <MenuItem label="Expand all emails" icon={<ChevronsUpDown size={15} />} onSelect={onExpandAll} />
              <MenuItem label="Collapse all emails" icon={<ChevronsDownUp size={15} />} onSelect={onCollapseAll} />
            </>
          )}
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
          {...tipAttrs(hint("+AI Summary", "summarize", "summary"))}
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
                  {...tipAttrs(hint(label, STYLE_SHORTCUTS[key]))}
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

/** `single` is one email's menu (its card's ⋯), where Trash takes only that email. */
function MessageMenuItems({
  message: m,
  actions,
  single = false,
}: {
  message: Message;
  actions: ThreadActions;
  single?: boolean;
}) {
  const spam = actions.inSpam(m);
  const deleted = m.state === "deleted";
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
      <MenuItem
        label={spam ? "Not Spam" : "Move to Spam"}
        icon={spam ? <ShieldCheck size={15} /> : <ShieldAlert size={15} />}
        onSelect={() => actions.spam(m)}
      />
      {single && (
        <MenuItem
          label={deleted ? "Delete Forever" : "Move to Trash"}
          icon={<Trash2 size={15} />}
          danger={deleted}
          onSelect={() => actions.trashOne(m)}
        />
      )}
      {!single && spam && (
        <MenuItem label="Delete Forever" icon={<Trash2 size={15} />} danger onSelect={() => actions.deleteForever(m)} />
      )}
      <MenuItem label="Block Sender" icon={<Ban size={15} />} danger onSelect={() => actions.block(m)} />
    </>
  );
}

/** Shown instead of the toolbar for messages still in screening. */
function GatekeeperToolbar({
  message: m,
  closeTip,
  actions,
}: {
  message: Message;
  closeTip: Tip;
  actions: ThreadActions;
}) {
  return (
    <div className="shrink-0 px-3 pb-2">
      <div className="flex h-12 items-center">
        <ToolbarButton tip={closeTip} Icon={ArrowLeft} onClick={actions.close} />
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
            {...tipAttrs({ label: "Accept", card: "accept" })}
            onClick={() => actions.accept(m)}
          >
            Accept
          </button>
          <button
            className="btn btn-sm border-0 bg-error/10 px-5 text-error shadow-none hover:bg-error/20"
            {...tipAttrs({ label: "Block", card: "block" })}
            onClick={() => actions.block(m)}
          >
            Block
          </button>
        </div>
      </div>
    </div>
  );
}

/** One email: a one-line header, the body, attachments and the bottom bar. */
function MessageCard({
  message: m,
  showReplyBar,
  quickReplies,
  onCollapse,
  actions,
}: {
  message: Message;
  showReplyBar: boolean;
  /** The quick replies, under the latest email. */
  quickReplies: boolean;
  /** Clicking the header collapses the card, in a conversation of several. */
  onCollapse?: () => void;
  actions: ThreadActions;
}) {
  const account = useAccount(m.account);
  const [state, retry] = useMessageContent(m.id);
  const content = state.status === "ready" ? state.content : null;
  // As in Spark, quick replies are for personal email that someone else sent.
  const offerQuickReplies = quickReplies && m.category === "people" && !isFromYou(m, account);
  return (
    <article data-message={m.id} className="scroll-mt-2 rounded-2xl border border-hairline bg-card">
      <header
        className={`flex items-center gap-3 px-5 pb-1.5 pt-3.5 ${onCollapse ? "cursor-pointer" : ""}`}
        onClick={onCollapse}
      >
        <Avatar name={m.from} size="lg" account={m.account} />
        <div className="min-w-0 flex-1 truncate text-sm leading-5">
          <span className="font-bold" data-tip={m.address}>
            {m.from}
          </span>
          {content && <Recipients content={content} account={account} />}
        </div>
        <time className="shrink-0 text-xs leading-5 text-text-2" dateTime={new Date(m.timestamp).toISOString()}>
          {formatFullDate(m.timestamp)}
        </time>
        {/* Its clicks and menu stay out of the header's collapse. */}
        <div className="contents" onClick={(e) => e.stopPropagation()}>
          <Dropdown
            align="end"
            width="w-48"
            trigger={({ toggle, open }) => (
              <button
                className={`grid size-6 place-items-center rounded-full text-text-2 hover:bg-base-content/10 hover:text-base-content ${
                  open ? "bg-base-content/10" : ""
                }`}
                data-tip="Message actions"
                aria-label="Message actions"
                aria-haspopup="menu"
                onClick={toggle}
              >
                <Ellipsis size={16} />
              </button>
            )}
          >
            <MessageMenuItems message={m} actions={actions} single />
          </Dropdown>
        </div>
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
      {content &&
        (content.invite ? (
          <InviteBody id={m.id} content={content} account={account} />
        ) : (
          <Body id={m.id} content={content} quotes={!isForward(m.subject)} />
        ))}
      {content && content.attachments.length > 0 && (
        <AttachmentList
          attachments={content.attachments}
          onOpen={(i) => actions.openAttachment(m, i)}
          onSave={(i) => actions.saveAttachment(m, i)}
          onSaveAll={() => actions.saveAllAttachments(m)}
        />
      )}
      {showReplyBar && (
        <footer className="flex flex-wrap items-center justify-end gap-1.5 rounded-b-2xl border-t border-hairline px-4 py-2.5">
          {offerQuickReplies && (
            <>
              {QUICK_REPLIES.map(({ key, label }) => (
                <button
                  key={key}
                  className="h-7 rounded-md bg-chip px-3 text-xs font-medium text-base-content/80 hover:brightness-95"
                  onClick={() => actions.quickReply(m, key)}
                >
                  {label}
                </button>
              ))}
              <span className="mx-1 h-5 w-px bg-hairline" />
            </>
          )}
          <ToolbarButton tip={hint("Reply", "reply")} Icon={Reply} onClick={() => actions.reply(m)} />
          <ToolbarButton tip={hint("Forward", "forward")} Icon={Forward} onClick={() => actions.forward(m)} />
        </footer>
      )}
    </article>
  );
}

/**
 * The email's HTML in the sandboxed frame (embedded images fetched, remote
 * ones on request), else its text. A reply's quote folds away unless `quotes`
 * is off, as it is for a forward, whose quote is the point.
 */
function Body({ id, content, quotes }: { id: number; content: MessageContent; quotes: boolean }) {
  const [images, setImages] = useState(() => remoteImagesAllowed(id));
  const html = content.html;
  const blocked = useMemo(() => html !== null && hasRemoteImages(html), [html]);
  const embeds = useMemo(() => html !== null && hasInlineImages(html), [html]);
  const inlineImages = useInlineImages(id, embeds);
  if (html === null) return <TextBody paragraphs={content.text} quotes={quotes} />;
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
      <HtmlBody html={html} remoteImages={images} inlineImages={inlineImages} quotes={quotes} />
    </>
  );
}

/**
 * An invite email: the event's card in place of the body, which mostly
 * repeats it, so the email itself folds away behind "⋯" as the related message.
 */
function InviteBody({ id, content, account }: { id: number; content: MessageContent; account?: AccountView }) {
  const [related, setRelated] = useState(false);
  const hasBody = content.html !== null || content.text.some((p) => p.trim() !== "");
  return (
    <>
      <InviteCard id={id} invite={content.invite!} account={account} />
      {hasBody &&
        (related ? (
          <>
            <Body id={id} content={content} quotes />
            <button className="mx-5 mb-4 text-xs font-medium text-primary hover:underline" onClick={() => setRelated(false)}>
              Hide related message
            </button>
          </>
        ) : (
          <button
            className="mx-5 mb-4 grid h-5 w-8 place-items-center rounded-full bg-chip text-text-2 hover:brightness-95 hover:text-base-content"
            data-tip="Related message"
            aria-label="Show related message"
            onClick={() => setRelated(true)}
          >
            <Ellipsis size={14} />
          </button>
        ))}
    </>
  );
}

/** "Fwd:", "FW:", "WG:" (German), "TR:" (French), "RV:" (Spanish), "ENC:" (Portuguese). */
function isForward(subject: string) {
  return /^\s*(?:fwd?|wg|tr|rv|enc)\s*:/i.test(subject);
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
            data-tip={you && account ? `${account.email} (${account.label})` : raw}
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

function SegmentButton({ tip, Icon, onClick }: { tip: Tip; Icon: LucideIcon; onClick: () => void }) {
  return (
    <button
      className="grid h-full w-8 place-items-center text-text-2 first:rounded-l-lg last:rounded-r-lg hover:bg-base-content/5 hover:text-base-content"
      {...tipAttrs(tip)}
      aria-label={tipLabel(tip)}
      onClick={onClick}
    >
      <Icon size={17} />
    </button>
  );
}

function ToolbarButton({
  tip,
  Icon,
  onClick,
  active = false,
  iconClass = "",
}: {
  tip: Tip;
  Icon: LucideIcon;
  onClick: () => void;
  active?: boolean;
  iconClass?: string;
}) {
  return (
    <button
      className="btn btn-ghost btn-sm btn-square text-text-2 hover:text-base-content"
      {...tipAttrs(tip)}
      aria-label={tipLabel(tip)}
      aria-pressed={active || undefined}
      onClick={onClick}
    >
      <Icon size={18} className={iconClass} />
    </button>
  );
}

function Pill({ className, children }: { className: string; children: ReactNode }) {
  return (
    <span className={`inline-flex h-[22px] shrink-0 items-center gap-1 rounded-full px-2 text-xs font-medium ${className}`}>
      {children}
    </span>
  );
}
