---
name: spark-rs-subscriptions-audit
description: >-
  Review the newsletters and marketing mail the user gets, with how much each
  sender sends and how much goes unread, then unsubscribe, keep or refile each
  with the user's approval. Use when the user asks to clean up subscriptions,
  unsubscribe from newsletters or cut down marketing mail.
metadata:
  version: {{version}}
  requires:
    skills:
      - use-spark-rs
    access: triage
---

# Subscriptions audit

A table of every list the user is on, with how much it sends and how much
of it they read, sorted into what to drop, what to keep and what to refile,
and then the unsubscribing done for the ones they approve.

**Before you start:** this builds on `use-spark-rs`; follow its "Working
safely" rules. The audit needs read access; unsubscribing, refiling,
blocking and archiving need triage.

## 1. Gather the senders

```bash
spark-cli search --filter "category:newsletter newer_than:90d" --page-size 1000 --json | jq '.emails | group_by(.from_address) | map({sender: .[0].from_address, name: .[0].from_name, emails: (map(.messages // 1) | add), ids: map(.id)}) | sort_by(-.emails)'
spark-cli search --filter "category:newsletter is:unread newer_than:90d" --page-size 1000 --json | jq '.emails | group_by(.from_address) | map({sender: .[0].from_address, unread: (map(.messages // 1) | add)})'
spark-cli search --filter "unsubscribe -category:newsletter newer_than:90d" --page-size 1000 --json
```

`search --filter` looks in the Inbox, Done, New Senders and the other
folders (not Trash, Spam or Blocked). The third line catches marketing
filed as People or Notifications: most of it says "unsubscribe" somewhere.
Rows are conversations, and a list that threads its issues can put many
emails in one, so count emails with `.messages // 1`, not rows. Mail per
30 days is the 90-day email count divided by three; the unread share is
the second search's count over the first's, not the representative row's
`.unread` flag.

## 2. Leave out transactional mail

Receipts, order and shipping updates, security codes, password resets,
account and billing notices and statements aren't subscriptions, even when
the same company also sends marketing. Tell them apart by the address
(`orders@shop.example` against `news@shop.example`) and the subject, and
leave them off the list.

## 3. Check for look-alikes

A brand's name from a domain that has nothing to do with it ("Northwind
Bank" from `northwind-offers.example.net`) is phishing, not a subscription.
Never unsubscribe from it: the link can be the trap, and it tells the
sender the address is read. Propose `action markAsSpam` for it instead, and
see `spark-rs-suspicious-email`. Never open links in these emails yourself.

## 4. Check for copies

Two rows that look like the same newsletter may be one list sent to two of
the user's addresses, or two lists. Before counting them as two
subscriptions, compare the To: lines (`spark-cli thread 1114`, then
`spark-cli thread 1131`). Different To: addresses mean each needs its own
unsubscribe.

## 5. Sort into tiers

- **Drop:** frequent and mostly unread (say, more than three-quarters
  unread), or the user says they never read it.
- **Keep:** read often enough to matter.
- **Keep but refile:** read sometimes, but filed as People or Notifications;
  moving the sender to Newsletters takes it out of the user's way.

Show the table (sender, per 30 days, unread share, tier) and wait for the
user's approval, row by row or as a whole.

## 6. Act on what's approved

```bash
spark-cli action unsubscribe 1114 1131 1140
spark-cli action changeCategoryNewsletters 1152
spark-cli action markAsSpam 1163
spark-cli search --filter "from:deals@shop.example.net newer_than:7d"
spark-cli contact-action blockContact deals@shop.example.net
spark-cli action archive 1101 1102 1103 1104 1105 1106
```

Pass one email per list. `unsubscribe` reports one of these outcomes:
unsubscribed through the list's link; an unsubscribe email sent from the
user's account to the list (only with send access: with triage it says the
list wants an email, and the user clicks Unsubscribe on it in spark-rs); or
only a web page, whose address you give the user to open themselves.
Unsubscribing can't be undone from the history.

Lists can take a few days to stop. A sender still mailing a week or two
later can be blocked, with a yes. If the user wants the old mail gone too,
list it in the Inbox with the whole-conversation fields, and archive only
rows that are all the list's mail; a conversation where a person also
wrote goes to the user to decide:

```bash
spark-cli emails --filter "from:news@deals.example.net" --page-size 200 --json | jq '[.emails[] | select((.messages // 1) == .email_count) | .id]'
```

Show how many and archive after a yes.

## Report

```
Subscriptions in the last 90 days: 23 senders.
Drop (9): Deals Weekly (26/month, 100% unread), Shop Daily (30/month, 97% unread), …
  Unsubscribed from 7, emailed 1 list. Gadget News only unsubscribes on its page:
  https://gadgets.example/unsubscribe (open it yourself).
Keep (8): Atlas Digest, The Morning Brief, …
Refiled as Newsletters (3): product updates from tools.example.com, …
Marked as spam (1): "Northwind Bank" offers from northwind-offers.example.net.
Left alone: receipts and account notices from 6 senders.
```

## Notes

- Ask before every unsubscribe batch: with send access it may send an
  email from the user's account, and it can't be taken back here.
- `changeCategoryNewsletters` refiles all of that sender's mail, now and
  later; `spark-cli history undo <ID>` puts it back.
