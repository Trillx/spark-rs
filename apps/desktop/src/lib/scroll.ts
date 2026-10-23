// Keyboard scrolling, Spark's way: in the message list Page Down and Page Up
// move a screen and line a row up with the top; Home and End (or Cmd/Ctrl+↑
// and ↓) go to either end. The reader pages without lining anything up.

/** The list's rows, group rows and date headers: what paging lines up, and what slides when rows leave. */
export const LIST_ITEMS = "[data-row], [data-group], [data-period]";

/** Where an element's top sits in its scroller's content. */
function topIn(scroller: HTMLElement, el: Element) {
  return el.getBoundingClientRect().top - scroller.getBoundingClientRect().top + scroller.scrollTop;
}

/**
 * Page Down puts the last row that shows (even partly) at the top. Page Up
 * goes back about as far, to the first row that starts within the screen
 * above.
 */
export function pageList(scroller: HTMLElement, dir: 1 | -1) {
  const { scrollTop, clientHeight } = scroller;
  const tops = [...scroller.querySelectorAll(LIST_ITEMS)].map((el) => topIn(scroller, el));
  let target: number | undefined;
  if (dir === 1) {
    target = tops.filter((top) => top < scrollTop + clientHeight).at(-1);
    // A row taller than the screen: just move a screen.
    if (target === undefined || target <= scrollTop) target = scrollTop + clientHeight;
  } else {
    target = tops.find((top) => top >= scrollTop - clientHeight && top < scrollTop) ?? scrollTop - clientHeight;
  }
  scroller.scrollTop = Math.max(0, target);
}

/** A screen at a time, keeping a line of the last one in view. */
export function pageReader(scroller: HTMLElement, dir: 1 | -1) {
  scroller.scrollTop += dir * Math.max(scroller.clientHeight - 40, scroller.clientHeight / 2);
}

export function scrollToEnd(scroller: HTMLElement, end: "top" | "bottom") {
  scroller.scrollTop = end === "top" ? 0 : scroller.scrollHeight;
}
