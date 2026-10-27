/**
 * Links in email, Spark's way (its main process's `ConfirmExternalUrl`): web, mail and phone
 * links open; script, data and file links never do; any other scheme (an app's, like
 * `zoommtg:`) asks first. spark-rs also refuses a few Windows schemes that have been used to
 * run code from a click.
 */
export type LinkVerdict = "open" | "ask" | "refuse";

const OPEN = new Set(["http:", "https:", "mailto:", "tel:"]);
const REFUSE = new Set([
  "javascript:",
  "data:",
  "vbscript:",
  "file:",
  "sftp:",
  "about:",
  "blob:",
  // Not Spark's: Windows handlers attackers have used from links.
  "ms-msdt:",
  "search-ms:",
  "search:",
  "ms-officecmd:",
]);

/** What to do with a link's address, and its scheme ("zoommtg:"); relative or broken ones are refused. */
export function linkVerdict(href: string | null | undefined): { verdict: LinkVerdict; url: string; scheme: string } {
  let url: URL;
  try {
    url = new URL((href ?? "").trim());
  } catch {
    return { verdict: "refuse", url: "", scheme: "" };
  }
  const scheme = url.protocol.toLowerCase();
  const verdict = REFUSE.has(scheme) ? "refuse" : OPEN.has(scheme) ? "open" : "ask";
  return { verdict, url: url.href, scheme };
}

/** Where links go: `mailto:` to a new email here, others out of the app, an odd scheme past a question. */
export interface LinkHandlers {
  mailto: (url: string) => void;
  external: (url: string) => void;
  ask: (url: string, scheme: string) => void;
}

let handlers: LinkHandlers | null = null;

/** The app sets these once it can compose and ask. */
export function setLinkHandlers(next: LinkHandlers | null) {
  handlers = next;
}

/** Opens a link from an email, as `linkVerdict` says. */
export function openLink(href: string | null | undefined) {
  const { verdict, url, scheme } = linkVerdict(href);
  if (verdict === "refuse" || !handlers) return;
  if (verdict === "ask") return handlers.ask(url, scheme);
  if (scheme === "mailto:") return handlers.mailto(url);
  handlers.external(url);
}
