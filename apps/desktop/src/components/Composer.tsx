import {
  BellRing,
  CalendarClock,
  ChevronsUpDown,
  CircleCheck,
  Maximize2,
  Minimize2,
  Paperclip,
  X,
} from "lucide-react";
import { useEffect, useRef, type CSSProperties, type KeyboardEvent, type ReactNode } from "react";
import type { Account } from "../types";
import { ACCOUNT_IDS, ACCOUNTS, LATER_PRESETS, presetText, type LaterPreset } from "../lib/format";
import { IS_MAC } from "../lib/shortcuts";
import Dropdown, { MenuItem } from "./Dropdown";

export interface Draft {
  title: string;
  to: string;
  cc: string;
  bcc: string;
  showCc: boolean;
  showBcc: boolean;
  subject: string;
  body: string;
  from: Account;
  sendLater: LaterPreset | null;
  remind: LaterPreset | null;
  doneAfter: boolean;
  /** The message being replied to or forwarded; "Mark as Done after sending" applies to it. */
  replyTo?: number;
}

export function emptyDraft(from: Account): Draft {
  return {
    title: "New message",
    to: "",
    cc: "",
    bcc: "",
    showCc: false,
    showBcc: false,
    subject: "",
    body: "",
    from,
    sendLater: null,
    remind: null,
    doneAfter: false,
  };
}

export function draftHasContent(d: Draft) {
  return [d.to, d.cc, d.bcc, d.subject, d.body].some((s) => s.trim());
}

/**
 * Compact floating composer, with a toggle that fills the reading pane.
 * Plain text only for now; the rich editor comes later.
 */
export default function Composer({
  draft,
  full,
  fullStyle,
  onChange,
  onToggleFull,
  onClose,
  onSend,
}: {
  draft: Draft;
  full: boolean;
  /** Where the full-pane composer sits (the reading pane's box). */
  fullStyle: CSSProperties;
  onChange: (patch: Partial<Draft>) => void;
  onToggleFull: () => void;
  onClose: () => void;
  onSend: () => void;
}) {
  const toRef = useRef<HTMLInputElement>(null);
  const bodyRef = useRef<HTMLTextAreaElement>(null);

  useEffect(() => {
    // Replies arrive with a recipient, so they start in the body.
    if (draft.to) {
      bodyRef.current?.focus();
      bodyRef.current?.setSelectionRange(draft.body.length, draft.body.length);
    } else {
      toRef.current?.focus();
    }
  }, []); // only when the composer opens

  function onKeyDown(e: KeyboardEvent) {
    const mod = IS_MAC ? e.metaKey : e.ctrlKey;
    if (mod && e.key === "Enter") {
      e.preventDefault();
      onSend();
    }
  }

  const sendLabel = draft.sendLater ? "Schedule" : "Send";
  const account = ACCOUNTS[draft.from];

  return (
    <div
      data-composer
      role="dialog"
      aria-label={draft.title}
      onKeyDown={onKeyDown}
      style={full ? fullStyle : undefined}
      className={`z-50 flex flex-col overflow-hidden border border-hairline bg-base-100 ${
        full
          ? "fixed bottom-0 top-8 rounded-tl-xl"
          : "fixed bottom-4 right-4 h-[min(540px,calc(100vh-64px))] w-[560px] max-w-[calc(100vw-32px)] rounded-xl shadow-popup"
      }`}
    >
      <div className="flex h-10 shrink-0 items-center gap-1 border-b border-hairline pl-4 pr-1.5">
        <span className="flex-1 truncate text-[13px] font-semibold">{draft.title}</span>
        <IconButton title={full ? "Compact view" : "Full screen"} onClick={onToggleFull}>
          {full ? <Minimize2 size={15} /> : <Maximize2 size={15} />}
        </IconButton>
        <IconButton title="Close" onClick={onClose}>
          <X size={16} />
        </IconButton>
      </div>

      <Field label="To">
        <input
          ref={toRef}
          className="min-w-0 flex-1 bg-transparent outline-none"
          placeholder="name@example.com"
          value={draft.to}
          onChange={(e) => onChange({ to: e.target.value })}
        />
        {!draft.showCc && <TextToggle onClick={() => onChange({ showCc: true })}>Cc</TextToggle>}
        {!draft.showBcc && <TextToggle onClick={() => onChange({ showBcc: true })}>Bcc</TextToggle>}
      </Field>
      {draft.showCc && (
        <Field label="Cc">
          <input
            autoFocus
            className="min-w-0 flex-1 bg-transparent outline-none"
            value={draft.cc}
            onChange={(e) => onChange({ cc: e.target.value })}
          />
        </Field>
      )}
      {draft.showBcc && (
        <Field label="Bcc">
          <input
            autoFocus
            className="min-w-0 flex-1 bg-transparent outline-none"
            value={draft.bcc}
            onChange={(e) => onChange({ bcc: e.target.value })}
          />
        </Field>
      )}
      <Field>
        <input
          className="min-w-0 flex-1 bg-transparent font-medium outline-none placeholder:font-normal placeholder:text-text-3"
          placeholder="Subject"
          value={draft.subject}
          onChange={(e) => onChange({ subject: e.target.value })}
          onKeyDown={(e) => {
            if (e.key === "Enter") {
              e.preventDefault();
              bodyRef.current?.focus();
            }
          }}
        />
      </Field>
      <textarea
        ref={bodyRef}
        className="min-h-0 flex-1 resize-none bg-transparent px-4 py-3 text-sm leading-[1.57] outline-none placeholder:text-text-3"
        placeholder="Enter text"
        value={draft.body}
        onChange={(e) => onChange({ body: e.target.value })}
      />

      {(draft.sendLater || draft.remind) && (
        <div className="flex shrink-0 flex-wrap gap-1.5 px-3 pb-2">
          {draft.sendLater && (
            <Chip onClear={() => onChange({ sendLater: null })}>
              <CalendarClock size={13} /> Send {presetText(draft.sendLater)}
            </Chip>
          )}
          {draft.remind && (
            <Chip onClear={() => onChange({ remind: null })}>
              <BellRing size={13} /> Remind me {presetText(draft.remind)}
            </Chip>
          )}
        </div>
      )}

      <div className="flex h-12 shrink-0 items-center gap-0.5 border-t border-hairline px-2">
        <IconButton title={`Attach files (${IS_MAC ? "⇧⌘A" : "Ctrl+Shift+A"})`} onClick={() => {}}>
          <Paperclip size={17} />
        </IconButton>
        <Dropdown
          direction="up"
          width="w-64"
          trigger={({ toggle, open }) => (
            <IconButton title="Send later" active={open || draft.sendLater !== null} onClick={toggle}>
              <CalendarClock size={17} />
            </IconButton>
          )}
        >
          <li className="menu-title px-2 py-1 text-[11px]">Send later</li>
          {LATER_PRESETS.map((p) => (
            <MenuItem
              key={p.label}
              label={p.label}
              detail={p.detail}
              checked={draft.sendLater?.label === p.label}
              onSelect={() => onChange({ sendLater: p })}
            />
          ))}
          {draft.sendLater && <MenuItem label="Don't schedule" onSelect={() => onChange({ sendLater: null })} />}
        </Dropdown>
        <Dropdown
          direction="up"
          width="w-64"
          trigger={({ toggle, open }) => (
            <IconButton title="Remind me" active={open || draft.remind !== null} onClick={toggle}>
              <BellRing size={17} />
            </IconButton>
          )}
        >
          <li className="menu-title px-2 py-1 text-[11px]">Remind me if no reply</li>
          {LATER_PRESETS.map((p) => (
            <MenuItem
              key={p.label}
              label={p.label}
              detail={p.detail}
              checked={draft.remind?.label === p.label}
              onSelect={() => onChange({ remind: p })}
            />
          ))}
          {draft.remind && <MenuItem label="No reminder" onSelect={() => onChange({ remind: null })} />}
        </Dropdown>
        <IconButton
          title="Mark as Done after sending"
          active={draft.doneAfter}
          onClick={() => onChange({ doneAfter: !draft.doneAfter })}
        >
          <CircleCheck size={17} />
        </IconButton>

        <div className="ml-auto flex items-center gap-1.5">
          <Dropdown
            direction="up"
            align="end"
            width="w-60"
            trigger={({ toggle }) => (
              <button
                className="flex h-9 items-center gap-1.5 rounded-lg px-2 text-left hover:bg-row-hover"
                title="From"
                onClick={toggle}
              >
                <span className={`size-2 rounded-full ${account.dot}`} />
                <span className="leading-none">
                  <span className="block text-xs">{account.email}</span>
                  <span className="block text-[8px] font-semibold uppercase tracking-wide text-text-2">
                    {account.name}
                  </span>
                </span>
                <ChevronsUpDown size={12} className="text-text-2" />
              </button>
            )}
          >
            {ACCOUNT_IDS.map((id) => (
              <MenuItem
                key={id}
                label={ACCOUNTS[id].email}
                detail={ACCOUNTS[id].name}
                checked={draft.from === id}
                onSelect={() => onChange({ from: id })}
              />
            ))}
          </Dropdown>
          <button
            className="btn btn-primary btn-sm px-5"
            title={`${sendLabel} (${IS_MAC ? "⌘Enter" : "Ctrl+Enter"})`}
            onClick={onSend}
          >
            {sendLabel}
          </button>
        </div>
      </div>
    </div>
  );
}

function Field({ label, children }: { label?: string; children: ReactNode }) {
  return (
    <div className="flex h-10 shrink-0 items-center gap-2 border-b border-hairline px-4 text-sm">
      {label && <span className="w-8 shrink-0 text-text-2">{label}</span>}
      {children}
    </div>
  );
}

function TextToggle({ onClick, children }: { onClick: () => void; children: ReactNode }) {
  return (
    <button className="rounded px-1 text-xs text-text-2 hover:text-base-content" onClick={onClick}>
      {children}
    </button>
  );
}

function IconButton({
  title,
  active = false,
  onClick,
  children,
}: {
  title: string;
  active?: boolean;
  onClick: () => void;
  children: ReactNode;
}) {
  return (
    <button
      title={title}
      aria-label={title}
      aria-pressed={active || undefined}
      onClick={onClick}
      className={`grid size-8 place-items-center rounded-lg ${
        active ? "bg-primary/10 text-primary" : "text-text-2 hover:bg-base-content/10 hover:text-base-content"
      }`}
    >
      {children}
    </button>
  );
}

function Chip({ onClear, children }: { onClear: () => void; children: ReactNode }) {
  return (
    <span className="inline-flex h-6 items-center gap-1 rounded-full bg-primary/10 pl-2 pr-1 text-xs text-primary">
      {children}
      <button aria-label="Remove" className="grid size-4 place-items-center rounded-full hover:bg-primary/15" onClick={onClear}>
        <X size={11} />
      </button>
    </span>
  );
}
