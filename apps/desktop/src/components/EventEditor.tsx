import { ChevronLeft, ChevronRight, FileText, MapPin, Users, Video, X } from "lucide-react";
import { useEffect, useLayoutEffect, useRef, useState, type MutableRefObject, type ReactNode } from "react";
import { parseEntry, splitAddresses } from "../lib/addresses";
import { addMonths, formatTime, startOfMonth, viewDays } from "../lib/calendar";
import {
  QUARTERS,
  alertChoices,
  allDayDays,
  atMinutes,
  attendeesChanged,
  composing,
  dateText,
  dateValue,
  dayOf,
  followStart,
  fromUtcDate,
  nthOf,
  presetRule,
  repeatKind,
  repeatText,
  type RepeatKind,
  draftChanged,
  minutesOf,
  parseTime,
  withAllDay,
  withDays,
  withEnd,
  withFirstDay,
  withStart,
  type NewEventDefaults,
} from "../lib/eventDraft";
import type { AccountView, CalendarEvent, CalendarInfo, EventDraft, Repeat } from "../types";
import Avatar from "./Avatar";
import { ChoiceDialog } from "./Dialog";
import { Popover } from "./Dropdown";
import { usePopupPlace } from "./EventPreview";
import { callService } from "./InviteCard";
import { SuggestionList, displayName, useSuggestions } from "./Recipients";

const WIDTH = 380;

const SHOW_AS: { value: CalendarEvent["showAs"]; label: string }[] = [
  { value: "free", label: "Free" },
  { value: "busy", label: "Busy" },
  { value: "tentative", label: "Tentative" },
  { value: "oof", label: "Away" },
  { value: "workingElsewhere", label: "Working elsewhere" },
];

const dateFormat = new Intl.DateTimeFormat(undefined, { weekday: "short", month: "short", day: "numeric", year: "numeric" });
const monthYear = new Intl.DateTimeFormat(undefined, { month: "long", year: "numeric" });
const weekdayInitial = new Intl.DateTimeFormat(undefined, { weekday: "narrow" });

/** The user's time zone as Windows words it, e.g. "Pacific Daylight Time", at `t`. */
function zoneName(t: number) {
  return new Intl.DateTimeFormat(undefined, { timeZoneName: "long" }).formatToParts(t).find((p) => p.type === "timeZoneName")
    ?.value;
}

/**
 * Spark's event editor, beside the event (or centred, in Day view or for New Event): the
 * calendar, title, all day and the times, the alert, how it shows (free or busy), a Teams
 * call, who's invited, the place and a note. Its one button creates, updates, or sends the
 * invitations when there are people to tell. Closing it with changes asks whether to save them.
 */
export default function EventEditor({
  event,
  initial,
  calendars,
  accounts,
  anchor,
  modal,
  defaults,
  unsaved = false,
  closeRef,
  onSave,
  onClose,
}: {
  /** The event being changed; absent for a new one. */
  event?: CalendarEvent;
  initial: EventDraft;
  /** Every calendar; a new event can go in any of the user's own. */
  calendars: CalendarInfo[];
  accounts: AccountView[];
  anchor: DOMRect;
  modal: boolean;
  defaults: NewEventDefaults;
  /** A copy not saved yet (Duplicate, Paste Event): closing asks even without changes, as Spark does. */
  unsaved?: boolean;
  /**
   * Set while it's open: asks it to close as its ✕ does (asking first about changes), then runs
   * `after` once it has; for the calendar closing around it.
   */
  closeRef?: MutableRefObject<((after: () => void) => void) | null>;
  /** Saves it: true once it's saved, false if the user went back to editing; rejects with what went wrong. */
  onSave: (draft: EventDraft) => Promise<boolean>;
  onClose: () => void;
}) {
  const ref = useRef<HTMLDivElement>(null);
  const title = useRef<HTMLTextAreaElement>(null);
  const [draft, setDraft] = useState(initial);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [asking, setAsking] = useState(false);
  /** The times it had before going all day, for coming back. */
  const timed = useRef<{ start: number; end: number } | undefined>(initial.allDay ? undefined : initial);
  const place = usePopupPlace(ref, anchor, modal, WIDTH, event?.id ?? "new");

  const changed = draftChanged(draft, initial);
  const latest = useRef({ changed: changed || unsaved, busy });
  latest.current = { changed: changed || unsaved, busy };
  const own = calendars.filter((c) => c.own);
  const calendar = calendars.find((c) => c.id === draft.calendar);

  useLayoutEffect(() => {
    const box = title.current;
    if (!box) return;
    box.style.height = "auto";
    box.style.height = `${box.scrollHeight}px`;
  }, [draft.title]);

  /** What to do once it's closed, when the calendar closing around it asked it to. */
  const then = useRef<(() => void) | undefined>(undefined);
  const done = () => {
    const after = then.current;
    then.current = undefined;
    onClose();
    after?.();
  };
  /** ✕, Esc or a click elsewhere: with changes, it asks first. */
  const close = (after?: () => void) => {
    if (latest.current.busy) return;
    then.current = after;
    if (latest.current.changed) setAsking(true);
    else done();
  };
  const latestClose = useRef(close);
  latestClose.current = close;

  useEffect(() => {
    const asked = () => document.querySelector("[aria-modal]") !== null;
    // What's still being typed (a time, someone to invite) counts: it's taken in first, then
    // whether anything changed is looked at.
    const closeSoon = (after?: () => void) => {
      const typing = document.activeElement;
      if (typing instanceof HTMLElement && ref.current?.contains(typing)) typing.blur();
      window.setTimeout(() => {
        if (!asked()) latestClose.current(after);
      });
    };
    if (closeRef) closeRef.current = closeSoon;
    // Like a backdrop: the press that closes it doesn't also act on what's under it (another
    // event, the calendar's close button, the sidebar).
    const swallow = (e: Event) => {
      e.stopPropagation();
      e.preventDefault();
    };
    const onPointer = (e: PointerEvent) => {
      if (asked() || ref.current?.contains(e.target as Node)) return;
      // Nor does it start anything there (a drag on the grid), or a right-click menu.
      e.stopPropagation();
      e.preventDefault();
      for (const type of ["click", "contextmenu"]) window.addEventListener(type, swallow, { capture: true, once: true });
      window.addEventListener(
        "pointerup",
        () =>
          window.setTimeout(() => {
            for (const type of ["click", "contextmenu"]) window.removeEventListener(type, swallow, true);
          }),
        { capture: true, once: true },
      );
      closeSoon();
    };
    // Tabbing out of it is leaving it too.
    const onFocus = (e: FocusEvent) => {
      if (asked() || ref.current?.contains(e.target as Node)) return;
      closeSoon();
    };
    // Capture, so Esc closes the editor and not the calendar; a list or picker open in it closes first.
    const onKey = (e: KeyboardEvent) => {
      // Esc in an input method (Japanese, Chinese) is for what's being typed there.
      if (e.key !== "Escape" || e.isComposing || asked()) return;
      if (ref.current?.querySelector("[role='listbox'], [role='dialog']")) return;
      e.stopPropagation();
      e.preventDefault();
      closeSoon();
    };
    window.addEventListener("pointerdown", onPointer, true);
    window.addEventListener("keydown", onKey, true);
    window.addEventListener("focusin", onFocus, true);
    return () => {
      if (closeRef) closeRef.current = null;
      window.removeEventListener("pointerdown", onPointer, true);
      window.removeEventListener("keydown", onKey, true);
      window.removeEventListener("focusin", onFocus, true);
    };
  }, []);

  const save = async () => {
    if (busy) return;
    setBusy(true);
    setError(null);
    try {
      if (await onSave(draft)) done();
      else setBusy(false);
    } catch (e) {
      setError(String(e));
      setBusy(false);
    }
  };

  const set = (change: Partial<EventDraft>) => setDraft((d) => ({ ...d, ...change }));
  const setAllDay = (on: boolean) =>
    setDraft((d) => {
      if (on) timed.current = { start: d.start, end: d.end };
      return withAllDay(d, on, defaults, timed.current);
    });

  // A preset repeat rule goes with the first day ("every week" on its weekday) as it moves.
  const lastStart = useRef({ start: draft.start, allDay: draft.allDay });
  useEffect(() => {
    const was = lastStart.current;
    lastStart.current = { start: draft.start, allDay: draft.allDay };
    if (was.allDay === draft.allDay && dayOf(was.start) === dayOf(draft.start)) return;
    setDraft((d) => ({ ...d, repeat: followStart(d.repeat, was, d) }));
  }, [draft.start, draft.allDay]);
  const [customizing, setCustomizing] = useState(false);
  const kind = repeatKind(draft.repeat, draft);
  /** An occurrence of a series whose rule this can't show: it's left as it is. */
  const unknownRule = event?.repeats === true && initial.repeat === null;

  const inviting = event ? attendeesChanged(draft, initial) : draft.attendees.length > 0;
  const label = inviting ? "Send Invitation" : event ? "Update" : "Create";
  const days = draft.allDay ? allDayDays(draft) : null;
  const zone = zoneName(draft.start);
  const call = event?.callLink && !event.onlineMeeting ? event.callLink : undefined;

  return (
    <>
      {modal && <div className="fixed inset-0 z-[2010] bg-black/20" />}
      <div
        ref={ref}
        role="dialog"
        aria-label={draft.title.trim() || "Calendar Event"}
        className="fixed z-[2020] flex max-h-[calc(100vh-56px)] flex-col rounded-2xl border border-hairline bg-popover text-[13px] text-base-content shadow-popup"
        style={{ width: WIDTH, left: place?.left ?? -9999, top: place?.top ?? 0 }}
      >
        <div className="flex shrink-0 items-center gap-2 px-4 pb-1 pt-3">
          <span className="size-2 shrink-0 rounded-full" style={{ background: calendar?.color }} />
          {event ? (
            <span className="min-w-0 flex-1 truncate text-xs text-text-2">{calendar?.name ?? "Calendar"}</span>
          ) : (
            <select
              aria-label="Select calendar account"
              className="min-w-0 flex-1 cursor-pointer truncate bg-transparent text-xs text-text-2 outline-none"
              value={draft.calendar}
              onChange={(e) => set({ calendar: Number(e.target.value) })}
            >
              {accounts.map((a) => {
                const mine = own.filter((c) => c.account === a.id);
                if (!mine.length) return null;
                return (
                  <optgroup key={a.id} label={a.label}>
                    {mine.map((c) => (
                      <option key={c.id} value={c.id}>
                        {c.name}
                      </option>
                    ))}
                  </optgroup>
                );
              })}
            </select>
          )}
          <button
            data-tip="Close"
            aria-label="Close"
            className="grid size-7 shrink-0 place-items-center rounded-lg text-text-2 hover:bg-base-content/10 hover:text-base-content"
            onClick={() => close()}
          >
            <X size={16} />
          </button>
        </div>

        <div className="min-h-0 overflow-y-auto px-4 pb-3">
          <textarea
            ref={title}
            rows={1}
            autoFocus={!event}
            aria-label="Event name"
            placeholder="New Event"
            className="block w-full resize-none overflow-hidden bg-transparent text-xl font-semibold leading-6 outline-none placeholder:text-text-3"
            value={draft.title}
            onChange={(e) => set({ title: e.target.value.replace(/\n/g, " ") })}
            onKeyDown={(e) => {
              if (e.key !== "Enter" || composing(e)) return;
              e.preventDefault();
              if (!event || changed) void save();
            }}
          />

          <div className="mt-3 flex items-center gap-2">
            <label className="flex cursor-pointer items-center gap-2">
              <input
                type="checkbox"
                className="checkbox checkbox-xs rounded"
                checked={draft.allDay}
                disabled={event?.repeats}
                onChange={(e) => setAllDay(e.target.checked)}
              />
              All day
            </label>
            {zone && <span className="min-w-0 flex-1 truncate text-right text-xs text-text-3">{zone}</span>}
          </div>

          <div className="mt-2 space-y-1.5">
            <Row label="Start:">
              {days ? (
                <DateField
                  label="Start date"
                  day={days.first}
                  onPick={(first) => setDraft((d) => withFirstDay(d, first))}
                />
              ) : (
                <>
                  <TimeField label="Start time" value={draft.start} onChange={(t) => setDraft((d) => withStart(d, t))} />
                  <DateField
                    label="Start date"
                    day={dayOf(draft.start)}
                    onPick={(day) => setDraft((d) => withStart(d, atMinutes(day, minutesOf(d.start))))}
                  />
                </>
              )}
            </Row>
            <Row label="End:">
              {days ? (
                <DateField label="End date" day={days.last} onPick={(last) => setDraft((d) => withDays(d, days.first, last))} />
              ) : (
                <>
                  <TimeField label="End time" value={draft.end} onChange={(t) => setDraft((d) => withEnd(d, t))} />
                  <DateField
                    label="End date"
                    day={dayOf(draft.end)}
                    onPick={(day) => setDraft((d) => withEnd(d, atMinutes(day, minutesOf(d.end))))}
                  />
                </>
              )}
            </Row>
          </div>

          <hr className="my-3 border-hairline" />

          <div className="relative space-y-1.5">
            <Row label="Repeat:">
              <select
                aria-label="Repeat frequency"
                className="select select-sm h-8 min-h-8 flex-1 rounded-lg border-hairline text-[13px]"
                value={unknownRule ? "unknown" : kind}
                disabled={unknownRule}
                onChange={(e) => {
                  const chosen = e.target.value as RepeatKind;
                  if (chosen === "custom") setCustomizing(true);
                  else set({ repeat: presetRule(chosen, draft) });
                }}
              >
                {unknownRule && <option value="unknown">Repeats (change it in Outlook)</option>}
                <option value="none">None</option>
                <option value="day">Every day</option>
                <option value="week">Every week</option>
                <option value="month">Every month</option>
                <option value="year">Every year</option>
                <option value="custom">{kind === "custom" && draft.repeat ? repeatText({ ...draft.repeat, ends: { kind: "never" } }) : "Custom…"}</option>
              </select>
            </Row>
            {draft.repeat && <Ends rule={draft.repeat} onChange={(repeat) => set({ repeat })} />}
            {customizing && (
              <RepeatDialog
                rule={draft.repeat ?? presetRule("week", draft)!}
                draft={draft}
                onSave={(repeat) => {
                  set({ repeat });
                  setCustomizing(false);
                }}
                onClose={() => setCustomizing(false)}
              />
            )}
            <Row label="Alert:">
              <select
                aria-label="Notification alert"
                className="select select-sm h-8 min-h-8 flex-1 rounded-lg border-hairline text-[13px]"
                value={draft.reminder ?? "none"}
                onChange={(e) => set({ reminder: e.target.value === "none" ? null : Number(e.target.value) })}
              >
                {alertChoices(draft.allDay, draft.reminder).map((c) => (
                  <option key={c.value ?? "none"} value={c.value ?? "none"}>
                    {c.label}
                  </option>
                ))}
              </select>
            </Row>
            <Row label="Show as:">
              <select
                aria-label="Occupancy"
                className="select select-sm h-8 min-h-8 flex-1 rounded-lg border-hairline text-[13px]"
                value={draft.showAs === "unknown" ? "busy" : draft.showAs}
                onChange={(e) => set({ showAs: e.target.value as CalendarEvent["showAs"] })}
              >
                {SHOW_AS.filter((s) => s.value === "free" || s.value === "busy" || s.value === initial.showAs).map((s) => (
                  <option key={s.value} value={s.value}>
                    {s.label}
                  </option>
                ))}
              </select>
            </Row>
          </div>

          <hr className="my-3 border-hairline" />

          <div className="space-y-2">
            <IconRow Icon={Video}>
              {call ? (
                <span className="flex h-8 items-center">{callService(call)}</span>
              ) : (
                <select
                  aria-label="Video conference"
                  className="select select-sm h-8 min-h-8 w-full rounded-lg border-hairline text-[13px]"
                  value={draft.teams ? "teams" : "none"}
                  // Outlook keeps a Teams meeting once it's made.
                  disabled={initial.teams}
                  onChange={(e) => set({ teams: e.target.value === "teams" })}
                >
                  <option value="none">No conference call</option>
                  <option value="teams">Microsoft Teams</option>
                </select>
              )}
            </IconRow>
            <IconRow Icon={Users}>
              <Attendees people={draft.attendees} onChange={(attendees) => set({ attendees })} />
            </IconRow>
            <IconRow Icon={MapPin}>
              <input
                aria-label="Location"
                placeholder="Add Location"
                className="h-8 w-full bg-transparent outline-none placeholder:text-text-3"
                value={draft.location}
                onChange={(e) => set({ location: e.target.value })}
              />
            </IconRow>
            <IconRow Icon={FileText}>
              <textarea
                aria-label="Note"
                placeholder="Add Note"
                rows={3}
                className="block w-full resize-y bg-transparent py-1.5 leading-5 outline-none placeholder:text-text-3"
                value={draft.notes}
                onChange={(e) => set({ notes: e.target.value })}
              />
            </IconRow>
          </div>
        </div>

        <div className="flex shrink-0 items-center gap-3 border-t border-hairline px-4 py-3">
          <p className="min-w-0 flex-1 select-text text-xs text-error">{error}</p>
          <button
            className="btn btn-primary btn-sm h-8 min-h-8 rounded-lg px-4"
            disabled={busy || (event !== undefined && !changed)}
            onClick={() => void save()}
          >
            {busy && <span className="loading loading-spinner loading-xs" />}
            {label}
          </button>
        </div>
      </div>
      {asking && (
        <ChoiceDialog
          title="Do you want to save changes?"
          message="Changes you don't save will be lost."
          choices={[
            { label: "Discard", value: "discard" as const },
            { label: "Continue Editing", value: "edit" as const },
            { label: "Save", value: "save" as const, primary: true },
          ]}
          onChoose={(choice) => {
            setAsking(false);
            if (choice === "discard") done();
            else if (choice === "save") void save();
            else then.current = undefined;
          }}
        />
      )}
    </>
  );
}

/** When a rule stops: never, after so many times, or on a date. */
function Ends({ rule, onChange }: { rule: Repeat; onChange: (rule: Repeat) => void }) {
  const ends = rule.ends;
  const [times, setTimes] = useState(ends.kind === "after" ? String(ends.times) : "1");
  return (
    <Row label="Ends:">
      <select
        aria-label="Repeat ends"
        className="select select-sm h-8 min-h-8 w-28 rounded-lg border-hairline text-[13px]"
        value={ends.kind}
        onChange={(e) => {
          const kind = e.target.value;
          if (kind === "never") onChange({ ...rule, ends: { kind } });
          else if (kind === "after") onChange({ ...rule, ends: { kind, times: 1 } });
          else onChange({ ...rule, ends: { kind: "on", date: dateText(dayOf(Date.now())) } });
          setTimes("1");
        }}
      >
        <option value="never">Never</option>
        <option value="after">After</option>
        <option value="on">On the date</option>
      </select>
      {ends.kind === "after" && (
        <>
          <input
            aria-label="Number of times to repeat event"
            inputMode="numeric"
            className="h-8 w-14 rounded-lg border border-hairline bg-base-100 px-2 outline-none focus:border-primary"
            value={times}
            onChange={(e) => {
              const text = e.target.value.replace(/\D/g, "").slice(0, 3);
              setTimes(text);
              if (Number(text) >= 1) onChange({ ...rule, ends: { kind: "after", times: Number(text) } });
            }}
            // Empty, it's once.
            onBlur={() => !(Number(times) >= 1) && (setTimes("1"), onChange({ ...rule, ends: { kind: "after", times: 1 } }))}
          />
          <span className="text-text-2">time(s)</span>
        </>
      )}
      {ends.kind === "on" && (
        <DateField
          label="Repeat end date"
          day={dateValue(ends.date)}
          onPick={(day) => onChange({ ...rule, ends: { kind: "on", date: dateText(day) } })}
        />
      )}
    </Row>
  );
}

const weekdayShort = (day: number) => new Intl.DateTimeFormat(undefined, { weekday: "short" }).format(new Date(2023, 0, 1 + day));
const monthLong = (month: number) => new Intl.DateTimeFormat(undefined, { month: "long" }).format(new Date(2023, month - 1, 1));

/**
 * Spark's Custom repeat: how often (daily, weekly, monthly, yearly) and every how many; weekly, its
 * days; monthly or yearly, on a day of the month or "on the second Tuesday"; yearly, its month.
 * A day of the month and a month each, as Microsoft 365 keeps them.
 */
function RepeatDialog({
  rule,
  draft,
  onSave,
  onClose,
}: {
  rule: Repeat;
  draft: EventDraft;
  onSave: (rule: Repeat) => void;
  onClose: () => void;
}) {
  const [r, setR] = useState<Repeat>(rule);
  const first = new Date(draft.allDay ? fromUtcDate(draft.start) : draft.start);
  const [count, setCount] = useState(String(rule.interval));
  useEffect(() => {
    // Capture, so Esc closes this and not the editor.
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape" || e.isComposing) return;
      e.stopPropagation();
      e.preventDefault();
      onClose();
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [onClose]);
  const units = { day: "day(s)", week: "week(s)", month: "month(s)", year: "year(s)" };
  const byDay = r.nth === undefined;
  const onThe = () => ({ nth: nthOf(first.getTime()), weekdays: [first.getDay()], day: undefined });
  const onDate = () => ({ nth: undefined, weekdays: [], day: first.getDate() });
  return (
    <div
      role="dialog"
      aria-label="Custom repeat"
      className="absolute left-0 right-0 top-9 z-40 rounded-xl border border-hairline bg-popover p-3 shadow-popup"
    >
      <div className="flex items-center gap-2">
        <span className="w-20 text-text-2">Frequency:</span>
        <select
          aria-label="Frequency"
          className="select select-sm h-8 min-h-8 flex-1 rounded-lg border-hairline text-[13px]"
          value={r.every}
          onChange={(e) => {
            const every = e.target.value as Repeat["every"];
            setR({ ...presetRule(every, draft)!, interval: r.interval, ends: r.ends });
          }}
        >
          <option value="day">Daily</option>
          <option value="week">Weekly</option>
          <option value="month">Monthly</option>
          <option value="year">Yearly</option>
        </select>
      </div>
      <div className="mt-2 flex items-center gap-2">
        <span className="w-20 text-text-2">Every</span>
        <input
          aria-label="Interval"
          inputMode="numeric"
          className="h-8 w-14 rounded-lg border border-hairline bg-base-100 px-2 outline-none focus:border-primary"
          value={count}
          onChange={(e) => {
            const text = e.target.value.replace(/\D/g, "").slice(0, 3);
            setCount(text);
            if (Number(text) >= 1) setR((x) => ({ ...x, interval: Number(text) }));
          }}
          onBlur={() => !(Number(count) >= 1) && (setCount("1"), setR((x) => ({ ...x, interval: 1 })))}
        />
        <span className="text-text-2">{units[r.every]}</span>
      </div>
      {r.every === "week" && (
        <div className="mt-2 flex gap-1" role="group" aria-label="On">
          {[0, 1, 2, 3, 4, 5, 6].map((d) => {
            const on = r.weekdays.includes(d);
            return (
              <button
                key={d}
                aria-pressed={on}
                className={`h-7 flex-1 rounded-md text-xs ${on ? "bg-primary text-primary-content" : "bg-base-content/[0.07] hover:bg-base-content/[0.12]"}`}
                // The last day on stays on.
                onClick={() =>
                  setR((x) => ({
                    ...x,
                    weekdays: on ? (x.weekdays.length > 1 ? x.weekdays.filter((w) => w !== d) : x.weekdays) : [...x.weekdays, d].sort(),
                  }))
                }
              >
                {weekdayShort(d)}
              </button>
            );
          })}
        </div>
      )}
      {r.every === "year" && (
        <div className="mt-2 flex items-center gap-2">
          <span className="w-20 text-text-2">In</span>
          <select
            aria-label="Month"
            className="select select-sm h-8 min-h-8 flex-1 rounded-lg border-hairline text-[13px]"
            value={r.month ?? first.getMonth() + 1}
            onChange={(e) => setR((x) => ({ ...x, month: Number(e.target.value) }))}
          >
            {Array.from({ length: 12 }, (_, i) => (
              <option key={i} value={i + 1}>
                {monthLong(i + 1)}
              </option>
            ))}
          </select>
        </div>
      )}
      {(r.every === "month" || r.every === "year") && (
        <div className="mt-2 space-y-1.5">
          <label className="flex cursor-pointer items-center gap-2">
            <input type="radio" className="radio radio-xs" checked={byDay} onChange={() => setR((x) => ({ ...x, ...onDate() }))} />
            <span>On day</span>
            <input
              aria-label="Day of the month"
              inputMode="numeric"
              disabled={!byDay}
              className="h-7 w-12 rounded-lg border border-hairline bg-base-100 px-2 outline-none focus:border-primary disabled:opacity-50"
              value={byDay ? (r.day ?? first.getDate()) : first.getDate()}
              onChange={(e) => {
                const day = Number(e.target.value.replace(/\D/g, "").slice(0, 2));
                if (day >= 1 && day <= 31) setR((x) => ({ ...x, day }));
              }}
            />
          </label>
          <label className="flex cursor-pointer items-center gap-2">
            <input type="radio" className="radio radio-xs" checked={!byDay} onChange={() => setR((x) => ({ ...x, ...onThe() }))} />
            <span>On the</span>
            <select
              aria-label="Week of the month"
              disabled={byDay}
              className="select select-xs h-7 min-h-7 rounded-lg border-hairline text-[13px]"
              value={r.nth ?? nthOf(first.getTime())}
              onChange={(e) => setR((x) => ({ ...x, nth: Number(e.target.value) }))}
            >
              {["first", "second", "third", "fourth", "last"].map((w, i) => (
                <option key={w} value={i + 1}>
                  {w}
                </option>
              ))}
            </select>
            <select
              aria-label="Weekday"
              disabled={byDay}
              className="select select-xs h-7 min-h-7 rounded-lg border-hairline text-[13px]"
              value={r.weekdays[0] ?? first.getDay()}
              onChange={(e) => setR((x) => ({ ...x, weekdays: [Number(e.target.value)] }))}
            >
              {[0, 1, 2, 3, 4, 5, 6].map((d) => (
                <option key={d} value={d}>
                  {new Intl.DateTimeFormat(undefined, { weekday: "long" }).format(new Date(2023, 0, 1 + d))}
                </option>
              ))}
            </select>
          </label>
        </div>
      )}
      <p className="mt-3 text-xs text-text-2">This event will repeat {repeatText({ ...r, ends: { kind: "never" } }).replace(/^Every/, "every")}.</p>
      <div className="mt-3 flex justify-end gap-2">
        <button className="btn btn-ghost btn-sm" onClick={onClose}>
          Cancel
        </button>
        <button className="btn btn-primary btn-sm" disabled={!(Number(count) >= 1)} onClick={() => onSave(r)}>
          Save
        </button>
      </div>
    </div>
  );
}

function Row({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="flex items-center gap-2">
      <span className="w-16 shrink-0 text-text-2">{label}</span>
      {children}
    </div>
  );
}

function IconRow({ Icon, children }: { Icon: typeof Video; children: ReactNode }) {
  return (
    <div className="flex items-start gap-3">
      <Icon size={16} className="mt-2 shrink-0 text-text-2" />
      <div className="min-w-0 flex-1">{children}</div>
    </div>
  );
}

/** A time of day: typed ("9:30", "930", "9:30pm"), or picked from the quarter hours. */
function TimeField({ label, value, onChange }: { label: string; value: number; onChange: (t: number) => void }) {
  const [text, setText] = useState<string | null>(null);
  /** What's typed, as of now: a pick from the list replaces it before the field's blur reads it. */
  const typed = useRef<string | null>(null);
  const [invalid, setInvalid] = useState(false);
  const list = useRef<HTMLUListElement>(null);
  const day = dayOf(value);
  const current = minutesOf(value);

  useLayoutEffect(() => {
    if (text !== null) list.current?.querySelector("[aria-selected='true']")?.scrollIntoView({ block: "center" });
  }, [text !== null]);

  const edit = (next: string | null) => {
    typed.current = next;
    setText(next);
  };
  const commit = () => {
    const now = typed.current;
    if (now === null) return;
    const minutes = parseTime(now);
    setInvalid(minutes === null);
    if (minutes !== null && minutes !== current) onChange(atMinutes(day, minutes));
    edit(null);
  };

  return (
    <div className="relative">
      <input
        aria-label={label}
        aria-invalid={invalid}
        className={`h-8 w-[92px] rounded-lg border bg-base-100 px-2 outline-none focus:border-primary ${
          invalid ? "border-error" : "border-hairline"
        }`}
        value={text ?? formatTime(value)}
        onFocus={(e) => {
          edit(formatTime(value));
          e.target.select();
        }}
        onChange={(e) => edit(e.target.value)}
        onBlur={commit}
        onKeyDown={(e) => {
          if (e.key === "Enter" && !composing(e)) {
            e.preventDefault();
            e.currentTarget.blur();
          } else if (e.key === "Escape" && !composing(e)) {
            // Closes the list, keeping what's typed; the next Esc closes the editor.
            e.currentTarget.blur();
          }
        }}
      />
      {invalid && <span className="sr-only">Invalid time</span>}
      {text !== null && (
        <ul
          ref={list}
          role="listbox"
          aria-label={label}
          className="absolute left-0 top-full z-30 mt-1 max-h-52 w-[120px] overflow-y-auto rounded-lg border border-hairline bg-base-100 py-1 shadow-popup"
          // Keep focus in the field, so picking works like typing.
          onMouseDown={(e) => e.preventDefault()}
        >
          {QUARTERS.map((m) => (
            <li
              key={m}
              role="option"
              aria-selected={m === current}
              className={`flex h-7 cursor-default items-center px-3 hover:bg-primary/10 ${m === current ? "font-semibold text-primary" : ""}`}
              onClick={() => {
                edit(null);
                setInvalid(false);
                if (m !== current) onChange(atMinutes(day, m));
                (document.activeElement as HTMLElement | null)?.blur();
              }}
            >
              {formatTime(atMinutes(day, m))}
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

/** A day, picked from a month. */
function DateField({ label, day, onPick }: { label: string; day: number; onPick: (day: number) => void }) {
  return (
    <Popover
      label={label}
      className="w-[252px]"
      trigger={({ open, toggle }) => (
        <button
          aria-label={label}
          aria-expanded={open}
          className={`h-8 rounded-lg border px-2.5 hover:bg-base-content/5 ${open ? "border-primary" : "border-hairline"}`}
          onClick={toggle}
        >
          {dateFormat.format(day)}
        </button>
      )}
    >
      {(close) => (
        <MonthPick
          day={day}
          onPick={(d) => {
            onPick(d);
            close();
          }}
        />
      )}
    </Popover>
  );
}

function MonthPick({ day, onPick }: { day: number; onPick: (day: number) => void }) {
  const [month, setMonth] = useState(() => startOfMonth(day));
  const grid = viewDays("month", month);
  const today = dayOf(Date.now());
  return (
    <div>
      <div className="mb-2 flex items-center gap-1">
        <span className="min-w-0 flex-1 truncate pl-1 font-semibold">{monthYear.format(month)}</span>
        <button
          aria-label="Previous month"
          className="grid size-6 place-items-center rounded text-text-2 hover:bg-base-content/10"
          onClick={() => setMonth((m) => addMonths(m, -1))}
        >
          <ChevronLeft size={15} />
        </button>
        <button
          aria-label="Next month"
          className="grid size-6 place-items-center rounded text-text-2 hover:bg-base-content/10"
          onClick={() => setMonth((m) => addMonths(m, 1))}
        >
          <ChevronRight size={15} />
        </button>
      </div>
      <div className="grid grid-cols-7 text-center text-[11px] text-text-2">
        {grid.slice(0, 7).map((d) => (
          <span key={d} className="pb-1">
            {weekdayInitial.format(d)}
          </span>
        ))}
      </div>
      <div className="grid grid-cols-7 gap-y-0.5">
        {grid.map((d) => {
          const inMonth = new Date(d).getMonth() === new Date(month).getMonth();
          return (
            <button
              key={d}
              aria-pressed={d === day}
              className={`mx-auto grid size-7 place-items-center rounded-full text-xs ${
                d === day
                  ? "bg-primary font-semibold text-primary-content"
                  : `hover:bg-base-content/10 ${d === today ? "font-semibold text-primary" : ""} ${inMonth ? "" : "text-text-3"}`
              }`}
              onClick={() => onPick(d)}
            >
              {new Date(d).getDate()}
            </button>
          );
        })}
      </div>
    </div>
  );
}

/** Who's invited: a chip each, with ✕, and a field that suggests people as it's typed in. */
function Attendees({
  people,
  onChange,
}: {
  people: EventDraft["attendees"];
  onChange: (people: EventDraft["attendees"]) => void;
}) {
  const [typed, setTyped] = useState("");
  const [focused, setFocused] = useState(false);
  const suggestions = useSuggestions(focused ? typed : "", 6);
  const shown = suggestions.list;

  const isAddress = (p: { address: string }) => /^[^\s@]+@[^\s@]+$/.test(p.address.trim());
  /** People added at once, each once. */
  const addPeople = (list: { name: string; address: string }[]) => {
    const next = [...people];
    for (const p of list) {
      const address = p.address.trim();
      if (!next.some((x) => x.address.toLowerCase() === address.toLowerCase())) next.push({ name: p.name.trim(), address });
    }
    onChange(next);
    suggestions.clear();
  };
  const add = (person: { name: string; address: string }) => {
    if (!isAddress(person)) return false;
    addPeople([person]);
    setTyped("");
    return true;
  };
  /** What's typed (or pasted), as people: every address in it, a list too; what isn't one stays typed. */
  const addTyped = (text: string) => {
    const entries = splitAddresses(text);
    const found = entries.map(parseEntry).filter(isAddress);
    if (!found.length) return false;
    addPeople(found);
    setTyped(entries.filter((e) => !isAddress(parseEntry(e))).join("; "));
    return true;
  };

  return (
    <div className="relative">
      <div className="flex min-h-8 flex-wrap items-center gap-1 py-1">
        {people.map((p) => (
          <span key={p.address} className="inline-flex h-6 max-w-full items-center gap-1 rounded-full bg-base-content/[0.07] pl-1 pr-0.5">
            <Avatar name={displayName(p)} size="xs" />
            <span className="min-w-0 truncate" data-tip={p.address}>
              {displayName(p)}
            </span>
            <button
              aria-label={`Remove attendee ${p.address}`}
              className="grid size-5 shrink-0 place-items-center rounded-full text-text-2 hover:bg-base-content/10"
              onClick={() => onChange(people.filter((x) => x !== p))}
            >
              <X size={12} />
            </button>
          </span>
        ))}
        <input
          aria-label="Add Attendee"
          placeholder={people.length ? "" : "Add Attendee"}
          className="h-6 min-w-24 flex-1 bg-transparent outline-none placeholder:text-text-3"
          value={typed}
          onFocus={() => setFocused(true)}
          onBlur={() => {
            setFocused(false);
            if (typed.trim()) addTyped(typed);
          }}
          onChange={(e) => setTyped(e.target.value)}
          onPaste={(e) => {
            const pasted = e.clipboardData.getData("text");
            if (!/[,;\r\n]/.test(pasted)) return;
            e.preventDefault();
            // In place of what's selected, as pasting does; a list a line each is a list too.
            const input = e.currentTarget;
            const from = input.selectionStart ?? typed.length;
            const to = input.selectionEnd ?? typed.length;
            const text = typed.slice(0, from) + pasted.replace(/\s*[\r\n]+\s*/g, "; ") + typed.slice(to);
            if (!addTyped(text)) setTyped(text);
          }}
          onKeyDown={(e) => {
            if (shown.length && (e.key === "ArrowDown" || e.key === "ArrowUp")) {
              e.preventDefault();
              suggestions.move(e.key === "ArrowDown" ? 1 : -1, shown.length);
            } else if (!composing(e) && (e.key === "Enter" || e.key === "Tab" || e.key === "," || e.key === ";")) {
              const picked = shown[suggestions.active];
              if (!typed.trim()) return;
              if (picked ? add(picked) : addTyped(typed)) e.preventDefault();
            } else if (e.key === "Backspace" && !typed && people.length) {
              onChange(people.slice(0, -1));
            } else if (e.key === "Escape" && shown.length) {
              suggestions.dismiss();
            }
          }}
        />
      </div>
      {shown.length > 0 && (
        <SuggestionList
          contacts={shown}
          active={suggestions.active}
          className="left-0 right-0 top-full mt-1"
          onHover={suggestions.setActive}
          onPick={(c) => add(c)}
          onNever={suggestions.never}
        />
      )}
    </div>
  );
}
