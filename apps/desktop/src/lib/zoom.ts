// View → Zoom: the whole window's content, kept on this machine like the theme.

import { isTauri } from "@tauri-apps/api/core";
import { getCurrentWebview } from "@tauri-apps/api/webview";

const STORAGE_KEY = "spark-rs.zoom";
/** The steps Zoom In and Zoom Out go through, as browsers have them. */
const LEVELS = [0.67, 0.75, 0.8, 0.9, 1, 1.1, 1.25, 1.5, 1.75, 2];

export function loadZoom(): number {
  const saved = Number(localStorage.getItem(STORAGE_KEY));
  return LEVELS.includes(saved) ? saved : 1;
}

export function applyZoom(level: number) {
  if (isTauri()) void getCurrentWebview().setZoom(level).catch(() => {});
  else document.documentElement.style.setProperty("zoom", String(level));
}

/** One step in (1) or out (-1), or back to actual size (0). Returns the new level. */
export function stepZoom(step: 1 | -1 | 0): number {
  const current = LEVELS.indexOf(loadZoom());
  const next = step === 0 ? 1 : LEVELS[Math.min(LEVELS.length - 1, Math.max(0, current + step))];
  localStorage.setItem(STORAGE_KEY, String(next));
  applyZoom(next);
  return next;
}
