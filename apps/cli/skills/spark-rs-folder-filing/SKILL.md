---
name: spark-rs-folder-filing
description: >-
  File conversations into the user's own folders or Gmail labels: learn the
  structure, see where each sender's mail went before, propose a mapping and
  move once the user agrees. Use when the user asks to file, sort or organize
  mail into folders, or "put these where they belong".
metadata:
  version: {{version}}
  requires:
    skills:
      - use-spark-rs
    access: triage
---

# Filing into folders

The user already has folders (or Gmail labels) and a habit of what goes
where. Learn the habit from their past filing, propose a mapping for the
mail at hand, and move it with their yes.

**Before you start:** this builds on `use-spark-rs`; follow its "Working
safely" rules. Learning the structure needs read access; moving needs
triage.

## 1. Learn the structure

```bash
spark-cli folders
spark-cli folders me@example.com
```

Each account's own folders and labels come with an identifier
(`me@example.com:Projects/Atlas`) and counts. `Parent/Child` marks nested
ones. Folders belong to one account: an email can only move to its own
account's folders.

## 2. Pick what to file

Take what the user names. Otherwise offer the Inbox's settled
conversations, such as mail from people with nothing new for a week:

```bash
spark-cli emails --filter "category:personal" --page-size 200 --json | jq '[.emails[] | {id, date, subject, unread}]'
```

Each row is a conversation dated by its newest email, and a move takes the
whole conversation. Keep the rows whose `.date` is over a week old (worked
out from now) and that aren't unread; an `older_than:` filter would bring
back a conversation with today's reply by its older email.

## 3. See where each sender's mail went before

```bash
spark-cli search --filter "from:ana@example.com" --page-size 200 --json | jq '.emails | group_by(.folder) | map({folder: .[0].folder, count: length})'
spark-cli search --filter "from:example.org" --page-size 200 --json | jq '.emails | group_by(.folder) | map({folder: .[0].folder, count: length})'
spark-cli thread 1114
```

The `.folder` of a sender's past mail shows the habit: if most of Ana's
mail is in `Projects/Atlas`, her new mail probably goes there too. A domain
(`from:example.org`) works for a whole client. `thread` shows `Labels:`,
the user's folders a conversation is in. When a sender's mail is spread
over several folders, look at the subjects: the project name usually
decides.

## 4. Propose a mapping

Show it grouped by folder: the folder, then each conversation's ID, sender
and subject, and why (sender's habit, project name in the subject). Put
the ones you couldn't place in a separate list for the user. Wait for a
yes, or changes.

## 5. Move

```bash
spark-cli action moveToFolder 1114 1120 1131 --folder "me@example.com:Projects/Atlas"
spark-cli action moveToFolder 1140 --folder Receipts
```

A bare folder name means that folder in the email's own account. The move
takes the conversation's emails in the same place along, out of the Inbox
and into the folder.

## 6. Say what changed

List what went where, with IDs, and point to `spark-cli history undo <ID>`
(`spark-cli history` lists the IDs).

## Report

```
Filed 14 conversations:
- Projects/Atlas (6): 1114, 1120, 1131 from Ana Ruiz; 1135, 1137, 1139 "Atlas" in the subject.
- Clients/Northwind (5): everything from example.org.
- Receipts (3): 1140, 1144, 1148.
Left for you (2): Lee Park "Quick question" (1150), "Offsite ideas" (1152), no clear folder.
No folder for "Hiring" yet: make one in spark-rs and I'll file 4 more there.
Undo with spark-cli history undo <ID>.
```

## Notes

- A move puts a conversation in one folder. spark-rs can't add a second
  label without moving, so a Gmail conversation filed under one label
  isn't also tagged with another.
- spark-cli can't make folders. When the right one doesn't exist, ask the
  user to create it in spark-rs (New Folder in the sidebar's folder list),
  then run `folders` again.
- Moves reach the server after a few seconds.
- Ask before moving more than five emails, and don't file mail the user
  still needs in the Inbox (unanswered or pinned).
