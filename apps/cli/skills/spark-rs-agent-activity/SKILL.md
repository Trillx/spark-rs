---
name: spark-rs-agent-activity
description: >-
  Explain from spark-rs's history what AI agents changed, read or were
  refused, and undo what the user wants put back. Use when the user asks
  "what did you do", "what did the agent change", "why did that email
  move", "undo that", or why an agent was blocked.
metadata:
  version: {{version}}
  requires:
    skills:
      - use-spark-rs
    access: read
---

# Agent activity

A plain account of what agents did in spark-rs, from its own record: what
changed, what was refused and why, and what can be put back.

**Before you start:** this builds on `use-spark-rs`; follow its "Working
safely" rules. Reading the history needs read access; undoing needs
triage in the accounts the entry touched.

## 1. List the changes

```bash
spark-cli history
```

Newest first: ID, time, agent, command, result (success, error or
blocked) and a summary, with "can undo" where it can. Reads are left out
by default. It keeps 30 days.

## 2. Narrow it down

```bash
spark-cli history --from 2026-10-27 --to 2026-10-28
spark-cli history --result blocked
spark-cli history --access readOnly --from 2026-10-28
spark-cli history --access send
spark-cli history --verbose --json
```

- Work `--from` and `--to` out from today in the user's time zone.
- `--access readOnly` adds the reads (searches, threads, attachments);
  `--access send` keeps only sends and calendar changes.
- `--result error` or `blocked` shows what didn't go through.
- `--verbose` adds each entry's arguments and targets: the email IDs,
  addresses or events it acted on.

## 3. Explain it plainly

Say what happened ("archived 3 newsletters from Contoso"), not the
command. Look up the subject of a target the user won't recognize. Group
runs of the same thing, and name the agent when there's more than one.

```bash
spark-cli thread 1114
```

## 4. Explain what was blocked

A blocked entry is a command the user's settings didn't allow, and it
changed nothing. Its text names the account, the access the user gave
there and what the command needed: "can't archive in me@example.com: the
user allowed read there, and this needs triage". Say it worked as set,
and that the user can change the access in spark-rs > Settings > Agents
if they want agents to do that. Don't push them to raise it. An error
entry tried and failed (an unknown ID, a server that said no) and changed
nothing: commands check every target before changing any.

A command over several targets that ran can still have had some fail on
their own (the network, the server): its entry is a success, and its
result says so per target (`"failed": 1` in `--json`, a line each in the
text). Read those lines before telling the user everything went, and
check `spark-cli emails Scheduled` after a send.

## 5. Undo what the user asks

Only on the user's request, one entry at a time; if "undo that" could
mean more than one, ask which.

```bash
spark-cli history undo 7
```

Undo puts back actions (archive, move, pin, snooze, category and the
rest), contact actions (Priority, accept, block) and drafts an agent
wrote, which it deletes. It only puts an email back if nothing changed it
since: "Left alone: email 1120 changed since" means the user or a sync
moved it afterwards. Say so, and offer to fix it by hand if the user
still wants it back, such as with `action moveToInbox 1120`.

What can't be undone from here:

- **Sends.** Sent mail has gone. A scheduled send that hasn't left yet
  (`emails Scheduled`) comes back to Drafts with `action unschedule s12`,
  which needs send access.
- **Calendar changes.** The attendees were already told. Another `event`
  command can change it back, and tells them again: ask first.
- **Unsubscribes.** The list was told. The user subscribes again on the
  sender's site.

Say what the undo put back, with IDs; it goes in the history too.

## Report

```
Since yesterday, agents tried 5 things and were blocked once:
- 09:02 Claude archived 3 Contoso newsletters (entry 7, can undo).
- 09:04 Claude moved "Bracket quote" (1114) to Projects/Atlas (entry 8).
- 09:10 Claude wrote a reply draft to Ana Ruiz (draft 2001, entry 9, can undo).
- 09:12 Claude was blocked sending draft 2001: me@example.com allows triage.
- 09:20 Codex accepted bo@example.com at the Gatekeeper (entry 11, can undo).
Undone as you asked: entry 8. "Bracket quote" is back in the Inbox.
```

## Notes

- spark-rs > Settings > Agents shows the same list with Undo buttons.
- The history lives on this computer; another computer's spark-rs has
  its own. An entry marked "undone" was already put back.
- When the user asks what you did, answer from the history, your own
  entries included.
