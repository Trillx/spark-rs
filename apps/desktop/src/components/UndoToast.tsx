import type { LucideIcon } from "lucide-react";
import type { CSSProperties } from "react";

/** The action's icon in its color, e.g. a green ↙ for Set Aside. */
export interface ToastIcon {
  Icon: LucideIcon;
  className: string;
}

export interface ToastData {
  id: number;
  text: string;
  undo?: () => void;
  /** The action's label when it isn't Undo, e.g. "Edit" to reopen a draft that failed to send. */
  actionLabel?: string;
  icon?: ToastIcon;
  /** How long it stays, in ms; TOAST_MS by default. */
  duration?: number;
}

export const TOAST_MS = 5000;

const RING_RADIUS = 7;
const RING_LENGTH = 2 * Math.PI * RING_RADIUS;

/**
 * Spark's undo popup at the bottom left of the content area: action icon,
 * message, divider, uppercase UNDO and a draining countdown ring.
 */
export default function UndoToast({ toast, left, onUndo }: { toast: ToastData; left: number; onUndo: () => void }) {
  const icon = toast.icon;
  return (
    <div
      role="status"
      style={{ left }}
      className="fixed bottom-4 z-[3000] flex h-11 items-center gap-2.5 rounded-lg border border-hairline bg-popover pl-3.5 pr-3 text-[13px] text-base-content shadow-popup"
    >
      {icon && <icon.Icon size={16} className={`shrink-0 ${icon.className}`} />}
      <span className="max-w-[420px] truncate">{toast.text}</span>
      {toast.undo && (
        <>
          <span className="mx-0.5 h-5 w-px bg-hairline" />
          <button
            className="text-[13px] font-semibold uppercase leading-4 text-primary hover:brightness-90"
            onClick={onUndo}
          >
            {toast.actionLabel ?? "Undo"}
          </button>
          <svg key={toast.id} width="18" height="18" viewBox="0 0 18 18" className="-rotate-90" aria-hidden>
            <circle cx="9" cy="9" r={RING_RADIUS} fill="none" strokeWidth="2" className="stroke-base-300" />
            <circle
              cx="9"
              cy="9"
              r={RING_RADIUS}
              fill="none"
              strokeWidth="2"
              strokeLinecap="round"
              strokeDasharray={RING_LENGTH}
              className="countdown-ring stroke-primary"
              style={{ "--ring-length": RING_LENGTH, "--ring-duration": `${toast.duration ?? TOAST_MS}ms` } as CSSProperties}
            />
          </svg>
        </>
      )}
    </div>
  );
}
