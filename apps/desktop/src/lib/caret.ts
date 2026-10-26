/** Styles that decide where a textarea's text wraps. */
const COPIED = [
  "boxSizing",
  "width",
  "paddingTop",
  "paddingRight",
  "paddingBottom",
  "paddingLeft",
  "borderTopWidth",
  "borderRightWidth",
  "borderBottomWidth",
  "borderLeftWidth",
  "fontFamily",
  "fontSize",
  "fontWeight",
  "fontStyle",
  "letterSpacing",
  "lineHeight",
  "textTransform",
  "wordSpacing",
  "tabSize",
] as const;

/**
 * Where text position `index` of a textarea is on screen: the top left of its line, and the
 * line's height. Measured on a hidden copy of the textarea that wraps the same way.
 */
export function caretPoint(el: HTMLTextAreaElement, index: number) {
  const style = getComputedStyle(el);
  const copy = document.createElement("div");
  for (const name of COPIED) copy.style[name] = style[name];
  Object.assign(copy.style, {
    position: "absolute",
    visibility: "hidden",
    top: "0",
    left: "-9999px",
    whiteSpace: "pre-wrap",
    overflowWrap: "break-word",
  });
  copy.textContent = el.value.slice(0, index);
  const marker = document.createElement("span");
  marker.textContent = "​";
  copy.append(marker);
  document.body.append(copy);
  const box = el.getBoundingClientRect();
  const point = {
    left: box.left + marker.offsetLeft - el.scrollLeft,
    top: box.top + marker.offsetTop - el.scrollTop,
    height: parseFloat(style.lineHeight) || marker.offsetHeight,
  };
  copy.remove();
  return point;
}
