import { LoaderCircle, Plus, Tag, Trash2 } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import {
  addDraftInstruction,
  getInboxAutomation,
  onAutomationProblem,
  removeDraftInstruction,
  saveInboxAutomation,
  setAutoDraftExcluded,
  type InboxAutomation,
  type InboxAutomationChange,
} from "../automation";
import { onSettingsChanged } from "../api";
import { isDemoMode, useAccounts } from "../lib/accounts";
import { AUTO_LABELS, AUTO_LABEL_ORDER } from "../lib/autoLabels";
import type { AccountId, AutoLabel } from "../types";
import AccountMark from "./AccountMark";
import { Group, INPUT, Row, StatusLine, Switch, type Status } from "./Settings";

/** What each Auto-Label is for, under its switch. */
const LABEL_DETAILS: Record<AutoLabel, string> = {
  toRespond: "Needs a reply or something done by you",
  fyi: "Worth reading, with nothing to do",
  comment: "Comments and mentions from Google Docs, Jira, GitHub, Figma…",
  notification: "Alerts, receipts, deliveries and account notices",
  meetingUpdate: "Invitations, changes and answers",
  actioned: "Done or settled. Replying moves To respond here",
  marketing: "Offers, promotions and sales outreach",
  news: "Newsletters and digests",
};

const CLEANUP_DEFAULT = 7;

/**
 * Settings → Inbox Automation: Spark's Auto-Labels and Auto-Drafts, per
 * account, and how drafts are written. Both run on the user's AI provider.
 */
export default function InboxAutomationPage({ onOpenAi }: { onOpenAi: () => void }) {
  const accounts = useAccounts();
  const demo = isDemoMode(accounts);
  const [view, setView] = useState<InboxAutomation | null>(null);
  /**
   * What the user has asked for so far. Saves go one at a time, each with all of it, and only the
   * last one's answer shows: one answered late can't undo a later choice.
   */
  const latest = useRef<InboxAutomation | null>(null);
  const saving = useRef<Promise<unknown>>(Promise.resolve());
  const saves = useRef(0);
  /**
   * The worker's problem as it last said, once it has. It says so on every change, so this is newer than
   * a save's answer, which can be read before a woken worker finds out.
   */
  const heard = useRef<{ problem?: string } | null>(null);
  const show = (v: InboxAutomation) => {
    const shown = heard.current ? { ...v, problem: heard.current.problem } : v;
    latest.current = shown;
    setView(shown);
  };
  const [status, setStatus] = useState<Status>({ kind: "idle" });
  const [instruction, setInstruction] = useState("");
  const [instructionAccount, setInstructionAccount] = useState<AccountId | "all">("all");
  const [excluding, setExcluding] = useState("");
  const [model, setModel] = useState("");

  useEffect(() => {
    void getInboxAutomation().then(
      (v) => {
        show(v);
        setModel(v.model ?? "");
      },
      (error) => setStatus({ kind: "error", text: String(error) }),
    );
  }, []);

  // Another window saved: what it changed shows here, read after this page's own saves, unless the user
  // changes something meanwhile (that save's answer shows then).
  useEffect(
    () =>
      onSettingsChanged(() => {
        const mine = saves.current;
        saving.current = saving.current
          .then(() => getInboxAutomation())
          .then((v) => {
            if (mine === saves.current) show(v);
          }, () => {});
      }),
    [],
  );

  // The worker finds out it can't work (no AI key, say) after a save returns.
  useEffect(
    () =>
      onAutomationProblem((problem) => {
        heard.current = { problem: problem ?? undefined };
        if (latest.current) show(latest.current);
      }),
    [],
  );

  if (view === null) {
    return status.kind === "error" ? <StatusLine status={status} /> : <LoaderCircle size={18} className="animate-spin text-text-2" />;
  }

  /** Saves `change`, worked out from what the user asked for last; the rest stays as saved. */
  function save(change: (asked: InboxAutomation) => InboxAutomationChange) {
    const asked = latest.current;
    if (!asked) return;
    const next = change(asked);
    // Shown at once; the last save's answer replaces it.
    show({ ...asked, ...next, model: next.model === undefined ? asked.model : (next.model ?? undefined) });
    const mine = ++saves.current;
    saving.current = saving.current
      .then(() => saveInboxAutomation(next))
      .then(
        (saved) => {
          if (mine !== saves.current) return;
          show(saved);
          setStatus((s) => (s.kind === "error" ? { kind: "idle" } : s));
        },
        (error) => {
          setStatus({ kind: "error", text: String(error) });
          // Back to what's saved, unless another save is on its way.
          if (mine === saves.current) void getInboxAutomation().then(show, () => {});
        },
      );
  }

  async function run<T>(work: Promise<T>, done: (result: T) => void) {
    try {
      done(await work);
      setStatus((s) => (s.kind === "error" ? { kind: "idle" } : s));
    } catch (error) {
      setStatus({ kind: "error", text: String(error) });
    }
  }

  const toggled = (list: AccountId[], id: AccountId) => (list.includes(id) ? list.filter((x) => x !== id) : [...list, id]);
  const labelsOn = view.labelAccounts.length > 0;
  const draftsOn = view.draftAccounts.length > 0;
  const accountName = (id?: AccountId) => accounts.find((a) => a.id === id)?.email;

  return (
    <>
      <p className="mb-4 px-1 text-[13px] leading-5 text-text-2">
        spark-rs can label new emails and draft replies to the ones that need an answer. It does this with your AI
        provider: once it's on for an account, that account's new Inbox mail goes to the provider in the background,
        a few emails at a time.
      </p>
      {demo && (
        <p className="mb-4 rounded-xl bg-base-200 px-3 py-2 text-[13px] leading-5 text-text-2">
          This is the demo mailbox. Add an account to use Auto-Labels and Auto-Drafts on your own mail.
        </p>
      )}
      {view.problem && (
        <div className="mb-4 flex flex-wrap items-center gap-x-3 gap-y-1 rounded-xl bg-error/10 px-3 py-2 text-[13px] leading-5 text-error">
          <span className="select-text">{view.problem}</span>
          <button className="font-medium text-primary hover:underline" onClick={onOpenAi}>
            Open AI settings
          </button>
        </div>
      )}

      <Group
        title="Auto-Labels"
        note="Each new email gets the one label that fits it. Gmail keeps them as labels and Microsoft 365 as Outlook categories, so they show in your other mail apps too. Turned on, the past week's mail is labeled as well."
      >
        {accounts.map((a) => (
          <Row
            key={a.id}
            label={a.email}
            htmlFor={`labels-${a.id}`}
            description={a.kind === "imap" ? "Labels stay in spark-rs: this server has nothing like them." : a.provider}
          >
            <AccountMark color={a.color} />
            <Switch
              id={`labels-${a.id}`}
              on={view.labelAccounts.includes(a.id)}
              disabled={demo}
              onToggle={() => save((v) => ({ labelAccounts: toggled(v.labelAccounts, a.id) }))}
            />
          </Row>
        ))}
      </Group>

      {labelsOn && (
        <Group title="Labels" note="A label that's off isn't given to new mail.">
          {AUTO_LABEL_ORDER.map((label) => (
            <Row
              key={label}
              label={AUTO_LABELS[label].name}
              htmlFor={`label-${label}`}
              description={LABEL_DETAILS[label]}
            >
              <Tag size={15} style={{ color: AUTO_LABELS[label].color }} />
              <Switch
                id={`label-${label}`}
                on={!view.labelsOff.includes(label)}
                onToggle={() =>
                  save((v) => ({
                    labelsOff: v.labelsOff.includes(label)
                      ? v.labelsOff.filter((l) => l !== label)
                      : [...v.labelsOff, label],
                  }))
                }
              />
            </Row>
          ))}
        </Group>
      )}

      <Group
        title="Auto-Drafts"
        note="When a new email needs a reply, a draft waits in the conversation and in Drafts, written in the email's language and signed with your signature. Nothing is sent without you."
      >
        {accounts.map((a) => (
          <Row key={a.id} label={a.email} htmlFor={`drafts-${a.id}`} description={a.provider}>
            <AccountMark color={a.color} />
            <Switch
              id={`drafts-${a.id}`}
              on={view.draftAccounts.includes(a.id)}
              disabled={demo}
              onToggle={() => save((v) => ({ draftAccounts: toggled(v.draftAccounts, a.id) }))}
            />
          </Row>
        ))}
        {draftsOn && (
          <>
            <Row
              label="Delete unused Auto-Drafts"
              htmlFor="drafts-cleanup"
              description="Drafts you haven't edited or sent are removed after this many days."
            >
              <input
                type="number"
                min={1}
                max={365}
                aria-label="Days"
                disabled={view.cleanupDays === null}
                value={view.cleanupDays ?? CLEANUP_DEFAULT}
                onChange={(e) => {
                  const days = Math.round(Number(e.target.value));
                  if (days >= 1 && days <= 365) save(() => ({ cleanupDays: days }));
                }}
                className={`${INPUT} w-16`}
              />
              <span className="text-[13px] text-text-2">days</span>
              <Switch
                id="drafts-cleanup"
                on={view.cleanupDays !== null}
                onToggle={() => save((v) => ({ cleanupDays: v.cleanupDays === null ? CLEANUP_DEFAULT : null }))}
              />
            </Row>
            <Row
              label="Create Auto-Drafts for new senders in Gatekeeper"
              htmlFor="drafts-gatekeeper"
              description="Off, people writing for the first time get no draft until you accept them."
            >
              <Switch
                id="drafts-gatekeeper"
                on={view.gatekeeperDrafts}
                onToggle={() => save((v) => ({ gatekeeperDrafts: !v.gatekeeperDrafts }))}
              />
            </Row>
          </>
        )}
      </Group>

      {draftsOn && (
        <>
          <Group
            title="Custom instructions"
            note="For example: Never use an em dash. Always answer in German from my work address. Keep replies to two sentences."
          >
            {view.instructions.map((i) => (
              <div key={i.id} className="flex min-h-[44px] items-center gap-3 px-4 py-2">
                <span className="min-w-0 flex-1 select-text text-[13px] leading-5">{i.text}</span>
                {i.accountId !== undefined && (
                  <span className="shrink-0 text-xs text-text-2">{accountName(i.accountId)}</span>
                )}
                <button
                  aria-label="Delete instruction"
                  data-tip="Delete"
                  className="grid size-7 place-items-center rounded-md text-text-2 hover:bg-base-content/10 hover:text-error"
                  onClick={() => void run(removeDraftInstruction(i.id), (list) => latest.current && show({ ...latest.current, instructions: list }))}
                >
                  <Trash2 size={15} />
                </button>
              </div>
            ))}
            <form
              className="flex flex-wrap items-center gap-2 px-4 py-2.5"
              onSubmit={(e) => {
                e.preventDefault();
                const text = instruction.trim();
                if (!text) return;
                const account = instructionAccount === "all" ? undefined : instructionAccount;
                void run(addDraftInstruction(text, account), (list) => {
                  if (latest.current) show({ ...latest.current, instructions: list });
                  setInstruction("");
                });
              }}
            >
              <input
                aria-label="Instruction"
                placeholder="Write an instruction for Auto-Drafts replies"
                value={instruction}
                onChange={(e) => setInstruction(e.target.value)}
                className={`${INPUT} min-w-0 flex-1`}
              />
              {accounts.length > 1 && (
                <select
                  aria-label="For"
                  value={instructionAccount}
                  onChange={(e) => setInstructionAccount(e.target.value === "all" ? "all" : Number(e.target.value))}
                  className="select select-sm h-8 rounded-lg border-hairline bg-base-100 text-[13px]"
                >
                  <option value="all">All accounts</option>
                  {accounts.map((a) => (
                    <option key={a.id} value={a.id}>
                      {a.email}
                    </option>
                  ))}
                </select>
              )}
              <button type="submit" className="btn btn-sm h-8 rounded-lg" disabled={!instruction.trim()}>
                <Plus size={14} /> Add
              </button>
            </form>
          </Group>

          <Group
            title="Disable Auto-Drafts for"
            note="Mail from these senders never gets a draft. Remove one to have drafts written again."
          >
            {view.excluded.length === 0 && (
              <p className="px-4 py-3 text-[13px] text-text-2">No senders yet.</p>
            )}
            {view.excluded.map((address) => (
              <div key={address} className="flex min-h-[44px] items-center gap-3 px-4 py-2">
                <span className="min-w-0 flex-1 select-text truncate text-[13px]">{address}</span>
                <button
                  aria-label={`Remove ${address}`}
                  data-tip="Remove"
                  className="grid size-7 place-items-center rounded-md text-text-2 hover:bg-base-content/10 hover:text-error"
                  onClick={() => void run(setAutoDraftExcluded(address, false), (list) => latest.current && show({ ...latest.current, excluded: list }))}
                >
                  <Trash2 size={15} />
                </button>
              </div>
            ))}
            <form
              className="flex items-center gap-2 px-4 py-2.5"
              onSubmit={(e) => {
                e.preventDefault();
                const address = excluding.trim();
                if (!address) return;
                void run(setAutoDraftExcluded(address, true), (list) => {
                  if (latest.current) show({ ...latest.current, excluded: list });
                  setExcluding("");
                });
              }}
            >
              <input
                type="email"
                aria-label="Sender's address"
                placeholder="name@example.com"
                value={excluding}
                onChange={(e) => setExcluding(e.target.value)}
                className={`${INPUT} min-w-0 flex-1`}
              />
              <button type="submit" className="btn btn-sm h-8 rounded-lg" disabled={!excluding.trim()}>
                <Plus size={14} /> Add
              </button>
            </form>
          </Group>
        </>
      )}

      {(labelsOn || draftsOn) && (
        <Group
          title="Model"
          note="Sorting runs on every new email, so a small, fast model keeps the cost down. Leave it empty to use the model in Settings → AI."
        >
          <Row label="Model for inbox automation" htmlFor="automation-model">
            <form
              className="flex items-center gap-1.5"
              onSubmit={(e) => {
                e.preventDefault();
                save(() => ({ model: model.trim() || null }));
              }}
            >
              <input
                id="automation-model"
                spellCheck={false}
                placeholder="Same as Settings → AI"
                value={model}
                onChange={(e) => setModel(e.target.value)}
                className={`${INPUT} w-56`}
              />
              <button
                type="submit"
                className="btn btn-sm h-8 rounded-lg"
                disabled={model.trim() === (view.model ?? "")}
              >
                Save
              </button>
            </form>
          </Row>
        </Group>
      )}

      <StatusLine status={status} />
    </>
  );
}
