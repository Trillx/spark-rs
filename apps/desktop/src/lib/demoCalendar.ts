import type { CalendarEvent, CalendarInfo, CalendarList, EventDraft, EventScope, InviteAnswer, InvitePerson, Repeat } from "../types";
import { DAY_MS, addDays, startOfDay, utcDate } from "./calendar";
import { atMinutes, dateValue, fromUtcDate, minutesOf, toUtcDate } from "./eventDraft";

// The demo accounts' calendars. They live here rather than on the Rust side
// because only the UI knows the local time zone the events are placed in:
// a 9:30 standup is at 9:30 wherever the demo runs.

const PERSONAL = 1;
const WORK = 2;

const CALENDARS: CalendarInfo[] = [
  { id: 1, account: WORK, name: "Calendar", color: "#3d8bff", isDefault: true, own: true },
  { id: 2, account: WORK, name: "Team events", color: "#3ccb6e", isDefault: false, own: true },
  { id: 3, account: PERSONAL, name: "Personal", color: "#ff9f2e", isDefault: true, own: true },
];

const ME_WORK: InvitePerson = { name: "Me", address: "me@outlook.com" };
const PRIYA: InvitePerson = { name: "Priya Raman", address: "priya@acme.example", answer: "accepted" };
const SAM: InvitePerson = { name: "Sam Okafor", address: "sam.okafor@acme.example", answer: "accepted" };
const HANNAH: InvitePerson = { name: "Hannah Cole", address: "hannah@acme.example", answer: "tentative" };
const ALEX: InvitePerson = { name: "Alex Moreno", address: "alex@moreno-design.example" };
const KAI: InvitePerson = { name: "Kai Tanaka", address: "kai@acme.example", answer: "declined" };
const MARCUS: InvitePerson = { name: "Marcus Lee", address: "marcus.lee@example.com", answer: "accepted" };

const TEAMS = "https://teams.microsoft.com/l/meetup-join/demo";
const MEET = "https://meet.google.com/abc-defg-hij";

/** Answers given in the demo, by event id. */
const answers = new Map<number, InviteAnswer>();
/** Events made, changed and deleted in the demo. */
const made: CalendarEvent[] = [];
/** How the demo's series repeat, by series. */
const rules = new Map<string, Repeat>([
  ["standup", { every: "week", interval: 1, weekdays: [1, 2, 3, 4, 5], ends: { kind: "never" } }],
  ["planning", { every: "week", interval: 1, weekdays: [1], ends: { kind: "never" } }],
]);
const changes = new Map<number, Partial<CalendarEvent>>();
const deleted = new Set<number>();

type Spec = Omit<
  CalendarEvent,
  "id" | "calendar" | "account" | "start" | "end" | "allDay" | "attendees" | "response" | "canceled" | "repeats" | "showAs" | "private" | "onlineMeeting"
> &
  Partial<Pick<CalendarEvent, "attendees" | "response" | "canceled" | "repeats" | "showAs" | "private" | "onlineMeeting">>;

/** The demo events of the weeks around today. */
function demoEvents(now: number): CalendarEvent[] {
  const today = startOfDay(now);
  const events: CalendarEvent[] = [];
  const add = (calendar: number, start: number, end: number, allDay: boolean, spec: Spec) => {
    const id = events.length + 1;
    const response = answers.get(id) ?? spec.response ?? "accepted";
    const me = { ...ME_WORK, answer: response === "none" || response === "organizer" ? undefined : response };
    const attendees = (spec.attendees ?? []).map((p) => (p === ME_WORK ? me : p));
    events.push({
      ...spec,
      // An organizer is going unless they say otherwise.
      organizer: spec.organizer && { ...spec.organizer, answer: spec.organizer.answer ?? "accepted" },
      id,
      calendar,
      account: CALENDARS.find((c) => c.id === calendar)!.account,
      start,
      end,
      allDay,
      attendees,
      response,
      canceled: spec.canceled ?? false,
      repeats: spec.repeats ?? false,
      showAs: spec.showAs ?? (response === "tentative" ? "tentative" : "busy"),
      private: spec.private ?? false,
      onlineMeeting: spec.onlineMeeting ?? spec.callLink === TEAMS,
      // Outlook's default reminder, unless the event sets its own.
      reminder: "reminder" in spec ? spec.reminder : allDay ? undefined : 15,
    });
  };
  /** `day` days from today at `hh:mm` local time, lasting `minutes`. */
  const at = (day: number, time: string, minutes: number): [number, number] => {
    const [h, m] = time.split(":").map(Number);
    const d = new Date(addDays(today, day));
    d.setHours(h, m, 0, 0);
    return [d.getTime(), d.getTime() + minutes * 60_000];
  };
  /** All day from `day` for `days` days. */
  const allDay = (day: number, days = 1): [number, number] => {
    const first = utcDate(addDays(today, day));
    return [first, first + days * DAY_MS];
  };
  const halfHours = (minutes: number) => Math.floor((now + minutes * 60_000) / 1_800_000) * 1_800_000;

  // The 9:30 standup, every weekday of three weeks.
  for (let day = -7; day <= 14; day++) {
    const weekday = new Date(addDays(today, day)).getDay();
    if (weekday === 0 || weekday === 6) continue;
    add(1, ...at(day, "09:30", 15), false, {
      title: "Standup",
      callLink: TEAMS,
      location: "Microsoft Teams Meeting",
      organizer: SAM,
      attendees: [ME_WORK, PRIYA, HANNAH],
      repeats: true,
      series: "standup",
    });
  }
  // Your own: a weekly planning hour on Mondays.
  for (let day = -7; day <= 14; day++) {
    if (new Date(addDays(today, day)).getDay() !== 1) continue;
    add(1, ...at(day, "16:00", 60), false, {
      title: "Weekly planning",
      response: "organizer",
      repeats: true,
      series: "planning",
    });
  }

  // Today: two meetings that overlap, a lunch, and focus time.
  add(1, ...at(0, "11:00", 60), false, {
    title: "Design review: Inbox v3",
    location: "Room 4 / Teams",
    callLink: TEAMS,
    organizer: HANNAH,
    attendees: [ME_WORK, PRIYA, KAI],
    notes: "Walk through the new list density and the hover actions.\nKai has the latest Figma.",
  });
  add(1, ...at(0, "11:30", 45), false, {
    title: "Vendor call",
    callLink: "https://us02web.zoom.us/j/123456789",
    organizer: ALEX,
    attendees: [ME_WORK],
    response: "tentative",
  });
  add(3, ...at(0, "12:30", 60), false, {
    title: "Lunch with Ana",
    location: "Café Luna",
    response: "organizer",
    reminder: 30,
  });
  add(1, ...at(0, "14:00", 120), false, { title: "Focus time", response: "organizer", showAs: "busy", private: true });

  // Tomorrow: the invitation from the demo mailbox, and a 1:1 that was canceled.
  const sync = halfHours(1500);
  add(1, sync, sync + 45 * 60_000, false, {
    title: "Q4 planning sync",
    location: "Microsoft Teams Meeting",
    callLink: TEAMS,
    organizer: PRIYA,
    attendees: [ME_WORK, SAM, HANNAH, ALEX],
    response: "none",
    notes: "Let's lock the Q4 priorities before the offsite. Please skim the roadmap doc beforehand.",
  });
  add(1, ...at(1, "15:00", 30), false, {
    title: "1:1 with Priya",
    organizer: PRIYA,
    attendees: [ME_WORK],
    canceled: true,
    repeats: true,
    series: "one-on-one",
  });
  add(1, ...at(1, "16:00", 30), false, {
    title: "Budget check-in",
    organizer: KAI,
    attendees: [ME_WORK, SAM],
    response: "declined",
  });

  // Later this week and next: an offsite over two days, a holiday, and more.
  add(2, ...allDay(3, 2), true, {
    title: "Team offsite",
    location: "Lakeside Lodge",
    organizer: SAM,
    attendees: [ME_WORK, PRIYA, HANNAH, KAI],
    notes: "Cabins are booked. Bring layers.",
  });
  add(3, ...allDay(5), true, { title: "Mom's birthday", response: "organizer", showAs: "free" });
  add(2, ...allDay(-2), true, { title: "Release day", organizer: PRIYA, showAs: "free" });
  add(1, ...at(2, "10:00", 60), false, {
    title: "Roadmap review",
    callLink: TEAMS,
    organizer: SAM,
    attendees: [ME_WORK, PRIYA, HANNAH, KAI, ALEX],
    response: "none",
  });
  add(1, ...at(2, "10:30", 30), false, { title: "Interview: frontend", organizer: HANNAH, attendees: [ME_WORK] });
  add(1, ...at(2, "10:30", 60), false, { title: "Security training", callLink: MEET, organizer: KAI, attendees: [ME_WORK] });
  const hike = halfHours(2700);
  add(3, hike, hike + 240 * 60_000, false, {
    title: "Saturday hike",
    location: "Pantoll trailhead, Mount Tamalpais",
    organizer: { name: "Me", address: "me@gmail.com" },
    attendees: [MARCUS],
    response: "organizer",
  });
  add(1, ...at(7, "13:00", 60), false, { title: "Customer interview", callLink: MEET, organizer: PRIYA, attendees: [ME_WORK] });
  add(1, ...at(8, "16:30", 90), false, { title: "Quarterly all-hands", callLink: TEAMS, organizer: SAM, response: "tentative" });
  add(3, ...at(-1, "18:00", 90), false, { title: "Climbing", location: "Mission Cliffs", response: "organizer" });
  return events;
}

export function demoCalendars(): CalendarList {
  return {
    calendars: CALENDARS,
    accounts: [PERSONAL, WORK].map((account) => ({ account, status: "ready" })),
  };
}

export function demoListEvents(start: number, end: number): CalendarEvent[] {
  return [...demoEvents(Date.now()), ...made]
    .filter((e) => !deleted.has(e.id))
    .map((e) => ({ ...e, ...changes.get(e.id) }))
    .filter((e) => e.start < end && e.end > start);
}

/** The demo's events that a change with `scope` to `id` is for: it, its whole series, or those of it from it on. */
function scoped(id: number, scope: EventScope) {
  const all = demoListEvents(-Infinity, Infinity);
  const e = all.find((x) => x.id === id);
  if (!e) return [];
  if (!e.series || scope === "one") return [e];
  return all.filter((x) => x.series === e.series && (scope === "all" || x.start >= e.start));
}

/** How the demo event `id`'s series repeats. */
export function demoRepeat(id: number): Repeat | null {
  const e = demoListEvents(-Infinity, Infinity).find((x) => x.id === id);
  return (e?.series && rules.get(e.series)) || null;
}

/** The days (local midnights) `rule` puts an event on from `first`, up to a year and a bit. */
function ruleDays(rule: Repeat, first: number) {
  const days: number[] = [];
  const start = new Date(first);
  const until = rule.ends.kind === "on" ? dateValue(rule.ends.date) : first + 400 * DAY_MS;
  const most = rule.ends.kind === "after" ? rule.ends.times : 200;
  for (let i = 0; i < 800 && days.length < most; i++) {
    const d = new Date(start.getFullYear(), start.getMonth(), start.getDate() + i);
    if (d.getTime() > until) break;
    const months = (d.getFullYear() - start.getFullYear()) * 12 + d.getMonth() - start.getMonth();
    const last = new Date(d.getFullYear(), d.getMonth(), d.getDate() + 7).getMonth() !== d.getMonth();
    const onDay =
      rule.nth === undefined
        ? d.getDate() === rule.day
        : rule.weekdays.includes(d.getDay()) && (rule.nth === 5 ? last : Math.ceil(d.getDate() / 7) === rule.nth);
    const hit =
      rule.every === "day"
        ? i % rule.interval === 0
        : rule.every === "week"
          ? rule.weekdays.includes(d.getDay()) && Math.floor((i + start.getDay()) / 7) % rule.interval === 0
          : rule.every === "month"
            ? months % rule.interval === 0 && onDay
            : (d.getFullYear() - start.getFullYear()) % rule.interval === 0 && d.getMonth() + 1 === (rule.month ?? 1) && onDay;
    if (hit) days.push(d.getTime());
  }
  return days;
}

/** As `saveEvent`, in the demo: made or changed here, the series moving by as much as the occurrence did. */
export function demoSaveEvent(id: number | null, draft: EventDraft, scope: EventScope): Promise<void> {
  const fields = {
    calendar: draft.calendar,
    title: draft.title.trim(),
    allDay: draft.allDay,
    location: draft.location.trim() || undefined,
    notes: draft.notes.trim() || undefined,
    attendees: draft.attendees.map((p) => ({ ...p, name: p.name || p.address })),
    reminder: draft.reminder ?? undefined,
    showAs: draft.showAs,
    onlineMeeting: draft.teams,
    callLink: draft.teams ? TEAMS : undefined,
    private: draft.private,
  };
  const make = (start: number, series?: string) =>
    made.push({
      ...fields,
      id: 10_000 + made.length,
      account: CALENDARS.find((c) => c.id === draft.calendar)!.account,
      start,
      end: start + (draft.end - draft.start),
      response: "organizer",
      canceled: false,
      repeats: series !== undefined,
      series,
    });
  /** A series by `rule` from the draft's own day, named `series`. */
  const makeSeries = (rule: Repeat, series: string) => {
    rules.set(series, rule);
    const first = draft.allDay ? fromUtcDate(draft.start) : startOfDay(draft.start);
    for (const day of ruleDays(rule, first)) make(draft.allDay ? toUtcDate(day) : atMinutes(day, minutesOf(draft.start)), series);
  };
  const before = id === null ? undefined : scoped(id, "one")[0];
  const oldRule = before?.series ? (rules.get(before.series) ?? null) : null;
  const ruleChanged = before !== undefined && JSON.stringify(draft.repeat) !== JSON.stringify(oldRule) && scope !== "one";
  if (id === null || !before) {
    if (draft.repeat) makeSeries(draft.repeat, `made-${made.length}`);
    else make(draft.start);
  } else if (ruleChanged || (!before.repeats && draft.repeat)) {
    // The occurrences it's for give way to the new rule's, from this one's day on.
    for (const e of before.repeats ? scoped(id, scope) : [before]) deleted.add(e.id);
    if (draft.repeat) makeSeries(draft.repeat, scope === "all" && before.series ? before.series : `made-${made.length}`);
    else make(draft.start);
  } else {
    const before = scoped(id, "one")[0];
    for (const e of scoped(id, scope)) {
      const shift = draft.start - before.start;
      const times = e.id === id ? { start: draft.start, end: draft.end } : { start: e.start + shift, end: e.start + shift + (draft.end - draft.start) };
      changes.set(e.id, { ...changes.get(e.id), ...fields, ...times });
    }
  }
  return new Promise((resolve) => setTimeout(resolve, 300));
}

/** As `deleteEvent`, in the demo. */
export function demoDeleteEvent(id: number, scope: EventScope): Promise<void> {
  for (const e of scoped(id, scope)) deleted.add(e.id);
  return new Promise((resolve) => setTimeout(resolve, 300));
}

export function demoAnswer(id: number, answer: InviteAnswer): Promise<void> {
  answers.set(id, answer);
  return new Promise((resolve) => setTimeout(resolve, 300));
}
