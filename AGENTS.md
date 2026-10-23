# AGENTS.md

## Agent skills

### Issue tracker

Issues live on git.cafe (`michael/spark-rs`) and are managed with the `cafe` CLI. See `docs/agents/issue-tracker.md`.

### Triage labels

The five default labels: `needs-triage`, `needs-info`, `ready-for-agent`, `ready-for-human`, `wontfix`. See `docs/agents/triage-labels.md`.

### Domain docs

Single-context: one `GLOSSARY.md` and `docs/adr/` at the repo root. See `docs/agents/domain.md`.

### Git workflow

More than one machine works on this repo, and git.cafe is all they share. Sync with origin when a session starts, and commit and push each finished change before ending the turn (unfinished work goes to a `wip/` branch). See `docs/agents/git-workflow.md`.
