import {
  ArrowLeft,
  Bell,
  Bold,
  Bot,
  Building2,
  Check,
  ChevronRight,
  ChevronsUpDown,
  Code,
  ImagePlus,
  Italic,
  Link,
  RemoveFormatting,
  Signature as SignatureIcon,
  Underline,
  CircleAlert,
  CircleCheck,
  CircleX,
  Copy,
  Ellipsis,
  Eye,
  House,
  Inbox,
  Info,
  LayoutTemplate,
  KeyRound,
  LoaderCircle,
  Mail,
  Monitor,
  Moon,
  Palette,
  Pencil,
  Plus,
  RefreshCw,
  Send,
  Server,
  Sparkles,
  SquarePen,
  Sun,
  Trash2,
  UserCheck,
  UsersRound,
  X,
  Zap,
  type LucideIcon,
} from "lucide-react";
import { useEffect, useRef, useState, type FormEvent, type KeyboardEvent, type ReactNode } from "react";
import {
  EFFORTS,
  KEY_STORE,
  KNOWN_MODELS,
  PRESETS,
  getAiSettings,
  listAiModels,
  presetDefaults,
  presetLabel,
  saveAiSettings,
  testAiConnection,
  type AiSettings,
  type Effort,
  type Preset,
  type ProviderConfig,
} from "../ai";
import {
  addGmailAccount,
  addImapAccount,
  addMicrosoftAccount,
  cancelSignIn,
  deleteSignature,
  failureOf,
  getAgents,
  getPrefs,
  mailApp,
  makeDefaultMailApp,
  openExternal,
  previewMeetingPopup,
  removeAccount,
  listSignatures,
  renameAccount,
  savePrefs,
  saveSignature,
  setAgentAccess,
  setDefaultSignature,
  testNotification,
  trustCertificate,
  type TrustedCerts,
} from "../api";
import { isDemoMode, useAccounts } from "../lib/accounts";
import { formatAgo, plural } from "../lib/format";
import { PRIORITY_LINE_CHOICES } from "../lib/priority";
import { CARDS, CARD_LIMIT, CARD_ORDER, type CardKey, type CardPrefs } from "../lib/layout";
import {
  DEFAULT_SETTINGS as HOME_DEFAULTS,
  TIMEOUT_MAX,
  TIMEOUT_MIN,
  addFocusTime,
  focusLabel,
  type FocusTime,
  type HomeSettings,
} from "../lib/homeScreen";
import { saveLoadRemoteImages, useLoadRemoteImages } from "../lib/remoteImages";
import type { ThemePref } from "../lib/theme";
import { signatureLabel } from "../lib/signatures";
import type {
  AccountView,
  AgentAccess,
  AgentsView,
  BadgeCount,
  DefaultSignature,
  GatekeeperMode,
  NotifyFor,
  Prefs,
  Signature,
  SignaturesView,
  SyncStatus,
  Untrusted,
} from "../types";
import AccountMark from "./AccountMark";
import About from "./About";
import { CertificateDialog } from "./Dialog";
import type { Updates } from "../lib/updates";
import Dropdown, { MenuItem } from "./Dropdown";
import SignatureFrame, { type SignatureFrameHandle } from "./SignatureFrame";
import TemplatesPage, { type TemplateDraft } from "./TemplatesPage";
import { tipAttrs } from "../lib/tooltip";

export type SettingsPage =
  | "accounts"
  | "composer"
  | "signatures"
  | "templates"
  | "ai"
  | "agents"
  | "home"
  | "gatekeeper"
  | "priority"
  | "notifications"
  | "appearance"
  | "about";

/** Nav groups, in order. Only pages that exist so far. */
const NAV: { key: SettingsPage; label: string; Icon: LucideIcon }[][] = [
  [
    { key: "accounts", label: "Accounts", Icon: UsersRound },
    { key: "composer", label: "Composer", Icon: SquarePen },
    { key: "signatures", label: "Signatures", Icon: SignatureIcon },
    { key: "templates", label: "Templates", Icon: LayoutTemplate },
    { key: "ai", label: "AI", Icon: Sparkles },
    { key: "agents", label: "Agents", Icon: Bot },
  ],
  [
    { key: "home", label: "Home Screen", Icon: House },
    { key: "gatekeeper", label: "Gatekeeper", Icon: UserCheck },
    { key: "priority", label: "Priority", Icon: Zap },
    { key: "notifications", label: "Notifications", Icon: Bell },
    { key: "appearance", label: "Appearance", Icon: Palette },
  ],
  [{ key: "about", label: "About", Icon: Info }],
];
const PAGES = NAV.flat();

/**
 * The Settings modal: 900px wide, viewport height − 80px, a 280px
 * nav that shrinks to an icon rail on narrow windows. Clicking outside
 * doesn't close it; Esc does. Changes apply as they're made.
 */
export default function Settings({
  page,
  onPage,
  onClose,
  theme,
  onTheme,
  minimize,
  onMinimize,
  cards,
  onCards,
  priorityLines,
  onPriorityLines,
  prioritySenders,
  onPrioritize,
  onAccountsChanged,
  gatekeeper,
  onGatekeeper,
  home,
  onHome,
  updates,
  newTemplate,
  onNewTemplateTaken,
}: {
  page: SettingsPage;
  onPage: (page: SettingsPage) => void;
  onClose: () => void;
  theme: ThemePref;
  onTheme: (theme: ThemePref) => void;
  /** Minimize old emails in a conversation. */
  minimize: boolean;
  onMinimize: (on: boolean) => void;
  /** Unread cards: which are on, and how many emails each shows. */
  cards: CardPrefs;
  onCards: (cards: CardPrefs) => void;
  /** Rows the Priority card shows; 0 shows them all. */
  priorityLines: number;
  onPriorityLines: (lines: number) => void;
  prioritySenders: string[];
  /** Adds senders to the Priority senders, or takes them off. */
  onPrioritize: (addresses: string[], priority: boolean) => void;
  /** After an add, rename or remove, so the list updates without waiting for the event. */
  onAccountsChanged: () => void;
  gatekeeper: GatekeeperMode;
  onGatekeeper: (mode: GatekeeperMode) => void;
  home: HomeSettings;
  onHome: (settings: HomeSettings) => void;
  updates: Updates;
  /** A template to start writing on the Templates page (made from a draft). */
  newTemplate?: TemplateDraft | null;
  onNewTemplateTaken?: () => void;
}) {
  const tabRefs = useRef<Partial<Record<SettingsPage, HTMLButtonElement | null>>>({});
  const title = PAGES.find((p) => p.key === page)!.label;

  useEffect(() => {
    tabRefs.current[page]?.focus();
    function onKey(e: globalThis.KeyboardEvent) {
      if (e.key !== "Escape" || e.defaultPrevented) return;
      e.preventDefault();
      onClose();
    }
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
    // Only on open: focus the tab and listen for Esc.
  }, []);

  function onNavKey(e: KeyboardEvent) {
    if (e.key !== "ArrowDown" && e.key !== "ArrowUp") return;
    e.preventDefault();
    const i = PAGES.findIndex((p) => p.key === page);
    const next = PAGES[(i + (e.key === "ArrowDown" ? 1 : -1) + PAGES.length) % PAGES.length].key;
    onPage(next);
    tabRefs.current[next]?.focus();
  }

  return (
    <div className="fixed inset-0 z-[104] grid place-items-center bg-black/30">
      <div
        role="dialog"
        aria-modal
        aria-label="Settings"
        className="grid h-[calc(100vh-80px)] w-[900px] max-w-[calc(100vw-32px)] grid-cols-[74px_1fr] overflow-hidden rounded-2xl bg-base-100 shadow-popup md:grid-cols-[280px_1fr]"
      >
        <nav className="overflow-y-auto border-r border-hairline bg-base-200 px-3 pt-8" aria-label="Settings sections">
          <div className="mb-4 hidden px-3 text-lg font-semibold md:block">Settings</div>
          <div role="tablist" aria-orientation="vertical" onKeyDown={onNavKey} className="space-y-4">
            {NAV.map((group, i) => (
              <ul key={i} className="space-y-0.5">
                {group.map(({ key, label, Icon }) => (
                  <li key={key}>
                    <button
                      ref={(el) => {
                        tabRefs.current[key] = el;
                      }}
                      role="tab"
                      aria-selected={page === key}
                      tabIndex={page === key ? 0 : -1}
                      data-tip={label}
                      onClick={() => onPage(key)}
                      className={`flex h-9 w-full items-center justify-center gap-3 rounded-lg px-3 text-[13px] md:justify-start ${
                        page === key ? "bg-row-selected font-medium" : "hover:bg-base-content/5"
                      }`}
                    >
                      <Icon size={17} className={page === key ? "text-primary" : "text-text-2"} />
                      <span className="hidden md:inline">{label}</span>
                    </button>
                  </li>
                ))}
              </ul>
            ))}
          </div>
        </nav>

        <section role="tabpanel" aria-label={title} className="flex min-h-0 flex-col">
          <header className="flex h-16 shrink-0 items-center px-8">
            <h2 className="text-lg font-semibold">{title}</h2>
            <button
              className="btn btn-circle btn-ghost btn-sm ml-auto text-text-2"
              {...tipAttrs({ label: "Close", keys: "Esc" })}
              aria-label="Close settings"
              onClick={onClose}
            >
              <X size={18} />
            </button>
          </header>
          <div className="min-h-0 flex-1 overflow-y-auto px-8 pb-10">
            <div className="max-w-[560px]">
              {page === "accounts" && <AccountsPage onChanged={onAccountsChanged} />}
              {page === "composer" && <ComposerPage />}
              {page === "signatures" && <SignaturesPage />}
              {page === "templates" && <TemplatesPage start={newTemplate} onStarted={onNewTemplateTaken} />}
              {page === "ai" && <AiPage />}
              {page === "agents" && <AgentsPage onAddAccount={() => onPage("accounts")} />}
              {page === "about" && <About updates={updates} />}
              {page === "home" && <HomeScreenPage settings={home} onChange={onHome} />}
              {page === "gatekeeper" && <GatekeeperPage mode={gatekeeper} onMode={onGatekeeper} />}
              {page === "priority" && (
                <PriorityPage
                  lines={priorityLines}
                  onLines={onPriorityLines}
                  senders={prioritySenders}
                  onPrioritize={onPrioritize}
                />
              )}
              {page === "notifications" && <NotificationsPage />}
              {page === "appearance" && (
                <AppearancePage
                  theme={theme}
                  onTheme={onTheme}
                  minimize={minimize}
                  onMinimize={onMinimize}
                  cards={cards}
                  onCards={onCards}
                />
              )}
            </div>
          </div>
        </section>
      </div>
    </div>
  );
}

// ---- shared rows ----

export function Group({ title, children, note }: { title?: string; children: ReactNode; note?: ReactNode }) {
  return (
    <section className="mb-7">
      {title && <h3 className="mb-2 px-1 text-[13px] font-semibold text-text-2">{title}</h3>}
      <div className="divide-y divide-hairline rounded-2xl border border-hairline">{children}</div>
      {note && <p className="mt-2 px-1 text-xs leading-[18px] text-text-2">{note}</p>}
    </section>
  );
}

function Row({
  label,
  description,
  children,
  htmlFor,
}: {
  label: string;
  description?: ReactNode;
  children?: ReactNode;
  htmlFor?: string;
}) {
  return (
    <div className="flex min-h-[52px] flex-wrap items-center gap-x-4 gap-y-2 px-4 py-2.5">
      <div className="min-w-0 flex-1">
        <label htmlFor={htmlFor} className="block text-[13px] font-medium leading-5">
          {label}
        </label>
        {description && <div className="text-xs leading-[18px] text-text-2">{description}</div>}
      </div>
      {children}
    </div>
  );
}

function Switch({
  id,
  on,
  disabled = false,
  onToggle,
}: {
  id: string;
  on: boolean;
  disabled?: boolean;
  onToggle: () => void;
}) {
  return (
    <button
      id={id}
      role="switch"
      aria-checked={on}
      disabled={disabled}
      onClick={onToggle}
      className={`relative h-5 w-9 shrink-0 rounded-full transition-colors disabled:opacity-40 ${
        on ? "bg-primary" : "bg-base-content/20"
      }`}
    >
      <span
        className={`absolute top-0.5 size-4 rounded-full bg-white shadow-sm transition-[left] ${
          on ? "left-[18px]" : "left-0.5"
        }`}
      />
    </button>
  );
}

export const INPUT = "input input-sm h-8 w-64 rounded-lg border-hairline bg-base-100 text-[13px] focus:outline-primary";

// ---- Accounts ----

type Provider = "gmail" | "microsoft" | "imap";

const PROVIDERS: { key: Provider; label: string; detail: string; Icon: LucideIcon }[] = [
  { key: "gmail", label: "Gmail", detail: "With an app password", Icon: Mail },
  { key: "microsoft", label: "Microsoft 365", detail: "Work or school", Icon: Building2 },
  { key: "imap", label: "Other (IMAP)", detail: "Any mail server", Icon: Server },
];

function AccountsPage({ onChanged }: { onChanged: () => void }) {
  const accounts = useAccounts();
  const demo = isDemoMode(accounts);
  /** "choose" shows the provider cards; demo mode shows them until a form opens. */
  const [adding, setAdding] = useState<Provider | "choose" | null>(null);
  const now = useNow(30_000);
  const choosing = adding === "choose" || (adding === null && demo);

  function added() {
    setAdding(null);
    onChanged();
  }
  const back = () => setAdding(demo ? null : "choose");

  return (
    <>
      <Group note={demo ? "These are demo mailboxes. They go away once you add an account of your own." : undefined}>
        {accounts.map((a) => (
          <AccountRow key={a.id} account={a} now={now} onChanged={onChanged} />
        ))}
      </Group>

      {adding === null && !demo && (
        <button className="btn btn-sm h-8 rounded-lg" onClick={() => setAdding("choose")}>
          <Plus size={14} /> Add account
        </button>
      )}
      {choosing && (
        <section className="mb-7">
          <div className="mb-2 flex items-center px-1">
            <h3 className="text-[13px] font-semibold text-text-2">Add an account</h3>
            {!demo && (
              <button className="ml-auto text-xs text-text-2 hover:text-base-content" onClick={() => setAdding(null)}>
                Cancel
              </button>
            )}
          </div>
          <div className="grid grid-cols-3 gap-3">
            {PROVIDERS.map(({ key, label, detail, Icon }) => (
              <button
                key={key}
                onClick={() => setAdding(key)}
                className="flex flex-col items-start rounded-2xl border border-hairline px-3.5 py-3 text-left hover:border-splitter hover:bg-row-hover"
              >
                <Icon size={18} className="mb-2 text-primary" />
                <span className="block text-[13px] font-semibold leading-5">{label}</span>
                <span className="block text-xs leading-4 text-text-2">{detail}</span>
              </button>
            ))}
          </div>
        </section>
      )}
      {adding === "gmail" && <GmailForm onAdded={added} onCancel={back} />}
      {adding === "microsoft" && <MicrosoftForm onAdded={added} onCancel={back} />}
      {adding === "imap" && <ImapForm onAdded={added} onCancel={back} />}
    </>
  );
}

/** One account: color, label, address and sync state, with Rename and Remove for real accounts. */
function AccountRow({ account: a, now, onChanged }: { account: AccountView; now: number; onChanged: () => void }) {
  const [mode, setMode] = useState<"view" | "rename" | "remove" | "trust">("view");
  const [label, setLabel] = useState(a.label);
  const [status, setStatus] = useState<Status>({ kind: "idle" });
  /** Esc in the label field: the blur that follows must not save. */
  const discard = useRef(false);
  const busy = status.kind === "busy";

  async function saveLabel() {
    const next = label.trim();
    if (discard.current || !next || next === a.label) {
      discard.current = false;
      setLabel(a.label);
      setMode("view");
      return;
    }
    setStatus({ kind: "busy", what: "Saving…" });
    try {
      await renameAccount(a.id, next);
      setStatus({ kind: "idle" });
      setMode("view");
      onChanged();
    } catch (error) {
      setStatus({ kind: "error", text: String(error) });
    }
  }

  async function remove() {
    setStatus({ kind: "busy", what: "Removing…" });
    try {
      await removeAccount(a.id);
      onChanged();
    } catch (error) {
      setStatus({ kind: "error", text: String(error) });
    }
  }

  return (
    <div className="px-4 py-3">
      <div className="flex items-center gap-3">
        <AccountMark color={a.color} size="lg" />
        <div className="min-w-0 flex-1">
          {mode === "rename" ? (
            <input
              autoFocus
              aria-label="Label"
              maxLength={40}
              spellCheck={false}
              value={label}
              disabled={busy}
              onChange={(e) => setLabel(e.target.value)}
              onBlur={() => void saveLabel()}
              onKeyDown={(e) => {
                if (e.key === "Enter") e.currentTarget.blur();
                if (e.key === "Escape") {
                  e.preventDefault(); // keeps Settings open
                  discard.current = true;
                  e.currentTarget.blur();
                }
              }}
              className={`${INPUT} h-7 w-48`}
            />
          ) : (
            <div className="truncate text-[13px] font-medium leading-5">{a.label}</div>
          )}
          <div className="truncate text-xs leading-[18px] text-text-2">
            {a.email} · {a.provider}
          </div>
          {!a.demo && <SyncLine sync={a.sync} now={now} />}
          {!a.demo && a.sync.certificate && (
            <button className="text-xs font-medium leading-[18px] text-primary hover:underline" onClick={() => setMode("trust")}>
              Review the certificate…
            </button>
          )}
        </div>
        {a.demo ? (
          <span className="rounded-full bg-base-300 px-2 text-[11px] font-medium leading-[18px] text-text-2">Demo</span>
        ) : (
          <Dropdown
            align="end"
            width="w-40"
            trigger={({ toggle, open }) => (
              <button
                className={`btn btn-ghost btn-sm btn-square text-text-2 hover:text-base-content ${open ? "bg-base-content/10" : ""}`}
                data-tip="Account actions"
                aria-label={`${a.label} actions`}
                aria-haspopup="menu"
                aria-expanded={open}
                onClick={toggle}
              >
                <Ellipsis size={16} />
              </button>
            )}
          >
            <MenuItem
              label="Rename"
              icon={<Pencil size={14} />}
              onSelect={() => {
                setLabel(a.label);
                setStatus({ kind: "idle" });
                setMode("rename");
              }}
            />
            <MenuItem
              label="Remove…"
              icon={<Trash2 size={14} />}
              danger
              onSelect={() => {
                setStatus({ kind: "idle" });
                setMode("remove");
              }}
            />
          </Dropdown>
        )}
      </div>
      {mode === "remove" && (
        <div className="mt-3 rounded-xl bg-error/5 px-3.5 py-3">
          <p className="text-[13px] leading-5">
            Remove {a.email}? Its mail is removed from this computer. Nothing is deleted on the server.
          </p>
          <div className="mt-2.5 flex gap-1.5">
            <button className="btn btn-error btn-sm h-8 rounded-lg" disabled={busy} onClick={() => void remove()}>
              {busy && <LoaderCircle size={14} className="animate-spin" />}
              Remove
            </button>
            <button className="btn btn-ghost btn-sm h-8 rounded-lg" disabled={busy} onClick={() => setMode("view")}>
              Cancel
            </button>
          </div>
        </div>
      )}
      {status.kind === "error" && (
        <div className="mt-2 flex">
          <StatusLine status={status} />
        </div>
      )}
      {mode === "trust" && a.sync.certificate && (
        <CertificateDialog
          certificate={a.sync.certificate}
          onTrust={async () => {
            await trustCertificate(a.id, a.sync.certificate!);
            onChanged();
          }}
          onClose={() => setMode("view")}
        />
      )}
    </div>
  );
}

/**
 * Adding an account whose server shows a certificate the system doesn't
 * trust: `failed` asks the user about it (`dialog`), and if they trust it,
 * `retry` tries again with it trusted. Other failures are returned to show.
 */
function useCertificateTrust() {
  const [trusted, setTrusted] = useState<TrustedCerts>({});
  const [asking, setAsking] = useState<{ certificate: Untrusted; retry: (trusted: TrustedCerts) => void } | null>(
    null,
  );

  function failed(error: unknown, retry: (trusted: TrustedCerts) => void): string | null {
    const failure = failureOf(error);
    if (!failure.certificate) return failure.message;
    setAsking({ certificate: failure.certificate, retry });
    return null;
  }

  const dialog = asking && (
    <CertificateDialog
      certificate={asking.certificate}
      onTrust={() => {
        const { service, fingerprint } = asking.certificate;
        const next = { ...trusted, [service === "imap" ? "trustedImap" : "trustedSmtp"]: fingerprint };
        setTrusted(next);
        asking.retry(next);
      }}
      onClose={() => setAsking(null)}
    />
  );
  return { trusted, failed, dialog };
}

function SyncLine({ sync, now }: { sync: SyncStatus; now: number }) {
  if (sync.status === "syncing") {
    return (
      <div className="flex items-center gap-1 text-xs leading-[18px] text-text-2">
        <LoaderCircle size={11} className="animate-spin" /> Syncing…
      </div>
    );
  }
  if (sync.status === "error") {
    return <div className="select-text text-xs leading-[18px] text-error">{sync.message ?? "Couldn't sync."}</div>;
  }
  return (
    <div className="text-xs leading-[18px] text-text-2">
      {sync.lastSync ? `Synced ${formatAgo(sync.lastSync, now)}` : "Not synced yet"}
    </div>
  );
}

/** The current time, refreshed every `ms`, for "Synced 2 min ago". */
function useNow(ms: number) {
  const [now, setNow] = useState(Date.now);
  useEffect(() => {
    const timer = window.setInterval(() => setNow(Date.now()), ms);
    return () => window.clearInterval(timer);
  }, [ms]);
  return now;
}

interface FormProps {
  onAdded: () => void;
  onCancel: () => void;
}

/** Submit (with a spinner while it works), Cancel, and the error if adding failed. */
function FormActions({
  label,
  busyLabel,
  busy,
  disabled,
  error,
  onCancel,
}: {
  label: string;
  busyLabel: string;
  busy: boolean;
  disabled: boolean;
  error: string | null;
  onCancel: () => void;
}) {
  return (
    <div className="mb-8 flex flex-wrap items-center gap-3">
      <button type="submit" className="btn btn-primary btn-sm h-8 rounded-lg px-4" disabled={disabled || busy}>
        {busy && <LoaderCircle size={14} className="animate-spin" />}
        {busy ? busyLabel : label}
      </button>
      <button type="button" className="btn btn-ghost btn-sm h-8 rounded-lg" disabled={busy} onClick={onCancel}>
        Cancel
      </button>
      {error && <StatusLine status={{ kind: "error", text: error }} />}
    </div>
  );
}

function Intro({ children }: { children: ReactNode }) {
  return <p className="mb-4 px-1 text-[13px] leading-5 text-text-2">{children}</p>;
}

const APP_PASSWORDS_URL = "https://myaccount.google.com/apppasswords";

function GmailForm({ onAdded, onCancel }: FormProps) {
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const [label, setLabel] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const trust = useCertificateTrust();

  async function add(trusted = trust.trusted) {
    setBusy(true);
    setError(null);
    try {
      // Google shows app passwords in groups of four; the spaces aren't part of it.
      await addGmailAccount(email.trim(), password.replace(/\s+/g, ""), label.trim() || "Personal", trusted);
      onAdded();
    } catch (err) {
      setError(trust.failed(err, (next) => void add(next)));
      setBusy(false);
    }
  }

  function submit(e: FormEvent) {
    e.preventDefault();
    void add();
  }

  return (
    <form onSubmit={submit}>
      {trust.dialog}
      <Intro>
        Google requires an app password for this. Turn on 2-Step Verification, then create one at{" "}
        <button type="button" className="text-primary hover:underline" onClick={() => void openExternal(APP_PASSWORDS_URL)}>
          myaccount.google.com/apppasswords
        </button>
        .
      </Intro>
      <fieldset disabled={busy}>
        <Group title="Gmail">
          <Row label="Email" htmlFor="gmail-email">
            <input
              id="gmail-email"
              type="email"
              autoFocus
              autoComplete="off"
              spellCheck={false}
              placeholder="you@gmail.com"
              value={email}
              onChange={(e) => setEmail(e.target.value)}
              className={INPUT}
            />
          </Row>
          <Row label="App password" htmlFor="gmail-password" description="16 letters, from Google.">
            <input
              id="gmail-password"
              type="password"
              autoComplete="off"
              spellCheck={false}
              placeholder="abcd efgh ijkl mnop"
              value={password}
              onChange={(e) => setPassword(e.target.value)}
              className={INPUT}
            />
          </Row>
          <LabelRow id="gmail-label" placeholder="Personal" value={label} onChange={setLabel} />
        </Group>
      </fieldset>
      <FormActions
        label="Add account"
        busyLabel="Checking…"
        busy={busy}
        disabled={!email.trim() || !password.trim()}
        error={error}
        onCancel={onCancel}
      />
    </form>
  );
}

/** The last app registration used; not secrets, so localStorage is fine. */
const MICROSOFT_KEY = "spark-rs.microsoft-app";

function savedMicrosoftApp(): { clientId: string; tenant: string } {
  try {
    const saved = JSON.parse(localStorage.getItem(MICROSOFT_KEY) ?? "{}");
    return { clientId: String(saved.clientId ?? ""), tenant: String(saved.tenant ?? "") };
  } catch {
    return { clientId: "", tenant: "" };
  }
}

function MicrosoftForm({ onAdded, onCancel }: FormProps) {
  const [app, setApp] = useState(savedMicrosoftApp);
  const [label, setLabel] = useState("");
  const [waiting, setWaiting] = useState(false);
  const [error, setError] = useState<string | null>(null);
  /** Set while sign-in is pending in the browser; closing the form abandons it. */
  const pending = useRef(false);
  const cancelled = useRef(false);

  useEffect(
    () => () => {
      if (pending.current) void cancelSignIn();
    },
    [],
  );

  async function submit(e: FormEvent) {
    e.preventDefault();
    const clientId = app.clientId.trim();
    const tenant = app.tenant.trim();
    localStorage.setItem(MICROSOFT_KEY, JSON.stringify({ clientId, tenant }));
    setError(null);
    setWaiting(true);
    pending.current = true;
    cancelled.current = false;
    try {
      await addMicrosoftAccount(clientId, tenant, label.trim() || "Work");
      pending.current = false;
      onAdded();
    } catch (err) {
      pending.current = false;
      setWaiting(false);
      if (!cancelled.current) setError(String(err));
    }
  }

  function cancel() {
    cancelled.current = true;
    void cancelSignIn();
  }

  return (
    <form onSubmit={(e) => void submit(e)}>
      <Intro>
        Use the app registration from your organization's Microsoft Entra admin center (platform: Mobile and desktop,
        redirect URI http://localhost).
      </Intro>
      <fieldset disabled={waiting}>
        <Group title="Microsoft 365">
          <Row label="Application (client) ID" htmlFor="ms-client">
            <input
              id="ms-client"
              autoFocus
              autoComplete="off"
              spellCheck={false}
              placeholder="00000000-0000-0000-0000-000000000000"
              value={app.clientId}
              onChange={(e) => setApp({ ...app, clientId: e.target.value })}
              className={INPUT}
            />
          </Row>
          <Row label="Directory (tenant) ID" htmlFor="ms-tenant" description="Or your organization's domain.">
            <input
              id="ms-tenant"
              autoComplete="off"
              spellCheck={false}
              placeholder="contoso.onmicrosoft.com"
              value={app.tenant}
              onChange={(e) => setApp({ ...app, tenant: e.target.value })}
              className={INPUT}
            />
          </Row>
          <LabelRow id="ms-label" placeholder="Work" value={label} onChange={setLabel} />
        </Group>
      </fieldset>
      {waiting ? (
        <div role="status" className="mb-8 flex items-center gap-3 rounded-2xl border border-hairline px-4 py-3">
          <LoaderCircle size={16} className="shrink-0 animate-spin text-primary" />
          <span className="flex-1 text-[13px] leading-5">Finish signing in in your browser…</span>
          <button type="button" className="btn btn-sm h-8 rounded-lg" onClick={cancel}>
            Cancel
          </button>
        </div>
      ) : (
        <FormActions
          label="Sign in with Microsoft"
          busyLabel="Signing in…"
          busy={false}
          disabled={!app.clientId.trim() || !app.tenant.trim()}
          error={error}
          onCancel={onCancel}
        />
      )}
    </form>
  );
}

function ImapForm({ onAdded, onCancel }: FormProps) {
  const [form, setForm] = useState({
    email: "",
    password: "",
    label: "",
    imapHost: "",
    imapPort: "993",
    smtpHost: "",
    smtpPort: "465",
  });
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const set = (patch: Partial<typeof form>) => setForm((f) => ({ ...f, ...patch }));
  const domain = form.email.includes("@") ? form.email.split("@").pop()!.trim() : "";
  const imapHost = form.imapHost.trim() || (domain && `imap.${domain}`);
  const smtpHost = form.smtpHost.trim() || (domain && `smtp.${domain}`);
  const imapPort = port(form.imapPort);
  const smtpPort = port(form.smtpPort);
  const ready = form.email.trim() && form.password && imapHost && smtpHost && imapPort && smtpPort;

  const trust = useCertificateTrust();

  async function add(trusted = trust.trusted) {
    setBusy(true);
    setError(null);
    try {
      await addImapAccount(
        {
          email: form.email.trim(),
          password: form.password,
          label: form.label.trim() || domain || "Mail",
          imapHost,
          imapPort,
          smtpHost,
          smtpPort,
        },
        trusted,
      );
      onAdded();
    } catch (err) {
      setError(trust.failed(err, (next) => void add(next)));
      setBusy(false);
    }
  }

  function submit(e: FormEvent) {
    e.preventDefault();
    if (ready) void add();
  }

  const portInput = "input input-sm h-8 w-20 rounded-lg border-hairline bg-base-100 text-[13px] focus:outline-primary";
  const hostInput = "input input-sm h-8 w-[178px] rounded-lg border-hairline bg-base-100 text-[13px] focus:outline-primary";
  return (
    <form onSubmit={submit}>
      {trust.dialog}
      <fieldset disabled={busy}>
        <Group title="Account">
          <Row label="Email" htmlFor="imap-email">
            <input
              id="imap-email"
              type="email"
              autoFocus
              autoComplete="off"
              spellCheck={false}
              placeholder="you@example.com"
              value={form.email}
              onChange={(e) => set({ email: e.target.value })}
              className={INPUT}
            />
          </Row>
          <Row label="Password" htmlFor="imap-password">
            <input
              id="imap-password"
              type="password"
              autoComplete="off"
              value={form.password}
              onChange={(e) => set({ password: e.target.value })}
              className={INPUT}
            />
          </Row>
          <LabelRow id="imap-label" placeholder={domain || "Mail"} value={form.label} onChange={(label) => set({ label })} />
        </Group>
        <Group title="Servers" note="Port 993 or 465 uses TLS; 143 or 587 uses STARTTLS.">
          <Row label="IMAP (incoming)" htmlFor="imap-host">
            <div className="flex items-center gap-1.5">
              <input
                id="imap-host"
                autoComplete="off"
                spellCheck={false}
                placeholder={domain ? `imap.${domain}` : "imap.example.com"}
                value={form.imapHost}
                onChange={(e) => set({ imapHost: e.target.value })}
                className={hostInput}
              />
              <input
                aria-label="IMAP port"
                inputMode="numeric"
                value={form.imapPort}
                onChange={(e) => set({ imapPort: e.target.value })}
                className={portInput}
              />
            </div>
          </Row>
          <Row label="SMTP (outgoing)" htmlFor="smtp-host">
            <div className="flex items-center gap-1.5">
              <input
                id="smtp-host"
                autoComplete="off"
                spellCheck={false}
                placeholder={domain ? `smtp.${domain}` : "smtp.example.com"}
                value={form.smtpHost}
                onChange={(e) => set({ smtpHost: e.target.value })}
                className={hostInput}
              />
              <input
                aria-label="SMTP port"
                inputMode="numeric"
                value={form.smtpPort}
                onChange={(e) => set({ smtpPort: e.target.value })}
                className={portInput}
              />
            </div>
          </Row>
        </Group>
      </fieldset>
      <FormActions
        label="Add account"
        busyLabel="Checking…"
        busy={busy}
        disabled={!ready}
        error={error}
        onCancel={onCancel}
      />
    </form>
  );
}

/** A port number, or 0 if the text isn't one. */
function port(text: string) {
  const n = Number(text.trim());
  return Number.isInteger(n) && n > 0 && n < 65536 ? n : 0;
}

function LabelRow({
  id,
  placeholder,
  value,
  onChange,
}: {
  id: string;
  placeholder: string;
  value: string;
  onChange: (value: string) => void;
}) {
  return (
    <Row label="Label" htmlFor={id} description="Shown in the sidebar.">
      <input
        id={id}
        autoComplete="off"
        maxLength={40}
        placeholder={placeholder}
        value={value}
        onChange={(e) => onChange(e.target.value)}
        className={INPUT}
      />
    </Row>
  );
}

// ---- Agents ----

const ACCESS: { key: AgentAccess; label: string; detail: string; Icon: LucideIcon }[] = [
  { key: "none", label: "No access", detail: "Agents can't see this account.", Icon: CircleX },
  { key: "read", label: "Read", detail: "Agents can list, search and read mail.", Icon: Eye },
  {
    key: "triage",
    label: "Triage",
    detail: "Agents can also mark mail Done, read, pinned, set aside, snoozed or deleted.",
    Icon: Inbox,
  },
  {
    key: "send",
    label: "Send",
    detail: "Agents can also send email as you. Each one waits 30 seconds so you can undo it.",
    Icon: Send,
  },
];

function AgentsPage({ onAddAccount }: { onAddAccount: () => void }) {
  const [view, setView] = useState<AgentsView | null>(null);
  const [status, setStatus] = useState<Status>({ kind: "idle" });

  useEffect(() => {
    void getAgents().then(setView, (error) => setStatus({ kind: "error", text: String(error) }));
  }, []);

  async function change(account: number, access: AgentAccess) {
    try {
      setView(await setAgentAccess(account, access));
      setStatus({ kind: "idle" });
    } catch (error) {
      setStatus({ kind: "error", text: String(error) });
    }
  }

  if (!view) return <StatusLine status={status} />;
  const cli = view.cli && `"${view.cli}"`;

  return (
    <>
      <p className="mb-6 text-[13px] leading-5 text-text-2">
        AI agents on this computer, such as Claude Code, Codex and Hermes, can work with your email through
        spark-cli. They see nothing until you allow it here, and they start spark-rs in the background when they
        need it.
      </p>

      <Group
        title="What agents can do"
        note="Emails can contain text written to trick an agent. Allow sending only if you trust the agents you use."
      >
        {view.accounts.length === 0 ? (
          <Row label="No mail accounts yet" description="Demo mail can't be shared with agents.">
            <button className="btn btn-sm h-8 rounded-lg" onClick={onAddAccount}>
              Add account
            </button>
          </Row>
        ) : (
          view.accounts.map((a) => (
            <Row key={a.id} label={a.email} description={a.label}>
              <AccessPicker
                access={a.access}
                label={`What agents can do in ${a.email}`}
                onChange={(access) => void change(a.id, access)}
              />
            </Row>
          ))
        )}
      </Group>

      <Group
        title="Connect an agent"
        note="Run the command in a terminal, then start a new session in that agent."
      >
        {cli ? (
          <>
            <CommandRow label="Claude Code" command={`claude mcp add --scope user spark-rs -- ${cli} mcp`} />
            <CommandRow label="Codex" command={`codex mcp add spark-rs -- ${cli} mcp`} />
            <CommandRow label="Hermes" command={`hermes mcp add spark-rs --command ${cli} --args mcp`} />
            <CommandRow label="Other agents" description="Their MCP server command." command={`${cli} mcp`} />
          </>
        ) : (
          <Row
            label="spark-cli is missing"
            description="Agents need spark-cli next to spark-rs. Build it with tools\win-arm64\cargo.cmd build -p spark-cli."
          />
        )}
      </Group>

      <StatusLine status={status} />
    </>
  );
}

/** Spark's four-way access switch: no access, read, triage, send. */
function AccessPicker({
  access,
  label,
  onChange,
}: {
  access: AgentAccess;
  label: string;
  onChange: (access: AgentAccess) => void;
}) {
  return (
    <div role="radiogroup" aria-label={label} className="flex rounded-full border border-hairline bg-base-200 p-0.5">
      {ACCESS.map(({ key, label, detail, Icon }) => (
        <button
          key={key}
          role="radio"
          aria-checked={access === key}
          aria-label={label}
          {...tipAttrs({ label, detail })}
          onClick={() => onChange(key)}
          className={`grid h-7 w-10 place-items-center rounded-full ${
            access === key ? "bg-base-100 text-primary shadow-sm" : "text-text-2 hover:text-base-content"
          }`}
        >
          <Icon size={15} />
        </button>
      ))}
    </div>
  );
}

function CommandRow({ label, description, command }: { label: string; description?: string; command: string }) {
  const [copied, setCopied] = useState(false);

  function copy() {
    void navigator.clipboard.writeText(command).then(() => {
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1500);
    });
  }

  return (
    <div className="px-4 py-2.5">
      <div className="flex min-h-8 items-center gap-3">
        <div className="min-w-0 flex-1">
          <div className="text-[13px] font-medium leading-5">{label}</div>
          {description && <div className="text-xs leading-[18px] text-text-2">{description}</div>}
        </div>
        <button className="btn btn-ghost btn-sm h-8 gap-1.5 rounded-lg text-text-2" onClick={copy}>
          {copied ? <Check size={14} /> : <Copy size={14} />}
          {copied ? "Copied" : "Copy"}
        </button>
      </div>
      <code className="mt-1 block select-text break-all rounded-lg bg-base-200 px-2.5 py-1.5 font-mono text-xs text-text-2">
        {command}
      </code>
    </div>
  );
}

// ---- Gatekeeper ----

const GATEKEEPER_CHOICES: { key: GatekeeperMode; label: string; detail: string }[] = [
  {
    key: "beforeInbox",
    label: "Screen new senders before the Inbox",
    detail: "Their mail waits in New senders at the top of the Inbox, out of the list until you accept them.",
  },
  {
    key: "insideInbox",
    label: "Screen new senders inside the Inbox",
    detail: "Their mail goes to the Inbox, and asks you to accept or block the sender when you open it.",
  },
  {
    key: "off",
    label: "Don't screen new senders",
    detail: "Everyone goes to the Inbox, including whoever is waiting now. You can still block a sender from an email's ⋯ menu.",
  },
];

function GatekeeperPage({ mode, onMode }: { mode: GatekeeperMode; onMode: (mode: GatekeeperMode) => void }) {
  return (
    <section className="mb-7">
      <p className="mb-4 px-1 text-[13px] leading-5 text-text-2">
        The Gatekeeper keeps unwanted mail out of your Inbox: the first time someone writes, you accept or block them.
      </p>
      <div role="radiogroup" aria-label="Gatekeeper mode" className="grid gap-2">
        {GATEKEEPER_CHOICES.map((c) => (
          <button
            key={c.key}
            role="radio"
            aria-checked={mode === c.key}
            onClick={() => mode !== c.key && onMode(c.key)}
            className={`rounded-2xl border px-3.5 py-3 text-left ${
              mode === c.key ? "border-primary bg-primary/5 ring-1 ring-primary" : "border-hairline hover:border-splitter"
            }`}
          >
            <span className="block text-[13px] font-semibold leading-5">{c.label}</span>
            <span className="block text-xs leading-4 text-text-2">{c.detail}</span>
          </button>
        ))}
      </div>
      <p className="mt-2 px-1 text-xs leading-[18px] text-text-2">
        A decision counts for all your accounts, and you can accept or block a whole domain from an email. Colleagues
        at your work domain, people you've written to, and notifications from a few big companies (GitHub, Google,
        Microsoft…) get through without asking. A blocked sender's mail goes to the Blocked folder on the server.
      </p>
    </section>
  );
}

// ---- Priority ----

/** Priority emails in the Inbox, and the Priority senders, whose mail is all Priority. */
function PriorityPage({
  lines,
  onLines,
  senders,
  onPrioritize,
}: {
  lines: number;
  onLines: (lines: number) => void;
  senders: string[];
  onPrioritize: (addresses: string[], priority: boolean) => void;
}) {
  const [address, setAddress] = useState("");
  const accounts = useAccounts();
  const key = address.trim().toLowerCase();
  const own = accounts.some((a) => a.email.toLowerCase() === key);
  const listed = senders.some((s) => s.toLowerCase() === key);
  const valid = /^[^\s@]+@[^\s@]+$/.test(key) && !own && !listed;

  function add(e: FormEvent) {
    e.preventDefault();
    if (!valid) return;
    onPrioritize([address.trim()], true);
    setAddress("");
  }

  return (
    <>
      <Group title="Priority emails in Inbox">
        <Row
          label="Show"
          description="Priority emails stay on top of your Inbox until they're Done. Show all opens the rest."
          htmlFor="priority-lines"
        >
          <select
            id="priority-lines"
            value={lines}
            onChange={(e) => onLines(Number(e.target.value))}
            className="select select-sm h-8 w-40 rounded-lg border-hairline text-[13px]"
          >
            {PRIORITY_LINE_CHOICES.map((n) => (
              <option key={n} value={n}>
                {n === 0 ? "All emails" : plural(n, "Line")}
              </option>
            ))}
          </select>
        </Row>
      </Group>

      <Group
        title="Priority senders"
        note="Every email from a Priority sender is Priority, the ones you have and the ones still to come. You can also add a sender from an email's ⋯ menu or the Command Center."
      >
        {senders.map((sender) => (
          <Row key={sender} label={sender}>
            <button
              className="btn btn-ghost btn-sm h-8 rounded-lg"
              aria-label={`Remove ${sender} from Priority senders`}
              onClick={() => onPrioritize([sender], false)}
            >
              Remove
            </button>
          </Row>
        ))}
        <div className="px-4 py-2.5">
          <form onSubmit={add} className="flex items-center gap-2">
            <input
              type="email"
              value={address}
              onChange={(e) => setAddress(e.target.value)}
              placeholder="name@example.com"
              aria-label="Address to add to Priority senders"
              className="input input-sm h-8 min-w-0 flex-1 rounded-lg border-hairline text-[13px]"
            />
            <button type="submit" className="btn btn-sm h-8 rounded-lg" disabled={!valid}>
              <Plus size={15} /> Add
            </button>
          </form>
          {(own || listed) && (
            <p className="mt-1.5 text-xs text-text-2">
              {own ? "That's one of your own addresses." : "That's a Priority sender already."}
            </p>
          )}
        </div>
      </Group>
    </>
  );
}

// ---- Notifications ----

const BADGE_CHOICES: { key: BadgeCount; label: string }[] = [
  { key: "new", label: "New emails" },
  { key: "inbox", label: "Whole Inbox" },
  { key: "off", label: "Off" },
];

/** When the meeting pop-up shows, as Spark offers it. */
const POPUP_CHOICES: { key: string; label: string }[] = [
  { key: "off", label: "Off" },
  { key: "0", label: "When meeting starts" },
  ...[1, 2, 3, 5, 10, 15].map((m) => ({ key: String(m), label: `${m} minute${m === 1 ? "" : "s"} before` })),
];

const NOTIFY_CHOICES: { key: NotifyFor; label: string; detail: string }[] = [
  { key: "people", label: "Smart", detail: "Only mail from people" },
  { key: "all", label: "All", detail: "Every new email" },
  { key: "priority", label: "Priority", detail: "Only Priority mail" },
  { key: "off", label: "Off", detail: "No notifications" },
];

/** The app's preferences, and changing them: saved at once, and put back if saving fails. */
function usePrefs() {
  const [prefs, setPrefs] = useState<Prefs | null>(null);
  const [status, setStatus] = useState<Status>({ kind: "idle" });

  useEffect(() => {
    void getPrefs().then(setPrefs, (error) => setStatus({ kind: "error", text: String(error) }));
  }, []);

  async function change(patch: Partial<Prefs>) {
    if (!prefs) return;
    const before = prefs;
    setPrefs({ ...before, ...patch });
    try {
      setPrefs(await savePrefs({ ...before, ...patch }));
      setStatus({ kind: "idle" });
    } catch (error) {
      setPrefs(before);
      setStatus({ kind: "error", text: String(error) });
    }
  }

  return { prefs, status, setStatus, change };
}

// ---- Home Screen ----

/** Seconds after midnight as an `<input type="time">` value. */
const timeValue = (at: number) =>
  `${String(Math.floor(at / 3600)).padStart(2, "0")}:${String(Math.floor((at % 3600) / 60)).padStart(2, "0")}`;

function HomeScreenPage({ settings, onChange }: { settings: HomeSettings; onChange: (settings: HomeSettings) => void }) {
  const set = (patch: Partial<HomeSettings>) => onChange({ ...settings, ...patch });
  const setTime = (index: number, patch: Partial<FocusTime>) =>
    set({ focusTimes: settings.focusTimes.map((f, i) => (i === index ? { ...f, ...patch } : f)) });
  const minutes = Math.round(settings.timeout / 60);
  /** The minutes field as typed; it saves once it holds 1 to 99. */
  const [typed, setTyped] = useState(String(minutes));
  useEffect(() => setTyped(String(minutes)), [minutes]);
  const on = settings.enabled;

  function typeMinutes(text: string) {
    setTyped(text);
    const n = Number(text);
    if (Number.isInteger(n) && n >= TIMEOUT_MIN && n <= TIMEOUT_MAX && n !== minutes) set({ timeout: n * 60 });
  }

  return (
    <>
      <Group>
        <Row
          label="Enable Home Screen"
          description="Between visits to your Inbox, a calm screen greets you with what came while you were away."
          htmlFor="home-enabled"
        >
          <Switch id="home-enabled" on={on} onToggle={() => set({ enabled: !on })} />
        </Row>
        <div className="flex min-h-[52px] items-center gap-4 px-4 py-2.5">
          <div className="min-w-0 flex-1">
            <div className={`flex flex-wrap items-baseline gap-x-2 text-[13px] font-medium leading-8 ${on ? "" : "opacity-40"}`}>
              <label htmlFor="home-timeout-on">Show Home Screen after</label>
              <input
                type="number"
                min={TIMEOUT_MIN}
                max={TIMEOUT_MAX}
                value={typed}
                disabled={!on || !settings.timeoutEnabled}
                onChange={(e) => typeMinutes(e.target.value)}
                onBlur={() => setTyped(String(minutes))}
                aria-label="Minutes of inactivity"
                className="input input-sm h-8 w-16 rounded-lg border-hairline px-2 text-center text-[13px]"
              />
              <span>min inactivity</span>
            </div>
            <div className="text-xs leading-[18px] text-text-2">
              The Home Screen comes back when spark-rs has been in the background this long, except while you write or
              read an email.
            </div>
          </div>
          <Switch
            id="home-timeout-on"
            on={settings.timeoutEnabled}
            disabled={!on}
            onToggle={() => set({ timeoutEnabled: !settings.timeoutEnabled })}
          />
        </div>
      </Group>

      <Group
        title="Email Focus"
        note={
          on && settings.focusScheduleEnabled
            ? "Remind me sends a notification at that time, unless spark-rs is in front."
            : undefined
        }
      >
        <Row
          label="Set Email Focus Schedule"
          description="Choose the times you plan to check your email. In between, the Home Screen says when the next one is."
          htmlFor="home-schedule"
        >
          <Switch
            id="home-schedule"
            on={settings.focusScheduleEnabled}
            disabled={!on}
            onToggle={() => set({ focusScheduleEnabled: !settings.focusScheduleEnabled })}
          />
        </Row>
        {on &&
          settings.focusScheduleEnabled &&
          settings.focusTimes.map((f, i) => {
            const label = focusLabel(f.name);
            return (
              <div key={`${f.name}`} className="flex min-h-[52px] flex-wrap items-center gap-x-4 gap-y-2 px-4 py-2.5">
                <Switch id={`focus-${i}`} on={f.enabled} onToggle={() => setTime(i, { enabled: !f.enabled })} />
                <label htmlFor={`focus-${i}`} className="min-w-0 flex-1 truncate text-[13px] font-medium">
                  {label}
                </label>
                <input
                  type="time"
                  value={timeValue(f.at)}
                  disabled={!f.enabled}
                  onChange={(e) => {
                    const [h, m] = e.target.value.split(":").map(Number);
                    if (Number.isInteger(h) && Number.isInteger(m)) setTime(i, { at: h * 3600 + m * 60 });
                  }}
                  aria-label={`${label} time`}
                  className="input input-sm h-8 w-32 rounded-lg border-hairline text-[13px] disabled:opacity-40"
                />
                <label className={`flex items-center gap-1.5 text-[13px] ${f.enabled ? "" : "opacity-40"}`}>
                  <input
                    type="checkbox"
                    className="checkbox checkbox-xs rounded"
                    checked={f.remindMe}
                    disabled={!f.enabled}
                    onChange={() => setTime(i, { remindMe: !f.remindMe })}
                  />
                  Remind me
                </label>
                <button
                  className="btn btn-circle btn-ghost btn-sm text-text-2"
                  aria-label={`Delete ${label}`}
                  {...tipAttrs("Delete")}
                  onClick={() => set({ focusTimes: settings.focusTimes.filter((_, j) => j !== i) })}
                >
                  <Trash2 size={15} />
                </button>
              </div>
            );
          })}
      </Group>

      {on && settings.focusScheduleEnabled && (
        <div className="flex gap-2">
          <button className="btn btn-sm h-8 rounded-lg" onClick={() => set({ focusTimes: addFocusTime(settings.focusTimes) })}>
            <Plus size={14} /> Add Email Focus Time
          </button>
          <button
            className="btn btn-ghost btn-sm h-8 rounded-lg text-error"
            onClick={() => onChange({ ...HOME_DEFAULTS, filter: settings.filter })}
          >
            Reset Settings
          </button>
        </div>
      )}
    </>
  );
}

function NotificationsPage() {
  const { prefs, status, setStatus, change } = usePrefs();
  const accounts = useAccounts().filter((a) => !a.demo);

  async function test() {
    setStatus({ kind: "busy", what: "Sending…" });
    try {
      await testNotification();
      setStatus({ kind: "ok", text: "Sent. Windows shows it at the bottom right of the screen." });
    } catch (error) {
      setStatus({ kind: "error", text: String(error) });
    }
  }

  async function preview() {
    try {
      await previewMeetingPopup();
      setStatus({ kind: "idle" });
    } catch (error) {
      setStatus({ kind: "error", text: String(error) });
    }
  }

  if (!prefs) return <StatusLine status={status} />;
  const off = prefs.notify === "off";

  return (
    <>
      <section className="mb-7">
        <h3 className="mb-2 px-1 text-[13px] font-semibold text-text-2">Notify me about new mail</h3>
        <div role="radiogroup" aria-label="Notify me about new mail" className="grid grid-cols-4 gap-3">
          {NOTIFY_CHOICES.map((c) => (
            <button
              key={c.key}
              role="radio"
              aria-checked={prefs.notify === c.key}
              onClick={() => void change({ notify: c.key })}
              className={`rounded-2xl border px-3.5 py-3 text-left ${
                prefs.notify === c.key
                  ? "border-primary bg-primary/5 ring-1 ring-primary"
                  : "border-hairline hover:border-splitter"
              }`}
            >
              <span className="block text-[13px] font-semibold leading-5">{c.label}</span>
              <span className="block text-xs leading-4 text-text-2">{c.detail}</span>
            </button>
          ))}
        </div>
        <p className="mt-2 px-1 text-xs leading-[18px] text-text-2">
          Smart leaves out notifications and newsletters. Priority is mail from Priority senders and in Priority
          conversations. Mail waiting in New senders doesn't notify, and while the spark-rs window is in front, new
          mail just appears in the list.
        </p>
      </section>

      <Group>
        <Row label="Sound" description="Windows' New Mail sound, set in Windows' sound settings." htmlFor="notify-sound">
          <Switch id="notify-sound" on={prefs.sound} disabled={off} onToggle={() => void change({ sound: !prefs.sound })} />
        </Row>
        <Row
          label="Snoozed mail returns"
          description="A notification when snoozed mail comes back to the Inbox."
          htmlFor="snooze-alert"
        >
          <Switch id="snooze-alert" on={prefs.snoozeAlert} onToggle={() => void change({ snoozeAlert: !prefs.snoozeAlert })} />
        </Row>
        <Row label="Test notification" description="Check how new mail will look and sound.">
          <button className="btn btn-sm h-8 rounded-lg" disabled={status.kind === "busy"} onClick={() => void test()}>
            Send test
          </button>
        </Row>
      </Group>

      <Group title="App icon badge">
        <Row
          label="Badge count"
          description="The number on spark-rs's taskbar button: unread conversations in the Inbox, or all of them."
          htmlFor="badge-count"
        >
          <select
            id="badge-count"
            value={prefs.badge}
            onChange={(e) => void change({ badge: e.target.value as BadgeCount })}
            className="select select-sm h-8 w-40 rounded-lg border-hairline text-[13px]"
          >
            {BADGE_CHOICES.map((c) => (
              <option key={c.key} value={c.key}>
                {c.label}
              </option>
            ))}
          </select>
        </Row>
        {accounts.length > 1 &&
          accounts.map((a) => {
            const included = !prefs.badgeExcluded.includes(a.id);
            return (
              <Row key={a.id} label={a.label} description={a.email} htmlFor={`badge-${a.id}`}>
                <Switch
                  id={`badge-${a.id}`}
                  on={included}
                  disabled={prefs.badge === "off"}
                  onToggle={() =>
                    void change({
                      badgeExcluded: included
                        ? [...prefs.badgeExcluded, a.id]
                        : prefs.badgeExcluded.filter((id) => id !== a.id),
                    })
                  }
                />
              </Row>
            );
          })}
      </Group>

      <Group title="Calendar">
        <Row
          label="Event alerts"
          description="A notification at each event's alert time, as set in Outlook, with Snooze. Clicking it opens the event."
          htmlFor="event-alerts"
        >
          <Switch id="event-alerts" on={prefs.eventAlerts} onToggle={() => void change({ eventAlerts: !prefs.eventAlerts })} />
        </Row>
      </Group>

      <Group title="Meeting pop-up">
        <Row
          label="Show meeting pop-up"
          description="A card over your other windows before each meeting, with the time left and a button to join the call."
          htmlFor="meeting-popup"
        >
          <select
            id="meeting-popup"
            value={prefs.meetingPopup ? String(prefs.meetingPopupMinutes) : "off"}
            onChange={(e) =>
              void change(
                e.target.value === "off"
                  ? { meetingPopup: false }
                  : { meetingPopup: true, meetingPopupMinutes: Number(e.target.value) },
              )
            }
            className="select select-sm h-8 w-44 rounded-lg border-hairline text-[13px]"
          >
            {POPUP_CHOICES.map((c) => (
              <option key={c.key} value={c.key}>
                {c.label}
              </option>
            ))}
          </select>
        </Row>
        <Row
          label="Only meetings with others"
          description="Leaves out events with no one else and no call link, like focus time."
          htmlFor="meeting-popup-guests"
        >
          <Switch
            id="meeting-popup-guests"
            on={prefs.meetingPopupGuestsOnly}
            disabled={!prefs.meetingPopup}
            onToggle={() => void change({ meetingPopupGuestsOnly: !prefs.meetingPopupGuestsOnly })}
          />
        </Row>
        <Row label="Sound" description="A soft chime as each meeting pops up." htmlFor="meeting-popup-sound">
          <Switch
            id="meeting-popup-sound"
            on={prefs.meetingPopupSound}
            disabled={!prefs.meetingPopup}
            onToggle={() => void change({ meetingPopupSound: !prefs.meetingPopupSound })}
          />
        </Row>
        <Row label="Preview" description="Shows a sample meeting in the pop-up.">
          <button className="btn btn-sm h-8 rounded-lg" onClick={() => void preview()}>
            Show
          </button>
        </Row>
      </Group>

      <Group title="In the background">
        <Row
          label="Keep running when closed"
          description="Closing the window leaves spark-rs in the notification area, checking for mail. Quit from its icon there."
          htmlFor="keep-running"
        >
          <Switch
            id="keep-running"
            on={prefs.keepRunning}
            onToggle={() => void change({ keepRunning: !prefs.keepRunning })}
          />
        </Row>
        <Row
          label="Open at sign-in"
          description="Starts in the background when you sign in to Windows."
          htmlFor="open-at-login"
        >
          <Switch
            id="open-at-login"
            on={prefs.openAtLogin}
            onToggle={() => void change({ openAtLogin: !prefs.openAtLogin })}
          />
        </Row>
      </Group>

      <StatusLine status={status} />
    </>
  );
}

// ---- Composer ----

const UNDO_SEND_SECONDS = [5, 10, 15, 30, 60];

function ComposerPage() {
  const { prefs, status, change } = usePrefs();
  const accounts = useAccounts().filter((a) => !a.demo);
  const [mail, setMail] = useState<{ supported: boolean; default: boolean } | null>(null);
  const [mailStatus, setMailStatus] = useState<Status>({ kind: "idle" });
  useEffect(() => {
    const check = () => void mailApp().then(setMail, () => {});
    check();
    // Back from Windows' Default apps, it may be the default now.
    window.addEventListener("focus", check);
    return () => window.removeEventListener("focus", check);
  }, []);
  if (!prefs) return <StatusLine status={status} />;
  const setAuto = (field: "autoCc" | "autoBcc", account: number, value: string) => {
    const next = { ...prefs[field] };
    if (value.trim()) next[String(account)] = value.trim();
    else delete next[String(account)];
    if ((prefs[field][String(account)] ?? "") !== (next[String(account)] ?? "")) void change({ [field]: next });
  };
  return (
    <>
      <Group title="Sending">
        <Row
          label="Undo Send"
          description="How long you have to take a message back after pressing Send."
          htmlFor="undo-send"
        >
          <select
            id="undo-send"
            value={prefs.undoSendSeconds}
            onChange={(e) => void change({ undoSendSeconds: Number(e.target.value) })}
            className="select select-sm h-8 w-40 rounded-lg border-hairline text-[13px]"
          >
            {UNDO_SEND_SECONDS.map((n) => (
              <option key={n} value={n}>
                {n} seconds
              </option>
            ))}
          </select>
        </Row>
        <Row
          label="Mark as Done after sending replies"
          description="Replies start with Send and Done on: the email you answer leaves the Inbox when you send."
          htmlFor="send-and-done"
        >
          <Switch id="send-and-done" on={prefs.sendAndDone} onToggle={() => void change({ sendAndDone: !prefs.sendAndDone })} />
        </Row>
        <Row
          label="Attachments with Replies"
          description="Whether a reply takes the files of the email you answer."
          htmlFor="reply-attachments"
        >
          <select
            id="reply-attachments"
            value={prefs.replyAttachments}
            onChange={(e) => void change({ replyAttachments: e.target.value as Prefs["replyAttachments"] })}
            className="select select-sm h-8 w-40 rounded-lg border-hairline text-[13px]"
          >
            <option value="never">Never include</option>
            <option value="ask">Ask Every Time</option>
            <option value="always">Always include</option>
          </select>
        </Row>
      </Group>
      {mail?.supported && (
        <Group
          title="Default email app"
          note="Windows asks you to choose spark-rs for email in its Default apps settings."
        >
          <Row label="Open email links in spark-rs" description="Email links (mailto:) start a new email here.">
            {mail.default ? (
              <span className="flex items-center gap-1.5 text-[13px] text-success">
                <CircleCheck size={14} />
                Default
              </span>
            ) : (
              <button
                className="btn btn-sm"
                onClick={() =>
                  void makeDefaultMailApp().then(
                    () => setMailStatus({ kind: "idle" }),
                    (error) => setMailStatus({ kind: "error", text: String(error) }),
                  )
                }
              >
                Make Default
              </button>
            )}
          </Row>
          {mailStatus.kind !== "idle" && (
            <div className="px-4 py-2">
              <StatusLine status={mailStatus} />
            </div>
          )}
        </Group>
      )}
      {accounts.length > 0 && (
        <Group title="Automatic Cc and Bcc" note="Added to every new message, reply and forward from the account.">
          {accounts.map((a) => (
            <Row key={a.id} label={a.label} description={a.email}>
              <div className="flex flex-col gap-1.5">
                {(["autoCc", "autoBcc"] as const).map((field) => (
                  <input
                    key={`${field}-${prefs[field][String(a.id)] ?? ""}`}
                    aria-label={`${field === "autoCc" ? "Cc" : "Bcc"} for ${a.label}`}
                    placeholder={field === "autoCc" ? "Cc" : "Bcc"}
                    spellCheck={false}
                    defaultValue={prefs[field][String(a.id)] ?? ""}
                    onBlur={(e) => setAuto(field, a.id, e.target.value)}
                    onKeyDown={(e) => e.key === "Enter" && e.currentTarget.blur()}
                    className={`${INPUT} h-8 w-56`}
                  />
                ))}
              </div>
            </Row>
          ))}
        </Group>
      )}
      <Group title="Recipients">
        <Row
          label="Suggest recipients"
          description="Suggests people from your mail as you type in To, Cc and Bcc."
          htmlFor="suggest-recipients"
        >
          <Switch
            id="suggest-recipients"
            on={prefs.suggestRecipients}
            onToggle={() => void change({ suggestRecipients: !prefs.suggestRecipients })}
          />
        </Row>
      </Group>
      <StatusLine status={status} />
    </>
  );
}

// ---- Appearance ----

const THEMES: { key: ThemePref; label: string; Icon: LucideIcon }[] = [
  { key: "system", label: "System", Icon: Monitor },
  { key: "light", label: "Light", Icon: Sun },
  { key: "dark", label: "Dark", Icon: Moon },
];

function AppearancePage({
  theme,
  onTheme,
  minimize,
  onMinimize,
  cards,
  onCards,
}: {
  theme: ThemePref;
  onTheme: (theme: ThemePref) => void;
  minimize: boolean;
  onMinimize: (on: boolean) => void;
  cards: CardPrefs;
  onCards: (cards: CardPrefs) => void;
}) {
  // Priority's size is Settings → Priority's; People always shows.
  const sized = (key: CardKey) => key !== "priority";
  const { prefs, change } = usePrefs();
  const remoteImages = useLoadRemoteImages();
  return (
    <>
      <section className="mb-7">
        <h3 className="mb-2 px-1 text-[13px] font-semibold text-text-2">Theme</h3>
        <div role="radiogroup" aria-label="Theme" className="grid grid-cols-3 gap-3">
          {THEMES.map(({ key, label, Icon }) => (
            <button
              key={key}
              role="radio"
              aria-checked={theme === key}
              onClick={() => onTheme(key)}
              className={`rounded-2xl border p-2 text-left ${
                theme === key ? "border-primary ring-1 ring-primary" : "border-hairline hover:border-splitter"
              }`}
            >
              <ThemePreview kind={key} />
              <span className="mt-2 flex items-center gap-1.5 px-1 text-[13px] font-medium">
                <Icon size={14} className="text-text-2" />
                {label}
              </span>
            </button>
          ))}
        </div>
      </section>

      <Group title="Conversations">
        <Row
          label="Minimize old emails"
          description="Emails before the first new one in a conversation show as one line each. Click one to read it."
          htmlFor="minimize-old"
        >
          <Switch id="minimize-old" on={minimize} onToggle={() => onMinimize(!minimize)} />
        </Row>
        <Row
          label="Load remote images"
          description="Senders may use remote images to track if you've opened their email. Tracking pixels stay blocked either way."
          htmlFor="remote-images"
        >
          <Switch id="remote-images" on={remoteImages} onToggle={() => saveLoadRemoteImages(!remoteImages)} />
        </Row>
        {prefs && (
          <Row
            label="Auto-download attachments"
            description="An email's files download as you open it, so they open at once. Off, they download when you open or save one."
            htmlFor="auto-download"
          >
            <Switch
              id="auto-download"
              on={prefs.autoDownloadAttachments}
              onToggle={() => void change({ autoDownloadAttachments: !prefs.autoDownloadAttachments })}
            />
          </Row>
        )}
      </Group>

      <Group title="Unread cards" note="A card that's off gives its emails to People.">
        {CARD_ORDER.map((key) => {
          const on = !cards.hidden.includes(key);
          return (
            <Row key={key} label={CARDS[key].label} htmlFor={`card-${key}`}>
              <div className="flex items-center gap-2">
                {sized(key) && (
                  <select
                    aria-label={`Emails the ${CARDS[key].label} card shows`}
                    value={cards.rows[key] ?? CARD_LIMIT}
                    disabled={!on}
                    onChange={(e) => onCards({ ...cards, rows: { ...cards.rows, [key]: Number(e.target.value) } })}
                    className="select select-sm h-8 w-32 rounded-lg border-hairline text-[13px]"
                  >
                    {[3, 5, 10].map((n) => (
                      <option key={n} value={n}>
                        {n} emails
                      </option>
                    ))}
                  </select>
                )}
                {key !== "people" && (
                  <Switch
                    id={`card-${key}`}
                    on={on}
                    onToggle={() =>
                      onCards({
                        ...cards,
                        hidden: on ? [...cards.hidden, key] : cards.hidden.filter((k) => k !== key),
                      })
                    }
                  />
                )}
              </div>
            </Row>
          );
        })}
      </Group>
    </>
  );
}

/** A tiny sketch of the window: dark sidebar, list, reader. */
function ThemePreview({ kind }: { kind: ThemePref }) {
  const pane = (dark: boolean) => (
    <div className={`flex h-full ${dark ? "bg-[#1f2124]" : "bg-white"}`}>
      <div className="w-3 bg-[#282a2e]" />
      <div className={`w-2/5 space-y-1 border-r p-1.5 ${dark ? "border-white/10" : "border-black/5"}`}>
        {[0, 1, 2].map((i) => (
          <div key={i} className={`h-1.5 rounded-sm ${dark ? "bg-white/15" : "bg-black/10"}`} />
        ))}
      </div>
      <div className="flex-1 p-1.5">
        <div className={`h-6 rounded ${dark ? "bg-[#282a2e]" : "bg-[#f5f6f8]"}`} />
      </div>
    </div>
  );
  return (
    <div className="h-[72px] overflow-hidden rounded-lg border border-hairline">
      {kind === "system" ? (
        <div className="grid h-full grid-cols-2">
          {pane(false)}
          {pane(true)}
        </div>
      ) : (
        pane(kind === "dark")
      )}
    </div>
  );
}

// ---- AI ----

// ---- Signatures ----

/**
 * Spark's signature settings: what each address starts new mail with, and
 * every signature, which opens in an editor.
 */
function SignaturesPage() {
  const [view, setView] = useState<SignaturesView | null>(null);
  const [editing, setEditing] = useState<{ id: number | null; html: string } | null>(null);
  const [status, setStatus] = useState<Status>({ kind: "idle" });
  const accounts = useAccounts();

  function load() {
    listSignatures().then(setView, (error) => setStatus({ kind: "error", text: String(error) }));
  }
  useEffect(load, []);

  if (editing) {
    return (
      <SignatureEditor
        id={editing.id}
        html={editing.html}
        onDone={() => {
          setEditing(null);
          load();
        }}
      />
    );
  }
  if (!view) return <StatusLine status={status} />;

  async function changeDefault(mailbox: string, signature: DefaultSignature) {
    if (!view) return;
    const before = view;
    setView({ ...view, defaults: view.defaults.map((d) => (d.mailbox === mailbox ? { ...d, signature } : d)) });
    try {
      await setDefaultSignature(mailbox, signature);
      setStatus({ kind: "idle" });
    } catch (error) {
      setView(before);
      setStatus({ kind: "error", text: String(error) });
    }
  }

  return (
    <>
      <div className="mb-6 flex items-center gap-4">
        <p className="flex-1 text-xs leading-[18px] text-text-2">
          Your signature goes below what you write, above the email you're replying to. You can change, edit or hide it
          in each email.
        </p>
        <button className="btn btn-primary btn-sm gap-1.5" onClick={() => setEditing({ id: null, html: "" })}>
          <Plus size={14} />
          Create signature
        </button>
      </div>
      <Group title="Default signatures" note="Most recent is the signature last sent from that address.">
        {view.defaults.map((d) => (
          <Row
            key={d.mailbox}
            label={d.mailbox}
            description={accounts.find((a) => a.email.toLowerCase() === d.mailbox.toLowerCase())?.label}
          >
            <DefaultSignaturePicker
              mailbox={d.mailbox}
              value={d.signature}
              signatures={view.signatures}
              onChange={(next) => void changeDefault(d.mailbox, next)}
            />
          </Row>
        ))}
      </Group>
      {view.signatures.length > 0 && (
        <Group title="All signatures">
          {view.signatures.map((s, i) => {
            const more = s.text.split("\n").filter((line) => line.trim()).slice(1);
            return (
              <button
                key={s.id}
                aria-label={`Signature ${i + 1}: ${signatureLabel(s)}`}
                onClick={() => setEditing({ id: s.id, html: s.html })}
                className="flex min-h-[52px] w-full items-center gap-3 px-4 py-2.5 text-left first:rounded-t-2xl last:rounded-b-2xl hover:bg-base-content/5"
              >
                <span className="min-w-0 flex-1">
                  <span className="block truncate text-[13px] font-medium leading-5">{signatureLabel(s)}</span>
                  {more.length > 0 && (
                    <span className="block truncate text-xs leading-[18px] text-text-2">{more.join(" · ")}</span>
                  )}
                </span>
                <ChevronRight size={16} className="shrink-0 text-text-3" />
              </button>
            );
          })}
        </Group>
      )}
      <StatusLine status={status} />
    </>
  );
}

function defaultLabel(value: DefaultSignature, signatures: Signature[]) {
  if (value.type === "none") return "No signature";
  const chosen = value.type === "custom" ? signatures.find((s) => s.id === value.id) : undefined;
  return chosen ? signatureLabel(chosen) : "Most recent";
}

function DefaultSignaturePicker({
  mailbox,
  value,
  signatures,
  onChange,
}: {
  mailbox: string;
  value: DefaultSignature;
  signatures: Signature[];
  onChange: (value: DefaultSignature) => void;
}) {
  return (
    <Dropdown
      align="end"
      width="w-60"
      trigger={({ toggle, open }) => (
        <button
          className={`flex h-8 max-w-48 items-center gap-1.5 rounded-lg border border-hairline px-2.5 text-[13px] hover:bg-base-content/5 ${
            open ? "bg-base-content/5" : ""
          }`}
          aria-label={`Default signature for ${mailbox}`}
          aria-haspopup="menu"
          aria-expanded={open}
          onClick={toggle}
        >
          <span className="truncate">{defaultLabel(value, signatures)}</span>
          <ChevronsUpDown size={12} className="shrink-0 text-text-2" />
        </button>
      )}
    >
      <MenuItem label="Most recent" checked={value.type === "mostRecent"} onSelect={() => onChange({ type: "mostRecent" })} />
      <MenuItem label="No signature" checked={value.type === "none"} onSelect={() => onChange({ type: "none" })} />
      {signatures.length > 0 && <li role="separator" className="my-0.5 h-px bg-hairline" />}
      {signatures.map((s) => (
        <MenuItem
          key={s.id}
          label={signatureLabel(s)}
          checked={value.type === "custom" && value.id === s.id}
          onSelect={() => onChange({ type: "custom", id: s.id })}
        />
      ))}
    </Dropdown>
  );
}

/** What a typed link means: a web address, or an email address to write to. */
function linkUrl(typed: string) {
  const url = typed.trim();
  if (!url) return null;
  if (/^(https?:|mailto:|tel:)/i.test(url)) return url;
  if (/^[^\s@/]+@[^\s@/]+\.[^\s@/]+$/.test(url)) return `mailto:${url}`;
  return `https://${url}`;
}

/** Creating or editing a signature: rich text, or its HTML. */
function SignatureEditor({ id, html: initial, onDone }: { id: number | null; html: string; onDone: () => void }) {
  const [html, setHtml] = useState(initial);
  const [source, setSource] = useState(false);
  const [link, setLink] = useState<string | null>(null);
  const [status, setStatus] = useState<Status>({ kind: "idle" });
  const frame = useRef<SignatureFrameHandle>(null);
  const fileRef = useRef<HTMLInputElement>(null);
  const busy = status.kind === "busy";

  async function save() {
    setStatus({ kind: "busy", what: "Saving…" });
    try {
      await saveSignature(id, html);
      onDone();
    } catch (error) {
      setStatus({ kind: "error", text: String(error) });
    }
  }

  async function remove() {
    if (id === null) return onDone();
    try {
      await deleteSignature(id);
      onDone();
    } catch (error) {
      setStatus({ kind: "error", text: String(error) });
    }
  }

  async function addImage(file: File) {
    try {
      const { resized } = await frame.current!.insertImage(file);
      setStatus(resized ? { kind: "ok", text: "The image was made smaller to fit in a signature." } : { kind: "idle" });
    } catch (error) {
      setStatus({ kind: "error", text: error instanceof Error ? error.message : String(error) });
    }
  }

  function addLink(e: FormEvent) {
    e.preventDefault();
    const url = linkUrl(link ?? "");
    if (url) frame.current?.link(url);
    setLink(null);
  }

  const tool = (label: string, keys: string | undefined, onClick: () => void, icon: ReactNode) => (
    <button
      className="btn btn-ghost btn-xs btn-square h-7 w-7 text-text-2 hover:text-base-content"
      {...tipAttrs(keys ? { label, keys } : label)}
      aria-label={label}
      // Keeps the selection in the editor.
      onMouseDown={(e) => e.preventDefault()}
      onClick={onClick}
    >
      {icon}
    </button>
  );

  return (
    <>
      <button className="btn btn-ghost btn-sm -ml-2 mb-2 gap-1 text-text-2" onClick={onDone}>
        <ArrowLeft size={14} />
        Signatures
      </button>
      <h3 className="mb-3 px-1 text-[15px] font-semibold">{id === null ? "Create signature" : "Edit signature"}</h3>
      <div className="overflow-hidden rounded-2xl border border-hairline">
        <div role="toolbar" aria-label="Formatting" className="flex h-10 items-center gap-0.5 border-b border-hairline px-2">
          {!source && (
            <>
              {tool("Bold", "Ctrl+B", () => frame.current?.exec("bold"), <Bold size={15} />)}
              {tool("Italic", "Ctrl+I", () => frame.current?.exec("italic"), <Italic size={15} />)}
              {tool("Underline", "Ctrl+U", () => frame.current?.exec("underline"), <Underline size={15} />)}
              {tool("Add link", undefined, () => setLink(""), <Link size={15} />)}
              {tool("Add image", undefined, () => fileRef.current?.click(), <ImagePlus size={15} />)}
              {tool("Reset fonts", undefined, () => frame.current?.resetFonts(), <RemoveFormatting size={15} />)}
            </>
          )}
          <button
            className={`btn btn-ghost btn-xs ml-auto h-7 gap-1 px-2 font-medium ${source ? "bg-primary/10 text-primary" : "text-text-2"}`}
            aria-pressed={source}
            data-tip="Edit the HTML"
            onClick={() => {
              setLink(null);
              setSource((on) => !on);
            }}
          >
            <Code size={14} />
            HTML
          </button>
        </div>
        {link !== null && !source && (
          <form onSubmit={addLink} className="flex items-center gap-2 border-b border-hairline px-3 py-2">
            <input
              autoFocus
              aria-label="Link address"
              className={`${INPUT} flex-1`}
              placeholder="https://example.com or name@example.com"
              value={link}
              onChange={(e) => setLink(e.target.value)}
              onKeyDown={(e) => {
                if (e.key !== "Escape") return;
                e.preventDefault();
                e.stopPropagation();
                setLink(null);
              }}
            />
            <button type="submit" className="btn btn-primary btn-sm" disabled={!link.trim()}>
              Add link
            </button>
            <button type="button" className="btn btn-ghost btn-sm" onClick={() => setLink(null)}>
              Cancel
            </button>
          </form>
        )}
        <div className="p-3">
          {source ? (
            <textarea
              aria-label="Signature HTML"
              spellCheck={false}
              className="block h-56 w-full resize-y rounded-lg bg-base-200 p-2 font-mono text-xs leading-5 outline-none"
              value={html}
              onChange={(e) => setHtml(e.target.value)}
            />
          ) : (
            <SignatureFrame
              ref={frame}
              html={html}
              onInput={setHtml}
              placeholder="Enter a text for signature…"
              minHeight={140}
              onError={(text) => setStatus({ kind: "error", text })}
            />
          )}
        </div>
      </div>
      <input
        ref={fileRef}
        type="file"
        accept="image/png,image/jpeg,image/gif,image/webp"
        hidden
        onChange={(e) => {
          const file = e.target.files?.[0];
          e.target.value = "";
          if (file) void addImage(file);
        }}
      />
      <div className="mt-4 flex flex-wrap items-center gap-2">
        <StatusLine status={status} />
        <div className="ml-auto flex gap-2">
          {id !== null && (
            <button className="btn btn-ghost btn-sm gap-1.5 text-error" disabled={busy} onClick={() => void remove()}>
              <Trash2 size={14} />
              Delete
            </button>
          )}
          <button className="btn btn-ghost btn-sm" onClick={onDone}>
            Cancel
          </button>
          <button className="btn btn-primary btn-sm px-5" disabled={busy || !html.trim()} onClick={() => void save()}>
            Save
          </button>
        </div>
      </div>
    </>
  );
}

export type Status =
  | { kind: "idle" }
  | { kind: "busy"; what: string }
  | { kind: "ok"; text: string }
  | { kind: "error"; text: string };

function AiPage() {
  const [saved, setSaved] = useState<AiSettings | null | undefined>(undefined);
  const [preset, setPreset] = useState<Preset>("anthropic");
  const [form, setForm] = useState<ProviderConfig>(presetDefaults("anthropic"));
  const [keyDraft, setKeyDraft] = useState("");
  const [replacingKey, setReplacingKey] = useState(false);
  const [models, setModels] = useState<string[]>([]);
  const [status, setStatus] = useState<Status>({ kind: "idle" });
  /** Each provider's last form this session, so switching back restores it. */
  const remembered = useRef<Partial<Record<Preset, ProviderConfig>>>({});

  useEffect(() => {
    void getAiSettings().then(
      (view) => {
        setSaved(view);
        if (view) {
          setPreset(view.preset);
          setForm(view.config);
        }
      },
      (error) => {
        setSaved(null);
        setStatus({ kind: "error", text: String(error) });
      },
    );
  }, []);

  const hasKey = saved?.preset === preset && saved.hasKey;
  const keyOptional = preset === "custom";
  const showKeyInput = !hasKey || replacingKey;

  async function save(next: { preset?: Preset; config?: ProviderConfig; apiKey?: string }) {
    const p = next.preset ?? preset;
    const config = next.config ?? form;
    try {
      const view = await saveAiSettings(p, config, next.apiKey);
      setSaved(view);
      setForm(view.config);
      remembered.current[p] = view.config;
      setStatus((s) => (s.kind === "error" ? { kind: "idle" } : s));
      return view;
    } catch (error) {
      setStatus({ kind: "error", text: String(error) });
      return null;
    }
  }

  function choosePreset(p: Preset) {
    if (p === preset) return;
    remembered.current[preset] = form;
    const config = remembered.current[p] ?? presetDefaults(p);
    setPreset(p);
    setForm(config);
    setModels([]);
    setKeyDraft("");
    setReplacingKey(false);
    setStatus({ kind: "idle" });
    void save({ preset: p, config });
  }

  /** Fields save when they lose focus, if they changed. */
  function commit(patch: Partial<ProviderConfig>) {
    const config = { ...form, ...patch };
    if (saved && saved.preset === preset && JSON.stringify(saved.config) === JSON.stringify(config)) return;
    void save({ config });
  }

  async function saveKey(key: string) {
    const view = await save({ apiKey: key });
    if (view) {
      setKeyDraft("");
      setReplacingKey(false);
    }
  }

  async function test() {
    // Save first, so a just-edited field can't race its own blur save.
    const typedKey = keyDraft.trim() ? keyDraft : undefined;
    if (!(await save({ apiKey: typedKey }))) return;
    if (typedKey) {
      setKeyDraft("");
      setReplacingKey(false);
    }
    setStatus({ kind: "busy", what: "Connecting…" });
    try {
      const ids = await testAiConnection();
      setModels(ids);
      setStatus({
        kind: "ok",
        text: `Connected to ${host(form.baseUrl)}. ${ids.length} ${ids.length === 1 ? "model" : "models"} available.`,
      });
    } catch (error) {
      setStatus({ kind: "error", text: String(error) });
    }
  }

  async function loadModels() {
    setStatus({ kind: "busy", what: "Loading models…" });
    try {
      const ids = await listAiModels();
      setModels(ids);
      setStatus({ kind: "ok", text: `${ids.length} ${ids.length === 1 ? "model" : "models"} available.` });
    } catch (error) {
      setStatus({ kind: "error", text: String(error) });
    }
  }

  if (saved === undefined) {
    return <LoaderCircle size={18} className="animate-spin text-text-2" />;
  }

  const suggestions = [...new Set([...models, ...KNOWN_MODELS[preset]])];

  return (
    <>
      <p className="mb-6 text-[13px] leading-5 text-text-2">
        AI features such as +AI Summary run on your own provider and API key. Nothing goes through spark-rs's
        servers, because there aren't any.
      </p>

      <section className="mb-7">
        <h3 className="mb-2 px-1 text-[13px] font-semibold text-text-2">Provider</h3>
        <div role="radiogroup" aria-label="Provider" className="grid grid-cols-3 gap-3">
          {PRESETS.map((p) => (
            <button
              key={p.key}
              role="radio"
              aria-checked={preset === p.key}
              onClick={() => choosePreset(p.key)}
              className={`rounded-2xl border px-3.5 py-3 text-left ${
                preset === p.key ? "border-primary bg-primary/5 ring-1 ring-primary" : "border-hairline hover:border-splitter"
              }`}
            >
              <span className="block text-[13px] font-semibold leading-5">{p.label}</span>
              <span className="block text-xs leading-4 text-text-2">{p.detail}</span>
            </button>
          ))}
        </div>
      </section>

      <Group
        title="Connection"
        note={
          preset === "custom"
            ? "Works with any server that speaks the OpenAI Chat Completions API: OpenRouter, Ollama (http://localhost:11434/v1), LM Studio (http://localhost:1234/v1), vLLM."
            : preset === "anthropic"
              ? "On Anthropic's API, a request the model declines is retried once on Anthropic's recommended fallback model."
              : undefined
        }
      >
        <Row
          label="API key"
          htmlFor="ai-key"
          description={
            hasKey && !replacingKey
              ? `Saved in ${KEY_STORE}.`
              : `${keyOptional ? "Optional for local servers. " : ""}Stored in ${KEY_STORE}, never in a settings file.`
          }
        >
          {showKeyInput ? (
            <form
              className="flex items-center gap-1.5"
              onSubmit={(e) => {
                e.preventDefault();
                if (keyDraft.trim()) void saveKey(keyDraft);
              }}
            >
              <input
                id="ai-key"
                type="password"
                autoComplete="off"
                spellCheck={false}
                placeholder={preset === "anthropic" ? "sk-ant-…" : preset === "openai" ? "sk-…" : "Optional"}
                value={keyDraft}
                onChange={(e) => setKeyDraft(e.target.value)}
                className={`${INPUT} w-48`}
              />
              <button type="submit" className="btn btn-sm h-8 rounded-lg" disabled={!keyDraft.trim()}>
                Save
              </button>
              {replacingKey && (
                <button
                  type="button"
                  className="btn btn-ghost btn-sm h-8 rounded-lg"
                  onClick={() => {
                    setReplacingKey(false);
                    setKeyDraft("");
                  }}
                >
                  Cancel
                </button>
              )}
            </form>
          ) : (
            <div className="flex items-center gap-1.5">
              <span className="flex h-8 items-center gap-1.5 rounded-lg bg-base-200 px-3 text-[13px] text-text-2">
                <KeyRound size={14} /> ••••••••••••
              </span>
              <button className="btn btn-sm h-8 rounded-lg" onClick={() => setReplacingKey(true)}>
                Replace
              </button>
              <button
                className="btn btn-ghost btn-sm h-8 rounded-lg text-error"
                onClick={() => void saveKey("")}
              >
                Remove
              </button>
            </div>
          )}
        </Row>
        <Row
          label="Base URL"
          htmlFor="ai-url"
          description={preset === "custom" ? "Usually ends in /v1." : "Change only for a proxy or gateway."}
        >
          <input
            id="ai-url"
            spellCheck={false}
            placeholder={preset === "custom" ? "http://localhost:11434/v1" : presetDefaults(preset).baseUrl}
            value={form.baseUrl}
            onChange={(e) => setForm({ ...form, baseUrl: e.target.value })}
            onBlur={() => commit({})}
            onKeyDown={(e) => e.key === "Enter" && e.currentTarget.blur()}
            className={INPUT}
          />
        </Row>
        <Row label="Model" htmlFor="ai-model" description={preset === "anthropic" ? "Claude Opus 5.5 by default." : undefined}>
          <div className="flex items-center gap-1.5">
            <input
              id="ai-model"
              list="ai-models"
              spellCheck={false}
              placeholder={preset === "custom" ? "e.g. llama3.3" : "e.g. gpt-5.5"}
              value={form.model}
              onChange={(e) => setForm({ ...form, model: e.target.value })}
              onBlur={() => commit({})}
              onKeyDown={(e) => e.key === "Enter" && e.currentTarget.blur()}
              className={`${INPUT} w-[220px]`}
            />
            <datalist id="ai-models">
              {suggestions.map((id) => (
                <option key={id} value={id} />
              ))}
            </datalist>
            <button
              className="btn btn-ghost btn-sm btn-square h-8 rounded-lg text-text-2"
              data-tip="Load the provider's models"
              aria-label="Load the provider's models"
              disabled={status.kind === "busy"}
              onClick={() => void loadModels()}
            >
              <RefreshCw size={15} />
            </button>
          </div>
        </Row>
        {preset === "anthropic" && (
          <Row label="Effort" htmlFor="ai-effort" description="How much the model thinks before answering. Higher is slower and costs more.">
            <select
              id="ai-effort"
              value={form.effort ?? "medium"}
              onChange={(e) => {
                const effort = e.target.value as Effort;
                setForm({ ...form, effort });
                commit({ effort });
              }}
              className="select select-sm h-8 w-40 rounded-lg border-hairline text-[13px]"
            >
              {EFFORTS.map((e) => (
                <option key={e.key} value={e.key}>
                  {e.label}
                </option>
              ))}
            </select>
          </Row>
        )}
      </Group>

      <div className="mb-8 flex flex-wrap items-center gap-3">
        <button className="btn btn-primary btn-sm h-8 rounded-lg px-4" disabled={status.kind === "busy"} onClick={() => void test()}>
          {status.kind === "busy" && <LoaderCircle size={14} className="animate-spin" />}
          Test connection
        </button>
        <StatusLine status={status} />
      </div>

      <Group title="Privacy">
        <p className="px-4 py-3 text-xs leading-[18px] text-text-2">
          When you use an AI feature, what it works on is sent to {presetLabel(preset)}
          {form.baseUrl ? ` (${host(form.baseUrl)})` : ""} and handled under that provider's terms: an email's sender,
          date, subject and text, and in the composer your request, your draft and your name for the sign-off. Nothing
          is sent in the background. Check your employer's policy before using AI on work mail.
        </p>
      </Group>
    </>
  );
}

export function StatusLine({ status }: { status: Status }) {
  if (status.kind === "idle") return null;
  if (status.kind === "busy") return <span className="text-xs text-text-2">{status.what}</span>;
  const ok = status.kind === "ok";
  const Icon = ok ? CircleCheck : CircleAlert;
  return (
    <span role="status" className={`flex min-w-0 flex-1 items-start gap-1.5 text-xs leading-[18px] ${ok ? "text-success" : "text-error"}`}>
      <Icon size={14} className="mt-0.5 shrink-0" />
      <span className="select-text">{status.text}</span>
    </span>
  );
}

function host(url: string) {
  try {
    return new URL(url).host;
  } catch {
    return url;
  }
}
