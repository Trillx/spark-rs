import { CircleAlert, Inbox, Lock, LockOpen, PanelLeftOpen, RefreshCw } from "lucide-react";
import { useState, type MouseEvent, type ReactNode } from "react";
import type { AccountView } from "../types";
import { useAccounts } from "../lib/accounts";
import { FOLDERS, type AccountFilter, type Folder, type SidebarCounts } from "../lib/layout";
import { SIDEBAR_DEFAULT, SIDEBAR_MAX, SIDEBAR_MIN, clamp, startHorizontalDrag } from "../lib/panes";
import { hint, shortcutLabel } from "../lib/shortcuts";
import AccountMark from "./AccountMark";
import { tipAttrs, tipLabel, type Tip } from "../lib/tooltip";

export type SidebarMode = "rail" | "overlay" | "locked";

interface Props {
  mode: SidebarMode;
  width: number;
  folder: Folder;
  account: AccountFilter;
  counts: SidebarCounts;
  /** Some account is syncing: the sync button spins until it's done. */
  syncing: boolean;
  onSelect: (folder: Folder, account?: AccountFilter) => void;
  onOpen: () => void;
  onClose: () => void;
  onToggleLock: () => void;
  onResize: (width: number) => void;
  onSync: () => void;
  onSettings: () => void;
}

/**
 * Always dark. By default a 72px icon rail; `/` or the rail's open
 * button shows the full sidebar as an overlay, and the lock button docks it.
 */
export default function Sidebar(props: Props) {
  const { mode, width, onClose } = props;
  if (mode === "locked") return <Expanded {...props} />;
  return (
    <>
      <Rail {...props} />
      {mode === "overlay" && (
        <>
          <div className="fixed inset-0 z-[101]" onClick={onClose} />
          <div
            className="fixed inset-y-0 left-0 z-[102] shadow-[10px_4px_20px_rgba(0,0,0,.25)]"
            style={{ width }}
          >
            <Expanded {...props} />
          </div>
        </>
      )}
    </>
  );
}

function Rail({ folder, account, counts, onSelect, onOpen, onSettings }: Props) {
  const accounts = useAccounts();
  // Clicking empty rail space opens the sidebar, like Spark.
  const openOnEmpty = (e: MouseEvent) => {
    if (e.target === e.currentTarget) onOpen();
  };
  return (
    <nav
      aria-label="Sidebar"
      className="flex h-full w-[72px] shrink-0 flex-col items-center gap-2 bg-neutral pb-3 text-neutral-content"
      onClick={openOnEmpty}
    >
      <div data-tauri-drag-region className="h-8 w-full shrink-0" />
      <RailButton
        title="Inbox"
        active={folder === "inbox" && account === "all"}
        dot={counts.inbox > 0}
        onClick={() => onSelect("inbox", "all")}
      >
        <Inbox size={18} />
      </RailButton>
      {accounts.map((a) => (
        <RailButton
          key={a.id}
          title={accountTitle(a)}
          active={folder === "inbox" && account === a.id}
          alert={a.sync.status === "error"}
          onClick={() => onSelect("inbox", a.id)}
        >
          <AccountMark color={a.color} />
        </RailButton>
      ))}
      <span className="my-1 h-px w-6 bg-white/10" />
      {FOLDERS.map(({ key, label, Icon }) => (
        <RailButton key={key} title={label} active={folder === key} onClick={() => onSelect(key)}>
          <Icon size={18} />
        </RailButton>
      ))}
      <div className="flex-1 self-stretch" onClick={openOnEmpty} />
      <RailButton title={hint("Open sidebar", "sidebar")} onClick={onOpen}>
        <PanelLeftOpen size={18} />
      </RailButton>
      <SettingsAvatar onClick={onSettings} />
    </nav>
  );
}

function RailButton({
  title,
  active = false,
  dot = false,
  alert = false,
  onClick,
  children,
}: {
  title: Tip;
  active?: boolean;
  /** Unread mail. */
  dot?: boolean;
  /** A sync error. */
  alert?: boolean;
  onClick: () => void;
  children: ReactNode;
}) {
  return (
    <button
      {...tipAttrs(title)}
      data-tip-side="right"
      aria-label={tipLabel(title)}
      aria-current={active || undefined}
      onClick={onClick}
      className={`relative grid size-[30px] shrink-0 place-items-center rounded-lg ${
        active ? "bg-sidebar-active text-white" : "hover:bg-sidebar-hover hover:text-white"
      }`}
    >
      {children}
      {(dot || alert) && (
        <span className={`absolute right-0.5 top-0.5 size-1.5 rounded-full ${alert ? "bg-error" : "bg-primary"}`} />
      )}
    </button>
  );
}

function Expanded({
  mode,
  width,
  folder,
  account,
  counts,
  syncing,
  onSelect,
  onToggleLock,
  onResize,
  onSync,
  onSettings,
}: Props) {
  const accounts = useAccounts();
  const [clicks, setClicks] = useState(0);
  const locked = mode === "locked";

  function sync() {
    setClicks((n) => n + 1);
    onSync();
  }

  return (
    <nav
      aria-label="Sidebar"
      className="relative flex h-full shrink-0 flex-col bg-neutral text-neutral-content"
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
        <SidebarIconButton title={locked ? "Unlock sidebar" : "Lock sidebar"} onClick={onToggleLock}>
          {locked ? <Lock size={15} /> : <LockOpen size={15} />}
        </SidebarIconButton>
      </div>

      <div className="flex-1 overflow-y-auto px-2 pb-2 pt-1">
        <SidebarRow
          icon={<Inbox size={18} />}
          label="Inbox"
          count={counts.inbox}
          active={folder === "inbox" && account === "all"}
          onClick={() => onSelect("inbox", "all")}
        />
        {accounts.map((a) => (
          <SidebarRow
            key={a.id}
            indent
            icon={<AccountMark color={a.color} />}
            label={a.label}
            title={accountTitle(a)}
            count={counts.accounts[a.id] ?? 0}
            alert={a.sync.status === "error" ? (a.sync.message ?? "Couldn't sync") : undefined}
            active={folder === "inbox" && account === a.id}
            onClick={() => onSelect("inbox", a.id)}
          />
        ))}

        <div className="mb-px mt-5 px-2 text-[11px] font-medium uppercase tracking-wide opacity-50">Folders</div>
        {FOLDERS.map(({ key, label, Icon }) => (
          <SidebarRow
            key={key}
            icon={<Icon size={18} />}
            label={label}
            count={key === "snoozed" ? counts.snoozed : key === "aside" ? counts.aside : 0}
            active={folder === key}
            onClick={() => onSelect(key)}
          />
        ))}
      </div>

      <div className="shrink-0 px-2 pb-3">
        <button
          className="flex h-[34px] w-full items-center gap-2 rounded-lg px-2 text-[13px] hover:bg-sidebar-hover hover:text-white"
          onClick={onSettings}
        >
          <SettingsAvatar />
          Settings
        </button>
      </div>

      {locked && (
        <div
          data-tip="Drag to resize, double-click to reset"
          className="absolute inset-y-0 -right-px w-[3px] cursor-col-resize hover:bg-primary"
          onPointerDown={(e) => startHorizontalDrag(e, (dx) => onResize(clamp(width + dx, SIDEBAR_MIN, SIDEBAR_MAX)))}
          onDoubleClick={() => onResize(SIDEBAR_DEFAULT)}
        />
      )}
    </nav>
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
  onClick,
}: {
  icon: ReactNode;
  label: string;
  title?: string;
  count: number;
  /** A sync error, shown as an icon in place of the count. */
  alert?: string;
  active: boolean;
  indent?: boolean;
  onClick: () => void;
}) {
  return (
    <button
      data-tip={title}
      data-tip-side="right"
      aria-current={active || undefined}
      onClick={onClick}
      className={`mb-0.5 flex h-[30px] w-full items-center gap-2 rounded-lg pr-2.5 text-left text-[13px] ${
        indent ? "pl-7" : "pl-2"
      } ${active ? "bg-sidebar-selected text-white" : "hover:bg-sidebar-hover hover:text-white"}`}
    >
      <span className="grid w-5 shrink-0 place-items-center">{icon}</span>
      <span className="min-w-0 flex-1 truncate">{label}</span>
      {alert ? (
        <span data-tip={alert} className="text-error">
          <CircleAlert size={14} aria-label={alert} />
        </span>
      ) : count > 0 && (
        <span className={`text-[11px] font-medium leading-[13px] ${active ? "" : "opacity-50"}`}>{count}</span>
      )}
    </button>
  );
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
