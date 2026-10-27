// Settings → Appearance → Load remote images, Spark's setting of that name:
// on unless turned off. Off, an email with remote pictures shows a bar to load
// them, and loading them is remembered for that email, as Spark remembers it.
// Tracking pixels never load either way; the backend takes them out before an
// email gets here (`trackers.rs`). Like the theme, both are this machine's.

import { useSyncExternalStore } from "react";

const SETTING_KEY = "spark-rs.loadRemoteImages";
const ALLOWED_KEY = "spark-rs.remoteImagesAllowed";
/** How many emails' "Show images" are remembered, the latest first to stay. */
export const ALLOWED_MAX = 1000;

/**
 * An email, by its id and the date it was stored with: a new email can get the id of the newest
 * one after that's deleted, even from the same sender with the same subject, but not its date.
 * (The demo's dates move with the clock, so there it's forgotten; no demo email has remote images.)
 */
type Email = { id: number; timestamp: number };
const emailKey = (m: Email) => `${m.id}:${m.timestamp}`;

let setting: boolean | null = null;
let allowed: Set<string> | null = null;
const listeners = new Set<() => void>();

function changed() {
  setting = null;
  allowed = null;
  for (const listener of listeners) listener();
}

/** Whether a URL, as an attribute gives it, loads from the web. Browsers drop tabs and newlines in URLs. */
export function isRemoteUrl(url: string) {
  return /^(?:https?:)?\/\//i.test(url.replace(/[\t\n\r]/g, "").trim());
}

/** Whether a `srcset` offers a picture from the web. */
export function srcsetIsRemote(srcset: string) {
  return srcset.split(/[\s,]+/).some(isRemoteUrl);
}

/** Whether CSS loads anything from the web: a `url()` with a web address. */
export function cssIsRemote(css: string) {
  return /url\(\s*["']?\s*(?:https?:)?\/\//i.test(css);
}

export function loadRemoteImages() {
  return (setting ??= localStorage.getItem(SETTING_KEY) !== "off");
}

export function saveLoadRemoteImages(on: boolean) {
  localStorage.setItem(SETTING_KEY, on ? "on" : "off");
  changed();
}

/** The remembered emails, oldest first. */
function allowedList(): string[] {
  try {
    const saved: unknown = JSON.parse(localStorage.getItem(ALLOWED_KEY) ?? "[]");
    return Array.isArray(saved) ? saved.filter((k): k is string => typeof k === "string") : [];
  } catch {
    return [];
  }
}

/** Whether this email's remote pictures load: the setting's on, or the user loaded them. */
export function remoteImagesAllowed(m: Email) {
  return loadRemoteImages() || (allowed ??= new Set(allowedList())).has(emailKey(m));
}

/** Spark's "Show images": this email's pictures load from now on, after a restart too. */
export function allowRemoteImages(m: Email) {
  const key = emailKey(m);
  const list = [...allowedList().filter((k) => k !== key), key].slice(-ALLOWED_MAX);
  localStorage.setItem(ALLOWED_KEY, JSON.stringify(list));
  changed();
}

// Another window changed them.
if (typeof window !== "undefined") {
  window.addEventListener("storage", (e) => {
    if (e.key === null || e.key === SETTING_KEY || e.key === ALLOWED_KEY) changed();
  });
}

function subscribe(listener: () => void) {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

/** `loadRemoteImages`, kept current as the setting changes here or in another window. */
export function useLoadRemoteImages() {
  return useSyncExternalStore(subscribe, loadRemoteImages);
}

/** `remoteImagesAllowed`, kept current the same way. */
export function useRemoteImagesAllowed(m: Email) {
  return useSyncExternalStore(subscribe, () => remoteImagesAllowed(m));
}
