---
name: spark-rs-schedule-meeting
description: >-
  Find a time for a meeting: offer the attendees two or three of the user's
  free slots by email, then put the agreed one in the calendar with
  invitations and a Teams call. Use when the user asks to set up a meeting,
  find a time with someone, "when can we meet", or to send an invite.
metadata:
  version: {{version}}
  requires:
    skills:
      - use-spark-rs
    access: send
---

# Scheduling a meeting

The user wants to meet some people and doesn't want the back-and-forth.
They get a short email offering times that suit them, and, once the others
pick one, the event in their calendar with everyone invited.

**Before you start:** this builds on `use-spark-rs`; follow its "Working
safely" rules. Finding free time needs read access, the email proposing
times needs triage, and sending it and creating the event need send.

## 1. Get the details

Ask what you can't tell: who, how long, how soon, in person or a Teams
call, and where. When a thread asked for the meeting, read it:

```bash
spark-cli thread 1114
spark-cli contacts ana
```

Look in the thread for each attendee's time zone: a city in the signature,
"my afternoon", "I'm in New York". The dates spark-rs shows are in the
user's own zone, so they don't tell you where the sender is. If you can't
tell, ask the user or write the times in the user's zone only and say so.

## 2. Find the user's free time

```bash
spark-cli availability --start 2026-11-03 --end 2026-11-06 --json
```

`.free[]` lists the user's open stretches (`.start`, `.end`, `.minutes`)
between 08:00 and 20:00 on weekdays, around everything that isn't free,
declined or canceled. Keep the ones long enough for the meeting. Work out
the dates from today in the user's time zone; don't copy the examples.

spark-rs can't see other people's calendars, so you can't check theirs:
that's why the email offers options.

## 3. Pick two or three options

- On different days, so one bad day doesn't sink them all.
- Inside the attendees' working hours too, after converting.
- Not wedged between two meetings when a gap with room either side exists.
- Clocks change on different dates (Europe on the last Sunday of October,
  the US on the first Sunday of November): work each date out on its own.

## 4. Draft the proposal

Reply in the thread when there is one, or write to the attendees:

```bash
spark-cli draft --reply-all 1114 --body $'Hi Ana, hi Bo,\n\nCould one of these work for a 45-minute call?\n\n- Tue 2026-11-03, 15:00 to 15:45 CET (09:00 New York)\n- Wed 2026-11-04, 16:00 to 16:45 CET (10:00 New York)\n- Thu 2026-11-05, 14:30 to 15:15 CET (08:30 New York)\n\nOnce we agree on one, I will send an invitation.'
spark-cli draft --to "Ana Ruiz <ana@example.com>" --to bo@example.com --subject "Time for the Atlas review" --body $'Hi Ana, hi Bo,\n\nCould one of these work?'
```

Show it, and send it only on the user's yes:

```bash
spark-cli action send 2001
```

## 5. Create the event once they agree

When the answers come in, check the chosen slot is still free
(`availability` for that day). Then confirm with the user the title, start,
end, who's invited, Teams or a place, and which calendar, because creating
it emails the attendees:

```bash
spark-cli event create --title "Atlas review" --start 2026-11-04T16:00 --end 2026-11-04T16:45 --add ana@example.com --add bo@example.com --video-conference teams
spark-cli event create --title "Atlas review" --start 2026-11-04T16:00 --end 2026-11-04T16:45 --location "Room 7" --calendar me@example.com
```

To be careful, create it without `--add`, show the user the event, then
invite: `spark-cli event update e42 --add ana@example.com`.

## Report

```
Proposed to Ana Ruiz and Bo Lind (draft 2001, sent):
- Tue 2026-11-03 15:00 CET / 09:00 New York
- Wed 2026-11-04 16:00 CET / 10:00 New York
- Thu 2026-11-05 14:30 CET / 08:30 New York
Ana picked Wednesday. Created "Atlas review" (e42), Wed 2026-11-04 16:00 to 16:45, Teams, both invited.
```

## Notes

- `availability --attendees` doesn't read their calendars; it only adds a
  note. Don't present the user's free time as everyone's.
- Calendars are Microsoft 365's. With only Gmail accounts spark-rs can't
  create the event: send a confirmation email, and the user sends the
  invitation from Google Calendar.
- Video calls are Teams only. For Zoom or Meet, the user makes the link;
  you can put it in `--location` or `--description`.
- Events can't be undone from the history. `event update` and
  `event delete` fix mistakes, and both email the attendees.
