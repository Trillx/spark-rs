// Run with `npm test` (Node's own test runner; it strips the types).
import assert from "node:assert/strict";
import { test } from "node:test";
import { pageList, pagedPastEnd, scrollToEnd } from "./scroll.ts";

/** A list `content` px tall on a `screen` px screen, without rows; like a browser, it keeps its place in range. */
function scroller(content: number, screen: number) {
  let top = 0;
  return {
    get scrollTop() {
      return top;
    },
    set scrollTop(y: number) {
      top = Math.max(0, Math.min(y, content - screen));
    },
    clientHeight: screen,
    scrollHeight: content,
    querySelectorAll: () => [],
    getBoundingClientRect: () => ({ top: 0 }),
  } as unknown as HTMLElement;
}

test("Page Down and End at a list's end tell it, since nothing scrolls", () => {
  const short = scroller(300, 800);
  let told = 0;
  pagedPastEnd(short, () => told++);
  pageList(short, 1);
  scrollToEnd(short, "bottom");
  assert.equal(told, 2, "a list too short to scroll");
  pageList(short, -1);
  scrollToEnd(short, "top");
  assert.equal(told, 2, "Page Up and Home don't");

  const long = scroller(5000, 800);
  told = 0;
  const stop = pagedPastEnd(long, () => told++);
  pageList(long, 1);
  assert.equal(long.scrollTop, 800);
  scrollToEnd(long, "bottom");
  assert.equal(long.scrollTop, 4200);
  assert.equal(told, 0, "they scrolled, and the scroll says so");
  pageList(long, 1);
  scrollToEnd(long, "bottom");
  assert.equal(told, 2, "at the end");
  stop();
  pageList(long, 1);
  assert.equal(told, 2, "not once the list stops listening");
  assert.doesNotThrow(() => scrollToEnd(scroller(300, 800), "bottom"), "the reader doesn't listen");
});
