import {
  AlarmClock,
  CalendarClock,
  ChevronsUpDown,
  CircleCheck,
  Maximize2,
  Minimize2,
  Paperclip,
  Signature as SignatureIcon,
  Square,
  Trash2,
  Undo2,
  X,
} from "lucide-react";
import {
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type ChangeEvent,
  type CSSProperties,
  type KeyboardEvent,
  type ReactNode,
  type RefObject,
} from "react";
import { describeFiles, getPrefs, onFileDrag, pickFiles, resolveSignature } from "../api";
import type { Contact, LocalFile, Signature } from "../types";
import type { Draft } from "../lib/composeDraft";
import { useAccounts } from "../lib/accounts";
import {
  completeMention,
  entryFor,
  joinAddresses,
  mentionAt,
  parseEntry,
  splitAddresses,
  type Mention,
} from "../lib/addresses";
import { caretPoint } from "../lib/caret";
import { formatBytes, formatLater, laterPresets, pickLater, presetDetail } from "../lib/format";
import { IS_MAC } from "../lib/shortcuts";
import AiMark from "./AiMark";
import AiMenu, { type AiRequest } from "./AiMenu";
import { DateTimeDialog } from "./Dialog";
import { AttachmentChip } from "./Attachments";
import Dropdown, { MenuItem } from "./Dropdown";
import Recipients, { SuggestionList, displayName, useSuggestions, type RecipientsHandle } from "./Recipients";
import { tipAttrs, tipLabel, type Tip } from "../lib/tooltip";
import SignatureFrame, { type SignatureFrameHandle } from "./SignatureFrame";
import SignaturePicker from "./SignaturePicker";

export { emptyDraft, draftHasContent, draftKey, outgoingDraft, savedDraft, type Draft } from "../lib/composeDraft";

/** What mail servers take; the Rust side refuses more. */
const MAX_ATTACHMENT_BYTES = 25 * 1024 * 1024;
const ATTACH_KEYS = IS_MAC ? "⇧⌘A" : "Ctrl+Shift+A";
/** How many people an @mention suggests, as in Spark. */
const MENTION_SUGGESTIONS = 6;

interface MentionEdit {
  before: string;
  after: string;
  added: Contact | null;
}

/**
 * AI working on the body: under way, failed, waiting for a provider to be set
 * up, or done replacing text that Undo brings back.
 */
export type Writing =
  | { status: "writing" }
  | { status: "error"; message: string }
  | { status: "setup"; message: string }
  | { status: "done"; message: string };

/** The composer's +AI: its menu, and what AI is doing with the body. */
export interface ComposerAi {
  writing: Writing | null;
  /** Offer the quick-reply chips while the body is empty. */
  quickReplies: boolean;
  run: (request: AiRequest) => void;
  stop: () => void;
  retry: () => void;
  undo: () => void;
  dismiss: () => void;
  setUp: () => void;
}

/**
 * Compact floating composer, with a toggle that fills the reading pane.
 * The body is plain text for now (the rich editor comes later); under it
 * are the signature, which is HTML, and a forward's original.
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
  onSignatureSettings,
  ai,
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
  /** Settings → Signatures. */
  onSignatureSettings: () => void;
  /** While AI writes the body, it's read-only and can't be sent. */
  ai?: ComposerAi;
}) {
  const recipientsRef = useRef<RecipientsHandle>(null);
  const bodyRef = useRef<HTMLTextAreaElement>(null);
  const quoteRef = useRef<HTMLTextAreaElement>(null);
  const signatureRef = useRef<SignatureFrameHandle>(null);
  const [dropping, setDropping] = useState(false);
  /** Picking a day and time for Send Later or Remind me. */
  const [picking, setPicking] = useState<"sendLater" | "remind" | null>(null);
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

  // The signature for the From account: its default, or for a reply the one
  // used last in the conversation. Choosing again replaces one edited here.
  useEffect(() => {
    const from = draft.from;
    if (draft.signatureFor === from) return;
    resolveSignature(from, draft.forward ? undefined : draft.replyTo).then(
      (s) => {
        if (latest.current.draft.from !== from) return;
        latest.current.onChange({ signature: s?.html ?? null, signatureId: s?.id ?? null, signatureFor: from });
      },
      () => latest.current.onChange({ signatureFor: from }),
    );
  }, [draft.from, draft.signatureFor]);

  function pickSignature(s: Signature) {
    onChange({ signature: s.html, signatureId: s.id, signatureHidden: false, signatureFor: draft.from });
    // Once the frame has the new signature.
    window.setTimeout(() => signatureRef.current?.focus(), 50);
  }

  // Replies arrive with a recipient, so they start in the body; anything else starts in To.
  const [startInTo] = useState(() => !draft.to);
  useEffect(() => {
    if (startInTo) return;
    bodyRef.current?.focus();
    bodyRef.current?.setSelectionRange(draft.body.length, draft.body.length);
  }, []); // only when the composer opens

  const [suggest, setSuggest] = useState(true);
  useEffect(() => {
    getPrefs().then(
      (p) => setSuggest(p.suggestRecipients),
      () => {},
    );
  }, []);

  function onKeyDown(e: KeyboardEvent) {
    // The day-and-time dialog sits inside the composer: its keys are its own.
    if (picking) return;
    const mod = IS_MAC ? e.metaKey : e.ctrlKey;
    const key = e.key.toLowerCase();
    if (mod && e.key === "Enter") {
      e.preventDefault();
      if (!busy) onSend();
    } else if (mod && e.shiftKey && key === "a") {
      e.preventDefault();
      void attach();
    } else if (mod && e.shiftKey && (key === "c" || key === "b")) {
      e.preventDefault();
      recipientsRef.current?.focus(key === "c" ? "cc" : "bcc");
    }
  }

  // @mentions in the body: picking someone puts their name in the text and adds them to To.
  const [mention, setMention] = useState<Mention | null>(null);
  const mentionHistory = useRef<{ undo: MentionEdit[]; redo: MentionEdit[] }>({ undo: [], redo: [] });
  const mentioned = useSuggestions(mention?.query ?? "", MENTION_SUGGESTIONS);
  const [mentionAnchor, setMentionAnchor] = useState<CSSProperties | null>(null);

  function trackMention(el: HTMLTextAreaElement) {
    const caret = el.selectionStart;
    setMention(caret === el.selectionEnd ? mentionAt(el.value, caret) : null);
  }

  useLayoutEffect(() => {
    const el = bodyRef.current;
    if (!el || !mention || mentioned.list.length === 0) return setMentionAnchor(null);
    const point = caretPoint(el, mention.start);
    const left = Math.max(8, Math.min(point.left - 12, window.innerWidth - 408));
    // Under the line, or over it when there's no room below.
    setMentionAnchor(
      point.top + point.height + 240 < window.innerHeight
        ? { position: "fixed", left, top: point.top + point.height + 4 }
        : { position: "fixed", left, bottom: window.innerHeight - point.top + 4 },
    );
  }, [mention, mentioned.list.length]);

  function pickMention(contact: Contact) {
    const el = bodyRef.current;
    if (!el || !mention) return;
    const done = completeMention(draft.body, mention, el.selectionStart, displayName(contact));
    const address = contact.address.toLowerCase();
    const listed = [draft.to, draft.cc, draft.bcc]
      .flatMap(splitAddresses)
      .some((e) => parseEntry(e).address.toLowerCase() === address);
    mentionHistory.current.undo.push({ before: draft.body, after: done.text, added: listed ? null : contact });
    mentionHistory.current.redo = [];
    // Use the editor's insertion command so the mention is one native undo step.
    const end = draft.body.length - (done.text.length - done.caret);
    el.focus();
    el.setSelectionRange(mention.start, end);
    document.execCommand("insertText", false, done.text.slice(mention.start, done.caret));
    onChange({
      body: done.text,
      ...(listed ? {} : { to: joinAddresses([...splitAddresses(draft.to), entryFor(contact)]) }),
    });
    setMention(null);
    requestAnimationFrame(() => el.setSelectionRange(done.caret, done.caret));
  }

  function onBodyChange(e: ChangeEvent<HTMLTextAreaElement>) {
    if (writing?.status === "done") ai?.dismiss();
    const body = e.target.value;
    const patch: Partial<Draft> = { body };
    const inputType = (e.nativeEvent as InputEvent).inputType;
    const history = mentionHistory.current;
    const undo = history.undo.at(-1);
    const redo = history.redo.at(-1);
    if (inputType === "historyUndo" && undo && (body === undo.before || draft.body === undo.after)) {
      history.undo.pop();
      history.redo.push(undo);
      if (undo.added) {
        const address = undo.added.address.toLowerCase();
        patch.to = joinAddresses(splitAddresses(draft.to).filter((entry) => parseEntry(entry).address.toLowerCase() !== address));
      }
    } else if (inputType === "historyRedo" && redo && (body === redo.after || draft.body === redo.before)) {
      history.redo.pop();
      history.undo.push(redo);
      if (redo.added) {
        const address = redo.added.address.toLowerCase();
        const listed = [draft.to, draft.cc, draft.bcc].flatMap(splitAddresses)
          .some((entry) => parseEntry(entry).address.toLowerCase() === address);
        if (!listed) patch.to = joinAddresses([...splitAddresses(draft.to), entryFor(redo.added)]);
      }
    } else if (inputType !== "historyUndo" && inputType !== "historyRedo") {
      history.redo = [];
    }
    onChange(patch);
    trackMention(e.target);
  }

  function onBodyKeyDown(e: KeyboardEvent<HTMLTextAreaElement>) {
    const list = mentioned.list;
    if (e.nativeEvent.isComposing || list.length === 0 || e.ctrlKey || e.metaKey || e.altKey) return;
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      mentioned.move(e.key === "ArrowDown" ? 1 : -1, list.length);
    } else if ((e.key === "Enter" || e.key === "Tab") && !e.shiftKey) {
      e.preventDefault();
      pickMention(list[Math.min(mentioned.active, list.length - 1)]);
    } else if (e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      mentioned.dismiss();
    }
  }

  const attachedBytes = draft.attachments.reduce((sum, a) => sum + a.size, 0);
  const showSignature = draft.signature !== null && !draft.signatureHidden;

  // The body and a forward's original grow with their text; the area they're in scrolls.
  const scrollRef = useRef<HTMLDivElement>(null);
  useLayoutEffect(() => {
    fit(bodyRef);
    fit(quoteRef);
  }, [draft.body, draft.quote, showSignature, full]);
  useEffect(() => {
    const area = scrollRef.current;
    if (!area) return;
    const observer = new ResizeObserver(() => {
      fit(bodyRef);
      fit(quoteRef);
    });
    observer.observe(area);
    return () => observer.disconnect();
  }, []);

  const writing = ai?.writing ?? null;
  const busy = writing?.status === "writing";
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

      <Recipients
        ref={recipientsRef}
        values={draft}
        suggest={suggest}
        autoFocus={startInTo}
        onChange={onChange}
      />
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
      <div
        ref={scrollRef}
        className="min-h-0 flex-1 overflow-y-auto"
        onMouseDown={(e) => {
          // Below everything is still the body's.
          if (e.target !== e.currentTarget) return;
          e.preventDefault();
          const body = bodyRef.current;
          body?.focus();
          body?.setSelectionRange(body.value.length, body.value.length);
        }}
      >
        <textarea
          ref={bodyRef}
          aria-label="Message"
          className={`block w-full resize-none overflow-hidden bg-transparent px-4 pb-2 pt-3 text-sm leading-[1.57] outline-none placeholder:text-text-3 ${
            showSignature || draft.quote !== undefined ? "min-h-[4.5rem]" : "min-h-full"
          }`}
          placeholder={busy ? "" : "Enter text"}
          value={draft.body}
          readOnly={busy}
          aria-busy={busy}
          onChange={onBodyChange}
          onSelect={(e) => trackMention(e.currentTarget)}
          onKeyDown={onBodyKeyDown}
          onBlur={() => setMention(null)}
        />
        {mentionAnchor && (
          <SuggestionList
            className="w-[400px]"
            style={mentionAnchor}
            contacts={mentioned.list}
            active={Math.min(mentioned.active, mentioned.list.length - 1)}
            onHover={mentioned.setActive}
            onPick={pickMention}
            onNever={mentioned.never}
          />
        )}
        {showSignature && (
          <div className="relative px-4 pb-3" data-signature>
            <SignaturePicker
              currentId={draft.signatureId}
              onPick={pickSignature}
              onHide={() => onChange({ signatureHidden: true })}
              onSettings={onSignatureSettings}
              trigger={({ open, toggle }) => (
                <button
                  aria-label="Choose signature"
                  data-tip="Choose signature"
                  aria-expanded={open}
                  onClick={toggle}
                  className={`absolute left-0.5 top-0.5 grid size-4 place-items-center rounded ${
                    open ? "text-primary" : "text-text-3 hover:text-base-content"
                  }`}
                >
                  <ChevronsUpDown size={12} />
                </button>
              )}
            />
            <SignatureFrame
              ref={signatureRef}
              html={draft.signature ?? ""}
              placeholder="No signature"
              onInput={(html) => onChange({ signature: html })}
              onError={onError}
            />
          </div>
        )}
        {draft.quote !== undefined && (
          <textarea
            ref={quoteRef}
            aria-label="Forwarded message"
            className="block w-full resize-none overflow-hidden bg-transparent px-4 pb-3 text-sm leading-[1.57] outline-none"
            value={draft.quote}
            onChange={(e) => onChange({ quote: e.target.value })}
          />
        )}
      </div>

      {ai && writing && (
        <div
          aria-live="polite"
          className={`mx-3 mb-2 flex min-h-9 shrink-0 items-center gap-2 rounded-lg px-3 py-1 text-xs ${
            writing.status === "error" ? "bg-error/10 text-error" : "bg-ai-tint"
          }`}
        >
          {writing.status === "writing" ? (
            <>
              <AiMark className="animate-pulse text-sm" />
              <span className="flex-1 text-text-2">Generating…</span>
              <button className="btn btn-ghost btn-xs h-6 gap-1 px-2 font-medium" onClick={ai.stop}>
                <Square size={10} className="fill-current" />
                Stop
              </button>
            </>
          ) : writing.status === "done" ? (
            <>
              <AiMark className="text-sm" />
              <span className="min-w-0 flex-1 text-text-2">{writing.message}</span>
              <button className="btn btn-ghost btn-xs h-6 gap-1 px-2 font-medium" onClick={ai.undo}>
                <Undo2 size={12} />
                Undo
              </button>
              <button aria-label="Close" className="btn btn-ghost btn-xs btn-square h-6" onClick={ai.dismiss}>
                <X size={13} />
              </button>
            </>
          ) : (
            <>
              <span className="min-w-0 flex-1 select-text">{writing.message}</span>
              {writing.status === "setup" ? (
                <button className="btn btn-primary btn-xs h-6 px-2 font-medium" onClick={ai.setUp}>
                  Set up AI
                </button>
              ) : (
                <button className="btn btn-ghost btn-xs h-6 px-2 font-medium" onClick={ai.retry}>
                  Try again
                </button>
              )}
              <button className="btn btn-ghost btn-xs h-6 px-2 font-medium" onClick={ai.dismiss}>
                Close
              </button>
            </>
          )}
        </div>
      )}

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
              <AlarmClock size={13} /> Remind me {formatLater(draft.remind.at)}
              {draft.remind.ifNoReply && " if no reply"}
            </Chip>
          )}
        </div>
      )}
      {picking && (
        <DateTimeDialog
          title={picking === "sendLater" ? "Send at" : "Remind me at"}
          confirmLabel={picking === "sendLater" ? "Schedule" : "Remind me"}
          initial={(picking === "sendLater" ? draft.sendLater?.at : draft.remind?.at) ?? Date.now() + 60 * 60_000}
          onSubmit={(at) =>
            picking === "sendLater"
              ? onChange({ sendLater: { key: "custom", at } })
              : onChange({ remind: { key: "custom", at, ifNoReply: draft.remind?.ifNoReply ?? true } })
          }
          onClose={() => setPicking(null)}
        />
      )}

      <div className="flex h-12 shrink-0 items-center gap-0.5 border-t border-hairline px-2">
        {ai && (
          <AiMenu body={draft.body} bodyRef={bodyRef} quickReplies={ai.quickReplies} disabled={busy} onRun={ai.run} />
        )}
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
          {laterPresets().map((p) => (
            <MenuItem
              key={p.key}
              label={p.label}
              detail={presetDetail(p)}
              checked={draft.sendLater?.key === p.key}
              onSelect={() => onChange({ sendLater: pickLater(p) })}
            />
          ))}
          <MenuItem
            label="Pick date & time…"
            checked={draft.sendLater?.key === "custom"}
            onSelect={() => setPicking("sendLater")}
          />
          {draft.sendLater && <MenuItem label="Don't schedule" onSelect={() => onChange({ sendLater: null })} />}
        </Dropdown>
        <Dropdown
          direction="up"
          width="w-64"
          trigger={({ toggle, open }) => (
            <IconButton title="Remind me" active={open || draft.remind !== null} onClick={toggle}>
              <AlarmClock size={17} />
            </IconButton>
          )}
        >
          <li className="menu-title px-2 py-1 text-[11px]">Remind me</li>
          {laterPresets().map((p) => (
            <MenuItem
              key={p.key}
              label={p.label}
              detail={presetDetail(p)}
              checked={draft.remind?.key === p.key}
              onSelect={() => onChange({ remind: { ...pickLater(p), ifNoReply: draft.remind?.ifNoReply ?? true } })}
            />
          ))}
          <MenuItem
            label="Pick date & time…"
            checked={draft.remind?.key === "custom"}
            onSelect={() => setPicking("remind")}
          />
          <li role="none" className="my-0.5 h-px bg-hairline" />
          <MenuItem
            label="Only if nobody replies"
            checked={draft.remind?.ifNoReply ?? true}
            onSelect={() => draft.remind && onChange({ remind: { ...draft.remind, ifNoReply: !draft.remind.ifNoReply } })}
          />
          {draft.remind && <MenuItem label="Don't remind me" onSelect={() => onChange({ remind: null })} />}
        </Dropdown>
        <IconButton
          title={{ label: "Mark as Done after sending", card: "doneAfter" }}
          active={draft.doneAfter}
          onClick={() => onChange({ doneAfter: !draft.doneAfter })}
        >
          <CircleCheck size={17} />
        </IconButton>
        {!showSignature &&
          (draft.signature !== null ? (
            <IconButton title="Add signature" onClick={() => onChange({ signatureHidden: false })}>
              <SignatureIcon size={17} />
            </IconButton>
          ) : (
            <SignaturePicker
              direction="up"
              currentId={null}
              onPick={pickSignature}
              onSettings={onSignatureSettings}
              trigger={({ open, toggle }) => (
                <IconButton title="Add signature" active={open} onClick={toggle}>
                  <SignatureIcon size={17} />
                </IconButton>
              )}
            />
          ))}

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
            disabled={busy}
            onClick={onSend}
          >
            {sendLabel}
          </button>
        </div>
      </div>
    </div>
  );
}

/** Sizes a textarea to its text. */
function fit(ref: RefObject<HTMLTextAreaElement | null>) {
  const el = ref.current;
  if (!el) return;
  el.style.height = "0px";
  el.style.height = `${el.scrollHeight}px`;
}

function Field({ label, children }: { label?: string; children: ReactNode }) {
  return (
    <div className="flex h-10 shrink-0 items-center gap-2 border-b border-hairline px-4 text-sm">
      {label && <span className="w-8 shrink-0 text-text-2">{label}</span>}
      {children}
    </div>
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
