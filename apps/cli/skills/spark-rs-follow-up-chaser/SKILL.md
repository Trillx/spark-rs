---
name: spark-rs-follow-up-chaser
description: >-
  Find email the user sent that nobody has answered, pick out what's worth a
  nudge, and draft short follow-ups inside the same conversations. Use when
  the user asks who owes them a reply, what's still open on their side, or
  to chase people.
metadata:
  version: {{version}}
  requires:
    skills:
      - use-spark-rs
    access: triage
---

# Chasing replies

Turn "I sent that and heard nothing" into a short list the user can act
on: what's waiting, how long it's been, and a ready nudge for each one
worth chasing.

**Before you start:** this builds on `use-spark-rs`; follow its "Working
safely" rules. Finding and reading needs read access, drafts need triage,
and sending them needs send.

## 1. Find what's waiting

`is:waiting` is email the user sent that nobody has written back to since.
Search every folder, not just Sent, so a conversation that was archived
still counts:

```bash
spark-cli search --filter "is:waiting newer_than:30d older_than:3d" --json
```

Start with that window: younger than three days is too soon to chase,
older than a month is usually dead. Widen it if the user asks ("anything
from this quarter"). One row is a conversation; its `id` is the user's
latest email in it.

## 2. Leave out what doesn't need an answer

Not everything the user sent expects a reply. Drop, without asking:

- Thank-yous, "sounds good", FYIs and announcements.
- Email to a list or to many people (eight or more recipients).
- Automated mail the user sent through a form or a service.
- Conversations where the user's email closes the matter.

Read the conversation when you can't tell from the subject:

```bash
spark-cli thread 1114
```

## 3. Rank what's left

Put first what has a deadline, money, or a decision the user is blocked
on; then requests to people outside the organization; then the rest.
Note how long each has waited.

## 4. Draft the nudges

Ask the user which to chase (or take "all of them"). For each, reply to the
user's own latest email, so the nudge lands in the same conversation:

```bash
spark-cli draft --reply-to 1114 --body $'Hi Ana,\n\nJust checking whether you had a chance to look at the quote. Could you let me know by Friday?'
```

- Keep it to two or three sentences: what was asked, and an easy next step
  or a date. No guilt, no "per my last email".
- Match the user's tone in the earlier message: their greeting, how formal
  they were, the language they wrote in.
- The account's signature is added below the body, so don't sign off.
- If the user's email went to several people, `--reply-all` keeps them in.

Drafts don't go anywhere. Send only when the user says so:

```bash
spark-cli action send 2001
spark-cli action send 2001 --date 2026-11-03T09:00
```

Scheduling for the recipient's next working morning reads better than a
nudge at 11 pm.

## 5. Set reminders on what can wait

For things worth chasing later rather than now, a reminder brings the
email back to the top of the Inbox if nobody answers by then:

```bash
spark-cli action changeReminder 1114 --date 2026-11-10
```

## Report

Lead with the decisions, then the drafts:

```
Waiting on 6 replies (3 worth chasing):
- Ana Ruiz, "Bracket quote" — 9 days, asked for a price by Friday. Draft 2001 ready.
- Bo Lind, "Contract review" — 6 days, you're blocked on it. Draft 2002 ready.
- Finance, "Invoice 12" — 15 days, unpaid. Draft 2003 ready.
Left alone: 3 thank-yous and announcements.
Reminders set: "Offsite venues" on 10 Nov.
```

## Notes

- `is:waiting` sees only what spark-rs has downloaded. Mail older than the
  account's download window won't show up.
- A reply that went to another address of the user's (an alias spark-rs
  doesn't know) can make a conversation look unanswered. If the thread
  shows the person did reply, say so and skip it.
- Never send a nudge without the user's yes, and never more than one to
  the same person in a week.
