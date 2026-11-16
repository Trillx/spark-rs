// Appearance → Theme. A display preference of this machine, so it lives in
// localStorage rather than in the core's settings.

import { useEffect, useState } from "react";

export type ThemePref = "system" | "light" | "dark" | "terminal";

const STORAGE_KEY = "spark-rs.theme";
const SYSTEM_DARK = "(prefers-color-scheme: dark)";

export function loadTheme(): ThemePref {
  const saved = localStorage.getItem(STORAGE_KEY);
  // Terminal is this fork's default: a machine that never chose a theme gets it.
  if (saved === null) return "terminal";
  return saved === "light" || saved === "dark" || saved === "terminal" ? saved : "system";
}

/** "system" removes `data-theme`, so the stylesheet follows `prefers-color-scheme`. */
export function applyTheme(pref: ThemePref) {
  const root = document.documentElement;
  if (pref === "system") delete root.dataset.theme;
  else root.dataset.theme = pref === "terminal" ? "spark-terminal" : pref === "dark" ? "spark-dark" : "spark-light";
}

export function saveTheme(pref: ThemePref) {
  localStorage.setItem(STORAGE_KEY, pref);
  applyTheme(pref);
}

/** Whether the app shows its dark theme now: chosen, or the system's. */
function isDark() {
  const chosen = document.documentElement.dataset.theme;
  return chosen ? chosen !== "spark-light" : matchMedia(SYSTEM_DARK).matches;
}

/** `isDark`, kept current as the setting or the system changes. */
export function useDarkTheme() {
  const [dark, setDark] = useState(isDark);
  useEffect(() => {
    const update = () => setDark(isDark());
    const system = matchMedia(SYSTEM_DARK);
    system.addEventListener("change", update);
    const chosen = new MutationObserver(update);
    chosen.observe(document.documentElement, { attributes: true, attributeFilter: ["data-theme"] });
    update();
    return () => {
      system.removeEventListener("change", update);
      chosen.disconnect();
    };
  }, []);
  return dark;
}

/** Whether the Terminal theme is showing, kept current as the setting changes. */
export function useTerminalTheme() {
  const read = () => document.documentElement.dataset.theme === "spark-terminal";
  const [on, setOn] = useState(read);
  useEffect(() => {
    const chosen = new MutationObserver(() => setOn(read()));
    chosen.observe(document.documentElement, { attributes: true, attributeFilter: ["data-theme"] });
    setOn(read());
    return () => chosen.disconnect();
  }, []);
  return on;
}
