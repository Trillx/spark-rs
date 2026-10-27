import { ArrowUpRight, CalendarDays, Check, Clock, Link2, Settings2, Video, X } from "lucide-react";
import { useEffect, useLayoutEffect, useRef, useState, type ReactNode } from "react";
import {
  dragWindow,
  fitMeetingPopup,
  meetingPopupAct,
  meetingPopupMeetings,
  onMeetingPopupChanged,
  type PopupAct,
} from "../api";
import { avatarColor, initials } from "../lib/format";
import { timeLeft } from "../lib/calendar";
import type { InvitePerson, PopupMeeting } from "../types";

/** Room around the cards for their shadow and the dismiss-all button; the window is this much bigger. */
const MARGIN = 16;

/**
 * The meeting pop-up's window (popup.html), like Spark's Meeting Pop-Up:
 * a card per meeting with who's in it, the time to go or left, and Join.
 * The Rust side (meetings.rs) decides what shows; the window fits itself
 * to the cards and can be dragged by them.
 */
export default function MeetingPopup() {
  const [meetings, setMeetings] = useState<PopupMeeting[]>([]);
  const [now, setNow] = useState(Date.now);
  const box = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const stop = onMeetingPopupChanged(setMeetings);
    void meetingPopupMeetings().then(setMeetings);
    return stop;
  }, []);

  useEffect(() => {
    const timer = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(timer);
  }, []);

  useLayoutEffect(() => {
    const el = box.current;
    if (!el) return;
    const observer = new ResizeObserver(() => {
      if (el.offsetHeight) void fitMeetingPopup(el.offsetWidth + 2 * MARGIN, el.offsetHeight + 2 * MARGIN).catch(() => {});
    });
    observer.observe(el);
    return () => observer.disconnect();
  }, []);

  const act = (a: PopupAct) => void meetingPopupAct(a).catch(() => {});

  return (
    <div
      ref={box}
      className="group/popup relative w-[440px] animate-[popup-in_240ms_ease-out] rounded-[15px] bg-popover shadow-[0_5px_10px_rgba(0,0,0,0.18)] ring-1 ring-hairline"
      style={{ margin: MARGIN }}
      onMouseDown={(e) => {
        if (e.button === 0 && !(e.target as Element).closest("button")) dragWindow();
      }}
    >
      {meetings.map((m, i) => (
        <Card key={m.id} meeting={m} now={now} divided={i > 0} act={act} />
      ))}
      {meetings.length > 1 && (
        <button
          className="absolute -left-2 -top-2 hidden size-5 place-items-center rounded-full bg-popover text-text-2 shadow ring-1 ring-hairline group-hover/popup:grid hover:text-base-content"
          title="Dismiss all"
          aria-label="Dismiss all"
          onClick={() => act({ action: "dismissAll" })}
        >
          <X size={12} strokeWidth={2.25} />
        </button>
      )}
    </div>
  );
}

function Card({
  meeting: m,
  now,
  divided,
  act,
}: {
  meeting: PopupMeeting;
  now: number;
  divided: boolean;
  act: (a: PopupAct) => void;
}) {
  const [copied, setCopied] = useState(false);
  useEffect(() => {
    if (!copied) return;
    const timer = window.setTimeout(() => setCopied(false), 2000);
    return () => window.clearTimeout(timer);
  }, [copied]);

  const copy = (link: string) => void copyText(link).then(() => setCopied(true), () => {});

  return (
    <div className={`relative p-3 ${divided ? "border-t border-hairline" : ""}`}>
      <div className="flex gap-3">
        <People people={m.people} call={!!m.callLink} />
        <div className="min-w-0 flex-1 pr-12 pt-1">
          <p className="truncate text-[14px] font-semibold leading-[18px]" title={m.title}>
            {m.title || "No Title"}
          </p>
          <p className="mt-0.5 text-xs font-medium tabular-nums text-text-2">{timeLeft(m.start, m.end, now)}</p>
        </div>
      </div>

      <div className="mt-2 flex items-center justify-end gap-1.5">
        <Tool label="Open in Calendar" onClick={() => act({ action: "openCalendar", id: m.id })}>
          <CalendarDays />
        </Tool>
        {m.callLink && (
          <Tool label={copied ? "Copied" : "Copy link"} onClick={() => copy(m.callLink!)}>
            {copied ? <Check /> : <Link2 />}
          </Tool>
        )}
        {m.callLink && m.joined && (
          <Tool label="Open meeting" onClick={() => act({ action: "join", id: m.id })}>
            <ArrowUpRight />
          </Tool>
        )}
        <Tool label="Snooze for a minute" onClick={() => act({ action: "snooze", id: m.id })}>
          <Clock />
        </Tool>
        {m.callLink && !m.joined && (
          <button
            className="btn btn-primary btn-sm ml-1.5 h-7 min-h-7 gap-1 rounded-lg px-3 text-[13px] font-medium"
            title={m.callLink}
            onClick={() => act({ action: "join", id: m.id })}
          >
            Join
            <ArrowUpRight size={14} />
          </button>
        )}
      </div>

      <div className="absolute right-2 top-2 flex">
        <Tool label="Pop-up settings" small onClick={() => act({ action: "openSettings" })}>
          <Settings2 size={13} />
        </Tool>
        <Tool label="Dismiss" small onClick={() => act({ action: "dismiss", id: m.id })}>
          <X size={15} />
        </Tool>
      </div>
    </div>
  );
}

/**
 * The clipboard API needs the page focused, which a pop-up that never took
 * the focus may not be; a selected textarea and the copy command don't.
 */
async function copyText(text: string) {
  try {
    await navigator.clipboard.writeText(text);
  } catch {
    const area = document.createElement("textarea");
    area.value = text;
    document.body.append(area);
    area.select();
    const copied = document.execCommand("copy");
    area.remove();
    if (!copied) throw new Error("Couldn't copy");
  }
}

/** A gray icon button. Native tooltips: the window is too small for the app's. */
function Tool({
  label,
  small,
  onClick,
  children,
}: {
  label: string;
  small?: boolean;
  onClick: () => void;
  children: ReactNode;
}) {
  return (
    <button
      className={`grid place-items-center rounded-lg text-text-2 hover:bg-base-content/10 hover:text-base-content ${small ? "size-6" : "size-[26px]"}`}
      title={label}
      aria-label={label}
      onClick={onClick}
    >
      {children}
    </button>
  );
}

/**
 * Spark's avatar group: up to four initials in a dark rounded box, the
 * fourth a "+N" when there are more. With no one else, a video or calendar
 * glyph.
 */
function People({ people, call }: { people: InvitePerson[]; call: boolean }) {
  const tile = "grid min-w-0 place-items-center rounded-[3px] text-[11px] font-medium leading-none text-white";
  let tiles: ReactNode;
  if (people.length === 0) {
    const Glyph = call ? Video : CalendarDays;
    tiles = (
      <span className={`${tile} col-span-2 row-span-2 bg-neutral-500`}>
        <Glyph size={18} />
      </span>
    );
  } else if (people.length === 1) {
    tiles = (
      <span className={`${tile} col-span-2 row-span-2 text-[15px] ${avatarColor(people[0].name)}`}>
        {initials(people[0].name)}
      </span>
    );
  } else {
    const shown = people.length > 4 ? people.slice(0, 3) : people;
    const more = people.length - shown.length;
    tiles = (
      <>
        {shown.map((p, i) => (
          <span
            key={p.address}
            className={`${tile} ${avatarColor(p.name)} ${people.length === 2 ? "row-span-2" : ""} ${
              people.length === 3 && i === 2 ? "col-span-2 mx-auto w-[calc(50%-1px)] self-stretch" : ""
            }`}
            title={p.name}
          >
            {(p.name.trim()[0] ?? "?").toUpperCase()}
          </span>
        ))}
        {more > 0 && <span className={`${tile} bg-neutral-500`}>{more > 99 ? "99+" : `+${more}`}</span>}
      </>
    );
  }
  return (
    <div className="grid h-11 w-[54px] shrink-0 grid-cols-2 grid-rows-2 gap-0.5 rounded-[10px] border-2 border-black bg-black">
      {tiles}
    </div>
  );
}
