---
name: spark-rs-priority-tuning
description: >-
  Review who counts as a Priority sender: check recent Priority mail, propose
  people to add (those the user writes to most and answers quickly) and to take
  off, then apply the changes. Use when the user asks to fix Priority, "who
  should be Priority" or says important mail gets buried.
metadata:
  version: {{version}}
  requires:
    skills:
      - use-spark-rs
    access: triage
---

# Tuning Priority

Priority should hold the people whose mail the user acts on first, and
nobody else. Check who's there now, find who's missing, and change the
list with the user's yes.

**Before you start:** this builds on `use-spark-rs`; follow its "Working
safely" rules. Reviewing needs read access; changing Priority senders needs
triage in every account, because it counts for all of them.

## 1. Who's Priority now

```bash
spark-cli search --filter "category:priority newer_than:28d" --page-size 1000 --json | jq '.emails | group_by(.from_address) | map({sender: .[0].from_address, name: .[0].from_name, count: length, unread: (map(select(.unread)) | length)}) | sort_by(-.count)'
```

For each sender, note how much they sent and how much the user left
unread. Candidates to take off: automated senders, newsletters, people the
user no longer works with, and senders whose mail mostly stays unread.

## 2. Who's missing

```bash
spark-cli contacts --limit 30
spark-cli emails Sent --filter "to:bo@example.com newer_than:60d" --json | jq '.total'
spark-cli search --filter "from:bo@example.com is:unreplied newer_than:60d" --json | jq '.total'
```

`contacts` with no query lists the people the user writes to most. For the
top ones, count how often the user wrote to them and how much of their mail
is still unanswered: someone the user writes to often and leaves little
unanswered is a good candidate. Read one or two threads with them
(`spark-cli thread 1114`) to see how fast the user usually answers. Leave
out the user's own addresses, mailing lists and shared inboxes like
`team@` or `support@`.

## 3. Check each before changing

```bash
spark-cli contact bo@example.com
spark-cli contact digest@tools.example.com
```

`contact` shows whether they're a Priority sender already, their category,
and whether the Gatekeeper accepted or blocked them. Don't propose someone
who's blocked.

## 4. Propose

Show two short lists, each with a reason: add (who, how often the user
writes to them, how fast they answer) and take off (who, and why). Wait for
the user's yes; they know who matters better than the numbers do.

## 5. Apply

```bash
spark-cli contact-action markContactAsPrimary bo@example.com kim@example.org
spark-cli contact-action unmarkContactAsPrimary digest@tools.example.com
```

In spark-rs this changes their existing mail too: marking makes all of
their mail Priority, and unmarking takes Priority off all of it, including
conversations marked by hand.

For one conversation rather than everything from a sender:

```bash
spark-cli action markThreadAsPriority 1120
spark-cli action unmarkThreadAsPriority 1131
```

## 6. Say what changed

Name the senders added and removed and the conversations changed, and
point to `spark-cli history undo <ID>` (`spark-cli history` lists the IDs).

## Report

```
Priority over the last 4 weeks: 41 conversations from 9 senders.
Add (3): Bo Lind (you wrote 14 times, usually answer within the hour), Kim Ito (client, 9 emails, none left unanswered),
  Ana Ruiz (supplier, 7 emails, answered all the same day).
Take off (2): digest@tools.example.com (automated weekly digest, 100% unread), Lee Park (left the project in September).
Changed: made Priority bo@example.com, kim@example.org, ana@example.com; no longer Priority digest@tools.example.com.
Undo with spark-cli history undo <ID>.
```

## Notes

- spark-rs won't make one of the user's own addresses a Priority sender.
- With notifications set to Priority (Settings → Notifications), only
  Priority mail notifies, so a good list matters more there.
- Keep the list short. When everyone is Priority, nobody is.
