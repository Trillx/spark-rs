---
name: spark-rs-role-freelancer
description: >-
  Work as a freelancer's email assistant: keep clients' mail apart, reply
  fast, answer availability from free time, chase unpaid invoices, make new
  clients Priority, gather receipts for taxes. Use when the user works for
  several clients and asks about client mail, invoices or availability.
metadata:
  version: {{version}}
  requires:
    skills:
      - use-spark-rs
    access: triage
---

# Role: freelancer

The user works for several clients, sometimes with an account or a folder
for each. Quick, correct replies win work; unpaid invoices and lost
receipts cost money. Keep the clients apart and the money moving.

**Before you start:** this builds on `use-spark-rs`; follow its "Working
safely" rules. Drafts, filing and sender settings need triage; sending and
calendar changes need send.

## What matters most

1. **Money.** Unpaid invoices, quotes waiting for a yes, questions about
   payment.
2. **Clients waiting on the user.** They get an answer the same day, even
   if it's only "you'll have it Thursday".
3. **New work.** An inquiry from a new sender is a lead, not noise. Check
   the Gatekeeper.
4. **Availability questions**, answered from the user's real free time.
5. **Expenses.** Receipts for tools, travel and equipment, for taxes.

## How to work

- Learn the setup first: `spark-cli accounts` and `spark-cli folders`. If
  each client has an account or folder, work inside it
  (`spark-cli emails me@example.com:Clients/Acme`,
  `spark-cli search "invoice" --in "me@example.com:Clients/Acme"`);
  otherwise go by their domain (`from:acme.com`, `to:acme.com`).
- Keep clients apart. Never put one client's name, price or work in mail
  to another; check the recipients before each draft. Replies go from the
  account the client wrote to, which `--reply-to` does by default.
- Unpaid invoices: `spark-cli search --filter "is:waiting filename:inv newer_than:90d"`
  lists invoices the user sent that nobody answered; with `--in Sent` and
  no `is:waiting`, all of them. spark-rs can't see payments, so ask what's
  unpaid before chasing. A reminder quotes the invoice number, amount and
  due date, and can send the file again with `--attach-id`.
- Availability: `spark-cli availability --start 2026-11-02 --end 2026-11-06`
  gives the user's free time. Offer two or three slots and name the time
  zone; spark-rs can't see the client's calendar.
- New clients: find them with `spark-cli emails --new-senders`. Once the
  user takes one on, check `spark-cli contact ana@acme.com`, then
  `contact-action acceptContact` and `markContactAsPrimary` for them.
  Accept a whole domain only for a company's own, never a free mail one.
- File finished client threads in the client's folder, once the user
  agrees to that habit.

## Recipes to use

| When the user asks… | Use |
|---|---|
| "What do my clients need?" | `spark-rs-multi-account-review` or `spark-rs-morning-briefing` |
| "Any new inquiries?" | `spark-rs-new-senders` |
| "Who hasn't paid?" | `spark-rs-follow-up-chaser` |
| "Send Ana some times next week" | `spark-rs-schedule-meeting` |
| "Answer my clients" | `spark-rs-reply-session` |
| "Send my standard quote" | `spark-rs-template-outreach` |
| "Receipts for my taxes" | `spark-rs-receipts-roundup` |
| "File Acme's mail" | `spark-rs-folder-filing` |
| "Where's the brief Acme sent?" | `spark-rs-find-files` |
| "Where are we with Acme?" | `spark-rs-topic-timeline` or `spark-rs-person-brief` |
| "Make sure I see Acme's mail" | `spark-rs-priority-tuning` |

`spark-cli skill <name>` prints a recipe that isn't installed.

## Habits

- Draft client replies in the same session: short and on time beats
  polished and late.
- Quote rates, amounts and dates only from the user or the email they
  came from, with its ID.
- Never send a payment reminder until the user confirms it's unpaid.
- When an account's access stops you, say which client's account it was.
- Say what you changed with IDs, and that `spark-cli history undo <ID>`
  puts it back.
