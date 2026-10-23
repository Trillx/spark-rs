import { useEffect, useRef, useState, type FormEvent, type ReactNode } from "react";

// Small modal dialogs: asking for a name, or confirming something that can't
// be undone. A failed action shows its error in the dialog and keeps it open.

function Shell({ label, onClose, children }: { label: string; onClose: () => void; children: ReactNode }) {
  useEffect(() => {
    // Capture phase, so Esc closes the dialog and nothing behind it.
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      e.stopPropagation();
      onClose();
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [onClose]);
  return (
    <div className="fixed inset-0 z-[2950] grid place-items-center bg-black/30" onMouseDown={onClose}>
      <div
        role="dialog"
        aria-modal
        aria-label={label}
        className="w-[380px] max-w-[calc(100vw-32px)] rounded-2xl bg-base-100 p-5 text-base-content shadow-popup"
        onMouseDown={(e) => e.stopPropagation()}
      >
        {children}
      </div>
    </div>
  );
}

function useAction(onClose: () => void) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  async function run(action: () => Promise<void> | void) {
    setBusy(true);
    setError(null);
    try {
      await action();
      onClose();
    } catch (e) {
      setError(String(e));
      setBusy(false);
    }
  }
  return { busy, error, run };
}

export function PromptDialog({
  title,
  label,
  initial = "",
  placeholder,
  confirmLabel,
  onSubmit,
  onClose,
}: {
  title: string;
  /** Says what the field is for, under the title. */
  label?: string;
  initial?: string;
  placeholder?: string;
  confirmLabel: string;
  onSubmit: (value: string) => Promise<void> | void;
  onClose: () => void;
}) {
  const [value, setValue] = useState(initial);
  const input = useRef<HTMLInputElement>(null);
  const { busy, error, run } = useAction(onClose);
  useEffect(() => input.current?.select(), []);

  function submit(e: FormEvent) {
    e.preventDefault();
    if (value.trim()) void run(() => onSubmit(value.trim()));
  }

  return (
    <Shell label={title} onClose={onClose}>
      <form onSubmit={submit}>
        <h2 className="text-base font-semibold">{title}</h2>
        {label && <p className="mt-1 text-[13px] text-text-2">{label}</p>}
        <input
          ref={input}
          className="input input-sm mt-4 w-full"
          value={value}
          placeholder={placeholder}
          spellCheck={false}
          onChange={(e) => setValue(e.target.value)}
        />
        {error && <p className="mt-2 text-[13px] text-error">{error}</p>}
        <div className="mt-5 flex justify-end gap-2">
          <button type="button" className="btn btn-ghost btn-sm" onClick={onClose}>
            Cancel
          </button>
          <button type="submit" className="btn btn-primary btn-sm" disabled={busy || !value.trim()}>
            {busy && <span className="loading loading-spinner loading-xs" />}
            {confirmLabel}
          </button>
        </div>
      </form>
    </Shell>
  );
}

export function ConfirmDialog({
  title,
  message,
  confirmLabel,
  onConfirm,
  onClose,
}: {
  title: string;
  message: string;
  confirmLabel: string;
  onConfirm: () => Promise<void> | void;
  onClose: () => void;
}) {
  const { busy, error, run } = useAction(onClose);
  return (
    <Shell label={title} onClose={onClose}>
      <h2 className="text-base font-semibold">{title}</h2>
      <p className="mt-2 text-[13px] text-text-2">{message}</p>
      {error && <p className="mt-2 text-[13px] text-error">{error}</p>}
      <div className="mt-5 flex justify-end gap-2">
        <button className="btn btn-ghost btn-sm" onClick={onClose}>
          Cancel
        </button>
        <button className="btn btn-error btn-sm" disabled={busy} onClick={() => void run(onConfirm)} autoFocus>
          {busy && <span className="loading loading-spinner loading-xs" />}
          {confirmLabel}
        </button>
      </div>
    </Shell>
  );
}
