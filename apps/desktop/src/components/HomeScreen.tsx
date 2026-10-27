import { ArrowUpDown, Bell, BellOff, Inbox, Pen, Search, Users, X, Zap } from "lucide-react";
import { useEffect, useState, type ReactNode } from "react";
import {
  greetingLine,
  secondLine,
  summaryText,
  type HomeCounts,
  type HomeSession,
  type HomeSettings,
  type PreviewFilter,
} from "../lib/homeScreen";
import { formatListDate } from "../lib/format";
import { tipAttrs } from "../lib/tooltip";
import type { Message } from "../types";
import Avatar from "./Avatar";
import Dropdown, { MenuItem } from "./Dropdown";

/** Spark's Preview Emails choices, in its order. */
const FILTERS: { key: PreviewFilter; label: string; Icon: typeof Bell }[] = [
  { key: "all", label: "Show All Emails", Icon: Bell },
  { key: "people", label: "Show Emails From People", Icon: Users },
  { key: "priority", label: "Show Priority Emails", Icon: Zap },
  { key: "none", label: "No Email Preview", Icon: BellOff },
];

/**
 * Wallpapers, one a day. Spark uses photos; these are calm gradients dark
 * enough for white text.
 */
const WALLPAPERS = [
  "linear-gradient(135deg, #1e3a5f 0%, #3b6e8f 45%, #c9826b 100%)",
  "linear-gradient(160deg, #233329 0%, #3f6b52 50%, #a7b98a 100%)",
  "linear-gradient(140deg, #2b2346 0%, #5b4b8a 50%, #d28a7c 100%)",
  "linear-gradient(150deg, #10324a 0%, #1f6f8b 55%, #99c5c4 100%)",
  "linear-gradient(135deg, #3a2a1f 0%, #8a5a3c 50%, #e0b07a 100%)",
  "linear-gradient(155deg, #1c2541 0%, #3a506b 50%, #5bc0be 100%)",
  "linear-gradient(145deg, #2d1e2f 0%, #6b3e5e 50%, #e8a87c 100%)",
];

const dateFormat = new Intl.DateTimeFormat(undefined, { weekday: "long", month: "long", day: "numeric" });
const clockFormat = new Intl.DateTimeFormat(undefined, { hour: "numeric", minute: "2-digit" });

/** The current time, every `ms`. */
function useClock(ms: number) {
  const [now, setNow] = useState(Date.now);
  useEffect(() => {
    const timer = window.setInterval(() => setNow(Date.now()), ms);
    return () => window.clearInterval(timer);
  }, [ms]);
  return now;
}

/** The day of the year, for the day's wallpaper. */
const dayOfYear = (t: number) => {
  const d = new Date(t);
  return Math.floor((d.getTime() - new Date(d.getFullYear(), 0, 0).getTime()) / 86_400_000);
};

/**
 * Spark's Home Screen ("Email Focus"): the date and a clock, a greeting, when
 * to check the Inbox, what came while away, and a feed of new unread mail.
 * It shows in place of the list and the reader between sessions.
 */
export default function HomeScreen({
  settings,
  session,
  name,
  counts,
  items,
  onCheckInbox,
  onNewEmails,
  onFilter,
  onOpen,
  onDismiss,
  onClearAll,
  onSearch,
  onCompose,
}: {
  settings: HomeSettings;
  session: HomeSession;
  /** The user's name, for the greeting. */
  name: string;
  /** New mail since the last session. */
  counts: HomeCounts;
  /** The feed, already filtered. */
  items: Message[];
  onCheckInbox: () => void;
  onNewEmails: () => void;
  onFilter: (filter: PreviewFilter) => void;
  onOpen: (m: Message) => void;
  onDismiss: (m: Message) => void;
  onClearAll: () => void;
  onSearch: () => void;
  onCompose: () => void;
}) {
  const now = useClock(1_000);
  const between = session.state === "betweenSessions";
  const welcoming = session.state === "welcome" || session.state === "welcomeBack";
  const hasNew = counts.people + counts.notifications + counts.newsletters > 0;
  const line = secondLine(session, settings, now);
  const filter = FILTERS.find((f) => f.key === settings.filter)!;

  return (
    <section
      aria-label="Home Screen"
      className="relative flex min-w-0 flex-1 flex-col overflow-hidden text-white"
      style={{ background: WALLPAPERS[dayOfYear(now) % WALLPAPERS.length] }}
    >
      <div className="pointer-events-none absolute inset-0 bg-black/20" />

      <header className="relative flex shrink-0 items-start gap-3 px-10 pt-12">
        <div>
          <div className="text-base font-medium leading-6 text-white/90">{dateFormat.format(now)}</div>
          <div className="text-[36px] font-light leading-[44px] tabular-nums">{clockFormat.format(now)}</div>
        </div>
        <div role="toolbar" aria-label="Home Screen toolbar" className="ml-auto flex items-center gap-1.5">
          <Dropdown
            align="end"
            width="w-60"
            trigger={({ toggle, open }) => (
              <button
                aria-haspopup="menu"
                aria-expanded={open}
                aria-label={`Preview Emails: ${filter.label}`}
                onClick={toggle}
                className={`${GLASS} flex h-9 items-center gap-2 rounded-full px-4 text-[13px] font-medium`}
              >
                <filter.Icon size={15} />
                <span className="hidden sm:inline">{filter.label}</span>
                <ArrowUpDown size={13} className="opacity-70" />
              </button>
            )}
          >
            <li className="menu-title px-2 py-1 text-xs">Preview Emails</li>
            {FILTERS.map((f) => (
              <MenuItem
                key={f.key}
                label={f.label}
                icon={<f.Icon size={14} />}
                checked={f.key === settings.filter}
                onSelect={() => onFilter(f.key)}
              />
            ))}
          </Dropdown>
          <GlassIcon label="Search" onClick={onSearch}>
            <Search size={16} />
          </GlassIcon>
          <GlassIcon label="New Message" onClick={onCompose}>
            <Pen size={16} />
          </GlassIcon>
        </div>
      </header>

      <div className="relative min-h-0 flex-1 overflow-y-auto px-10 pb-10">
        <div className="mt-[9vh] max-w-[900px]">
          <h2 className="text-[40px] font-bold leading-[1.15] tracking-tight drop-shadow-sm lg:text-[70px]">
            {greetingLine(new Date(now).getHours(), name)}
          </h2>
          {line && (
            <p className="mt-2 text-xl font-medium text-white/90 drop-shadow-sm lg:text-[30px] lg:leading-10">
              {line.kind === "time"
                ? "It's time to check your Inbox"
                : `We suggest checking your inbox at ${clockFormat.format(line.at)}${line.tomorrow ? " tomorrow" : ""}`}
            </p>
          )}

          <div className="mt-7 flex flex-wrap items-center gap-2.5">
            <button
              onClick={onCheckInbox}
              className={`flex h-[38px] items-center gap-2 rounded-full px-5 text-sm font-semibold ${
                between ? GLASS : "bg-primary text-primary-content hover:brightness-110"
              }`}
            >
              <Inbox size={16} />
              {between ? "Open Inbox" : "Check Inbox"}
            </button>
            {welcoming && hasNew && (
              <button onClick={onNewEmails} className={`${GLASS} h-[38px] rounded-full px-5 text-sm font-semibold`}>
                New emails only
              </button>
            )}
          </div>

          {welcoming && <Summary counts={counts} />}
        </div>

        {settings.filter !== "none" && items.length > 0 && (
          <Feed
            title={settings.filter === "priority" ? "New Unread Priority Emails" : "New Unread Emails"}
            items={items}
            now={now}
            onOpen={onOpen}
            onDismiss={onDismiss}
            onClearAll={onClearAll}
          />
        )}
      </div>
    </section>
  );
}

/** Frosted glass over the wallpaper. */
const GLASS = "bg-white/15 backdrop-blur-md hover:bg-white/25 focus-visible:outline-white";

function GlassIcon({ label, onClick, children }: { label: string; onClick: () => void; children: ReactNode }) {
  return (
    <button {...tipAttrs(label)} aria-label={label} onClick={onClick} className={`${GLASS} grid size-9 place-items-center rounded-full`}>
      {children}
    </button>
  );
}

/** "3 emails from people, 2 newsletters", the people part in bold, as in Spark. */
function Summary({ counts }: { counts: HomeCounts }) {
  const text = summaryText(counts);
  const [first, ...rest] = text.split(", ");
  return (
    <p className="mt-5 text-[15px] text-white/90 drop-shadow-sm">
      <b className="font-semibold text-white">{first}</b>
      {rest.length > 0 && `, ${rest.join(", ")}`}
    </p>
  );
}

function Feed({
  title,
  items,
  now,
  onOpen,
  onDismiss,
  onClearAll,
}: {
  title: string;
  items: Message[];
  now: number;
  onOpen: (m: Message) => void;
  onDismiss: (m: Message) => void;
  onClearAll: () => void;
}) {
  return (
    <section aria-label={title} className="group/feed mt-10 w-[535px] max-w-full">
      <div className="mb-2 flex h-6 items-center px-1">
        <h3 className="text-[13px] font-semibold uppercase tracking-wide text-white/85">{title}</h3>
        <button
          onClick={onClearAll}
          aria-label="Clear all notifications"
          className="ml-auto rounded-full px-2.5 py-0.5 text-xs font-medium text-white/85 opacity-0 hover:bg-white/20 focus-visible:opacity-100 group-hover/feed:opacity-100"
        >
          Clear All
        </button>
      </div>
      <ul className="space-y-1">
        {items.map((m) => (
          <li key={m.id} className="group/row relative">
            <button
              onClick={() => onOpen(m)}
              aria-label={`Open the email from ${m.from || m.address}: ${m.subject || "No subject"}`}
              className={`${GLASS} flex h-[34px] w-full items-center gap-2.5 rounded-lg pl-2 pr-9 text-left text-[13px]`}
            >
              <Avatar name={m.from || m.address} account={m.account} size="xs" />
              <span className="w-36 shrink-0 truncate font-semibold">{m.from || m.address}</span>
              {m.priority && <Zap size={12} className="shrink-0 fill-current text-priority" aria-label="Priority" />}
              <span className="min-w-0 flex-1 truncate text-white/85">{m.subject || "No subject"}</span>
              <time className="shrink-0 text-xs text-white/70">{formatListDate(m.timestamp, now)}</time>
            </button>
            <button
              onClick={() => onDismiss(m)}
              aria-label={`Remove the notification from ${m.from || m.address}`}
              className="absolute right-1.5 top-1/2 grid size-6 -translate-y-1/2 place-items-center rounded-full text-white/85 opacity-0 hover:bg-white/25 focus-visible:opacity-100 group-hover/row:opacity-100"
            >
              <X size={13} />
            </button>
          </li>
        ))}
      </ul>
    </section>
  );
}
