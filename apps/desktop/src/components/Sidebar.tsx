import {
  Check,
  ChevronRight,
  CircleAlert,
  Ellipsis,
  House,
  Inbox,
  Menu,
  PanelLeft,
  PanelLeftDashed,
  RefreshCw,
} from "lucide-react";
import {
  Fragment,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type KeyboardEvent as ReactKeyboardEvent,
  type MouseEvent,
  type PointerEvent as ReactPointerEvent,
  type ReactNode,
  type RefObject,
} from "react";
import type { AccountId, AccountView, FolderView, Message } from "../types";
import { useAccounts } from "../lib/accounts";
import { dropAttrs, trackDrag, useMessageDrag, useMessageDrop, type DropTarget } from "../lib/drag";
import type { AccountFilter, SidebarCounts } from "../lib/layout";
import { SIDEBAR_DEFAULT, SIDEBAR_MAX, SIDEBAR_MIN, clamp, startHorizontalDrag } from "../lib/panes";
import { hint, shortcutLabel } from "../lib/shortcuts";
import {
  FOLDER_COLORS,
  SUBFOLDER_DEPTH,
  SYSTEM_LISTS,
  accepts,
  addToSidebar,
  autoLabelOf,
  dropPatch,
  folderIdOf,
  folderKey,
  hasAccountRows,
  inSidebar,
  isSystemList,
  listInfo,
  moveInSidebar,
  pinToTop,
  removeFromSidebar,
  removeFromTop,
  setColor,
  smartIdOf,
  subfolders,
  toggleBadge,
  toggleExpanded,
  type ListInfo,
  type ListKey,
  type SidebarLayout,
  type SidebarSection,
} from "../lib/sidebar";
import { tipAttrs, tipLabel, type Tip } from "../lib/tooltip";
import AccountMark from "./AccountMark";
import AccountTile from "./AccountTile";
import CalendarIcon from "./CalendarIcon";
import { ConfirmDialog, PromptDialog } from "./Dialog";
import FolderPicker from "./FolderPicker";
import OverlayScroll from "./OverlayScroll";

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
  markAllRead: (key: ListKey) => void;
  /** Whether a list's mail can all be deleted for good (Trash, Spam, folders that aren't Gmail labels). */
  canEmpty: (key: ListKey) => boolean;
  /** Asks, then deletes all of a list's mail for good. */
  empty: (key: ListKey) => void;
  calendar: () => void;
  /** Ends the session: the Home Screen shows. */
  home: () => void;
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
  updateAvailable: boolean;
  pickerOpen: boolean;
  /** Only the demo accounts: their folders can't be changed. */
  demo: boolean;
  /** The calendar shows in place of the lists: no list is current. */
  calendarOpen: boolean;
  /** The Home Screen row, while Settings has it on; `open` while it shows (no list is current then). */
  home: { open: boolean } | null;
  actions: SidebarActions;
}

/** Where a row is; `other` is a subfolder under its parent, or the open list on the rail when it's in no section. */
type Section = SidebarSection | "recents" | "other";
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
  // While the calendar or the Home Screen shows, no list is current.
  const elsewhere = props.calendarOpen || props.home?.open;
  const view = { ...props, current: elsewhere ? NO_LIST : props.current, onMenu: openMenu };
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

/** `current` while the calendar or Home Screen is open: a smart folder that can't exist, so nothing matches it. */
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
    const section = (["folders", "labels", "recents"] as const).find((s) => layout[s].includes(current));
    keys.push([current, section ?? "other"]);
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
        {props.home && (
          <RailButton title="Home Screen" active={props.home.open} onClick={actions.home}>
            <House size={18} />
          </RailButton>
        )}
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
              <ListIcon info={i} color={layout.colors[key] ?? i.color} abbreviate />
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
        <SettingsAvatar onClick={actions.settings} updateAvailable={props.updateAvailable} />
      </div>
    </nav>
  );
}

/** How long the pointer rests on the Inbox before its accounts menu opens, and may be away before it closes. */
const MENU_OPEN_DELAY = 220;
const MENU_CLOSE_DELAY = 160;
/** How long a dragged message is held over the Inbox before the menu opens to drop it on one account. */
const MENU_DRAG_DELAY = 400;

/** What opened the accounts menu: resting the pointer closes it on leaving, a drag when it ends. */
type MenuVia = "hover" | "click" | "key" | "drag";

/**
 * The Inbox; a click shows every account's mail. With more than one account,
 * resting on it, clicking its arrow or holding a dragged message over it opens
 * a menu beside the rail that narrows it to one account. Nothing in the rail moves.
 */
function RailInbox({ current, account, counts, folders, actions }: ViewProps) {
  const accounts = useAccounts();
  const drop = useRowDrop(folders);
  const drag = useMessageDrag();
  const anchor = useRef<HTMLDivElement>(null);
  const arrow = useRef<HTMLButtonElement>(null);
  const menuRef = useRef<HTMLDivElement>(null);
  const timer = useRef<number | undefined>(undefined);
  const [menu, setMenu] = useState<{ via: MenuVia; top: number; left: number } | null>(null);
  const inbox = current === "inbox";
  const narrowed = inbox && account !== "all" ? accounts.find((a) => a.id === account) : undefined;
  const many = accounts.length > 1;

  const later = (run: () => void, ms: number) => {
    window.clearTimeout(timer.current);
    timer.current = window.setTimeout(run, ms);
  };
  const open = (via: MenuVia) => {
    window.clearTimeout(timer.current);
    const r = anchor.current?.getBoundingClientRect();
    if (r) setMenu({ via, top: r.top, left: r.right });
  };
  /** Keyboard focus in the menu goes back to the arrow, so it isn't lost. */
  const close = () => {
    window.clearTimeout(timer.current);
    const focused = document.activeElement;
    const keyboard = focused instanceof HTMLElement && menuRef.current?.contains(focused) && focused.matches(":focus-visible");
    setMenu(null);
    if (keyboard) arrow.current?.focus();
  };
  const select = (a: AccountFilter) => {
    close();
    actions.select("inbox", a);
  };
  useEffect(() => () => window.clearTimeout(timer.current), []);

  // Open: a click outside or Esc closes it. Capture phase, so Esc closes nothing behind it.
  const isOpen = menu !== null;
  useEffect(() => {
    if (!isOpen) return;
    const onPointer = (e: PointerEvent) => {
      if (!anchor.current?.contains(e.target as Node)) close();
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      e.stopPropagation();
      close();
    };
    window.addEventListener("pointerdown", onPointer);
    window.addEventListener("keydown", onKey, true);
    return () => {
      window.removeEventListener("pointerdown", onPointer);
      window.removeEventListener("keydown", onKey, true);
    };
  }, [isOpen]);

  // A message held over the Inbox opens the menu. Moving it to another list closes the menu, and so does
  // dropping it, if the drag opened it. The pointer is captured while dragging, so no enter/leave arrives.
  const dragging = drag !== null;
  const overInbox = drag?.over?.key === "inbox";
  const overOther = drag?.over != null && drag.over.key !== "inbox";
  useEffect(() => {
    if (!many) return;
    if (!dragging) {
      if (menu?.via === "drag") close();
      return;
    }
    if (overOther) close();
    else if (overInbox && !isOpen) later(() => open("drag"), MENU_DRAG_DELAY);
    else if (!isOpen) window.clearTimeout(timer.current);
  }, [dragging, overInbox, overOther, isOpen]);

  return (
    <div
      ref={anchor}
      className="relative flex w-full justify-center"
      onPointerEnter={() => {
        if (!many) return;
        if (isOpen) window.clearTimeout(timer.current);
        else later(() => open("hover"), MENU_OPEN_DELAY);
      }}
      onPointerLeave={() => {
        if (!isOpen) window.clearTimeout(timer.current);
        else if (menu.via === "hover") later(close, MENU_CLOSE_DELAY);
      }}
    >
      {/* With a menu to show, it takes the tooltip's place. */}
      <RailButton
        title="Inbox"
        tip={!many}
        active={inbox}
        dot={counts.inbox > 0}
        drop={drop("inbox")}
        onClick={() => select("all")}
      >
        <Inbox size={18} />
        {narrowed && <NarrowedMark account={narrowed} />}
      </RailButton>
      {many && (
        <button
          ref={arrow}
          aria-label="Inbox accounts"
          aria-haspopup="menu"
          aria-expanded={isOpen}
          className={`absolute right-1.5 top-1/2 grid size-4 -translate-y-1/2 place-items-center rounded transition-[color,transform] duration-150 hover:text-white ${
            isOpen ? "translate-x-0.5 text-white/70" : "text-neutral-content/40"
          }`}
          // A click pins a menu the pointer opened; Enter or Space also moves focus into it.
          onClick={(e) => (isOpen && menu.via !== "hover" ? close() : open(e.detail === 0 ? "key" : "click"))}
        >
          <ChevronRight size={11} />
        </button>
      )}
      {many && menu && (
        <AccountsMenu
          menuRef={menuRef}
          top={menu.top}
          left={menu.left}
          focus={menu.via === "key"}
          inbox={inbox}
          account={account}
          counts={counts}
          folders={folders}
          onSelect={select}
          onClose={close}
        />
      )}
    </div>
  );
}

/**
 * The Inbox's accounts beside the rail: All Inboxes, then each account with its
 * address (or sync error) and unread count. Rows take a dragged message.
 */
function AccountsMenu({
  menuRef,
  top,
  left,
  focus,
  inbox,
  account,
  counts,
  folders,
  onSelect,
  onClose,
}: {
  menuRef: RefObject<HTMLDivElement | null>;
  /** The rail's Inbox row: the menu lines up with its top, just over the rail's edge. */
  top: number;
  left: number;
  /** Opened from the keyboard: focus the checked row. */
  focus: boolean;
  inbox: boolean;
  account: AccountFilter;
  counts: SidebarCounts;
  folders: FolderView[];
  onSelect: (account: AccountFilter) => void;
  onClose: () => void;
}) {
  const accounts = useAccounts();
  const drop = useRowDrop(folders);
  const [y, setY] = useState(top - 6);

  // Kept inside the window.
  useLayoutEffect(() => {
    const el = menuRef.current;
    if (el) setY(Math.max(8, Math.min(top - 6, window.innerHeight - el.offsetHeight - 8)));
  }, [top]);

  useEffect(() => {
    if (!focus) return;
    const items = [...(menuRef.current?.querySelectorAll<HTMLElement>("[role='menuitemradio']") ?? [])];
    (items.find((i) => i.getAttribute("aria-checked") === "true") ?? items[0])?.focus();
  }, []);

  function onKeyDown(e: ReactKeyboardEvent<HTMLDivElement>) {
    const items = [...e.currentTarget.querySelectorAll<HTMLElement>("[role='menuitemradio']")];
    const at = items.indexOf(document.activeElement as HTMLElement);
    const move = (i: number) => items[(i + items.length) % items.length]?.focus();
    switch (e.key) {
      case "ArrowDown":
        move(at + 1);
        break;
      case "ArrowUp":
        move(at < 0 ? -1 : at - 1);
        break;
      case "Home":
        move(0);
        break;
      case "End":
        move(-1);
        break;
      case "ArrowLeft":
        onClose();
        break;
      case "Tab":
        // Focus goes back to the arrow, and Tab moves on from there.
        onClose();
        return;
      default:
        return;
    }
    e.preventDefault();
    e.stopPropagation();
  }

  return (
    <div
      ref={menuRef}
      role="menu"
      aria-label="Inbox accounts"
      onKeyDown={onKeyDown}
      className="flyout-in fixed z-[2020] max-h-[calc(100vh-16px)] w-64 overflow-y-auto rounded-xl bg-[#34373c] p-1.5 text-neutral-content shadow-[0_10px_30px_rgba(0,0,0,.45)] ring-1 ring-white/10"
      style={{ left: left - 4, top: y }}
    >
      <AccountsMenuRow
        icon={
          <span className="grid size-7 place-items-center rounded-lg bg-white/10 text-white">
            <Inbox size={15} />
          </span>
        }
        label="All Inboxes"
        count={counts.inbox}
        checked={inbox && account === "all"}
        drop={drop("inbox")}
        onClick={() => onSelect("all")}
      />
      <div role="separator" className="mx-2 my-1 h-px bg-white/10" />
      {accounts.map((a, i) => {
        const error = a.sync.status === "error";
        return (
          <AccountsMenuRow
            key={a.id}
            delay={40 + i * 25}
            icon={<AccountTile account={a} size={28} />}
            label={a.label}
            detail={error ? (a.sync.message ?? "Couldn't sync") : a.email}
            alert={error}
            count={counts.accounts[a.id] ?? 0}
            checked={inbox && account === a.id}
            drop={drop("inbox", a.id)}
            onClick={() => onSelect(a.id)}
          />
        );
      })}
    </div>
  );
}

function AccountsMenuRow({
  icon,
  label,
  detail,
  alert = false,
  count,
  checked,
  drop,
  delay,
  onClick,
}: {
  icon: ReactNode;
  label: string;
  detail?: string;
  /** A sync error: `detail` says what, and it shows in place of the count. */
  alert?: boolean;
  count: number;
  checked: boolean;
  drop?: RowDrop;
  /** The rows slide in a beat after the menu, one after another. */
  delay?: number;
  onClick: () => void;
}) {
  return (
    <button
      role="menuitemradio"
      aria-checked={checked}
      tabIndex={-1}
      {...dropAttrs(drop?.target)}
      onClick={onClick}
      style={delay ? { animationDelay: `${delay}ms` } : undefined}
      className={`flex w-full items-center gap-2.5 rounded-lg px-1.5 py-1.5 text-left outline-none ${
        drop?.over ? DROP_OVER : checked ? "bg-white/[.08]" : "hover:bg-white/[.06] focus-visible:bg-white/[.06]"
      } ${drop && !drop.target ? "opacity-40" : ""}`}
    >
      {icon}
      <span className="min-w-0 flex-1">
        <span className={`block truncate text-[13px] ${checked ? "font-semibold text-white" : "text-white/90"}`}>{label}</span>
        {detail && (
          <span className={`block truncate text-[11px] ${alert ? "text-error" : "text-neutral-content/70"}`}>{detail}</span>
        )}
      </span>
      {alert ? (
        <CircleAlert size={14} className="shrink-0 text-error" />
      ) : (
        count > 0 && <span className="text-xs tabular-nums text-neutral-content/80">{count}</span>
      )}
      <span className="grid w-4 shrink-0 place-items-center text-primary">{checked && <Check size={14} />}</span>
    </button>
  );
}

/** The account a rail list is narrowed to, on its icon's corner. */
function NarrowedMark({ account }: { account: AccountView }) {
  return (
    <span className="absolute -bottom-1 -right-1 rounded ring-2 ring-neutral">
      <AccountTile account={account} size={14} />
    </span>
  );
}

function RailButton({
  title,
  tip = true,
  active = false,
  dot = false,
  alert = false,
  drop,
  onClick,
  onContextMenu,
  children,
}: {
  title: Tip;
  /** Whether `title` shows as a tooltip; it's the button's label either way. */
  tip?: boolean;
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
      {...(tip ? tipAttrs(title) : {})}
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
        const next = slotAt(scroller.current, x, y, key);
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

  const toggle = (key: ListKey, open: boolean, under: "accounts" | "subfolders" = "accounts") => ({
    open,
    label: `${open ? "Hide" : "Show"} ${under}`,
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

  /** A subfolder under its parent's row, and its own subfolders when it's expanded. */
  const subfolderRow = (f: FolderView, level: number): ReactNode => {
    const key = folderKey(f.id);
    const i = info(key);
    if (!i) return null;
    const children = level < SUBFOLDER_DEPTH ? subfolders(key, folders) : [];
    const open = children.length > 0 && layout.expanded.includes(key);
    return (
      <Fragment key={key}>
        <SidebarRow
          level={level}
          icon={<ListIcon info={i} color={layout.colors[key]} />}
          label={i.label}
          title={i.detail}
          count={hidden(key) ? 0 : (counts.lists[key] ?? 0)}
          active={current === key}
          toggle={children.length ? toggle(key, open, "subfolders") : undefined}
          drop={drop(key)}
          onClick={() => {
            if (f.selectable) actions.select(key);
            // A folder that only holds folders opens and closes instead.
            else if (children.length) actions.layout((l) => toggleExpanded(l, key));
          }}
          onContextMenu={f.selectable ? (e) => onMenu(key, "other", e) : undefined}
        />
        {open && children.map((c) => subfolderRow(c, level + 1))}
      </Fragment>
    );
  };

  /** A list's row and, when it's expanded, one row per account or its subfolders under it. */
  const row = (key: ListKey, section: Section, index: number) => {
    const i = info(key);
    if (!i) return null;
    const expandable = accounts.length > 1 && hasAccountRows(key);
    const children = subfolders(key, folders);
    const open = (expandable || children.length > 0) && layout.expanded.includes(key);
    return (
      <div key={key} data-item={index} className={`relative ${moving?.key === key ? "opacity-50" : ""}`}>
        {line(section, index, "top")}
        <SidebarRow
          icon={<ListIcon info={i} color={layout.colors[key] ?? i.color} />}
          label={i.label}
          title={i.detail}
          count={hidden(key) ? 0 : (counts.lists[key] ?? 0)}
          active={current === key && (account === "all" || !open)}
          toggle={expandable ? toggle(key, open) : children.length ? toggle(key, open, "subfolders") : undefined}
          drop={drop(key)}
          onClick={() => actions.select(key)}
          onContextMenu={(e) => onMenu(key, section, e)}
          onPointerDown={(e) => startMove(e, key, i.label)}
        />
        {open &&
          expandable &&
          accounts.map((a) => accountRow(key, a, hidden(key) ? 0 : (counts.listAccounts[key]?.[a.id] ?? 0)))}
        {open && children.map((f) => subfolderRow(f, 1))}
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

      {/* The left padding holds the expand arrows, as in Spark. */}
      <OverlayScroll viewportRef={scroller} className="min-h-0 flex-1" viewportClassName="pb-2 pl-5 pr-2 pt-1">
        {props.home && (
          <SidebarRow
            icon={<House size={18} />}
            label="Home Screen"
            count={0}
            active={props.home.open}
            onClick={actions.home}
          />
        )}
        {/* The Inbox stays first among the lists; others can be dragged around it. */}
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

        {layout.labels.length > 0 && (
          <>
            <SectionLabel
              open={layout.labelsOpen}
              onToggle={() => actions.layout((l) => ({ ...l, labelsOpen: !l.labelsOpen }))}
            >
              Auto-Labels
            </SectionLabel>
            {layout.labelsOpen && sectionRows("labels")}
          </>
        )}

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
      </OverlayScroll>

      <div className="shrink-0 pb-3 pl-5 pr-2">
        <button
          className="flex h-[34px] w-full items-center gap-2 rounded-lg px-2 text-[13px] hover:bg-sidebar-hover hover:text-white"
          onClick={actions.settings}
        >
          <SettingsAvatar updateAvailable={props.updateAvailable} />
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
        <span className="absolute -left-3.5 top-0 grid h-[22px] w-3.5 place-items-center">
          <ChevronRight size={12} className={`transition-transform ${open ? "rotate-90" : ""}`} />
        </span>
      )}
      <span className="truncate">{children}</span>
    </>
  );
  return (
    <div className="group relative mb-px mt-5 flex min-h-[22px] items-center gap-1 pl-2 pr-1 text-[13px] leading-[22px]">
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
  level = 0,
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
  /** An account's row: its content is indented. */
  indent?: boolean;
  /** A subfolder's depth: the whole row moves in, its arrow with it. */
  level?: number;
  /** An expand toggle in the padding to the row's left, e.g. the Inbox's accounts. */
  toggle?: { open: boolean; label: string; onToggle: () => void };
  drop?: RowDrop;
  onClick: () => void;
  onContextMenu?: (e: MouseEvent) => void;
  /** Starts dragging the row to another place. */
  onPointerDown?: (e: ReactPointerEvent<HTMLElement>) => void;
}) {
  return (
    <div
      className={`group relative mb-0.5 ${drop && !drop.target ? "opacity-40" : ""}`}
      style={level ? { marginLeft: 10 * Math.min(level, 5) } : undefined}
    >
      {toggle && (
        <button
          aria-label={toggle.label}
          aria-expanded={toggle.open}
          className="absolute -left-3.5 top-0 grid h-[30px] w-3.5 place-items-center opacity-50 hover:text-white hover:opacity-100"
          onClick={toggle.onToggle}
        >
          <ChevronRight size={12} className={`transition-transform ${toggle.open ? "rotate-90" : ""}`} />
        </button>
      )}
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
            <span className={`text-[11px] font-medium leading-[13px] ${active ? "" : "opacity-50"}`}>{count}</span>
          )
        )}
      </button>
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

/**
 * The gap nearest the pointer in a section that takes `key` (the top section, and Folders or
 * Auto-Labels), while the pointer is over the sidebar's lists.
 */
function slotAt(scroller: HTMLElement | null, x: number, y: number, key: ListKey): Slot | null {
  const box = scroller?.getBoundingClientRect();
  if (!scroller || !box || x < box.left || x > box.right || y < box.top || y > box.bottom) return null;
  let best: Slot | null = null;
  let distance = Infinity;
  for (const section of (["favorites", "folders", "labels"] as const).filter((s) => accepts(s, key))) {
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
  const countable =
    key === "snoozed" ||
    key === "aside" ||
    key === "drafts" ||
    key === "spam" ||
    folder !== undefined ||
    autoLabelOf(key) !== undefined;

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
      {section !== "other"
        ? item(section === "recents" ? "Remove from Recents" : "Remove from Sidebar", change((l) => removeFromSidebar(l, key)))
        : !inSidebar(layout, key) && item("Add to Sidebar", change((l) => addToSidebar(l, key)))}
      {countable && item(layout.hiddenBadges.includes(key) ? "Show Badge" : "Hide Badge", change((l) => toggleBadge(l, key)))}
      {key !== "drafts" && item("Mark All as Read", () => actions.markAllRead(key))}
      {actions.canEmpty(key) &&
        item(`Empty ${name || (isSystemList(key) ? SYSTEM_LISTS[key].label : "Folder")}…`, () => actions.empty(key), true)}
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

function SettingsAvatar({ onClick, updateAvailable }: { onClick?: () => void; updateAvailable?: boolean }) {
  const face = (
    <span className="relative grid size-[22px] place-items-center rounded-full bg-[#6d3ecf] text-[10px] font-semibold text-white">
      M
      {updateAvailable && <span role="status" aria-label="Update available" className="absolute -right-0.5 -top-0.5 size-2 rounded-full bg-primary ring-2 ring-neutral" />}
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
