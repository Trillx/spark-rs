import type { AccountId, OutgoingAttachment, OutgoingDraft } from "../types";
import type { Later } from "./format";

export interface Draft {
  title: string;
  to: string;
  cc: string;
  bcc: string;
  showCc: boolean;
  showBcc: boolean;
  subject: string;
  body: string;
  from: AccountId;
  sendLater: Later | null;
  doneAfter: boolean;
  /** The message being replied to or forwarded; "Mark as Done after sending" applies to it. */
  replyTo?: number;
  /** `replyTo` is forwarded, not answered, so the message isn't sent as a reply. */
  forward?: boolean;
  /** Files from disk, and a forwarded message's own attachments. */
  attachments: OutgoingAttachment[];
  /** The saved draft this edits (opened from Drafts, or saved before). */
  draftId?: number;
  /** A reply's threading when its original isn't stored here. */
  inReplyTo?: string;
  references?: string;
  /** The signature below the body, as HTML (it can be edited for this message); null for none. */
  signature: string | null;
  /** The saved signature it started as. */
  signatureId: number | null;
  /** Hidden for this message: it's sent without it, and Add Signature brings it back. */
  signatureHidden: boolean;
  /** The account the signature was chosen for. Another From chooses again. */
  signatureFor?: AccountId;
  /** A forward's original, below the signature. */
  quote?: string;
  /** "Remind me" (if nobody replies, with `ifNoReply`). */
  remind: (Later & { ifNoReply: boolean }) | null;
  /** The From account's automatic Cc and Bcc as they were added, so another From swaps them. */
  auto?: { cc: string[]; bcc: string[] };
}


export function emptyDraft(from: AccountId): Draft {
  return {
    title: "New message",
    to: "",
    cc: "",
    bcc: "",
    showCc: false,
    showBcc: false,
    subject: "",
    body: "",
    from,
    sendLater: null,
    remind: null,
    doneAfter: false,
    attachments: [],
    signature: null,
    signatureId: null,
    signatureHidden: false,
  };
}

/** A signature alone isn't content: a new message with just one is empty. */
export function draftHasContent(d: Draft) {
  return [d.to, d.cc, d.bcc, d.subject, d.body, d.quote ?? ""].some((s) => s.trim()) || d.attachments.length > 0;
}

/** The signature the draft goes out with, if any. */
function sentSignature(d: Draft) {
  return d.signatureHidden ? null : d.signature;
}

/** What a saved draft keeps, to tell whether there's anything new to save. */
export function draftKey(d: Draft) {
  return JSON.stringify([d.from, d.to, d.cc, d.bcc, d.subject, d.body, d.attachments, sentSignature(d), d.quote, d.sendLater]);
}

/** The draft as the backend takes it, to save or to send at `sendAt`. */
export function outgoingDraft(d: Draft, sendAt: number, draftId = d.draftId): OutgoingDraft {
  return {
    account: d.from,
    to: d.to,
    cc: d.cc,
    bcc: d.bcc,
    subject: d.subject,
    body: d.body,
    // A forward isn't a reply; it only carries replyTo for "Mark as Done after sending".
    replyTo: d.forward ? undefined : d.replyTo,
    sendAt,
    sendLater: d.sendLater ?? undefined,
    attachments: d.attachments,
    draftId,
    inReplyTo: d.inReplyTo,
    references: d.references,
    signature: sentSignature(d) ?? undefined,
    signatureId: sentSignature(d) !== null ? (d.signatureId ?? undefined) : undefined,
    quote: d.quote,
    remind: d.remind ? { at: d.remind.at, ifNoReply: d.remind.ifNoReply } : undefined,
  };
}

/** A draft from Drafts, back in the composer. */
export function savedDraft(saved: OutgoingDraft): Draft {
  return {
    ...emptyDraft(saved.account),
    title: "Draft",
    to: saved.to,
    cc: saved.cc,
    bcc: saved.bcc,
    showCc: saved.cc.trim() !== "",
    showBcc: saved.bcc.trim() !== "",
    subject: saved.subject,
    body: saved.body,
    sendLater: saved.sendLater ?? null,
    replyTo: saved.replyTo,
    attachments: saved.attachments,
    draftId: saved.draftId,
    inReplyTo: saved.inReplyTo,
    references: saved.references,
    // It keeps the signature it was saved with, or none.
    signature: saved.signature ?? null,
    signatureId: saved.signatureId ?? null,
    signatureFor: saved.account,
    quote: saved.quote,
    remind: saved.remind ? { key: "custom", ...saved.remind } : null,
  };
}

