// Settings → Appearance → Terminal colors (RiverCity fork). Like the theme, a
// display preference of this machine, so it lives in localStorage. The colors
// are worked out here as plain hex and written into one <style> scoped to the
// Terminal theme, so they win over styles.css's defaults and the other themes
// never see them. Hex, not color-mix(): `cardColor()` parses `--spark-card`.

import { useEffect, useState } from "react";
import jetbrainsMono from "@fontsource-variable/jetbrains-mono/files/jetbrains-mono-latin-wght-normal.woff2?inline";

export type EmailStyle = "original" | "font" | "colors";

export interface TerminalPrefs {
  accent: string;
  background: string;
  /** All text in the accent color, like a one-color CRT. */
  mono: boolean;
  /** How email bodies show in the Terminal theme. */
  email: EmailStyle;
}

export const ACCENTS: { label: string; hex: string }[] = [
  { label: "Green", hex: "#16c60c" },
  { label: "Amber", hex: "#ffb000" },
  { label: "Cyan", hex: "#61d6d6" },
  { label: "Blue", hex: "#3b8eea" },
  { label: "Magenta", hex: "#c678dd" },
  { label: "Red", hex: "#e74856" },
  { label: "White", hex: "#d0d0d0" },
];

export const BACKGROUNDS: { label: string; hex: string }[] = [
  { label: "Black", hex: "#0c0c0c" },
  { label: "Charcoal", hex: "#1c1c1c" },
  { label: "Navy", hex: "#0a1622" },
  { label: "Brown", hex: "#15100a" },
];

export const DEFAULT_TERMINAL: TerminalPrefs = { accent: "#16c60c", background: "#0c0c0c", mono: false, email: "font" };

const STORAGE_KEY = "spark-rs.terminal";
const CHANGED = "spark-rs.terminal-changed";
const HEX = /^#[\da-f]{6}$/i;

export function loadTerminal(): TerminalPrefs {
  try {
    const saved = JSON.parse(localStorage.getItem(STORAGE_KEY) ?? "{}") as Partial<TerminalPrefs>;
    return {
      accent: typeof saved.accent === "string" && HEX.test(saved.accent) ? saved.accent : DEFAULT_TERMINAL.accent,
      background:
        typeof saved.background === "string" && HEX.test(saved.background) ? saved.background : DEFAULT_TERMINAL.background,
      mono: saved.mono === true,
      email: saved.email === "original" || saved.email === "colors" || saved.email === "font" ? saved.email : "font",
    };
  } catch {
    return DEFAULT_TERMINAL;
  }
}

export function saveTerminal(prefs: TerminalPrefs) {
  localStorage.setItem(STORAGE_KEY, JSON.stringify(prefs));
  applyTerminal(prefs);
  // `storage` only reaches other windows; this one hears this.
  window.dispatchEvent(new Event(CHANGED));
}

type Rgb = [number, number, number];
const rgb = (hex: string): Rgb => {
  const n = parseInt(hex.slice(1), 16);
  return [n >> 16, (n >> 8) & 255, n & 255];
};
const hex = ([r, g, b]: Rgb) => "#" + [r, g, b].map((v) => Math.round(v).toString(16).padStart(2, "0")).join("");
/** `a` with `t` of `b` mixed in. */
const mix = (a: string, b: string, t: number) => {
  const [x, y] = [rgb(a), rgb(b)];
  return hex([0, 1, 2].map((i) => x[i] + (y[i] - x[i]) * t) as Rgb);
};
const alpha = (color: string, a: number) => `rgba(${rgb(color).join(", ")}, ${a})`;
const luminance = (color: string) => {
  const [r, g, b] = rgb(color).map((v) => {
    const c = v / 255;
    return c <= 0.03928 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
  });
  return 0.2126 * r + 0.7152 * g + 0.0722 * b;
};
/** Black or white, whichever reads on `color`. */
export const contrast = (color: string) => (luminance(color) > 0.18 ? "#000000" : "#ffffff");

/** The terminal font as a data: URL, for frames whose CSP only lets data: fonts in. */
export const TERMINAL_FONT_FACE = `@font-face { font-family: "Spark Terminal"; font-weight: 100 800; src: url(${jetbrainsMono}) format("woff2"); }`;
export const TERMINAL_FONT = `"Spark Terminal", "JetBrains Mono", "Cascadia Mono", Consolas, ui-monospace, monospace`;

/** The variables for these prefs: what styles.css's Terminal block sets, in these colors. */
export function terminalVars({ accent, background: bg, mono }: TerminalPrefs): Record<string, string> {
  const text = mono ? mix(accent, "#ffffff", 0.1) : "#d0d0d0";
  const on = contrast(accent);
  return {
    "--term-accent": accent,
    "--term-dim": mix(bg, accent, 0.5),
    "--term-glow": alpha(accent, 0.35),
    "--term-home": mix(bg, accent, 0.08),
    "--color-base-100": bg,
    "--color-base-200": mix(bg, "#ffffff", 0.025),
    "--color-base-300": mix(bg, "#ffffff", 0.06),
    "--color-base-content": text,
    "--color-primary": accent,
    "--color-primary-content": on,
    "--color-success": accent,
    "--color-success-content": on,
    "--spark-text-2": mono ? mix(bg, accent, 0.62) : "#8f8f8f",
    "--spark-text-3": mono ? mix(bg, accent, 0.38) : "#5c5c5c",
    "--spark-hairline": alpha(accent, 0.16),
    "--spark-splitter": alpha(accent, 0.32),
    "--spark-row-selected": mix(bg, accent, 0.16),
    "--spark-row-hover": mix(bg, accent, 0.06),
    "--spark-popover": bg,
    "--spark-card": mix(bg, "#ffffff", 0.02),
    "--spark-sidebar-rail": mix(bg, "#000000", 0.6),
    "--spark-sidebar-hover": alpha(accent, 0.14),
    "--spark-sidebar-active": mix(bg, accent, 0.45),
    "--spark-sidebar-selected": mix(bg, accent, 0.45),
    "--spark-command-bg": bg,
    "--spark-command-row": mix(bg, accent, 0.45),
    "--spark-tooltip": accent,
    "--spark-tooltip-text": on,
    "--spark-shadow-popup": `0 0 0 1px ${alpha(accent, 0.55)}, 0 0 24px ${alpha(accent, 0.1)}`,
  };
}

/** Writes the colors into the page, scoped to the Terminal theme. */
export function applyTerminal(prefs: TerminalPrefs) {
  let style = document.getElementById("spark-terminal-colors");
  if (!style) {
    style = document.createElement("style");
    style.id = "spark-terminal-colors";
    document.head.append(style);
  }
  const vars = Object.entries(terminalVars(prefs))
    .map(([name, value]) => `${name}: ${value};`)
    .join(" ");
  style.textContent = `:root[data-theme="spark-terminal"] { ${vars} }`;
}

/** The prefs, kept current as they change here or in another window. */
export function useTerminalPrefs() {
  const [prefs, setPrefs] = useState(loadTerminal);
  useEffect(() => {
    const update = () => setPrefs(loadTerminal());
    window.addEventListener(CHANGED, update);
    window.addEventListener("storage", update);
    return () => {
      window.removeEventListener(CHANGED, update);
      window.removeEventListener("storage", update);
    };
  }, []);
  return prefs;
}

/**
 * CSS for an email or signature frame in the Terminal theme: the font, and with
 * "colors", the theme's colors on everything. Only what's shown changes.
 */
export function terminalFrameCss(prefs: TerminalPrefs, card: string): string {
  if (prefs.email === "original") return "";
  const font =
    TERMINAL_FONT_FACE +
    ` body, body *:not(img, svg, svg *) { font-family: ${TERMINAL_FONT} !important; font-variant-ligatures: none; }`;
  if (prefs.email === "font") return font;
  const vars = terminalVars(prefs);
  return (
    font +
    ` :root:root { color-scheme: dark; visibility: visible !important; }` +
    ` body { background: ${card} !important; color: ${vars["--color-base-content"]} !important; }` +
    ` body *:not(img, svg, svg *) { color: inherit !important; background-color: transparent !important;` +
    ` background-image: none !important; border-color: ${vars["--spark-hairline"]} !important; box-shadow: none !important; }` +
    ` a, a * { color: ${prefs.accent} !important; }` +
    ` ::selection { background: ${prefs.accent}; color: ${contrast(prefs.accent)}; }`
  );
}
