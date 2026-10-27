import { Folder, FolderSearch, Plus, Star, Tag, X } from "lucide-react";
import { useState, type KeyboardEvent, type MouseEvent, type ReactNode } from "react";
import type { AccountId, AccountView, FolderView } from "../types";
import { useAccounts } from "../lib/accounts";
import { AUTO_LABELS, AUTO_LABEL_ORDER } from "../lib/autoLabels";
import { IS_MAC } from "../lib/shortcuts";
import {
  SYSTEM_LISTS,
  SYSTEM_ORDER,
  autoKey,
  folderKey,
  folderPath,
  inSidebar,
  isUserFolder,
  smartKey,
  type ListKey,
  type SidebarLayout,
} from "../lib/sidebar";
import AccountMark from "./AccountMark";
import OverlayScroll from "./OverlayScroll";

interface Props {
  /** The sidebar's width: the picker opens flush against it. */
  left: number;
  layout: SidebarLayout;
  folders: FolderView[];
  /** Demo folders can't be changed. */
  demo: boolean;
  /** `add`: Ctrl/Cmd-click or the star, which adds without opening. */
  onPick: (key: ListKey, add: boolean) => void;
  onRemove: (key: ListKey) => void;
  onNewSmartFolder: () => void;
  onNewFolder: (account: AccountId, parent?: number) => void;
  onHideTip: () => void;
  onClose: () => void;
}

/**
 * "More": every list that can go in the sidebar, in a 250px panel beside
 * it. Lists already in the sidebar have a filled star; a click opens a list
 * (it shows under Recents), Ctrl-click or the outline star adds it.
 */
export default function FolderPicker(props: Props) {
  const { layout, folders } = props;
  const accounts = useAccounts();
  const [scrolled, setScrolled] = useState(false);
  const added = (key: ListKey) => inSidebar(layout, key);
  const star = (key: ListKey) => (added(key) ? props.onRemove(key) : props.onPick(key, true));

  function onKeyDown(e: KeyboardEvent) {
    if (e.key !== "Escape") return;
    e.stopPropagation();
    props.onClose();
  }

  return (
    <div
      role="dialog"
      aria-label="All Folders"
      className="fixed top-0 z-[102] flex h-screen w-[250px] flex-col bg-[#2f3237] text-neutral-content shadow-[0_0_20px_rgba(0,0,0,.35)]"
      style={{ left: props.left }}
      onKeyDown={onKeyDown}
    >
      <OverlayScroll
        className="min-h-0 flex-1"
        viewportClassName={`px-2 pb-4 pt-8 ${scrolled ? "shadow-[inset_0_8px_8px_-8px_rgba(0,0,0,.5)]" : ""}`}
        onScroll={(e) => setScrolled(e.currentTarget.scrollTop > 0)}
      >
        <Heading>All Folders</Heading>
        <ul>
          {SYSTEM_ORDER.filter((key) => !added(key)).map((key) => {
            const { label, Icon } = SYSTEM_LISTS[key];
            return (
              <li key={key}>
                <Row
                  icon={<Icon size={16} />}
                  label={label}
                  added={false}
                  onPick={(add) => props.onPick(key, add)}
                  onStar={() => star(key)}
                />
              </li>
            );
          })}
        </ul>

        <Heading
          action={
            <button
              data-tip="New Smart Folder"
              aria-label="New Smart Folder"
              className="grid size-5 place-items-center rounded text-[#66707e] hover:text-neutral-content"
              onClick={props.onNewSmartFolder}
            >
              <Plus size={14} />
            </button>
          }
        >
          Smart Folders
        </Heading>
        <ul>
          {layout.smartFolders.map((f) => {
            const key = smartKey(f.id);
            return (
              <li key={f.id}>
                <Row
                  icon={<FolderSearch size={16} style={colorStyle(layout, key)} />}
                  label={f.name}
                  title={f.query}
                  added={added(key)}
                  onPick={(add) => props.onPick(key, add)}
                  onStar={() => star(key)}
                />
              </li>
            );
          })}
        </ul>

        {layout.autoOffered.length > 0 && (
          <>
            <Heading>Auto-Labels</Heading>
            <ul>
              {AUTO_LABEL_ORDER.map((label) => {
                const key = autoKey(label);
                const { name, color } = AUTO_LABELS[label];
                return (
                  <li key={label}>
                    <Row
                      icon={<Tag size={16} style={{ color: layout.colors[key] ?? color }} />}
                      label={name}
                      added={added(key)}
                      onPick={(add) => props.onPick(key, add)}
                      onStar={() => star(key)}
                    />
                  </li>
                );
              })}
            </ul>
          </>
        )}

        {accounts.map((a) => (
          <AccountSection key={a.id} account={a} {...props} added={added} star={star} />
        ))}
      </OverlayScroll>

      {!layout.tipHidden && (
        <div className="group flex shrink-0 items-start gap-2 border-t border-white/5 px-4 py-2.5 text-xs leading-[18px] text-[#66707e]">
          <span className="flex-1">
            Use <b className="font-semibold text-neutral-content">{IS_MAC ? "Cmd" : "Ctrl"}</b> to add multiple folders to the
            Sidebar
          </span>
          <button
            aria-label="Close tip"
            className="invisible mt-0.5 grid size-4 place-items-center rounded hover:text-neutral-content group-hover:visible"
            onClick={props.onHideTip}
          >
            <X size={14} />
          </button>
        </div>
      )}
    </div>
  );
}

function AccountSection({
  account,
  folders,
  layout,
  demo,
  onPick,
  onNewFolder,
  added,
  star,
}: Props & { account: AccountView; added: (key: ListKey) => boolean; star: (key: ListKey) => void }) {
  const own = folders.filter((f) => f.account === account.id && isUserFolder(f));
  const ids = new Set(own.map((f) => f.id));
  // Nested in a folder that isn't listed (e.g. a system folder): shown at the top.
  const children = (parent: number | undefined) =>
    own.filter((f) => (parent === undefined ? f.parent === undefined || !ids.has(f.parent) : f.parent === parent));

  const tree = (parent: number | undefined, level: number): ReactNode[] =>
    children(parent).flatMap((f) => {
      const key = folderKey(f.id);
      return [
        <li key={f.id}>
          <Row
            icon={<Folder size={16} style={colorStyle(layout, key)} />}
            label={f.name}
            title={folderPath(f, folders)}
            level={level}
            added={added(key)}
            disabled={!f.selectable}
            onPick={(add) => onPick(key, add)}
            onStar={() => star(key)}
            onAdd={demo ? undefined : () => onNewFolder(account.id, f.id)}
          />
        </li>,
        ...tree(f.id, level + 1),
      ];
    });

  return (
    <section aria-label={account.label} className="mt-4">
      <div className="group flex h-[30px] items-center gap-2 px-1.5 text-[13px] text-[#66707e]">
        <AccountMark color={account.color} />
        <span className="min-w-0 flex-1 truncate" title={account.email}>
          {account.label}
        </span>
        {!demo && (
          <button
            data-tip="New Folder"
            aria-label={`New folder in ${account.label}`}
            className="grid size-5 place-items-center rounded opacity-0 hover:text-neutral-content group-hover:opacity-100"
            onClick={() => onNewFolder(account.id)}
          >
            <Plus size={14} />
          </button>
        )}
      </div>
      <ul>{tree(undefined, 0)}</ul>
      {own.length === 0 && <p className="px-1.5 text-xs leading-[30px] text-[#66707e]">No folders yet</p>}
    </section>
  );
}

function Heading({ children, action }: { children: ReactNode; action?: ReactNode }) {
  return (
    <div className="flex items-center gap-2 px-1.5 pb-1 pt-4 first:pt-0">
      <h2 className="flex-1 text-[13px] font-normal leading-4 text-[#66707e]">{children}</h2>
      {action}
    </div>
  );
}

function Row({
  icon,
  label,
  title,
  added,
  level = 0,
  disabled = false,
  onPick,
  onStar,
  onAdd,
}: {
  icon: ReactNode;
  label: string;
  title?: string;
  added: boolean;
  level?: number;
  /** A folder that only holds folders. */
  disabled?: boolean;
  onPick: (add: boolean) => void;
  onStar: () => void;
  onAdd?: () => void;
}) {
  const pick = (e: MouseEvent | KeyboardEvent) => {
    if (!disabled) onPick(IS_MAC ? e.metaKey : e.ctrlKey);
  };
  return (
    <div
      role="button"
      tabIndex={disabled ? -1 : 0}
      aria-disabled={disabled || undefined}
      aria-label={added ? `${label}, in the sidebar` : label}
      data-tip={title !== label ? title : undefined}
      data-tip-side="right"
      className={`group flex h-[30px] cursor-default items-center gap-2 rounded pr-1 text-[13px] outline-none hover:bg-white/10 focus-visible:bg-white/10 ${
        added ? "text-white" : ""
      } ${disabled ? "opacity-50" : ""}`}
      style={{ paddingLeft: 6 + 10 * Math.min(level, 10) }}
      onClick={pick}
      onKeyDown={(e) => {
        if (e.key === "Enter" || e.key === " ") {
          e.preventDefault();
          pick(e);
        }
      }}
    >
      <span className="grid w-5 shrink-0 place-items-center">{icon}</span>
      <span className="min-w-0 flex-1 truncate">{label}</span>
      {onAdd && (
        <button
          data-tip="New Subfolder"
          aria-label={`New subfolder in ${label}`}
          className="grid size-5 place-items-center rounded text-[#66707e] opacity-0 hover:text-neutral-content group-hover:opacity-100"
          onClick={(e) => {
            e.stopPropagation();
            onAdd();
          }}
        >
          <Plus size={14} />
        </button>
      )}
      {!disabled && (
        <button
          aria-label={added ? `Remove ${label} from the sidebar` : `Add ${label} to the sidebar`}
          data-tip={added ? "Remove from Sidebar" : "Add to Sidebar"}
          className={`grid size-5 place-items-center rounded text-[#66707e] hover:text-neutral-content ${
            added ? "" : "opacity-0 group-hover:opacity-100 group-focus-visible:opacity-100"
          }`}
          onClick={(e) => {
            e.stopPropagation();
            onStar();
          }}
        >
          <Star size={14} className={added ? "fill-current" : ""} />
        </button>
      )}
    </div>
  );
}

function colorStyle(layout: SidebarLayout, key: ListKey) {
  const color = layout.colors[key];
  return color ? { color } : undefined;
}
