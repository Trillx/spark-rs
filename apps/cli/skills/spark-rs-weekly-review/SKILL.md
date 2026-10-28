---
name: spark-rs-weekly-review
description: >-
  Look back on the user's week and plan the next one: meetings and hours,
  mail volume by category, open loops, what agents did, and next week's
  calendar with its free blocks. Use when the user asks for a weekly review,
  "how did my week go", "what's still open" or to plan next week.
metadata:
  version: {{version}}
  requires:
    skills:
      - use-spark-rs
    access: read
---

# Weekly review

A look back and a look ahead on one page: where the week's time and mail
went, what's still open, and what next week holds, with room left for
focused work.

**Before you start:** this builds on `use-spark-rs`; follow its "Working
safely" rules. The review needs only read access; tidying up afterwards
(unpinning, archiving, snoozing) needs triage.

Work out the dates from today in the user's time zone; the examples use
Friday 2026-11-06, so this week is 2026-11-02 to 2026-11-06 and next week
2026-11-09 to 2026-11-13.

## 1. This week's meetings

```bash
spark-cli events --start 2026-11-02 --end 2026-11-06 --json
```

Count the meetings, leaving out `.canceled`, `.all_day`, declined ones
(`.response` `declined`) and those shown as free (`.show_as` `free`). Add
up end minus start for the hours. Note the busiest day, and how many had
outside people (at a domain none of the user's accounts has).

## 2. Mail volume by category

```bash
spark-cli search --filter "category:priority after:2026/11/02" --json | jq '.total'
spark-cli search --filter "category:personal after:2026/11/02" --json | jq '.total'
spark-cli search --filter "category:invitation after:2026/11/02" --json | jq '.total'
spark-cli search --filter "category:notification after:2026/11/02" --json | jq '.total'
spark-cli search --filter "category:newsletter after:2026/11/02" --json | jq '.total'
spark-cli emails Sent --filter "after:2026/11/02" --json | jq '.total'
```

`.total` counts conversations, not single emails. Priority is a mark on
top of a category, so it overlaps People. For trends, run the same with
`after:2026/10/26 before:2026/11/02`.

## 3. Open loops

```bash
spark-cli search --filter "is:unreplied category:personal newer_than:14d" --json
spark-cli search --filter "is:waiting newer_than:14d" --json
spark-cli emails "Set Aside" --json
spark-cli emails Pinned --json
spark-cli emails Snoozed --json
```

Count each pile. Leave out unreplied mail that needs no answer; for Set
Aside and Pinned give the oldest and any that look finished; for Snoozed,
what comes back next week (`.snoozed_until`), by day.

## 4. What agents did

```bash
spark-cli history --from 2026-11-02
spark-cli history --from 2026-11-02 --result blocked
```

Sum it up by kind (emails archived, drafts written, senders blocked) and
name anything blocked or failed. `spark-rs-agent-activity` goes into detail.

## 5. Next week

```bash
spark-cli events --start 2026-11-09 --end 2026-11-13 --json
spark-cli availability --start 2026-11-09 --end 2026-11-13 --json
```

Per day: meetings, hours booked, events not yet answered (`.response`
`none`) and meetings with outside people that need prep. List free blocks
of 90 minutes or more (`.minutes`, weekdays 08:00 to 20:00) for focused work.

## 6. Tidy up, if the user wants

Offer to unpin finished pins, archive finished Set Aside mail, and snooze
open loops to Monday morning. Show the list and wait for a yes:

```bash
spark-cli action unpin 1098
spark-cli action archive 1077 1081
spark-cli action snooze 1120 1131 --date 2026-11-09T09:00
```

Then say what changed, with IDs, and point to `spark-cli history undo <ID>`.

## Report

```
Week of 2 November
Meetings: 14, 11.5 hours; Wednesday was the heaviest (4.5 h). 5 with outside people.
Mail: 23 Priority, 61 People, 4 invitations, 140 notifications, 38 newsletters; you sent 47.
Open: 5 unreplied, waiting on 7 replies, 9 set aside (oldest 3 weeks), 4 pinned (2 look finished).
Back next week: 3 snoozed on Monday, 1 on Wednesday.
Agents: archived 62 emails, wrote 9 drafts, blocked 2 senders; 1 blocked command.
Next week: 12 meetings, 9 hours. Free blocks: Mon 13:00–17:00, Thu 08:00–11:30.
Unanswered: "Q4 planning" on Tuesday (e57).
```

## Notes

- These searches count the user's own sent conversations too. For received
  mail only, add `-from:me@example.com` for each of the user's addresses.
