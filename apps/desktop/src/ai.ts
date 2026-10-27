import { Channel, invoke, isTauri } from "@tauri-apps/api/core";
import { announceSettingsChanged } from "./api";
import type { AccountId, Message } from "./types";

// Mirrors src-tauri/src/ai/*.rs and crates/ai. The API key goes in once and
// never comes back: the Rust side only reports whether one is stored.

export type Preset = "anthropic" | "openai" | "custom";
export type ProviderKind = "anthropic" | "open-ai-compatible";
export type Effort = "low" | "medium" | "high" | "xhigh" | "max";

export interface ProviderConfig {
  kind: ProviderKind;
  baseUrl: string;
  model: string;
  /** Anthropic only. */
  effort?: Effort;
}

export interface AiSettings {
  preset: Preset;
  config: ProviderConfig;
  /** Whether a key is stored for this preset. */
  hasKey: boolean;
}

/** Spark's "Summarize with" choices (SummaryStyle in summary.rs). */
export type SummaryStyle = "short" | "detailed" | "actionPoints";

export const SUMMARY_STYLES: { key: SummaryStyle; label: string; title: string }[] = [
  { key: "short", label: "Short", title: "Short summary" },
  { key: "detailed", label: "Detailed", title: "Detailed summary" },
  { key: "actionPoints", label: "Action Points", title: "Action Points Summary" },
];

/** A streamed answer (stream.rs): any number of deltas, then exactly one of the others. */
export type AiEvent =
  | { event: "delta"; data: { text: string } }
  | { event: "done"; data: { model: string; inputTokens: number; outputTokens: number; truncated: boolean } }
  | { event: "refused"; data: { category: string | null } }
  | { event: "error"; data: { message: string } }
  | { event: "notConfigured"; data: { message: string } };

/** The quick-reply chips (QuickReply in compose.rs). */
export type QuickReply = "interested" | "notInterested" | "thanks";

/** The +AI menu's edits (Rewrite in compose.rs). */
export type Rewrite =
  | "rephrase"
  | "shorten"
  | "expand"
  | "proofread"
  | "paragraphs"
  | "friendly"
  | "formal"
  | "neutral";

/** What the composer asks AI for (ComposeAction in compose.rs). */
export type ComposeAction =
  | { type: "write"; instructions: string }
  | { type: "quickReply"; reply: QuickReply }
  | { type: "rewrite"; text: string; how: Rewrite }
  | { type: "refine"; text: string; instructions: string };

/** The email a draft is about, and how (Reference in compose.rs). */
export interface Reference {
  id: number;
  relation: "reply" | "followUp" | "forward";
}

/** `text` goes in instead when no AI provider is set up. */
export const QUICK_REPLIES: { key: QuickReply; label: string; text: string }[] = [
  { key: "interested", label: "Interested", text: "Thanks, I'm interested. " },
  { key: "notInterested", label: "Not interested", text: "Thanks for reaching out, but I'm not interested." },
  { key: "thanks", label: "Thanks", text: "Thanks!" },
];

export const ANTHROPIC_BASE_URL = "https://api.anthropic.com";
export const OPENAI_BASE_URL = "https://api.openai.com/v1";

export const PRESETS: { key: Preset; label: string; detail: string }[] = [
  { key: "anthropic", label: "Anthropic", detail: "Claude, through the Messages API" },
  { key: "openai", label: "OpenAI", detail: "GPT models, through Chat Completions" },
  { key: "custom", label: "Custom", detail: "Any OpenAI-compatible server" },
];

export const EFFORTS: { key: Effort; label: string }[] = [
  { key: "low", label: "Low" },
  { key: "medium", label: "Medium" },
  { key: "high", label: "High" },
  { key: "xhigh", label: "Extra high" },
  { key: "max", label: "Max" },
];

/** Suggestions before the provider's own list is loaded. */
export const KNOWN_MODELS: Record<Preset, string[]> = {
  anthropic: ["claude-opus-5-5", "claude-sonnet-5-5", "claude-haiku-5-5", "claude-fable-5-1"],
  openai: [],
  custom: [],
};

/** Same as Preset::defaults in settings.rs. */
export function presetDefaults(preset: Preset): ProviderConfig {
  switch (preset) {
    case "anthropic":
      return { kind: "anthropic", baseUrl: ANTHROPIC_BASE_URL, model: "claude-opus-5-5", effort: "medium" };
    case "openai":
      return { kind: "open-ai-compatible", baseUrl: OPENAI_BASE_URL, model: "" };
    case "custom":
      return { kind: "open-ai-compatible", baseUrl: "", model: "" };
  }
}

export function presetLabel(preset: Preset) {
  return PRESETS.find((p) => p.key === preset)!.label;
}

/** Where keys are kept, in words for the settings page. */
export const KEY_STORE = /Windows/.test(navigator.userAgent) ? "Windows Credential Manager" : "the system keyring";

/** `null` until AI settings are saved for the first time. */
export function getAiSettings(): Promise<AiSettings | null> {
  if (isTauri()) return invoke("get_ai_settings");
  return Promise.resolve(browser.settings && browserView(browser.settings));
}

/** `apiKey`: omit to keep the stored key, `""` to delete it. */
export function saveAiSettings(preset: Preset, config: ProviderConfig, apiKey?: string): Promise<AiSettings> {
  if (isTauri())
    return invoke<AiSettings>("save_ai_settings", { preset, config, apiKey: apiKey ?? null }).then((saved) => {
      announceSettingsChanged();
      return saved;
    });
  return browserSave(preset, config, apiKey);
}

/** Lists the provider's models with the saved settings, which proves the key and URL work. */
export function testAiConnection(): Promise<string[]> {
  if (isTauri()) return invoke("test_ai_connection");
  return Promise.reject(BROWSER_ONLY);
}

export function listAiModels(): Promise<string[]> {
  if (isTauri()) return invoke("list_ai_models");
  return Promise.reject(BROWSER_ONLY);
}

/** Starts a summary; events arrive on `onEvent`. Resolves once it has started. */
export function summarizeMessage(
  message: Message,
  style: SummaryStyle,
  onEvent: (event: AiEvent) => void,
): Promise<void> {
  if (isTauri()) {
    return invoke("summarize_message", { id: message.id, style, window: RUNS, onEvent: channel(onEvent) });
  }
  browserSummarize(message, style, onEvent);
  return Promise.resolve();
}

export function cancelSummary(id: number): Promise<void> {
  if (isTauri()) return invoke("cancel_summary", { id, window: RUNS });
  browserStop(`summary:${id}`);
  return Promise.resolve();
}

/**
 * This window's own range for its AI runs where the backend keys them: composers are numbered
 * from 1 in each of the app's windows, and two windows can summarize the same email (they'd
 * stop each other). Under 2^52, so it stays exact.
 */
const RUNS = crypto.getRandomValues(new Uint32Array(1))[0] * 2 ** 20;

/**
 * Starts AI writing for composer number `composer`, whose draft is from
 * `account` and about `reference`; the text arrives on `onEvent`. Resolves
 * once it has started.
 */
/** `signed`: the draft has a signature below, so what's written doesn't sign off. */
export function composeWithAi(
  composer: number,
  action: ComposeAction,
  account: AccountId,
  reference: Reference | null,
  signed: boolean,
  onEvent: (event: AiEvent) => void,
): Promise<void> {
  if (isTauri()) {
    return invoke("compose_with_ai", {
      composer: RUNS + composer,
      action,
      account,
      reference,
      signed,
      onEvent: channel(onEvent),
    });
  }
  browserStream(`composer:${composer}`, browserComposed(action), onEvent);
  return Promise.resolve();
}

export function cancelCompose(composer: number): Promise<void> {
  if (isTauri()) return invoke("cancel_compose", { composer: RUNS + composer });
  browserStop(`composer:${composer}`);
  return Promise.resolve();
}

function channel(onEvent: (event: AiEvent) => void) {
  const events = new Channel<AiEvent>();
  events.onmessage = onEvent;
  return events;
}

// ---- browser preview (`npm run dev`) ----
// Nothing is sent anywhere: settings live in memory, a typed key only flips
// `hasKey`, and summaries and composer text are labeled stand-ins streamed word by word.

const BROWSER_ONLY = "The browser preview doesn't call AI providers. Run the desktop app to test the connection.";

const browser = {
  settings: null as { preset: Preset; config: ProviderConfig } | null,
  keys: new Set<Preset>(),
  streams: new Map<string, number>(),
};

function browserView(s: { preset: Preset; config: ProviderConfig }): AiSettings {
  return { ...s, hasKey: browser.keys.has(s.preset) };
}

function browserSave(preset: Preset, config: ProviderConfig, apiKey?: string): Promise<AiSettings> {
  const url = config.baseUrl.trim();
  if (url && !/^https?:\/\//.test(url)) {
    return Promise.reject("The base URL must start with http:// or https://, e.g. http://localhost:11434/v1.");
  }
  const defaults = presetDefaults(preset);
  browser.settings = {
    preset,
    config: {
      kind: defaults.kind,
      baseUrl: url || defaults.baseUrl,
      model: config.model.trim(),
      ...(preset === "anthropic" ? { effort: config.effort ?? "medium" } : {}),
    },
  };
  if (apiKey !== undefined) {
    if (apiKey.trim()) browser.keys.add(preset);
    else browser.keys.delete(preset);
  }
  return Promise.resolve(browserView(browser.settings));
}

function browserMissing(): string | null {
  const s = browser.settings;
  if (!s) return "Choose a provider and add your API key in Settings → AI.";
  if (s.preset !== "custom" && !browser.keys.has(s.preset)) {
    return `Add your ${presetLabel(s.preset)} API key in Settings → AI.`;
  }
  if (!s.config.baseUrl) return "Enter your server's base URL in Settings → AI.";
  if (!s.config.model) return "Choose a model in Settings → AI.";
  return null;
}

function browserSummarize(m: Message, style: SummaryStyle, onEvent: (event: AiEvent) => void) {
  const firstLine = m.body?.find((p) => p.trim().length > 20) ?? m.snippet;
  const text =
    style === "actionPoints"
      ? `- Preview only, not written by AI.\n- Reply to ${m.from} about "${m.subject}".`
      : `Preview only, not written by AI: ${m.from} wrote about "${m.subject}".\n` +
        `- ${firstLine.trim()}\n- In the desktop app this card shows your provider's summary.`;
  browserStream(`summary:${m.id}`, text, onEvent);
}

function browserComposed(action: ComposeAction) {
  const note = "(Preview only, not written by AI: in the desktop app your provider writes this.)";
  switch (action.type) {
    case "write":
      return `Hi,\n\nAn email about "${action.instructions.trim()}". ${note}\n\nBest,`;
    case "quickReply":
      return `Hi,\n\n${QUICK_REPLIES.find((q) => q.key === action.reply)!.text.trim()} ${note}\n\nBest,`;
    case "rewrite":
    case "refine":
      return `${action.text.trim()} (Preview only, not rewritten by AI.)`;
  }
}

/** Streams `text` word by word as `key`'s run, or says what's missing. */
function browserStream(key: string, text: string, onEvent: (event: AiEvent) => void) {
  const missing = browserMissing();
  if (missing) {
    onEvent({ event: "notConfigured", data: { message: missing } });
    return;
  }
  const words = text.split(/(?<=\s)/);
  let i = 0;
  browserStop(key);
  const timer = window.setInterval(() => {
    if (i < words.length) {
      onEvent({ event: "delta", data: { text: words[i++] } });
      return;
    }
    browserStop(key);
    onEvent({
      event: "done",
      data: { model: "browser-preview", inputTokens: 0, outputTokens: 0, truncated: false },
    });
  }, 35);
  browser.streams.set(key, timer);
}

function browserStop(key: string) {
  window.clearInterval(browser.streams.get(key));
  browser.streams.delete(key);
}
