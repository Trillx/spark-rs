#!/usr/bin/env bash
# The scheduled GitHub mirror remains a fallback when this server is offline.
set -euo pipefail
source_sha="$(git ls-remote https://git.cafe/michael/spark-rs.git refs/heads/main | cut -f1)"
mirror_sha="$(gh api repos/Trillx/spark-rs/git/ref/heads/main --jq .object.sha)"
test -n "$source_sha"
if [ "$source_sha" = "$mirror_sha" ]; then exit 0; fi

# Don't queue another workflow while the previous mirror is catching up.
active="$(gh run list -R Trillx/spark-rs --workflow mirror.yml --limit 20 \
  --json status --jq '[.[] | select(.status != "completed")] | length')"
if [ "$active" != 0 ]; then exit 0; fi
gh workflow run mirror.yml -R Trillx/spark-rs
