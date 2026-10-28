import {
  ArrowDownLeft,
  Check,
  Circle,
  Clock,
  FolderInput,
  Inbox,
  MailX,
  Pin,
  ShieldAlert,
  ShieldCheck,
  Trash2,
  X,
  Zap,
  type LucideIcon,
} from "lucide-react";
import { laterPresets, plural, presetDetail, type LaterPreset } from "../lib/format";
import { hint } from "../lib/shortcuts";
import { tipAttrs, tipLabel, type Tip } from "../lib/tooltip";
import Dropdown, { MenuItem } from "./Dropdown";

/** What the selected conversations have in common, for the buttons' labels. */
export interface Selection {
  count: number;
  allDone: boolean;
  /** Some email in them is unread: reading reads them all. */
  anyUnread: boolean;
  allPinned: boolean;
  allPriority: boolean;
  allAside: boolean;
  allSnoozed: boolean;
  allSpam: boolean;
  allDeleted: boolean;
  /** Some of them came from a mailing list that can be left. */
  anyUnsubscribe: boolean;
}

export interface BulkActions {
  done: () => void;
  trash: () => void;
  read: () => void;
  pin: () => void;
  priority: () => void;
  aside: () => void;
  snooze: (preset: LaterPreset) => void;
  /** Asks for a day and time. */
  snoozeCustom: () => void;
  unsnooze: () => void;
  spam: () => void;
  /** Opens Move to folder. */
  move: () => void;
  /** Unsubscribes from the lists they came from, once per sender. */
  unsubscribe: () => void;
  clear: () => void;
}

/**
 * The reader while conversations are selected (Ctrl- or Shift-click, Ctrl+A),
 * as in Spark: how many, and what can be done to all of them at once. The
 * single-key shortcuts act on the selection too. `compact` is a bar under
 * the list, for when there's no room for the reader.
 */
export default function BulkPanel({
  selection: s,
  actions,
  compact = false,
}: {
  selection: Selection;
  actions: BulkActions;
  compact?: boolean;
}) {
  if (compact) {
    return (
      <div className="flex shrink-0 items-center gap-1 border-t border-hairline px-3 py-2">
        <span className="mr-auto text-[13px] font-medium">{s.count} selected</span>
        <Buttons selection={s} actions={actions} />
        <button className={BUTTON} {...tipAttrs({ label: "Clear selection", keys: "Esc" })} aria-label="Clear selection" onClick={actions.clear}>
          <X size={18} />
        </button>
      </div>
    );
  }
  return (
    <div className="grid flex-1 place-items-center px-6 text-center">
      <div>
        <span className="mx-auto grid size-20 place-items-center rounded-full bg-primary/10 text-2xl font-semibold text-primary">
          {s.count > 999 ? "999+" : s.count}
        </span>
        <p className="mt-3 text-sm font-medium">{plural(s.count, "conversation")} selected</p>
        <div className="mt-5 flex flex-wrap justify-center gap-1">
          <Buttons selection={s} actions={actions} />
        </div>
        <button
          className="mt-5 inline-flex items-center gap-1 text-[13px] text-text-2 hover:text-base-content"
          {...tipAttrs({ label: "Clear selection", keys: "Esc" })}
          onClick={actions.clear}
        >
          <X size={14} />
          Clear selection
        </button>
      </div>
    </div>
  );
}

function Buttons({ selection: s, actions }: { selection: Selection; actions: BulkActions }) {
  return (
    <>
          <Action
            tip={s.allDone ? "Move to Inbox" : hint("Done", "done")}
            Icon={s.allDone ? Inbox : Check}
            onClick={actions.done}
          />
          <Dropdown
            trigger={({ toggle, open }) => (
              <button
                className={`${BUTTON} ${open ? "bg-base-content/10" : ""}`}
                {...tipAttrs(hint("Snooze", "snooze"))}
                aria-label="Snooze"
                aria-haspopup="menu"
                aria-expanded={open}
                onClick={toggle}
              >
                <Clock size={18} />
              </button>
            )}
          >
            {s.allSnoozed && <MenuItem label="Discard Snooze" onSelect={actions.unsnooze} />}
            {laterPresets().map((p) => (
              <MenuItem key={p.key} label={p.label} detail={presetDetail(p)} onSelect={() => actions.snooze(p)} />
            ))}
            <MenuItem label="Pick date & time…" onSelect={actions.snoozeCustom} />
          </Dropdown>
          <Action
            tip={hint(s.allPinned ? "Unpin" : "Pin", "pin")}
            Icon={Pin}
            iconClass={s.allPinned ? "fill-current text-pin" : ""}
            onClick={actions.pin}
          />
          <Action
            tip={hint(s.anyUnread ? "Mark as Read" : "Mark as Unread", "read")}
            Icon={Circle}
            iconClass={s.anyUnread ? "fill-current text-primary" : ""}
            onClick={actions.read}
          />
          <Action
            tip={hint(s.allPriority ? "Remove from Priority" : "Mark as Priority", "priority")}
            Icon={Zap}
            iconClass={s.allPriority ? "fill-current text-priority" : ""}
            onClick={actions.priority}
          />
          <Action
            tip={s.allAside ? "Move back to Inbox" : hint("Set Aside", "aside")}
            Icon={ArrowDownLeft}
            onClick={actions.aside}
          />
          <Action tip={hint("Move to Folder…", "move")} Icon={FolderInput} onClick={actions.move} />
          {s.anyUnsubscribe && <Action tip="Unsubscribe" Icon={MailX} onClick={actions.unsubscribe} />}
          <Action
            tip={s.allSpam ? "Not Spam" : "Move to Spam"}
            Icon={s.allSpam ? ShieldCheck : ShieldAlert}
            onClick={actions.spam}
          />
          <Action
            tip={hint(s.allDeleted ? "Delete Forever" : "Move to Trash", "trash")}
            Icon={Trash2}
            iconClass={s.allDeleted ? "text-error" : ""}
            onClick={actions.trash}
          />
    </>
  );
}

const BUTTON =
  "btn btn-ghost btn-square h-10 w-10 rounded-full border border-hairline text-text-2 hover:text-base-content";

function Action({
  tip,
  Icon,
  iconClass = "",
  onClick,
}: {
  tip: Tip;
  Icon: LucideIcon;
  iconClass?: string;
  onClick: () => void;
}) {
  return (
    <button className={BUTTON} {...tipAttrs(tip)} aria-label={tipLabel(tip)} onClick={onClick}>
      <Icon size={18} className={iconClass} />
    </button>
  );
}
