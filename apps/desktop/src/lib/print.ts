import type { InlineImages } from "../types";
import type { MarkdownHeader } from "./markdown";
import { cidKey, escapeHtml } from "./richText";

/** An email to print: who, when and what, its body, and which of its pictures may load. */
export interface PrintEmail {
  header: MarkdownHeader;
  /** Untrusted HTML, or null for a text email (`text`). */
  html: string | null;
  text: string[];
  /** Its remote pictures may load: Load remote images is on, or the user showed them. */
  remoteImages: boolean;
  inlineImages: InlineImages | null;
}

const PRINT_CSS = `
:root { color-scheme: light; }
body { margin: 0; color: #141414; background: #fff; font: 13px/1.45 -apple-system, BlinkMacSystemFont, "Segoe UI", system-ui, Roboto, sans-serif; }
h1 { font-size: 18px; margin: 0 0 14px; }
section + section { border-top: 1px solid #ccc; margin-top: 22px; padding-top: 14px; }
.head { margin-bottom: 12px; color: #444; }
.head b { color: #141414; }
.text { white-space: pre-wrap; overflow-wrap: anywhere; }
.body { overflow-wrap: anywhere; }
`;

/**
 * An email's HTML as it prints: nothing that runs, its embedded pictures in, and with `remote`
 * off, no remote ones (the frame's CSP also blocks them, CSS backgrounds and all).
 */
function printableHtml(email: PrintEmail, remote: boolean) {
  const doc = new DOMParser().parseFromString(email.html ?? "", "text/html");
  for (const el of doc.querySelectorAll("script, meta, base, link, iframe, frame, frameset, object, embed, form")) el.remove();
  for (const el of doc.querySelectorAll("*")) {
    for (const attr of [...el.attributes]) if (/^on/i.test(attr.name)) el.removeAttribute(attr.name);
  }
  for (const img of doc.querySelectorAll("img")) {
    const src = img.getAttribute("src") ?? "";
    if (/^\s*cid:/i.test(src)) {
      const data = email.inlineImages?.[cidKey(src)];
      if (data) img.setAttribute("src", data);
      else img.remove();
    } else if (!remote && !/^\s*data:/i.test(src)) {
      img.removeAttribute("src");
    }
    if (!remote) img.removeAttribute("srcset");
  }
  if (!remote) for (const source of doc.querySelectorAll("source")) source.remove();
  const styles = [...doc.querySelectorAll("style")].map((s) => s.outerHTML).join("");
  return styles + doc.body.innerHTML;
}

function headerHtml(h: MarkdownHeader) {
  const line = (label: string, value: string) => `<div><b>${label}:</b> ${escapeHtml(value)}</div>`;
  return (
    line("From", h.from) +
    (h.to.length ? line("To", h.to.join(", ")) : "") +
    (h.cc.length ? line("Cc", h.cc.join(", ")) : "") +
    line("Date", h.date) +
    line("Subject", h.subject)
  );
}

/**
 * Prints emails (one, or a conversation) through the system's print dialog, which on Windows
 * also saves as PDF. They print in a frame sandboxed without scripts, each body in a shadow
 * root of its own so one email's styles can't reach another's.
 */
export function printEmails(title: string, emails: PrintEmail[]) {
  // One frame, one policy: remote pictures load only if every email printed allows them.
  const remote = emails.length > 0 && emails.every((e) => e.remoteImages);
  const imgSrc = remote ? "data: https: http:" : "data:";
  const sections = emails
    .map(
      (e, i) =>
        `<section><div class="head">${headerHtml(e.header)}</div>` +
        (e.html !== null
          ? `<div class="body" data-email="${i}"></div>`
          : `<div class="text">${escapeHtml(e.text.join("\n\n"))}</div>`) +
        "</section>",
    )
    .join("");
  const frame = document.createElement("iframe");
  // allow-modals lets this side call print(); with no allow-scripts, nothing in the emails runs.
  frame.setAttribute("sandbox", "allow-same-origin allow-modals");
  frame.setAttribute("aria-hidden", "true");
  frame.style.cssText = "position:fixed;right:0;bottom:0;width:0;height:0;border:0;";
  frame.srcdoc =
    "<!doctype html><html><head>" +
    `<meta http-equiv="Content-Security-Policy" content="default-src 'none'; img-src ${imgSrc}; style-src 'unsafe-inline'; font-src data:">` +
    `<meta name="referrer" content="no-referrer"><title>${escapeHtml(title)}</title><style>${PRINT_CSS}</style></head>` +
    `<body><h1>${escapeHtml(title)}</h1>${sections}</body></html>`;
  const remove = () => frame.remove();
  frame.addEventListener(
    "load",
    () => {
      const doc = frame.contentDocument;
      const win = frame.contentWindow;
      if (!doc || !win) return remove();
      emails.forEach((e, i) => {
        const host = doc.querySelector(`[data-email="${i}"]`);
        if (host && e.html !== null) host.attachShadow({ mode: "open" }).innerHTML = printableHtml(e, remote);
      });
      // The pictures in the bodies load once they're in; print when they have (or soon anyway).
      const pending = [...doc.querySelectorAll("[data-email]")].flatMap((host) =>
        [...(host.shadowRoot?.querySelectorAll("img") ?? [])].filter((img) => img.getAttribute("src") && !img.complete),
      );
      const loaded = Promise.all(
        pending.map(
          (img) =>
            new Promise<void>((done) => {
              img.addEventListener("load", () => done());
              img.addEventListener("error", () => done());
            }),
        ),
      );
      void Promise.race([loaded, new Promise((done) => setTimeout(done, 3000))]).then(() => {
        win.addEventListener("afterprint", remove, { once: true });
        win.print();
        // Some print dialogs don't say when they're done.
        setTimeout(remove, 10 * 60_000);
      });
    },
    { once: true },
  );
  document.body.append(frame);
}
