# AGENTS.md

## Agent skills

### Issue tracker

Issues live on git.cafe (`michael/spark-rs`) and are managed with the `cafe` CLI. See `docs/agents/issue-tracker.md`.

### Milestones

The roadmap is git.cafe's milestones, M1 onward (`cafe milestone list`). Work toward one names it: a `Milestone: M7` line in commit bodies and issue bodies. See `docs/agents/issue-tracker.md`.

### Triage labels

The five default labels: `needs-triage`, `needs-info`, `ready-for-agent`, `ready-for-human`, `wontfix`. See `docs/agents/triage-labels.md`.

### Domain docs

Single-context: one `GLOSSARY.md` and `docs/adr/` at the repo root. See `docs/agents/domain.md`.

### Git workflow

More than one machine works on this repo, and git.cafe is all they share. Sync with origin when a session starts, and commit and push each finished change before ending the turn (unfinished work goes to a `wip/` branch). See `docs/agents/git-workflow.md`.

### Feature table

`FEATURES.md` marks every feature on Spark's pricing page ✅, 🟡, ❌ or ➖ for spark-rs. When a change adds, finishes or removes a user-visible feature, update that row's mark and note in the same commit. ✅ means it works end to end with a real account, beyond the demo mailbox; a 🟡 note names what's missing. Asked for a full check, verify every row against the code and update "Last full check".
