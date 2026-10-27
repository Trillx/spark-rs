import { ArrowDown, ArrowLeft, ArrowUp, Braces, ChevronRight, Copy, Ellipsis, Plus, Trash2 } from "lucide-react";
import { useEffect, useRef, useState, type FormEvent } from "react";
import { deleteTemplate, listTemplates, orderTemplates, saveTemplate } from "../api";
import type { Template } from "../types";
import { placeholderHtml } from "../lib/richText";
import { PRESET_PLACEHOLDERS } from "../lib/templates";
import Dropdown, { MenuItem } from "./Dropdown";
import RichEditor, { type RichEditorHandle } from "./RichEditor";
import { Group, INPUT, StatusLine, type Status } from "./Settings";

/** A template being written: one to edit, or a new one (from a draft, say). */
export interface TemplateDraft {
  id: number | null;
  name: string;
  subject: string;
  html: string;
}

/**
 * Settings → Templates: Spark's list of your templates, in the order the
 * composer lists them, and the editor for one.
 */
export default function TemplatesPage({ start, onStarted }: { start?: TemplateDraft | null; onStarted?: () => void }) {
  const [templates, setTemplates] = useState<Template[] | null>(null);
  const [editing, setEditing] = useState<TemplateDraft | null>(null);
  const [status, setStatus] = useState<Status>({ kind: "idle" });

  function load() {
    listTemplates().then(setTemplates, (error) => setStatus({ kind: "error", text: String(error) }));
  }
  useEffect(load, []);

  // "Create a New Template from draft" opens here with the draft in it.
  useEffect(() => {
    if (!start) return;
    setEditing(start);
    onStarted?.();
  }, [start]);

  if (editing) {
    return (
      <TemplateEditor
        initial={editing}
        onDone={() => {
          setEditing(null);
          load();
        }}
      />
    );
  }
  if (!templates) return <StatusLine status={status} />;

  async function run(what: () => Promise<unknown>) {
    try {
      await what();
      setStatus({ kind: "idle" });
    } catch (error) {
      setStatus({ kind: "error", text: String(error) });
    }
    load();
  }

  function move(index: number, by: number) {
    const ids = templates!.map((t) => t.id);
    const [id] = ids.splice(index, 1);
    ids.splice(index + by, 0, id);
    void run(() => orderTemplates(ids));
  }

  return (
    <>
      <div className="mb-6 flex items-center gap-4">
        <p className="flex-1 text-xs leading-[18px] text-text-2">
          Start an email from a template with the template button in the composer or Ctrl+Shift+T. Placeholders fill in
          the recipient's name and yours.
        </p>
        <button
          className="btn btn-primary btn-sm gap-1.5"
          onClick={() => setEditing({ id: null, name: "", subject: "", html: "" })}
        >
          <Plus size={14} />
          Create template
        </button>
      </div>
      {templates.length > 0 && (
        <Group title="My Templates">
          {templates.map((t, i) => (
            <div key={t.id} className="flex min-h-[52px] items-center first:rounded-t-2xl last:rounded-b-2xl hover:bg-base-content/5">
              <button
                className="flex min-w-0 flex-1 items-center gap-3 py-2.5 pl-4 text-left"
                onClick={() => setEditing({ id: t.id, name: t.name, subject: t.subject, html: t.html })}
              >
                <span className="min-w-0 flex-1">
                  <span className="block truncate text-[13px] font-medium leading-5">{t.name}</span>
                  <span className="block truncate text-xs leading-[18px] text-text-2">
                    {[t.subject, t.text.replace(/\s+/g, " ")].filter(Boolean).join(" — ")}
                  </span>
                </span>
                <ChevronRight size={16} className="shrink-0 text-text-3" />
              </button>
              <Dropdown
                align="end"
                width="w-48"
                trigger={({ toggle, open }) => (
                  <button
                    aria-label={`More for ${t.name}`}
                    aria-expanded={open}
                    className="mx-2 grid size-7 place-items-center rounded-md text-text-2 hover:bg-base-content/10"
                    onClick={toggle}
                  >
                    <Ellipsis size={15} />
                  </button>
                )}
              >
                <MenuItem
                  label="Duplicate"
                  icon={<Copy size={14} />}
                  onSelect={() => void run(() => saveTemplate(null, `${t.name} copy`, t.subject, t.html))}
                />
                {i > 0 && <MenuItem label="Move Up" icon={<ArrowUp size={14} />} onSelect={() => move(i, -1)} />}
                {i < templates.length - 1 && (
                  <MenuItem label="Move Down" icon={<ArrowDown size={14} />} onSelect={() => move(i, 1)} />
                )}
                <MenuItem label="Delete" danger icon={<Trash2 size={14} />} onSelect={() => void run(() => deleteTemplate(t.id))} />
              </Dropdown>
            </div>
          ))}
        </Group>
      )}
      <StatusLine status={status} />
    </>
  );
}

/** Creating or editing a template: its name, subject and body, with placeholders. */
function TemplateEditor({ initial, onDone }: { initial: TemplateDraft; onDone: () => void }) {
  const [name, setName] = useState(initial.name);
  const [subject, setSubject] = useState(initial.subject);
  const [html, setHtml] = useState(initial.html);
  const [custom, setCustom] = useState<string | null>(null);
  const [status, setStatus] = useState<Status>({ kind: "idle" });
  const editor = useRef<RichEditorHandle>(null);
  const busy = status.kind === "busy";

  async function save() {
    setStatus({ kind: "busy", what: "Saving…" });
    try {
      await saveTemplate(initial.id, name, subject, html);
      onDone();
    } catch (error) {
      setStatus({ kind: "error", text: String(error) });
    }
  }

  async function remove() {
    if (initial.id === null) return onDone();
    try {
      await deleteTemplate(initial.id);
      onDone();
    } catch (error) {
      setStatus({ kind: "error", text: String(error) });
    }
  }

  function addCustom(e: FormEvent) {
    e.preventDefault();
    const label = custom?.trim();
    if (label) editor.current?.insertHtml(placeholderHtml("custom", label));
    setCustom(null);
  }

  return (
    <>
      <button className="btn btn-ghost btn-sm -ml-2 mb-2 gap-1 text-text-2" onClick={onDone}>
        <ArrowLeft size={14} />
        Templates
      </button>
      <h3 className="mb-3 px-1 text-[15px] font-semibold">{initial.id === null ? "Create template" : "Edit template"}</h3>
      <div className="overflow-hidden rounded-2xl border border-hairline">
        <div className="flex h-10 items-center gap-2 border-b border-hairline px-4 text-[13px]">
          <label htmlFor="template-name" className="w-14 shrink-0 text-text-2">
            Name
          </label>
          <input
            id="template-name"
            autoFocus={initial.id === null && !initial.name}
            className="min-w-0 flex-1 bg-transparent font-medium outline-none placeholder:font-normal placeholder:text-text-3"
            placeholder="Template name"
            value={name}
            onChange={(e) => setName(e.target.value)}
          />
        </div>
        <div className="flex h-10 items-center gap-2 border-b border-hairline px-4 text-[13px]">
          <label htmlFor="template-subject" className="w-14 shrink-0 text-text-2">
            Subject
          </label>
          <input
            id="template-subject"
            className="min-w-0 flex-1 bg-transparent outline-none placeholder:text-text-3"
            placeholder="Optional: used when the email has none"
            value={subject}
            onChange={(e) => setSubject(e.target.value)}
          />
        </div>
        <div role="toolbar" aria-label="Placeholders" className="flex h-10 items-center gap-2 border-b border-hairline px-2">
          <Dropdown
            width="w-56"
            trigger={({ toggle, open }) => (
              <button
                className={`btn btn-ghost btn-xs h-7 gap-1.5 px-2 font-medium ${open ? "bg-primary/10 text-primary" : "text-text-2"}`}
                aria-expanded={open}
                // Keeps the caret in the editor.
                onMouseDown={(e) => e.preventDefault()}
                onClick={toggle}
              >
                <Braces size={14} />
                Add placeholder
              </button>
            )}
          >
            {PRESET_PLACEHOLDERS.map((p) => (
              <MenuItem key={p.kind} label={p.label} onSelect={() => editor.current?.insertHtml(placeholderHtml(p.kind, p.label))} />
            ))}
            <li role="separator" className="my-0.5 h-px bg-hairline" />
            <MenuItem label="Custom…" onSelect={() => setCustom("")} />
          </Dropdown>
          {custom !== null && (
            <form onSubmit={addCustom} className="flex flex-1 items-center gap-2">
              <input
                autoFocus
                aria-label="Placeholder name"
                className={`${INPUT} flex-1`}
                placeholder="What to fill in, e.g. Company"
                value={custom}
                onChange={(e) => setCustom(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key !== "Escape") return;
                  e.preventDefault();
                  e.stopPropagation();
                  setCustom(null);
                }}
              />
              <button type="submit" className="btn btn-primary btn-sm" disabled={!custom.trim()}>
                Add
              </button>
            </form>
          )}
        </div>
        <RichEditor
          ref={editor}
          html={html}
          onChange={(next) => setHtml(next)}
          placeholder="Write the template"
          minHeight={220}
          imageSize="actual"
          onError={(text) => setStatus({ kind: "error", text })}
        />
      </div>
      <div className="mt-4 flex items-center gap-2">
        <button className="btn btn-primary btn-sm" disabled={busy} onClick={() => void save()}>
          Save
        </button>
        {initial.id !== null && (
          <button className="btn btn-ghost btn-sm text-error" disabled={busy} onClick={() => void remove()}>
            Delete
          </button>
        )}
        <StatusLine status={status} />
      </div>
    </>
  );
}
