/** A `mailto:` link (RFC 6068) as the new email it asks for. */
export interface MailtoDraft {
  to: string;
  cc: string;
  bcc: string;
  subject: string;
  /** Plain text. */
  body: string;
}

/** Percent-decoding; a "+" stays a plus, as addresses use it. Bad escapes stay as they are. */
function decode(text: string) {
  try {
    return decodeURIComponent(text);
  } catch {
    return text.replace(/%([\da-f]{2})/gi, (escape, hex: string) => {
      const code = parseInt(hex, 16);
      return code < 0x80 ? String.fromCharCode(code) : escape;
    });
  }
}

/**
 * What a link asks for: its addresses, plus To, Cc, Bcc, Subject and Body from its query.
 * Other fields (attachments among them) are left out. Null if it isn't a mailto link.
 */
export function parseMailto(link: string): MailtoDraft | null {
  const match = /^mailto:([^?#]*)(?:\?([^#]*))?/i.exec(link.trim());
  if (!match) return null;
  const to = match[1] ? [decode(match[1])] : [];
  const cc: string[] = [];
  const bcc: string[] = [];
  let subject = "";
  let body = "";
  for (const pair of (match[2] ?? "").split("&")) {
    if (!pair) continue;
    const at = pair.indexOf("=");
    const key = decode(at < 0 ? pair : pair.slice(0, at)).toLowerCase();
    const value = at < 0 ? "" : decode(pair.slice(at + 1));
    if (key === "to" && value) to.push(value);
    else if (key === "cc" && value) cc.push(value);
    else if (key === "bcc" && value) bcc.push(value);
    else if (key === "subject") subject = value.replace(/[\r\n]+/g, " ");
    else if (key === "body") body = value.replace(/\r\n?/g, "\n");
  }
  const list = (fields: string[]) =>
    fields
      .flatMap((field) => field.split(","))
      .map((address) => address.trim())
      .filter(Boolean)
      .join(", ");
  return { to: list(to), cc: list(cc), bcc: list(bcc), subject, body };
}
