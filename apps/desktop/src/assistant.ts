import { Channel, invoke, isTauri } from "@tauri-apps/api/core";

// Mirrors src-tauri/src/ai/assistant.rs and its tools.rs and sessions.rs.

/** An email the assistant looked at, for the citations in its answers. */
export interface EmailRef {
  id: number;
  from: string;
  address: string;
  subject: string;
  /** Unix ms. */
  date: number;
}

export type CardState = "waiting" | "done" | "skipped" | "failed";

export interface DraftCard {
  account: number;
  from: string;
  to: string;
  cc: string;
  subject: string;
  /** Plain text. */
  body: string;
  replyTo?: number;
}

/** An event a card adds, as the user reads it. */
export interface EventCard {
  title: string;
  /** When, on the user's clock: "Thu, Oct 29, 1:00 – 2:00 PM". */
  when: string;
  location?: string;
  /** The calendar and its account: "Calendar · me@work.example". */
  calendar: string;
}

/** Something in the chat for the user to act on. */
export type Card =
  | {
      type: "action";
      id: string;
      /** What it does, e.g. "Mark 3 emails Done". */
      title: string;
      emails: EmailRef[];
      senders: string[];
      state: CardState;
      note?: string;
      /** The event it adds to the calendar. */
      event?: EventCard;
    }
  | { type: "draft"; id: string; draft: DraftCard };

export type Block = { type: "text"; text: string } | { type: "card"; card: Card };

export type AnswerStatus = "working" | "done" | "truncated" | "stopped" | "refused" | "failed";

export interface Answer {
  blocks: Block[];
  steps: string[];
  emails: EmailRef[];
  status: AnswerStatus;
  message?: string;
}

export type Entry = { role: "user"; text: string } | { role: "assistant"; answer: Answer };

export interface SessionInfo {
  id: string;
  title: string;
  updated: number;
}

export interface SessionView {
  id: string;
  title: string;
  entries: Entry[];
}

/** While an answer is written: any of the others, then exactly one `finished`. */
export type AssistantEvent =
  | { event: "text"; data: { text: string } }
  | { event: "step"; data: { label: string } }
  | { event: "emails"; data: { emails: EmailRef[] } }
  | { event: "card"; data: { card: Card } }
  | { event: "finished"; data: { answer: Answer } }
  | { event: "notConfigured"; data: { message: string } };

/** What's on screen as the user asks. */
export interface Screen {
  openEmail?: number;
}

export function emptyAnswer(): Answer {
  return { blocks: [], steps: [], emails: [], status: "working" };
}

/** Folds one streamed event into the answer being written. */
export function reduceAnswer(answer: Answer, event: AssistantEvent): Answer {
  switch (event.event) {
    case "text": {
      const last = answer.blocks.at(-1);
      const blocks =
        last?.type === "text"
          ? [...answer.blocks.slice(0, -1), { type: "text" as const, text: last.text + event.data.text }]
          : [...answer.blocks, { type: "text" as const, text: event.data.text }];
      return { ...answer, blocks };
    }
    case "step":
      return { ...answer, steps: [...answer.steps, event.data.label] };
    case "emails": {
      const known = new Map(answer.emails.map((e) => [e.id, e]));
      for (const e of event.data.emails) known.set(e.id, e);
      return { ...answer, emails: [...known.values()] };
    }
    case "card": {
      const card = event.data.card;
      const has = answer.blocks.some((b) => b.type === "card" && b.card.id === card.id);
      const blocks: Block[] = has
        ? answer.blocks.map((b) => (b.type === "card" && b.card.id === card.id ? { type: "card", card } : b))
        : [...answer.blocks, { type: "card", card }];
      return { ...answer, blocks };
    }
    case "finished":
      return event.data.answer;
    case "notConfigured":
      return { ...answer, status: "failed", message: event.data.message };
  }
}

/**
 * Asks in chat `session` (null for a new one). Resolves with the chat's id once it has started,
 * or null when nothing was saved (no provider set up); events arrive on `onEvent`.
 */
export function assistantSend(
  session: string | null,
  text: string,
  screen: Screen,
  onEvent: (event: AssistantEvent) => void,
): Promise<string | null> {
  if (isTauri()) {
    const events = new Channel<AssistantEvent>();
    events.onmessage = onEvent;
    return invoke("assistant_send", { session, text, screen, onEvent: events });
  }
  return Promise.resolve(preview.send(session, text, onEvent));
}

/** Stops the answer being written; resolves with it as it stands. */
export function assistantStop(session: string): Promise<Answer | null> {
  if (isTauri()) return invoke("assistant_stop", { session });
  return Promise.resolve(preview.stop(session));
}

export function assistantDecide(session: string, card: string, approve: boolean): Promise<void> {
  if (isTauri()) return invoke("assistant_decide", { session, card, approve });
  preview.decide(card, approve);
  return Promise.resolve();
}

export function assistantSessions(): Promise<SessionInfo[]> {
  if (isTauri()) return invoke("assistant_sessions");
  return Promise.resolve([...preview.sessions.values()].map(({ id, title, updated }) => ({ id, title, updated })).reverse());
}

export function assistantSession(id: string): Promise<SessionView> {
  if (isTauri()) return invoke("assistant_session", { id });
  const s = preview.sessions.get(id);
  return s ? Promise.resolve({ id, title: s.title, entries: s.entries }) : Promise.reject("This chat isn't saved anymore.");
}

export function assistantDeleteSession(id: string): Promise<void> {
  if (isTauri()) return invoke("assistant_delete_session", { id });
  preview.stop(id);
  preview.sessions.delete(id);
  return Promise.resolve();
}

// ---- browser preview (`npm run dev`) ----
// No AI runs: a labeled stand-in answer streams in, with a step, a citation and, when asked to
// archive, add to the calendar or draft, a card, so the panel can be tried.

interface PreviewSession {
  id: string;
  title: string;
  updated: number;
  entries: Entry[];
}

/** The demo mailbox's lease email, which the stand-in answer cites. */
const PREVIEW_EMAIL: EmailRef = {
  id: 1,
  from: "Dana Whitfield",
  address: "dana@whitfieldproperties.example",
  subject: "Lease renewal — signature needed by Friday",
  date: Date.now() - 3_600_000,
};

const preview = {
  sessions: new Map<string, PreviewSession>(),
  timers: new Map<string, number>(),
  waiting: new Map<string, (approve: boolean) => void>(),

  send(session: string | null, text: string, onEvent: (event: AssistantEvent) => void): string {
    const id = session ?? `p${Date.now()}`;
    const s = this.sessions.get(id) ?? { id, title: text.trim().slice(0, 80), updated: 0, entries: [] };
    s.updated = Date.now();
    s.entries.push({ role: "user", text: text.trim() });
    this.sessions.delete(id);
    this.sessions.set(id, s);
    let answer = emptyAnswer();
    s.entries.push({ role: "assistant", answer });
    const emit = (event: AssistantEvent) => {
      answer = reduceAnswer(answer, event);
      s.entries[s.entries.length - 1] = { role: "assistant", answer };
      onEvent(event);
    };
    const words = (
      `This is the browser preview, so no AI ran. In the desktop app your provider answers from your ` +
      `mail, citing the emails it used, like this one [[${PREVIEW_EMAIL.id}]].`
    ).split(/(?<=\s)/);
    const card = previewCard(text);
    const steps: (() => boolean)[] = [
      () => (emit({ event: "step", data: { label: "Checking your emails" } }), true),
      () => (emit({ event: "emails", data: { emails: [PREVIEW_EMAIL] } }), true),
      ...words.map((w) => () => (emit({ event: "text", data: { text: w } }), true)),
      ...(card ? [() => (emit({ event: "card", data: { card } }), true)] : []),
      () => {
        if (card?.type === "action" && card.state === "waiting") {
          if (!this.waiting.has(card.id)) {
            this.waiting.set(card.id, (approve) => {
              card.state = approve ? "done" : "skipped";
              emit({ event: "card", data: { card: { ...card } } });
            });
          }
          return false;
        }
        emit({ event: "finished", data: { answer: { ...answer, status: "done" } } });
        this.stop(id, false);
        return true;
      },
    ];
    let i = 0;
    this.stop(id, false);
    this.timers.set(
      id,
      window.setInterval(() => {
        if (i < steps.length && steps[i]()) i++;
      }, 40),
    );
    return id;
  },

  stop(id: string, mark = true): Answer | null {
    window.clearInterval(this.timers.get(id));
    this.timers.delete(id);
    if (!mark) return null;
    const last = this.sessions.get(id)?.entries.at(-1);
    if (last?.role !== "assistant") return null;
    if (last.answer.status === "working") last.answer = { ...last.answer, status: "stopped" };
    return last.answer;
  },

  decide(card: string, approve: boolean) {
    this.waiting.get(card)?.(approve);
    this.waiting.delete(card);
  },
};

function previewCard(question: string): Card | null {
  if (/\b(archive|done|delete|trash|snooze)\b/i.test(question)) {
    return {
      type: "action",
      id: `card${Date.now()}`,
      title: "Mark 1 email Done",
      emails: [PREVIEW_EMAIL],
      senders: [],
      state: "waiting",
    };
  }
  if (/\b(calendar|event|meeting)\b/i.test(question)) {
    return {
      type: "action",
      id: `card${Date.now()}`,
      title: "Add to calendar",
      emails: [],
      senders: [],
      state: "waiting",
      event: {
        title: "Lease signing (preview only)",
        when: "Fri, 10:00 – 10:30 AM",
        location: "Whitfield Properties, 12 Harbor St",
        calendar: "Calendar · you@example.com",
      },
    };
  }
  if (/\b(draft|write)\b/i.test(question)) {
    return {
      type: "draft",
      id: `card${Date.now()}`,
      draft: {
        account: 0,
        from: "you@example.com",
        to: PREVIEW_EMAIL.address,
        cc: "",
        subject: `Re: ${PREVIEW_EMAIL.subject}`,
        body: "Thanks for the welcome! (Preview only, not written by AI.)",
      },
    };
  }
  return null;
}
