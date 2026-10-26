import { useEffect, useRef, useState } from "react";
import { checkForUpdates, getUpdates, installUpdate, onUpdateProgress, onUpdatesChanged } from "../api";
import type { UpdateProgress, UpdatesView } from "../types";

/** One subscription for About and the sidebar, even while Settings is closed. */
export function useUpdates(onUpdated: (build: number) => void) {
  const [view, setView] = useState<UpdatesView | null>(null);
  const [progress, setProgress] = useState<UpdateProgress | null>(null);
  const [error, setError] = useState<string | null>(null);
  const updated = useRef(onUpdated);
  updated.current = onUpdated;

  useEffect(() => {
    let live = true;
    const apply = (next: UpdatesView) => {
      if (!live) return;
      setView(next);
      setError(null);
      if (next.canUpdate && next.build !== null) {
        try {
          const previous = localStorage.getItem("spark-rs:last-build");
          localStorage.setItem("spark-rs:last-build", String(next.build));
          if (previous !== null && Number(previous) < next.build) updated.current(next.build);
        } catch {
          /* Updates work even when webview storage is unavailable. */
        }
      }
    };
    const stops = [onUpdatesChanged(apply), onUpdateProgress(setProgress)];
    void getUpdates().then(apply, (e) => live && setError(String(e)));
    return () => {
      live = false;
      stops.forEach((stop) => stop());
    };
  }, []);

  async function check() {
    setError(null);
    try {
      setView(await checkForUpdates());
    } catch (e) {
      setError(String(e));
    }
  }

  async function install() {
    setError(null);
    setProgress(null);
    try {
      await installUpdate();
    } catch (e) {
      setError(String(e));
    }
  }

  return { view, progress, error, check, install };
}

export type Updates = ReturnType<typeof useUpdates>;
