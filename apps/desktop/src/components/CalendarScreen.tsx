import { ArrowLeft, Check, ChevronDown, ChevronLeft, ChevronRight, LoaderCircle, PanelRight, Plus, Video, X } from "lucide-react";
import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState, type ReactNode } from "react";
import {
  answerEvent,
  cancelSignIn,
  deleteEvent,
  getPrefs,
  listCalendars,
  listEvents,
  openExternal,
  reconnectAccount,
  refreshCalendars,
  saveEvent,
} from "../api";
import { useAccounts } from "../lib/accounts";
import {
  DAY_MS,
  addDays,
  addMonths,
  countdown,
  eventDate,
  eventTime,
  loadHidden,
  loadKind,
  loadSidePanel,
  nextMeeting,
  onDay,
  saveHidden,
  saveKind,
  saveSidePanel,
  startOfDay,
  startOfMonth,
  stepAnchor,
  viewDays,
  type CalendarKind,
} from "../lib/calendar";
import { demoAnswer, demoCalendars, demoDeleteEvent, demoListEvents, demoSaveEvent } from "../lib/demoCalendar";
import {
  atMinutes,
  copyOf,
  dayOf,
  draftOf,
  fromUtcDate,
  minutesOf,
  newDraft,
  newEventStart,
  toUtcDate,
  type DragOutcome,
  type NewEventDefaults,
} from "../lib/eventDraft";
import { isInsideOverlay, isTypingTarget } from "../lib/shortcuts";
import type {
  AccountId,
  AccountView,
  CalendarEvent,
  CalendarInfo,
  CalendarList,
  EventDraft,
  EventScope,
  InviteAnswer,
} from "../types";
import AccountMark from "./AccountMark";
import { MonthGrid, WeekGrid, type GridMenuTarget } from "./CalendarGrid";
import { ChoiceDialog } from "./Dialog";
import { Popover } from "./Dropdown";
import EventEditor from "./EventEditor";
import EventPreview from "./EventPreview";
import { callService } from "./InviteCard";

/** Servers are asked again this often while the calendar is open, and on focus if it's been this long. */
const REFRESH_MS = 5 * 60_000;
const FOCUS_REFRESH_MS = 60_000;
/** Narrower than this, the side panel floats over the grid. */
const SIDE_DOCK_MIN = 925;
const DISMISSED_KEY = "spark.calendarUnsupportedSeen";
const MINUTE = 60_000;

const KINDS: { kind: CalendarKind; label: string; key: string }[] = [
  { kind: "day", label: "Day", key: "d" },
  { kind: "week", label: "Week", key: "w" },
  { kind: "month", label: "Month", key: "m" },
];

/** Where events come from: the servers through the app, or the demo. */
interface Source {
  calendars: () => Promise<CalendarList>;
  events: (start: number, end: number) => Promise<CalendarEvent[]>;
  refresh: (start: number, end: number) => Promise<void>;
  answer: (id: number, answer: InviteAnswer) => Promise<void>;
  /** Makes or changes an event, then reads [start, end) of its calendar again. */
  save: (
    id: number | null,
    draft: EventDraft,
    before: EventDraft | null,
    scope: EventScope,
    start: number,
    end: number,
  ) => Promise<void>;
  remove: (id: number, scope: EventScope, start: number, end: number) => Promise<void>;
}

const LIVE: Source = {
  calendars: listCalendars,
  events: listEvents,
  refresh: refreshCalendars,
  answer: answerEvent,
  save: saveEvent,
  remove: deleteEvent,
};
const DEMO: Source = {
  calendars: () => Promise.resolve(demoCalendars()),
  events: (start, end) => Promise.resolve(demoListEvents(start, end)),
  refresh: () => Promise.resolve(),
  answer: demoAnswer,
  save: (id, draft, _before, scope) => demoSaveEvent(id, draft, scope),
  remove: (id, scope) => demoDeleteEvent(id, scope),
};

/** Settings → Calendar's length and alerts for new events; the defaults if they can't be read. */
async function newEventDefaults(): Promise<NewEventDefaults> {
  const prefs = await getPrefs().catch(() => null);
  return prefs
    ? { minutes: prefs.eventMinutes, alert: prefs.eventAlert, allDayAlert: prefs.allDayAlert }
    : { minutes: 60, alert: 10, allDayAlert: 720 };
}

/** What a move being saved is for: the event, or every occurrence of its series. */
const savingKey = (e: CalendarEvent) => e.series ?? String(e.id);

/** A question the screen asks, and what to do with its answer. */
interface Question {
  title: string;
  message: string;
  choices: { label: string; value: string; primary?: boolean; danger?: boolean }[];
  answer: (value: string | null) => void;
}

/**
 * The calendar, Spark's way, in place of the list and the reader: Day, Week
 * or Month of every account's calendars, an event's details beside it, and
 * a side panel with a small month and the next meeting. New Event (Ctrl+E)
 * and an own event's Edit open the editor; Delete asks first, and about
 * repeating events which occurrences.
 */
export default function CalendarScreen({
  demo,
  focus,
  onClose,
}: {
  demo: boolean;
  /** A time whose day to show, and an event there to open; a new object shows it again. */
  focus?: { at: number; event?: number };
  onClose: () => void;
}) {
  const source = demo ? DEMO : LIVE;
  const accounts = useAccounts();
  const [kind, setKind] = useState<CalendarKind>(loadKind);
  const [anchor, setAnchor] = useState(() => startOfDay(focus?.at ?? Date.now()));
  const [hidden, setHidden] = useState(loadHidden);
  const [list, setList] = useState<CalendarList>({ calendars: [], accounts: [] });
  const [events, setEvents] = useState<CalendarEvent[]>([]);
  const [refreshing, setRefreshing] = useState(false);
  const [preview, setPreview] = useState<{ id: number; anchor: DOMRect } | null>(null);
  const [editor, setEditor] = useState<{
    event?: CalendarEvent;
    initial: EventDraft;
    defaults: NewEventDefaults;
    anchor: DOMRect;
    modal: boolean;
    /** A copy not saved yet (Duplicate, Paste): closing asks even without changes. */
    unsaved?: boolean;
  } | null>(null);
  const [menu, setMenu] = useState<{ target: GridMenuTarget; x: number; y: number } | null>(null);
  /** The event Copy remembered, for Paste Event. */
  const [copied, setCopied] = useState<CalendarEvent | null>(null);
  /** A word that the last thing happened ("Event copied"), for a moment. */
  const [notice, setNotice] = useState<string | null>(null);
  const noticeTimer = useRef(0);
  /** The open editor's close, which asks about changes first; the calendar closes after it. */
  const closeEditor = useRef<((after: () => void) => void) | null>(null);
  /** The editor or a question is up, as of the last render: a drag that ends meanwhile does nothing. */
  const busyNow = useRef(false);
  /** Events (or series) whose move is being saved; `savingNow` as of now, for a drag ending meanwhile. */
  const [saving, setSaving] = useState<ReadonlySet<string>>(new Set());
  const savingNow = useRef<ReadonlySet<string>>(saving);
  const [question, setQuestion] = useState<Question | null>(null);
  /** What went wrong deleting an event, until the next try. */
  const [failed, setFailed] = useState<string | null>(null);
  busyNow.current = editor !== null || question !== null;
  const [now, setNow] = useState(Date.now);
  const box = useRef<HTMLElement>(null);
  const [width, setWidth] = useState(1200);
  const [side, setSide] = useState(() => loadSidePanel() ?? window.innerWidth >= 1100);

  const days = useMemo(() => viewDays(kind, anchor), [kind, anchor]);
  const start = days[0];
  const end = addDays(days[days.length - 1], 1);

  // A load that a newer one replaced is dropped.
  const loads = useRef(0);
  const load = useCallback(async () => {
    const run = ++loads.current;
    try {
      // All-day events are stored by date: a day either side covers every time zone.
      const [calendars, found] = await Promise.all([source.calendars(), source.events(start - DAY_MS, end + DAY_MS)]);
      if (run !== loads.current) return;
      setList(calendars);
      setEvents(found);
    } catch {
      // Keep what's shown; the next refresh tries again.
    }
  }, [source, start, end]);

  const busy = useRef(false);
  /** Asked for while one was running (e.g. the user moved on a week): runs next, for the range shown then. */
  const again = useRef(false);
  const latest = useRef<() => Promise<void>>(() => Promise.resolve());
  const lastRefresh = useRef(0);
  const refresh = useCallback(async () => {
    if (busy.current) {
      again.current = true;
      return;
    }
    busy.current = true;
    lastRefresh.current = Date.now();
    setRefreshing(true);
    try {
      await source.refresh(start, end).catch(() => {});
      await load();
    } finally {
      busy.current = false;
      setRefreshing(false);
      if (again.current) {
        again.current = false;
        void latest.current();
      }
    }
  }, [source, start, end, load]);
  useEffect(() => {
    latest.current = refresh;
  }, [refresh]);

  // What's stored shows at once; then the servers are asked.
  useEffect(() => {
    void load().then(refresh);
  }, [load, refresh]);

  useEffect(() => {
    const timer = window.setInterval(() => void refresh(), REFRESH_MS);
    const onFocus = () => {
      if (Date.now() - lastRefresh.current > FOCUS_REFRESH_MS) void refresh();
    };
    window.addEventListener("focus", onFocus);
    return () => {
      window.clearInterval(timer);
      window.removeEventListener("focus", onFocus);
    };
  }, [refresh]);

  useEffect(() => {
    const timer = window.setInterval(() => setNow(Date.now()), 30_000);
    return () => window.clearInterval(timer);
  }, []);

  useLayoutEffect(() => {
    const el = box.current;
    if (!el) return;
    const observer = new ResizeObserver(() => setWidth(el.clientWidth));
    observer.observe(el);
    setWidth(el.clientWidth);
    return () => observer.disconnect();
  }, []);

  /** The event a calendar alert or the meeting pop-up asked for, opened once it's drawn. */
  const wanted = useRef<number | undefined>(undefined);
  useEffect(() => {
    if (!focus) return;
    setAnchor(startOfDay(focus.at));
    setPreview(null);
    wanted.current = focus.event;
  }, [focus]);

  const changeKind = (next: CalendarKind) => {
    setKind(next);
    saveKind(next);
    setPreview(null);
    wanted.current = undefined;
  };
  const go = (n: number) => {
    setAnchor((a) => stepAnchor(kind, a, n));
    setPreview(null);
    wanted.current = undefined;
  };
  const goToday = () => {
    setAnchor(startOfDay(Date.now()));
    setPreview(null);
    wanted.current = undefined;
  };
  const pickDay = (day: number) => {
    setAnchor(day);
    changeKind("day");
  };
  const toggleSide = () => {
    setSide((s) => {
      saveSidePanel(!s);
      return !s;
    });
  };

  // Keys: Esc closes; T today; D/W/M the view; arrows page; Ctrl+E a new event; Delete the
  // event open. Others are kept from the hidden list. The editor and its questions have their own.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.defaultPrevented || editor || question || menu) return;
      // A drag on the grid takes Esc for itself.
      if (document.documentElement.hasAttribute("data-calendar-drag")) return;
      if (isTypingTarget(e.target) || isInsideOverlay(e.target) || document.querySelector("[data-composer]")) return;
      const key = e.key.length === 1 ? e.key.toLowerCase() : e.key;
      const handled = () => {
        e.preventDefault();
        e.stopPropagation();
      };
      if ((e.ctrlKey || e.metaKey) && !e.altKey && !e.shiftKey && key === "e") {
        handled();
        void newEvent();
        return;
      }
      if ((e.ctrlKey || e.metaKey) && !e.altKey && !e.shiftKey && key === "c" && open && !window.getSelection()?.toString()) {
        handled();
        copyEvent(open);
        return;
      }
      if (e.ctrlKey || e.metaKey || e.altKey) return;
      if ((key === "Delete" || key === "Backspace") && open && editable(open)) {
        handled();
        void removeEvent(open);
      } else if (key === "Escape") {
        // An open menu or the event's details close first, on their own.
        if (document.querySelector("[role='dialog'], [role='menu']")) return;
        handled();
        onClose();
      } else if (key === "t") {
        handled();
        goToday();
      } else if (key === "ArrowLeft" || key === "ArrowRight") {
        handled();
        go(key === "ArrowLeft" ? -1 : 1);
      } else if (KINDS.some((k) => k.key === key)) {
        handled();
        changeKind(KINDS.find((k) => k.key === key)!.kind);
      } else if (e.key.length === 1 || ["Enter", "Delete", "Backspace", "ArrowUp", "ArrowDown"].includes(e.key)) {
        // Mail shortcuts would act on the list behind the calendar.
        e.preventDefault();
      }
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  });

  const calendars = useMemo(() => new Map(list.calendars.map((c) => [c.id, c])), [list.calendars]);
  const shown = useMemo(
    () => events.filter((e) => !hidden.has(e.calendar) && calendars.has(e.calendar)),
    [events, hidden, calendars],
  );
  const open = preview ? events.find((e) => e.id === preview.id) : undefined;

  useEffect(() => {
    const id = wanted.current;
    if (id === undefined || !shown.some((e) => e.id === id)) return;
    const block = box.current?.querySelector<HTMLElement>(`[data-event="${id}"]`);
    if (!block) return;
    wanted.current = undefined;
    block.scrollIntoView({ block: "nearest" });
    setPreview({ id, anchor: block.getBoundingClientRect() });
  }, [shown, focus]);

  /** The user's own event, in their own calendar: they can change and delete it. */
  const editable = (e: CalendarEvent) => calendars.get(e.calendar)?.own === true && e.response === "organizer";
  /** Whether it's not still saving a move: one at a time, so a second drag can't race the first. */
  const draggable = (e: CalendarEvent) => editable(e) && !saving.has(savingKey(e));

  const ask = (q: Omit<Question, "answer">) =>
    new Promise<string | null>((answer) => setQuestion({ ...q, answer }));

  /** Where a new event goes: the default calendar of the user's own that show, else any of theirs. */
  const newEventCalendar = async () => {
    const mine = list.calendars.filter((c) => c.own);
    const target =
      mine.find((c) => c.isDefault && !hidden.has(c.id)) ?? mine.find((c) => !hidden.has(c.id)) ?? mine[0];
    if (!target) {
      await ask({
        title: "Turn on calendars",
        message: "To create events, add a Microsoft 365 account with its calendar in Settings → Accounts.",
        choices: [{ label: "Got it", value: "ok", primary: true }],
      });
    }
    return target;
  };

  /** Spark's New Event: the next quarter hour today (going to today first), in the default calendar. */
  const newEvent = async () => {
    const target = await newEventCalendar();
    if (!target) return;
    const defaults = await newEventDefaults();
    const today = startOfDay(Date.now());
    if (!days.includes(today)) goToday();
    setPreview(null);
    setEditor({
      initial: newDraft(target.id, newEventStart(Date.now(), today), false, defaults),
      defaults,
      anchor: new DOMRect(),
      modal: true,
    });
  };

  /** Time dragged across or double-clicked on the grid: a new event there (`end` null: the default length). */
  const createAt = async (start: number, end: number | null, allDay: boolean, anchor: DOMRect) => {
    if (busyNow.current) return;
    const target = await newEventCalendar();
    if (!target) return;
    const defaults = await newEventDefaults();
    const draft = newDraft(target.id, start, allDay, defaults);
    setPreview(null);
    setEditor({ initial: end === null ? draft : { ...draft, end }, defaults, anchor, modal: kind === "day" });
  };

  /**
   * An event dragged to new times on the grid: it's there at once, then saved (a repeating one asks
   * which occurrences first); back where it was if that's cancelled or fails.
   */
  const moveEvent = async (e: CalendarEvent, to: DragOutcome) => {
    const key = savingKey(e);
    if (busyNow.current || savingNow.current.has(key)) return;
    const mark = (on: boolean) => {
      const next = new Set(savingNow.current);
      if (on) next.add(key);
      else next.delete(key);
      savingNow.current = next;
      setSaving(next);
    };
    mark(true);
    try {
      await saveMove(e, to);
    } finally {
      mark(false);
    }
  };
  const saveMove = async (e: CalendarEvent, to: DragOutcome) => {
    const defaults = await newEventDefaults();
    const end = to.end ?? to.start + defaults.minutes * MINUTE;
    const draft: EventDraft = {
      ...draftOf(e),
      start: to.start,
      end,
      allDay: to.allDay,
      // Going all day or back takes the alert new events of that kind get.
      ...(to.allDay !== e.allDay ? { reminder: to.allDay ? defaults.allDayAlert : defaults.alert } : {}),
    };
    setFailed(null);
    setEvents((all) => all.map((x) => (x.id === e.id ? { ...x, start: to.start, end, allDay: to.allDay } : x)));
    try {
      if (!(await saveDraft(e, draft, draftOf(e)))) await load();
    } catch (error) {
      setFailed(`Couldn't move "${e.title || "No Title"}": ${String(error)}`);
      await load();
    }
  };

  /** Where an event's editor goes when it's opened from the menu: beside the event, else at the pointer. */
  const eventBox = (e: CalendarEvent, x: number, y: number) =>
    box.current?.querySelector(`[data-event="${e.id}"]`)?.getBoundingClientRect() ?? new DOMRect(x, y, 1, 1);

  /** Of the user's own calendars: `id` if it's one, else where new events go. */
  const ownCalendar = async (id: number) => (calendars.get(id)?.own ? id : (await newEventCalendar())?.id);

  /** Spark's Duplicate: a copy of the event in the editor, saved only by Create. */
  const duplicate = async (e: CalendarEvent, anchor: DOMRect) => {
    const calendar = await ownCalendar(e.calendar);
    if (calendar === undefined) return;
    const defaults = await newEventDefaults();
    setPreview(null);
    setEditor({ initial: { ...copyOf(e), calendar }, defaults, anchor, modal: kind === "day", unsaved: true });
  };

  /** Spark's Copy: the event remembered for Paste Event, and "Title (date, time)" on the clipboard. */
  const copyEvent = (e: CalendarEvent) => {
    setCopied(e);
    void navigator.clipboard?.writeText(`${e.title || "No Title"} (${eventDate(e)}, ${eventTime(e)})`).catch(() => {});
    window.clearTimeout(noticeTimer.current);
    setNotice("Event copied");
    noticeTimer.current = window.setTimeout(() => setNotice(null), 2500);
  };

  /** Spark's Paste Event: a copy of the copied event at `target`, the same length, in the editor. */
  const pasteAt = async (target: GridMenuTarget, anchor: DOMRect) => {
    if (!copied) return;
    const calendar = await ownCalendar(copied.calendar);
    if (calendar === undefined) return;
    const length = copied.end - copied.start;
    const start = copied.allDay
      ? target.allDay
        ? target.at
        : toUtcDate(dayOf(target.at))
      : target.allDay
        ? atMinutes(fromUtcDate(target.at), minutesOf(copied.start))
        : target.at;
    const defaults = await newEventDefaults();
    setPreview(null);
    setEditor({
      initial: { ...copyOf(copied), calendar, start, end: start + length },
      defaults,
      anchor,
      modal: kind === "day",
      unsaved: true,
    });
  };

  /** The right-click menu's items for `target`, at `x`, `y`. */
  const menuItems = ({ event: e, ...at }: GridMenuTarget, x: number, y: number): MenuEntry[] => {
    const here = new DOMRect(x, y, 1, 1);
    if (!e) {
      return [
        { label: "New Event", run: () => void createAt(at.at, null, at.allDay, here) },
        ...(copied ? [{ label: "Paste Event", run: () => void pasteAt({ ...at }, here) }] : []),
      ];
    }
    const items: MenuEntry[] = [];
    if (e.response !== "organizer" && !e.canceled) {
      const answers: [InviteAnswer, string][] = [
        ["accepted", "Accept"],
        ["tentative", "Maybe"],
        ["declined", "Decline"],
      ];
      for (const [chosen, label] of answers) {
        items.push({
          label,
          checked: e.response === chosen,
          run: () => void answer(e, chosen).catch((error: unknown) => setFailed(`Couldn't send your answer: ${String(error)}`)),
        });
      }
      items.push("-");
    }
    if (editable(e)) items.push({ label: "Edit", run: () => void editEvent(e, eventBox(e, x, y)) });
    if (list.calendars.some((c) => c.own)) items.push({ label: "Duplicate", run: () => void duplicate(e, eventBox(e, x, y)) });
    items.push({ label: "Copy", run: () => copyEvent(e) });
    if (editable(e)) items.push({ label: "Delete", danger: true, run: () => void removeEvent(e) });
    items.push("-", {
      label: "Show only this calendar",
      run: () => {
        const next = new Set(list.calendars.filter((c) => c.id !== e.calendar).map((c) => c.id));
        setHidden(next);
        saveHidden(next);
      },
    });
    return items;
  };

  const editEvent = async (e: CalendarEvent, anchor: DOMRect) => {
    const defaults = await newEventDefaults();
    setPreview(null);
    setEditor({ event: e, initial: draftOf(e), defaults, anchor, modal: kind === "day" });
  };

  /** Saves the editor's draft; a repeating event asks which occurrences first. False to go on editing. */
  /** `before` is the event as the editor opened it: only what changed from it is saved. */
  const saveDraft = async (event: CalendarEvent | undefined, draft: EventDraft, before: EventDraft | null) => {
    let scope: EventScope = "one";
    if (event?.repeats) {
      const choice = await ask({
        title: "You're changing a repeating event.",
        message: "Do you want to change only this occurrence of the event, or all occurrences?",
        choices: [
          { label: "Back to edit", value: "back" },
          { label: "All", value: "all" },
          { label: "Only This Event", value: "one", primary: true },
        ],
      });
      if (choice !== "one" && choice !== "all") return false;
      scope = choice;
    }
    await source.save(event?.id ?? null, draft, before, scope, start - DAY_MS, end + DAY_MS);
    await load();
    return true;
  };

  /** Spark's Delete: are you sure, then for a repeating event which occurrences. Gone at once; back if it fails. */
  const removeEvent = async (e: CalendarEvent) => {
    const sure = await ask({
      title: "Do you want to delete the event?",
      message: "You can't undo this action.",
      choices: [
        { label: "No", value: "no" },
        { label: "Yes", value: "yes", danger: true },
      ],
    });
    if (sure !== "yes") return;
    let scope: EventScope = "one";
    if (e.repeats) {
      const choice = await ask({
        title: "Delete an event",
        message: "Do you want to delete all occurrences or only this event?",
        choices: [
          { label: "Cancel", value: "cancel" },
          { label: "All", value: "all" },
          { label: "Only This Event", value: "one", primary: true },
        ],
      });
      if (choice !== "one" && choice !== "all") return;
      scope = choice;
    }
    setPreview(null);
    setFailed(null);
    const gone = (x: CalendarEvent) => x.id === e.id || (scope === "all" && e.series !== undefined && x.series === e.series);
    setEvents((all) => all.filter((x) => !gone(x)));
    try {
      await source.remove(e.id, scope, start - DAY_MS, end + DAY_MS);
    } catch (error) {
      setFailed(`Couldn't delete "${e.title || "No Title"}": ${String(error)}`);
    }
    await load();
  };

  const answer = (e: CalendarEvent, chosen: InviteAnswer) => {
    const before = e.response;
    const me = accounts.find((a) => a.id === e.account)?.email.toLowerCase();
    const set = (response: CalendarEvent["response"]) =>
      setEvents((all) =>
        all.map((x) =>
          x.id === e.id
            ? {
                ...x,
                response,
                attendees: x.attendees.map((p) =>
                  p.address.toLowerCase() === me
                    ? { ...p, answer: response === "none" || response === "organizer" ? undefined : response }
                    : p,
                ),
              }
            : x,
        ),
      );
    set(chosen);
    return source.answer(e.id, chosen).catch((error: unknown) => {
      set(before);
      throw error;
    });
  };

  const dockSide = width >= SIDE_DOCK_MIN;
  const todayVisible = days.some((d) => d === startOfDay(now));
  const unit = KINDS.find((k) => k.kind === kind)!.label;

  return (
    <section ref={box} aria-label="Calendar" className="relative flex min-w-0 flex-1 flex-col pt-8">
      <header className="flex h-[58px] shrink-0 items-center gap-2 pl-3 pr-4">
        <HeaderButton tip="Close calendar" onClick={() => (closeEditor.current ? closeEditor.current(onClose) : onClose())}>
          <ArrowLeft size={18} />
        </HeaderButton>
        <h1 className="ml-1 min-w-0 truncate text-xl font-semibold">{title(kind, anchor, days)}</h1>
        {refreshing && <LoaderCircle size={14} className="shrink-0 animate-spin text-text-2" aria-label="Updating" />}
        <div className="flex-1" />
        <div className="flex h-8 shrink-0 items-center rounded-lg border border-hairline">
          <button
            className="grid h-full w-8 place-items-center rounded-l-lg text-text-2 hover:bg-base-content/5 hover:text-base-content"
            data-tip={`Previous ${unit}`}
            aria-label={`Previous ${unit}`}
            onClick={() => go(-1)}
          >
            <ChevronLeft size={17} />
          </button>
          <button
            className="h-full border-x border-hairline px-3 text-[13px] font-medium hover:bg-base-content/5"
            data-tip={todayVisible ? "Today" : "Go to Today"}
            onClick={goToday}
          >
            Today
          </button>
          <button
            className="grid h-full w-8 place-items-center rounded-r-lg text-text-2 hover:bg-base-content/5 hover:text-base-content"
            data-tip={`Next ${unit}`}
            aria-label={`Next ${unit}`}
            onClick={() => go(1)}
          >
            <ChevronRight size={17} />
          </button>
        </div>
        <div className="flex h-8 shrink-0 rounded-lg bg-chip p-0.5" role="tablist" aria-label="View">
          {KINDS.map((k) => (
            <button
              key={k.kind}
              role="tab"
              aria-selected={kind === k.kind}
              className={`rounded-md px-3 text-[13px] font-medium ${
                kind === k.kind ? "bg-base-100 shadow-sm" : "text-text-2 hover:text-base-content"
              }`}
              onClick={() => changeKind(k.kind)}
            >
              {k.label}
            </button>
          ))}
        </div>
        <CalendarsMenu
          accounts={accounts}
          calendars={list.calendars}
          hidden={hidden}
          onChange={(next) => {
            setHidden(next);
            saveHidden(next);
          }}
        />
        <button
          className="btn btn-primary btn-sm h-8 min-h-8 shrink-0 gap-1 rounded-lg pl-2 pr-3"
          data-tip="New event (Ctrl+E)"
          onClick={() => void newEvent()}
        >
          <Plus size={16} />
          New event
        </button>
        <HeaderButton tip={side ? "Hide Side Panel" : "Show Side Panel"} pressed={side} onClick={toggleSide}>
          <PanelRight size={18} />
        </HeaderButton>
      </header>

      <Notices list={list} accounts={accounts} onReconnected={() => void refresh()} />
      {failed && (
        <div className="flex shrink-0 items-center gap-2 px-4 pb-2 text-xs text-error">
          <span className="min-w-0 flex-1 select-text truncate" data-tip={failed}>
            {failed}
          </span>
          <button className="grid size-5 place-items-center rounded hover:bg-base-content/10" aria-label="Dismiss" onClick={() => setFailed(null)}>
            <X size={12} />
          </button>
        </div>
      )}

      <div className="relative flex min-h-0 flex-1 border-t border-hairline">
        <div className="flex min-w-0 flex-1 flex-col">
          {kind === "month" ? (
            <MonthGrid
              days={days}
              month={new Date(anchor).getMonth()}
              events={shown}
              calendars={calendars}
              now={now}
              selected={open?.id}
              onOpen={(e, rect) => !editor && setPreview({ id: e.id, anchor: rect })}
              onPickDay={pickDay}
              canEdit={draggable}
              onCreate={(start, end, allDay, anchor) => void createAt(start, end, allDay, anchor)}
              onChange={(e, to) => void moveEvent(e, to)}
              onMenu={(target, x, y) => setMenu({ target, x, y })}
            />
          ) : (
            <WeekGrid
              key={kind}
              days={days}
              events={shown}
              calendars={calendars}
              now={now}
              selected={open?.id}
              onOpen={(e, rect) => !editor && setPreview({ id: e.id, anchor: rect })}
              onPickDay={pickDay}
              canEdit={draggable}
              onCreate={(start, end, allDay, anchor) => void createAt(start, end, allDay, anchor)}
              onChange={(e, to) => void moveEvent(e, to)}
              onMenu={(target, x, y) => setMenu({ target, x, y })}
            />
          )}
        </div>
        {side && (
          <SidePanel
            docked={dockSide}
            kind={kind}
            anchor={anchor}
            days={days}
            events={shown}
            now={now}
            onPickDay={(day) => {
              setAnchor(day);
              setPreview(null);
              if (!dockSide) toggleSide();
            }}
            onOpen={(e, rect) => !editor && setPreview({ id: e.id, anchor: rect })}
          />
        )}
      </div>

      {open && preview && (
        <EventPreview
          event={open}
          calendar={calendars.get(open.calendar)}
          anchor={preview.anchor}
          modal={kind === "day"}
          onAnswer={(a) => answer(open, a)}
          onEdit={editable(open) ? () => void editEvent(open, preview.anchor) : undefined}
          onDelete={editable(open) ? () => void removeEvent(open) : undefined}
          onClose={() => setPreview(null)}
        />
      )}
      {editor && (
        <EventEditor
          event={editor.event}
          initial={editor.initial}
          calendars={list.calendars}
          accounts={accounts}
          anchor={editor.anchor}
          modal={editor.modal}
          defaults={editor.defaults}
          unsaved={editor.unsaved}
          closeRef={closeEditor}
          onSave={(draft) => saveDraft(editor.event, draft, editor.event ? editor.initial : null)}
          onClose={() => setEditor(null)}
        />
      )}
      {menu && (
        <GridMenu
          x={menu.x}
          y={menu.y}
          label={menu.target.event ? `${menu.target.event.title || "No Title"} options` : "Calendar options"}
          items={menuItems(menu.target, menu.x, menu.y)}
          onClose={() => setMenu(null)}
        />
      )}
      {notice && (
        <div
          role="status"
          className="pointer-events-none fixed bottom-6 left-1/2 z-[2040] -translate-x-1/2 rounded-full bg-neutral px-4 py-2 text-[13px] text-neutral-content shadow-popup"
        >
          {notice}
        </div>
      )}
      {question && (
        <ChoiceDialog
          title={question.title}
          message={question.message}
          choices={question.choices}
          onChoose={(value) => {
            setQuestion(null);
            question.answer(value);
          }}
        />
      )}
    </section>
  );
}

/** A right-click menu's item, or "-" between groups. */
type MenuEntry = { label: string; run: () => void; danger?: boolean; checked?: boolean } | "-";

/** A menu at the pointer, kept in the window; Esc or a click elsewhere closes it. */
function GridMenu({
  x,
  y,
  label,
  items,
  onClose,
}: {
  x: number;
  y: number;
  label: string;
  items: MenuEntry[];
  onClose: () => void;
}) {
  const ref = useRef<HTMLUListElement>(null);
  const [place, setPlace] = useState({ left: x, top: y });
  const close = useRef(onClose);
  close.current = onClose;
  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    setPlace({
      left: Math.max(8, Math.min(x, window.innerWidth - el.offsetWidth - 8)),
      top: Math.max(8, Math.min(y, window.innerHeight - el.offsetHeight - 8)),
    });
  }, [x, y]);
  useEffect(() => {
    const onPointer = (e: PointerEvent) => {
      if (!ref.current?.contains(e.target as Node)) close.current();
    };
    // Capture, so Esc closes the menu and not the calendar.
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      e.stopPropagation();
      close.current();
    };
    window.addEventListener("pointerdown", onPointer, true);
    window.addEventListener("keydown", onKey, true);
    return () => {
      window.removeEventListener("pointerdown", onPointer, true);
      window.removeEventListener("keydown", onKey, true);
    };
  }, []);
  const checks = items.some((i) => i !== "-" && i.checked !== undefined);
  return (
    <ul
      ref={ref}
      role="menu"
      aria-label={label}
      className="menu menu-sm fixed z-[2030] w-56 gap-0.5 rounded-box border border-hairline bg-popover p-1 text-[13px] text-base-content shadow-popup"
      style={place}
      onContextMenu={(e) => e.preventDefault()}
    >
      {items.map((item, i) =>
        item === "-" ? (
          <li key={i} role="separator" className="my-1 h-px bg-hairline" />
        ) : (
          <li key={item.label} role="none">
            <button
              role="menuitem"
              className={`rounded-md ${item.danger ? "text-error" : ""}`}
              onClick={() => {
                onClose();
                item.run();
              }}
            >
              {checks && <Check size={14} className={item.checked ? "" : "invisible"} />}
              <span className="flex-1 text-left">{item.label}</span>
            </button>
          </li>
        ),
      )}
    </ul>
  );
}

const monthYear = new Intl.DateTimeFormat(undefined, { month: "long", year: "numeric" });
const shortMonth = new Intl.DateTimeFormat(undefined, { month: "short" });
const shortMonthYear = new Intl.DateTimeFormat(undefined, { month: "short", year: "numeric" });

/** "October 2026"; a week across two months is "Oct – Nov 2026". */
function title(kind: CalendarKind, anchor: number, days: number[]) {
  if (kind !== "week") return monthYear.format(anchor);
  const first = new Date(days[0]);
  const last = new Date(days[days.length - 1]);
  if (first.getMonth() === last.getMonth()) return monthYear.format(first);
  if (first.getFullYear() === last.getFullYear()) return `${shortMonth.format(first)} – ${shortMonthYear.format(last)}`;
  return `${shortMonthYear.format(first)} – ${shortMonthYear.format(last)}`;
}

function HeaderButton({
  tip,
  pressed,
  onClick,
  children,
}: {
  tip: string;
  pressed?: boolean;
  onClick: () => void;
  children: ReactNode;
}) {
  return (
    <button
      data-tip={tip}
      aria-label={tip}
      aria-pressed={pressed}
      className={`grid size-8 shrink-0 place-items-center rounded-lg text-text-2 hover:bg-base-content/10 hover:text-base-content ${
        pressed ? "bg-base-content/10 text-base-content" : ""
      }`}
      onClick={onClick}
    >
      {children}
    </button>
  );
}

/** Which calendars show, per account, with "Hide all" / "Select all". */
function CalendarsMenu({
  accounts,
  calendars,
  hidden,
  onChange,
}: {
  accounts: AccountView[];
  calendars: CalendarInfo[];
  hidden: ReadonlySet<number>;
  onChange: (hidden: Set<number>) => void;
}) {
  const groups = accounts
    .map((a) => ({ account: a, calendars: calendars.filter((c) => c.account === a.id) }))
    .filter((g) => g.calendars.length > 0);
  const toggle = (ids: number[], hide: boolean) => {
    const next = new Set(hidden);
    for (const id of ids) {
      if (hide) next.add(id);
      else next.delete(id);
    }
    onChange(next);
  };
  return (
    <Popover
      align="end"
      label="Calendars"
      className="w-64"
      trigger={({ open, toggle: show }) => (
        <button
          className={`flex h-8 shrink-0 items-center gap-1 rounded-lg px-2.5 text-[13px] font-medium hover:bg-base-content/10 ${
            open ? "bg-base-content/10" : ""
          }`}
          aria-haspopup="dialog"
          onClick={show}
        >
          Calendars
          <ChevronDown size={14} className="text-text-2" />
        </button>
      )}
    >
      {() =>
        groups.length === 0 ? (
          <p className="px-1 py-1 text-text-2">No calendars yet.</p>
        ) : (
          <div className="space-y-3">
            {groups.map(({ account, calendars: own }) => {
              const ids = own.map((c) => c.id);
              const allShown = ids.every((id) => !hidden.has(id));
              return (
                <div key={account.id}>
                  <div className="mb-1 flex items-center gap-2 px-1">
                    <AccountMark color={account.color} />
                    <span className="min-w-0 flex-1 truncate font-semibold">{account.label}</span>
                    <button className="text-xs font-medium text-primary hover:underline" onClick={() => toggle(ids, allShown)}>
                      {allShown ? "Hide all" : "Select all"}
                    </button>
                  </div>
                  {own.map((c) => (
                    <label key={c.id} className="flex h-8 cursor-pointer items-center gap-2.5 rounded-md px-1 hover:bg-base-content/5">
                      <input
                        type="checkbox"
                        className="checkbox checkbox-xs rounded"
                        style={{ borderColor: c.color, ...(hidden.has(c.id) ? {} : { background: c.color, color: "#fff" }) }}
                        checked={!hidden.has(c.id)}
                        onChange={(e) => toggle([c.id], !e.target.checked)}
                      />
                      <span className="min-w-0 flex-1 truncate">{c.name}</span>
                    </label>
                  ))}
                </div>
              );
            })}
          </div>
        )
      }
    </Popover>
  );
}

/** Accounts whose calendar can't show, and why: sign in again, an error, or not supported yet. */
function Notices({
  list,
  accounts,
  onReconnected,
}: {
  list: CalendarList;
  accounts: AccountView[];
  onReconnected: () => void;
}) {
  const [seen, setSeen] = useState<AccountId[]>(() => {
    try {
      return JSON.parse(localStorage.getItem(DISMISSED_KEY) ?? "[]") as AccountId[];
    } catch {
      return [];
    }
  });
  const [waiting, setWaiting] = useState<AccountId | null>(null);
  const [failed, setFailed] = useState<{ account: AccountId; message: string } | null>(null);
  const label = (id: AccountId) => accounts.find((a) => a.id === id)?.label ?? "this account";

  const reconnect = (id: AccountId) => {
    setWaiting(id);
    setFailed(null);
    reconnectAccount(id).then(
      () => {
        setWaiting(null);
        onReconnected();
      },
      (error: unknown) => {
        setWaiting(null);
        if (!String(error).includes("cancelled")) setFailed({ account: id, message: String(error) });
      },
    );
  };
  const dismiss = (ids: AccountId[]) => {
    const next = [...new Set([...seen, ...ids])];
    setSeen(next);
    localStorage.setItem(DISMISSED_KEY, JSON.stringify(next));
  };

  const signIn = list.accounts.filter((a) => a.status === "signIn");
  const errors = list.accounts.filter((a) => a.status === "error");
  const unsupported = list.accounts.filter((a) => a.status === "unsupported" && !seen.includes(a.account));
  if (!signIn.length && !errors.length && !unsupported.length) return null;

  return (
    <div className="shrink-0 space-y-1.5 px-4 pb-2">
      {signIn.map(({ account }) => (
        <div key={account} className="flex items-center gap-3 rounded-lg bg-primary/10 px-3 py-2 text-[13px]">
          <span className="min-w-0 flex-1">
            {waiting === account
              ? "Waiting for the browser… Finish signing in to Microsoft there."
              : `Sign in to Microsoft again to show ${label(account)}'s calendar.`}
            {failed?.account === account && <span className="text-error"> {failed.message}</span>}
          </span>
          {waiting === account ? (
            <button className="btn btn-ghost btn-sm h-7 min-h-7" onClick={() => void cancelSignIn()}>
              Cancel
            </button>
          ) : (
            <button className="btn btn-primary btn-sm h-7 min-h-7" disabled={waiting !== null} onClick={() => reconnect(account)}>
              Sign in
            </button>
          )}
        </div>
      ))}
      {errors.map(({ account, message }) => (
        <p key={account} className="select-text truncate text-xs text-text-2" data-tip={message}>
          Couldn't update {label(account)}'s calendar{message ? `: ${message}` : "."}
        </p>
      ))}
      {unsupported.length > 0 && (
        <div className="flex items-center gap-2 text-xs text-text-2">
          <span className="min-w-0 flex-1 truncate">
            Calendars from {unsupported.map((a) => label(a.account)).join(", ")} aren't supported yet; Microsoft 365
            calendars are.
          </span>
          <button
            className="grid size-5 place-items-center rounded hover:bg-base-content/10"
            aria-label="Dismiss"
            onClick={() => dismiss(unsupported.map((a) => a.account))}
          >
            <X size={12} />
          </button>
        </div>
      )}
    </div>
  );
}

const weekdayInitial = new Intl.DateTimeFormat(undefined, { weekday: "narrow" });
const yearFormat = new Intl.DateTimeFormat(undefined, { year: "numeric" });

/**
 * A small month to move around in (a year of months in Month view, which
 * already shows the month), and the next meeting. Floats over the grid
 * when the window is narrow.
 */
function SidePanel({
  docked,
  kind,
  anchor,
  days,
  events,
  now,
  onPickDay,
  onOpen,
}: {
  docked: boolean;
  kind: CalendarKind;
  anchor: number;
  days: number[];
  events: CalendarEvent[];
  now: number;
  onPickDay: (day: number) => void;
  onOpen: (e: CalendarEvent, anchor: DOMRect) => void;
}) {
  const next = nextMeeting(events, now);

  return (
    <aside
      aria-label="Side panel"
      className={`flex w-[300px] shrink-0 flex-col gap-4 overflow-y-auto border-l border-hairline bg-base-100 p-4 ${
        docked ? "" : "absolute inset-y-0 right-0 z-30 shadow-popup"
      }`}
    >
      {kind === "month" ? (
        <MonthPicker anchor={anchor} now={now} onPick={onPickDay} />
      ) : (
        <MiniMonth week={kind === "week"} anchor={anchor} days={days} events={events} now={now} onPickDay={onPickDay} />
      )}

      <div className="rounded-xl border border-hairline p-3">
        <h2 className="text-[11px] font-semibold uppercase tracking-wide text-text-2">Next meeting</h2>
        {next ? (
          <>
            <button
              className="mt-1.5 block w-full text-left"
              onClick={(e) => onOpen(next, e.currentTarget.getBoundingClientRect())}
            >
              <span className="line-clamp-2 text-[15px] font-semibold leading-5">{next.title || "No Title"}</span>
              <span className="mt-0.5 block text-[13px] text-text-2">
                {next.start <= now ? `Ends ${countdown(next.end, now)}` : `Starts ${countdown(next.start, now)}`}
                {" · "}
                {eventTime(next)}
              </span>
            </button>
            {next.callLink && (
              <button
                className="btn btn-primary btn-sm mt-2.5 h-8 min-h-8 w-full gap-1.5 rounded-lg"
                onClick={() => void openExternal(next.callLink!)}
              >
                <Video size={15} />
                Join {callService(next.callLink)}
              </button>
            )}
          </>
        ) : (
          <p className="mt-1.5 text-[13px] text-text-2">Nothing else today or tomorrow.</p>
        )}
      </div>
    </aside>
  );
}

/** The panel's title, with ‹ › to page through months or years. */
function PanelNav({ title, unit, onStep }: { title: string; unit: string; onStep: (n: number) => void }) {
  return (
    <div className="mb-2 flex items-center gap-1">
      <h2 className="min-w-0 flex-1 truncate pl-1 text-[13px] font-semibold">{title}</h2>
      <button
        className="grid size-6 place-items-center rounded text-text-2 hover:bg-base-content/10"
        aria-label={`Previous ${unit}`}
        onClick={() => onStep(-1)}
      >
        <ChevronLeft size={15} />
      </button>
      <button
        className="grid size-6 place-items-center rounded text-text-2 hover:bg-base-content/10"
        aria-label={`Next ${unit}`}
        onClick={() => onStep(1)}
      >
        <ChevronRight size={15} />
      </button>
    </div>
  );
}

/** Today filled; the days shown tinted: the day, or the week's whole row. */
function MiniMonth({
  week,
  anchor,
  days,
  events,
  now,
  onPickDay,
}: {
  week: boolean;
  anchor: number;
  days: number[];
  events: CalendarEvent[];
  now: number;
  onPickDay: (day: number) => void;
}) {
  const [month, setMonth] = useState(() => startOfMonth(anchor));
  useEffect(() => setMonth(startOfMonth(anchor)), [anchor]);
  const grid = viewDays("month", month);
  const rows = Array.from({ length: grid.length / 7 }, (_, i) => grid.slice(i * 7, i * 7 + 7));
  const today = startOfDay(now);
  const shown = new Set(days);

  return (
    <div>
      <PanelNav title={monthYear.format(month)} unit="month" onStep={(n) => setMonth((m) => addMonths(m, n))} />
      <div className="grid grid-cols-7 text-center text-[11px]">
        {grid.slice(0, 7).map((d) => (
          <span key={d} className="pb-1 text-text-2">
            {weekdayInitial.format(d)}
          </span>
        ))}
      </div>
      {rows.map((row) => (
        <div
          key={row[0]}
          className={`my-px grid grid-cols-7 rounded-full ${week && row.some((d) => shown.has(d)) ? "bg-primary/12" : ""}`}
        >
          {row.map((day) => {
            const inMonth = new Date(day).getMonth() === new Date(month).getMonth();
            const busy = onDay(events, day).length > 0;
            return (
              <button
                key={day}
                className={`relative mx-auto grid size-7 place-items-center rounded-full text-xs ${
                  day === today
                    ? "bg-primary font-semibold text-primary-content"
                    : `${!week && shown.has(day) ? "bg-primary/12 font-medium" : "hover:bg-base-content/10"} ${
                        inMonth ? "" : "text-text-3"
                      }`
                }`}
                onClick={() => onPickDay(day)}
              >
                {new Date(day).getDate()}
                {busy && day !== today && <span className="absolute bottom-0.5 size-[3px] rounded-full bg-text-2" />}
              </button>
            );
          })}
        </div>
      ))}
    </div>
  );
}

/** Month view's picker: a year of months, this one filled and the one shown tinted. */
function MonthPicker({ anchor, now, onPick }: { anchor: number; now: number; onPick: (month: number) => void }) {
  const [year, setYear] = useState(() => new Date(anchor).getFullYear());
  useEffect(() => setYear(new Date(anchor).getFullYear()), [anchor]);
  const shown = startOfMonth(anchor);
  const current = startOfMonth(now);
  const months = Array.from({ length: 12 }, (_, i) => new Date(year, i, 1).getTime());

  return (
    <div>
      <PanelNav title={yearFormat.format(months[0])} unit="year" onStep={(n) => setYear((y) => y + n)} />
      <div className="grid grid-cols-4 gap-1 text-xs">
        {months.map((m) => (
          <button
            key={m}
            className={`h-8 rounded-lg ${
              m === current
                ? "bg-primary font-semibold text-primary-content"
                : m === shown
                  ? "bg-primary/12 font-medium"
                  : "hover:bg-base-content/10"
            }`}
            aria-current={m === shown ? "date" : undefined}
            onClick={() => onPick(m)}
          >
            {shortMonth.format(m)}
          </button>
        ))}
      </div>
    </div>
  );
}
