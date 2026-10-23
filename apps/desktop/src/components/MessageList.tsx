import {
  ArrowLeft,
  Check,
  CheckCheck,
  ChevronDown,
  CircleCheck,
  FolderSearch,
  Pen,
  Search,
  SearchX,
  ThumbsDown,
  ThumbsUp,
  X,
} from "lucide-react";
import { useLayoutEffect, useRef, type KeyboardEvent, type RefObject, type UIEvent } from "react";
import type { Message } from "../types";
import { plural } from "../lib/format";
import {
  INBOX_MODES,
  type CardKey,
  type GroupKey,
  type InboxMode,
  type ListModel,
  type ListView,
} from "../lib/layout";
import { LIST_MAX, LIST_MIN, startHorizontalDrag } from "../lib/panes";
import { LIST_ITEMS } from "../lib/scroll";
import { hint, shortcutLabel } from "../lib/shortcuts";
import Dropdown, { MenuItem } from "./Dropdown";
import MessageRow, { type RowActions } from "./MessageRow";
import OverlayScroll from "./OverlayScroll";
import { GatekeeperCard, GroupRow, SmartInboxCard } from "./SmartInboxCard";
import { tipAttrs } from "../lib/tooltip";

export interface GatekeeperActions {
  accept: (m: Message) => void;
  block: (m: Message) => void;
  acceptAll: (items: Message[]) => void;
  blockAll: (items: Message[]) => void;
}

interface Props {
  model: ListModel;
  view: ListView;
  selectedId: number | null;
  openId: number | null;
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
  useRowSlide(scrollRef, listId, model.order);
  const reachAtEnd = useOlderAtEnd(scrollRef, listId, model, older);
  const row = (m: Message, inPriorityCard = false) => (
    <MessageRow
      key={m.id}
      message={m}
      count={model.threads.get(m.id)?.count}
      names={model.threads.get(m.id)?.names}
      selected={m.id === selectedRow}
      actions={rows}
      inPriorityCard={inPriorityCard}
      recipients={model.recipients}
    />
  );

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
          <SearchTips onPick={(q) => props.onSearchChange(q)} />
        ) : searching && model.found === 0 ? (
          <NoResults />
        ) : (
          <>
            {searching && (
              <div className="flex h-9 items-center px-3 text-[13px] text-text-2">
                {plural(model.found ?? 0, "email")} found
              </div>
            )}
            {model.blocks.map((block, i) => {
              switch (block.kind) {
                case "row":
                  return row(block.message);
                case "period":
                  return (
                    <div
                      key={`p-${block.title}`}
                      data-period={block.title}
                      className="flex h-10 items-end px-3 pb-2 text-[13px] text-text-2"
                    >
                      {block.title}
                    </div>
                  );
                case "card":
                  return (
                    <SmartInboxCard
                      key={block.key}
                      cardKey={block.key}
                      items={block.items}
                      renderRow={row}
                      onCompleteAll={() => props.onDoneAll(block.items)}
                      onShowAll={() => props.onShowAll(block.key)}
                    />
                  );
                case "gatekeeper":
                  return (
                    <GatekeeperCard
                      key="gatekeeper"
                      items={block.items}
                      layout={block.layout}
                      openId={props.openId}
                      onOpen={rows.open}
                      onShowAll={props.onOpenGatekeeper}
                      onAccept={props.gatekeeper.acceptAll}
                      onBlock={props.gatekeeper.blockAll}
                      onAcceptAll={() => props.gatekeeper.acceptAll(block.items)}
                      onBlockAll={() => props.gatekeeper.blockAll(block.items)}
                    />
                  );
                case "group":
                  return (
                    <GroupRow
                      key={`g-${block.key}-${i}`}
                      groupKey={block.key}
                      items={block.items}
                      onOpen={() => props.onOpenGroup(block.key)}
                    />
                  );
              }
            })}
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

/** A row by its message id, a group row or a date header by what it shows. */
function itemKey(el: HTMLElement) {
  const { row, group, period } = el.dataset;
  return row !== undefined ? `row:${row}` : group !== undefined ? `group:${group}` : `period:${period}`;
}

/** Where each row, group row and date header is on screen. */
function itemTops(scroller: HTMLElement) {
  const tops = new Map<string, number>();
  for (const el of scroller.querySelectorAll<HTMLElement>(LIST_ITEMS)) {
    tops.set(itemKey(el), el.getBoundingClientRect().top);
  }
  return tops;
}

const rowKeys = (tops: Map<string, number>) => [...tops.keys()].filter((key) => key.startsWith("row:"));

/** Spark moves rows in 200ms. */
const SLIDE_MS = 200;

/**
 * When rows leave the list (Done, Delete, Snooze…) and none arrive, what's
 * after them slides up into place instead of jumping. Where things were is
 * read while rendering, before React changes the DOM.
 */
function useRowSlide(scrollRef: RefObject<HTMLDivElement | null>, listId: string, order: number[]) {
  const committed = useRef({ order, listId });
  const before = useRef<Map<string, number> | null>(null);
  if (committed.current.order !== order && before.current === null && scrollRef.current) {
    before.current = itemTops(scrollRef.current);
  }
  useLayoutEffect(() => {
    const was = before.current;
    const sameList = committed.current.listId === listId;
    before.current = null;
    committed.current = { order, listId };
    const el = scrollRef.current;
    if (!was || !el || !sameList) return;
    const now = itemTops(el);
    const removed = rowKeys(was).some((key) => !now.has(key));
    const added = rowKeys(now).some((key) => !was.has(key));
    if (!removed || added) return;
    const box = el.getBoundingClientRect();
    for (const item of el.querySelectorAll<HTMLElement>(LIST_ITEMS)) {
      const from = was.get(itemKey(item));
      const to = now.get(itemKey(item))!;
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

function SearchTips({ onPick }: { onPick: (query: string) => void }) {
  return (
    <div className="px-1 pt-3">
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
