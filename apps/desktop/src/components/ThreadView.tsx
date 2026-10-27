import {
  AlignLeft,
  ArrowDownLeft,
  ArrowLeft,
  Ban,
  Bell,
  BellOff,
  Check,
  ChevronDown,
  ChevronsDownUp,
  ChevronsUpDown,
  Circle,
  Clock,
  Copy,
  Ellipsis,
  FileDown,
  FolderInput,
  Forward,
  Group,
  Inbox,
  ListChecks,
  MailX,
  Pin,
  Printer,
  Reply,
  ReplyAll,
  Search,
  Send,
  ShieldAlert,
  ShieldCheck,
  Tags,
  Text,
  TextSearch,
  ThumbsDown,
  ThumbsUp,
  Trash2,
  Zap,
  type LucideIcon,
} from "lucide-react";
import { Fragment, useEffect, useLayoutEffect, useMemo, useRef, useState, type ReactNode, type RefObject } from "react";
import { QUICK_REPLIES, SUMMARY_STYLES, type QuickReply, type SummaryStyle } from "../ai";
import { senderStatus } from "../api";
import type { AccountView, Category, Message, MessageContent, SenderStatus } from "../types";

const CATEGORIES: { key: Category; label: string }[] = [
  { key: "people", label: "People" },
  { key: "notifications", label: "Notifications" },
  { key: "newsletters", label: "Newsletters" },
];
import { isFromYou, useAccount } from "../lib/accounts";
import { readingStart } from "../lib/conversation";
import { decidableDomain, domainOf } from "../lib/gatekeeper";
import { shownFromCache, useInlineImages, useMessageContent, type ContentState } from "../lib/content";
import { allowRemoteImages, useRemoteImagesAllowed } from "../lib/remoteImages";
import {
  laterPresets,
  formatFullDate,
  formatLater,
  formatListDate,
  parseAddress,
  presetDetail,
  type LaterPreset,
} from "../lib/format";
import {
  LOADING_LIMIT,
  OVERSCAN,
  READING_CONTEXT,
  bodyScheduler,
  movedByUser,
  overlaps,
  scrollToPlace,
  type BodyScheduler,
  type Place,
} from "../lib/readerViewport";
import { hint, shortcutLabel, type ActionId } from "../lib/shortcuts";
import { tipAttrs, tipLabel, type Tip } from "../lib/tooltip";
import AiMark from "./AiMark";
import OverlayScroll from "./OverlayScroll";
import { AttachmentList } from "./Attachments";
import Avatar from "./Avatar";
import Dropdown, { MenuItem, Popover } from "./Dropdown";
import FindBar from "./FindBar";
import InviteCard from "./InviteCard";
import { HtmlBody, TextBody, hasInlineImages, type KeptBody } from "./MessageBody";
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
  /** Adds the sender to the Priority senders, or takes them off. */
  prioritySender: (m: Message) => void;
  /** Whether the sender is a Priority sender; null for the user's own mail. */
  senderPriority: (m: Message) => boolean | null;
  priority: (m: Message) => void;
  read: (m: Message) => void;
  pin: (m: Message) => void;
  aside: (m: Message) => void;
  snooze: (m: Message, preset: LaterPreset) => void;
  /** Asks for a day and time to snooze until. */
  snoozeCustom: (m: Message) => void;
  unsnooze: (m: Message) => void;
  /** Mutes or unmutes its conversation. */
  mute: (m: Message) => void;
  unsubscribe: (m: Message) => void;
  /** Searches for the sender's mail. */
  searchSender: (m: Message) => void;
  /** Your sent email as a new one, to send again. */
  sendAgain: (m: Message) => void;
  /** Opens Move to folder. */
  move: (m: Message) => void;
  /** Files the sender's mail under another category. */
  changeCategory: (m: Message, category: Category) => void;
  /** Group by Sender / Domain, or ungroup. */
  groupSender: (m: Message) => void;
  groupDomain: (m: Message) => void;
  grouping: (m: Message) => { sender: boolean; domain?: string; domainGrouped: boolean };
  summarize: (m: Message, style: SummaryStyle) => void;
  stopSummary: (m: Message) => void;
  deleteSummary: (m: Message) => void;
  summaryCopied: () => void;
  openAiSettings: () => void;
  /** Opens the settings page with Load remote images. */
  openImageSettings: () => void;
  reply: (m: Message) => void;
  /** Opens a reply that AI writes for the chip. */
  quickReply: (m: Message, chip: QuickReply) => void;
  replyAll: (m: Message) => void;
  forward: (m: Message) => void;
  /** The Gatekeeper's decisions about `m`'s sender, or everyone at its domain, for every account. */
  accept: (m: Message) => void;
  /** Accepts the sender and shows the email in the Inbox. */
  acceptAndOpen: (m: Message) => void;
  /** Accepts the sender and marks the mail they had waiting Done. */
  acceptAndDone: (m: Message) => void;
  acceptDomain: (m: Message) => void;
  block: (m: Message) => void;
  blockDomain: (m: Message) => void;
  /** Prints the email, or with `all` its whole conversation. */
  print: (m: Message, all: boolean) => void;
  /** Copies the email, or with `all` its conversation, as Markdown. */
  copyMarkdown: (m: Message, all: boolean) => void;
  copyAddresses: (m: Message) => void;
  /** Saves the email as an .eml file. */
  saveEml: (m: Message) => void;
  /** Spark's Auto-download: the email's files, fetched ahead as it shows (if that's on). */
  prefetchAttachments: (m: Message) => void;
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
 * Expanded cards only mount their bodies near the screen (`useBodies`), so a
 * long conversation opens as quickly as a short one.
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
  findRequest = 0,
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
  /** Goes up each time Find in Conversation is asked for (Ctrl+Shift+F, the Command Center). */
  findRequest?: number;
}) {
  const screening = m.screening && m.state === "inbox";
  const blocked = m.state === "blocked";
  /** A blocked sender's email stays hidden until asked for, as in Spark. */
  const [shown, setShown] = useState(false);
  const summaryRef = useRef<HTMLElement>(null);
  /** Stops holding the place reading starts at (or the summary jumped to), once the user moves around. */
  const stopHolding = useRef(() => {});
  /** Scrolled down: the subject shrinks to a line over a border, like Spark's header. */
  const [scrolled, setScrolled] = useState(false);
  const closeTip: Tip = singleColumn ? { label: "Back to list", keys: "Esc" } : hint("Close", "close");
  const jumpToSummary = () => {
    stopHolding.current();
    const el = scrollRef.current;
    const card = summaryRef.current;
    if (!el || !card) return;
    // Bodies that mount on the way, or around it, can move it; once the scroll
    // ends it's held in view like the place reading starts at, unless the user
    // has taken over (the scrollbar is beside `el`, so its parent listens).
    const area = el.parentElement ?? el;
    const arrive = () => {
      letGo();
      card.scrollIntoView({ block: "nearest" });
      stopHolding.current = hold(el, card, card.getBoundingClientRect().top - el.getBoundingClientRect().top);
    };
    const letGo = () => {
      el.removeEventListener("scrollend", arrive);
      area.removeEventListener("wheel", letGo);
      area.removeEventListener("pointerdown", letGo);
      window.removeEventListener("keydown", letGo, true);
    };
    el.addEventListener("scrollend", arrive);
    area.addEventListener("wheel", letGo, { passive: true });
    area.addEventListener("pointerdown", letGo);
    window.addEventListener("keydown", letGo, true);
    stopHolding.current = letGo;
    card.scrollIntoView({ block: "nearest", behavior: "smooth" });
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

  /** Search in conversation: open, and asked for again (its field takes focus). */
  const [finding, setFinding] = useState(false);
  const [findFocus, setFindFocus] = useState(0);

  // Reading starts at the first expanded email, below the ones collapsed above
  // it, with a little of the one before still in view. Like Spark, that place
  // holds as bodies load and grow, until the user scrolls or clicks.
  useLayoutEffect(() => {
    const el = scrollRef.current;
    const target =
      start === undefined || start === conversation[0]?.id
        ? null
        : el?.querySelector<HTMLElement>(`[data-message="${start}"]`);
    if (el && target) stopHolding.current = hold(el, target, READING_CONTEXT);
    return () => stopHolding.current();
  }, []); // Only when the conversation opens.

  // Its effects run after the one above, so cards are placed by hand where reading starts.
  // While Find is open every expanded body mounts, so it can search them all.
  const [{ mounted, reloading }, bodies] = useBodies(
    scrollRef,
    conversation.filter((x) => !collapsed.has(x.id)).map((x) => x.id),
    start,
    finding,
  );

  const toggle = (id: number) => {
    // An email the user expands shows at once, without waiting its turn.
    if (collapsed.has(id)) bodies.mount(id);
    setCollapsed((c) => {
      const next = new Set(c);
      if (!next.delete(id)) next.add(id);
      return next;
    });
  };
  const expandAll = () => {
    stopHolding.current();
    setCollapsed(new Set());
  };
  const find = () => {
    // It searches what shows: every email.
    expandAll();
    setFinding(true);
    setFindFocus((n) => n + 1);
  };
  // A request made while this conversation is open; one from before it opened is done.
  const findSeen = useRef(findRequest);
  useEffect(() => {
    if (findRequest === findSeen.current) return;
    findSeen.current = findRequest;
    find();
  }, [findRequest]);
  /** Like Spark, the latest email stays open. */
  const collapseAll = () => {
    stopHolding.current();
    setCollapsed(new Set(conversation.filter((x) => x !== latest).map((x) => x.id)));
  };

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
          onFind={find}
          several={several}
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
        {m.muted && (
          <Pill className="bg-base-200 text-text-2">
            <BellOff size={12} /> Muted
          </Pill>
        )}
        {m.category === "newsletters" && !scrolled && (
          <button
            className="ml-auto shrink-0 text-xs font-medium text-primary hover:underline"
            onClick={() => actions.unsubscribe(m)}
          >
            Unsubscribe
          </button>
        )}
      </div>
      {blocked && <BlockedSenderCard message={m} actions={actions} />}
      {finding && <FindBar root={scrollRef} focus={findFocus} onClose={() => setFinding(false)} />}
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
        {blocked && !shown ? (
          <div className="grid place-items-center gap-2 rounded-2xl border border-dashed border-hairline px-4 py-8 text-[13px] text-text-2">
            Message is hidden
            <button className="btn btn-ghost btn-sm h-8 rounded-lg text-primary" onClick={() => setShown(true)}>
              Show message
            </button>
          </div>
        ) : (
          <div className="space-y-2">
            {conversation.map((x, i) => (
              <Fragment key={x.id}>
                {i === firstNew && i > 0 && <NewMessagesLine />}
                {collapsed.has(x.id) ? (
                  <CollapsedCard message={x} onExpand={() => toggle(x.id)} />
                ) : (
                  <MessageCard
                    message={x}
                    mounted={mounted.has(x.id)}
                    reload={reloading.has(x.id)}
                    bodies={bodies}
                    showReplyBar={!screening}
                    quickReplies={x === latest}
                    onCollapse={several ? () => toggle(x.id) : undefined}
                    actions={actions}
                  />
                )}
              </Fragment>
            ))}
          </div>
        )}
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

/** More than the subject gives back when it shrinks to a line. */
const HEADER_SLACK = 48;

/**
 * Holds `target` `offset` below the top of the reader `el` while bodies mount,
 * load and grow, until the user scrolls or clicks. Returns how to let go.
 */
function hold(el: HTMLElement, target: Element, offset: number): () => void {
  let placed = { top: -1, height: -1 };
  const place = () => {
    const top = target.getBoundingClientRect().top - el.getBoundingClientRect().top;
    el.scrollTop = scrollToPlace(el.scrollTop, top, offset);
    placed = { top: el.scrollTop, height: el.scrollHeight };
  };
  const onScroll = () => {
    if (movedByUser(placed, { top: el.scrollTop, height: el.scrollHeight })) stop();
  };
  const resized = new ResizeObserver(place);
  const stop = () => {
    resized.disconnect();
    el.removeEventListener("scroll", onScroll);
    el.removeEventListener("wheel", stop);
    el.removeEventListener("pointerdown", stop);
  };
  place();
  resized.observe(el.firstElementChild ?? el);
  el.addEventListener("scroll", onScroll, { passive: true });
  el.addEventListener("wheel", stop, { passive: true });
  el.addEventListener("pointerdown", stop);
  return stop;
}

/** What cards tell the reader about their bodies (see `useBodies`). */
interface Bodies {
  /** Ref for a card's article, watched for coming near the screen. */
  card: (el: HTMLElement) => () => void;
  /** Ref for a mounted body, measured so its placeholder can take its height. */
  body: (el: HTMLDivElement) => () => void;
  /**
   * A body's state, as one report from whatever knows all of it: the card
   * while its content loads or fails, then what shows the content, with the
   * embedded images it fetches. Either says whether what it shows is stale,
   * which `useMessageContent` knows. Null comes only as the body unmounts, so
   * a change never lets its turn go in between.
   */
  content: BodyScheduler["content"];
  mount: BodyScheduler["mount"];
  height: BodyScheduler["height"];
  /** Starts watching the cards in the reader `root`; returns how to stop. */
  watch: (root: HTMLElement) => () => void;
  /** After each commit: the expanded cards (with `all`, every body is to mount), and where any new ones are. */
  update: (cards: readonly number[], all: boolean) => void;
}

/**
 * What a card's body keeps while it's out of view and unmounted, so it comes
 * back as it was. It lasts while the card is expanded, as the body's own
 * state did before bodies unmounted.
 */
interface Kept extends KeptBody {
  /** An invite's related message, while it's showing: what its body keeps. */
  related?: KeptBody;
  /** Auto-download was asked for its files, so a remount doesn't ask again. */
  prefetched?: boolean;
}

/** The cards whose bodies are mounted, and the stale ones of those that may load again (see `useBodies`). */
interface Turns {
  mounted: ReadonlySet<number>;
  reloading: ReadonlySet<number>;
}

/**
 * Mounts the bodies of the expanded `cards` (oldest first) near the reader's
 * screen, a few loads at a time, as lib/readerViewport decides; the card
 * reading starts at mounts straight away. With `all` (Find in Conversation
 * open) every one mounts, still a few loads at a time, and stays. A mounted
 * body gone stale loads again in its turn among those loads. Returns the
 * cards whose bodies are mounted and the ones that may load again, and what
 * cards use to report.
 */
function useBodies(
  scrollRef: RefObject<HTMLDivElement | null>,
  cards: readonly number[],
  start: number | undefined,
  all: boolean,
): [Turns, Bodies] {
  const [turns, setTurns] = useState<Turns>(() => ({
    mounted: new Set(start === undefined ? [] : [start]),
    reloading: new Set(),
  }));
  const [bodies] = useState(() =>
    readerBodies(
      bodyScheduler(
        start,
        (mounted, reloading) => setTurns({ mounted, reloading }),
        LOADING_LIMIT,
        (id) => shownFromCache(id, hasInlineImages),
      ),
    ),
  );
  useLayoutEffect(() => bodies.watch(scrollRef.current!), [bodies, scrollRef]);
  useLayoutEffect(() => bodies.update(cards, all));
  return [turns, bodies];
}

/**
 * The DOM side of `useBodies`: observers that tell the scheduler where cards
 * are and how tall their bodies were. A card that appears is placed by hand
 * after the commit that adds it, as observers only report after the next
 * paint, so the first paint already shows the bodies near the screen.
 */
function readerBodies(scheduler: BodyScheduler): Bodies {
  const cards = new Map<Element, number>();
  const bodies = new Map<Element, number>();
  const fresh = new Set<HTMLElement>();
  let root: HTMLElement | null = null;
  let watching: { near: IntersectionObserver; onScreen: IntersectionObserver; sizes: ResizeObserver } | null = null;
  const places = (entries: IntersectionObserverEntry[], place: (id: number, on: boolean) => Place) =>
    entries.flatMap((e) => {
      const id = cards.get(e.target);
      return id === undefined ? [] : [place(id, e.isIntersecting)];
    });

  return {
    card(el) {
      const id = Number(el.dataset.message);
      cards.set(el, id);
      fresh.add(el);
      watching?.near.observe(el);
      watching?.onScreen.observe(el);
      return () => {
        cards.delete(el);
        fresh.delete(el);
        watching?.near.unobserve(el);
        watching?.onScreen.unobserve(el);
        scheduler.forget(id);
      };
    },
    body(el) {
      bodies.set(el, Number(el.closest<HTMLElement>("[data-message]")?.dataset.message));
      watching?.sizes.observe(el);
      return () => {
        bodies.delete(el);
        watching?.sizes.unobserve(el);
      };
    },
    content: scheduler.content,
    mount: scheduler.mount,
    height: scheduler.height,
    watch(el) {
      root = el;
      const near = new IntersectionObserver(
        (entries) => scheduler.place(places(entries, (id, on) => ({ id, near: on }))),
        { root: el, rootMargin: `${OVERSCAN * 100}% 0px` },
      );
      const onScreen = new IntersectionObserver(
        (entries) => scheduler.place(places(entries, (id, on) => ({ id, visible: on }))),
        { root: el },
      );
      const sizes = new ResizeObserver((entries) => {
        for (const e of entries) {
          const id = bodies.get(e.target);
          if (id !== undefined) scheduler.measure(id, e.borderBoxSize[0]?.blockSize ?? e.contentRect.height);
        }
      });
      for (const card of cards.keys()) {
        near.observe(card);
        onScreen.observe(card);
      }
      for (const body of bodies.keys()) sizes.observe(body);
      watching = { near, onScreen, sizes };
      return () => {
        near.disconnect();
        onScreen.disconnect();
        sizes.disconnect();
        watching = null;
        root = null;
      };
    },
    update(expanded, all) {
      const placed: Place[] = [];
      if (root) {
        const screen = root.getBoundingClientRect();
        for (const el of fresh) {
          const box = el.getBoundingClientRect();
          const near = overlaps(box, screen, screen.height * OVERSCAN);
          placed.push({ id: cards.get(el)!, near, visible: overlaps(box, screen) });
        }
        fresh.clear();
      }
      scheduler.cards(expanded, all);
      if (placed.length > 0) scheduler.place(placed);
    },
  };
}

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
  onFind,
  several,
  actions,
}: {
  message: Message;
  closeTip: Tip;
  summary?: SummaryState;
  onJumpToSummary: () => void;
  /** Only for a conversation of several emails. */
  onExpandAll?: () => void;
  onCollapseAll?: () => void;
  onFind: () => void;
  /** The conversation has more than this email. */
  several: boolean;
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
        {m.state === "snoozed" && <MenuItem label="Discard Snooze" onSelect={() => actions.unsnooze(m)} />}
        {laterPresets().map((p) => (
          <MenuItem key={p.key} label={p.label} detail={presetDetail(p)} onSelect={() => actions.snooze(m, p)} />
        ))}
        <MenuItem label="Pick date & time…" onSelect={() => actions.snoozeCustom(m)} />
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
          <li role="none" className="my-0.5 h-px bg-hairline" />
          <MenuItem
            label="Find in Conversation"
            detail={shortcutLabel("findInThread")}
            icon={<TextSearch size={15} />}
            onSelect={onFind}
          />
          <MenuItem
            label={several ? "Print Conversation" : "Print"}
            icon={<Printer size={15} />}
            onSelect={() => actions.print(m, true)}
          />
          <MenuItem
            label={several ? "Copy Conversation as Markdown" : "Copy as Markdown"}
            icon={<Copy size={15} />}
            onSelect={() => actions.copyMarkdown(m, true)}
          />
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
  const senderPriority = actions.senderPriority(m);
  const yours = isFromYou(m, useAccount(m.account));
  const grouping = actions.grouping(m);
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
      {yours && <MenuItem label="Send Again" icon={<Send size={15} />} onSelect={() => actions.sendAgain(m)} />}
      <MenuItem label="Move to Folder…" detail={shortcutLabel("move")} icon={<FolderInput size={15} />} onSelect={() => actions.move(m)} />
      {!yours &&
        CATEGORIES.filter((c) => c.key !== m.category).map((c) => (
          <MenuItem
            key={c.key}
            label={`Change Category to ${c.label}`}
            icon={<Tags size={15} />}
            onSelect={() => actions.changeCategory(m, c.key)}
          />
        ))}
      {!yours && (
        <MenuItem
          label={grouping.sender ? "Ungroup Emails from Sender" : "Group Emails by Sender"}
          icon={<Group size={15} />}
          onSelect={() => actions.groupSender(m)}
        />
      )}
      {!yours && grouping.domain && (
        <MenuItem
          label={grouping.domainGrouped ? `Ungroup Emails from ${grouping.domain}` : `Group Emails from ${grouping.domain}`}
          icon={<Group size={15} />}
          onSelect={() => actions.groupDomain(m)}
        />
      )}
      <MenuItem label="Search Emails by Sender" icon={<Search size={15} />} onSelect={() => actions.searchSender(m)} />
      {!single && (
        <MenuItem
          label={m.muted ? "Unmute Conversation" : "Mute Conversation"}
          icon={m.muted ? <Bell size={15} /> : <BellOff size={15} />}
          onSelect={() => actions.mute(m)}
        />
      )}
      {m.category === "newsletters" && (
        <MenuItem label="Unsubscribe" icon={<MailX size={15} />} onSelect={() => actions.unsubscribe(m)} />
      )}
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
      {senderPriority !== null && (
        <MenuItem
          label={senderPriority ? "Remove Sender from Priority" : "Mark Sender as Priority"}
          icon={<Zap size={15} />}
          onSelect={() => actions.prioritySender(m)}
        />
      )}
      {single && (
        <>
          <li role="none" className="my-0.5 h-px bg-hairline" />
          <MenuItem label="Print Message" icon={<Printer size={15} />} onSelect={() => actions.print(m, false)} />
          <MenuItem label="Save as .eml…" icon={<FileDown size={15} />} onSelect={() => actions.saveEml(m)} />
          <MenuItem label="Copy as Markdown" icon={<Copy size={15} />} onSelect={() => actions.copyMarkdown(m, false)} />
          <MenuItem label="Copy All Email Addresses" icon={<Copy size={15} />} onSelect={() => actions.copyAddresses(m)} />
        </>
      )}
      <SenderMenuItems message={m} actions={actions} />
    </>
  );
}

/** Block (or, for blocked mail, Accept) the sender, and everyone at their domain unless anyone can sign up there. */
function SenderMenuItems({ message: m, actions }: { message: Message; actions: ThreadActions }) {
  const domain = decidableDomain(m.address);
  if (m.state === "blocked") {
    return (
      <>
        <MenuItem label="Accept Sender" detail={shortcutLabel("acceptSender")} icon={<ThumbsUp size={15} />} onSelect={() => actions.accept(m)} />
        {domain && (
          <MenuItem label={`Accept all from ${domain}`} icon={<ThumbsUp size={15} />} onSelect={() => actions.acceptDomain(m)} />
        )}
      </>
    );
  }
  return (
    <>
      <MenuItem
        label="Block Sender"
        detail={shortcutLabel("blockSender")}
        icon={<Ban size={15} />}
        danger
        onSelect={() => actions.block(m)}
      />
      {domain && (
        <MenuItem label={`Block all from ${domain}`} icon={<Ban size={15} />} danger onSelect={() => actions.blockDomain(m)} />
      )}
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
  const domain = decidableDomain(m.address);
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
          <SplitButton
            label="Accept"
            tip={{ label: "Accept", keys: shortcutLabel("acceptSender"), card: "accept" }}
            fill="bg-primary/10 text-primary"
            hover="hover:bg-primary/20"
            onClick={() => actions.accept(m)}
            menu="Accept options"
          >
            <MenuItem label="Accept and Open" detail={shortcutLabel("acceptAndOpen")} onSelect={() => actions.acceptAndOpen(m)} />
            <MenuItem label="Accept and Done" detail={shortcutLabel("done")} onSelect={() => actions.acceptAndDone(m)} />
            {domain && (
              <MenuItem
                label={`Accept all from ${domain}`}
                detail={shortcutLabel("acceptDomain")}
                onSelect={() => actions.acceptDomain(m)}
              />
            )}
          </SplitButton>
          <SplitButton
            label="Block"
            tip={{ label: "Block", keys: shortcutLabel("blockSender"), card: "block" }}
            fill="bg-error/10 text-error"
            hover="hover:bg-error/20"
            onClick={() => actions.block(m)}
            menu={domain ? "Block options" : undefined}
          >
            {domain && (
              <MenuItem
                label={`Block all from ${domain}`}
                detail={shortcutLabel("blockDomain")}
                danger
                onSelect={() => actions.blockDomain(m)}
              />
            )}
          </SplitButton>
        </div>
      </div>
    </div>
  );
}

/** A button with a chevron for its other choices, like Spark's Accept and Block. */
function SplitButton({
  label,
  tip,
  fill,
  hover,
  onClick,
  menu,
  children,
}: {
  label: string;
  tip: Tip;
  fill: string;
  hover: string;
  onClick: () => void;
  /** The chevron's name; without one there's only the button. */
  menu?: string;
  children: ReactNode;
}) {
  return (
    <div className={`flex h-8 items-stretch rounded-lg text-[13px] font-medium ${fill}`}>
      <button className={`px-4 ${menu ? "rounded-l-lg" : "rounded-lg"} ${hover}`} {...tipAttrs(tip)} onClick={onClick}>
        {label}
      </button>
      {menu && (
        <Dropdown
          align="end"
          width="w-60"
          trigger={({ toggle, open }) => (
            <button
              className={`grid h-full w-7 place-items-center rounded-r-lg border-l border-current/15 ${hover} ${open ? "brightness-95" : ""}`}
              aria-label={menu}
              aria-haspopup="menu"
              aria-expanded={open}
              onClick={toggle}
            >
              <ChevronDown size={14} />
            </button>
          )}
        >
          {children}
        </Dropdown>
      )}
    </div>
  );
}

/** Over a blocked sender's email: who's blocked (them, or their domain), and accepting them. */
function BlockedSenderCard({ message: m, actions }: { message: Message; actions: ThreadActions }) {
  const [status, setStatus] = useState<SenderStatus | null>(null);
  useEffect(() => {
    let current = true;
    void senderStatus(m.address).then(
      (s) => current && setStatus(s),
      () => {},
    );
    return () => {
      current = false;
    };
  }, [m.address, m.state]);
  const byDomain = status !== null && status.sender === null && status.domain === "blocked";
  const domain = domainOf(m.address);
  return (
    <div className="mx-3 mb-2 flex flex-wrap items-center gap-4 rounded-2xl border border-hairline px-4 py-3.5">
      <Avatar name={m.from} size="lg" />
      <div className="min-w-0 flex-1">
        <span className="inline-block rounded-full bg-error/10 px-2 text-[11px] font-medium leading-[18px] text-error">
          {byDomain ? "Blocked domain" : "Blocked sender"}
        </span>
        <div className="mt-1 text-[13px] leading-5 text-text-2">
          {byDomain ? (
            <>
              You blocked everyone at <span className="font-medium text-base-content">{domain}</span>. Accept{" "}
              {m.address} to see their mail in the Inbox.
            </>
          ) : (
            <>
              You blocked <span className="font-medium text-base-content">{m.address}</span>. Their mail goes to
              Blocked.
            </>
          )}
        </div>
      </div>
      <div className="flex gap-2">
        <button
          className="btn btn-sm h-8 rounded-lg border-0 bg-primary/10 px-4 text-primary shadow-none hover:bg-primary/20"
          {...tipAttrs({ label: "Accept Sender", keys: shortcutLabel("acceptSender") })}
          onClick={() => actions.accept(m)}
        >
          Accept Sender
        </button>
        {byDomain && (
          <button
            className="btn btn-sm h-8 rounded-lg border-0 bg-primary/10 px-4 text-primary shadow-none hover:bg-primary/20"
            onClick={() => actions.acceptDomain(m)}
          >
            Accept {domain}
          </button>
        )}
      </div>
    </div>
  );
}

/**
 * One email: a one-line header, the body, attachments and the bottom bar.
 * Until its body mounts, and once it unmounts far off screen, the body is a
 * loading placeholder of the height it last had.
 */
function MessageCard({
  message: m,
  mounted,
  reload,
  bodies,
  showReplyBar,
  quickReplies,
  onCollapse,
  actions,
}: {
  message: Message;
  /** Whether the body is mounted (see `useBodies`). */
  mounted: boolean;
  /** Whether the body, stale, may load its content again now (see `useBodies`). */
  reload: boolean;
  bodies: Bodies;
  showReplyBar: boolean;
  /** The quick replies, under the latest email. */
  quickReplies: boolean;
  /** Clicking the header collapses the card, in a conversation of several. */
  onCollapse?: () => void;
  actions: ThreadActions;
}) {
  const account = useAccount(m.account);
  const [kept] = useState<Kept>(() => ({}));
  // As in Spark, quick replies are for personal email that someone else sent.
  const offerQuickReplies = quickReplies && m.category === "people" && !isFromYou(m, account);
  return (
    <article ref={bodies.card} data-message={m.id} className="scroll-mt-2 rounded-2xl border border-hairline bg-card">
      {mounted ? (
        <CardContent
          message={m}
          reload={reload}
          account={account}
          bodies={bodies}
          kept={kept}
          onCollapse={onCollapse}
          actions={actions}
        />
      ) : (
        <>
          <CardHeader message={m} account={account} content={null} onCollapse={onCollapse} actions={actions} />
          <div className="flow-root overflow-hidden" style={{ height: bodies.height(m.id) }}>
            <BodySkeleton />
          </div>
        </>
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

/** A card with its body mounted: the header with recipients, then the content as it loads, and attachments. */
function CardContent({
  message: m,
  reload,
  account,
  bodies,
  kept,
  onCollapse,
  actions,
}: {
  message: Message;
  /** Whether its content, stale, may load again now (see `useBodies`). */
  reload: boolean;
  account?: AccountView;
  bodies: Bodies;
  kept: Kept;
  onCollapse?: () => void;
  actions: ThreadActions;
}) {
  const [state, stale, retry] = useMessageContent(m.id, reload);
  const content = state.status === "ready" ? state.content : null;
  // Ready, the content is reported by what shows it (see `Bodies`).
  useLayoutEffect(() => {
    if (state.status !== "ready") bodies.content(m.id, state.status, false, stale);
  }, [bodies, m.id, state.status, stale]);
  useLayoutEffect(() => () => bodies.content(m.id, null), [bodies, m.id]);
  const hasFiles = (content?.attachments.length ?? 0) > 0;
  useEffect(() => {
    if (!hasFiles || kept.prefetched) return;
    kept.prefetched = true;
    actions.prefetchAttachments(m);
  }, [hasFiles]);
  return (
    <>
      <CardHeader message={m} account={account} content={content} onCollapse={onCollapse} actions={actions} />
      {/* Measured for its placeholder (flow-root keeps the margins inside); a
          body loading again after the cache let it go keeps that height. */}
      <div
        ref={bodies.body}
        className="flow-root"
        style={{ minHeight: state.status === "loading" ? bodies.height(m.id) : undefined }}
      >
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
            <InviteBody
              message={m}
              content={content}
              stale={stale}
              account={account}
              bodies={bodies}
              kept={kept}
              onImageSettings={actions.openImageSettings}
            />
          ) : (
            <Body
              message={m}
              content={content}
              stale={stale}
              quotes={!isForward(m.subject)}
              bodies={bodies}
              kept={kept}
              onImageSettings={actions.openImageSettings}
            />
          ))}
        {content && content.attachments.length > 0 && (
          <AttachmentList
            attachments={content.attachments}
            onOpen={(i) => actions.openAttachment(m, i)}
            onSave={(i) => actions.saveAttachment(m, i)}
            onSaveAll={() => actions.saveAllAttachments(m)}
          />
        )}
      </div>
    </>
  );
}

/** A card's one-line header: who, to whom once the content has loaded, when, and the email's menu. */
function CardHeader({
  message: m,
  account,
  content,
  onCollapse,
  actions,
}: {
  message: Message;
  account?: AccountView;
  content: MessageContent | null;
  onCollapse?: () => void;
  actions: ThreadActions;
}) {
  return (
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
  );
}

/**
 * The email's HTML in the sandboxed frame (embedded images fetched, remote
 * ones as Load remote images says, or once the user shows them), else its
 * text. A reply's quote folds away unless `quotes` is off, as it is for a
 * forward, whose quote is the point.
 */
function Body({
  message: m,
  content,
  stale,
  quotes,
  bodies,
  kept,
  onImageSettings,
}: {
  message: Message;
  content: MessageContent;
  /** Whether `content` is stale: it reports that too (see `Bodies`). */
  stale: boolean;
  quotes: boolean;
  bodies: Bodies;
  kept: KeptBody;
  onImageSettings: () => void;
}) {
  const images = useRemoteImagesAllowed(m);
  const html = content.html;
  const embeds = useMemo(() => html !== null && hasInlineImages(html), [html]);
  const inlineImages = useInlineImages(m.id, embeds);
  // Fetching embedded images takes a turn among the reader's loads, as the
  // content did: the content is ready, and its images are what's loading.
  const fetching = embeds && inlineImages === null;
  useLayoutEffect(() => bodies.content(m.id, "ready", fetching, stale), [bodies, m.id, fetching, stale]);
  if (html === null) return <TextBody paragraphs={content.text} quotes={quotes} kept={kept} />;
  return (
    <HtmlBody
      html={html}
      remoteImages={images}
      inlineImages={inlineImages}
      quotes={quotes}
      kept={kept}
      onShowImages={() => allowRemoteImages(m)}
      onImageSettings={onImageSettings}
    />
  );
}

/**
 * An invite email: the event's card in place of the body, which mostly
 * repeats it, so the email itself folds away behind "⋯" as the related message.
 */
function InviteBody({
  message: m,
  content,
  stale,
  account,
  bodies,
  kept,
  onImageSettings,
}: {
  message: Message;
  content: MessageContent;
  /** Whether `content` is stale: it reports that too (see `Bodies`). */
  stale: boolean;
  account?: AccountView;
  bodies: Bodies;
  kept: Kept;
  onImageSettings: () => void;
}) {
  const [related, setRelated] = useState(kept.related !== undefined);
  const showRelated = (shown: boolean) => {
    kept.related = shown ? {} : undefined;
    setRelated(shown);
  };
  const hasBody = content.html !== null || content.text.some((p) => p.trim() !== "");
  // Showing, the related message reports the content, with its embedded images.
  const bodyShown = hasBody && related;
  useLayoutEffect(() => {
    if (!bodyShown) bodies.content(m.id, "ready", false, stale);
  }, [bodies, m.id, bodyShown, stale]);
  return (
    <>
      <InviteCard id={m.id} invite={content.invite!} account={account} />
      {hasBody &&
        (related ? (
          <>
            <Body
              message={m}
              content={content}
              stale={stale}
              quotes
              bodies={bodies}
              kept={kept.related ?? {}}
              onImageSettings={onImageSettings}
            />
            <button className="mx-5 mb-4 text-xs font-medium text-primary hover:underline" onClick={() => showRelated(false)}>
              Hide related message
            </button>
          </>
        ) : (
          <button
            className="mx-5 mb-4 grid h-5 w-8 place-items-center rounded-full bg-chip text-text-2 hover:brightness-95 hover:text-base-content"
            data-tip="Related message"
            aria-label="Show related message"
            onClick={() => showRelated(true)}
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
