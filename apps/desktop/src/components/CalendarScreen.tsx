import { ArrowLeft, ChevronDown, ChevronLeft, ChevronRight, LoaderCircle, PanelRight, Video, X } from "lucide-react";
import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState, type ReactNode } from "react";
import {
  answerEvent,
  cancelSignIn,
  listCalendars,
  listEvents,
  openExternal,
  reconnectAccount,
  refreshCalendars,
} from "../api";
import { useAccounts } from "../lib/accounts";
import {
  DAY_MS,
  addDays,
  addMonths,
  countdown,
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
  startOfWeek,
  stepAnchor,
  viewDays,
  type CalendarKind,
} from "../lib/calendar";
import { demoAnswer, demoCalendars, demoListEvents } from "../lib/demoCalendar";
import { isInsideOverlay, isTypingTarget } from "../lib/shortcuts";
import type { AccountId, AccountView, CalendarEvent, CalendarInfo, CalendarList, InviteAnswer } from "../types";
import AccountMark from "./AccountMark";
import { MonthGrid, WeekGrid } from "./CalendarGrid";
import { Popover } from "./Dropdown";
import EventPreview from "./EventPreview";
import { callService } from "./InviteCard";

/** Servers are asked again this often while the calendar is open, and on focus if it's been this long. */
const REFRESH_MS = 5 * 60_000;
const FOCUS_REFRESH_MS = 60_000;
/** Narrower than this, the side panel floats over the grid. */
const SIDE_DOCK_MIN = 925;
const DISMISSED_KEY = "spark.calendarUnsupportedSeen";

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
}

const LIVE: Source = { calendars: listCalendars, events: listEvents, refresh: refreshCalendars, answer: answerEvent };
const DEMO: Source = {
  calendars: () => Promise.resolve(demoCalendars()),
  events: (start, end) => Promise.resolve(demoListEvents(start, end)),
  refresh: () => Promise.resolve(),
  answer: demoAnswer,
};

/**
 * The calendar, Spark's way, in place of the list and the reader: Day, Week
 * or Month of every account's calendars, an event's details beside it, and
 * a side panel with a small month and the next meeting. Read-only apart
 * from answering invitations.
 */
export default function CalendarScreen({
  demo,
  focus,
  onClose,
}: {
  demo: boolean;
  /** A time whose day to show; a new object shows it again. */
  focus?: { at: number };
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

  useEffect(() => {
    if (!focus) return;
    setAnchor(startOfDay(focus.at));
    setPreview(null);
  }, [focus]);

  const changeKind = (next: CalendarKind) => {
    setKind(next);
    saveKind(next);
    setPreview(null);
  };
  const go = (n: number) => {
    setAnchor((a) => stepAnchor(kind, a, n));
    setPreview(null);
  };
  const goToday = () => {
    setAnchor(startOfDay(Date.now()));
    setPreview(null);
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

  // Keys: Esc closes; T today; D/W/M the view; arrows page. Others are kept from the hidden list.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.defaultPrevented || e.ctrlKey || e.metaKey || e.altKey) return;
      if (isTypingTarget(e.target) || isInsideOverlay(e.target) || document.querySelector("[data-composer]")) return;
      const key = e.key.length === 1 ? e.key.toLowerCase() : e.key;
      const handled = () => {
        e.preventDefault();
        e.stopPropagation();
      };
      if (key === "Escape") {
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
        <HeaderButton tip="Close calendar" onClick={onClose}>
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
        <HeaderButton tip={side ? "Hide Side Panel" : "Show Side Panel"} pressed={side} onClick={toggleSide}>
          <PanelRight size={18} />
        </HeaderButton>
      </header>

      <Notices list={list} accounts={accounts} onReconnected={() => void refresh()} />

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
              onOpen={(e, rect) => setPreview({ id: e.id, anchor: rect })}
              onPickDay={pickDay}
            />
          ) : (
            <WeekGrid
              key={kind}
              days={days}
              events={shown}
              calendars={calendars}
              now={now}
              selected={open?.id}
              onOpen={(e, rect) => setPreview({ id: e.id, anchor: rect })}
              onPickDay={pickDay}
            />
          )}
        </div>
        {side && (
          <SidePanel
            docked={dockSide}
            anchor={anchor}
            days={days}
            events={shown}
            now={now}
            onPickDay={(day) => {
              setAnchor(day);
              setPreview(null);
              if (!dockSide) toggleSide();
            }}
            onOpen={(e, rect) => setPreview({ id: e.id, anchor: rect })}
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
          onClose={() => setPreview(null)}
        />
      )}
    </section>
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

/** A small month to move around in, and the next meeting. Floats over the grid when the window is narrow. */
function SidePanel({
  docked,
  anchor,
  days,
  events,
  now,
  onPickDay,
  onOpen,
}: {
  docked: boolean;
  anchor: number;
  days: number[];
  events: CalendarEvent[];
  now: number;
  onPickDay: (day: number) => void;
  onOpen: (e: CalendarEvent, anchor: DOMRect) => void;
}) {
  const [month, setMonth] = useState(() => startOfMonth(anchor));
  useEffect(() => setMonth(startOfMonth(anchor)), [anchor]);
  const grid = viewDays("month", month);
  const today = startOfDay(now);
  const shown = new Set(days);
  const next = nextMeeting(events, now);

  return (
    <aside
      aria-label="Side panel"
      className={`flex w-[300px] shrink-0 flex-col gap-4 overflow-y-auto border-l border-hairline bg-base-100 p-4 ${
        docked ? "" : "absolute inset-y-0 right-0 z-30 shadow-popup"
      }`}
    >
      <div>
        <div className="mb-2 flex items-center gap-1">
          <h2 className="min-w-0 flex-1 truncate pl-1 text-[13px] font-semibold">{monthYear.format(month)}</h2>
          <button
            className="grid size-6 place-items-center rounded text-text-2 hover:bg-base-content/10"
            aria-label="Previous month"
            onClick={() => setMonth((m) => addMonths(m, -1))}
          >
            <ChevronLeft size={15} />
          </button>
          <button
            className="grid size-6 place-items-center rounded text-text-2 hover:bg-base-content/10"
            aria-label="Next month"
            onClick={() => setMonth((m) => addMonths(m, 1))}
          >
            <ChevronRight size={15} />
          </button>
        </div>
        <div className="grid grid-cols-7 text-center text-[11px]">
          {Array.from({ length: 7 }, (_, i) => addDays(startOfWeek(month), i)).map((d) => (
            <span key={d} className="pb-1 text-text-2">
              {weekdayInitial.format(d)}
            </span>
          ))}
          {grid.map((day) => {
            const inMonth = new Date(day).getMonth() === new Date(month).getMonth();
            const busy = onDay(events, day).length > 0;
            return (
              <button
                key={day}
                className={`relative mx-auto my-px grid size-7 place-items-center rounded-full text-xs ${
                  day === today
                    ? "bg-primary font-semibold text-primary-content"
                    : shown.has(day)
                      ? "bg-primary/12 font-medium"
                      : inMonth
                        ? "hover:bg-base-content/10"
                        : "text-text-3 hover:bg-base-content/10"
                }`}
                onClick={() => onPickDay(day)}
              >
                {new Date(day).getDate()}
                {busy && day !== today && <span className="absolute bottom-0.5 size-[3px] rounded-full bg-text-2" />}
              </button>
            );
          })}
        </div>
      </div>

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
