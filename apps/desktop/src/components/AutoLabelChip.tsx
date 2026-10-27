import { Check, Tag, X } from "lucide-react";
import { AUTO_LABELS, AUTO_LABEL_ORDER } from "../lib/autoLabels";
import type { AutoLabel } from "../types";
import Dropdown, { MenuItem } from "./Dropdown";

/** An Auto-Label as a small colored chip, for list rows. */
export function AutoLabelChip({ label }: { label: AutoLabel }) {
  const { name, color } = AUTO_LABELS[label];
  return (
    <span
      className="h-4 shrink-0 rounded-[3px] px-1.5 text-[11px] font-semibold leading-4"
      style={{ color, backgroundColor: `${color}1f` }}
    >
      {name}
    </span>
  );
}

/**
 * The conversation's Auto-Label beside its subject: a click picks another
 * or takes it off, and hovering shows the × that takes it off, as in Spark.
 */
export function AutoLabelPill({ label, onChange }: { label: AutoLabel; onChange: (label: AutoLabel | null) => void }) {
  const { name, color } = AUTO_LABELS[label];
  return (
    <span className="group relative flex shrink-0 items-center">
      <Dropdown
        width="w-48"
        trigger={({ toggle, open }) => (
          <button
            className={`flex h-[22px] items-center gap-1 rounded-md pl-1.5 pr-2 text-xs font-semibold ${open ? "brightness-95" : ""}`}
            style={{ color, backgroundColor: `${color}1f` }}
            data-tip="Added by Auto-Labels: click to change it"
            aria-haspopup="menu"
            aria-expanded={open}
            onClick={toggle}
          >
            <Tag size={12} />
            {name}
          </button>
        )}
      >
        {AUTO_LABEL_ORDER.map((l) => (
          <MenuItem
            key={l}
            label={AUTO_LABELS[l].name}
            icon={l === label ? <Check size={14} /> : <span className="w-3.5" />}
            onSelect={() => l !== label && onChange(l)}
          />
        ))}
        <li role="none" className="my-0.5 h-px bg-hairline" />
        <MenuItem label="Remove Label" icon={<X size={14} />} onSelect={() => onChange(null)} />
      </Dropdown>
      <button
        aria-label={`Remove ${name}`}
        data-tip="Remove label"
        className="absolute -right-1.5 -top-1.5 hidden size-4 place-items-center rounded-full bg-base-100 text-text-2 shadow-sm ring-1 ring-hairline hover:text-base-content group-hover:grid"
        onClick={() => onChange(null)}
      >
        <X size={10} />
      </button>
    </span>
  );
}
