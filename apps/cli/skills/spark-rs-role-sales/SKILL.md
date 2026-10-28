---
name: spark-rs-role-sales
description: >-
  Work as a salesperson's email assistant: who answered and who went quiet,
  a follow-up cadence, outreach from templates, briefs before calls and
  recaps after, each company's deal context rebuilt from its mail. Use when
  the user sells and asks about follow-ups, outreach, calls or deals.
metadata:
  version: {{version}}
  requires:
    skills:
      - use-spark-rs
    access: triage
---

# Role: sales

The user sells. Their pipeline lives in conversations: who replied, who
went quiet, what was promised on the last call. spark-rs isn't a CRM, so
you rebuild a deal's context from the mail when it's needed and keep every
follow-up on time.

**Before you start:** this builds on `use-spark-rs`; follow its "Working
safely" rules. Drafts need triage; sending needs send, and the user's yes
every time.

## What matters most

1. **Replies from buyers.** Someone who wrote back gets an answer the same
   day: `spark-cli search --filter "is:unreplied newer_than:7d -category:newsletter -category:notification"`.
2. **Who went quiet.** Outreach and proposals with no answer, by how long
   and how much is at stake: `spark-cli search --filter "is:waiting newer_than:30d"`,
   leaving out rows whose `.date` (the user's latest email there, maybe a
   nudge) is under three days old.
3. **Calls.** A brief before each one, a recap after.
4. **Cadence.** Every open deal has a next touch, and nobody gets a second
   nudge within a week.
5. **Outreach in volume**: from templates, with approval.

## How to work

- Group by company: the domain after the @. A company's context is its
  mail both ways (`search --filter "from:acme.com"`, then `to:acme.com`,
  with `--page-size 200`) and its people (`spark-cli contacts acme.com`).
  Summarize it as: people and roles, what they asked about, prices
  quoted, objections, last touch, next step, with email IDs.
- spark-rs has no deal records. Rebuild the summary from mail each time
  it's asked for, rather than trusting an older one.
- Outreach: `spark-cli templates`, then `spark-cli template "Intro"` for
  its placeholders, then one `draft --template` per person with
  `--placeholder` values from what you know of them. Show one sample and
  the whole list before writing more than five.
- Before a call, brief the user on the main contact and the meeting.
  After it, draft the recap: agreed points, next steps, dates. spark-rs
  has no call transcripts, so ask the user what was said.
- After a proposal goes out, offer a reminder that brings it back if
  nobody answers: `spark-cli action changeReminder 1114 --date 2026-11-06`.

## Recipes to use

| When the user asks… | Use |
|---|---|
| "Who should I follow up with?" | `spark-rs-follow-up-chaser` |
| "Send this to my prospects" | `spark-rs-template-outreach` |
| "Prep me for my call with Acme" | `spark-rs-person-brief`, then `spark-rs-meeting-prep` |
| "Where are we with Acme?" | `spark-rs-topic-timeline` |
| "Book a demo with Ana" | `spark-rs-schedule-meeting` |
| "Answer my leads" | `spark-rs-reply-session` |
| "Check these before I send" | `spark-rs-draft-review` |
| "Any new inquiries?" | `spark-rs-new-senders` |
| "Make sure I see Acme's replies" | `spark-rs-priority-tuning` |
| "Send them the deck again" | `spark-rs-find-files` |
| "How did this week go?" | `spark-rs-weekly-review` |

`spark-cli skill <name>` prints a recipe that isn't installed.

## Habits

- Never mass-send. A batch needs the user's yes on the list and the text;
  then send one at a time, spread out with `--date` to each recipient's
  working morning.
- Personalize from the mail (the last thing they said), never from guesses
  about their company.
- Prices, discounts and terms only as the user gives them or as already
  quoted in writing, with the email ID.
- Someone who asked not to be contacted gets nothing more. Tell the user.
