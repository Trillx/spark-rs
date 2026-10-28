---
name: spark-rs-calendar-audit
description: >-
  Measure the user's meeting load over a range: hours a day, busiest and
  lightest days, back-to-back runs, focus blocks, and meetings to decline
  or shorten. Changes nothing. Use when the user asks how busy their
  calendar is, "too many meetings", "where did my week go", or for a review.
metadata:
  version: {{version}}
  requires:
    skills:
      - use-spark-rs
    access: read
---

# Calendar audit

The user feels their calendar is full and wants numbers. They get hours
in meetings day by day, where the long runs and the free stretches are,
and a short list of meetings to question.

**Before you start:** this builds on `use-spark-rs`; follow its "Working
safely" rules. Everything here needs read access only, and nothing
changes.

## 1. Pick the range and the calendars

Ask which days; this week or the next two weeks is a good default. Work
the dates out from today in the user's time zone. `events` takes at most
three months a call.

```bash
spark-cli accounts
spark-cli events --start 2026-11-02 --end 2026-11-13 --in "me@example.com:Calendar" --json
```

`accounts` lists the calendars. Count the user's own; leave out holiday,
shared and other people's calendars, or their events count as the user's.

## 2. Keep what takes the user's time

Leave out events that are `.canceled`, that the user `declined`, or whose
`.show_as` is `free`. With jq (or filter the JSON yourself):

```bash
spark-cli events --start 2026-11-02 --end 2026-11-13 --in "me@example.com:Calendar" --json | jq '[.events[] | select((.canceled | not) and .response != "declined" and .show_as != "free")]'
```

Count all-day events apart: holidays, travel and out of office
(`.show_as` `oof`) are days, not hours. A day the user is out has no
meeting load; say so instead of calling it light. Keep `tentative` ones in,
and say how many hours are tentative.

## 3. Add up each day

Merge overlapping events before adding: two meetings at 14:00 to 15:00
are one busy hour, not two. Keep the overlaps as a finding (double-booked).
Per day, note the number of meetings, merged hours, the first start and
the last end.

## 4. Find back-to-back runs

Sort the merged blocks by start and join any two with less than five
minutes between them. A run of 90 minutes or more, or of three meetings or
more, is worth reporting.

## 5. Find the longest focus blocks

```bash
spark-cli availability --start 2026-11-02 --end 2026-11-13 --json
```

`.free[]` gives the open stretches with `.minutes`, between 08:00 and
20:00 on weekdays. Report the longest, and the days with none of an hour
or more. Days already past have none listed.

## 6. Look for meetings to question

For each, give the reason in a few words:

- Repeating meetings (`.repeats`) that add up to the most hours.
- Big meetings the user doesn't organize, where they may only listen.
- An hour booked for what reads like a 30-minute topic.
- Double-bookings, and tentative answers never settled.
- Meetings with no description, a sign there's no agenda.

## Report

```
2026-11-02 to 2026-11-13, own calendar, 10 working days:
Day         Meetings  Hours  Longest free
Mon 11-02   6         5.5    0:45
Tue 11-03   3         2.0    3:00
Wed 11-04   5         4.0    1:30
...
Total: 38 meetings, 27.5 h (2.8 h a day), 3.0 h of it tentative.
All-day: Wed 11-11 public holiday; Fri 11-13 out of office.
Busiest: Mon 11-02, 5.5 h. Lightest: Tue 11-03, 2.0 h.
Back-to-back: Mon 11-02 09:00 to 12:30 (4 meetings); Thu 11-05 13:00 to 15:00 (3).
Focus blocks: Tue 11-03 13:00 to 16:00; Wed 11-04 08:00 to 09:30.
Double-booked: Thu 11-05 14:00, "Vendor demo" and "Design sync".
To question:
- "Weekly status" (e42): 60 min, 14 people, you don't organize it. 5 h over the range.
- "Design sync" (e47): clashes with "Vendor demo"; tentative since 2026-10-20.
```

## Notes

- Change nothing. To answer an invitation use `spark-rs-invitations`; to
  move a meeting, `spark-rs-schedule-meeting`.
- Times are in the computer's time zone. A meeting that crosses midnight
  counts on both days.
- Only Microsoft 365 calendars are read. If the user also keeps a Google
  calendar, say its meetings aren't counted.
- `events` reads the range from the server first. If `.problems` says an
  account couldn't be read, the numbers are what spark-rs had; say so.
