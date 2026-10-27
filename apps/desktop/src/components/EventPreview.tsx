import {
  Bell,
  CircleHelp,
  Check,
  ExternalLink,
  Lock,
  MapPin,
  Pencil,
  Repeat,
  Trash2,
  Users,
  Video,
  X,
  type LucideIcon,
} from "lucide-react";
import { useEffect, useLayoutEffect, useRef, useState, type ReactNode, type RefObject } from "react";
import { openExternal } from "../api";
import { useAccount } from "../lib/accounts";
import { alertText, eventDate, eventTime } from "../lib/calendar";
import type { CalendarEvent, CalendarInfo, InviteAnswer, InvitePerson } from "../types";
import { Description, People, callService } from "./InviteCard";

const WIDTH = 320;
const GAP = 8;
/** Below the window's title bar. */
const TOP = 40;

const ATTENDING: { answer: InviteAnswer; label: string; Icon: LucideIcon; tone: string }[] = [
  { answer: "accepted", label: "Yes", Icon: Check, tone: "bg-success/15 text-success" },
  { answer: "tentative", label: "Maybe", Icon: CircleHelp, tone: "bg-warning/15 text-warning" },
  { answer: "declined", label: "No", Icon: X, tone: "bg-error/12 text-error" },
];

/**
 * Where an event's pop-up goes: beside `anchor` (the event's box), right of it or else left,
 * centred on it at first and then only moving to stay in the window; with `modal`, centred
 * in the window. Placed again whenever it or the window changes size ("Read more", an error,
 * details that arrive later), so it never runs off the bottom. `key` starts it over.
 */
export function usePopupPlace(ref: RefObject<HTMLElement | null>, anchor: DOMRect, modal: boolean, width: number, key: unknown) {
  const [place, setPlace] = useState<{ left: number; top: number } | null>(null);
  useLayoutEffect(() => {
    const box = ref.current;
    if (!box) return;
    let top: number | undefined;
    const fit = () => {
      const height = box.offsetHeight;
      if (modal) {
        setPlace({ left: (window.innerWidth - width) / 2, top: Math.max(48, (window.innerHeight - height) / 2) });
        return;
      }
      const right = anchor.right + GAP;
      const left = right + width <= window.innerWidth - GAP ? right : Math.max(GAP, anchor.left - GAP - width);
      const wanted = top ?? anchor.top + anchor.height / 2 - height / 2;
      top = Math.max(TOP, Math.min(wanted, window.innerHeight - height - GAP));
      setPlace({ left, top });
    };
    fit();
    const observer = new ResizeObserver(fit);
    observer.observe(box);
    window.addEventListener("resize", fit);
    return () => {
      observer.disconnect();
      window.removeEventListener("resize", fit);
    };
  }, [anchor, modal, width, key]);
  return place;
}

/**
 * An event's details, beside it (or centred, in Day view): its calendar,
 * title and time, the call to join, place, repeats, its alert, your answer,
 * who's invited, and the notes; Edit and Delete for the user's own. Closes
 * on Esc or a click elsewhere.
 */
export default function EventPreview({
  event: e,
  calendar,
  anchor,
  modal,
  onAnswer,
  repeatText,
  onEdit,
  onDelete,
  onClose,
}: {
  event: CalendarEvent;
  calendar?: CalendarInfo;
  /** The event's box on screen. */
  anchor: DOMRect;
  /** Centred over a dimmed calendar rather than beside the event. */
  modal: boolean;
  /** Rejects with a user-facing error; the answer is then undone. */
  onAnswer: (answer: InviteAnswer) => Promise<void>;
  /** How it repeats, in words, once its series' rule is read. */
  repeatText?: string;
  /** The user's own event: Edit and Delete show. */
  onEdit?: () => void;
  onDelete?: () => void;
  onClose: () => void;
}) {
  const ref = useRef<HTMLDivElement>(null);
  const account = useAccount(e.account);
  const [error, setError] = useState<string | null>(null);
  const place = usePopupPlace(ref, anchor, modal, WIDTH, e.id);

  useEffect(() => {
    const onPointer = (ev: PointerEvent) => {
      // A question it asked (Delete) is above it, not elsewhere.
      if (document.querySelector("[aria-modal]")) return;
      if (!ref.current?.contains(ev.target as Node)) onClose();
    };
    // Capture, so Esc closes the preview and not the calendar.
    const onKey = (ev: KeyboardEvent) => {
      if (ev.key !== "Escape" || document.querySelector("[aria-modal]")) return;
      ev.stopPropagation();
      ev.preventDefault();
      onClose();
    };
    window.addEventListener("pointerdown", onPointer, true);
    window.addEventListener("keydown", onKey, true);
    return () => {
      window.removeEventListener("pointerdown", onPointer, true);
      window.removeEventListener("keydown", onKey, true);
    };
  }, [onClose]);

  const me = account?.email.toLowerCase();
  const isMe = (p: InvitePerson) => p.address.toLowerCase() === me;
  const people = [
    ...(e.organizer ? [{ ...e.organizer, organizer: true }] : []),
    ...e.attendees.filter((p) => !e.organizer || p.address !== e.organizer.address).map((p) => ({ ...p, organizer: false })),
  ];
  const answerable = e.response !== "organizer" && !e.canceled;
  const where = whereOf(e);

  const choose = (answer: InviteAnswer) => {
    if (answer === e.response) return;
    setError(null);
    onAnswer(answer).catch((err: unknown) => setError(String(err)));
  };

  return (
    <>
      {modal && <div className="fixed inset-0 z-[2010] bg-black/20" />}
      <div
        ref={ref}
        role="dialog"
        aria-label={e.title || "Event"}
        className="fixed z-[2020] flex max-h-[calc(100vh-56px)] flex-col overflow-hidden rounded-2xl border border-hairline bg-popover text-[13px] text-base-content shadow-popup"
        style={{ width: WIDTH, left: place?.left ?? -9999, top: place?.top ?? 0 }}
      >
        <div className="flex shrink-0 items-center gap-2 px-4 pb-1 pt-3">
          <span className="size-2 shrink-0 rounded-full" style={{ background: calendar?.color }} />
          <span className="min-w-0 flex-1 truncate text-xs text-text-2">
            {calendar?.name ?? "Calendar"}
            {account && ` · ${account.label}`}
          </span>
          {onEdit && (
            <IconButton tip="Edit" onClick={onEdit}>
              <Pencil size={15} />
            </IconButton>
          )}
          {onDelete && (
            <IconButton tip="Delete" onClick={onDelete}>
              <Trash2 size={15} />
            </IconButton>
          )}
          {e.webLink && (
            <IconButton tip="Open in Outlook" onClick={() => void openExternal(e.webLink!)}>
              <ExternalLink size={15} />
            </IconButton>
          )}
          <IconButton tip="Close" onClick={onClose}>
            <X size={16} />
          </IconButton>
        </div>
        <div className="min-h-0 overflow-y-auto px-4 pb-4">
          <h2 className={`select-text break-words text-xl font-semibold leading-6 ${e.canceled ? "line-through opacity-70" : ""}`}>
            {e.title || (e.private ? "Busy" : "No Title")}
          </h2>
          <p className="mt-1.5 select-text text-[15px] leading-5">
            <span className="font-medium">{eventDate(e)}</span>
            <span className="text-text-2"> · {eventTime(e)}</span>
          </p>
          {e.canceled && (
            <p className="mt-2 inline-flex rounded-md bg-error/10 px-2 py-0.5 text-xs font-semibold text-error">Canceled</p>
          )}

          <div className="mt-3 space-y-2.5">
            {e.callLink && (
              <Row Icon={Video}>
                <div className="flex items-center gap-2">
                  <div className="min-w-0 flex-1">
                    <div className="truncate">{callService(e.callLink)}</div>
                    <div className="select-text truncate text-xs text-text-2" data-tip={e.callLink}>
                      {e.callLink}
                    </div>
                  </div>
                  <button
                    className="btn btn-primary btn-xs h-7 rounded-lg px-3"
                    onClick={() => void openExternal(e.callLink!)}
                  >
                    Join
                  </button>
                </div>
              </Row>
            )}
            {where && (
              <Row Icon={MapPin}>
                {/^https?:\/\/\S+$/.test(where) ? (
                  <button
                    className="block max-w-full truncate text-left text-primary hover:underline"
                    data-tip={where}
                    onClick={() => void openExternal(where)}
                  >
                    {where}
                  </button>
                ) : (
                  <span className="select-text break-words">{where}</span>
                )}
              </Row>
            )}
            {e.repeats && (
              <Row Icon={Repeat}>
                <span>{repeatText ?? "Repeats"}</span>
              </Row>
            )}
            {e.reminder !== undefined && (
              <Row Icon={Bell}>
                <span>{alertText(e)}</span>
              </Row>
            )}
            {e.private && (
              <Row Icon={Lock}>
                <span>Private</span>
              </Row>
            )}
          </div>

          {answerable && (
            <div className="mt-4 flex items-center gap-3" role="group" aria-label="Attending status">
              <span className="text-text-2">{e.response === "none" ? "Going?" : "Attending:"}</span>
              <div className="flex gap-1.5">
                {ATTENDING.map(({ answer, label, Icon, tone }) => (
                  <button
                    key={answer}
                    aria-pressed={e.response === answer}
                    className={`inline-flex h-7 items-center gap-1 rounded-full px-3 text-xs font-semibold ${
                      e.response === answer ? tone : "bg-base-content/[0.07] hover:bg-base-content/[0.12]"
                    }`}
                    onClick={() => choose(answer)}
                  >
                    {e.response === answer && <Icon size={13} strokeWidth={2.5} />}
                    {label}
                  </button>
                ))}
              </div>
            </div>
          )}
          {error && <p className="mt-2 select-text text-error">Couldn't send your answer. {error}</p>}

          {people.length > 0 && (
            <div className="mt-4">
              <div className="mb-1.5 flex items-center gap-2 text-text-2">
                <Users size={15} />
                {people.length === 1 ? "1 person" : `${people.length} people`}
              </div>
              <People people={people} isMe={isMe} chip="bg-base-content/[0.07]" />
            </div>
          )}

          {e.notes && (
            <div className="mt-4 border-t border-hairline pt-3 leading-5">
              <Description text={e.notes} />
            </div>
          )}
        </div>
      </div>
    </>
  );
}

/** The location, less the call link that has its own row; nothing if that was all of it. */
function whereOf(e: CalendarEvent) {
  let where = e.location?.trim() ?? "";
  if (e.callLink) where = where.split(e.callLink).join(" ").replace(/^[\s,;|]+|[\s,;|]+$/g, "");
  return where || undefined;
}

function Row({ Icon, children }: { Icon: LucideIcon; children: ReactNode }) {
  return (
    <div className="flex items-start gap-3">
      <Icon size={16} className="mt-0.5 shrink-0 text-text-2" />
      <div className="min-w-0 flex-1">{children}</div>
    </div>
  );
}

function IconButton({ tip, onClick, children }: { tip: string; onClick: () => void; children: ReactNode }) {
  return (
    <button
      data-tip={tip}
      aria-label={tip}
      className="grid size-7 place-items-center rounded-lg text-text-2 hover:bg-base-content/10 hover:text-base-content"
      onClick={onClick}
    >
      {children}
    </button>
  );
}
