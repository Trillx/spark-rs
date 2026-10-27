import { Command as CommandIcon, type LucideIcon } from "lucide-react";
import { useEffect, useMemo, useRef, useState, type KeyboardEvent } from "react";
import { IS_MAC } from "../lib/shortcuts";

export interface Command {
  id: string;
  label: string;
  Icon: LucideIcon;
  /** Extra words that match, e.g. "archive" for Done. */
  keywords?: string;
  shortcut?: string;
  /** Right-side text when there is no shortcut, e.g. a snooze time. */
  detail?: string;
  run: () => void;
}

/** The Command Center: a 500px dark-blue palette, filtered as you type. */
export default function CommandCenter({
  title,
  placeholder,
  commands,
  fallback,
  onRun,
  onClose,
}: {
  title: string;
  placeholder: string;
  commands: Command[];
  /** A last command for what's typed, e.g. creating a folder of that name. */
  fallback?: (query: string) => Command | null;
  onRun: (command: Command) => void;
  onClose: () => void;
}) {
  const [query, setQuery] = useState("");
  const [index, setIndex] = useState(0);
  const listRef = useRef<HTMLUListElement>(null);

  const results = useMemo(() => {
    const words = query.toLowerCase().split(/\s+/).filter(Boolean);
    const found = commands.filter((c) => {
      const text = `${c.label} ${c.keywords ?? ""}`.toLowerCase();
      return words.every((w) => text.includes(w));
    });
    const extra = query.trim() ? fallback?.(query.trim()) : null;
    return extra ? [...found, extra] : found;
  }, [commands, query, fallback]);
  const active = Math.min(index, Math.max(results.length - 1, 0));

  useEffect(() => {
    listRef.current?.children[active]?.scrollIntoView({ block: "nearest" });
  }, [active]);

  function onKeyDown(e: KeyboardEvent) {
    const mod = IS_MAC ? e.metaKey : e.ctrlKey;
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      const step = e.key === "ArrowDown" ? 1 : -1;
      if (results.length) setIndex((active + step + results.length) % results.length);
    } else if (e.key === "Enter") {
      e.preventDefault();
      if (results[active]) onRun(results[active]);
    } else if (e.key === "Escape" || (mod && e.key.toLowerCase() === "k")) {
      e.preventDefault();
      onClose();
    }
  }

  return (
    <div className="fixed inset-0 z-[2900] bg-black/20" onMouseDown={onClose}>
      <div
        role="dialog"
        aria-modal
        aria-label="Command Center"
        onMouseDown={(e) => e.stopPropagation()}
        onKeyDown={onKeyDown}
        className="absolute left-1/2 top-[14%] w-[500px] max-w-[calc(100vw-32px)] -translate-x-1/2 overflow-hidden rounded-lg bg-command text-white shadow-popup"
      >
        <div className="flex h-[30px] items-center gap-1.5 px-4 text-xs text-white/60">
          <CommandIcon size={12} />
          {title}
        </div>
        <input
          autoFocus
          spellCheck={false}
          placeholder={placeholder}
          className="h-[50px] w-full border-y border-white/15 bg-transparent px-4 text-base text-white outline-none selection:bg-[#3382d6] placeholder:text-white/40"
          value={query}
          onChange={(e) => {
            setQuery(e.target.value);
            setIndex(0);
          }}
        />
        <ul ref={listRef} role="menu" className="max-h-[320px] overflow-y-auto p-1.5">
          {results.map((c, i) => (
            <li
              key={c.id}
              role="menuitem"
              aria-selected={i === active}
              onMouseMove={() => setIndex(i)}
              onClick={() => onRun(c)}
              className={`flex h-9 cursor-default items-center gap-3 rounded-md px-3 text-[13px] ${
                i === active ? "bg-command-row" : ""
              }`}
            >
              <c.Icon size={16} className="shrink-0 opacity-80" />
              <span className="flex-1 truncate">{c.label}</span>
              {c.shortcut ? (
                <kbd className="kbd kbd-xs border-white/20 bg-white/10 font-sans text-[11px] text-white">{c.shortcut}</kbd>
              ) : (
                c.detail && <span className="text-xs text-white/50">{c.detail}</span>
              )}
            </li>
          ))}
          {results.length === 0 && <li className="px-3 py-2 text-[13px] text-white/50">No matching commands</li>}
        </ul>
      </div>
    </div>
  );
}
