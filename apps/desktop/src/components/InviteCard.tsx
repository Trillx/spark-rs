import {
  AlignLeft,
  CalendarCheck,
  CalendarClock,
  CalendarX2,
  Check,
  CircleCheck,
  CircleDashed,
  CircleHelp,
  CircleX,
  MapPin,
  Users,
  Video,
  X,
  type LucideIcon,
} from "lucide-react";
import { useLayoutEffect, useRef, useState, type ReactNode } from "react";
import { answerInvite, openExternal } from "../api";
import { cacheInviteAnswer } from "../lib/content";
import type { AccountView, InviteAnswer, InviteCard as Invite, InvitePerson } from "../types";
import Avatar from "./Avatar";

const DAY_MS = 24 * 60 * 60 * 1000;
/** People shown before "N more". */
const PEOPLE_SHOWN = 7;

const ANSWERS: {
  answer: InviteAnswer;
  label: string;
  /** The chosen button's wider label. */
  chosen: string;
  Icon: LucideIcon;
  tint: string;
  tile: string;
}[] = [
  { answer: "accepted", label: "Accept", chosen: "Accepted by you", Icon: Check, tint: "text-success", tile: "bg-success/12" },
  { answer: "tentative", label: "Maybe", chosen: "Maybe you will join", Icon: CircleHelp, tint: "text-warning", tile: "bg-warning/12" },
  { answer: "declined", label: "Decline", chosen: "Declined by you", Icon: X, tint: "text-error", tile: "bg-error/10" },
];

/**
 * An invite email's event in place of its body, Spark style: a date tile,
 * the title and time, then Accept / Maybe / Decline for an invitation (a
 * banner for a reply or cancellation), and who's invited, the call to join,
 * the place and the description. An answer shows at once and goes out by
 * email to the organizer; if sending fails, the card says so and goes back.
 */
export default function InviteCard({ id, invite, account }: { id: number; invite: Invite; account?: AccountView }) {
  const [answer, setAnswer] = useState(invite.answer);
  /** "Change" brings the three buttons back; nothing is sent until one is chosen. */
  const [changing, setChanging] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const me = account?.email.toLowerCase();

  const choose = (chosen: InviteAnswer) => {
    setChanging(false);
    if (chosen === answer) return;
    const before = answer;
    setAnswer(chosen);
    setError(null);
    answerInvite(id, chosen).then(
      () => cacheInviteAnswer(id, chosen),
      (e: unknown) => {
        setAnswer(before);
        setError(String(e));
      },
    );
  };

  const isMe = (p: InvitePerson) => p.address.toLowerCase() === me;
  const people = [
    ...(invite.organizer ? [{ ...invite.organizer, organizer: true }] : []),
    ...invite.attendees.map((p) => ({ ...p, answer: isMe(p) && answer ? answer : p.answer, organizer: false })),
  ];
  const details = people.length > 0 || invite.callLink || invite.location || invite.description;

  return (
    <section className="mx-5 mb-3 mt-1.5 overflow-hidden rounded-xl border border-hairline" aria-label="Invitation">
      <Banner invite={invite} isMe={isMe} />
      <div className="flex flex-wrap items-center gap-x-4 gap-y-3 p-4">
        <DateTile invite={invite} />
        <div className="min-w-[180px] flex-1 select-text">
          <h3 className="line-clamp-3 text-[15px] font-bold leading-5">{invite.summary || "No title"}</h3>
          <p className="mt-1 text-[13px] leading-5 text-text-2">
            {timeRange(invite)}
            {invite.repeats && " · Repeats"}
          </p>
        </div>
        {invite.canAnswer && <Answers answer={changing ? undefined : answer} onChoose={choose} onChange={() => setChanging(true)} />}
      </div>
      {error && <p className="select-text px-4 pb-3 text-[13px] leading-5 text-error">Couldn't send your answer. {error}</p>}
      {details && (
        <div className="space-y-2.5 border-t border-hairline px-4 py-3 text-[13px] leading-5">
          {people.length > 0 && (
            <Row Icon={Users}>
              <People people={people} isMe={isMe} />
            </Row>
          )}
          {invite.callLink && (
            <Row Icon={Video}>
              <button
                className="font-medium text-primary hover:underline"
                data-tip={invite.callLink}
                onClick={() => void openExternal(invite.callLink!)}
              >
                Join with {callService(invite.callLink)}
              </button>
            </Row>
          )}
          {invite.location && (
            <Row Icon={MapPin}>
              <span className="select-text break-words">{invite.location}</span>
            </Row>
          )}
          {invite.description && (
            <Row Icon={AlignLeft}>
              <Description text={invite.description} />
            </Row>
          )}
        </div>
      )}
    </section>
  );
}

/** A reply's answer, a proposal or a cancellation, above the event. */
function Banner({ invite, isMe }: { invite: Invite; isMe: (p: InvitePerson) => boolean }) {
  const name = (p: InvitePerson | undefined) => (p ? (isMe(p) ? "You" : p.name) : "Someone");
  let line: [string, LucideIcon, string];
  if (invite.kind === "reply") {
    const who = invite.attendees[0];
    const n = name(who);
    line =
      who?.answer === "accepted"
        ? [`${n} accepted this event`, CalendarCheck, "bg-success/12 text-success"]
        : who?.answer === "tentative"
          ? [`${n} tentatively accepted this event`, CalendarClock, "bg-warning/12 text-warning"]
          : who?.answer === "declined"
            ? [`${n} declined this event`, CalendarX2, "bg-error/10 text-error"]
            : [`${n} replied to this event`, CalendarClock, "bg-base-content/5 text-text-2"];
  } else if (invite.kind === "cancellation" || invite.canceled) {
    const by = invite.organizer;
    line = [by ? `${name(by)} canceled this event` : "The event was canceled", CalendarX2, "bg-error/10 text-error"];
  } else if (invite.kind === "proposal") {
    line = [`${name(invite.attendees[0])} proposed a new time`, CalendarClock, "bg-info/12 text-info"];
  } else {
    return null;
  }
  const [text, Icon, tone] = line;
  return (
    <div className={`flex items-center gap-2 px-4 py-2 text-[13px] font-semibold ${tone}`}>
      <Icon size={15} />
      {text}
    </div>
  );
}

/** Month, day and weekday of the start. */
function DateTile({ invite }: { invite: Invite }) {
  const part = (options: Intl.DateTimeFormatOptions) =>
    new Intl.DateTimeFormat(undefined, { ...options, timeZone: zoneOf(invite) }).format(invite.start);
  return (
    <div className="flex size-[60px] shrink-0 flex-col overflow-hidden rounded-lg border border-hairline bg-base-100 text-center" aria-hidden>
      <span className="bg-error text-[10px] font-bold uppercase leading-4 tracking-wide text-error-content">
        {part({ month: "short" })}
      </span>
      <span className="text-[22px] font-semibold leading-[26px]">{part({ day: "numeric" })}</span>
      <span className="text-[10px] font-medium leading-3 text-text-2">{part({ weekday: "short" })}</span>
    </div>
  );
}

/** The three answers, or the one given with "Change". */
function Answers({
  answer,
  onChoose,
  onChange,
}: {
  answer?: InviteAnswer;
  onChoose: (answer: InviteAnswer) => void;
  onChange: () => void;
}) {
  const given = ANSWERS.find((a) => a.answer === answer);
  if (given) {
    return (
      <div className="flex items-center gap-3">
        <span className={`inline-flex h-8 items-center gap-1.5 rounded-lg px-3 text-[13px] font-semibold ${given.tile} ${given.tint}`}>
          <given.Icon size={15} strokeWidth={2.5} />
          {given.chosen}
        </span>
        <button className="text-xs font-semibold uppercase tracking-wide text-primary hover:underline" onClick={onChange}>
          Change
        </button>
      </div>
    );
  }
  return (
    <div className="flex items-center gap-1.5" role="group" aria-label="Answer the invitation">
      {ANSWERS.map(({ answer, label, Icon, tint }) => (
        <button
          key={answer}
          className="inline-flex h-8 items-center gap-1.5 rounded-lg bg-chip px-3 text-[13px] font-medium hover:brightness-95"
          onClick={() => onChoose(answer)}
        >
          <Icon size={15} strokeWidth={2.5} className={tint} />
          {label}
        </button>
      ))}
    </div>
  );
}

function Row({ Icon, children }: { Icon: LucideIcon; children: ReactNode }) {
  return (
    <div className="flex gap-3">
      <Icon size={16} className="mt-0.5 shrink-0 text-text-2" />
      <div className="min-w-0 flex-1">{children}</div>
    </div>
  );
}

const STATUS: Record<InviteAnswer | "none", [LucideIcon, string, string]> = {
  accepted: [CircleCheck, "text-success", "Accepted"],
  tentative: [CircleHelp, "text-warning", "Maybe"],
  declined: [CircleX, "text-error", "Declined"],
  none: [CircleDashed, "text-text-2", "No answer yet"],
};

/** The organizer first, then the guests, each with their answer; past the first few, "N more". */
function People({
  people,
  isMe,
}: {
  people: (InvitePerson & { organizer: boolean })[];
  isMe: (p: InvitePerson) => boolean;
}) {
  const [all, setAll] = useState(false);
  const shown = all || people.length <= PEOPLE_SHOWN + 1 ? people : people.slice(0, PEOPLE_SHOWN);
  return (
    <div className="flex flex-wrap gap-1.5">
      {shown.map((p, i) => {
        const [Icon, tint, status] = STATUS[p.answer ?? "none"];
        const name = isMe(p) ? "You" : p.name;
        return (
          <span
            key={i}
            className="inline-flex h-7 max-w-full items-center gap-1.5 rounded-full bg-chip pl-0.5 pr-2"
            data-tip={`${p.address} · ${status}`}
          >
            <Avatar name={p.name} />
            <span className="truncate text-xs font-medium">
              {name}
              {p.organizer && <span className="text-text-2"> (Organizer)</span>}
            </span>
            <Icon size={14} className={`shrink-0 ${tint}`} aria-label={status} />
          </span>
        );
      })}
      {shown.length < people.length && (
        <button className="h-7 rounded-full bg-chip px-3 text-xs font-medium text-text-2 hover:brightness-95" onClick={() => setAll(true)}>
          {people.length - shown.length} more
        </button>
      )}
    </div>
  );
}

/** Seven lines, with "Read more" if there's more. */
function Description({ text }: { text: string }) {
  const ref = useRef<HTMLParagraphElement>(null);
  const [open, setOpen] = useState(false);
  const [clamped, setClamped] = useState(false);
  useLayoutEffect(() => {
    const p = ref.current;
    if (p && !open) setClamped(p.scrollHeight > p.clientHeight + 1);
  }, [text, open]);
  return (
    <>
      <p ref={ref} className={`select-text whitespace-pre-line break-words ${open ? "" : "line-clamp-7"}`}>
        {text}
      </p>
      {clamped && !open && (
        <button className="font-medium text-primary hover:underline" onClick={() => setOpen(true)}>
          Read more
        </button>
      )}
    </>
  );
}

/** All-day and floating times are as written, read as UTC; others show in local time. */
function zoneOf(invite: Invite) {
  return invite.allDay || invite.floating ? "UTC" : undefined;
}

/**
 * "10:00 – 10:45 AM GMT-7", with dates when it ends on another day; "All
 * day" (with the days, if several).
 */
function timeRange(invite: Invite) {
  const timeZone = zoneOf(invite);
  const day = new Intl.DateTimeFormat(undefined, { weekday: "short", month: "short", day: "numeric", timeZone });
  const time = new Intl.DateTimeFormat(undefined, { hour: "numeric", minute: "2-digit", timeZone });
  const key = new Intl.DateTimeFormat("en-CA", { year: "numeric", month: "2-digit", day: "2-digit", timeZone });
  const sameDay = (a: number, b: number) => key.format(a) === key.format(b);
  const { start, end } = invite;
  if (invite.allDay) {
    // The end is the day after the last.
    const last = end !== undefined ? end - DAY_MS : start;
    return last > start && !sameDay(start, last) ? `${day.format(start)} – ${day.format(last)}, all day` : "All day";
  }
  let text =
    end === undefined
      ? time.format(start)
      : sameDay(start, end)
        ? time.formatRange(start, end)
        : `${day.format(start)}, ${time.format(start)} – ${day.format(end)}, ${time.format(end)}`;
  if (!invite.floating) {
    const zone = new Intl.DateTimeFormat(undefined, { timeZoneName: "shortOffset" })
      .formatToParts(start)
      .find((p) => p.type === "timeZoneName")?.value;
    if (zone) text += ` ${zone}`;
  }
  return text;
}

/** "Google Meet", "Microsoft Teams", "Zoom", …, else the link's host. */
function callService(link: string) {
  let host = "";
  try {
    host = new URL(link).hostname.toLowerCase();
  } catch {
    return "the call link";
  }
  const on = (domain: string) => host === domain || host.endsWith(`.${domain}`);
  if (on("meet.google.com")) return "Google Meet";
  if (on("teams.microsoft.com") || on("teams.live.com")) return "Microsoft Teams";
  if (on("zoom.us") || on("zoomgov.com")) return "Zoom";
  if (on("webex.com")) return "Webex";
  if (on("gotomeeting.com") || on("goto.com")) return "GoTo Meeting";
  return host;
}
