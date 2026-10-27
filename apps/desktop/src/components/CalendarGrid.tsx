import { useLayoutEffect, useRef, useState, type CSSProperties, type MouseEvent, type PointerEvent as ReactPointerEvent } from "react";
import type { CalendarEvent, CalendarInfo } from "../types";
import OverlayScroll from "./OverlayScroll";
import {
  MINUTES_PER_DAY,
  addDays,
  compareEvents,
  eventDays,
  eventTime,
  formatTime,
  inAllDayRow,
  isOff,
  isPast,
  laneLayout,
  layoutDay,
  localDate,
  onDay,
  sameDay,
  startOfDay,
  tint,
} from "../lib/calendar";
import {
  atMinutes,
  dayOf,
  gestureOutcome,
  minutesOf,
  shiftDays,
  type DragOutcome,
  type GridGesture,
  type GridPoint,
} from "../lib/eventDraft";

/** Hours the day grid fits in its height; the rest scrolls. */
const VISIBLE_HOURS = 12;
const GUTTER = 67;
/** An all-day bar and the gap under it. */
const LANE = 18;
const NOW_COLOR = "#f5c400";
const NOW_PILL = "#ff8a00";
const OFF_GRAY = "#8a8f98";

export type OpenEvent = (event: CalendarEvent, anchor: DOMRect) => void;

/** What a right-click was on: an event, or empty time at `at` (all day when `allDay`). */
export interface GridMenuTarget {
  event?: CalendarEvent;
  at: number;
  allDay: boolean;
}

interface GridProps {
  /** Local midnights, one per column. */
  days: number[];
  events: CalendarEvent[];
  calendars: ReadonlyMap<number, CalendarInfo>;
  now: number;
  /** The event whose preview is open. */
  selected?: number;
  onOpen: OpenEvent;
  /** A date was clicked: show that day. */
  onPickDay: (day: number) => void;
  /** The user can move and resize it: their own. */
  canEdit: (e: CalendarEvent) => boolean;
  /** Empty time dragged across or double-clicked (`end` null: the default length); `anchor` is where it shows. */
  onCreate: (start: number, end: number | null, allDay: boolean, anchor: DOMRect) => void;
  /** An event dropped somewhere else, or an edge dragged (`end` null: the default length). */
  onChange: (e: CalendarEvent, to: DragOutcome) => void;
  onMenu: (target: GridMenuTarget, x: number, y: number) => void;
}

/** How far the pointer goes before a press is a drag. */
const DRAG_PX = 4;
const MINUTE = 60_000;

/**
 * Follows the pointer from a press until it's let go: `move` with each position once it's gone
 * further than a click would, then `drop` (or `cancel`, on Esc). Returns whether it became a drag,
 * through `dragged` once it ends.
 */
function follow<T>(
  ev: ReactPointerEvent,
  at: (x: number, y: number) => T,
  move: (p: T) => void,
  drop: (p: T) => void,
  cancel: () => void,
) {
  const x0 = ev.clientX;
  const y0 = ev.clientY;
  let last: T | null = null;
  const onMove = (e: PointerEvent) => {
    if (last === null && Math.hypot(e.clientX - x0, e.clientY - y0) < DRAG_PX) return;
    last = at(e.clientX, e.clientY);
    move(last);
  };
  const stop = () => {
    window.removeEventListener("pointermove", onMove);
    window.removeEventListener("pointerup", onUp);
    window.removeEventListener("keydown", onKey, true);
  };
  const onUp = () => {
    stop();
    if (last !== null) drop(last);
  };
  const onKey = (e: KeyboardEvent) => {
    if (e.key !== "Escape") return;
    e.stopPropagation();
    e.preventDefault();
    stop();
    if (last !== null) cancel();
  };
  window.addEventListener("pointermove", onMove);
  window.addEventListener("pointerup", onUp);
  window.addEventListener("keydown", onKey, true);
}

const colorOf = (calendars: ReadonlyMap<number, CalendarInfo>, e: CalendarEvent) =>
  calendars.get(e.calendar)?.color ?? "#3d8bff";

/** How an event is drawn: tinted with a bar; outlined until answered; dashed if tentative; hatched and struck through if off. */
function eventStyle(e: CalendarEvent, color: string, past: boolean, bar = true): CSSProperties {
  if (isOff(e)) {
    return {
      background: `repeating-linear-gradient(135deg, ${tint(OFF_GRAY, 16)} 0 3px, transparent 3px 7px)`,
      borderLeft: bar ? `2px solid ${tint(OFF_GRAY, 60)}` : undefined,
      color: "var(--spark-text-2)",
      textDecoration: "line-through",
    };
  }
  if (e.response === "none") {
    return { background: "var(--color-base-100)", boxShadow: `inset 0 0 0 1px ${color}`, color, opacity: past ? 0.7 : 1 };
  }
  return {
    background: tint(color, past ? 13 : 25),
    borderLeft: bar ? `2px ${e.response === "tentative" ? "dashed" : "solid"} ${color}` : undefined,
  };
}

const weekdayFormat = new Intl.DateTimeFormat(undefined, { weekday: "short" });
const hourFormat = new Intl.DateTimeFormat(undefined, { hour: "numeric" });
const twelveHour = new Intl.DateTimeFormat(undefined, { hour: "numeric" }).resolvedOptions().hourCycle?.startsWith("h1");

function hourLabel(hour: number) {
  if (hour === 12 && twelveHour) return "Midday";
  const d = new Date(2000, 0, 1, hour);
  return hourFormat.format(d);
}

/**
 * Day and Week: the all-day row over a scrolling 24-hour grid. Dragging across empty time makes an
 * event; the user's own events move and resize by dragging, to the quarter hour, and in and out of
 * the all-day row; double-click makes an event there; right-click asks `onMenu`.
 */
export function WeekGrid({
  days,
  events,
  calendars,
  now,
  selected,
  onOpen,
  onPickDay,
  canEdit,
  onCreate,
  onChange,
  onMenu,
}: GridProps) {
  const scroller = useRef<HTMLDivElement>(null);
  const timeline = useRef<HTMLDivElement>(null);
  const allDayBox = useRef<HTMLDivElement>(null);
  const ghost = useRef<HTMLDivElement>(null);
  const [hour, setHour] = useState(48);
  const scrolled = useRef(false);
  const [drag, setDrag] = useState<{ g: GridGesture; p: GridPoint } | null>(null);
  /** A drag just ended: the click it makes doesn't open what it was on. */
  const justDragged = useRef(false);

  useLayoutEffect(() => {
    const el = scroller.current;
    if (!el) return;
    const measure = () => {
      const height = Math.max(32, Math.floor(el.clientHeight / VISIBLE_HOURS));
      setHour(height);
      // Opens on the hours around now, three of them before it.
      if (!scrolled.current) {
        scrolled.current = true;
        const minutes = (Date.now() - startOfDay(Date.now())) / 60_000;
        el.scrollTop = Math.max(0, (minutes / 60 - 3) * height);
      }
    };
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(el);
    return () => observer.disconnect();
  }, []);

  const n = days.length;
  const today = startOfDay(now);
  const nowMinutes = (now - today) / 60_000;
  // Events come with a day either side of the view: keep only those on the days shown.
  const allDay = events
    .filter((e) => {
      if (!inAllDayRow(e)) return false;
      const { first, last } = eventDays(e);
      return first <= days[n - 1] && last >= days[0];
    })
    .sort(compareEvents);
  const spans = allDay.map((e) => {
    const { first, last } = eventDays(e);
    const from = Math.max(0, days.findIndex((d) => d >= first));
    const to = days.filter((d) => d <= last).length - 1;
    return { from, to: Math.max(from, to) };
  });
  const lanes = laneLayout(spans);
  const laneCount = lanes.length ? Math.max(...lanes) + 1 : 0;

  /** Where `x`, `y` is: the column, the minutes into its day, and whether it's over the all-day row. */
  const pointAt = (x: number, y: number): GridPoint => {
    const t = timeline.current!.getBoundingClientRect();
    const a = allDayBox.current?.getBoundingClientRect();
    const allDay = a !== undefined && y <= a.bottom;
    const left = allDay ? a.left : t.left + GUTTER;
    const width = allDay ? a.width : t.width - GUTTER;
    const col = Math.max(0, Math.min(n - 1, Math.floor(((x - left) / width) * n)));
    const minutes = Math.max(0, Math.min(MINUTES_PER_DAY - 1, ((y - t.top) / hour) * 60));
    return { col, minutes, allDay };
  };

  const press = (ev: ReactPointerEvent, gesture: (p: GridPoint) => GridGesture) => {
    if (ev.button !== 0) return;
    ev.stopPropagation();
    const g = gesture(pointAt(ev.clientX, ev.clientY));
    follow(
      ev,
      pointAt,
      (p) => setDrag({ g, p }),
      (p) => {
        justDragged.current = true;
        window.setTimeout(() => (justDragged.current = false));
        const anchor = ghost.current?.getBoundingClientRect() ?? new DOMRect(ev.clientX, ev.clientY, 1, 1);
        setDrag(null);
        const to = gestureOutcome(g, p, days);
        if (!to) return;
        if (g.kind === "create") onCreate(to.start, to.end, false, anchor);
        else onChange(g.event, to);
      },
      () => setDrag(null),
    );
  };
  const pressEvent = (ev: ReactPointerEvent, e: CalendarEvent, edge?: "start" | "end") => {
    if (!canEdit(e)) return;
    press(ev, (p) =>
      edge
        ? { kind: "resize", event: e, edge }
        : inAllDayRow(e)
          ? { kind: "row", event: e, from: p }
          : { kind: "move", event: e, grab: p.minutes - (e.start - days[p.col]) / MINUTE },
    );
  };
  const open = (ev: MouseEvent<HTMLElement>, e: CalendarEvent) => {
    if (justDragged.current) return;
    onOpen(e, ev.currentTarget.getBoundingClientRect());
  };
  const menu = (ev: MouseEvent, e?: CalendarEvent) => {
    ev.preventDefault();
    const p = pointAt(ev.clientX, ev.clientY);
    const at = p.allDay ? Date.UTC(...ymd(days[p.col])) : atMinutes(days[p.col], Math.floor(p.minutes / 15) * 15);
    onMenu({ event: e, at, allDay: p.allDay }, ev.clientX, ev.clientY);
  };
  /** Double-click on empty time: a new event of the default length, or all day in the all-day row. */
  const doubleClick = (ev: MouseEvent) => {
    if ((ev.target as Element).closest("[data-event], button")) return;
    const p = pointAt(ev.clientX, ev.clientY);
    const t = timeline.current!.getBoundingClientRect();
    const width = (t.width - GUTTER) / n;
    if (p.allDay) {
      onCreate(Date.UTC(...ymd(days[p.col])), null, true, new DOMRect(t.left + GUTTER + p.col * width, ev.clientY - 8, width, 16));
      return;
    }
    const minutes = Math.floor(p.minutes / 15) * 15;
    const top = t.top + (minutes / 60) * hour;
    onCreate(atMinutes(days[p.col], minutes), null, false, new DOMRect(t.left + GUTTER + p.col * width, top, width, hour));
  };

  // What the drag would do now: a ghost where it'd land, the event it moves faded.
  const outcome = drag ? gestureOutcome(drag.g, drag.p, days) : null;
  const moving = drag && drag.g.kind !== "create" ? drag.g.event : undefined;
  const ghostColor = moving ? colorOf(calendars, moving) : "#3d8bff";
  const ghostTitle = moving ? moving.title || "No Title" : "No Title";
  const timedGhost = outcome && !outcome.allDay ? { ...outcome, end: outcome.end ?? outcome.start + 60 * MINUTE } : null;
  const ghostCol = timedGhost ? days.indexOf(dayOf(timedGhost.start)) : -1;
  const rowGhost =
    outcome && outcome.allDay
      ? (() => {
          const first = localDate(outcome.start);
          const last = localDate(Math.max(outcome.start, (outcome.end ?? outcome.start) - 1));
          const from = days.findIndex((d) => d >= first);
          const to = days.filter((d) => d <= last).length - 1;
          return from < 0 || to < from ? null : { from, to };
        })()
      : null;

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      {/* Day names, then the all-day row. */}
      <div className="flex shrink-0 border-b border-hairline">
        <div style={{ width: GUTTER }} className="shrink-0" />
        {days.map((day) => {
          const isToday = day === today;
          return (
            <div key={day} className="flex min-w-0 flex-1 items-center justify-center gap-1.5 py-2 text-[13px]">
              <span className={isToday ? "font-semibold text-primary" : "text-text-2"}>{weekdayFormat.format(day)}</span>
              <button
                className={`grid size-[26px] place-items-center rounded-full text-[13px] font-semibold ${
                  isToday ? "bg-primary text-primary-content" : "hover:bg-base-content/10"
                }`}
                data-tip={n > 1 ? "Show this day" : undefined}
                onClick={() => onPickDay(day)}
              >
                {new Date(day).getDate()}
              </button>
            </div>
          );
        })}
      </div>
      <div className="flex shrink-0 border-b border-hairline">
        <div style={{ width: GUTTER }} className="shrink-0 pr-2 pt-1 text-right text-[10px] text-text-2">
          all-day
        </div>
        <div
          ref={allDayBox}
          className="relative min-w-0 flex-1 overflow-y-auto [scrollbar-width:none]"
          style={{ maxHeight: LANE * 4.5 + 4 }}
          onDoubleClick={doubleClick}
          onContextMenu={(ev) => menu(ev)}
        >
          <div className="relative" style={{ height: Math.max(1, laneCount) * LANE + 4 }}>
            {allDay.map((e, i) => {
              const { from, to } = spans[i];
              const color = colorOf(calendars, e);
              const single = from === to && (e.allDay || e.end - e.start < 2 * 86_400_000);
              return (
                <button
                  key={e.id}
                  className={`absolute flex h-4 items-center gap-1 overflow-hidden rounded px-1.5 text-left text-[11px] font-medium leading-4 ${
                    selected === e.id ? "ring-2 ring-primary" : ""
                  }`}
                  style={{
                    top: 2 + lanes[i] * LANE,
                    left: `calc(${(from / n) * 100}% + 2px)`,
                    width: `calc(${((to - from + 1) / n) * 100}% - 5px)`,
                    ...(single && !isOff(e) && e.response !== "none" ? {} : eventStyle(e, color, isPast(e, now), false)),
                    ...(!single && !isOff(e) && e.response !== "none" ? { background: tint(color, 30) } : {}),
                    ...(moving?.id === e.id && outcome ? { opacity: 0.35 } : {}),
                  }}
                  data-event={e.id}
                  onPointerDown={(ev) => pressEvent(ev, e)}
                  onClick={(ev) => open(ev, e)}
                  onContextMenu={(ev) => {
                    ev.stopPropagation();
                    menu(ev, e);
                  }}
                >
                  {single && !isOff(e) && e.response !== "none" && (
                    <span className="size-1.5 shrink-0 rounded-full" style={{ background: color }} />
                  )}
                  <span className="truncate">{e.title || "No Title"}</span>
                </button>
              );
            })}
            {rowGhost && (
              <div
                ref={ghost}
                className="pointer-events-none absolute z-10 flex h-4 items-center overflow-hidden rounded px-1.5 text-[11px] font-medium leading-4 shadow-md"
                style={{
                  top: 2,
                  left: `calc(${(rowGhost.from / n) * 100}% + 2px)`,
                  width: `calc(${((rowGhost.to - rowGhost.from + 1) / n) * 100}% - 5px)`,
                  background: tint(ghostColor, 45),
                }}
              >
                <span className="truncate">{ghostTitle}</span>
              </div>
            )}
          </div>
        </div>
      </div>

      <OverlayScroll viewportRef={scroller} className="min-h-0 flex-1">
        <div
          ref={timeline}
          className="relative flex"
          style={{ height: hour * 24 }}
          onPointerDown={(ev) => {
            if ((ev.target as Element).closest("[data-event]") || ev.clientX < timeline.current!.getBoundingClientRect().left + GUTTER) return;
            press(ev, (p) => ({ kind: "create", from: p }));
          }}
          onDoubleClick={doubleClick}
          onContextMenu={(ev) => menu(ev)}
        >
          {/* Hour labels, and now in an orange pill. */}
          <div style={{ width: GUTTER }} className="relative shrink-0 select-none">
            {Array.from({ length: 23 }, (_, i) => i + 1).map((h) => (
              <span
                key={h}
                className="absolute right-2 -translate-y-1/2 text-[10px] text-text-2"
                style={{ top: h * hour, visibility: Math.abs(h * 60 - nowMinutes) < 15 ? "hidden" : undefined }}
              >
                {hourLabel(h)}
              </span>
            ))}
            <span
              className="absolute right-1 z-10 grid h-[21px] w-[54px] -translate-y-1/2 place-items-center rounded-md text-[11px] font-semibold tabular-nums text-white"
              style={{ top: (nowMinutes / 60) * hour, background: NOW_PILL }}
            >
              {formatTime(now)}
            </span>
          </div>
          {days.map((day) => (
            <DayColumn
              key={day}
              day={day}
              events={onDay(events, day).filter((e) => !inAllDayRow(e))}
              calendars={calendars}
              hour={hour}
              now={now}
              selected={selected}
              faded={moving && outcome ? moving.id : undefined}
              canEdit={canEdit}
              onPress={pressEvent}
              onOpen={open}
              onMenu={menu}
            />
          ))}
          {timedGhost && ghostCol >= 0 && (
            <div
              ref={ghost}
              className="pointer-events-none absolute z-30 overflow-hidden rounded px-1.5 py-0.5 text-[11px] leading-[14px] shadow-md"
              style={{
                top: (minutesOf(timedGhost.start) / 60) * hour + 1,
                height: Math.max(15, ((Math.min(timedGhost.end, addDays(days[ghostCol], 1)) - timedGhost.start) / 3_600_000) * hour - 2),
                left: `calc(${GUTTER}px + (100% - ${GUTTER}px) * ${ghostCol / n} + 2px)`,
                width: `calc((100% - ${GUTTER}px) / ${n} - 4px)`,
                background: tint(ghostColor, 45),
                borderLeft: `2px solid ${ghostColor}`,
              }}
            >
              <span className="block truncate tabular-nums opacity-80">
                {formatTime(timedGhost.start)} – {formatTime(timedGhost.end)}
              </span>
              <span className="line-clamp-2 font-semibold">{ghostTitle}</span>
            </div>
          )}
        </div>
      </OverlayScroll>
    </div>
  );
}

/** The year, month and day of a local midnight, for `Date.UTC`: an all-day event's date. */
function ymd(day: number): [number, number, number] {
  const d = new Date(day);
  return [d.getFullYear(), d.getMonth(), d.getDate()];
}

function DayColumn({
  day,
  events,
  calendars,
  hour,
  now,
  selected,
  faded,
  canEdit,
  onPress,
  onOpen,
  onMenu,
}: {
  day: number;
  events: CalendarEvent[];
  calendars: ReadonlyMap<number, CalendarInfo>;
  hour: number;
  now: number;
  selected?: number;
  /** The event being dragged elsewhere. */
  faded?: number;
  canEdit: (e: CalendarEvent) => boolean;
  /** Pressed on an event, or on one of its edges. */
  onPress: (ev: ReactPointerEvent, e: CalendarEvent, edge?: "start" | "end") => void;
  onOpen: (ev: MouseEvent<HTMLElement>, e: CalendarEvent) => void;
  onMenu: (ev: MouseEvent, e?: CalendarEvent) => void;
}) {
  const weekday = new Date(day).getDay();
  const isToday = sameDay(day, now);
  const next = addDays(day, 1);
  const placed = layoutDay(
    events.map((e) => ({
      item: e,
      start: Math.max(0, (e.start - day) / 60_000),
      end: Math.min(MINUTES_PER_DAY, (Math.min(e.end, next) - day) / 60_000),
    })),
    (15 / hour) * 60,
  );
  const nowMinutes = (now - startOfDay(now)) / 60_000;
  return (
    <div
      className={`relative min-w-0 flex-1 border-l border-hairline ${
        isToday ? "bg-primary/[0.035]" : weekday === 0 || weekday === 6 ? "bg-base-content/[0.025]" : ""
      }`}
      style={{
        backgroundImage: "linear-gradient(to bottom, var(--spark-hairline) 1px, transparent 1px)",
        backgroundSize: `100% ${hour}px`,
      }}
    >
      {placed.map(({ item: e, top, bottom, column, span, columns }) => {
        const color = colorOf(calendars, e);
        const height = Math.max(15, ((bottom - top) / 60) * hour - 2);
        const edges = canEdit(e) && height >= 20;
        return (
          <button
            key={e.id}
            className={`group absolute overflow-hidden rounded text-left text-[11px] leading-[14px] ${
              selected === e.id ? "z-10 ring-2 ring-primary" : "hover:z-10"
            }`}
            style={{
              top: (top / 60) * hour + 1,
              height,
              left: `calc(${(column / columns) * 100}% + 2px)`,
              width: `calc(${(span / columns) * 100}% - 4px)`,
              ...eventStyle(e, color, isPast(e, now)),
              ...(faded === e.id ? { opacity: 0.35 } : {}),
            }}
            data-event={e.id}
            onPointerDown={(ev) => onPress(ev, e)}
            onClick={(ev) => onOpen(ev, e)}
            onContextMenu={(ev) => {
              ev.stopPropagation();
              onMenu(ev, e);
            }}
          >
            {edges && (
              <>
                <span
                  aria-hidden
                  className="absolute inset-x-0 top-0 z-10 h-[5px] cursor-ns-resize"
                  onPointerDown={(ev) => onPress(ev, e, "start")}
                />
                <span
                  aria-hidden
                  className="absolute inset-x-0 bottom-0 z-10 h-[5px] cursor-ns-resize"
                  onPointerDown={(ev) => onPress(ev, e, "end")}
                />
              </>
            )}
            {height >= 38 ? (
              <span className="block px-1.5 py-0.5">
                <span className="block truncate tabular-nums opacity-80">{eventTime(e)}</span>
                <span className="line-clamp-2 font-semibold">{e.title || "No Title"}</span>
              </span>
            ) : (
              <span className={`block truncate px-1.5 ${height < 18 ? "text-[10px] leading-[13px]" : "py-px"}`}>
                <span className="font-semibold">{e.title || "No Title"}</span>
                {height >= 18 && <span className="opacity-80">, {formatTime(e.start)}</span>}
              </span>
            )}
          </button>
        );
      })}
      {/* Now: a yellow line, full on today. */}
      <div
        className="pointer-events-none absolute inset-x-0 z-20 h-px"
        style={{ top: (nowMinutes / 60) * hour, background: NOW_COLOR, opacity: isToday ? 1 : 0.3 }}
      >
        {isToday && <span className="absolute -left-[5px] -top-[4.5px] size-2.5 rounded-full" style={{ background: NOW_PILL }} />}
      </div>
    </div>
  );
}

const monthDayFormat = new Intl.DateTimeFormat(undefined, { month: "short", day: "numeric" });
const ROW = 18;

/**
 * Month: whole weeks; each day lists what fits, then "N more", which opens the day. The user's own
 * events move to another day by dragging, keeping their times; double-click on a day makes an event.
 */
export function MonthGrid({
  days,
  month,
  events,
  calendars,
  now,
  selected,
  onOpen,
  onPickDay,
  canEdit,
  onCreate,
  onChange,
  onMenu,
}: GridProps & { month: number }) {
  const grid = useRef<HTMLDivElement>(null);
  const [size, setSize] = useState({ width: 1000, height: 600 });
  /** An event being dragged, and the day (its index) it'd land on. */
  const [drag, setDrag] = useState<{ event: CalendarEvent; to: number } | null>(null);
  const justDragged = useRef(false);
  useLayoutEffect(() => {
    const el = grid.current;
    if (!el) return;
    const measure = () => setSize({ width: el.clientWidth, height: el.clientHeight });
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(el);
    return () => observer.disconnect();
  }, []);
  const weeks = days.length / 7;
  const perCell = Math.max(0, Math.floor((size.height / weeks - 32) / ROW));
  const today = startOfDay(now);
  const showTimes = size.width > 700;

  /** The day (its index) under `x`, `y`. */
  const cellAt = (x: number, y: number) => {
    const r = grid.current!.getBoundingClientRect();
    const col = Math.max(0, Math.min(6, Math.floor(((x - r.left) / r.width) * 7)));
    const row = Math.max(0, Math.min(weeks - 1, Math.floor(((y - r.top) / r.height) * weeks)));
    return row * 7 + col;
  };
  const press = (ev: ReactPointerEvent, e: CalendarEvent) => {
    if (ev.button !== 0 || !canEdit(e)) return;
    const from = cellAt(ev.clientX, ev.clientY);
    follow(
      ev,
      cellAt,
      (to) => setDrag({ event: e, to }),
      (to) => {
        justDragged.current = true;
        window.setTimeout(() => (justDragged.current = false));
        setDrag(null);
        if (to !== from) onChange(e, { ...shiftDays(e, Math.round((days[to] - days[from]) / 86_400_000)), allDay: e.allDay });
      },
      () => setDrag(null),
    );
  };
  /** A new event in a day: at 9:00, the default length. */
  const create = (ev: MouseEvent, day: number) => {
    if ((ev.target as Element).closest("[data-event], button")) return;
    onCreate(atMinutes(day, 9 * 60), null, false, (ev.currentTarget as HTMLElement).getBoundingClientRect());
  };

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <div className="grid shrink-0 grid-cols-7 border-b border-hairline text-[13px] text-text-2">
        {days.slice(0, 7).map((d) => (
          <div key={d} className="px-2 py-2.5 text-right">
            {weekdayFormat.format(d)}
          </div>
        ))}
      </div>
      <div ref={grid} className="grid min-h-0 flex-1 grid-cols-7" style={{ gridTemplateRows: `repeat(${weeks}, minmax(0, 1fr))` }}>
        {days.map((day) => {
          const list = onDay(events, day).sort(compareEvents);
          const fits = list.length > perCell ? Math.max(0, perCell - 1) : list.length;
          const inMonth = new Date(day).getMonth() === month;
          const weekday = new Date(day).getDay();
          return (
            <div
              key={day}
              className={`flex min-h-0 min-w-0 flex-col border-b border-l border-hairline px-1 pb-1 ${
                weekday === 0 || weekday === 6 ? "bg-base-content/[0.025]" : ""
              } ${drag && days[drag.to] === day ? "bg-primary/10 ring-2 ring-inset ring-primary/50" : ""}`}
              onDoubleClick={(ev) => create(ev, day)}
              onContextMenu={(ev) => {
                ev.preventDefault();
                onMenu({ at: atMinutes(day, 9 * 60), allDay: false }, ev.clientX, ev.clientY);
              }}
            >
              <div className="flex h-8 shrink-0 items-center justify-end gap-1.5 pr-0.5">
                {day === today && <span className="text-[11px] font-semibold text-primary">Today</span>}
                <button
                  className={`grid h-[26px] min-w-[26px] place-items-center rounded-full px-1 text-[13px] font-semibold ${
                    day === today
                      ? "bg-primary text-primary-content"
                      : inMonth
                        ? "hover:bg-base-content/10"
                        : "text-text-3 hover:bg-base-content/10"
                  }`}
                  data-tip="Show this day"
                  onClick={() => onPickDay(day)}
                >
                  {new Date(day).getDate() === 1 ? monthDayFormat.format(day) : new Date(day).getDate()}
                </button>
              </div>
              {list.slice(0, fits).map((e) => {
                const color = colorOf(calendars, e);
                const bar = inAllDayRow(e) && eventDays(e).first !== eventDays(e).last;
                const plain = !bar && !isOff(e) && e.response !== "none";
                return (
                  <button
                    key={e.id}
                    className={`mb-0.5 flex h-4 shrink-0 items-center gap-1 overflow-hidden rounded px-1 text-left text-[11px] leading-4 ${
                      selected === e.id ? "ring-2 ring-primary" : plain ? "hover:bg-base-content/5" : ""
                    } ${isPast(e, now) && plain ? "opacity-60" : ""}`}
                    style={{
                      ...(plain ? {} : bar && !isOff(e) ? { background: tint(color, 30) } : eventStyle(e, color, isPast(e, now), false)),
                      ...(drag?.event.id === e.id ? { opacity: 0.35 } : {}),
                    }}
                    data-event={e.id}
                    onPointerDown={(ev) => press(ev, e)}
                    onClick={(ev) => {
                      if (!justDragged.current) onOpen(e, ev.currentTarget.getBoundingClientRect());
                    }}
                    onContextMenu={(ev) => {
                      ev.preventDefault();
                      ev.stopPropagation();
                      onMenu({ event: e, at: e.start, allDay: e.allDay }, ev.clientX, ev.clientY);
                    }}
                  >
                    {plain && <span className="size-1.5 shrink-0 rounded-full" style={{ background: color }} />}
                    <span className="min-w-0 flex-1 truncate font-medium">{e.title || "No Title"}</span>
                    {showTimes && !inAllDayRow(e) && (
                      <span className="shrink-0 tabular-nums text-text-2">{formatTime(e.start)}</span>
                    )}
                  </button>
                );
              })}
              {list.length > fits && (
                <button
                  className="h-4 shrink-0 rounded px-1 text-left text-[11px] font-medium leading-4 text-text-2 hover:bg-base-content/5 hover:text-base-content"
                  onClick={() => onPickDay(day)}
                >
                  {list.length - fits} more {list.length - fits === 1 ? "event" : "events"}
                </button>
              )}
            </div>
          );
        })}
      </div>
    </div>
  );
}
