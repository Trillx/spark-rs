import {
  Check,
  ChevronRight,
  CircleAlert,
  Ellipsis,
  Inbox,
  Menu,
  PanelLeft,
  PanelLeftDashed,
  RefreshCw,
} from "lucide-react";
import {
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type MouseEvent,
  type PointerEvent as ReactPointerEvent,
  type ReactNode,
} from "react";
import type { AccountId, AccountView, FolderView, Message } from "../types";
import { useAccounts } from "../lib/accounts";
import { dropAttrs, trackDrag, useMessageDrag, useMessageDrop, type DropTarget } from "../lib/drag";
import type { AccountFilter, SidebarCounts } from "../lib/layout";
import { SIDEBAR_DEFAULT, SIDEBAR_MAX, SIDEBAR_MIN, clamp, startHorizontalDrag } from "../lib/panes";
import { hint, shortcutLabel } from "../lib/shortcuts";
import {
  FOLDER_COLORS,
  dropPatch,
  folderIdOf,
  hasAccountRows,
  listInfo,
  moveInSidebar,
  pinToTop,
  removeFromSidebar,
  removeFromTop,
  setColor,
  smartIdOf,
  toggleBadge,
  toggleExpanded,
  type ListInfo,
  type ListKey,
  type SidebarLayout,
  type SidebarSection,
} from "../lib/sidebar";
import { tipAttrs, tipLabel, type Tip } from "../lib/tooltip";
import AccountMark from "./AccountMark";
import CalendarIcon from "./CalendarIcon";
import { ConfirmDialog, PromptDialog } from "./Dialog";
import FolderPicker from "./FolderPicker";

export type SidebarMode = "rail" | "overlay" | "locked";

/** What the sidebar asks App to do. */
export interface SidebarActions {
  select: (key: ListKey, account?: AccountFilter) => void;
  /** From the folder picker. `add` (Ctrl-click, star) puts it in the sidebar without opening it. */
  pick: (key: ListKey, add: boolean) => void;
  layout: (change: (layout: SidebarLayout) => SidebarLayout) => void;
  open: () => void;
  /** Closes the overlay and the folder picker. */
  close: () => void;
  toggleLock: () => void;
  resize: (width: number) => void;
  sync: () => void;
  settings: () => void;
  togglePicker: () => void;
  closePicker: () => void;
  newSmartFolder: () => void;
  createFolder: (account: AccountId, parent: number | undefined, name: string) => Promise<void>;
  /** Renames a server folder or a smart folder. */
  renameList: (key: ListKey, name: string) => Promise<void>;
  deleteList: (key: ListKey) => Promise<void>;
  /** A message dragged from the list onto a list here; `account` for an account row. */
  drop: (m: Message, key: ListKey, account?: AccountId) => void;
  calendar: () => void;
}

interface Props {
  mode: SidebarMode;
  width: number;
  layout: SidebarLayout;
  folders: FolderView[];
  /** The open list, and the account it's narrowed to. */
  current: ListKey;
  account: AccountFilter;
  counts: SidebarCounts;
  /** Some account is syncing: the sync button spins until it's done. */
  syncing: boolean;
  pickerOpen: boolean;
  /** Only the demo accounts: their folders can't be changed. */
  demo: boolean;
  /** The calendar shows in place of the lists: no list is current. */
  calendarOpen: boolean;
  actions: SidebarActions;
}

type Section = "favorites" | "folders" | "recents";
type RowMenuState = { key: ListKey; section: Section; x: number; y: number };
type DialogState =
  | { kind: "create"; account: AccountId; parent?: number }
  | { kind: "rename"; key: ListKey; name: string }
  | { kind: "delete"; key: ListKey; name: string };

/**
 * Always dark. By default a 72px icon rail; `/`, the ≡ button or a click on
 * empty rail space shows the full sidebar over the list, and Lock Sidebar
 * docks it. "More" opens the folder picker beside it.
 */
export default function Sidebar(props: Props) {
  const { mode, width, pickerOpen, actions } = props;
  const [menu, setMenu] = useState<RowMenuState | null>(null);
  const [dialog, setDialog] = useState<DialogState | null>(null);
  const openMenu = (key: ListKey, section: Section, e: MouseEvent) => {
    e.preventDefault();
    setMenu({ key, section, x: e.clientX, y: e.clientY });
  };
  // While the calendar shows, no list is current.
  const view = { ...props, current: props.calendarOpen ? NO_LIST : props.current, onMenu: openMenu };
  useMessageDrop((m, target) => actions.drop(m, target.key, target.account));

  return (
    <>
      {mode === "locked" ? <Expanded {...view} /> : <Rail {...view} />}
      {(mode === "overlay" || pickerOpen) && (
        <div className="fixed inset-0 z-[101]" onClick={mode === "overlay" ? actions.close : actions.closePicker} />
      )}
      {mode === "overlay" && (
        <div className="fixed inset-y-0 left-0 z-[102] shadow-[10px_4px_20px_rgba(0,0,0,.25)]" style={{ width }}>
          <Expanded {...view} />
        </div>
      )}
      {pickerOpen && mode !== "rail" && (
        <FolderPicker
          left={width}
          layout={props.layout}
          folders={props.folders}
          demo={props.demo}
          onPick={actions.pick}
          onRemove={(key) => actions.layout((l) => removeFromSidebar(l, key))}
          onNewSmartFolder={actions.newSmartFolder}
          onNewFolder={(account, parent) => setDialog({ kind: "create", account, parent })}
          onHideTip={() => actions.layout((l) => ({ ...l, tipHidden: true }))}
          onClose={actions.close}
        />
      )}
      {menu && <RowMenu {...props} menu={menu} onDialog={setDialog} onClose={() => setMenu(null)} />}
      {dialog && <SidebarDialog {...props} dialog={dialog} onClose={() => setDialog(null)} />}
    </>
  );
}

type ViewProps = Props & { onMenu: (key: ListKey, section: Section, e: MouseEvent) => void };

/** `current` while the calendar is open: a smart folder that can't exist, so nothing matches it. */
const NO_LIST: ListKey = "smart:";

/** While a message is dragged: whether a row takes it (`target`), and whether it's under the pointer. */
type RowDrop = { target?: DropTarget; over: boolean };

function useRowDrop(folders: FolderView[]) {
  const drag = useMessageDrag();
  return (key: ListKey, account?: AccountId): RowDrop | undefined => {
    if (!drag) return undefined;
    if (!dropPatch(drag.message, key, folders, account)) return { over: false };
    return { target: { key, account }, over: drag.over?.key === key && drag.over.account === account };
  };
}

/** A row the dragged message is over, ready to take it. */
const DROP_OVER = "bg-primary/30 text-white ring-1 ring-inset ring-primary";

/** The rail shows the top section, plus the open list when it's further down. */
function railKeys(layout: SidebarLayout, current: ListKey): [ListKey, Section][] {
  const keys: [ListKey, Section][] = layout.favorites.map((k) => [k, "favorites"]);
  if (current !== "inbox" && current !== NO_LIST && !layout.favorites.includes(current)) {
    keys.push([current, layout.folders.includes(current) ? "folders" : "recents"]);
  }
  return keys;
}

function Rail(props: ViewProps) {
  const { layout, folders, current, account, actions, onMenu } = props;
  const accounts = useAccounts();
  const info = useListInfo(folders, layout);
  const drop = useRowDrop(folders);
  const narrowed = account !== "all" ? accounts.find((a) => a.id === account) : undefined;
  const hasIssue = accounts.some((a) => a.sync.status === "error");
  return (
    <nav aria-label="Sidebar" className="flex h-full w-[72px] shrink-0 flex-col bg-neutral pb-3 text-neutral-content">
      <div data-tauri-drag-region className="h-8 w-full shrink-0" />
      <div className="flex min-h-0 shrink flex-col items-center gap-2 overflow-y-auto overflow-x-hidden">
        <RailInbox {...props} />
        <RailButton title="Calendar" active={props.calendarOpen} onClick={actions.calendar}>
          <CalendarIcon active={props.calendarOpen} />
        </RailButton>
        {railKeys(layout, current).map(([key, section]) => {
          const i = info(key);
          if (!i) return null;
          return (
            <RailButton
              key={key}
              title={i.detail ? { label: i.label, detail: i.detail } : i.label}
              active={current === key}
              drop={drop(key)}
              onClick={() => actions.select(key)}
              onContextMenu={(e) => onMenu(key, section, e)}
            >
              <ListIcon info={i} color={layout.colors[key]} abbreviate />
              {current === key && narrowed && <NarrowedMark account={narrowed} />}
            </RailButton>
          );
        })}
        <RailButton title={hint("Open Sidebar", "sidebar")} alert={hasIssue} onClick={actions.open}>
          <Menu size={18} />
        </RailButton>
      </div>
      {/* Clicking empty rail space opens the sidebar, like Spark. */}
      <div className="w-full flex-1" onClick={actions.open} />
      <div className="flex justify-center">
        <SettingsAvatar onClick={actions.settings} />
      </div>
    </nav>
  );
}

/** The Inbox, and on hover a toggle that drops down one button per account. */
function RailInbox({ current, account, counts, folders, actions }: ViewProps) {
  const accounts = useAccounts();
  const drop = useRowDrop(folders);
  const [open, setOpen] = useState(false);
  const inbox = current === "inbox";
  const narrowed = inbox && account !== "all" ? accounts.find((a) => a.id === account) : undefined;
  return (
    <div className="group relative flex w-full justify-center">
      {accounts.length > 1 && (
        <button
          aria-label={open ? "Hide accounts" : "Show accounts"}
          aria-expanded={open}
          className="absolute left-1.5 top-[7px] grid size-4 place-items-center rounded opacity-0 hover:text-white focus-visible:opacity-100 group-hover:opacity-100"
          onClick={() => setOpen((o) => !o)}
        >
          <ChevronRight size={12} className={`transition-transform ${open ? "rotate-90" : ""}`} />
        </button>
      )}
      <div className={`flex flex-col items-center gap-1 rounded-lg ${open ? "bg-sidebar-hover pb-1" : ""}`}>
        <RailButton
          title="Inbox"
          active={inbox && (account === "all" || !open)}
          dot={counts.inbox > 0}
          drop={drop("inbox")}
          onClick={() => actions.select("inbox", "all")}
        >
          <Inbox size={18} />
          {narrowed && !open && <NarrowedMark account={narrowed} />}
        </RailButton>
        {open &&
          accounts.map((a) => (
            <RailButton
              key={a.id}
              title={accountTitle(a)}
              active={inbox && account === a.id}
              alert={a.sync.status === "error"}
              drop={drop("inbox", a.id)}
              onClick={() => actions.select("inbox", a.id)}
            >
              <AccountMark color={a.color} />
            </RailButton>
          ))}
      </div>
    </div>
  );
}

/** The account a rail list is narrowed to, on its icon's corner. */
function NarrowedMark({ account }: { account: AccountView }) {
  return (
    <span className="absolute -bottom-0.5 -right-0.5 rounded-full bg-neutral">
      <AccountMark color={account.color} />
    </span>
  );
}

function RailButton({
  title,
  active = false,
  dot = false,
  alert = false,
  drop,
  onClick,
  onContextMenu,
  children,
}: {
  title: Tip;
  active?: boolean;
  /** Unread mail. */
  dot?: boolean;
  /** A sync error. */
  alert?: boolean;
  drop?: RowDrop;
  onClick: () => void;
  onContextMenu?: (e: MouseEvent) => void;
  children: ReactNode;
}) {
  return (
    <button
      {...tipAttrs(title)}
      {...dropAttrs(drop?.target)}
      data-tip-side="right"
      aria-label={tipLabel(title)}
      aria-current={active || undefined}
      onClick={onClick}
      onContextMenu={onContextMenu}
      className={`relative grid size-[30px] shrink-0 place-items-center rounded-lg ${
        drop?.over
          ? DROP_OVER
          : active
            ? "bg-sidebar-active text-white"
            : "hover:bg-sidebar-hover hover:text-white"
      } ${drop && !drop.target ? "opacity-40" : ""}`}
    >
      {children}
      {(dot || alert) && (
        <span className={`absolute right-0.5 top-0.5 size-1.5 rounded-full ${alert ? "bg-error" : "bg-primary"}`} />
      )}
    </button>
  );
}

/** A list's icon; on the rail, server folders show the first three letters of their name instead. */
function ListIcon({ info, color, abbreviate = false }: { info: ListInfo; color?: string; abbreviate?: boolean }) {
  if (abbreviate && info.folder) {
    return (
      <span className="text-[11px] font-semibold leading-none" style={color ? { color } : undefined}>
        {[...info.label].slice(0, 3).join("")}
      </span>
    );
  }
  return <info.Icon size={18} style={color ? { color } : undefined} />;
}

function useListInfo(folders: FolderView[], layout: SidebarLayout) {
  const accounts = useAccounts();
  const label = (id: AccountId) => accounts.find((a) => a.id === id)?.label ?? "";
  return (key: ListKey) => listInfo(key, folders, layout.smartFolders, label);
}

/** Where a list dragged in the sidebar would land: before row `index` of a section. */
type Slot = { section: SidebarSection; index: number };

function Expanded(props: ViewProps) {
  const { mode, width, layout, folders, current, account, counts, syncing, pickerOpen, actions, onMenu } = props;
  const accounts = useAccounts();
  const info = useListInfo(folders, layout);
  const drop = useRowDrop(folders);
  const [clicks, setClicks] = useState(0);
  /** A list being dragged to another place in the sidebar. */
  const [moving, setMoving] = useState<{ key: ListKey; to: Slot | null } | null>(null);
  const scroller = useRef<HTMLDivElement>(null);
  const locked = mode === "locked";
  const hidden = (key: ListKey) => layout.hiddenBadges.includes(key);
  const inboxOpen = accounts.length === 1 || layout.expanded.includes("inbox");

  function sync() {
    setClicks((n) => n + 1);
    actions.sync();
  }

  function startMove(e: ReactPointerEvent<HTMLElement>, key: ListKey, label: string) {
    let to: Slot | null = null;
    trackDrag(e, {
      ghost: label,
      start: () => setMoving({ key, to }),
      move: (x, y) => {
        const next = slotAt(scroller.current, x, y);
        if (next?.section === to?.section && next?.index === to?.index) return;
        to = next;
        setMoving({ key, to });
      },
      end: (dropped) => {
        setMoving(null);
        if (!dropped || !to) return;
        const { section, index } = to;
        actions.layout((l) => moveInSidebar(l, key, section, index));
      },
    });
  }

  /** The line where the dragged list would land. */
  const line = (section: Section, index: number, edge: "top" | "bottom") =>
    moving?.to?.section === section && moving.to.index === index && <DropLine edge={edge} />;

  const toggle = (key: ListKey, open: boolean) => ({
    open,
    label: open ? "Hide accounts" : "Show accounts",
    onToggle: () => actions.layout((l) => toggleExpanded(l, key)),
  });

  const accountRow = (key: ListKey, a: AccountView, count: number) => (
    <SidebarRow
      key={a.id}
      indent
      icon={<AccountMark color={a.color} />}
      label={a.label}
      title={accountTitle(a)}
      count={count}
      alert={a.sync.status === "error" ? (a.sync.message ?? "Couldn't sync") : undefined}
      active={current === key && account === a.id}
      drop={drop(key, a.id)}
      onClick={() => actions.select(key, a.id)}
    />
  );

  /** A list's row and, when it's expanded, one row per account under it. */
  const row = (key: ListKey, section: Section, index: number) => {
    const i = info(key);
    if (!i) return null;
    const expandable = accounts.length > 1 && hasAccountRows(key);
    const open = expandable && layout.expanded.includes(key);
    return (
      <div key={key} data-item={index} className={`relative ${moving?.key === key ? "opacity-50" : ""}`}>
        {line(section, index, "top")}
        <SidebarRow
          icon={<ListIcon info={i} color={layout.colors[key]} />}
          label={i.label}
          title={i.detail}
          count={hidden(key) ? 0 : (counts.lists[key] ?? 0)}
          active={current === key && (account === "all" || !open)}
          toggle={expandable ? toggle(key, open) : undefined}
          drop={drop(key)}
          onClick={() => actions.select(key)}
          onContextMenu={(e) => onMenu(key, section, e)}
          onPointerDown={(e) => startMove(e, key, i.label)}
        />
        {open &&
          accounts.map((a) => accountRow(key, a, hidden(key) ? 0 : (counts.listAccounts[key]?.[a.id] ?? 0)))}
        {line(section, index + 1, "bottom")}
      </div>
    );
  };

  const sectionRows = (name: SidebarSection, className = "") => (
    <div data-section={name} className={`relative ${className}`}>
      {layout[name].length === 0 && line(name, 0, "top")}
      {layout[name].map((k, index) => row(k, name, index))}
    </div>
  );

  return (
    <nav
      aria-label="Sidebar"
      className={`relative flex h-full shrink-0 flex-col bg-neutral text-neutral-content ${locked ? "z-[102]" : ""}`}
      style={{ width }}
    >
      <div data-tauri-drag-region className="flex h-8 shrink-0 items-center justify-end gap-0.5 px-2">
        <SidebarIconButton title="Check for new mail" onClick={sync}>
          <RefreshCw
            key={syncing ? "syncing" : clicks}
            size={15}
            className={syncing ? "animate-spin" : clicks ? "animate-sync" : ""}
          />
        </SidebarIconButton>
        <SidebarIconButton title={locked ? "Unlock Sidebar" : "Lock Sidebar"} onClick={actions.toggleLock}>
          {locked ? <PanelLeft size={15} /> : <PanelLeftDashed size={15} />}
        </SidebarIconButton>
      </div>

      <div ref={scroller} className="flex-1 overflow-y-auto px-2 pb-2 pt-1">
        {/* The Inbox stays first; other lists can be dragged around it. */}
        <SidebarRow
          icon={<Inbox size={18} />}
          label="Inbox"
          count={counts.inbox}
          active={current === "inbox" && (account === "all" || !inboxOpen)}
          drop={drop("inbox")}
          onClick={() => actions.select("inbox", "all")}
          toggle={accounts.length > 1 ? toggle("inbox", inboxOpen) : undefined}
        />
        {inboxOpen && accounts.map((a) => accountRow("inbox", a, counts.accounts[a.id] ?? 0))}
        <SidebarRow
          icon={<CalendarIcon active={props.calendarOpen} />}
          label="Calendar"
          count={0}
          active={props.calendarOpen}
          onClick={actions.calendar}
        />

        {sectionRows("favorites", "mt-2")}

        <SectionLabel>Folders</SectionLabel>
        {sectionRows("folders")}
        <SidebarRow
          icon={<Ellipsis size={18} />}
          label="More"
          count={0}
          active={pickerOpen}
          onClick={actions.togglePicker}
        />

        {layout.recents.length > 0 && (
          <>
            <SectionLabel
              open={layout.recentsOpen}
              onToggle={() => actions.layout((l) => ({ ...l, recentsOpen: !l.recentsOpen }))}
              action={
                <button
                  aria-label="Clear recent folders"
                  className="rounded px-1.5 text-[11px] font-medium opacity-0 hover:bg-sidebar-hover hover:text-white focus-visible:opacity-100 group-hover:opacity-100"
                  onClick={() => actions.layout((l) => ({ ...l, recents: [] }))}
                >
                  Clear
                </button>
              }
            >
              Recents
            </SectionLabel>
            {layout.recentsOpen && layout.recents.map((k, index) => row(k, "recents", index))}
          </>
        )}
      </div>

      <div className="shrink-0 px-2 pb-3">
        <button
          className="flex h-[34px] w-full items-center gap-2 rounded-lg px-2 text-[13px] hover:bg-sidebar-hover hover:text-white"
          onClick={actions.settings}
        >
          <SettingsAvatar />
          Settings
        </button>
      </div>

      {locked && (
        <div
          data-tip="Drag to resize, double-click to reset"
          className="absolute inset-y-0 -right-px w-[3px] cursor-col-resize hover:bg-primary"
          onPointerDown={(e) =>
            startHorizontalDrag(e, (dx) => actions.resize(clamp(width + dx, SIDEBAR_MIN, SIDEBAR_MAX)))
          }
          onDoubleClick={() => actions.resize(SIDEBAR_DEFAULT)}
        />
      )}
    </nav>
  );
}

function SectionLabel({
  children,
  open,
  onToggle,
  action,
}: {
  children: ReactNode;
  /** Collapsible when given. */
  open?: boolean;
  onToggle?: () => void;
  action?: ReactNode;
}) {
  const label = (
    <>
      {onToggle && (
        <ChevronRight size={12} className={`-ml-0.5 shrink-0 transition-transform ${open ? "rotate-90" : ""}`} />
      )}
      <span className="truncate">{children}</span>
    </>
  );
  return (
    <div className="group mb-px mt-5 flex min-h-[22px] items-center gap-1 pl-2 pr-1 text-[13px] leading-[22px]">
      {onToggle ? (
        <button className="flex min-w-0 flex-1 items-center gap-1 text-left opacity-50 hover:opacity-80" aria-expanded={open} onClick={onToggle}>
          {label}
        </button>
      ) : (
        <h3 className="min-w-0 flex-1 truncate font-normal opacity-50">{label}</h3>
      )}
      {action}
    </div>
  );
}

function SidebarRow({
  icon,
  label,
  title,
  count,
  alert,
  active,
  indent = false,
  toggle,
  drop,
  onClick,
  onContextMenu,
  onPointerDown,
}: {
  icon: ReactNode;
  label: string;
  title?: string;
  count: number;
  /** A sync error, shown as an icon in place of the count. */
  alert?: string;
  active: boolean;
  indent?: boolean;
  /** An expand toggle, e.g. the Inbox's accounts. */
  toggle?: { open: boolean; label: string; onToggle: () => void };
  drop?: RowDrop;
  onClick: () => void;
  onContextMenu?: (e: MouseEvent) => void;
  /** Starts dragging the row to another place. */
  onPointerDown?: (e: ReactPointerEvent<HTMLElement>) => void;
}) {
  return (
    <div className={`group relative mb-0.5 ${drop && !drop.target ? "opacity-40" : ""}`}>
      <button
        data-tip={title}
        data-tip-side="right"
        {...dropAttrs(drop?.target)}
        aria-current={active || undefined}
        onClick={onClick}
        onContextMenu={onContextMenu}
        onPointerDown={onPointerDown}
        className={`flex h-[30px] w-full items-center gap-2 rounded-lg pr-2.5 text-left text-[13px] ${indent ? "pl-7" : "pl-2"} ${
          drop?.over
            ? DROP_OVER
            : active
              ? "bg-sidebar-selected text-white"
              : "hover:bg-sidebar-hover hover:text-white"
        }`}
      >
        <span className="grid w-5 shrink-0 place-items-center">{icon}</span>
        <span className="min-w-0 flex-1 truncate">{label}</span>
        {alert ? (
          <span data-tip={alert} className="text-error">
            <CircleAlert size={14} aria-label={alert} />
          </span>
        ) : (
          count > 0 && (
            <span className={`text-[11px] font-medium leading-[13px] ${active ? "" : "opacity-50"} ${toggle ? "mr-5" : ""}`}>
              {count}
            </span>
          )
        )}
      </button>
      {toggle && (
        <button
          aria-label={toggle.label}
          aria-expanded={toggle.open}
          className={`absolute right-1.5 top-[7px] grid size-4 place-items-center rounded opacity-50 hover:opacity-100 ${
            active ? "text-white" : ""
          }`}
          onClick={toggle.onToggle}
        >
          <ChevronRight size={12} className={`transition-transform ${toggle.open ? "rotate-90" : ""}`} />
        </button>
      )}
    </div>
  );
}

function DropLine({ edge }: { edge: "top" | "bottom" }) {
  return (
    <span
      className={`pointer-events-none absolute inset-x-1 z-10 h-0.5 rounded-full bg-primary ${
        edge === "top" ? "-top-0.5" : "-bottom-0.5"
      }`}
    />
  );
}

/** The gap nearest the pointer in the top section or Folders, while the pointer is over the sidebar's lists. */
function slotAt(scroller: HTMLElement | null, x: number, y: number): Slot | null {
  const box = scroller?.getBoundingClientRect();
  if (!scroller || !box || x < box.left || x > box.right || y < box.top || y > box.bottom) return null;
  let best: Slot | null = null;
  let distance = Infinity;
  for (const section of ["favorites", "folders"] as const) {
    const list = scroller.querySelector<HTMLElement>(`[data-section="${section}"]`);
    if (!list) continue;
    // Before each row, and after the last; an empty section is one gap.
    const items = [...list.querySelectorAll<HTMLElement>(":scope > [data-item]")];
    const gaps = items.map((el) => ({ y: el.getBoundingClientRect().top, index: Number(el.dataset.item) }));
    const last = items.at(-1);
    gaps.push(
      last
        ? { y: last.getBoundingClientRect().bottom, index: Number(last.dataset.item) + 1 }
        : { y: list.getBoundingClientRect().top, index: 0 },
    );
    for (const gap of gaps) {
      if (Math.abs(y - gap.y) >= distance) continue;
      distance = Math.abs(y - gap.y);
      best = { section, index: gap.index };
    }
  }
  return best;
}

function SidebarIconButton({ title, onClick, children }: { title: string; onClick: () => void; children: ReactNode }) {
  return (
    <button
      data-tip={title}
      aria-label={title}
      onClick={onClick}
      className="grid size-[26px] place-items-center rounded-lg hover:bg-sidebar-hover hover:text-white"
    >
      {children}
    </button>
  );
}

/** Right-click on a sidebar list: where it sits, its badge and color, and renaming or deleting it. */
function RowMenu({
  menu,
  layout,
  folders,
  demo,
  actions,
  onDialog,
  onClose,
}: Props & { menu: RowMenuState; onDialog: (d: DialogState) => void; onClose: () => void }) {
  const ref = useRef<HTMLUListElement>(null);
  const [position, setPosition] = useState({ left: menu.x, top: menu.y });
  const { key, section } = menu;
  const folder = folders.find((f) => f.id === folderIdOf(key));
  const smart = layout.smartFolders.find((f) => f.id === smartIdOf(key));
  const name = folder?.name ?? smart?.name ?? "";
  const countable = key === "snoozed" || key === "aside" || key === "drafts" || key === "spam" || folder !== undefined;

  const close = useRef(onClose);
  close.current = onClose;

  // Kept inside the window.
  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    setPosition({
      left: Math.min(menu.x, window.innerWidth - el.offsetWidth - 8),
      top: Math.min(menu.y, window.innerHeight - el.offsetHeight - 8),
    });
  }, [menu]);

  useEffect(() => {
    const onPointer = (e: PointerEvent) => {
      if (!ref.current?.contains(e.target as Node)) close.current();
    };
    // Capture phase, so Esc closes the menu and nothing behind it.
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      e.stopPropagation();
      close.current();
    };
    window.addEventListener("pointerdown", onPointer);
    window.addEventListener("keydown", onKey, true);
    return () => {
      window.removeEventListener("pointerdown", onPointer);
      window.removeEventListener("keydown", onKey, true);
    };
  }, []);

  const item = (label: string, run: () => void, danger = false) => (
    <li role="none">
      <button
        role="menuitem"
        className={`rounded-md ${danger ? "text-error" : ""}`}
        onClick={() => {
          onClose();
          run();
        }}
      >
        {label}
      </button>
    </li>
  );
  const change = (f: (l: SidebarLayout) => SidebarLayout) => () => actions.layout(f);

  return (
    <ul
      ref={ref}
      role="menu"
      aria-label={`${name || "List"} options`}
      className="menu menu-sm fixed z-[2020] w-52 gap-0.5 rounded-box border border-hairline bg-popover p-1 text-[13px] text-base-content shadow-popup"
      style={position}
    >
      {section === "favorites"
        ? item("Remove from the top", change((l) => removeFromTop(l, key)))
        : item("Pin to the top", change((l) => pinToTop(l, key)))}
      {item(section === "recents" ? "Remove from Recents" : "Remove from Sidebar", change((l) => removeFromSidebar(l, key)))}
      {countable && item(layout.hiddenBadges.includes(key) ? "Show Badge" : "Hide Badge", change((l) => toggleBadge(l, key)))}
      {(folder || smart) && (
        <li role="none" className="menu-title px-2 pb-1 pt-2 text-xs font-normal">
          <span className="p-0">Color</span>
          <div className="mt-1.5 flex flex-wrap gap-1.5">
            {[undefined, ...FOLDER_COLORS].map((color) => (
              <button
                key={color ?? "none"}
                role="menuitemradio"
                aria-checked={layout.colors[key] === color}
                aria-label={color ? `Color ${color}` : "No color"}
                className="grid size-5 place-items-center rounded-full border border-hairline"
                style={{ backgroundColor: color }}
                onClick={() => {
                  onClose();
                  actions.layout((l) => setColor(l, key, color));
                }}
              >
                {layout.colors[key] === color && <Check size={12} className={color ? "text-white" : ""} />}
              </button>
            ))}
          </div>
        </li>
      )}
      {((folder && !demo) || smart) && (
        <>
          {item("Rename…", () => onDialog({ kind: "rename", key, name }))}
          {item("Delete…", () => onDialog({ kind: "delete", key, name }), true)}
        </>
      )}
    </ul>
  );
}

function SidebarDialog({
  dialog,
  folders,
  actions,
  onClose,
}: Props & { dialog: DialogState; onClose: () => void }) {
  const accounts = useAccounts();
  switch (dialog.kind) {
    case "create": {
      const account = accounts.find((a) => a.id === dialog.account);
      const parent = folders.find((f) => f.id === dialog.parent);
      return (
        <PromptDialog
          title={parent ? "New Subfolder" : "New Folder"}
          label={parent ? `In ${parent.name} (${account?.label ?? ""})` : `In ${account?.label ?? "the account"}`}
          placeholder="Folder name"
          confirmLabel="Create"
          onSubmit={(name) => actions.createFolder(dialog.account, dialog.parent, name)}
          onClose={onClose}
        />
      );
    }
    case "rename":
      return (
        <PromptDialog
          title={smartIdOf(dialog.key) ? "Rename Smart Folder" : "Rename Folder"}
          initial={dialog.name}
          confirmLabel="Rename"
          onSubmit={(name) => actions.renameList(dialog.key, name)}
          onClose={onClose}
        />
      );
    case "delete": {
      const id = folderIdOf(dialog.key);
      const nested = id !== undefined && folders.some((f) => f.parent === id);
      const account = accounts.find((a) => a.id === folders.find((f) => f.id === id)?.account);
      return (
        <ConfirmDialog
          title={id === undefined ? "Delete this smart folder?" : account?.kind === "gmail" ? "Delete this label?" : "Delete this folder?"}
          message={
            id === undefined
              ? `“${dialog.name}” will be deleted. The emails in it won't be affected.`
              : account?.kind === "gmail"
                ? `“${dialog.name}”${nested ? " and the labels nested in it" : ""} will be deleted from Gmail. The emails keep their other labels and stay in All Mail.`
                : nested
                  ? `“${dialog.name}”, its subfolders and the emails in them will be deleted on the server. This can't be undone.`
                  : `“${dialog.name}” and the emails in it will be deleted on the server. This can't be undone.`
          }
          confirmLabel="Delete"
          onConfirm={() => actions.deleteList(dialog.key)}
          onClose={onClose}
        />
      );
    }
  }
}

/** "Work · me@contoso.com · Microsoft 365" */
function accountTitle(a: AccountView) {
  return `${a.label} · ${a.email} · ${a.provider}`;
}

function SettingsAvatar({ onClick }: { onClick?: () => void }) {
  const face = (
    <span className="grid size-[22px] place-items-center rounded-full bg-[#6d3ecf] text-[10px] font-semibold text-white">
      M
    </span>
  );
  if (!onClick) return face;
  return (
    <button
      {...tipAttrs({ label: "Settings", keys: shortcutLabel("settings") })}
      data-tip-side="right"
      aria-label="Settings"
      className="grid size-[30px] place-items-center rounded-lg hover:bg-sidebar-hover"
      onClick={onClick}
    >
      {face}
    </button>
  );
}
