// Ctrl+wheel zoom: WebView2's own is off in Tauri, so the app turns the wheel into View → Zoom steps.

/** How much wheel travel makes one zoom step: a mouse notch is about 100px, a pinch far less. */
const WHEEL_STEP = 50;
/** A pause this long between wheel events starts a fresh gesture. */
const WHEEL_PAUSE_MS = 250;

/**
 * Ctrl+wheel (and a touchpad pinch, which arrives as one) as zoom steps. Each event's travel adds
 * up, so a mouse notch is one step and a pinch zooms as far as the fingers move. Returns 1 to zoom
 * in, -1 to zoom out, or 0 while the travel hasn't reached a step yet.
 */
export function wheelZoom() {
  let travel = 0;
  let last = -Infinity;
  return (e: { deltaY: number; deltaMode: number; timeStamp: number }): 1 | -1 | 0 => {
    if (e.timeStamp - last > WHEEL_PAUSE_MS) travel = 0;
    last = e.timeStamp;
    // Lines and pages, from some mice, as pixels.
    travel += e.deltaY * (e.deltaMode === 1 ? 40 : e.deltaMode === 2 ? 800 : 1);
    if (Math.abs(travel) < WHEEL_STEP) return 0;
    const step = travel < 0 ? 1 : -1;
    travel = 0;
    return step;
  };
}

/**
 * A frame's Ctrl+wheel never reaches the app's window; this hands it to `host` (the <iframe>), as
 * if the frame were part of the page. Add it to the frame's document with `{ passive: false }`.
 */
export function forwardWheelZoom(host: EventTarget) {
  return (e: WheelEvent) => {
    if (!e.ctrlKey && !e.metaKey) return;
    e.preventDefault();
    host.dispatchEvent(
      new WheelEvent("wheel", {
        deltaY: e.deltaY,
        deltaMode: e.deltaMode,
        ctrlKey: e.ctrlKey,
        metaKey: e.metaKey,
        bubbles: true,
        cancelable: true,
      }),
    );
  };
}
