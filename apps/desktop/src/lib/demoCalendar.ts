import type { CalendarEvent, CalendarInfo, CalendarList, InviteAnswer, InvitePerson } from "../types";
import { DAY_MS, addDays, startOfDay, utcDate } from "./calendar";

// The demo accounts' calendars. They live here rather than on the Rust side
// because only the UI knows the local time zone the events are placed in:
// a 9:30 standup is at 9:30 wherever the demo runs.

const PERSONAL = 1;
const WORK = 2;

const CALENDARS: CalendarInfo[] = [
  { id: 1, account: WORK, name: "Calendar", color: "#3d8bff", isDefault: true },
  { id: 2, account: WORK, name: "Team events", color: "#3ccb6e", isDefault: false },
  { id: 3, account: PERSONAL, name: "Personal", color: "#ff9f2e", isDefault: true },
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

type Spec = Omit<CalendarEvent, "id" | "calendar" | "account" | "start" | "end" | "allDay" | "attendees" | "response" | "canceled" | "repeats" | "showAs" | "private"> &
  Partial<Pick<CalendarEvent, "attendees" | "response" | "canceled" | "repeats" | "showAs" | "private">>;

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
  add(3, ...at(0, "12:30", 60), false, { title: "Lunch with Ana", location: "Café Luna", response: "organizer" });
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
  return demoEvents(Date.now()).filter((e) => e.start < end && e.end > start);
}

export function demoAnswer(id: number, answer: InviteAnswer): Promise<void> {
  answers.set(id, answer);
  return new Promise((resolve) => setTimeout(resolve, 300));
}
