// Settings → Composer → Reply in the conversation (RiverCity fork). On, a reply or forward is
// written under the conversation's last email; off, in the floating window. A preference of
// this machine, like the theme.

import { useEffect, useState } from "react";

const KEY = "spark-rs.replyInline";
const CHANGED = "spark-rs.reply-inline-changed";

export function loadReplyInline() {
  return localStorage.getItem(KEY) !== "off";
}

export function saveReplyInline(on: boolean) {
  localStorage.setItem(KEY, on ? "on" : "off");
  window.dispatchEvent(new Event(CHANGED));
}

/** The setting, kept current as it changes here or in another window. */
export function useReplyInline() {
  const [on, setOn] = useState(loadReplyInline);
  useEffect(() => {
    const update = () => setOn(loadReplyInline());
    window.addEventListener(CHANGED, update);
    window.addEventListener("storage", update);
    return () => {
      window.removeEventListener(CHANGED, update);
      window.removeEventListener("storage", update);
    };
  }, []);
  return on;
}
