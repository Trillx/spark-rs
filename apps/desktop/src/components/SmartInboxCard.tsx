import { Check, ChevronRight, Ellipsis, Mail, Minus, Plus, ThumbsDown, ThumbsUp, Zap } from "lucide-react";
import { useEffect, useRef, useState, type ReactNode } from "react";
import type { Message } from "../types";
import { CARDS, GATEKEEPER, groupInfo, type CardKey, type GroupKey } from "../lib/layout";
import Avatar from "./Avatar";
import Dropdown, { MenuItem } from "./Dropdown";

/** Smart Inbox card: icon header, complete-all, the first `shown` rows, "Show all (N)". */
/** Rows a card can show before "Show all", in its menu. */
const CARD_ROWS = [3, 5, 10];

/** A card's own settings, from its menu. */
export interface CardSettings {
  setRows: (rows: number) => void;
  /** Turns the card off; its mail goes to People. Not offered for People. */
  hide: () => void;
}

export function SmartInboxCard({
  cardKey,
  items,
  shown,
  renderRow,
  onCompleteAll,
  onShowAll,
  settings,
}: {
  cardKey: CardKey;
  items: Message[];
  shown: number;
  renderRow: (m: Message, inPriorityCard: boolean) => ReactNode;
  onCompleteAll: () => void;
  onShowAll: () => void;
  settings: CardSettings;
}) {
  if (cardKey === "priority") {
    return (
      <PriorityCard
        items={items}
        shown={shown}
        renderRow={renderRow}
        onCompleteAll={onCompleteAll}
        onShowAll={onShowAll}
      />
    );
  }
  const { label, Icon, iconClass } = CARDS[cardKey];
  return (
    <section aria-label={label} className="mb-2.5 rounded-2xl border border-hairline">
      <header className="flex h-[52px] items-center gap-2.5 pl-3.5 pr-2.5">
        <span className={`grid size-6 shrink-0 place-items-center rounded-full text-white ${iconClass}`}>
          <Icon size={14} strokeWidth={2} />
        </span>
        <h3 className="min-w-0 flex-1 truncate text-[13px] font-medium leading-[18px]">{label}</h3>
        <CompleteAll label={label} count={items.length} onClick={onCompleteAll} />
        <Dropdown
          align="end"
          width="w-48"
          trigger={({ toggle, open }) => (
            <button
              data-tip="Cards settings"
              aria-label={`${label} card settings`}
              aria-haspopup="menu"
              aria-expanded={open}
              onClick={toggle}
              className={`grid size-7 place-items-center rounded-full text-text-2 hover:bg-base-content/10 hover:text-base-content ${
                open ? "bg-base-content/10" : ""
              }`}
            >
              <Ellipsis size={16} />
            </button>
          )}
        >
          {CARD_ROWS.map((n) => (
            <MenuItem key={n} label={`Show ${n} emails`} checked={shown === n} onSelect={() => settings.setRows(n)} />
          ))}
          {cardKey !== "people" && (
            <>
              <li role="none" className="my-0.5 h-px bg-hairline" />
              <MenuItem label="Turn off this card" onSelect={settings.hide} />
            </>
          )}
        </Dropdown>
      </header>
      <div className="px-[5px]">{items.slice(0, shown).map((m) => renderRow(m, false))}</div>
      <button
        onClick={onShowAll}
        className="flex h-10 w-full items-center gap-1 rounded-b-2xl px-3.5 text-[13px] text-text-2 hover:bg-row-hover hover:text-base-content"
      >
        Show all ({items.length})
        <ChevronRight size={14} className="ml-auto" />
      </button>
    </section>
  );
}

/** The peach Priority card that tops the Inbox. "Show all" only when some rows are left out. */
function PriorityCard({
  items,
  shown,
  renderRow,
  onCompleteAll,
  onShowAll,
}: {
  items: Message[];
  shown: number;
  renderRow: (m: Message, inPriorityCard: boolean) => ReactNode;
  onCompleteAll: () => void;
  onShowAll: () => void;
}) {
  const faces = [...new Map(items.map((m) => [m.address, m])).values()].slice(0, 3);
  return (
    <section aria-label="Priority" className="mb-2.5 rounded-2xl bg-priority-fill">
      <header className="flex h-11 items-center gap-1.5 border-b border-priority-line pl-3.5 pr-2.5 text-priority">
        <Zap size={14} className="fill-current" />
        <h3 className="min-w-0 flex-1 truncate text-[13px] font-medium leading-[18px]">Priority</h3>
        <CompleteAll label="Priority" count={items.length} onClick={onCompleteAll} />
      </header>
      <div className={`px-[5px] pt-[5px] ${shown < items.length ? "" : "pb-[5px]"}`}>
        {items.slice(0, shown).map((m) => renderRow(m, true))}
      </div>
      {shown < items.length && (
        <button
          onClick={onShowAll}
          className="flex h-10 w-full items-center gap-2 rounded-b-2xl px-3.5 text-[13px] font-medium text-priority hover:bg-priority-line/60"
        >
          Show all ({items.length})
          <span className="flex -space-x-1">
            {faces.map((m) => (
              <span key={m.address} className="rounded-full ring-2 ring-priority-fill">
                <Avatar name={m.from} size="xs" />
              </span>
            ))}
          </span>
        </button>
      )}
    </section>
  );
}

function CompleteAll({ label, count, onClick }: { label: string; count: number; onClick: () => void }) {
  return (
    <button
      data-tip={`Mark all ${count} as Done`}
      aria-label={`Mark all ${label} as Done`}
      onClick={onClick}
      className="grid size-7 place-items-center rounded-full text-text-2 hover:bg-base-content/10 hover:text-base-content"
    >
      <Check size={16} />
    </button>
  );
}

/** One card per new sender: their newest message, plus a count of the rest. */
function bySender(items: Message[]) {
  const senders = new Map<string, Message[]>();
  for (const m of items) senders.set(m.address, [...(senders.get(m.address) ?? []), m]);
  return [...senders.values()];
}

/**
 * New senders (Gatekeeper): a collapsible strip of sender cards
 * above the list, or the same cards in a grid on its own page.
 */
export function GatekeeperCard({
  items,
  layout,
  openId,
  onOpen,
  onShowAll,
  onAccept,
  onBlock,
  onAcceptAll,
  onBlockAll,
}: {
  items: Message[];
  layout: "strip" | "grid";
  openId: number | null;
  onOpen: (id: number) => void;
  onShowAll: () => void;
  /** All of one sender's messages. */
  onAccept: (items: Message[]) => void;
  onBlock: (items: Message[]) => void;
  onAcceptAll: () => void;
  onBlockAll: () => void;
}) {
  const [collapsed, setCollapsed] = useState(false);
  const senders = bySender(items);
  const cards = senders.map((group) => (
    <SenderCard
      key={group[0].address}
      items={group}
      open={group.some((m) => m.id === openId)}
      onOpen={() => onOpen(group[0].id)}
      onAccept={() => onAccept(group)}
      onBlock={() => onBlock(group)}
    />
  ));

  if (layout === "grid") {
    return <div className="grid grid-cols-[repeat(auto-fill,minmax(220px,1fr))] gap-2 px-1 pt-2">{cards}</div>;
  }

  const roundButton =
    "grid size-6 place-items-center rounded-full border border-hairline text-text-2 hover:bg-base-content/5 hover:text-base-content";
  return (
    <section aria-label={GATEKEEPER.label} className="mb-2.5">
      <header className="flex h-10 items-center gap-1.5 pl-2 pr-1">
        <button
          className="flex items-center gap-1.5 rounded-md px-1 py-0.5 text-[13px] font-medium text-text-2 hover:text-base-content"
          data-tip="Show all New Senders"
          onClick={onShowAll}
        >
          {GATEKEEPER.label}
          <span className="rounded-md bg-base-300 px-1.5 text-[11px] font-semibold leading-[18px]">
            {senders.length}
          </span>
          <ChevronRight size={14} />
        </button>
        <div className="ml-auto flex items-center gap-1.5">
          <Dropdown
            align="end"
            width="w-44"
            trigger={({ toggle, open }) => (
              <button
                className={`${roundButton} ${open ? "bg-base-content/5" : ""}`}
                data-tip="More"
                aria-label="New senders options"
                aria-haspopup="menu"
                onClick={toggle}
              >
                <Ellipsis size={14} />
              </button>
            )}
          >
            <MenuItem label="Accept all" icon={<ThumbsUp size={14} />} onSelect={onAcceptAll} />
            <MenuItem label="Block all" icon={<ThumbsDown size={14} />} onSelect={onBlockAll} />
          </Dropdown>
          <button
            className={roundButton}
            data-tip={collapsed ? "Expand" : "Collapse"}
            aria-label={collapsed ? "Expand new senders" : "Collapse new senders"}
            aria-expanded={!collapsed}
            onClick={() => setCollapsed((c) => !c)}
          >
            {collapsed ? <Plus size={14} /> : <Minus size={14} />}
          </button>
        </div>
      </header>
      {!collapsed && <div className="flex snap-x gap-2 overflow-x-auto px-1 pb-1">{cards}</div>}
    </section>
  );
}

function SenderCard({
  items,
  open,
  onOpen,
  onAccept,
  onBlock,
}: {
  items: Message[];
  open: boolean;
  onOpen: () => void;
  onAccept: () => void;
  onBlock: () => void;
}) {
  const m = items[0];
  const stop = (fn: () => void) => (e: { stopPropagation: () => void }) => {
    e.stopPropagation();
    fn();
  };
  return (
    <div
      onClick={onOpen}
      className={`flex w-[236px] shrink-0 cursor-default snap-start flex-col rounded-2xl border p-3.5 ${
        open ? "border-primary bg-row-selected" : "border-hairline bg-card hover:bg-row-hover"
      }`}
    >
      <div className="flex items-center gap-2.5">
        <Avatar name={m.from} size="lg" />
        <div className="min-w-0">
          <div className="truncate text-[13px] font-semibold leading-5">{m.from}</div>
          <div className="truncate text-xs leading-4 text-text-2">{m.address}</div>
        </div>
      </div>
      <div className="mt-2.5 flex items-center gap-1.5 text-xs leading-4 text-text-2">
        <Mail size={12} className="shrink-0" />
        <span className="truncate">{m.subject || "No subject"}</span>
        {items.length > 1 && <span className="shrink-0 font-medium">+{items.length - 1}</span>}
      </div>
      <div className="mt-3 grid grid-cols-2 gap-1.5">
        <button
          className="btn btn-sm h-8 gap-1.5 rounded-lg border-0 bg-primary/10 font-medium text-primary shadow-none hover:bg-primary/20"
          data-tip="Accept"
          data-tip-card="accept"
          onClick={stop(onAccept)}
        >
          <ThumbsUp size={14} /> Accept
        </button>
        <button
          className="btn btn-sm h-8 gap-1.5 rounded-lg border-0 bg-chip font-medium text-base-content/80 shadow-none hover:brightness-95"
          data-tip="Block"
          data-tip-card="block"
          onClick={stop(onBlock)}
        >
          <ThumbsDown size={14} /> Block
        </button>
      </div>
    </div>
  );
}

/** Pale tints for the Focused list's group icons (green3 / purple3 at low alpha). */
const GROUP_TINT: Record<string, string> = {
  notifications: "bg-[#48c725]/15 text-[#48c725]",
  newsletters: "bg-[#a06ad0]/15 text-[#a06ad0]",
  invitations: "bg-[#f05a5a]/15 text-[#f05a5a]",
};
/** A grouped sender's or domain's row. */
const OWN_GROUP_TINT = "bg-primary/10 text-primary";

/**
 * Composite row: Focused's notifications, newsletters or invitations, or a
 * sender or domain the user grouped, opening as a sub-list.
 */
export function GroupRow({
  groupKey,
  items,
  selected = false,
  picked = false,
  onOpen,
  onPick,
}: {
  groupKey: GroupKey;
  items: Message[];
  /** Under the list cursor, or in the selection for a batch action. */
  selected?: boolean;
  picked?: boolean;
  onOpen: () => void;
  /** Ctrl/⌘-click and Shift-click, as on rows. */
  onPick?: (range: boolean) => void;
}) {
  const { label, Icon } = groupInfo(groupKey, items);
  const ref = useRef<HTMLDivElement>(null);
  // Like rows, it scrolls into view when the cursor moves to it.
  useEffect(() => {
    if (selected && !ref.current?.matches(":hover")) ref.current?.scrollIntoView({ block: "nearest" });
  }, [selected]);
  const unread = items.filter((m) => m.unread).length;
  const senders = new Map<string, { count: number; unread: boolean }>();
  for (const m of items) {
    const s = senders.get(m.from) ?? { count: 0, unread: false };
    senders.set(m.from, { count: s.count + 1, unread: s.unread || m.unread });
  }
  return (
    <div
      ref={ref}
      role="button"
      tabIndex={-1}
      aria-pressed={selected || picked}
      data-group={groupKey}
      onClick={(e) => {
        if (onPick && (e.ctrlKey || e.metaKey || e.shiftKey)) onPick(e.shiftKey);
        else onOpen();
      }}
      className={`group grid min-h-[60px] cursor-default grid-cols-[28px_26px_minmax(0,1fr)] rounded-lg py-[5px] pr-2.5 ${
        selected || picked ? "bg-row-selected" : "hover:bg-row-hover"
      }`}
    >
      <span className="flex h-5 items-center justify-center">
        {unread > 0 && <span className="size-2 rounded-full bg-primary" />}
      </span>
      <span className={`grid size-[26px] place-items-center rounded-full ${GROUP_TINT[groupKey] ?? OWN_GROUP_TINT}`}>
        <Icon size={14} />
      </span>
      <div className="min-w-0 pl-2.5">
        <div className="flex h-5 items-center gap-1.5">
          <span className="text-sm font-semibold">{label}</span>
          <span className="text-xs text-text-2">{items.length}</span>
          <ChevronRight size={14} className="ml-auto text-text-3" />
        </div>
        <div className="mt-1 flex h-5 items-center gap-3 overflow-hidden text-xs">
          {[...senders].map(([name, s]) => (
            <span key={name} className="inline-flex shrink-0 items-center gap-1">
              {s.unread && <span className="size-1.5 rounded-full bg-primary" />}
              <Avatar name={name} size="xs" />
              <span className={s.unread ? "font-medium" : "text-text-2"}>{name}</span>
              {s.count > 1 && <span className="text-text-2">{s.count}</span>}
            </span>
          ))}
        </div>
      </div>
    </div>
  );
}
