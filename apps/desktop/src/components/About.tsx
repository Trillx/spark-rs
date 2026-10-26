import { CircleCheck, Download, LoaderCircle, RefreshCw } from "lucide-react";
import { useState } from "react";
import type { Updates } from "../lib/updates";
import { ConfirmDialog } from "./Dialog";

export default function About({ updates }: { updates: Updates }) {
  const { view, progress, check, install } = updates;
  const [confirm, setConfirm] = useState(false);
  const error = updates.error ?? view?.error;
  const busy = view?.checking || view?.installing;
  const available = view?.available;
  const percent = progress?.total ? Math.min(100, Math.floor((progress.downloaded / progress.total) * 100)) : null;

  return (
    <>
      <div className="mb-7 px-1">
        <h3 className="text-xl font-semibold">spark-rs</h3>
        <p className="mt-1 select-text text-[13px] text-text-2">
          {view
            ? `${view.version}${view.build === null ? "" : ` · build ${view.build}`}${view.commit ? ` (${view.commit})` : ""}`
            : "Loading version…"}
        </p>
      </div>
      <section className="rounded-2xl border border-hairline p-4" aria-label="Updates">
        <div className="flex items-center gap-2 text-[13px] font-semibold" role="status">
          {busy ? (
            <LoaderCircle size={16} className="animate-spin text-primary" />
          ) : available ? (
            <Download size={16} className="text-primary" />
          ) : (
            <CircleCheck size={16} className="text-text-2" />
          )}
          {view?.installing
            ? "Downloading update…"
            : view?.checking
              ? "Checking for updates…"
              : !view
                ? "Updates"
                : !view.canUpdate
                  ? "Updates are available in installed copies"
                  : available
                    ? `${available.behind.count} ${available.behind.count === 1 ? "commit" : "commits"} behind`
                    : view.checkedAt
                      ? "Up to date"
                      : "Ready to check for updates"}
        </div>
        {view && !view.canUpdate ? (
          <p className="mt-2 text-xs leading-5 text-text-2">
            Install spark-rs with the Windows installer to get updates here.
          </p>
        ) : (
          <>
            <p className="mt-2 text-xs leading-5 text-text-2">
              {view?.checkedAt
                ? `Last checked ${new Date(view.checkedAt).toLocaleString()}.`
                : "spark-rs checks on start and every four hours."}{" "}
              Updates install when you choose.
            </p>
            {available && available.behind.commits.length > 0 && (
              <ul className="mt-4 space-y-2 border-t border-hairline pt-3 text-[13px] leading-5">
                {available.behind.commits.map((commit) => (
                  <li key={commit.sha} className="flex gap-2">
                    <span className="text-text-2">·</span>
                    <span>{commit.subject}</span>
                  </li>
                ))}
              </ul>
            )}
            {view?.installing && (
              <div className="mt-4">
                <progress
                  aria-label="Update download"
                  className="progress progress-primary w-full"
                  max={100}
                  value={percent ?? undefined}
                />
                <p className="mt-1 text-xs text-text-2">
                  {percent === null ? "Downloading…" : `${percent}%`}
                  {percent === 100 ? " · Preparing to restart…" : ""}
                </p>
              </div>
            )}
            <div className="mt-4 flex flex-wrap gap-2">
              {available && (
                <button className="btn btn-primary btn-sm rounded-lg" disabled={busy} onClick={() => setConfirm(true)}>
                  <Download size={14} />
                  Update and restart
                </button>
              )}
              <button className="btn btn-sm rounded-lg" disabled={busy} onClick={() => void check()}>
                <RefreshCw size={14} />
                {error ? "Retry" : "Check for updates"}
              </button>
            </div>
          </>
        )}
        {error && (
          <p role="alert" className="mt-3 select-text text-xs leading-5 text-error">
            Couldn't {view?.available ? "update spark-rs" : "check for updates"}: {error}
          </p>
        )}
      </section>
      {confirm && (
        <ConfirmDialog
          title="Update spark-rs?"
          message="spark-rs will download the update and restart. Agents using spark-rs will disconnect. Finish writing any unsent emails before continuing."
          confirmLabel="Update and restart"
          onClose={() => setConfirm(false)}
          onConfirm={() => {
            setConfirm(false);
            void install();
          }}
        />
      )}
    </>
  );
}
