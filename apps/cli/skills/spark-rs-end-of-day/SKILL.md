---
name: spark-rs-end-of-day
description: >-
  Find the loose ends before the user stops: unanswered mail from people,
  finished pins, today's drafts, scheduled sends and tomorrow's meetings, then
  offer to snooze the rest. Use when the user asks to wrap up, "what's left",
  "before I log off" or for an end-of-day check.
metadata:
  version: {{version}}
  requires:
    skills:
      - use-spark-rs
    access: read
---

# End of day

A short list of what's still open before the user stops: who's waiting on
them, what's half-done, what goes out later, and what tomorrow needs. With
a yes, move the rest out of sight until the morning.

**Before you start:** this builds on `use-spark-rs`; follow its "Working
safely" rules. Looking needs only read access; unpinning and snoozing need
triage, and unscheduling a send needs send.

Work out today's and tomorrow's dates in the user's time zone; the examples
use Tuesday 2026-11-03.

## 1. Mail from people still unanswered today

```bash
spark-cli search --filter "is:unreplied category:priority after:2026/11/03" --json
spark-cli search --filter "is:unreplied category:personal -category:priority after:2026/11/03" --json
```

Use `is:unreplied`, not `is:unread`: mail the user read and meant to answer
counts too, and `search` finds it even if it was archived. Leave out what
needs no answer (thanks, FYIs, a long Cc list). For each one left: who,
the subject, what they asked, and whether it can wait until tomorrow.

## 2. Pins that look finished

```bash
spark-cli emails Pinned
spark-cli thread 1098
spark-cli action unpin 1098 1102
```

A pin looks finished when the last email is the user's and closes the
matter, the other side said it's all set, or the date it was about has
passed. List those and offer to unpin them; unpin only after a yes.

## 3. Drafts and scheduled sends

```bash
spark-cli emails Drafts --filter "after:2026/11/03"
spark-cli emails Scheduled
```

Name each draft from today with its subject and its first words, so the
user can finish or drop it. Never delete a draft without a yes: drafts
have no Trash. For scheduled sends (`s12` IDs), say what goes when and to
whom; flag one that failed or goes out at an odd hour. With a yes,
`spark-cli action unschedule s12` takes one back to Drafts.

## 4. Tomorrow

```bash
spark-cli events --tomorrow --json
```

Give the first meeting's time, meetings with outside people (an attendee at
a domain none of the user's accounts has), events not yet answered
(`.response` is `none`), and what each needs first: a document to read, a
reply to send. `spark-rs-meeting-prep` goes deeper on one meeting.

## 5. Offer to clear the decks

Ask whether to snooze what's left until tomorrow morning, before the first
meeting. Show the list (IDs, senders, subjects) and wait for a yes:

```bash
spark-cli action snooze 1120 1131 --date 2026-11-04T08:30
```

For the user's own sent email that's waiting on someone, a reminder fits
better: it comes back only if nobody answers.

```bash
spark-cli action changeReminder 1140 --date 2026-11-04T08:30
```

Afterwards say what moved, with the IDs, and that
`spark-cli history undo <ID>` puts it back.

## Report

```
Before you stop (Tuesday 3 November)

Still unanswered from today: 3
- Ana Ruiz, "Bracket quote" (Priority): wants a price by Thursday. 1114
- Bo Lind, "Contract review": two questions at 16:10. 1120
- Kim Ito, "Offsite dates": can wait. 1131
Pinned and finished? "Venue booking" (1098): you confirmed, they thanked you. Unpin?
Drafts from today: "Re: Atlas budget" (2001) to Ana, half-written.
Sending later: s12 "Weekly update" to the team, tomorrow 08:00.
Tomorrow: 3 meetings, the first at 09:00. Atlas review with example.org needs the quote (1114).

Snooze 1120 and 1131 until 08:30 tomorrow?
```

## Notes

- `after:2026/11/03` counts from midnight in the user's time zone;
  `newer_than:1d` would reach back into yesterday evening.
- If tomorrow is a weekend day or the user is off, snooze to their next
  working morning instead, and ask which day that is when you don't know.
