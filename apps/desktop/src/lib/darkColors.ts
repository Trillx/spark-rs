// Spark's dark mode for HTML without one of its own: an email in the reader
// (`MessageBody`), and a signature in the composer and its editor
// (`SignatureFrame`). Only what's shown changes; mail keeps its colors.

export type Rgb = { r: number; g: number; b: number };

/** Spark's reference dark background, for when the card's color can't be read. */
const SPARK_DARK: Rgb = { r: 31, g: 33, b: 36 };

/** The message card's color in the current theme. */
export function cardColor(): Rgb {
  const card = getComputedStyle(document.documentElement).getPropertyValue("--spark-card").trim();
  return card ? rgbOf(card) : SPARK_DARK;
}

/**
 * The color an element is shown on: its own background or the nearest
 * ancestor's that's opaque. A tint, like a selected row's, doesn't count.
 */
export function backdropColor(el: Element): Rgb {
  for (let node: Element | null = el; node; node = node.parentElement) {
    const background = getComputedStyle(node).backgroundColor;
    // The browser writes an opaque sRGB color as rgb(), one with alpha as rgba().
    if (background.startsWith("rgb(")) return rgbOf(background);
  }
  return SPARK_DARK;
}

/** New colors for one element: its text's, its background's, or both. */
export interface Adapted {
  el: HTMLElement;
  color?: Rgb;
  background?: Rgb;
}

/**
 * Spark's dark mode for an email without one of its own. Text that doesn't
 * reach 4.5:1 contrast with the card gets lighter, and a background that then
 * doesn't reach it with its text gets darker, both by CIELAB lightness so
 * the hue stays. Every color is read before any changes; the caller makes them.
 */
export function darkColors(doc: Document, card: Rgb): Adapted[] {
  const view = doc.defaultView;
  if (!view || !doc.body) return [];
  const elements = [doc.body, ...doc.body.querySelectorAll("*")].filter(
    (el): el is HTMLElement => el instanceof view.HTMLElement,
  );
  const computed = elements.map((el) => {
    const style = view.getComputedStyle(el);
    return [style.color, style.backgroundColor] as const;
  });
  const parsed = new Map<string, Rgb>();
  const rgb = (color: string) => {
    let value = parsed.get(color);
    if (!value) parsed.set(color, (value = rgbOf(color)));
    return value;
  };
  const adapted: Adapted[] = [];
  elements.forEach((el, i) => {
    const [color, background] = computed[i];
    const change: Adapted = { el };
    let text = rgb(color);
    if (!readable(text, card)) change.color = text = shift(text, card, card);
    // A transparent background reads as black, which light text is readable on.
    const fill = rgb(background);
    if (!readable(fill, text)) change.background = shift(fill, text, card);
    if (change.color || change.background) adapted.push(change);
  });
  return adapted;
}

/**
 * `color` with its CIELAB lightness moved to the side of the scale away
 * from `against`: below the middle if `against` is light, above it if dark.
 * The scale is compressed above the card's own lightness, so nothing gets
 * darker than the card.
 */
function shift(color: Rgb, against: Rgb, card: Rgb): Rgb {
  const floor = lab(card).l;
  const { l, a, b } = lab(color);
  const middle = (100 + floor) / 2;
  let lightness = l * ((100 - floor) / 100) + floor;
  if (lab(against).l > middle) {
    lightness = Math.min(lightness, 2 * middle - lightness);
    lightness = ((lightness - floor) * (50 - floor)) / (middle - floor) + floor;
  } else {
    lightness = Math.max(lightness, 2 * middle - lightness);
    lightness = 100 - ((100 - lightness) * (50 - floor)) / (100 - middle);
  }
  return rgbFromLab(lightness, a, b);
}

/** WCAG AA for body text: a contrast ratio of at least 4.5:1. */
function readable(one: Rgb, other: Rgb) {
  const [light, dark] = [luminance(one), luminance(other)].sort((x, y) => y - x);
  return (light + 0.05) / (dark + 0.05) >= 4.5;
}

function luminance({ r, g, b }: Rgb) {
  const linear = (c: number) => (c / 255 <= 0.03928 ? c / 255 / 12.92 : ((c / 255 + 0.055) / 1.055) ** 2.4);
  return 0.2126 * linear(r) + 0.7152 * linear(g) + 0.0722 * linear(b);
}

/** sRGB to CIELAB under D65. */
function lab({ r, g, b }: Rgb) {
  const linear = (c: number) => (c / 255 > 0.04045 ? ((c / 255 + 0.055) / 1.055) ** 2.4 : c / 255 / 12.92);
  const [lr, lg, lb] = [linear(r), linear(g), linear(b)];
  const f = (t: number) => (t > 0.008856 ? Math.cbrt(t) : 7.787 * t + 16 / 116);
  const x = f((0.4124564 * lr + 0.3575761 * lg + 0.1804375 * lb) / 0.95047);
  const y = f(0.2126729 * lr + 0.7151522 * lg + 0.072175 * lb);
  const z = f((0.0193339 * lr + 0.119192 * lg + 0.9503041 * lb) / 1.08883);
  return { l: 116 * y - 16, a: 500 * (x - y), b: 200 * (y - z) };
}

function rgbFromLab(l: number, a: number, b: number): Rgb {
  const fy = (l + 16) / 116;
  const unf = (t: number) => (t ** 3 > 0.008856 ? t ** 3 : (t - 16 / 116) / 7.787);
  const x = unf(a / 500 + fy) * 0.95047;
  const y = unf(fy);
  const z = unf(fy - b / 200) * 1.08883;
  const channel = (c: number) => {
    const encoded = c > 0.0031308 ? 1.055 * c ** (1 / 2.4) - 0.055 : 12.92 * c;
    return Math.round(255 * Math.min(Math.max(encoded, 0), 1));
  };
  return {
    r: channel(3.2404542 * x - 1.5371385 * y - 0.4985314 * z),
    g: channel(-0.969266 * x + 1.8760108 * y + 0.041556 * z),
    b: channel(0.0556434 * x - 0.2040259 * y + 1.0572252 * z),
  };
}

/** A hex or computed `rgb()`/`rgba()` color's channels; alpha is ignored, as in Spark. */
function rgbOf(color: string): Rgb {
  const hex = /^#([\da-f]{3}|[\da-f]{6})$/i.exec(color.trim())?.[1];
  if (hex) {
    const n = parseInt(hex.length === 3 ? [...hex].map((h) => h + h).join("") : hex, 16);
    return { r: n >> 16, g: (n >> 8) & 255, b: n & 255 };
  }
  const [r = 0, g = 0, b = 0] = (/rgba?\(([^)]*)\)/i.exec(color)?.[1] ?? "")
    .split(/[\s,/]+/)
    .filter(Boolean)
    .map(Number);
  return { r, g, b };
}

export function cssColor({ r, g, b }: Rgb) {
  return `rgb(${r}, ${g}, ${b})`;
}
