# Issue tracker: git.cafe

Issues and specs for this repo live as issues on git.cafe (`michael/spark-rs`). Use the `cafe` CLI for all operations. It picks the repo from `git remote -v` when run inside a clone; elsewhere pass `--repo michael/spark-rs`. Add `--json` when you need to parse the output.

On Windows, run `cafe` from PowerShell: its shim fails under Git Bash and other MSYS shells.

## Conventions

- **Create an issue**: `cafe issue create --title "..." --body-file <file> --no-input`. Use a body file for multi-line bodies; add `--label <name>` per label.
- **Read an issue**: `cafe issue view <number>` for the body and labels, `cafe issue comments <number>` for the conversation.
- **List issues**: `cafe issue list --state open --json`, with `--label <name>` and `--state` filters.
- **Comment on an issue**: `cafe issue comment <number> --body "..."` (or `--body-file <file>`)
- **Apply / remove labels**: `cafe issue label add <number> <name>` / `cafe issue label remove <number> <name>`. A label has to exist before it can be applied: check `cafe issue label list` and create a missing one with `cafe issue label create <name> --color "#rrggbb" --description "..."`.
- **Close**: `cafe issue comment <number> --body "..."`, then `cafe issue close <number>`. For won't-do, use `cafe issue edit <number> --state canceled` instead of `close`.

git.cafe issues also have a workflow state (`triage`, `backlog`, `unstarted`, `started`, `completed`, `canceled`). Triage roles are tracked with the labels in `triage-labels.md`; leave the workflow state at its default except when closing.

## Pull requests as a triage surface

**PRs as a request surface: no.** _(Set to `yes` if this repo treats external PRs as feature requests; `/triage` reads this flag.)_

When set to `yes`, PRs run through the same labels and states as issues, using the `cafe pr` equivalents (`cafe pr list`, `cafe pr view <number>`; see `cafe pr --help`).

## When a skill says "publish to the issue tracker"

Create a git.cafe issue with `cafe issue create`.

## When a skill says "fetch the relevant ticket"

Run `cafe issue view <number>` and `cafe issue comments <number>`.

## Wayfinding operations

Used by `/wayfinder`. The **map** is a single issue with **child** issues as tickets. The `cafe` CLI has no sub-issues or issue dependencies, so links live in issue bodies.

- **Map**: a single issue labelled `wayfinder:map`, holding the Notes / Decisions-so-far / Fog body. `cafe issue create --label wayfinder:map`.
- **Child ticket**: an issue with `Part of #<map>` at the top of its body, added to a task list in the map body. Labels: `wayfinder:<type>` (`research`/`prototype`/`grilling`/`task`). Once claimed, the ticket is assigned to the driving dev.
- **Blocking**: a `Blocked by: #<n>, #<n>` line at the top of the child body. A ticket is unblocked when every blocker is closed.
- **Frontier query**: take the open issues in the map's task list, drop any with an open blocker in its `Blocked by` line or with an assignee; first in map order wins.
- **Claim**: `cafe issue assign <n> <your-handle>`, the session's first write.
- **Resolve**: `cafe issue comment <n> --body "<answer>"`, then `cafe issue close <n>`, then append a context pointer (gist + link) to the map's Decisions-so-far.
