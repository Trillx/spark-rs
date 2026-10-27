import {
  ArrowLeft,
  Check,
  CheckCheck,
  ChevronDown,
  CircleCheck,
  FolderSearch,
  History,
  Pen,
  Search,
  SearchX,
  ThumbsDown,
  ThumbsUp,
  X,
} from "lucide-react";
import {
  useCallback,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type KeyboardEvent,
  type ReactNode,
  type RefObject,
  type UIEvent,
} from "react";
import { flushSync } from "react-dom";
import type { Message } from "../types";
import { plural } from "../lib/format";
import {
  INBOX_MODES,
  type CardKey,
  type GroupKey,
  type InboxMode,
  type ListBlock,
  type ListModel,
  type ListView,
} from "../lib/layout";
import { LIST_MAX, LIST_MIN, startHorizontalDrag } from "../lib/panes";
import { LIST_ITEMS, renderForPaging } from "../lib/scroll";
import { hint, shortcutLabel } from "../lib/shortcuts";
import {
  anchoredTop,
  blockOffsets,
  blocksBetween,
  covers,
  gaps,
  joinSpans,
  reaches,
  stretchAround,
  type Span,
  type Stretch,
} from "../lib/virtualList";
import Dropdown, { MenuItem } from "./Dropdown";
import MessageRow, { ROW_HEIGHT, type RowActions } from "./MessageRow";
import OverlayScroll from "./OverlayScroll";
import { GatekeeperCard, GroupRow, SmartInboxCard, type CardSettings } from "./SmartInboxCard";
import { tipAttrs } from "../lib/tooltip";

export interface GatekeeperActions {
  /** One sender's waiting mail, from their card. */
  acceptSender: (items: Message[]) => void;
  blockSender: (items: Message[]) => void;
  /** Everyone waiting; these ask first. */
  acceptAll: (items: Message[]) => void;
  blockAll: (items: Message[]) => void;
}

interface Props {
  model: ListModel;
  view: ListView;
  selectedId: number | null;
  openId: number | null;
  /** Rows selected for a batch action. */
  picked: ReadonlySet<number>;
  rows: RowActions;
  gatekeeper: GatekeeperActions;
  onDoneAll: (items: Message[]) => void;
  onShowAll: (key: CardKey) => void;
  onOpenGroup: (key: GroupKey) => void;
  onBack: () => void;
  onModeChange: (mode: InboxMode) => void;
  onSearchOpen: () => void;
  onSearchChange: (query: string) => void;
  onSearchClose: () => void;
  onSearchKeyDown: (e: KeyboardEvent<HTMLInputElement>) => void;
  onCompose: () => void;
  onToggleDone: () => void;
  onOpenGatekeeper: () => void;
  searchRef: RefObject<HTMLInputElement | null>;
  /** Only the demo accounts exist: a notice offers to add real ones. */
  demo: boolean;
  onAddAccounts: () => void;
  /** The list's server folders haven't synced yet. */
  loading: boolean;
  /** Saves the search as a smart folder. */
  onSaveSearch: (query: string) => void;
  /** The element that scrolls, for keyboard paging. */
  scrollRef: RefObject<HTMLDivElement | null>;
  /** Set when the list can reach further back on the servers. */
  older: { loading: boolean; onNearEnd: () => void } | null;
  /** The search is still being put to the mail servers. */
  searchingServers: boolean;
  /** Set for lists that can be emptied (Trash, Spam, folders): deletes all their mail for good. */
  onEmpty?: () => void;
  /** A card's menu: how many rows it shows, and turning it off. */
  cardSettings: (key: CardKey) => CardSettings;
  /** Past searches, newest first, offered while the field is empty. */
  recentSearches: string[];
  onForgetSearch: (query: string) => void;
  /** The account a search is narrowed to, by label; null for all of them. */
  searchScope: string | null;
  onSearchAllAccounts: () => void;
}

const SEARCH_TIPS = [
  { query: "from:", example: "from:marcus", text: "Emails from a sender" },
  { query: "has:attachment ", example: "has:attachment", text: "Emails with attachments" },
  { query: "is:unread ", example: "is:unread", text: "Unread emails" },
];

export default function MessageList(props: Props) {
  const { model, view, selectedId, rows, scrollRef, older } = props;
  const searching = view.search !== null;
  /** The cursor's row: it may be on any email of the conversation the row stands for. */
  const selectedRow = selectedId === null ? null : (model.rowOf.get(selectedId) ?? selectedId);
  const listId = listIdOf(view);
  useScrollPlace(scrollRef, listId, view);
  const blocks = useBlockViewport(scrollRef, listId, model.blocks);
  useRowSlide(scrollRef, blocks, listId, model.order);
  const reachAtEnd = useOlderAtEnd(scrollRef, listId, model, older);
  useCursorInView(scrollRef, blocks, selectedRow, model.order);
  const row = (m: Message, inPriorityCard = false) => (
    <MessageRow
      key={m.id}
      message={m}
      count={model.threads.get(m.id)?.count}
      names={model.threads.get(m.id)?.names}
      selected={m.id === selectedRow}
      picked={props.picked.has(m.id)}
      actions={rows}
      inPriorityCard={inPriorityCard}
      recipients={model.recipients}
    />
  );
  /** A block other than a row, in a box that holds its margins, so it can be measured. */
  const boxed = (b: Exclude<ListBlock, { kind: "row" }>, key: string, content: ReactNode) => {
    const size = sizeKey(b);
    return (
      <div key={key} ref={blocks.measure(size)} data-size={size} className="flow-root">
        {content}
      </div>
    );
  };
  const block = (b: ListBlock, i: number) => {
    switch (b.kind) {
      case "row":
        return row(b.message);
      case "period":
        return boxed(
          b,
          `p-${b.title}`,
          <div data-period={b.title} className="flex h-10 items-end px-3 pb-2 text-[13px] text-text-2">
            {b.title}
          </div>,
        );
      case "card":
        return boxed(
          b,
          b.key,
          <SmartInboxCard
            cardKey={b.key}
            items={b.items}
            shown={b.shown}
            renderRow={row}
            onCompleteAll={() => props.onDoneAll(b.items)}
            onShowAll={() => props.onShowAll(b.key)}
            settings={props.cardSettings(b.key)}
          />,
        );
      case "gatekeeper":
        return boxed(
          b,
          "gatekeeper",
          <GatekeeperCard
            items={b.items}
            layout={b.layout}
            openId={props.openId}
            onOpen={rows.open}
            onShowAll={props.onOpenGatekeeper}
            onAccept={props.gatekeeper.acceptSender}
            onBlock={props.gatekeeper.blockSender}
            onAcceptAll={() => props.gatekeeper.acceptAll(b.items)}
            onBlockAll={() => props.gatekeeper.blockAll(b.items)}
          />,
        );
      case "group":
        return boxed(
          b,
          `g-${b.key}-${i}`,
          <GroupRow
            groupKey={b.key}
            items={b.items}
            selected={b.items[0].id === selectedRow}
            picked={props.picked.has(b.items[0].id)}
            onOpen={() => props.onOpenGroup(b.key)}
            onPick={(range) => rows.pick(b.items[0].id, range)}
          />,
        );
    }
  };

  return (
    <>
      <Toolbar {...props} />
      {props.demo && <DemoNotice onAddAccounts={props.onAddAccounts} />}
      <OverlayScroll
        role="listbox"
        aria-label={model.title}
        className="min-h-0 flex-1"
        viewportClassName="px-2 pb-8"
        // Clear of the list/reader splitter's grab zone.
        barClassName="right-1.5"
        viewportRef={scrollRef}
        onScroll={reachAtEnd}
      >
        {searching && !view.search!.trim() ? (
          <SearchTips
            recent={props.recentSearches}
            onPick={(q) => props.onSearchChange(q)}
            onForget={props.onForgetSearch}
          />
        ) : searching && model.found === 0 ? (
          props.searchingServers ? (
            <SearchingServers />
          ) : (
            <>
              {props.searchScope && <SearchScope {...props} />}
              <NoResults />
            </>
          )
        ) : (
          <>
            {searching && (
              <div className="flex h-9 items-center gap-2 px-3 text-[13px] text-text-2">
                {plural(model.found ?? 0, "email")} found
                {props.searchScope && <SearchScope {...props} inline />}
                {props.searchingServers && (
                  <span
                    className="loading loading-spinner loading-xs text-text-3"
                    aria-label="Searching your mail servers"
                  />
                )}
              </div>
            )}
            <div ref={blocks.container}>{rendered(blocks, block)}</div>
            {!searching && model.blocks.length === 0 && (
              <EmptyList inbox={view.folder === "inbox"} loading={props.loading} />
            )}
            {older?.loading && model.blocks.length > 0 && (
              <div className="flex h-14 items-center justify-center">
                <span className="loading loading-spinner loading-sm text-text-3" aria-label="Loading older emails" />
              </div>
            )}
          </>
        )}
      </OverlayScroll>
    </>
  );
}

/** Lists that keep their own scroll position. Typing a search keeps the same list. */
function listIdOf(view: ListView) {
  const sub = view.sub ? `${view.sub.kind}:${"key" in view.sub ? view.sub.key : ""}` : "";
  return [view.folder, view.account, view.mode, sub, view.search === null ? "" : "search"].join("|");
}

/** The list a sub-list or search goes back to. */
function parentOf(view: ListView): ListView | null {
  if (view.search !== null) return { ...view, search: null };
  if (view.sub) return { ...view, sub: null };
  return null;
}

/**
 * Like Spark, a list opens at the top, except the Inbox, which stays where it
 * was left, and the list that a card's "Show all", a group or a search goes
 * back to.
 */
function useScrollPlace(scrollRef: RefObject<HTMLDivElement | null>, listId: string, view: ListView) {
  const places = useRef(new Map<string, number>());
  const shown = useRef({ id: listId, view });
  useLayoutEffect(() => {
    const el = scrollRef.current;
    if (!el) return;
    const save = () => places.current.set(shown.current.id, el.scrollTop);
    el.addEventListener("scroll", save, { passive: true });
    return () => el.removeEventListener("scroll", save);
  }, [scrollRef]);
  useLayoutEffect(() => {
    const el = scrollRef.current;
    const prev = shown.current;
    if (!el || prev.id === listId) return;
    shown.current = { id: listId, view };
    const parent = parentOf(prev.view);
    const back = parent !== null && listIdOf(parent) === listId;
    const home = view.folder === "inbox" && !view.sub && view.search === null;
    el.scrollTop = back || home ? (places.current.get(listId) ?? 0) : 0;
  }, [listId]);
}

/** A block by what it shows: a row by its message id, the others by their kind and key. */
type BlockKey = number | string;

function blockKey(block: ListBlock): BlockKey {
  switch (block.kind) {
    case "row":
      return block.message.id;
    case "period":
      return `period:${block.title}`;
    case "group":
      return `group:${block.key}`;
    case "card":
      return `card:${block.key}`;
    case "gatekeeper":
      return "gatekeeper";
  }
}

/** Blocks that look alike share a measured height: date headers, group rows, cards with as many rows. */
function sizeKey(block: Exclude<ListBlock, { kind: "row" }>) {
  switch (block.kind) {
    case "period":
    case "group":
      return block.kind;
    case "card": {
      const rows = Math.min(block.shown, block.items.length);
      return `card:${block.key}:${rows}:${rows < block.items.length}`;
    }
    case "gatekeeper":
      return `gatekeeper:${block.layout}`;
  }
}

/**
 * A block's height until it has been measured, from its classes: a card's
 * header, rows, "Show all" and margin (the Priority card has no border, and
 * "Show all" only when rows are left out); New senders' cards are 138px.
 */
function estimate(block: Exclude<ListBlock, { kind: "row" }>) {
  switch (block.kind) {
    case "period":
      return 40;
    case "group":
      return 60;
    case "card": {
      const shown = Math.min(block.shown, block.items.length);
      const rows = shown * ROW_HEIGHT;
      if (block.key !== "priority") return 2 + 52 + rows + 40 + 10;
      return 44 + 5 + rows + (shown < block.items.length ? 40 : 5) + 10;
    }
    case "gatekeeper":
      return block.layout === "strip" ? 40 + 138 + 4 + 10 : 8 + Math.ceil(block.items.length / 2) * 146;
  }
}

/** Where a list's blocks go. */
interface Layout {
  blocks: ListBlock[];
  /** Where each block starts, then where the last one ends. */
  offsets: number[];
  /** Each block's index by `blockKey`; a card's rows give the card's, a group row's id in `order` the group's. */
  index: Map<BlockKey, number>;
  /** The heights given to blocks that get measured, by `sizeKey`. */
  sized: Map<string, number>;
}

function layoutOf(blocks: ListBlock[], measured: ReadonlyMap<string, number>): Layout {
  const heights = new Array<number>(blocks.length);
  const index = new Map<BlockKey, number>();
  const sized = new Map<string, number>();
  blocks.forEach((block, i) => {
    index.set(blockKey(block), i);
    if (block.kind === "row") {
      heights[i] = ROW_HEIGHT;
      return;
    }
    if (block.kind === "card") for (const m of block.items.slice(0, block.shown)) index.set(m.id, i);
    if (block.kind === "group") index.set(block.items[0].id, i);
    const size = sizeKey(block);
    heights[i] = measured.get(size) ?? estimate(block);
    sized.set(size, heights[i]);
  });
  return { blocks, offsets: blockOffsets(heights), index, sized };
}

const totalOf = (layout: Layout) => layout.offsets[layout.offsets.length - 1];

interface BlockViewport {
  layout: Layout;
  /** The layout on screen: the last one, until this render commits. */
  committed: () => Layout;
  /** Holds the blocks: where it starts in the scroller is where they do. */
  container: RefObject<HTMLDivElement | null>;
  /** The blocks rendered. */
  spans: Span[];
  /** The ref that measures a block of this `sizeKey`. */
  measure: (size: string) => (el: HTMLDivElement | null) => (() => void) | undefined;
  /** A row rendered wherever it is, for the cursor to scroll to. */
  pinned: number | null;
  pin: (id: number | null) => void;
  /** Renders around the screen again if it's near the edge of what's rendered. */
  ensure: () => void;
}

/**
 * Renders the blocks within a screen of the one that shows, with empty space
 * as tall as the rest, so a long list costs no more than a short one to show
 * or change. Rows are 70px; the other blocks are measured once rendered.
 */
function useBlockViewport(
  scrollRef: RefObject<HTMLDivElement | null>,
  listId: string,
  blocks: ListBlock[],
): BlockViewport {
  const sizes = useRef(new Map<string, number>());
  const [measured, setMeasured] = useState(0);
  const layout = useMemo(() => layoutOf(blocks, sizes.current), [blocks, measured]);
  const [stretch, setStretch] = useState<Stretch>(() => stretchAround(0, window.innerHeight));
  const [pinned, pin] = useState<number | null>(null);
  const container = useRef<HTMLDivElement>(null);
  /** What's on screen, as of the last commit. */
  const shown = useRef({ layout, stretch, listId });

  // When blocks come or go above the screen, the browser keeps the first one
  // that shows in its place (scroll anchoring), as long as it's still
  // rendered: so it is, with what will show around it.
  let anchored: Span | undefined;
  const before = shown.current;
  const scroller = scrollRef.current;
  if (before.layout !== layout && before.listId === listId && scroller && container.current) {
    const top = scroller.scrollTop - container.current.offsetTop;
    const at = anchoredTop(before.layout.offsets, layout.offsets, top, (i) => {
      const key = blockKey(before.layout.blocks[i]);
      const j = layout.index.get(key);
      return j !== undefined && blockKey(layout.blocks[j]) === key ? j : undefined;
    });
    if (at !== undefined) {
      const around = stretchAround(at, scroller.clientHeight);
      anchored = blocksBetween(layout.offsets, around.from, around.to);
    }
  }
  const single = (i: number | undefined) => (i === undefined ? [] : [{ start: i, end: i + 1 }]);
  const spans = joinSpans([
    blocksBetween(layout.offsets, stretch.from, stretch.to),
    ...(anchored ? [anchored] : []),
    // New senders stay rendered, so they stay collapsed if they were.
    ...single(layout.index.get("gatekeeper")),
    ...single(pinned === null ? undefined : layout.index.get(pinned)),
  ]);

  useLayoutEffect(() => {
    shown.current = { layout, stretch, listId };
  });

  const ensure = useCallback(
    (now = false) => {
      const el = scrollRef.current;
      const box = container.current;
      if (!el || !box) return;
      const { layout, stretch } = shown.current;
      const top = el.scrollTop - box.offsetTop;
      if (covers(stretch, top, el.clientHeight, totalOf(layout))) return;
      const next = stretchAround(top, el.clientHeight);
      // Scrolling renders before the screen is painted.
      if (now) flushSync(() => setStretch(next));
      else setStretch(next);
    },
    [scrollRef],
  );

  const observer = useRef<ResizeObserver | null>(null);
  const resizes = () =>
    (observer.current ??= new ResizeObserver((entries) => {
      let changed = false;
      for (const { target } of entries) {
        if (!(target instanceof HTMLElement) || target === scrollRef.current) continue;
        const size = target.dataset.size!;
        sizes.current.set(size, target.offsetHeight);
        if (shown.current.layout.sized.get(size) !== target.offsetHeight) changed = true;
      }
      if (changed) setMeasured((n) => n + 1);
      else ensure();
    }));
  // One ref per size key: a block whose rows change is measured again.
  const refs = useRef(new Map<string, (el: HTMLDivElement | null) => (() => void) | undefined>());
  const measure = (size: string) => {
    let ref = refs.current.get(size);
    if (!ref) {
      ref = (el) => {
        if (!el) return;
        const ro = resizes();
        ro.observe(el);
        return () => ro.unobserve(el);
      };
      refs.current.set(size, ref);
    }
    return ref;
  };

  useLayoutEffect(() => {
    const el = scrollRef.current;
    if (!el) return;
    const onScroll = () => ensure(true);
    el.addEventListener("scroll", onScroll, { passive: true });
    // A taller screen needs more rendered.
    resizes().observe(el);
    const stopPaging = renderForPaging(el, (from, to) => {
      const box = container.current;
      if (!box) return;
      const { layout, stretch } = shown.current;
      const wanted = { from: from - box.offsetTop, to: to - box.offsetTop };
      if (!reaches(stretch, wanted.from, wanted.to, totalOf(layout))) flushSync(() => setStretch(wanted));
    });
    return () => {
      el.removeEventListener("scroll", onScroll);
      stopPaging();
      observer.current?.disconnect();
      observer.current = null;
    };
  }, [scrollRef]);

  // A list that opened at its place, or blocks that changed under the screen.
  useLayoutEffect(() => ensure(), [listId, layout]);

  return { layout, committed: () => shown.current.layout, container, spans, measure, pinned, pin, ensure };
}

/** The rendered blocks, with empty space standing in for the others. */
function rendered({ layout, spans }: BlockViewport, block: (b: ListBlock, i: number) => ReactNode) {
  const space = gaps(layout.offsets, spans);
  const out: ReactNode[] = [];
  spans.forEach(({ start, end }, k) => {
    out.push(<Gap key={`gap-${k}`} height={space[k]} />);
    for (let i = start; i < end; i++) out.push(block(layout.blocks[i], i));
  });
  out.push(<Gap key="gap-end" height={space[spans.length]} />);
  return out;
}

/** Room for blocks that aren't rendered; never what the browser keeps in place as content changes. */
function Gap({ height }: { height: number }) {
  return <div aria-hidden className="[overflow-anchor:none]" style={{ height }} />;
}

/** A row by its message id, a group row or a date header by what it shows, as `blockKey` has them. */
function itemKey(el: HTMLElement): BlockKey {
  const { row, group, period } = el.dataset;
  return row !== undefined ? Number(row) : group !== undefined ? `group:${group}` : `period:${period}`;
}

/** Where each rendered row, group row and date header is on screen. */
function itemTops(scroller: HTMLElement) {
  const tops = new Map<BlockKey, number>();
  for (const el of scroller.querySelectorAll<HTMLElement>(LIST_ITEMS)) {
    tops.set(itemKey(el), el.getBoundingClientRect().top);
  }
  return tops;
}

/** Spark moves rows in 200ms. */
const SLIDE_MS = 200;

/** The list before a change: where its rendered items were on screen, and the layout for the rest. */
interface Before {
  tops: Map<BlockKey, number>;
  layout: Layout;
  /** Where the blocks started on screen. */
  origin: number;
}

/** Where a row, group row or date header that wasn't rendered was on screen; not a row in a card. */
function placeBefore(was: Before, key: BlockKey) {
  const i = was.layout.index.get(key);
  if (i === undefined || blockKey(was.layout.blocks[i]) !== key) return undefined;
  return was.origin + was.layout.offsets[i];
}

/**
 * When rows leave the list (Done, Delete, Snooze…) and none arrive, what's
 * after them slides up into place instead of jumping. Where things were is
 * read while rendering, before React changes the DOM, and what wasn't
 * rendered was where the layout had it.
 */
function useRowSlide(
  scrollRef: RefObject<HTMLDivElement | null>,
  blocks: BlockViewport,
  listId: string,
  order: number[],
) {
  const committed = useRef({ order, listId });
  const before = useRef<Before | null>(null);
  if (committed.current.order !== order && before.current === null && scrollRef.current) {
    before.current = {
      tops: itemTops(scrollRef.current),
      layout: blocks.committed(),
      origin: blocks.container.current?.getBoundingClientRect().top ?? 0,
    };
  }
  const { index } = blocks.layout;
  useLayoutEffect(() => {
    const was = before.current;
    const sameList = committed.current.listId === listId;
    const gone = committed.current.order;
    before.current = null;
    committed.current = { order, listId };
    const el = scrollRef.current;
    if (!was || !el || !sameList) return;
    // Rows, a card's included, as the DOM had them when every row was rendered.
    const removed = gone.some((id) => !index.has(id));
    const added = order.some((id) => !was.layout.index.has(id));
    if (!removed || added) return;
    const now = itemTops(el);
    const box = el.getBoundingClientRect();
    for (const item of el.querySelectorAll<HTMLElement>(LIST_ITEMS)) {
      const key = itemKey(item);
      const from = was.tops.get(key) ?? placeBefore(was, key);
      const to = now.get(key)!;
      if (from === undefined || Math.abs(from - to) < 1) continue;
      // Only what's on screen before or after.
      if (Math.max(from, to) < box.top - 100 || Math.min(from, to) > box.bottom) continue;
      item.animate([{ transform: `translateY(${from - to}px)` }, { transform: "none" }], {
        duration: SLIDE_MS,
        easing: "ease-out",
      });
    }
  }, [order, listId]);
}

/** Spark reaches further back once the first row that shows is within 1.5 screens of the end. */
function nearEnd(el: HTMLElement) {
  return el.scrollHeight - el.scrollTop < 1.5 * el.clientHeight;
}

/**
 * Scrolling checks for the end (the returned handler); so does a list that
 * changed and grew, which may still not fill the screen. One that came back
 * the same height doesn't try again on its own, so a reach that found nothing
 * to show waits for the next scroll.
 */
function useOlderAtEnd(
  scrollRef: RefObject<HTMLDivElement | null>,
  listId: string,
  model: ListModel,
  older: Props["older"],
) {
  const tried = useRef({ listId: "", height: 0 });
  const reach = (el: HTMLElement, scrolled: boolean) => {
    if (!older || older.loading || !nearEnd(el)) return;
    if (!scrolled && tried.current.listId === listId && tried.current.height === el.scrollHeight) return;
    tried.current = { listId, height: el.scrollHeight };
    older.onNearEnd();
  };
  const loading = older?.loading ?? false;
  useLayoutEffect(() => {
    if (scrollRef.current) reach(scrollRef.current, false);
  }, [model, listId, older !== null, loading]);
  return (e: UIEvent<HTMLElement>) => reach(e.currentTarget, true);
}

/**
 * Like Spark, only a row the cursor moves to scrolls into view: not one that
 * was clicked (it's under the pointer), nor one already selected when its
 * list opens, which keeps the list's place, nor one that's new to the list.
 * Group rows, which the cursor reaches by their newest email, go by the same
 * rule. A row that isn't rendered is rendered first.
 */
function useCursorInView(
  scrollRef: RefObject<HTMLDivElement | null>,
  blocks: BlockViewport,
  selectedRow: number | null,
  order: number[],
) {
  const { layout, pinned, pin, ensure } = blocks;
  const shown = useRef({ row: selectedRow, order });
  const reveal = (id: number) => {
    const i = layout.index.get(id);
    const b = i === undefined ? undefined : layout.blocks[i];
    const selector = b?.kind === "group" ? `[data-group="${CSS.escape(b.key)}"]` : `[data-row="${id}"]`;
    const el = scrollRef.current?.querySelector<HTMLElement>(selector);
    if (!el) return pin(id !== pinned && layout.index.has(id) ? id : null);
    if (!el.matches(":hover")) el.scrollIntoView({ block: "nearest" });
    pin(null);
    ensure();
  };
  useLayoutEffect(() => {
    const was = shown.current;
    shown.current = { row: selectedRow, order };
    if (selectedRow !== null && selectedRow !== was.row && was.order.includes(selectedRow)) reveal(selectedRow);
  });
  useLayoutEffect(() => {
    if (pinned !== null) reveal(pinned);
  }, [pinned]);
}

function Toolbar({
  model,
  view,
  gatekeeper,
  onBack,
  onDoneAll,
  onToggleDone,
  onModeChange,
  onSearchOpen,
  onSearchChange,
  onSearchClose,
  onSearchKeyDown,
  onCompose,
  onSaveSearch,
  onEmpty,
  searchRef,
}: Props) {
  const iconButton = "btn btn-ghost btn-sm btn-square text-text-2 hover:text-base-content";

  if (view.search !== null) {
    return (
      <div role="toolbar" className="flex min-h-[58px] shrink-0 items-center gap-1 border-b border-hairline px-2">
        <button className={iconButton} {...tipAttrs({ label: "Close search", keys: "Esc" })} aria-label="Close search" onClick={onSearchClose}>
          <ArrowLeft size={18} />
        </button>
        <label className="flex h-10 flex-1 items-center gap-2 rounded-md border border-hairline px-2.5 focus-within:border-2 focus-within:border-[#81beff] focus-within:px-[9px]">
          <Search size={16} className="shrink-0 text-text-2" />
          <input
            ref={searchRef}
            type="text"
            spellCheck={false}
            placeholder="Search emails…"
            className="min-w-0 flex-1 bg-transparent text-sm leading-[26px] outline-none placeholder:text-text-3"
            value={view.search}
            onChange={(e) => onSearchChange(e.target.value)}
            onKeyDown={onSearchKeyDown}
          />
          {view.search && (
            <button
              aria-label="Clear search"
              className="grid size-5 place-items-center rounded-full text-text-2 hover:bg-base-content/10"
              onClick={() => {
                onSearchChange("");
                searchRef.current?.focus();
              }}
            >
              <X size={14} />
            </button>
          )}
        </label>
        {view.search.trim() && (
          <button
            className={iconButton}
            data-tip="Save as Smart Folder"
            aria-label="Save as Smart Folder"
            onClick={() => onSaveSearch(view.search!.trim())}
          >
            <FolderSearch size={18} />
          </button>
        )}
      </div>
    );
  }

  const modeLabel = INBOX_MODES.find((m) => m.key === view.mode)!.label;
  return (
    <div role="toolbar" className="flex min-h-[58px] shrink-0 items-center gap-1 border-b border-hairline pl-2 pr-2">
      {view.sub ? (
        <>
          <button className={iconButton} {...tipAttrs({ label: "Back", keys: "Esc" })} aria-label="Back" onClick={onBack}>
            <ArrowLeft size={18} />
          </button>
          <h2 className="min-w-0 truncate pl-1 text-base font-semibold">{model.title}</h2>
          {view.sub.kind === "gatekeeper" && screening(model).length > 0 && (
            <div className="ml-auto flex items-center gap-1">
              <button
                className={iconButton}
                data-tip="Accept all new senders"
                aria-label="Accept all new senders"
                onClick={() => gatekeeper.acceptAll(screening(model))}
              >
                <ThumbsUp size={17} />
              </button>
              <button
                className={iconButton}
                data-tip="Block all new senders"
                aria-label="Block all new senders"
                onClick={() => gatekeeper.blockAll(screening(model))}
              >
                <ThumbsDown size={17} />
              </button>
            </div>
          )}
          {model.bulk.length > 0 && (
            <button
              className="btn btn-ghost btn-sm ml-1 gap-1.5 px-2 font-medium text-primary"
              onClick={() => onDoneAll(model.bulk)}
            >
              <CheckCheck size={16} />
              Done all
            </button>
          )}
        </>
      ) : view.folder === "inbox" ? (
        <Dropdown
          trigger={({ toggle, open }) => (
            <button
              className={`flex h-10 items-center gap-1.5 rounded-lg px-2 text-left hover:bg-row-hover ${open ? "bg-row-hover" : ""}`}
              onClick={toggle}
              aria-haspopup="menu"
              aria-expanded={open}
            >
              <span>
                <span className="block text-base font-semibold leading-5">{model.title}</span>
                <span className="block text-[11px] leading-[14px] text-text-2">{modeLabel}</span>
              </span>
              <ChevronDown size={14} className="text-text-2" />
            </button>
          )}
        >
          {INBOX_MODES.map((m) => (
            <MenuItem
              key={m.key}
              label={m.label}
              detail={shortcutLabel(m.action)}
              checked={view.mode === m.key}
              onSelect={() => onModeChange(m.key)}
            />
          ))}
        </Dropdown>
      ) : (
        <h2 className="truncate px-2 text-base font-semibold">{model.title}</h2>
      )}

      {view.sub?.kind !== "gatekeeper" && (
        <div className="ml-auto flex items-center gap-1">
          {view.folder === "inbox" && !view.sub && <DoneSwitch on={view.showDone} onToggle={onToggleDone} />}
          {onEmpty && !view.sub && model.order.length > 0 && (
            <button
              className="btn btn-ghost btn-sm px-2 font-medium text-error"
              data-tip="Delete every email in it for good"
              onClick={onEmpty}
            >
              Empty
            </button>
          )}
          <button className={iconButton} {...tipAttrs(hint("Search", "search"))} aria-label="Search" onClick={onSearchOpen}>
            <Search size={18} />
          </button>
          <button className={iconButton} {...tipAttrs(hint("Compose", "compose"))} aria-label="Compose" onClick={onCompose}>
            <Pen size={17} />
          </button>
        </div>
      )}
    </div>
  );
}

function DemoNotice({ onAddAccounts }: { onAddAccounts: () => void }) {
  return (
    <div className="flex shrink-0 flex-wrap items-center gap-x-1.5 border-b border-hairline px-4 py-1.5 text-xs leading-5 text-text-2">
      You're looking at demo mail.
      <button className="font-medium text-primary hover:underline" onClick={onAddAccounts}>
        Add your accounts
      </button>
    </div>
  );
}

/** The messages on the New senders page, for its Accept all / Block all. */
function screening(model: ListModel) {
  return model.blocks.flatMap((b) => (b.kind === "gatekeeper" ? b.items : []));
}

/** Spark's show/hide Done switch: a small track with a check in the knob. */
function DoneSwitch({ on, onToggle }: { on: boolean; onToggle: () => void }) {
  const title = hint(on ? "Hide emails marked as Done" : "Show emails marked as Done", "toggleDone");
  return (
    <button
      role="switch"
      aria-checked={on}
      aria-label="Show emails marked as Done"
      {...tipAttrs(title)}
      onClick={onToggle}
      className="grid h-8 w-10 place-items-center rounded-md hover:bg-base-content/5"
    >
      <span
        className={`relative h-4 w-7 rounded-full transition-colors ${on ? "bg-primary" : "bg-base-content/20"}`}
      >
        <span
          className={`absolute top-0.5 grid size-3 place-items-center rounded-full bg-white shadow-sm transition-[left] ${
            on ? "left-[14px] text-primary" : "left-0.5 text-text-3"
          }`}
        >
          <Check size={8} strokeWidth={3} />
        </span>
      </span>
    </button>
  );
}

/** "in Work · Search all accounts", for a search narrowed to one account. */
function SearchScope({ searchScope, onSearchAllAccounts, inline = false }: Props & { inline?: boolean }) {
  return (
    <span className={inline ? "contents" : "flex h-9 items-center gap-2 px-3 text-[13px] text-text-2"}>
      in {searchScope} ·
      <button className="font-medium text-primary hover:underline" onClick={onSearchAllAccounts}>
        Search all accounts
      </button>
    </span>
  );
}

function SearchTips({
  recent,
  onPick,
  onForget,
}: {
  recent: string[];
  onPick: (query: string) => void;
  onForget: (query: string) => void;
}) {
  return (
    <div className="px-1 pt-3">
      {recent.length > 0 && (
        <>
          <div className="px-2 pb-1 text-[13px] text-text-2">Recent searches</div>
          {recent.map((q) => (
            <div key={q} className="group flex h-9 items-center rounded-lg pl-2 pr-1 hover:bg-row-hover">
              <button className="flex min-w-0 flex-1 items-center gap-3 text-left text-[13px]" onClick={() => onPick(q)}>
                <History size={14} className="shrink-0 text-text-3" />
                <span className="truncate">{q}</span>
              </button>
              <button
                className="hidden size-6 place-items-center rounded-full text-text-2 hover:bg-base-content/10 group-hover:grid"
                aria-label={`Remove “${q}” from recent searches`}
                data-tip="Remove"
                onClick={() => onForget(q)}
              >
                <X size={13} />
              </button>
            </div>
          ))}
          <div className="h-2" />
        </>
      )}
      <div className="px-2 pb-1 text-[13px] text-text-2">Search tips</div>
      {SEARCH_TIPS.map((tip) => (
        <button
          key={tip.example}
          className="flex h-10 w-full items-center gap-3 rounded-lg px-2 text-left text-[13px] hover:bg-row-hover"
          onClick={() => onPick(tip.query)}
        >
          <code className="rounded bg-token-contact px-1.5 py-0.5 font-sans text-xs font-medium">{tip.example}</code>
          <span className="text-text-2">{tip.text}</span>
        </button>
      ))}
    </div>
  );
}

function SearchingServers() {
  return (
    <div className="flex flex-col items-center px-6 pt-20 text-center text-sm text-text-2">
      <span className="loading loading-spinner loading-md text-text-3" />
      <p className="mt-3">Searching your mail servers…</p>
    </div>
  );
}

function NoResults() {
  return (
    <div className="flex flex-col items-center px-6 pt-16 text-center">
      <span className="grid size-24 place-items-center rounded-full bg-base-200 text-text-3">
        <SearchX size={40} />
      </span>
      <h3 className="mt-5 text-xl font-semibold">No emails found</h3>
      <ul className="mt-3 space-y-1.5 text-left text-[13px] text-text-2">
        {["Try fewer search filters", "Try other word combinations", "Check the spelling"].map((t) => (
          <li key={t} className="flex items-center gap-2">
            <CircleCheck size={14} className="text-success" />
            {t}
          </li>
        ))}
      </ul>
    </div>
  );
}

function EmptyList({ inbox, loading }: { inbox: boolean; loading: boolean }) {
  if (loading) {
    return (
      <div className="flex flex-col items-center px-6 pt-20 text-center text-sm text-text-2">
        <span className="loading loading-spinner loading-md text-text-3" />
        <p className="mt-3">Loading this folder…</p>
      </div>
    );
  }
  return (
    <div className="flex flex-col items-center px-6 pt-20 text-center">
      <span className="grid size-24 place-items-center rounded-full bg-primary/10 text-primary">
        <CheckCheck size={40} />
      </span>
      <h3 className="mt-5 text-xl font-semibold">{inbox ? "Great job!" : "Nothing here"}</h3>
      <p className="mt-1 text-sm text-text-2">{inbox ? "Everything here is done." : "This folder is empty."}</p>
    </div>
  );
}

/** List/reader splitter: 12px hit zone; the grip grows and turns blue on hover. */
export function Splitter({ width, onResize }: { width: number; onResize: (width: number) => void }) {
  return (
    <div
      role="separator"
      aria-orientation="vertical"
      aria-label="Resize message list"
      aria-valuemin={LIST_MIN}
      aria-valuemax={LIST_MAX}
      aria-valuenow={width}
      data-tip="Drag to resize, double-click to reset"
      className="group absolute inset-y-0 -right-1.5 z-10 flex w-3 cursor-col-resize items-center justify-center"
      onPointerDown={(e) => startHorizontalDrag(e, (dx) => onResize(width + dx))}
      onDoubleClick={() => onResize(LIST_MIN)}
    >
      <span className="h-[19px] w-px rounded-sm bg-splitter transition-all duration-150 group-hover:h-[31px] group-hover:w-[3px] group-hover:bg-primary" />
    </div>
  );
}
