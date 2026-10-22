import { isTauri } from "@tauri-apps/api/core";
import { getCurrentWindow, type Window } from "@tauri-apps/api/window";
import { Copy, Minus, Square, X } from "lucide-react";
import { useEffect, useState, type ReactNode } from "react";

/**
 * The invisible 32px title-bar strip over the content. Tauri starts a window
 * drag on mousedown and toggles maximize on double-click for elements with
 * `data-tauri-drag-region`.
 */
export function DragStrip() {
  return <div data-tauri-drag-region className="absolute inset-x-0 top-0 z-[99] h-8" />;
}

function withWindow(action: (w: Window) => Promise<void>) {
  if (isTauri()) void action(getCurrentWindow());
}

/** Windows caption buttons: 40×32 squares at the top right; close hovers red. */
export function CaptionButtons() {
  const [maximized, setMaximized] = useState(false);

  useEffect(() => {
    if (!isTauri()) return;
    const win = getCurrentWindow();
    let stop: (() => void) | undefined;
    let cancelled = false;
    const sync = () => void win.isMaximized().then(setMaximized);
    sync();
    void win.onResized(sync).then((unlisten) => {
      if (cancelled) unlisten();
      else stop = unlisten;
    });
    return () => {
      cancelled = true;
      stop?.();
    };
  }, []);

  return (
    <div role="toolbar" aria-label="Window controls" className="absolute right-0 top-0 z-[100] flex h-8">
      <CaptionButton label="Minimize" onClick={() => withWindow((w) => w.minimize())}>
        <Minus size={16} strokeWidth={1.25} />
      </CaptionButton>
      <CaptionButton
        label={maximized ? "Restore" : "Maximize"}
        onClick={() => withWindow((w) => w.toggleMaximize())}
      >
        {maximized ? <Copy size={13} strokeWidth={1.25} /> : <Square size={12} strokeWidth={1.25} />}
      </CaptionButton>
      <CaptionButton label="Close" danger onClick={() => withWindow((w) => w.close())}>
        <X size={16} strokeWidth={1.25} />
      </CaptionButton>
    </div>
  );
}

function CaptionButton({
  label,
  danger = false,
  onClick,
  children,
}: {
  label: string;
  danger?: boolean;
  onClick: () => void;
  children: ReactNode;
}) {
  return (
    <button
      aria-label={label}
      title={label}
      onClick={onClick}
      className={`grid h-8 w-10 place-items-center text-base-content/70 ${
        danger ? "hover:bg-[#e81123] hover:text-white" : "hover:bg-base-content/10 hover:text-base-content"
      }`}
    >
      {children}
    </button>
  );
}
