import {
  DraftingCompass,
  Download,
  File,
  FileArchive,
  FileCode,
  FileImage,
  FileMusic,
  FileSpreadsheet,
  FileText,
  FileVideoCamera,
  LoaderCircle,
  Mail,
  Paperclip,
  Presentation,
  X,
  type LucideIcon,
} from "lucide-react";
import { useState } from "react";
import type { Attachment } from "../types";
import { formatBytes, plural } from "../lib/format";

interface FileKind {
  /** Short type label, e.g. "PDF". */
  label: string;
  Icon: LucideIcon;
  /** Icon tile colors. */
  tint: string;
}

const KINDS: { test: RegExp; Icon: LucideIcon; tint: string }[] = [
  { test: /^(pdf)$/, Icon: FileText, tint: "bg-error/12 text-error" },
  { test: /^(png|jpe?g|gif|webp|heic|bmp|tiff?|svg)$/, Icon: FileImage, tint: "bg-secondary/15 text-secondary" },
  { test: /^(zip|7z|rar|gz|tar|tgz)$/, Icon: FileArchive, tint: "bg-warning/15 text-warning" },
  { test: /^(xlsx?|xlsm|csv|ods|numbers)$/, Icon: FileSpreadsheet, tint: "bg-success/15 text-success" },
  { test: /^(pptx?|key|odp)$/, Icon: Presentation, tint: "bg-priority/15 text-priority" },
  { test: /^(docx?|rtf|odt|pages|txt|md)$/, Icon: FileText, tint: "bg-primary/12 text-primary" },
  { test: /^(dxf|dwg|step|stp|igs|iges|stl|sldprt|sldasm|ipt|iam)$/, Icon: DraftingCompass, tint: "bg-info/15 text-info" },
  { test: /^(mp4|mov|avi|mkv|webm)$/, Icon: FileVideoCamera, tint: "bg-accent/15 text-accent" },
  { test: /^(mp3|wav|m4a|aac|flac)$/, Icon: FileMusic, tint: "bg-accent/15 text-accent" },
  { test: /^(eml|msg)$/, Icon: Mail, tint: "bg-primary/12 text-primary" },
  { test: /^(html?|json|xml|js|ts|css|py|rs)$/, Icon: FileCode, tint: "bg-base-content/10 text-text-2" },
];

/** How to show a file: by extension, else by MIME type. */
export function fileKind(name: string, mime = ""): FileKind {
  const ext = name.includes(".") ? name.split(".").pop()!.toLowerCase() : "";
  const fromMime = mime === "message/rfc822" ? "eml" : mime.startsWith("image/") ? "png" : mime === "application/pdf" ? "pdf" : "";
  const key = ext || fromMime;
  const kind = KINDS.find((k) => k.test.test(key));
  return {
    label: (ext || fromMime || "file").slice(0, 4).toUpperCase(),
    Icon: kind?.Icon ?? File,
    tint: kind?.tint ?? "bg-base-content/10 text-text-2",
  };
}

/**
 * A message's attachments, Spark style: a header with "Save all", then a
 * tile per file. Clicking a tile opens it; its corner button saves it.
 */
export function AttachmentList({
  attachments,
  onOpen,
  onSave,
  onSaveAll,
}: {
  attachments: Attachment[];
  onOpen: (index: number) => Promise<void>;
  onSave: (index: number) => Promise<void>;
  onSaveAll: () => Promise<void>;
}) {
  const [busy, setBusy] = useState<Set<number>>(() => new Set());
  const [savingAll, setSavingAll] = useState(false);
  const total = attachments.reduce((sum, a) => sum + a.size, 0);

  async function run(index: number, action: (index: number) => Promise<void>) {
    if (busy.has(index)) return;
    setBusy((b) => new Set(b).add(index));
    try {
      await action(index);
    } finally {
      setBusy((b) => {
        const next = new Set(b);
        next.delete(index);
        return next;
      });
    }
  }

  return (
    <section aria-label="Attachments" className="mx-5 mb-4 border-t border-hairline pt-3">
      <div className="mb-2.5 flex items-center gap-2 text-xs leading-5">
        <Paperclip size={14} className="rotate-45 text-text-2" />
        <span className="font-semibold">{plural(attachments.length, "attachment")}</span>
        <button
          className="inline-flex items-center gap-1 font-medium text-primary hover:underline disabled:opacity-60"
          disabled={savingAll}
          data-tip="Save every file to a folder"
          onClick={async () => {
            setSavingAll(true);
            try {
              await onSaveAll();
            } finally {
              setSavingAll(false);
            }
          }}
        >
          {savingAll && <LoaderCircle size={12} className="animate-spin" />}
          Save all{total > 0 && <span className="text-text-2">{formatBytes(total)}</span>}
        </button>
      </div>
      <ul className="flex flex-wrap gap-3">
        {attachments.map((a, i) => (
          <AttachmentTile
            key={i}
            attachment={a}
            busy={busy.has(i)}
            onOpen={() => void run(i, onOpen)}
            onSave={() => void run(i, onSave)}
          />
        ))}
      </ul>
    </section>
  );
}

function AttachmentTile({
  attachment: a,
  busy,
  onOpen,
  onSave,
}: {
  attachment: Attachment;
  busy: boolean;
  onOpen: () => void;
  onSave: () => void;
}) {
  const kind = fileKind(a.name, a.mime);
  return (
    <li className="group relative w-[128px]">
      <button
        className="relative flex h-[104px] w-full flex-col items-center justify-center gap-1.5 rounded-xl border border-hairline bg-base-100 transition duration-150 hover:-translate-y-0.5 hover:border-primary/40 hover:shadow-popup focus-visible:outline-2 focus-visible:outline-primary"
        aria-label={`Open ${a.name}`}
        data-tip="Open"
        data-tip-detail={a.name}
        onClick={onOpen}
      >
        <span className={`grid size-12 place-items-center rounded-xl ${kind.tint}`}>
          <kind.Icon size={26} strokeWidth={1.75} />
        </span>
        <span className="text-[10px] font-bold tracking-wider text-text-2">{kind.label}</span>
        {busy && (
          <span className="absolute inset-0 grid place-items-center rounded-xl bg-base-100/75">
            <LoaderCircle size={22} className="animate-spin text-primary" />
          </span>
        )}
      </button>
      <button
        className="absolute right-1.5 top-1.5 grid size-7 place-items-center rounded-lg bg-base-100 text-text-2 opacity-0 shadow-sm ring-1 ring-hairline transition hover:text-primary focus-visible:opacity-100 group-hover:opacity-100"
        aria-label={`Save ${a.name}`}
        data-tip="Save to…"
        onClick={onSave}
      >
        <Download size={14} />
      </button>
      <div className="mt-1.5 line-clamp-2 break-words text-center text-xs font-medium leading-4" data-tip={a.name}>
        {a.name}
      </div>
      {a.size > 0 && <div className="text-center text-[11px] leading-4 text-text-2">{formatBytes(a.size)}</div>}
    </li>
  );
}

/** A file on the draft in the composer, with a remove button. */
export function AttachmentChip({ name, size, onRemove }: { name: string; size: number; onRemove: () => void }) {
  const kind = fileKind(name);
  return (
    <li className="flex h-9 max-w-[240px] items-center gap-2 rounded-lg border border-hairline bg-base-100 pl-1.5 pr-1 text-xs">
      <span className={`grid size-6 shrink-0 place-items-center rounded-md ${kind.tint}`}>
        <kind.Icon size={14} />
      </span>
      <span className="min-w-0 flex-1 truncate font-medium" data-tip={name}>
        {name}
      </span>
      {size > 0 && <span className="shrink-0 text-text-2">{formatBytes(size)}</span>}
      <button
        className="grid size-6 shrink-0 place-items-center rounded-md text-text-2 hover:bg-base-content/10 hover:text-base-content"
        aria-label={`Remove ${name}`}
        data-tip="Remove"
        onClick={onRemove}
      >
        <X size={13} />
      </button>
    </li>
  );
}
