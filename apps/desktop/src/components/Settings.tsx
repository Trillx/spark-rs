import {
  Bell,
  Bot,
  Building2,
  Check,
  CircleAlert,
  CircleCheck,
  CircleX,
  Copy,
  Ellipsis,
  Eye,
  Inbox,
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
  Sun,
  Trash2,
  UsersRound,
  X,
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
  getAgents,
  getPrefs,
  openExternal,
  previewMeetingPopup,
  removeAccount,
  renameAccount,
  savePrefs,
  setAgentAccess,
  testNotification,
} from "../api";
import { isDemoMode, useAccounts } from "../lib/accounts";
import { formatAgo } from "../lib/format";
import type { ThemePref } from "../lib/theme";
import type { AccountView, AgentAccess, AgentsView, NotifyFor, Prefs, SyncStatus } from "../types";
import AccountMark from "./AccountMark";
import Dropdown, { MenuItem } from "./Dropdown";
import { tipAttrs } from "../lib/tooltip";

export type SettingsPage = "accounts" | "ai" | "agents" | "notifications" | "appearance";

/** Nav groups, in order. Only pages that exist so far. */
const NAV: { key: SettingsPage; label: string; Icon: LucideIcon }[][] = [
  [
    { key: "accounts", label: "Accounts", Icon: UsersRound },
    { key: "ai", label: "AI", Icon: Sparkles },
    { key: "agents", label: "Agents", Icon: Bot },
  ],
  [
    { key: "notifications", label: "Notifications", Icon: Bell },
    { key: "appearance", label: "Appearance", Icon: Palette },
  ],
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
  onAccountsChanged,
}: {
  page: SettingsPage;
  onPage: (page: SettingsPage) => void;
  onClose: () => void;
  theme: ThemePref;
  onTheme: (theme: ThemePref) => void;
  /** Minimize old emails in a conversation. */
  minimize: boolean;
  onMinimize: (on: boolean) => void;
  /** After an add, rename or remove, so the list updates without waiting for the event. */
  onAccountsChanged: () => void;
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
              {page === "ai" && <AiPage />}
              {page === "agents" && <AgentsPage onAddAccount={() => onPage("accounts")} />}
              {page === "notifications" && <NotificationsPage />}
              {page === "appearance" && (
                <AppearancePage theme={theme} onTheme={onTheme} minimize={minimize} onMinimize={onMinimize} />
              )}
            </div>
          </div>
        </section>
      </div>
    </div>
  );
}

// ---- shared rows ----

function Group({ title, children, note }: { title?: string; children: ReactNode; note?: ReactNode }) {
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

const INPUT = "input input-sm h-8 w-64 rounded-lg border-hairline bg-base-100 text-[13px] focus:outline-primary";

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
  const [mode, setMode] = useState<"view" | "rename" | "remove">("view");
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
    </div>
  );
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

  async function submit(e: FormEvent) {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      // Google shows app passwords in groups of four; the spaces aren't part of it.
      await addGmailAccount(email.trim(), password.replace(/\s+/g, ""), label.trim() || "Personal");
      onAdded();
    } catch (err) {
      setError(String(err));
      setBusy(false);
    }
  }

  return (
    <form onSubmit={(e) => void submit(e)}>
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

  async function submit(e: FormEvent) {
    e.preventDefault();
    if (!ready) return;
    setBusy(true);
    setError(null);
    try {
      await addImapAccount({
        email: form.email.trim(),
        password: form.password,
        label: form.label.trim() || domain || "Mail",
        imapHost,
        imapPort,
        smtpHost,
        smtpPort,
      });
      onAdded();
    } catch (err) {
      setError(String(err));
      setBusy(false);
    }
  }

  const portInput = "input input-sm h-8 w-20 rounded-lg border-hairline bg-base-100 text-[13px] focus:outline-primary";
  const hostInput = "input input-sm h-8 w-[178px] rounded-lg border-hairline bg-base-100 text-[13px] focus:outline-primary";
  return (
    <form onSubmit={(e) => void submit(e)}>
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

// ---- Notifications ----

const NOTIFY_CHOICES: { key: NotifyFor; label: string; detail: string }[] = [
  { key: "people", label: "Smart", detail: "Only mail from people" },
  { key: "all", label: "All", detail: "Every new email" },
  { key: "off", label: "Off", detail: "No notifications" },
];

function NotificationsPage() {
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
        <div role="radiogroup" aria-label="Notify me about new mail" className="grid grid-cols-3 gap-3">
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
          Smart leaves out notifications and newsletters. Mail from new senders waits in the Gatekeeper without a
          notification, and while the spark-rs window is in front, new mail just appears in the list.
        </p>
      </section>

      <Group>
        <Row label="Sound" description="Windows' New Mail sound, set in Windows' sound settings." htmlFor="notify-sound">
          <Switch id="notify-sound" on={prefs.sound} disabled={off} onToggle={() => void change({ sound: !prefs.sound })} />
        </Row>
        <Row label="Test notification" description="Check how new mail will look and sound.">
          <button className="btn btn-sm h-8 rounded-lg" disabled={status.kind === "busy"} onClick={() => void test()}>
            Send test
          </button>
        </Row>
      </Group>

      <Group title="Meetings">
        <Row
          label="Meeting pop-up"
          description="A card over your other windows a minute before each meeting, with the time left and a button to join the call."
          htmlFor="meeting-popup"
        >
          <Switch
            id="meeting-popup"
            on={prefs.meetingPopup}
            onToggle={() => void change({ meetingPopup: !prefs.meetingPopup })}
          />
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
}: {
  theme: ThemePref;
  onTheme: (theme: ThemePref) => void;
  minimize: boolean;
  onMinimize: (on: boolean) => void;
}) {
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

type Status =
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
          When you use an AI feature on an email, its sender, date, subject and text are sent to{" "}
          {presetLabel(preset)}
          {form.baseUrl ? ` (${host(form.baseUrl)})` : ""} and handled under that provider's terms. Nothing is sent in
          the background. Check your employer's policy before using AI on work mail.
        </p>
      </Group>
    </>
  );
}

function StatusLine({ status }: { status: Status }) {
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
