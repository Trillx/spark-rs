import {
  BellRing,
  CalendarClock,
  ChevronsUpDown,
  CircleCheck,
  Maximize2,
  Minimize2,
  Paperclip,
  Trash2,
  X,
} from "lucide-react";
import { useEffect, useRef, useState, type CSSProperties, type KeyboardEvent, type ReactNode } from "react";
import { describeFiles, onFileDrag, pickFiles } from "../api";
import type { AccountId, LocalFile, OutgoingAttachment, OutgoingDraft } from "../types";
import { useAccounts } from "../lib/accounts";
import { LATER_PRESETS, formatBytes, formatLater, pickLater, presetDetail, type Later } from "../lib/format";
import { IS_MAC } from "../lib/shortcuts";
import { AttachmentChip } from "./Attachments";
import Dropdown, { MenuItem } from "./Dropdown";
import { tipAttrs, tipLabel, type Tip } from "../lib/tooltip";

/** What mail servers take; the Rust side refuses more. */
const MAX_ATTACHMENT_BYTES = 25 * 1024 * 1024;
const ATTACH_KEYS = IS_MAC ? "⇧⌘A" : "Ctrl+Shift+A";

export interface Draft {
  title: string;
  to: string;
  cc: string;
  bcc: string;
  showCc: boolean;
  showBcc: boolean;
  subject: string;
  body: string;
  from: AccountId;
  sendLater: Later | null;
  remind: Later | null;
  doneAfter: boolean;
  /** The message being replied to or forwarded; "Mark as Done after sending" applies to it. */
  replyTo?: number;
  /** `replyTo` is forwarded, not answered, so the message isn't sent as a reply. */
  forward?: boolean;
  /** Files from disk, and a forwarded message's own attachments. */
  attachments: OutgoingAttachment[];
  /** The saved draft this edits (opened from Drafts, or saved before). */
  draftId?: number;
  /** A reply's threading when its original isn't stored here. */
  inReplyTo?: string;
  references?: string;
}

export function emptyDraft(from: AccountId): Draft {
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
    attachments: [],
  };
}

export function draftHasContent(d: Draft) {
  return [d.to, d.cc, d.bcc, d.subject, d.body].some((s) => s.trim()) || d.attachments.length > 0;
}

/** What a saved draft keeps, to tell whether there's anything new to save. */
export function draftKey(d: Draft) {
  return JSON.stringify([d.from, d.to, d.cc, d.bcc, d.subject, d.body, d.attachments]);
}

/** The draft as the backend takes it, to save or to send at `sendAt`. */
export function outgoingDraft(d: Draft, sendAt: number, draftId = d.draftId): OutgoingDraft {
  return {
    account: d.from,
    to: d.to,
    cc: d.cc,
    bcc: d.bcc,
    subject: d.subject,
    body: d.body,
    // A forward isn't a reply; it only carries replyTo for "Mark as Done after sending".
    replyTo: d.forward ? undefined : d.replyTo,
    sendAt,
    attachments: d.attachments,
    draftId,
    inReplyTo: d.inReplyTo,
    references: d.references,
  };
}

/** A draft from Drafts, back in the composer. */
export function savedDraft(saved: OutgoingDraft): Draft {
  return {
    ...emptyDraft(saved.account),
    title: "Draft",
    to: saved.to,
    cc: saved.cc,
    bcc: saved.bcc,
    showCc: saved.cc.trim() !== "",
    showBcc: saved.bcc.trim() !== "",
    subject: saved.subject,
    body: saved.body,
    replyTo: saved.replyTo,
    attachments: saved.attachments,
    draftId: saved.draftId,
    inReplyTo: saved.inReplyTo,
    references: saved.references,
  };
}

/**
 * Compact floating composer, with a toggle that fills the reading pane.
 * Plain text only for now; the rich editor comes later.
 */
export default function Composer({
  draft,
  saved = false,
  full,
  fullStyle,
  onChange,
  onToggleFull,
  onClose,
  onDiscard,
  onSend,
  onError,
}: {
  draft: Draft;
  /** It's in Drafts as it stands. */
  saved?: boolean;
  full: boolean;
  /** Where the full-pane composer sits (the reading pane's box). */
  fullStyle: CSSProperties;
  onChange: (patch: Partial<Draft>) => void;
  onToggleFull: () => void;
  /** Closing keeps it in Drafts. */
  onClose: () => void;
  onDiscard: () => void;
  onSend: () => void;
  onError: (text: string) => void;
}) {
  const toRef = useRef<HTMLInputElement>(null);
  const bodyRef = useRef<HTMLTextAreaElement>(null);
  const [dropping, setDropping] = useState(false);
  // File drops arrive through a listener set up once; it reads these.
  const latest = useRef({ draft, onChange });
  latest.current = { draft, onChange };

  function addFiles(files: LocalFile[]) {
    const { draft: current, onChange: change } = latest.current;
    const fresh = files.filter((f) => !current.attachments.some((a) => a.kind === "file" && a.path === f.path));
    if (fresh.length) change({ attachments: [...current.attachments, ...fresh.map((f) => ({ kind: "file" as const, ...f }))] });
  }

  async function attach() {
    try {
      addFiles(await pickFiles());
    } catch (error) {
      onError(String(error));
    }
  }

  useEffect(
    () =>
      onFileDrag((e) => {
        if (e.type === "drop") {
          setDropping(false);
          void describeFiles(e.paths).then(addFiles);
        } else {
          setDropping(e.type === "over");
        }
      }),
    [],
  );

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
    } else if (mod && e.shiftKey && e.key.toLowerCase() === "a") {
      e.preventDefault();
      void attach();
    }
  }

  const attachedBytes = draft.attachments.reduce((sum, a) => sum + a.size, 0);

  const sendLabel = draft.sendLater ? "Schedule" : "Send";
  const accounts = useAccounts();
  const account = accounts.find((a) => a.id === draft.from) ?? accounts[0];

  return (
    <div
      data-composer
      role="dialog"
      aria-label={draft.title}
      onKeyDown={onKeyDown}
      style={full ? fullStyle : undefined}
      className={`z-50 flex flex-col overflow-hidden border bg-base-100 transition-colors ${dropping ? "border-primary" : "border-hairline"} ${
        full
          ? "fixed bottom-0 top-8 rounded-tl-xl"
          : "fixed bottom-4 right-4 h-[min(540px,calc(100vh-64px))] w-[560px] max-w-[calc(100vw-32px)] rounded-xl shadow-popup"
      }`}
    >
      <div className="flex h-10 shrink-0 items-center gap-1 border-b border-hairline pl-4 pr-1.5">
        <span className="flex-1 truncate text-[13px] font-semibold">
          {draft.title}
          {saved && <span className="ml-2 text-xs font-normal text-text-3">Saved</span>}
        </span>
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

      {draft.attachments.length > 0 && (
        <div className="shrink-0 px-3 pb-2">
          <ul aria-label="Attachments" className="flex max-h-[84px] flex-wrap gap-1.5 overflow-y-auto">
            {draft.attachments.map((a, i) => (
              <AttachmentChip
                key={a.kind === "file" ? a.path : `${a.message}:${a.index}`}
                name={a.name}
                size={a.size}
                onRemove={() => onChange({ attachments: draft.attachments.filter((_, j) => j !== i) })}
              />
            ))}
          </ul>
          {attachedBytes > MAX_ATTACHMENT_BYTES && (
            <p className="mt-1.5 text-xs text-error">
              The attachments add up to {formatBytes(attachedBytes)}; mail servers take up to 25 MB.
            </p>
          )}
        </div>
      )}

      {dropping && (
        <div className="pointer-events-none absolute inset-0 z-10 grid place-items-center rounded-[inherit] border-2 border-dashed border-primary bg-primary/10">
          <div className="flex flex-col items-center gap-2 rounded-2xl bg-base-100/90 px-6 py-4 text-primary shadow-popup">
            <Paperclip size={26} className="rotate-45" />
            <span className="text-sm font-semibold">Drop files to attach</span>
          </div>
        </div>
      )}

      {(draft.sendLater || draft.remind) && (
        <div className="flex shrink-0 flex-wrap gap-1.5 px-3 pb-2">
          {draft.sendLater && (
            <Chip onClear={() => onChange({ sendLater: null })}>
              <CalendarClock size={13} /> Send {formatLater(draft.sendLater.at)}
            </Chip>
          )}
          {draft.remind && (
            <Chip onClear={() => onChange({ remind: null })}>
              <BellRing size={13} /> Remind me {formatLater(draft.remind.at)}
            </Chip>
          )}
        </div>
      )}

      <div className="flex h-12 shrink-0 items-center gap-0.5 border-t border-hairline px-2">
        <IconButton
          title={{ label: "Attach files", keys: ATTACH_KEYS, card: "attach" }}
          active={draft.attachments.length > 0}
          onClick={() => void attach()}
        >
          <Paperclip size={17} />
        </IconButton>
        <Dropdown
          direction="up"
          width="w-64"
          trigger={({ toggle, open }) => (
            <IconButton
              title={{ label: "Send later", card: open ? undefined : "sendLater" }}
              active={open || draft.sendLater !== null}
              onClick={toggle}
            >
              <CalendarClock size={17} />
            </IconButton>
          )}
        >
          <li className="menu-title px-2 py-1 text-[11px]">Send later</li>
          {LATER_PRESETS.map((p) => (
            <MenuItem
              key={p.key}
              label={p.label}
              detail={presetDetail(p)}
              checked={draft.sendLater?.key === p.key}
              onSelect={() => onChange({ sendLater: pickLater(p) })}
            />
          ))}
          {draft.sendLater && <MenuItem label="Don't schedule" onSelect={() => onChange({ sendLater: null })} />}
        </Dropdown>
        <Dropdown
          direction="up"
          width="w-64"
          trigger={({ toggle, open }) => (
            <IconButton
              title={{ label: "Remind me", card: open ? undefined : "remind" }}
              active={open || draft.remind !== null}
              onClick={toggle}
            >
              <BellRing size={17} />
            </IconButton>
          )}
        >
          <li className="menu-title px-2 py-1 text-[11px]">Remind me if no reply</li>
          {LATER_PRESETS.map((p) => (
            <MenuItem
              key={p.key}
              label={p.label}
              detail={presetDetail(p)}
              checked={draft.remind?.key === p.key}
              onSelect={() => onChange({ remind: pickLater(p) })}
            />
          ))}
          {draft.remind && <MenuItem label="No reminder" onSelect={() => onChange({ remind: null })} />}
        </Dropdown>
        <IconButton
          title={{ label: "Mark as Done after sending", card: "doneAfter" }}
          active={draft.doneAfter}
          onClick={() => onChange({ doneAfter: !draft.doneAfter })}
        >
          <CircleCheck size={17} />
        </IconButton>

        <div className="ml-auto flex items-center gap-1.5">
          <IconButton title="Delete draft" onClick={onDiscard}>
            <Trash2 size={17} />
          </IconButton>
          <Dropdown
            direction="up"
            align="end"
            width="w-60"
            trigger={({ toggle }) => (
              <button
                className="flex h-9 items-center gap-1.5 rounded-lg px-2 text-left hover:bg-row-hover"
                data-tip="Send from"
                onClick={toggle}
              >
                <span className="size-2 rounded-full" style={{ backgroundColor: account?.color }} />
                <span className="leading-none">
                  <span className="block text-xs">{account?.email}</span>
                  <span className="block text-[8px] font-semibold uppercase tracking-wide text-text-2">
                    {account?.label}
                  </span>
                </span>
                <ChevronsUpDown size={12} className="text-text-2" />
              </button>
            )}
          >
            {accounts.map((a) => (
              <MenuItem
                key={a.id}
                label={a.email}
                detail={a.label}
                checked={account?.id === a.id}
                onSelect={() => onChange({ from: a.id })}
              />
            ))}
          </Dropdown>
          <button
            className="btn btn-primary btn-sm px-5"
            {...tipAttrs({ label: sendLabel, keys: IS_MAC ? "⌘Enter" : "Ctrl+Enter" })}
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
  title: Tip;
  active?: boolean;
  onClick: () => void;
  children: ReactNode;
}) {
  return (
    <button
      {...tipAttrs(title)}
      aria-label={tipLabel(title)}
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
