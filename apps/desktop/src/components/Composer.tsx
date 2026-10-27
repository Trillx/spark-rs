import {
  AlarmClock,
  CalendarClock,
  ChevronsUpDown,
  CircleCheck,
  ImageDown,
  LayoutTemplate,
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
  type CSSProperties,
  type KeyboardEvent,
  type ReactNode,
  type RefObject,
} from "react";
import {
  describeFiles,
  getPrefs,
  listTemplates,
  onFileDrag,
  pickFiles,
  readImage,
  resolveSignature,
  suggestContacts,
  templateUsed,
  type DragPoint,
} from "../api";
import type { Contact, LocalFile, Signature, Template } from "../types";
import type { Draft } from "../lib/composeDraft";
import { useAccounts } from "../lib/accounts";
import { entryFor, joinAddresses, parseEntry, splitAddresses } from "../lib/addresses";
import { formatBytes, formatLater, laterPresets, pickLater, presetDetail } from "../lib/format";
import { IMAGE_SIZES, fillPlaceholders, textToHtml, type ImageSize } from "../lib/richText";
import { placeholderValues, recentTemplates } from "../lib/templates";
import { IS_MAC } from "../lib/shortcuts";
import AiMark from "./AiMark";
import AiMenu, { type AiRequest } from "./AiMenu";
import { DateTimeDialog } from "./Dialog";
import { AttachmentChip } from "./Attachments";
import Dropdown, { MenuItem } from "./Dropdown";
import Recipients, { SuggestionList, displayName, useSuggestions, type RecipientsHandle } from "./Recipients";
import RichEditor, { type MentionQuery, type RichEditorHandle } from "./RichEditor";
import { tipAttrs, tipLabel, type Tip } from "../lib/tooltip";
import SignatureFrame, { type SignatureFrameHandle } from "./SignatureFrame";
import SignaturePicker from "./SignaturePicker";

export { emptyDraft, draftHasContent, draftKey, outgoingDraft, savedDraft, type Draft } from "../lib/composeDraft";

/** What mail servers take; the Rust side refuses more. */
const MAX_ATTACHMENT_BYTES = 25 * 1024 * 1024;
const ATTACH_KEYS = IS_MAC ? "⇧⌘A" : "Ctrl+Shift+A";
const TEMPLATE_KEYS = IS_MAC ? "⇧⌘T" : "Ctrl+Shift+T";
/** How many people an @mention suggests, as in Spark. */
const MENTION_SUGGESTIONS = 6;

/** A picked @mention: Undo takes them off To again if picking added them. */
interface MentionEdit {
  address: string;
  added: Contact | null;
  /** How many links to them the body had once it was in. */
  links: number;
}

/** Picture files that can go in the body when they're dropped on it. */
const PICTURE = /\.(png|jpe?g|gif|webp|bmp)$/i;

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
 * The body is rich text; under it are the signature, which is HTML too,
 * and a forward's original.
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
  onTemplateFromDraft,
  onTemplateSettings,
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
  /** Spark's "Create a New Template from draft": Settings → Templates, with this email in a new one. */
  onTemplateFromDraft: () => void;
  /** Settings → Templates. */
  onTemplateSettings: () => void;
  /** While AI writes the body, it's read-only and can't be sent. */
  ai?: ComposerAi;
}) {
  const rootRef = useRef<HTMLDivElement>(null);
  const recipientsRef = useRef<RecipientsHandle>(null);
  const editorRef = useRef<RichEditorHandle>(null);
  const quoteRef = useRef<HTMLTextAreaElement>(null);
  const signatureRef = useRef<SignatureFrameHandle>(null);
  const [dropping, setDropping] = useState(false);
  /** How big pictures go in the body (Spark's image size). */
  const [imageSize, setImageSize] = useState<ImageSize>("actual");
  /** Picking a day and time for Send Later or Remind me. */
  const [picking, setPicking] = useState<"sendLater" | "remind" | null>(null);
  // File drops arrive through a listener set up once; it reads these.
  const latest = useRef({ draft, onChange });
  latest.current = { draft, onChange };
  /** Unmounted: another email has the composer (it's keyed by session). */
  const closed = useRef(false);
  useEffect(() => {
    closed.current = false;
    return () => {
      closed.current = true;
    };
  }, []);

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

  /** Dropped files: pictures dropped on the body go in it, the rest are attached. */
  async function dropFiles(paths: string[], at: DragPoint) {
    const files = await describeFiles(paths);
    const editor = editorRef.current;
    const inline = editor?.covers(at.x, at.y) ? files.filter((f) => PICTURE.test(f.name)) : [];
    addFiles(files.filter((f) => !inline.includes(f)));
    if (!inline.length) return;
    try {
      await editor!.insertImages(await Promise.all(inline.map((f) => readImage(f.path))), at);
    } catch (error) {
      onError(String(error));
    }
  }

  useEffect(
    () =>
      onFileDrag((e) => {
        if (e.type === "drop") {
          setDropping(false);
          void dropFiles(e.paths, e.at);
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

  // Templates, read again each time the menu opens: they're edited in Settings.
  const [templates, setTemplates] = useState<Template[] | null>(null);
  function loadTemplates() {
    listTemplates().then(setTemplates, (error) => onError(String(error)));
  }

  /**
   * Puts a template in at the caret, its placeholders filled in where the names are known (the
   * first recipient's from To or, for a bare address, their contact); its subject too if there's none.
   */
  async function applyTemplate(t: Template) {
    const first = splitAddresses(draft.to)[0];
    const entry = first ? parseEntry(first) : null;
    const mine = account?.name ?? "";
    let name = entry?.name ?? "";
    if (entry && !name) {
      const contacts = await suggestContacts(entry.address).catch(() => []);
      name = contacts.find((c) => c.address.toLowerCase() === entry.address.toLowerCase())?.name ?? "";
      // This composer may be gone by now, with another email open.
      if (closed.current) return;
    }
    editorRef.current?.insertHtml(fillPlaceholders(t.html, placeholderValues(name, mine)));
    const { draft: now, onChange: change } = latest.current;
    if (!now.subject.trim() && t.subject) change({ subject: t.subject });
    void templateUsed(t.id).catch(() => {});
  }

  // Replies arrive with a recipient, so they start in the body; anything else starts in To.
  const [startInTo] = useState(() => !draft.to);

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
    } else if (mod && e.shiftKey && key === "t") {
      e.preventDefault();
      rootRef.current?.querySelector<HTMLButtonElement>("[data-templates] button")?.click();
    }
  }

  // @mentions in the body: picking someone links their name in the text and adds them to To.
  const [mention, setMention] = useState<MentionQuery | null>(null);
  const mentionHistory = useRef<{ undo: MentionEdit[]; redo: MentionEdit[] }>({ undo: [], redo: [] });
  const mentioned = useSuggestions(mention?.query ?? "", MENTION_SUGGESTIONS);
  const latestMention = useRef({ mentioned, pick: pickMention });
  latestMention.current = { mentioned, pick: pickMention };

  let mentionAnchor: CSSProperties | null = null;
  if (mention && mentioned.list.length > 0) {
    const { rect } = mention;
    const left = Math.max(8, Math.min(rect.left - 12, window.innerWidth - 408));
    // Under the line, or over it when there's no room below.
    mentionAnchor =
      rect.bottom + 240 < window.innerHeight
        ? { position: "fixed", left, top: rect.bottom + 4 }
        : { position: "fixed", left, bottom: window.innerHeight - rect.top + 4 };
  }

  function listed(address: string) {
    return [draft.to, draft.cc, draft.bcc]
      .flatMap(splitAddresses)
      .some((entry) => parseEntry(entry).address.toLowerCase() === address);
  }

  function pickMention(contact: Contact) {
    const editor = editorRef.current;
    if (!editor || !mention) return;
    const address = contact.address.toLowerCase();
    const added = listed(address) ? null : contact;
    editor.completeMention(displayName(contact), contact.address);
    mentionHistory.current.undo.push({ address, added, links: editor.mentions(address) });
    mentionHistory.current.redo = [];
    if (added) onChange({ to: joinAddresses([...splitAddresses(draft.to), entryFor(contact)]) });
    setMention(null);
  }

  function onBodyChange(html: string, text: string, inputType: string) {
    if (writing?.status === "done") ai?.dismiss();
    const patch: Partial<Draft> = { body: text, bodyHtml: html };
    const history = mentionHistory.current;
    const editor = editorRef.current;
    const undo = history.undo.at(-1);
    const redo = history.redo.at(-1);
    if (inputType === "historyUndo" && undo && editor && editor.mentions(undo.address) < undo.links) {
      history.undo.pop();
      history.redo.push(undo);
      if (undo.added) {
        patch.to = joinAddresses(splitAddresses(draft.to).filter((entry) => parseEntry(entry).address.toLowerCase() !== undo.address));
      }
    } else if (inputType === "historyRedo" && redo && editor && editor.mentions(redo.address) >= redo.links) {
      history.redo.pop();
      history.undo.push(redo);
      if (redo.added && !listed(redo.address)) patch.to = joinAddresses([...splitAddresses(draft.to), entryFor(redo.added)]);
    } else if (inputType !== "historyUndo" && inputType !== "historyRedo") {
      history.redo = [];
    }
    onChange(patch);
  }

  /** The mention list's keys, while it's showing. */
  function onBodyKey(e: globalThis.KeyboardEvent) {
    const { mentioned: shown, pick } = latestMention.current;
    const list = shown.list;
    if (e.isComposing || list.length === 0 || e.ctrlKey || e.metaKey || e.altKey) return false;
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      shown.move(e.key === "ArrowDown" ? 1 : -1, list.length);
    } else if ((e.key === "Enter" || e.key === "Tab") && !e.shiftKey) {
      pick(list[Math.min(shown.active, list.length - 1)]);
    } else if (e.key === "Escape") {
      shown.dismiss();
    } else {
      return false;
    }
    return true;
  }

  const attachedBytes = draft.attachments.reduce((sum, a) => sum + a.size, 0);
  const showSignature = draft.signature !== null && !draft.signatureHidden;

  // The body and a forward's original grow with their text; the area they're in scrolls.
  const scrollRef = useRef<HTMLDivElement>(null);
  const [areaHeight, setAreaHeight] = useState(0);
  useLayoutEffect(() => {
    fit(quoteRef);
  }, [draft.quote, showSignature, full]);
  useEffect(() => {
    const area = scrollRef.current;
    if (!area) return;
    const observer = new ResizeObserver(() => {
      setAreaHeight(area.clientHeight);
      fit(quoteRef);
    });
    observer.observe(area);
    return () => observer.disconnect();
  }, []);
  const hasImages = /<img\b/i.test(draft.bodyHtml ?? "");

  const writing = ai?.writing ?? null;
  const busy = writing?.status === "writing";
  const sendLabel = draft.sendLater ? "Schedule" : "Send";
  const accounts = useAccounts();
  const account = accounts.find((a) => a.id === draft.from) ?? accounts[0];

  return (
    <div
      ref={rootRef}
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
              editorRef.current?.focus("start");
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
          editorRef.current?.focus("end");
        }}
      >
        <div aria-busy={busy}>
          <RichEditor
            ref={editorRef}
            html={draft.bodyHtml ?? textToHtml(draft.body)}
            onChange={onBodyChange}
            placeholder={busy ? "" : "Enter text"}
            readOnly={busy}
            minHeight={showSignature || draft.quote !== undefined ? 72 : areaHeight}
            autoFocus={startInTo ? undefined : "end"}
            scroller={scrollRef}
            imageSize={imageSize}
            onMention={setMention}
            onKey={onBodyKey}
            onError={onError}
          />
        </div>
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

      {(draft.sendLater || draft.remind || hasImages) && (
        <div className="flex shrink-0 flex-wrap gap-1.5 px-3 pb-2">
          {hasImages && (
            <Dropdown
              direction="up"
              width="w-48"
              trigger={({ toggle, open }) => (
                <button
                  className={`inline-flex h-6 items-center gap-1 rounded-full px-2 text-xs ${
                    open ? "bg-primary/10 text-primary" : "bg-base-200 text-text-2 hover:text-base-content"
                  }`}
                  aria-expanded={open}
                  onClick={toggle}
                >
                  <ImageDown size={13} /> {IMAGE_SIZES.find((s) => s.key === imageSize)!.label} Image Size
                </button>
              )}
            >
              <li className="menu-title px-2 py-1 text-[11px]">Change images size</li>
              {IMAGE_SIZES.map((s) => (
                <MenuItem
                  key={s.key}
                  label={s.label}
                  detail={Number.isFinite(s.pixels) ? `${s.pixels}px` : undefined}
                  checked={imageSize === s.key}
                  onSelect={() => {
                    setImageSize(s.key);
                    void editorRef.current?.resizeImages(s.key);
                  }}
                />
              ))}
            </Dropdown>
          )}
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
          <AiMenu
            body={draft.body}
            selected={() => editorRef.current?.selection() ?? null}
            quickReplies={ai.quickReplies}
            disabled={busy}
            onRun={ai.run}
          />
        )}
        <IconButton
          title={{ label: "Attach files", keys: ATTACH_KEYS, card: "attach" }}
          active={draft.attachments.length > 0}
          onClick={() => void attach()}
        >
          <Paperclip size={17} />
        </IconButton>
        <span data-templates className="contents">
          <Dropdown
            direction="up"
            width="w-72"
            trigger={({ toggle, open }) => (
              <IconButton
                title={{ label: "Use a Template", keys: TEMPLATE_KEYS }}
                active={open}
                onClick={() => {
                  if (!open) loadTemplates();
                  toggle();
                }}
              >
                <LayoutTemplate size={17} />
              </IconButton>
            )}
          >
            {templates && templates.length > 0 && recentTemplates(templates).length > 0 && (
              <>
                <li className="menu-title px-2 py-1 text-[11px]">Recent</li>
                {recentTemplates(templates).map((t) => (
                  <MenuItem key={`recent-${t.id}`} label={t.name} detail={t.subject || undefined} onSelect={() => void applyTemplate(t)} />
                ))}
              </>
            )}
            <li className="menu-title px-2 py-1 text-[11px]">My Templates</li>
            {templates?.length === 0 && <li className="px-2 py-1 text-xs text-text-2">No templates yet</li>}
            {templates?.map((t) => (
              <MenuItem key={t.id} label={t.name} detail={t.subject || undefined} onSelect={() => void applyTemplate(t)} />
            ))}
            <li role="none" className="my-0.5 h-px bg-hairline" />
            <MenuItem label="Create a New Template from draft" onSelect={onTemplateFromDraft} />
            <MenuItem label="Edit Templates…" onSelect={onTemplateSettings} />
          </Dropdown>
        </span>
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
