import { X } from "lucide-react";
import {
  useEffect,
  useImperativeHandle,
  useRef,
  useState,
  type ClipboardEvent,
  type CSSProperties,
  type KeyboardEvent,
  type ReactNode,
  type Ref,
} from "react";
import { neverSuggestContact, suggestContacts } from "../api";
import type { Contact } from "../types";
import { entryFor, joinAddresses, parseEntry, splitAddresses, takeFinished } from "../lib/addresses";
import { IS_MAC } from "../lib/shortcuts";
import { tipAttrs } from "../lib/tooltip";
import Avatar from "./Avatar";

export type RecipientField = "to" | "cc" | "bcc";

export interface RecipientValues {
  to: string;
  cc: string;
  bcc: string;
  /** Cc and Bcc opened while empty; they close again when the recipients do. */
  showCc: boolean;
  showBcc: boolean;
}

/** What the composer does with the recipients from outside: its Cc and Bcc shortcuts. */
export interface RecipientsHandle {
  focus: (field: RecipientField) => void;
}

const LABELS: Record<RecipientField, string> = { to: "To", cc: "Cc", bcc: "Bcc" };
const KEYS = {
  cc: IS_MAC ? "⇧⌘C" : "Ctrl+Shift+C",
  bcc: IS_MAC ? "⇧⌘B" : "Ctrl+Shift+B",
};
/** Names the collapsed summary shows per field before "+N". */
const SUMMARY_NAMES = 3;

/**
 * To, Cc and Bcc, as Spark has them: one summary line ("To: Ana, Bo +2 Cc: Cy") until focus
 * comes in, then a row per field, with Cc and Bcc rows only once they're used or opened.
 */
export default function Recipients({
  values,
  suggest,
  autoFocus,
  onChange,
  ref,
}: {
  values: RecipientValues;
  /** Suggest people while typing (Settings → Composer). */
  suggest: boolean;
  /** Start in To, open. */
  autoFocus: boolean;
  onChange: (patch: Partial<RecipientValues>) => void;
  ref?: Ref<RecipientsHandle>;
}) {
  const [open, setOpen] = useState(autoFocus);
  const inputs = useRef<Partial<Record<RecipientField, HTMLInputElement | null>>>({});
  // The field to focus once its row is on screen.
  const [focusing, setFocusing] = useState<RecipientField | null>(autoFocus ? "to" : null);
  // While the summary gives way to the rows, focus is briefly nowhere: that isn't leaving.
  const opening = useRef(false);

  useEffect(() => {
    if (!focusing) return;
    inputs.current[focusing]?.focus();
    opening.current = false;
    setFocusing(null);
  }, [focusing, open, values.showCc, values.showBcc]);

  function focus(field: RecipientField) {
    opening.current = true;
    setOpen(true);
    if (field === "cc" && !values.showCc) onChange({ showCc: true });
    if (field === "bcc" && !values.showBcc) onChange({ showBcc: true });
    setFocusing(field);
  }
  useImperativeHandle(ref, () => ({ focus }));

  const shown: Record<RecipientField, boolean> = {
    to: true,
    cc: values.showCc || values.cc.trim() !== "",
    bcc: values.showBcc || values.bcc.trim() !== "",
  };
  const fields = (["to", "cc", "bcc"] as const).filter((f) => shown[f]);

  return (
    <div
      className="shrink-0"
      onFocus={() => setOpen(true)}
      onBlur={(e) => {
        if (opening.current || e.currentTarget.contains(e.relatedTarget)) return;
        setOpen(false);
        // Empty Cc and Bcc rows close with the rest.
        onChange({ showCc: values.showCc && values.cc.trim() !== "", showBcc: values.showBcc && values.bcc.trim() !== "" });
      }}
    >
      {open ? (
        fields.map((field) => (
          <AddressField
            key={field}
            label={LABELS[field]}
            value={values[field]}
            suggest={suggest}
            inputRef={(el) => {
              inputs.current[field] = el;
            }}
            onChange={(value) => onChange({ [field]: value })}
          >
            {field === "to" && (
              <span className="flex shrink-0 gap-2 self-start pt-px">
                {!shown.cc && <CopyToggle field="cc" onClick={() => focus("cc")} />}
                {!shown.bcc && <CopyToggle field="bcc" onClick={() => focus("bcc")} />}
              </span>
            )}
          </AddressField>
        ))
      ) : (
        <Summary values={values} onOpen={focus} />
      )}
    </div>
  );
}

function CopyToggle({ field, onClick }: { field: "cc" | "bcc"; onClick: () => void }) {
  return (
    <button
      tabIndex={-1}
      // Taking focus would close the rows before the click lands.
      onMouseDown={(e) => e.preventDefault()}
      onClick={onClick}
      {...tipAttrs({ label: field === "cc" ? "Carbon copy" : "Blind carbon copy", keys: KEYS[field] })}
      className="rounded px-1 text-sm leading-[22px] text-text-2 hover:text-base-content"
    >
      {LABELS[field]}
    </button>
  );
}

/** The collapsed recipients: each used field's label and its first few names. */
function Summary({ values, onOpen }: { values: RecipientValues; onOpen: (field: RecipientField) => void }) {
  const groups = (["to", "cc", "bcc"] as const)
    .map((field) => ({ field, names: splitAddresses(values[field]).map((e) => displayName(parseEntry(e))) }))
    .filter((g) => g.field === "to" || g.names.length > 0);
  return (
    <div
      role="button"
      tabIndex={0}
      aria-label="Recipients"
      className="flex h-10 cursor-text items-center gap-4 overflow-hidden border-b border-hairline px-4 text-sm"
      onFocus={() => onOpen("to")}
      onMouseDown={(e) => {
        e.preventDefault();
        onOpen("to");
      }}
    >
      {groups.map(({ field, names }) => (
        <span
          key={field}
          className="flex min-w-0 shrink items-baseline gap-1.5 whitespace-nowrap"
          onMouseDown={(e) => {
            e.preventDefault();
            e.stopPropagation();
            onOpen(field);
          }}
        >
          <span className="text-text-2">{LABELS[field]}:</span>
          <span className="truncate text-primary">{names.slice(0, SUMMARY_NAMES).join(", ")}</span>
          {names.length > SUMMARY_NAMES && <span className="text-text-2">+{names.length - SUMMARY_NAMES}</span>}
        </span>
      ))}
    </div>
  );
}

/**
 * One recipient row: the people added so far as blue names, then what's being typed, with
 * suggestions under it. Its value is the field as the draft keeps it; while focused, text after
 * the last comma is still being typed.
 */
function AddressField({
  label,
  value,
  suggest,
  inputRef,
  onChange,
  children,
}: {
  label: string;
  value: string;
  suggest: boolean;
  inputRef: (el: HTMLInputElement | null) => void;
  onChange: (value: string) => void;
  children?: ReactNode;
}) {
  const [focused, setFocused] = useState(false);
  const [selected, setSelected] = useState<number | null>(null);
  const input = useRef<HTMLInputElement | null>(null);
  const { finished, rest } = takeFinished(value);
  const entries = focused ? finished : splitAddresses(value);
  const typed = focused ? rest : "";
  const added = new Set(entries.map((e) => parseEntry(e).address.toLowerCase()));
  const people = useSuggestions(suggest && focused ? typed : "");
  const list = people.list.filter((c) => !added.has(c.address.toLowerCase()));

  /** The field with `entries` added and nothing being typed. */
  const withEntries = (all: string[]) => (all.length ? `${joinAddresses(all)}, ` : "");

  function pick(contact: Contact) {
    onChange(withEntries([...finished, entryFor(contact)]));
    people.clear();
  }

  function remove(index: number) {
    onChange(withEntries(entries.filter((_, i) => i !== index)) + typed);
    setSelected(null);
  }

  function onKeyDown(e: KeyboardEvent<HTMLInputElement>) {
    if (e.nativeEvent.isComposing || e.ctrlKey || e.metaKey || e.altKey) return;
    const showing = list.length > 0;
    if (showing && (e.key === "ArrowDown" || e.key === "ArrowUp")) {
      e.preventDefault();
      people.move(e.key === "ArrowDown" ? 1 : -1, list.length);
    } else if (e.key === "Enter" || (e.key === "Tab" && !e.shiftKey)) {
      // Tab picks too, then moves on as usual.
      if (e.key === "Enter") e.preventDefault();
      if (showing) pick(list[Math.min(people.active, list.length - 1)]);
      else if (typed.trim()) onChange(withEntries([...finished, typed.trim()]));
    } else if (e.key === "Escape" && showing) {
      e.preventDefault();
      e.stopPropagation();
      people.dismiss();
    } else if ((e.key === "Backspace" || e.key === "Delete") && selected !== null) {
      e.preventDefault();
      remove(selected);
    } else if (e.key === "Backspace" && typed === "" && entries.length > 0) {
      e.preventDefault();
      remove(entries.length - 1);
    }
  }

  function onPaste(e: ClipboardEvent<HTMLInputElement>) {
    const text = e.clipboardData.getData("text");
    // A list of addresses becomes recipients at once; anything else is typed.
    if (!text.includes("@")) return;
    e.preventDefault();
    const { selectionStart, selectionEnd } = e.currentTarget;
    const replaced = typed.slice(0, selectionStart ?? typed.length)
      + text.replace(/[\r\n]+/g, ",")
      + typed.slice(selectionEnd ?? typed.length);
    onChange(withEntries([...finished, ...splitAddresses(replaced)]));
  }

  return (
    <div className="relative flex min-h-10 shrink-0 items-start gap-2 border-b border-hairline px-4 py-[9px] text-sm leading-[22px]">
      <span className="w-8 shrink-0 text-text-2">{label}</span>
      <div
        className="flex max-h-[66px] min-w-0 flex-1 cursor-text flex-wrap items-center gap-x-1 overflow-y-auto"
        onMouseDown={(e) => {
          if (e.target !== e.currentTarget) return;
          e.preventDefault();
          input.current?.focus();
        }}
      >
        {entries.map((entry, i) => (
          <span
            key={`${i}:${entry}`}
            data-tip={parseEntry(entry).address}
            className={`cursor-default rounded px-px ${selected === i ? "bg-primary text-primary-content" : ""}`}
            onMouseDown={(e) => {
              e.preventDefault();
              input.current?.focus();
              setSelected(i);
            }}
            onDoubleClick={() => {
              // Back to text, to fix a typo.
              onChange(withEntries(entries.filter((_, j) => j !== i)) + entry);
              setSelected(null);
            }}
          >
            <span className={selected === i ? "" : "text-primary"}>{displayName(parseEntry(entry))}</span>
            {(i < entries.length - 1 || focused) && ","}
          </span>
        ))}
        <input
          ref={(el) => {
            input.current = el;
            inputRef(el);
          }}
          aria-label={label}
          spellCheck={false}
          autoComplete="off"
          className="h-[22px] min-w-16 flex-1 bg-transparent outline-none"
          value={typed}
          onFocus={() => {
            setFocused(true);
            // Whatever was there is finished; typing starts a new one.
            if (rest.trim()) onChange(withEntries(splitAddresses(value)));
          }}
          onBlur={() => {
            setFocused(false);
            setSelected(null);
            people.clear();
            const clean = joinAddresses(splitAddresses(value));
            if (clean !== value) onChange(clean);
          }}
          onChange={(e) => {
            setSelected(null);
            onChange((finished.length ? `${joinAddresses(finished)}, ` : "") + e.target.value);
          }}
          onKeyDown={onKeyDown}
          onPaste={onPaste}
        />
      </div>
      {children}
      {list.length > 0 && (
        <SuggestionList
          className="left-14 top-full mt-0.5 w-[440px] max-w-[calc(100%-4.5rem)]"
          contacts={list}
          active={Math.min(people.active, list.length - 1)}
          onHover={people.setActive}
          onPick={pick}
          onNever={people.never}
        />
      )}
    </div>
  );
}

/** Who an entry shows as: their name, else their address. */
export function displayName(person: { name: string; address: string }) {
  return person.name || person.address;
}

/**
 * Suggestions for `query` as it's typed: the latest answer only, the first one highlighted.
 * `limit` caps how many show.
 */
export function useSuggestions(query: string, limit?: number) {
  const [result, setResult] = useState<{ query: string; contacts: Contact[] }>({ query: "", contacts: [] });
  const [active, setActive] = useState(0);
  const [dismissed, setDismissed] = useState<string | null>(null);
  const q = query.trim();

  useEffect(() => {
    if (dismissed !== null && q !== dismissed) setDismissed(null);
    if (!q || q === dismissed) {
      return;
    }
    let current = true;
    const timer = window.setTimeout(() => {
      suggestContacts(q).then(
        (found) => {
          if (!current) return;
          setResult({ query: q, contacts: limit ? found.slice(0, limit) : found });
          setActive(0);
        },
        () => current && setResult({ query: q, contacts: [] }),
      );
    }, 60);
    return () => {
      current = false;
      window.clearTimeout(timer);
    };
  }, [q, dismissed, limit]);

  return {
    list: q && q !== dismissed && result.query === q ? result.contacts : [],
    active,
    setActive,
    move: (step: number, count: number) => setActive((i) => (i + step + count) % count),
    /** Esc: hidden until the query changes. */
    dismiss: () => setDismissed(q),
    clear: () => setResult({ query: "", contacts: [] }),
    /** The ✕ on a suggestion. */
    never: (contact: Contact) => {
      void neverSuggestContact(contact.address).then(() => {
        setResult((r) => ({ ...r, contacts: r.contacts.filter((c) => c.address !== contact.address) }));
      }, () => {});
    },
  };
}

/** The people suggested for a recipient field or an @mention: avatar, name, address. */
export function SuggestionList({
  contacts,
  active,
  className = "",
  style,
  onHover,
  onPick,
  onNever,
}: {
  contacts: Contact[];
  active: number;
  className?: string;
  style?: CSSProperties;
  onHover: (index: number) => void;
  onPick: (contact: Contact) => void;
  onNever: (contact: Contact) => void;
}) {
  const activeRef = useRef<HTMLLIElement>(null);
  useEffect(() => {
    activeRef.current?.scrollIntoView({ block: "nearest" });
  }, [active]);
  return (
    <ul
      role="listbox"
      aria-label="Suggestions"
      style={style}
      className={`absolute z-30 max-h-[268px] overflow-y-auto rounded-lg border border-hairline bg-base-100 py-1 shadow-popup ${className}`}
      // Keep focus in the field being typed in.
      onMouseDown={(e) => e.preventDefault()}
    >
      {contacts.map((c, i) => (
        <li
          key={c.address}
          ref={i === active ? activeRef : undefined}
          role="option"
          aria-selected={i === active}
          className={`group flex h-9 cursor-default items-center gap-2.5 pl-3 pr-1 ${i === active ? "bg-primary/10" : ""}`}
          onMouseMove={() => i !== active && onHover(i)}
          onClick={() => onPick(c)}
        >
          <Avatar name={displayName(c)} />
          {c.name && <span className="max-w-[220px] shrink-0 truncate font-semibold">{c.name}</span>}
          <span className="min-w-0 flex-1 truncate text-text-2">{c.address}</span>
          <button
            aria-label={`Don't suggest ${c.address}`}
            data-tip="Don't suggest again"
            className="grid size-6 shrink-0 place-items-center rounded text-text-3 opacity-0 hover:bg-base-content/10 hover:text-base-content group-hover:opacity-100"
            onClick={(e) => {
              e.stopPropagation();
              onNever(c);
            }}
          >
            <X size={13} />
          </button>
        </li>
      ))}
    </ul>
  );
}
