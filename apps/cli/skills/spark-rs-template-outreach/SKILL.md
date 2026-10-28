---
name: spark-rs-template-outreach
description: >-
  Send the same email to a list of people from one of the user's templates:
  one draft per person with their values, a sample approved first, sends
  spread out. Use when the user asks to email a list of people, send a
  template to several clients, do outreach, or "send this to everyone".
metadata:
  version: {{version}}
  requires:
    skills:
      - use-spark-rs
    access: triage
---

# Template outreach

The user has a message they send often and a list of people to send it
to. They get one personal email per person, written from their template,
checked on a sample before the rest are written.

**Before you start:** this builds on `use-spark-rs`; follow its "Working
safely" rules. Reading templates and contacts needs read access, drafts
need triage, and sending or scheduling them needs send.

## 1. Pick the template

```bash
spark-cli templates
spark-cli template "Quote ready"
```

Show the user its subject, text and placeholders. `[auto]` ones fill in
from the recipient's and the user's names; each `[manual]` one needs a
value for every person.

## 2. Collect the people and their values

```bash
spark-cli contacts ana
spark-cli contacts example.com
```

Resolve the names the user gives to addresses (ask when two match), then
build a table, one row a person: name, address, and a value for each
manual placeholder. Ask for what's missing and confirm the table.

## 3. Write one sample

```bash
spark-cli draft --template "Quote ready" --to "Ana Ruiz <ana@example.com>" --placeholder "Project=Bracket" --placeholder "Price=EUR 1,240"
```

- Pass the name with the address (`"Ana Ruiz <ana@example.com>"`): the
  auto placeholders take the recipient's name from `--to`. A bare address
  leaves them empty, and the draft can't be sent until they're filled.
- Placeholder names match the template's, ignoring case.
- `--subject` replaces the template's subject. `--body` replaces its whole
  text, placeholders and all, so don't pass it with `--template`.

Show the sample as `draft` printed it and wait for a yes. If the template
itself needs changing, the user edits it in spark-rs; then delete the
sample (`draft --delete 2001`, after a yes) and start again.

## 4. Write the rest

One draft per person, each with only that person on To:

```bash
spark-cli draft --template "Quote ready" --to "Bo Lind <bo@example.com>" --placeholder "Project=Shelving" --placeholder "Price=EUR 860"
spark-cli draft --template "Quote ready" --to "Chen Wu <chen@example.net>" --placeholder "Project=Counter" --placeholder "Price=EUR 2,100"
```

Read each reply for a "still to fill in" warning. To add a line for one
person, `draft --edit 2003 --body …` with the whole text as it printed,
changed, and without the signature: the draft keeps its own.

## 5. Send or schedule after a yes

List the drafts (ID, recipient, subject) and ask. Once the user says yes,
spread the sends five to ten minutes apart, in the recipients' working
hours:

```bash
spark-cli action send 2001 --date 2026-11-03T09:00
spark-cli action send 2002 --date 2026-11-03T09:07
spark-cli action send 2003 --date 2026-11-03T09:15
spark-cli emails Scheduled
```

A burst of near-identical email looks like bulk mail to spam filters. Work
out the dates from today in the user's time zone; don't copy these.

## 6. Chase later

After a few days, `spark-rs-follow-up-chaser` finds who hasn't answered:

```bash
spark-cli search --filter 'is:waiting subject:"Your quote"' --json
```

## Report

```
"Quote ready" to 3 people:
- Ana Ruiz (draft 2001): scheduled 2026-11-03 09:00, s12
- Bo Lind (2002): scheduled 09:07, s13
- Chen Wu (2003): scheduled 09:15, s14
Not written: Dan Ek, no price given yet.
action unschedule <s ID> takes one back; spark-cli history undo <ID> deletes an unsent draft.
```

## Notes

- Never put the whole list on To or Cc: everyone would see everyone's
  address, and the placeholders fill in for the first person only.
- For more than a few dozen people, say a mailing tool fits better:
  providers limit how much one account may send in a day.
- Placeholders fill in the text, not the subject; pass `--subject` when it
  should name the person or project.
