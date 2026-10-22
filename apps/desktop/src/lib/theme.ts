// Appearance → Theme. A display preference of this machine, so it lives in
// localStorage rather than in the core's settings.

export type ThemePref = "system" | "light" | "dark";

const STORAGE_KEY = "spark-rs.theme";

export function loadTheme(): ThemePref {
  const saved = localStorage.getItem(STORAGE_KEY);
  return saved === "light" || saved === "dark" ? saved : "system";
}

/** "system" removes `data-theme`, so the stylesheet follows `prefers-color-scheme`. */
export function applyTheme(pref: ThemePref) {
  const root = document.documentElement;
  if (pref === "system") delete root.dataset.theme;
  else root.dataset.theme = pref === "dark" ? "spark-dark" : "spark-light";
}

export function saveTheme(pref: ThemePref) {
  localStorage.setItem(STORAGE_KEY, pref);
  applyTheme(pref);
}
