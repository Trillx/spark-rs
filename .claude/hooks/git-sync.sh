#!/usr/bin/env bash
# SessionStart hook: catches this checkout up with git.cafe, so a session
# starts from whatever the other machine pushed. Only a clean checkout that
# is strictly behind gets fast-forwarded; anything else is reported. The
# output becomes context for the agent. See docs/agents/git-workflow.md.

cd "$(git rev-parse --show-toplevel 2>/dev/null)" || exit 0

# The repo's git hooks (.githooks) need a per-clone setting; turn them on
# unless this clone already uses hooks of its own.
[ -n "$(git config core.hooksPath)" ] || git config core.hooksPath .githooks
# A plain `git push` goes to the branch a checkout tracks, even when the names
# differ: T3 Code's worktree branches track main (below) and push to it.
[ -n "$(git config push.default)" ] || git config push.default upstream

branch=$(git symbolic-ref --short -q HEAD) || {
  echo "git: detached HEAD, nothing synced."
  exit 0
}

if ! GIT_TERMINAL_PROMPT=0 timeout 20 git fetch -q origin 2>/dev/null; then
  echo "git: couldn't reach origin, so what follows may be out of date."
fi

dirty=$(git status --porcelain | wc -l)

# T3 Code starts each thread's worktree on a branch of its own that tracks
# nothing. Changes go to main, so one with no commits of its own tracks main:
# T3's branch menu then shows it in sync, and its Push button pushes to main.
if ! git rev-parse -q --verify '@{upstream}' >/dev/null 2>&1 &&
  git merge-base --is-ancestor HEAD origin/main 2>/dev/null; then
  git branch -q --set-upstream-to=origin/main "$branch" &&
    echo "git: $branch had no upstream; it now tracks origin/main."
fi

upstream=$(git rev-parse --abbrev-ref -q '@{upstream}' 2>/dev/null) || {
  echo "git: $branch has no upstream on origin (never pushed), $dirty uncommitted file(s)."
  exit 0
}

read -r ahead behind < <(git rev-list --left-right --count 'HEAD...@{upstream}')

if [ "$behind" -gt 0 ] && [ "$ahead" -eq 0 ] && [ "$dirty" -eq 0 ]; then
  if git merge -q --ff-only '@{upstream}' 2>/dev/null; then
    echo "git: fast-forwarded $branch by $behind commit(s) from $upstream."
    behind=0
  else
    echo "git: fast-forwarding $branch to $upstream failed."
  fi
fi

if [ "$ahead" -eq 0 ] && [ "$behind" -eq 0 ] && [ "$dirty" -eq 0 ]; then
  echo "git: $branch matches $upstream, working tree clean."
  exit 0
fi

echo "git: $branch vs $upstream: $ahead unpushed commit(s), $behind commit(s) behind, $dirty uncommitted file(s)."
if [ "$behind" -gt 0 ]; then
  echo "Origin has work this checkout lacks. Tell the user before editing; don't stash, reset or rebase without asking."
fi
if [ "$ahead" -gt 0 ] || [ "$dirty" -gt 0 ]; then
  echo "Work here isn't on origin yet, so the other machine can't see it. Mention it to the user and offer to commit and push it."
fi
