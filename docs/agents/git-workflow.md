# Git workflow: keeping machines in sync

This repo is worked on from more than one machine (a Windows ARM laptop and a Linux server). The only thing they share is git.cafe (`origin`, `michael/spark-rs`), so work that isn't pushed doesn't exist on the other machine.

GitHub (`Trillx/spark-rs`) is a read-only mirror of `main`, there for release downloads. Never push to it: the mirror workflow overwrites it from git.cafe. Releases are cut by bumping the app version on `main` (see "Releases" in the README).

## Starting a session

- Run `git fetch origin` and `git status -sb`. Claude Code does this for you: the SessionStart hook in `.claude/settings.json` (`.claude/hooks/git-sync.sh`) fetches, fast-forwards a clean checkout that's behind, and reports anything else.
- If the checkout is behind and clean, run `git pull --ff-only`.
- The same hook turns on the repo's git hooks in `.githooks` (`git config core.hooksPath .githooks`, once per clone; run it yourself in a clone Claude Code hasn't opened). Their post-commit hook reminds you to update `FEATURES.md` when a commit changes the app.
- If there are uncommitted changes or unpushed commits, or the checkout has diverged from origin, tell the user before editing anything. Never stash, reset or rebase their work without asking.

## Finishing a change

A change is finished when it builds and its tests pass. In the same turn:

1. Commit only the files that belong to the change, with a message that says what changed.
2. Push it: `git push origin main`. By default, changes go straight to `main`.
3. Open a PR instead when the user asks for one. Push the branch, then follow "Pull requests" below.

Don't end a turn with a finished change that's still only local. If the push fails, say so.

## Stopping partway through

When work has to pause before it builds, don't leave it only on this machine, and don't push it to `main`: the other machine builds from `main`. Put it on a branch instead:

```
git switch -c wip/<topic>
git commit -am "WIP: <what's done, what's left>"
git push -u origin wip/<topic>
```

Tell the user the branch name. On the other machine, `git fetch && git switch wip/<topic>` picks it up. When the work is finished, it goes to `main` (or a PR) as usual, and the `wip/` branch gets deleted on origin.

## Never commit

- Secrets: tokens, API keys, `.env` files, OAuth credentials. The repo is public.
- Real mail, account addresses, or screenshots of the user's mailbox.
- Build output (`target/`, `dist/`, `node_modules/`), which `.gitignore` already covers.

## Pull requests

- Create: `cafe pr create --repo michael/spark-rs --base main --head <branch> --title "..." --body-file <file> --no-input --json`. On Windows, run `cafe` from PowerShell (see `issue-tracker.md`).
- In T3 Code, link it to the thread with `link_pull_request`, passing `host: git.cafe`, `repository: michael/spark-rs` and the number. The full URL is rejected.
- Issues and PRs share one number space, so refer to a PR by its full `/pulls/<n>` URL in issue text.
