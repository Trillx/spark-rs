import { LoaderCircle, Sparkles } from "lucide-react";
import { useEffect, useState } from "react";
import { openDraft } from "../api";
import { getAutoDraft, type AutoDraftView } from "../automation";
import { recipientNames } from "../lib/recipients";
import type { Message, OutgoingDraft } from "../types";

export interface AutoDraftActions {
  /** Opens the draft in the composer. */
  edit: (draft: Message) => void;
  send: (draft: Message) => void;
  remove: (draft: Message) => void;
  /** The draft says option `index` now. */
  choose: (draft: Message, index: number) => Promise<void>;
}

/**
 * Spark's auto-draft in the conversation, under the email it answers: when
 * it offers several replies, "Replies ready for you:" and a chip for each;
 * then the draft as it is, with Delete, Edit and Send.
 */
export default function AutoDraftCard({ draft: d, actions }: { draft: Message; actions: AutoDraftActions }) {
  const [saved, setSaved] = useState<OutgoingDraft | null>(null);
  const [auto, setAuto] = useState<AutoDraftView | null>(null);
  const [failed, setFailed] = useState<string | null>(null);
  const [choosing, setChoosing] = useState<number | null>(null);
  // A save changes the row: then the draft is read again.
  const version = `${d.id} ${d.timestamp} ${d.snippet}`;
  useEffect(() => {
    let current = true;
    Promise.all([openDraft(d.id), getAutoDraft(d.id)]).then(
      ([draft, view]) => {
        if (!current) return;
        setSaved(draft);
        setAuto(view);
        setFailed(null);
      },
      (error) => current && setFailed(String(error)),
    );
    return () => {
      current = false;
    };
  }, [version]);

  const options = auto?.options ?? [];
  const chosen = choosing ?? auto?.chosen ?? 0;
  // Once the user has changed the text, another option would throw that away: the picker goes.
  const edited = saved !== null && !options.some((o) => sameText(o.body, saved.body));
  const to = recipientNames(d.to);

  async function choose(index: number) {
    if (index === chosen || choosing !== null) return;
    setChoosing(index);
    try {
      await actions.choose(d, index);
    } finally {
      setChoosing(null);
    }
  }

  return (
    <article data-auto-draft={d.id} className="rounded-2xl border border-hairline bg-card">
      {options.length > 1 && !edited && (
        <section className="flex flex-wrap items-center gap-2 border-b border-hairline px-5 py-2.5 text-[13px] text-text-2">
          <Sparkles size={16} className="text-primary" aria-hidden />
          <span>Replies ready for you:</span>
          <div role="radiogroup" aria-label="Replies ready for you" className="flex flex-wrap gap-1.5">
            {options.map((o, i) => (
              <button
                key={`${i} ${o.title}`}
                role="radio"
                aria-checked={chosen === i}
                className={`flex h-7 items-center gap-1 rounded-md px-3 text-xs font-medium ${
                  chosen === i ? "bg-primary/15 text-primary" : "bg-chip text-base-content/80 hover:brightness-95"
                }`}
                onClick={() => void choose(i)}
              >
                {choosing === i && <LoaderCircle size={12} className="animate-spin" />}“{o.title || `Option ${i + 1}`}”
              </button>
            ))}
          </div>
        </section>
      )}
      <header className="flex flex-wrap items-center gap-x-3 gap-y-2 px-5 pb-1.5 pt-3.5">
        <span className="rounded-[4px] bg-error/10 px-1.5 text-[11px] font-semibold leading-5 text-error">Auto Draft</span>
        <span className="min-w-0 flex-1 truncate text-sm leading-5 text-text-2">
          {to.shown.length ? `To: ${to.shown.join(", ")}${to.more > 0 ? ` +${to.more}` : ""}` : ""}
        </span>
        <div className="flex shrink-0 items-center gap-1.5">
          {/* Not while another reply is being put in: they'd act on the one before. */}
          <button
            className="btn btn-ghost btn-sm h-7 min-h-7 rounded-md px-3 text-xs"
            disabled={choosing !== null}
            onClick={() => actions.remove(d)}
          >
            Delete
          </button>
          <button
            className="btn btn-ghost btn-sm h-7 min-h-7 rounded-md px-3 text-xs"
            disabled={choosing !== null}
            onClick={() => actions.edit(d)}
          >
            Edit
          </button>
          <span className="h-5 w-px bg-hairline" />
          <button
            className="btn btn-primary btn-sm h-7 min-h-7 rounded-md px-3 text-xs"
            disabled={saved === null || choosing !== null}
            onClick={() => actions.send(d)}
          >
            Send
          </button>
        </div>
      </header>
      <div className="select-text space-y-3 whitespace-pre-wrap px-5 pb-5 pt-2 text-[13px] leading-5">
        {failed ? (
          <p className="text-error">Couldn't load this draft. {failed}</p>
        ) : saved === null ? (
          <div className="h-10 animate-pulse rounded bg-base-content/5" />
        ) : (
          saved.body
            .split(/\n{2,}/)
            .filter((p) => p.trim())
            .map((p, i) => <p key={i}>{p}</p>)
        )}
      </div>
    </article>
  );
}

/** The same words, whatever the spacing: a save can change line endings. */
function sameText(a: string, b: string) {
  const words = (text: string) => text.split(/\s+/).filter(Boolean).join(" ");
  return words(a) === words(b);
}
