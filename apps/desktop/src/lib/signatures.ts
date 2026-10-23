import type { Signature } from "../types";

/** Signatures are named by their first line, as Spark lists them. */
export function signatureLabel(s: Signature) {
  return s.text.split("\n").find((line) => line.trim())?.trim() ?? "[Image]";
}

/** Embedded images a signature may carry, each at most this big (the Rust side checks too). */
export const MAX_IMAGE_BYTES = 1024 * 1024;
/** Wider images are made this wide: wider than any signature needs. */
const MAX_IMAGE_WIDTH = 600;
/** Smaller images go in as they are. */
const SMALL_IMAGE_BYTES = 200 * 1024;

/**
 * An image for a signature, as a data URL: as it is if it's small, else
 * redrawn at most 600px wide. `resized` says it was made smaller. Rejects
 * with a user-facing reason.
 */
export async function fitImage(file: Blob): Promise<{ url: string; resized: boolean }> {
  if (!file.type.startsWith("image/")) throw new Error("That isn't an image.");
  const url = await dataUrl(file);
  const img = await loaded(url);
  if (file.size <= SMALL_IMAGE_BYTES && img.naturalWidth <= MAX_IMAGE_WIDTH) return { url, resized: false };
  const scale = Math.min(1, MAX_IMAGE_WIDTH / img.naturalWidth);
  const canvas = document.createElement("canvas");
  canvas.width = Math.max(1, Math.round(img.naturalWidth * scale));
  canvas.height = Math.max(1, Math.round(img.naturalHeight * scale));
  canvas.getContext("2d")!.drawImage(img, 0, 0, canvas.width, canvas.height);
  // PNG keeps logos and transparency sharp; photos are smaller as JPEG.
  let fitted = canvas.toDataURL(file.type === "image/png" || file.type === "image/gif" ? "image/png" : "image/jpeg", 0.9);
  if (dataBytes(fitted) > MAX_IMAGE_BYTES) fitted = canvas.toDataURL("image/jpeg", 0.8);
  if (dataBytes(fitted) > MAX_IMAGE_BYTES) throw new Error("That image is too big for a signature, even made smaller.");
  return { url: fitted, resized: true };
}

function dataUrl(file: Blob): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => resolve(reader.result as string);
    reader.onerror = () => reject(new Error("Couldn't read the image."));
    reader.readAsDataURL(file);
  });
}

function loaded(url: string): Promise<HTMLImageElement> {
  return new Promise((resolve, reject) => {
    const img = new Image();
    img.onload = () => resolve(img);
    img.onerror = () => reject(new Error("Couldn't read the image."));
    img.src = url;
  });
}

/** The decoded size of a base64 data URL. */
function dataBytes(url: string) {
  const data = url.slice(url.indexOf(",") + 1);
  return Math.floor((data.length * 3) / 4);
}

/**
 * Takes the fonts out of a signature, so it's written in the same font as
 * the message: Spark's "Reset fonts".
 */
export function resetFonts(root: HTMLElement) {
  for (const el of root.querySelectorAll<HTMLElement>("[style]")) {
    el.style.removeProperty("font-family");
    el.style.removeProperty("font-size");
    el.style.removeProperty("font");
    if (!el.getAttribute("style")?.trim()) el.removeAttribute("style");
  }
  for (const font of root.querySelectorAll("font")) {
    font.removeAttribute("face");
    font.removeAttribute("size");
  }
}

/** HTML as text, a line per block; like the Rust side's, for the browser preview. */
export function htmlText(html: string) {
  const doc = new DOMParser().parseFromString(html, "text/html");
  for (const br of doc.querySelectorAll("br")) br.replaceWith("\n");
  for (const block of doc.querySelectorAll("div, p, li, tr, h1, h2, h3, h4, h5, h6, blockquote")) block.append("\n");
  return (doc.body.textContent ?? "")
    .split("\n")
    .map((line) => line.replace(/\s+/g, " ").trim())
    .join("\n")
    .replace(/\n{3,}/g, "\n\n")
    .trim();
}
