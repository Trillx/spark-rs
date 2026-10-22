import {
  CircleAlert,
  CircleCheck,
  KeyRound,
  LoaderCircle,
  Monitor,
  Moon,
  Palette,
  RefreshCw,
  Sparkles,
  Sun,
  UsersRound,
  X,
  type LucideIcon,
} from "lucide-react";
import { useEffect, useRef, useState, type KeyboardEvent, type ReactNode } from "react";
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
import { ACCOUNTS, ACCOUNT_IDS } from "../lib/format";
import type { ThemePref } from "../lib/theme";

export type SettingsPage = "accounts" | "ai" | "appearance";

/** Nav groups, in order. Only pages that exist so far. */
const NAV: { key: SettingsPage; label: string; Icon: LucideIcon }[][] = [
  [
    { key: "accounts", label: "Accounts", Icon: UsersRound },
    { key: "ai", label: "AI", Icon: Sparkles },
  ],
  [{ key: "appearance", label: "Appearance", Icon: Palette }],
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
}: {
  page: SettingsPage;
  onPage: (page: SettingsPage) => void;
  onClose: () => void;
  theme: ThemePref;
  onTheme: (theme: ThemePref) => void;
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
                      title={label}
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
              title="Close (Esc)"
              aria-label="Close settings"
              onClick={onClose}
            >
              <X size={18} />
            </button>
          </header>
          <div className="min-h-0 flex-1 overflow-y-auto px-8 pb-10">
            <div className="max-w-[560px]">
              {page === "accounts" && <AccountsPage />}
              {page === "ai" && <AiPage />}
              {page === "appearance" && <AppearancePage theme={theme} onTheme={onTheme} />}
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

const INPUT = "input input-sm h-8 w-64 rounded-lg border-hairline bg-base-100 text-[13px] focus:outline-primary";

// ---- Accounts ----

function AccountsPage() {
  return (
    <>
      <Group
        note="These are demo mailboxes. Signing in to Microsoft 365 (through Microsoft Graph) and Gmail (IMAP) comes in the next phase."
      >
        {ACCOUNT_IDS.map((id) => {
          const a = ACCOUNTS[id];
          return (
            <div key={id} className="flex items-center gap-3 px-4 py-3">
              <span className={`grid size-9 place-items-center rounded-full border-2 ${a.ring}`}>
                <span className={`size-3 rounded-full ${a.dot}`} />
              </span>
              <div className="min-w-0 flex-1">
                <div className="text-[13px] font-medium leading-5">{a.name}</div>
                <div className="truncate text-xs text-text-2">
                  {a.email} · {a.provider}
                </div>
              </div>
              <span className="rounded-full bg-base-300 px-2 text-[11px] font-medium leading-[18px] text-text-2">
                Demo
              </span>
            </div>
          );
        })}
      </Group>
      <button className="btn btn-sm rounded-lg" disabled>
        Add account
      </button>
    </>
  );
}

// ---- Appearance ----

const THEMES: { key: ThemePref; label: string; Icon: LucideIcon }[] = [
  { key: "system", label: "System", Icon: Monitor },
  { key: "light", label: "Light", Icon: Sun },
  { key: "dark", label: "Dark", Icon: Moon },
];

function AppearancePage({ theme, onTheme }: { theme: ThemePref; onTheme: (theme: ThemePref) => void }) {
  return (
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
              title="Load the provider's models"
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
