import {
  ArrowDownLeft,
  Check,
  Circle,
  CircleCheck,
  Clock,
  Ellipsis,
  Paperclip,
  Pin,
  Trash2,
  Zap,
  type LucideIcon,
} from "lucide-react";
import { memo, useEffect, useRef } from "react";
import type { Message } from "../types";
import { dragMessage } from "../lib/drag";
import { formatLater, formatLaterShort, formatListDate } from "../lib/format";
import { recipientNames } from "../lib/recipients";
import { hint } from "../lib/shortcuts";
import Avatar from "./Avatar";
import { tipAttrs, tipLabel, type Tip } from "../lib/tooltip";

export interface RowActions {
  open: (id: number) => void;
  /** Ctrl/⌘-click adds the row to the selection or takes it out; Shift-click selects up to it. */
  pick: (id: number, range: boolean) => void;
  done: (m: Message) => void;
  pin: (m: Message) => void;
  snooze: (m: Message) => void;
  aside: (m: Message) => void;
  trash: (m: Message) => void;
  priority: (m: Message) => void;
  read: (m: Message) => void;
  /** Opens the Command Center for this row. */
  more: (m: Message) => void;
}

/**
 * Split-view row, about 70px: unread dot, 26px avatar with the
 * account ring, sender / subject / snippet. A row stands for a conversation:
 * its newest email in the list, with who wrote in it and a count badge.
 * Hover shows a Done circle in the leading column and swaps the date for the
 * row actions. Rows picked for a batch action show a check there instead.
 * Memoized: a list has thousands, and most renders change none of them.
 */
export default memo(MessageRow);

function MessageRow({
  message: m,
  count = 1,
  names,
  selected,
  picked = false,
  actions,
  inPriorityCard = false,
  recipients = false,
}: {
  message: Message;
  /** Emails in its conversation; two or more show a badge. */
  count?: number;
  /** Who wrote in the conversation, in place of the sender. */
  names?: string;
  selected: boolean;
  /** In the selection for a batch action. */
  picked?: boolean;
  actions: RowActions;
  /** Rows on the peach Priority card hover in its own tint. */
  inPriorityCard?: boolean;
  /** Sent and Drafts rows name who it's to, and show the first recipient's avatar. */
  recipients?: boolean;
}) {
  const ref = useRef<HTMLDivElement>(null);
  const wasSelected = useRef(selected);
  // Like Spark, only a row the cursor moves to scrolls into view: not one
  // that was clicked (it's under the pointer), nor one already selected when
  // its list opens, which keeps the list's place.
  useEffect(() => {
    if (selected && !wasSelected.current && !ref.current?.matches(":hover")) {
      ref.current?.scrollIntoView({ block: "nearest" });
    }
    wasSelected.current = selected;
  }, [selected]);

  const done = m.state === "done";
  const deleted = m.state === "deleted";
  const snoozed = m.state === "snoozed" && m.snoozedUntil !== undefined;
  const hover = inPriorityCard ? "hover:bg-priority-line/60" : "hover:bg-row-hover";
  const to = recipients ? recipientNames(m.to) : null;
  const weight = m.unread ? "font-semibold" : "font-medium";
  return (
    <div
      ref={ref}
      role="option"
      data-row={m.id}
      aria-selected={selected || picked}
      onClick={(e) => {
        if (e.ctrlKey || e.metaKey || e.shiftKey) actions.pick(m.id, e.shiftKey);
        else actions.open(m.id);
      }}
      // Onto a sidebar list: a folder, Inbox, Archive, Trash, Spam or Pins.
      onPointerDown={(e) => dragMessage(e, m)}
      className={`group grid h-[70px] cursor-default grid-cols-[28px_26px_minmax(0,1fr)] rounded-lg py-[5px] pr-2.5 ${
        selected || picked ? "bg-row-selected" : hover
      }`}
    >
      <span className="flex h-5 items-center justify-center">
        {picked ? (
          <LeadingButton title="Deselect" picked onClick={() => actions.pick(m.id, false)} />
        ) : done ? (
          <LeadingButton title="Move to Inbox" filled onClick={() => actions.done(m)} />
        ) : (
          <>
            {m.unread && <span className="size-2 rounded-full bg-primary group-hover:hidden" />}
            <span className="hidden group-hover:contents">
              <LeadingButton title={hint("Done", "done")} onClick={() => actions.done(m)} />
            </span>
          </>
        )}
      </span>
      <span className={done ? "opacity-50" : ""}>
        <Avatar name={to?.first ?? m.from} account={m.account} />
      </span>
      <div className={`min-w-0 pl-2.5 ${done ? "text-text-2" : ""}`}>
        <div className="flex h-5 items-center gap-1">
          {to ? (
            <span className="flex min-w-0 items-center gap-1 text-sm leading-5" data-tip={m.to?.join(", ")}>
              <span className={`truncate ${weight}`}>
                {to.shown.length ? `To: ${to.shown.join(", ")}` : <i className="font-normal">No recipients</i>}
              </span>
              {to.more > 0 && <span className="shrink-0 text-[13px] text-text-2">+{to.more}</span>}
            </span>
          ) : (
            <span className={`truncate text-sm leading-5 ${weight}`}>{names ?? m.from}</span>
          )}
          <span
            data-tip={snoozed ? `Snoozed until ${formatLater(m.snoozedUntil!)}` : undefined}
            className={`ml-auto flex shrink-0 items-center gap-1 pl-2 text-[11px] leading-5 group-hover:hidden ${
              snoozed ? "text-priority" : "text-text-2"
            }`}
          >
            {snoozed ? (
              <>
                <Clock size={12} />
                {formatLaterShort(m.snoozedUntil!)}
              </>
            ) : (
              formatListDate(m.timestamp)
            )}
          </span>
          <div className="ml-auto hidden shrink-0 items-center gap-0.5 pl-2 group-hover:flex">
            <HoverButton
              title={hint(deleted ? "Delete Forever" : "Move to Trash", "trash")}
              Icon={Trash2}
              onClick={() => actions.trash(m)}
            />
            <HoverButton
              title={hint(m.priority ? "Remove from Priority" : "Mark as Priority", "priority")}
              Icon={Zap}
              active={m.priority}
              activeClass="fill-current text-priority"
              onClick={() => actions.priority(m)}
            />
            <HoverButton
              title={hint(m.unread ? "Mark as Read" : "Mark as Unread", "read")}
              Icon={Circle}
              active={m.unread}
              activeClass="fill-current text-primary"
              onClick={() => actions.read(m)}
            />
            <HoverButton
              title={hint(m.pinned ? "Unpin" : "Pin", "pin")}
              Icon={Pin}
              active={m.pinned}
              activeClass="fill-current text-pin"
              onClick={() => actions.pin(m)}
            />
            <HoverButton
              title={m.state === "aside" ? "Move back to Inbox" : hint("Set Aside", "aside")}
              Icon={ArrowDownLeft}
              onClick={() => actions.aside(m)}
            />
            <HoverButton title={hint("Snooze", "snooze")} Icon={Clock} onClick={() => actions.snooze(m)} />
            <HoverButton title="More actions" Icon={Ellipsis} onClick={() => actions.more(m)} />
          </div>
        </div>
        <div className="flex items-center gap-1 text-[13px] font-medium leading-5">
          {m.priority ? (
            <Zap size={13} className="shrink-0 fill-current text-priority" aria-label="Priority" />
          ) : m.pinned ? (
            <Pin size={13} className="shrink-0 fill-current text-pin" aria-label="Pinned" />
          ) : null}
          <span className="truncate">{m.subject || <i className="font-normal">No subject</i>}</span>
          {count > 1 && (
            <span
              aria-label={`${count} emails`}
              className="h-4 min-w-5 shrink-0 rounded-[3px] bg-chip px-1 text-center text-[11px] font-semibold leading-4 text-text-2"
            >
              {count > 99 ? "99+" : count}
            </span>
          )}
          {m.attachment && <Paperclip size={12} className="shrink-0 rotate-45 text-text-2" aria-label="Has attachment" />}
        </div>
        <div className="truncate text-[13px] leading-5 text-text-2">{m.snippet || "No content"}</div>
      </div>
    </div>
  );
}

/** The leading column's Done circle: outline on hover, filled for Done mail; a check for picked rows. */
function LeadingButton({
  title,
  filled = false,
  picked = false,
  onClick,
}: {
  title: Tip;
  filled?: boolean;
  picked?: boolean;
  onClick: () => void;
}) {
  return (
    <button
      {...tipAttrs(title)}
      aria-label={tipLabel(title)}
      onClick={(e) => {
        e.stopPropagation();
        onClick();
      }}
      className={`grid size-[18px] place-items-center rounded-full ${
        filled || picked ? "bg-primary text-primary-content" : "text-text-2 hover:text-primary"
      }`}
    >
      {filled || picked ? <Check size={12} strokeWidth={2.5} /> : <CircleCheck size={18} />}
    </button>
  );
}

function HoverButton({
  title,
  Icon,
  onClick,
  active = false,
  activeClass = "",
}: {
  title: Tip;
  Icon: LucideIcon;
  onClick: () => void;
  active?: boolean;
  activeClass?: string;
}) {
  return (
    <button
      {...tipAttrs(title)}
      aria-label={tipLabel(title)}
      aria-pressed={active || undefined}
      onClick={(e) => {
        e.stopPropagation();
        onClick();
      }}
      className="grid size-5 place-items-center rounded-full text-text-2 hover:bg-base-content/10 hover:text-base-content"
    >
      <Icon size={14} className={active ? activeClass : ""} />
    </button>
  );
}
