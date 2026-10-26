import type { Message, Screen, SenderChange, SenderDecision } from "../types";

// The Gatekeeper's decisions as the store applies them to mail (see
// crates/store/src/gatekeeper.rs): the UI uses this to show a decision at
// once, and the browser preview as its store.

/** The domain of an address, lowercased. */
export function domainOf(address: string): string | undefined {
  const at = address.lastIndexOf("@");
  const domain = at < 0 ? "" : address.slice(at + 1).trim().toLowerCase();
  return domain || undefined;
}

/** Free mail providers, as in `PUBLIC_DOMAINS` in gatekeeper.rs: the store refuses to decide about them as a whole. */
const PUBLIC_DOMAINS = new Set([
  "gmail.com", "googlemail.com", "outlook.com", "hotmail.com", "live.com", "msn.com", "yahoo.com", "ymail.com",
  "rocketmail.com", "icloud.com", "me.com", "mac.com", "aol.com", "proton.me", "protonmail.com", "pm.me", "gmx.com",
  "gmx.net", "gmx.de", "web.de", "mail.com", "zoho.com", "fastmail.com", "hey.com", "tutanota.com", "tuta.io",
  "yandex.com", "yandex.ru", "mail.ru", "qq.com", "163.com", "126.com", "comcast.net", "verizon.net", "att.net",
  "sbcglobal.net", "btinternet.com", "orange.fr", "free.fr", "t-online.de", "libero.it", "seznam.cz", "naver.com",
]);
const PUBLIC_PREFIXES = ["hotmail.", "outlook.", "live.", "yahoo.", "gmx.", "yandex."];

/** The domain of `address` if it can be accepted or blocked as a whole: not a free mail provider's. */
export function decidableDomain(address: string): string | undefined {
  const domain = domainOf(address);
  if (!domain || PUBLIC_DOMAINS.has(domain) || PUBLIC_PREFIXES.some((p) => domain.startsWith(p))) return undefined;
  return domain;
}

export const sender = (address: string): Screen => ({ sender: address });
export const domain = (name: string): Screen => ({ domain: name });

/** Whether a decision about `screen` is about mail from `address`. */
export function covers(screen: Screen, address: string): boolean {
  return "sender" in screen
    ? screen.sender.toLowerCase() === address.toLowerCase()
    : domainOf(address) === screen.domain.replace(/^@/, "").toLowerCase();
}

/** `m` once its sender's decision is `decision`: blocked inbox mail goes to Blocked; accepted or new, Blocked mail comes back. */
export function settled(m: Message, decision: SenderDecision | null): Message {
  if (decision === "blocked") return m.state === "inbox" ? { ...m, state: "blocked", screening: false } : m;
  const state = m.state === "blocked" ? "inbox" : m.state;
  const screening = decision === null ? m.screening || state === "inbox" : false;
  return { ...m, state, screening };
}

/** The mail as it'll be after `changes` about single senders; domain decisions wait for the store. */
export function predict(messages: Message[], changes: SenderChange[]): Message[] {
  return messages.map((m) => {
    const change = changes.find((c) => "sender" in c.screen && covers(c.screen, m.address));
    return change ? settled(m, change.decision) : m;
  });
}

/** One change per sender of `items`, each address once. */
export function forSenders(items: Message[], decision: SenderDecision | null): SenderChange[] {
  const seen = new Set<string>();
  const changes: SenderChange[] = [];
  for (const m of items) {
    const key = m.address.toLowerCase();
    if (seen.has(key)) continue;
    seen.add(key);
    changes.push({ screen: sender(m.address), decision });
  }
  return changes;
}
