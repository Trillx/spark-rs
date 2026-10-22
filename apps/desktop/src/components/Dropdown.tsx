import { Check } from "lucide-react";
import { useEffect, useRef, useState, type ReactNode, type RefObject } from "react";

/** Open state that closes on an outside click or Esc. */
function useDismissable(): [boolean, (open: boolean | ((o: boolean) => boolean)) => void, RefObject<HTMLDivElement | null>] {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    const onPointer = (e: PointerEvent) => {
      if (!ref.current?.contains(e.target as Node)) setOpen(false);
    };
    // Capture phase, so Esc closes the menu without also closing the thread.
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      e.stopPropagation();
      setOpen(false);
    };
    window.addEventListener("pointerdown", onPointer);
    window.addEventListener("keydown", onKey, true);
    return () => {
      window.removeEventListener("pointerdown", onPointer);
      window.removeEventListener("keydown", onKey, true);
    };
  }, [open]);

  return [open, setOpen, ref];
}

/**
 * A click-to-open menu. daisyUI's focus-based dropdown closes unpredictably
 * around our global key handling, so this one is controlled.
 */
export default function Dropdown({
  trigger,
  children,
  align = "start",
  direction = "down",
  width = "w-52",
}: {
  trigger: (props: { open: boolean; toggle: () => void }) => ReactNode;
  /** Menu items; choosing one closes the menu. */
  children: ReactNode;
  align?: "start" | "end";
  direction?: "down" | "up";
  width?: string;
}) {
  const [open, setOpen, ref] = useDismissable();

  return (
    <div ref={ref} className="relative">
      {trigger({ open, toggle: () => setOpen((o) => !o) })}
      {open && (
        <ul
          role="menu"
          onClick={(e) => {
            if ((e.target as Element).closest("[role='menuitem']")) setOpen(false);
          }}
          className={`menu menu-sm absolute z-[2020] gap-0.5 rounded-box border border-hairline bg-popover p-1 text-[13px] text-base-content shadow-popup ${width} ${
            align === "end" ? "right-0" : "left-0"
          } ${direction === "up" ? "bottom-full mb-1" : "top-full mt-1"}`}
        >
          {children}
        </ul>
      )}
    </div>
  );
}

export function MenuItem({
  label,
  detail,
  icon,
  checked,
  danger = false,
  onSelect,
}: {
  label: string;
  detail?: string;
  icon?: ReactNode;
  /** Shows a check column when defined. */
  checked?: boolean;
  danger?: boolean;
  onSelect: () => void;
}) {
  return (
    <li role="none">
      <button role="menuitem" className={`rounded-md ${danger ? "text-error" : ""}`} onClick={onSelect}>
        {checked !== undefined && <Check size={14} className={checked ? "" : "invisible"} />}
        {icon}
        <span className="flex-1 text-left">{label}</span>
        {detail && <span className="text-xs text-text-2">{detail}</span>}
      </button>
    </li>
  );
}

/** Like Dropdown, but the panel holds arbitrary content and closes through `close`. */
export function Popover({
  trigger,
  children,
  align = "start",
  label,
  className = "w-72",
}: {
  trigger: (props: { open: boolean; toggle: () => void }) => ReactNode;
  children: (close: () => void) => ReactNode;
  align?: "start" | "end";
  label: string;
  className?: string;
}) {
  const [open, setOpen, ref] = useDismissable();
  return (
    <div ref={ref} className="relative">
      {trigger({ open, toggle: () => setOpen((o) => !o) })}
      {open && (
        <div
          role="dialog"
          aria-label={label}
          className={`absolute top-full z-[2020] mt-1 rounded-2xl border border-hairline bg-popover p-3 text-[13px] text-base-content shadow-popup ${className} ${
            align === "end" ? "right-0" : "left-0"
          }`}
        >
          {children(() => setOpen(false))}
        </div>
      )}
    </div>
  );
}
