import { useLayoutEffect, useRef, useState, type CSSProperties } from "react";
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
  onDay,
  sameDay,
  startOfDay,
  tint,
} from "../lib/calendar";

/** Hours the day grid fits in its height; the rest scrolls. */
const VISIBLE_HOURS = 12;
const GUTTER = 67;
/** An all-day bar and the gap under it. */
const LANE = 18;
const NOW_COLOR = "#f5c400";
const NOW_PILL = "#ff8a00";
const OFF_GRAY = "#8a8f98";

export type OpenEvent = (event: CalendarEvent, anchor: DOMRect) => void;

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

/** Day and Week: the all-day row over a scrolling 24-hour grid. */
export function WeekGrid({ days, events, calendars, now, selected, onOpen, onPickDay }: GridProps) {
  const scroller = useRef<HTMLDivElement>(null);
  const [hour, setHour] = useState(48);
  const scrolled = useRef(false);

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
        <div className="relative min-w-0 flex-1 overflow-y-auto [scrollbar-width:none]" style={{ maxHeight: LANE * 4.5 + 4 }}>
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
                  }}
                  onClick={(ev) => onOpen(e, ev.currentTarget.getBoundingClientRect())}
                >
                  {single && !isOff(e) && e.response !== "none" && (
                    <span className="size-1.5 shrink-0 rounded-full" style={{ background: color }} />
                  )}
                  <span className="truncate">{e.title || "No Title"}</span>
                </button>
              );
            })}
          </div>
        </div>
      </div>

      <OverlayScroll viewportRef={scroller} className="min-h-0 flex-1">
        <div className="relative flex" style={{ height: hour * 24 }}>
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
              onOpen={onOpen}
            />
          ))}
        </div>
      </OverlayScroll>
    </div>
  );
}

function DayColumn({
  day,
  events,
  calendars,
  hour,
  now,
  selected,
  onOpen,
}: {
  day: number;
  events: CalendarEvent[];
  calendars: ReadonlyMap<number, CalendarInfo>;
  hour: number;
  now: number;
  selected?: number;
  onOpen: OpenEvent;
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
        return (
          <button
            key={e.id}
            className={`absolute overflow-hidden rounded text-left text-[11px] leading-[14px] ${
              selected === e.id ? "z-10 ring-2 ring-primary" : "hover:z-10"
            }`}
            style={{
              top: (top / 60) * hour + 1,
              height,
              left: `calc(${(column / columns) * 100}% + 2px)`,
              width: `calc(${(span / columns) * 100}% - 4px)`,
              ...eventStyle(e, color, isPast(e, now)),
            }}
            onClick={(ev) => onOpen(e, ev.currentTarget.getBoundingClientRect())}
          >
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

/** Month: whole weeks; each day lists what fits, then "N more", which opens the day. */
export function MonthGrid({ days, month, events, calendars, now, selected, onOpen, onPickDay }: GridProps & { month: number }) {
  const grid = useRef<HTMLDivElement>(null);
  const [size, setSize] = useState({ width: 1000, height: 600 });
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
              }`}
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
                    style={plain ? undefined : bar && !isOff(e) ? { background: tint(color, 30) } : eventStyle(e, color, isPast(e, now), false)}
                    onClick={(ev) => onOpen(e, ev.currentTarget.getBoundingClientRect())}
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
