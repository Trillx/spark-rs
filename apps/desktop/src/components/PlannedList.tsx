import { AlarmClock, CalendarClock, Pencil, RefreshCw, Send, Trash2, type LucideIcon } from "lucide-react";
import type { ReminderView, ScheduledView } from "../types";
import { useAccounts } from "../lib/accounts";
import { formatLater } from "../lib/format";
import { tipAttrs, tipLabel, type Tip } from "../lib/tooltip";
import AccountMark from "./AccountMark";

export interface ScheduledActions {
  /** Back in the composer to change, out of the queue until it's sent again. */
  edit: (s: ScheduledView) => void;
  sendNow: (s: ScheduledView) => void;
  /** Asks for a new day and time. */
  reschedule: (s: ScheduledView) => void;
  /** Not sent: it's kept in Drafts. */
  cancel: (s: ScheduledView) => void;
}

export interface ReminderActions {
  /** The email it's about, once it's back in the Inbox. */
  open: (r: ReminderView) => void;
  /** Asks for a new day and time. */
  change: (r: ReminderView) => void;
  remove: (r: ReminderView) => void;
}

/**
 * The Scheduled and Reminders lists, which hold what's planned rather than
 * mail: Send Later's queue, soonest first, with mail that couldn't be sent
 * yet and is tried again, and the reminders on sent mail.
 */
export default function PlannedList(
  props:
    | { kind: "scheduled"; items: ScheduledView[]; actions: ScheduledActions }
    | { kind: "reminders"; items: ReminderView[]; actions: ReminderActions },
) {
  const title = props.kind === "scheduled" ? "Scheduled" : "Reminders";
  return (
    <>
      <div role="toolbar" className="flex min-h-[58px] shrink-0 items-center border-b border-hairline px-4">
        <h2 className="truncate text-base font-semibold">{title}</h2>
      </div>
      <div role="list" aria-label={title} className="min-h-0 flex-1 overflow-y-auto px-2 pb-8 pt-1">
        {props.items.length === 0 ? (
          <Empty kind={props.kind} />
        ) : props.kind === "scheduled" ? (
          props.items.map((s) => (
            <Row
              key={s.id}
              account={s.account}
              to={s.to}
              subject={s.subject}
              snippet={s.snippet}
              when={
                s.failed === undefined
                  ? `Sends ${formatLater(s.sendAt)}`
                  : s.sending
                    ? "Trying again…"
                    : "Couldn't send yet"
              }
              Icon={s.failed === undefined ? CalendarClock : RefreshCw}
              problem={s.failed && `${s.failed.replace(/\.$/, "")}. It'll keep trying.`}
              onOpen={() => props.actions.edit(s)}
              buttons={[
                { tip: "Send now", Icon: Send, run: () => props.actions.sendNow(s) },
                { tip: "Change the time", Icon: CalendarClock, run: () => props.actions.reschedule(s) },
                { tip: "Edit", Icon: Pencil, run: () => props.actions.edit(s) },
                { tip: "Don't send (keep in Drafts)", Icon: Trash2, run: () => props.actions.cancel(s) },
              ]}
            />
          ))
        ) : (
          props.items.map((r) => (
            <Row
              key={r.id}
              account={r.account}
              to={r.to}
              subject={r.subject}
              snippet={r.ifNoReply ? "If nobody replies" : "Whether or not anyone replies"}
              when={r.due ? "In your Inbox" : `Reminds you ${formatLater(r.remindAt)}`}
              Icon={AlarmClock}
              onOpen={() => (r.message !== undefined ? props.actions.open(r) : props.actions.change(r))}
              buttons={[
                { tip: "Change the time", Icon: AlarmClock, run: () => props.actions.change(r) },
                { tip: "Remove the reminder", Icon: Trash2, run: () => props.actions.remove(r) },
              ]}
            />
          ))
        )}
      </div>
    </>
  );
}

function Row({
  account,
  to,
  subject,
  snippet,
  when,
  Icon,
  problem,
  onOpen,
  buttons,
}: {
  account: number;
  to: string;
  subject: string;
  snippet: string;
  when: string;
  Icon: LucideIcon;
  /** What went wrong, shown in place of the snippet. */
  problem?: string;
  onOpen: () => void;
  buttons: { tip: Tip; Icon: LucideIcon; run: () => void }[];
}) {
  const accounts = useAccounts();
  const color = accounts.find((a) => a.id === account)?.color;
  return (
    <div
      role="listitem"
      className="group flex h-[70px] cursor-default items-center gap-2.5 rounded-lg px-2.5 hover:bg-row-hover"
      onClick={onOpen}
    >
      {color && <AccountMark color={color} />}
      <div className="min-w-0 flex-1">
        <div className="flex h-5 items-center gap-1">
          <span className="truncate text-sm font-medium leading-5">
            {to.trim() ? `To: ${to}` : <i className="font-normal">No recipients</i>}
          </span>
          <span
            className={`ml-auto flex shrink-0 items-center gap-1 pl-2 text-[11px] leading-5 group-hover:hidden ${problem ? "text-error" : "text-priority"}`}
          >
            <Icon size={12} />
            {when}
          </span>
          <div className="ml-auto hidden shrink-0 items-center gap-0.5 pl-2 group-hover:flex">
            {buttons.map((b) => (
              <button
                key={tipLabel(b.tip)}
                {...tipAttrs(b.tip)}
                aria-label={tipLabel(b.tip)}
                className="grid size-6 place-items-center rounded-full text-text-2 hover:bg-base-content/10 hover:text-base-content"
                onClick={(e) => {
                  e.stopPropagation();
                  b.run();
                }}
              >
                <b.Icon size={14} />
              </button>
            ))}
          </div>
        </div>
        <div className="truncate text-[13px] font-medium leading-5">
          {subject.trim() || <i className="font-normal">No subject</i>}
        </div>
        {problem ? (
          <div className="truncate text-[13px] leading-5 text-error" title={problem}>
            {problem}
          </div>
        ) : (
          <div className="truncate text-[13px] leading-5 text-text-2">{snippet || "No content"}</div>
        )}
      </div>
    </div>
  );
}

function Empty({ kind }: { kind: "scheduled" | "reminders" }) {
  const [Icon, title, text] =
    kind === "scheduled"
      ? [
          CalendarClock,
          "Nothing scheduled",
          "Mail you send later waits here until it goes out, as does mail that couldn't be sent yet.",
        ]
      : [AlarmClock, "No reminders", "Choose Remind me when you send, and the email comes back if nobody replies."];
  return (
    <div className="flex flex-col items-center px-6 pt-20 text-center">
      <span className="grid size-24 place-items-center rounded-full bg-base-200 text-text-3">
        <Icon size={40} />
      </span>
      <h3 className="mt-5 text-xl font-semibold">{title}</h3>
      <p className="mt-1 text-sm text-text-2">{text}</p>
    </div>
  );
}
