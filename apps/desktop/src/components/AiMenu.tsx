import {
  ArrowUp,
  Briefcase,
  CalendarPlus,
  Expand,
  Heart,
  Pilcrow,
  RefreshCw,
  Scale,
  Shrink,
  Smile,
  SpellCheck,
  ThumbsDown,
  ThumbsUp,
  UserPlus,
  type LucideIcon,
} from "lucide-react";
import { useRef, useState } from "react";
import { QUICK_REPLIES, type QuickReply, type Rewrite } from "../ai";
import { tipAttrs } from "../lib/tooltip";
import AiMark from "./AiMark";
import { MenuItem, Popover } from "./Dropdown";

/**
 * What the composer's +AI menu asks for. Edits apply to `range` of the body:
 * the selected text, or all of it.
 */
export type AiRequest =
  | { type: "write"; instructions: string }
  | { type: "quickReply"; reply: QuickReply }
  | { type: "rewrite"; how: Rewrite; range: [number, number] }
  | { type: "refine"; instructions: string; range: [number, number] };

const EDITS: { key: Rewrite; label: string; Icon: LucideIcon }[] = [
  { key: "rephrase", label: "Rephrase", Icon: RefreshCw },
  { key: "shorten", label: "Shorten", Icon: Shrink },
  { key: "expand", label: "Expand", Icon: Expand },
  { key: "proofread", label: "Proofread", Icon: SpellCheck },
  { key: "paragraphs", label: "Break into paragraphs", Icon: Pilcrow },
];

const TONES: { key: Rewrite; label: string; Icon: LucideIcon }[] = [
  { key: "friendly", label: "Friendly tone", Icon: Smile },
  { key: "formal", label: "Formal tone", Icon: Briefcase },
  { key: "neutral", label: "Neutral tone", Icon: Scale },
];

const CHIP_ICONS: Record<QuickReply, LucideIcon> = { interested: ThumbsUp, notInterested: ThumbsDown, thanks: Heart };

/** Spark's starters: they begin the request for you to finish. */
const STARTERS = [
  { label: "Invite to a meeting…", prompt: "Invite to a meeting ", Icon: CalendarPlus },
  { label: "Make an intro to…", prompt: "Make an intro to ", Icon: UserPlus },
];

/**
 * The composer's +AI button. With an empty body it writes the draft from a
 * request (or a quick reply); with text, it rewrites the selection or, with
 * none, all of it.
 */
export default function AiMenu({
  body,
  selected,
  quickReplies,
  disabled,
  onRun,
}: {
  /** The body's text. */
  body: string;
  /** What's selected in the body, as offsets in its text. */
  selected: () => [number, number] | null;
  /** Offer the quick-reply chips while the body is empty. */
  quickReplies: boolean;
  disabled: boolean;
  onRun: (request: AiRequest) => void;
}) {
  const [prompt, setPrompt] = useState("");
  const [selection, setSelection] = useState<[number, number] | null>(null);
  const input = useRef<HTMLInputElement>(null);
  const blank = !body.trim();
  const range = selection ?? [0, body.length];

  /** Opening notes the selection, which clicking the menu doesn't clear. */
  function toggleMenu(toggle: () => void) {
    setSelection(selected());
    toggle();
  }

  function start(text: string) {
    setPrompt(text);
    window.setTimeout(() => {
      input.current?.focus();
      input.current?.setSelectionRange(text.length, text.length);
    });
  }

  return (
    <Popover
      label={blank ? "Write with AI" : "Edit with AI"}
      direction="up"
      className="w-80"
      trigger={({ toggle, open }) => (
        <button
          className={`flex h-8 items-center rounded-lg px-2 hover:bg-base-content/10 disabled:opacity-40 ${
            open ? "bg-base-content/10" : ""
          }`}
          {...tipAttrs(blank ? "Write with +AI" : "Edit with +AI")}
          aria-label="+AI"
          aria-haspopup="dialog"
          aria-expanded={open}
          disabled={disabled}
          onClick={() => toggleMenu(toggle)}
        >
          <AiMark className="text-sm" />
        </button>
      )}
    >
      {(close) => {
        const run = (request: AiRequest) => {
          close();
          setPrompt("");
          onRun(request);
        };
        return (
          <>
            <div className="mb-2 flex items-center gap-2">
              <span className="text-sm font-semibold">{blank ? "Write with" : "Edit with"}</span>
              <span className="rounded-md bg-base-300 px-1.5 py-1">
                <AiMark className="text-xs" />
              </span>
              {selection && <span className="ml-auto text-xs text-text-2">Selected text</span>}
            </div>
            <form
              className="flex items-center gap-1 rounded-xl border border-hairline bg-base-100 py-1 pl-2.5 pr-1 focus-within:border-primary"
              onSubmit={(e) => {
                e.preventDefault();
                const instructions = prompt.trim();
                if (!instructions) return;
                run(blank ? { type: "write", instructions } : { type: "refine", instructions, range });
              }}
            >
              <input
                ref={input}
                autoFocus
                aria-label={blank ? "What to write" : "How to change it"}
                className="min-w-0 flex-1 bg-transparent py-1 outline-none placeholder:text-text-3"
                placeholder={
                  !blank
                    ? "How should AI change it?"
                    : quickReplies
                      ? "What should the reply say?"
                      : "Describe the email you want"
                }
                value={prompt}
                onChange={(e) => setPrompt(e.target.value)}
                // Keeps Ctrl+Enter from sending the email.
                onKeyDown={(e) => e.stopPropagation()}
              />
              <button
                type="submit"
                aria-label={blank ? "Write" : "Rewrite"}
                disabled={!prompt.trim()}
                className="grid size-7 shrink-0 place-items-center rounded-lg bg-primary text-primary-content disabled:opacity-40"
              >
                <ArrowUp size={15} />
              </button>
            </form>
            <ul className="menu menu-sm mt-2 w-full gap-0.5 p-0">
              {blank ? (
                <>
                  {quickReplies &&
                    QUICK_REPLIES.map(({ key, label }) => {
                      const Icon = CHIP_ICONS[key];
                      return (
                        <MenuItem
                          key={key}
                          label={label}
                          icon={<Icon size={15} />}
                          onSelect={() => run({ type: "quickReply", reply: key })}
                        />
                      );
                    })}
                  {quickReplies && <li role="none" className="my-0.5 h-px bg-hairline" />}
                  {STARTERS.map(({ label, prompt, Icon }) => (
                    <MenuItem key={label} label={label} icon={<Icon size={15} />} onSelect={() => start(prompt)} />
                  ))}
                </>
              ) : (
                <>
                  {EDITS.map(({ key, label, Icon }) => (
                    <MenuItem
                      key={key}
                      label={label}
                      icon={<Icon size={15} />}
                      onSelect={() => run({ type: "rewrite", how: key, range })}
                    />
                  ))}
                  <li role="none" className="my-0.5 h-px bg-hairline" />
                  {TONES.map(({ key, label, Icon }) => (
                    <MenuItem
                      key={key}
                      label={label}
                      icon={<Icon size={15} />}
                      onSelect={() => run({ type: "rewrite", how: key, range })}
                    />
                  ))}
                </>
              )}
            </ul>
          </>
        );
      }}
    </Popover>
  );
}
