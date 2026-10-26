/** The taskbar badge on Windows: an overlay icon with the count, drawn here. */

/** Same size as `set_badge` expects. */
const SIZE = 32;

/** A red disc with `count` in it (99+ past 99), as 32×32 RGBA; null if the canvas can't draw. */
export function badgeIcon(count: number): number[] | null {
  const canvas = document.createElement("canvas");
  canvas.width = SIZE;
  canvas.height = SIZE;
  const ctx = canvas.getContext("2d");
  if (!ctx) return null;
  ctx.fillStyle = "#e5484d";
  ctx.beginPath();
  ctx.arc(SIZE / 2, SIZE / 2, SIZE / 2, 0, Math.PI * 2);
  ctx.fill();
  const text = count > 99 ? "99+" : String(count);
  const px = [0, 22, 18, 13][text.length];
  ctx.fillStyle = "#ffffff";
  ctx.font = `600 ${px}px "Segoe UI", system-ui, sans-serif`;
  ctx.textAlign = "center";
  ctx.textBaseline = "middle";
  ctx.fillText(text, SIZE / 2, SIZE / 2 + 1);
  return Array.from(ctx.getImageData(0, 0, SIZE, SIZE).data);
}
