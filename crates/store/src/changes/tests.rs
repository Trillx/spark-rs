use std::collections::{HashMap, HashSet};

use super::*;
use crate::{
    AccountKind, Added, Attachment, Category, Change, Decision, DraftFile, DraftSave, FolderRole,
    GatekeeperMode, Incoming, MessageState, NewAccount, NewReminder, OutboxFile, Patch,
    RemoteFolder, Screen,
};

/// The UI's copy of the message list: what it read, and its mark.
struct UiCopy {
    messages: Vec<Message>,
    mark: Mark,
}

impl UiCopy {
    fn read(store: &Store) -> Self {
        let (messages, mark) = store.messages_marked().unwrap();
        Self {
            messages,
            mark: mark.expect("outside a transaction"),
        }
    }

    /// Brings the copy up to date from the changes alone, as the UI does,
    /// checks that it's what reading every message gives, and returns how
    /// many messages the changes named.
    #[track_caller]
    fn follow(&mut self, store: &Store, what: &str) -> usize {
        let changes = store
            .message_changes(&self.mark)
            .unwrap()
            .unwrap_or_else(|| panic!("{what}: the changes can't be told"));
        let named: HashSet<i64> = changes
            .messages
            .iter()
            .map(|m| m.id)
            .chain(changes.gone.iter().copied())
            .collect();
        assert_eq!(
            named.len(),
            changes.messages.len() + changes.gone.len(),
            "{what}: a message named twice"
        );
        let mut merged: Vec<Message> = self
            .messages
            .iter()
            .filter(|m| !named.contains(&m.id))
            .chain(&changes.messages)
            .cloned()
            .collect();
        merged.sort_by(|a, b| b.date.cmp(&a.date).then(a.id.cmp(&b.id)));
        assert_eq!(merged, store.messages().unwrap(), "{what}");
        let kept: i64 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM temp.message_log WHERE seq <= ?1",
                [self.mark.seq],
                |r| r.get(0),
            )
            .unwrap();
        assert!(kept <= 1, "{what}: the log keeps what was read");
        self.messages = merged;
        self.mark = changes.mark;
        named.len()
    }
}

const ME: &str = "me@fastmail.com";

fn imap_store() -> (Store, i64) {
    let store = Store::open_in_memory().unwrap();
    let account = store
        .add_account(&NewAccount {
            kind: AccountKind::Imap,
            email: ME,
            name: "Me",
            label: "Personal",
            config: "{}",
        })
        .unwrap();
    (store, account.id)
}

fn mail(account_id: i64, uid: u32, from: &str) -> Incoming {
    Incoming {
        account_id,
        mailbox: Some("INBOX".into()),
        uid: Some(uid),
        message_id: Some(format!("<{uid}@example.com>")),
        from_name: "Sender".into(),
        from_address: from.into(),
        to: vec![format!("Me <{ME}>")],
        subject: format!("Message {uid}"),
        snippet: format!("About {uid}"),
        date: 1_000 + uid as i64,
        unread: true,
        category: Category::People,
        ..Default::default()
    }
}

fn id_of(added: Added) -> i64 {
    match added {
        Added::New(id) | Added::Known(id) => id,
    }
}

fn folder(store: &Store, remote_id: &str) -> i64 {
    store
        .folders()
        .unwrap()
        .into_iter()
        .find(|f| f.remote_id == remote_id)
        .unwrap()
        .id
}

fn remote(remote_id: &str, role: Option<FolderRole>) -> RemoteFolder {
    RemoteFolder {
        remote_id: remote_id.into(),
        parent: None,
        name: remote_id.into(),
        role,
        selectable: true,
    }
}

fn patch(f: impl FnOnce(&mut Patch)) -> Patch {
    let mut p = Patch::default();
    f(&mut p);
    p
}

fn save_draft(store: &mut Store, account_id: i64, existing: Option<i64>, body: &str) -> i64 {
    let to = ["Ana Ruiz <ana@x.com>".to_owned()];
    let file = OutboxFile {
        name: "plan.pdf".into(),
        mime: "application/pdf".into(),
        data: body.as_bytes().to_vec(),
    };
    let attachments = [Attachment {
        name: "plan.pdf".into(),
        mime: "application/pdf".into(),
        size: body.len() as u64,
        remote_id: None,
    }];
    store
        .save_draft(
            existing,
            &DraftSave {
                account_id,
                draft: &format!("{{\"body\":\"{body}\"}}"),
                send_later: None,
                from_name: "Me",
                from_address: ME,
                to: &to,
                cc: &[],
                bcc: &[],
                subject: "Plan",
                snippet: body,
                text: body,
                html: None,
                in_reply_to: None,
                references: None,
                attachments: &attachments,
                files: &[DraftFile::Copy(&file)],
            },
            5_000,
        )
        .unwrap()
}

#[test]
fn every_kind_of_change_follows_from_the_log() {
    let (mut store, account) = imap_store();
    store
        .replace_folders(
            account,
            &[
                remote("Travel", None),
                remote("Receipts", None),
                remote("Archive", Some(FolderRole::Archive)),
                remote("Trash", Some(FolderRole::Trash)),
                remote("Drafts", Some(FolderRole::Drafts)),
                remote("Sent", Some(FolderRole::Sent)),
            ],
        )
        .unwrap();
    let travel = folder(&store, "Travel");
    let receipts = folder(&store, "Receipts");
    let trash = folder(&store, "Trash");
    let mut copy = UiCopy::read(&store);
    assert!(copy.messages.is_empty());
    assert_eq!(copy.follow(&store, "nothing yet"), 0);

    // New mail, the first sync's and then a chunk of it.
    let mut ids = Vec::new();
    for uid in 1..=3 {
        ids.push(id_of(
            store
                .add_incoming(&mail(account, uid, "ana@x.com"), false)
                .unwrap(),
        ));
    }
    copy.follow(&store, "first sync");
    let chunk = store
        .batch(|b| {
            (4..=6)
                .map(|uid| {
                    b.add_incoming(&mail(account, uid, "bo@y.com"), false)
                        .map(id_of)
                })
                .collect::<Result<Vec<_>>>()
        })
        .unwrap();
    assert_eq!(copy.follow(&store, "a chunk"), 3);
    let (a, b, c) = (ids[0], ids[1], ids[2]);

    // Flags, the user's and the server's.
    store
        .update(a, &patch(|p| p.unread = Some(false)), 10)
        .unwrap();
    assert_eq!(copy.follow(&store, "read"), 1);
    store
        .update(a, &patch(|p| p.pinned = Some(true)), 10)
        .unwrap();
    assert_eq!(copy.follow(&store, "pinned"), 1);
    store
        .update(a, &patch(|p| p.priority = Some(true)), 10)
        .unwrap();
    assert_eq!(copy.follow(&store, "priority"), 1);
    store
        .observe_flags(account, "INBOX", 2, false, true)
        .unwrap();
    assert_eq!(copy.follow(&store, "server flags"), 1);
    store
        .observe_flags(account, "INBOX", 2, false, true)
        .unwrap();
    store
        .update(a, &patch(|p| p.unread = Some(false)), 10)
        .unwrap();
    assert_eq!(copy.follow(&store, "nothing new"), 0);

    // Done, Snooze and back, filed in a folder.
    store
        .update(b, &patch(|p| p.state = Some(MessageState::Done)), 10)
        .unwrap();
    copy.follow(&store, "done");
    store
        .update(
            c,
            &patch(|p| {
                p.state = Some(MessageState::Snoozed);
                p.snoozed_until = Some(50);
            }),
            10,
        )
        .unwrap();
    copy.follow(&store, "snoozed");
    assert_eq!(store.wake_snoozed(100).unwrap(), [c]);
    assert_eq!(copy.follow(&store, "back from snooze"), 1);
    store
        .update(b, &patch(|p| p.folder = Some(travel)), 10)
        .unwrap();
    copy.follow(&store, "filed");

    // Folder membership, as a folder sync finds it.
    let receipt = Incoming {
        mailbox: Some("Receipts".into()),
        ..mail(account, 40, "shop@z.com")
    };
    let receipt = id_of(store.add_folder_message(receipts, &receipt).unwrap());
    store
        .add_folder_message(
            receipts,
            &Incoming {
                mailbox: Some("Receipts".into()),
                uid: Some(41),
                ..mail(account, 4, "bo@y.com")
            },
        )
        .unwrap();
    copy.follow(&store, "in a folder");
    store
        .set_folder_members(receipts, &[(receipt, Some(40))])
        .unwrap();
    copy.follow(&store, "out of a folder");

    // A reply that joins two conversations.
    let joining = Incoming {
        in_reply_to: Some("<1@example.com>".into()),
        references: Some("<2@example.com>".into()),
        subject: "Re: Message 1".into(),
        ..mail(account, 7, "cy@w.com")
    };
    let first = store.message(a).unwrap().unwrap().conversation;
    let second = store.message(b).unwrap().unwrap().conversation;
    store.add_incoming(&joining, false).unwrap();
    copy.follow(&store, "re-threaded");
    let merged = store.message(a).unwrap().unwrap().conversation;
    assert!(merged == first || merged == second);

    // Muted, as a whole conversation.
    store.set_muted(merged, true).unwrap();
    assert!(copy.follow(&store, "muted") >= 2);
    store.set_muted(merged, false).unwrap();
    copy.follow(&store, "unmuted");

    // The Gatekeeper.
    let waiting = id_of(
        store
            .add_incoming(&mail(account, 8, "new@v.com"), true)
            .unwrap(),
    );
    copy.follow(&store, "new sender");
    assert!(store.message(waiting).unwrap().unwrap().screening);
    let decided = |store: &mut Store, address: &str, decision| {
        store
            .decide(
                &[Change {
                    screen: Screen::Sender(address.into()),
                    decision,
                }],
                20,
            )
            .unwrap()
    };
    decided(&mut store, "new@v.com", Some(Decision::Blocked));
    copy.follow(&store, "blocked");
    let undo = decided(&mut store, "new@v.com", Some(Decision::Accepted));
    copy.follow(&store, "accepted");
    store.decide(&undo, 20).unwrap();
    copy.follow(&store, "undone");
    store
        .add_incoming(&mail(account, 9, "other@u.com"), true)
        .unwrap();
    store.set_gatekeeper_mode(GatekeeperMode::Off).unwrap();
    copy.follow(&store, "gatekeeper off");

    // Senders' category and Priority.
    store
        .set_sender_category("bo@y.com", Some(Category::Newsletters))
        .unwrap();
    assert_eq!(copy.follow(&store, "category"), 3);
    let undo = store
        .prioritize_senders(&["bo@y.com".into()], true)
        .unwrap();
    copy.follow(&store, "priority sender");
    store.undo_prioritize(&undo).unwrap();
    copy.follow(&store, "priority undone");

    // Drafts: saved, saved again, sent with Undo, scheduled.
    let draft = save_draft(&mut store, account, None, "First");
    assert_eq!(copy.follow(&store, "draft saved"), 1);
    assert_eq!(
        save_draft(&mut store, account, Some(draft), "Second"),
        draft
    );
    assert_eq!(copy.follow(&store, "draft saved again"), 1);
    let queued = store.queue_send(account, "{}", 6_000, &[]).unwrap();
    store.hold_draft_for_send(queued, draft).unwrap();
    assert_eq!(copy.follow(&store, "draft sending"), 1);
    assert!(copy.messages.iter().all(|m| m.id != draft));
    assert!(store.cancel_send(queued).unwrap());
    copy.follow(&store, "send undone");
    assert!(copy.messages.iter().any(|m| m.id == draft));
    let queued = store.queue_send(account, "{}", 6_000, &[]).unwrap();
    store.hold_draft_for_send(queued, draft).unwrap();
    store.claim_due_sends(account, 6_000).unwrap();
    copy.follow(&store, "draft sending again");
    store.discard_draft(draft).unwrap();
    store.finish_send(queued).unwrap();
    copy.follow(&store, "draft sent");
    let later = store.queue_send(account, "{}", 9_000, &[]).unwrap();
    store.reschedule_send(later, Some(10_000), 0).unwrap();
    assert_eq!(copy.follow(&store, "scheduled"), 0);

    // A reminder on sent mail: due, done, undone, moved, removed.
    let sent = Incoming {
        mailbox: Some("Sent".into()),
        from_address: ME.into(),
        ..mail(account, 30, ME)
    };
    let sent = id_of(store.add_incoming(&sent, false).unwrap());
    copy.follow(&store, "sent mail");
    let reminder = store
        .add_reminder(&NewReminder {
            account_id: account,
            message_id: Some("<30@example.com>"),
            reply_to: None,
            subject: "Message 30",
            to: "Ana <ana@x.com>",
            sent_at: 1_000,
            remind_at: 5_000,
            if_no_reply: false,
        })
        .unwrap();
    assert_eq!(copy.follow(&store, "reminder set"), 0);
    let due = store
        .wake_reminders(5_000, &HashMap::from([(account, 5_000)]))
        .unwrap();
    assert_eq!(due[0].message, Some(sent));
    assert_eq!(copy.follow(&store, "reminder due"), 1);
    store.finish_reminder(reminder, true).unwrap();
    copy.follow(&store, "reminder done");
    store.finish_reminder(reminder, false).unwrap();
    copy.follow(&store, "reminder undone");
    store.set_reminder(reminder, Some(9_000)).unwrap();
    copy.follow(&store, "reminder moved");
    store.set_reminder(reminder, None).unwrap();
    copy.follow(&store, "reminder removed");

    // Trash, deleted for good, gone from the server's inbox, emptied.
    store
        .update(
            chunk[0],
            &patch(|p| p.state = Some(MessageState::Deleted)),
            30,
        )
        .unwrap();
    copy.follow(&store, "trashed");
    store.remove_messages(&[chunk[0]]).unwrap();
    assert_eq!(copy.follow(&store, "deleted forever"), 1);
    store.observe_gone_uids(account, "INBOX", &[5]).unwrap();
    copy.follow(&store, "expunged");
    let binned = Incoming {
        mailbox: Some("Trash".into()),
        ..mail(account, 50, "old@t.com")
    };
    store.add_folder_message(trash, &binned).unwrap();
    copy.follow(&store, "in the trash");
    assert_eq!(store.forget_emptied(trash).unwrap(), 1);
    copy.follow(&store, "trash emptied");

    // A folder gone from the server, with the mail filed in it.
    store
        .replace_folders(
            account,
            &[
                remote("Receipts", None),
                remote("Archive", Some(FolderRole::Archive)),
                remote("Trash", Some(FolderRole::Trash)),
                remote("Drafts", Some(FolderRole::Drafts)),
                remote("Sent", Some(FolderRole::Sent)),
            ],
        )
        .unwrap();
    copy.follow(&store, "folder removed");
    store.remove_folder(receipts).unwrap();
    copy.follow(&store, "another folder removed");

    // Reaching back: older mail lands below what's listed.
    store
        .batch(|b| {
            for uid in 60..63 {
                let older = Incoming {
                    date: uid as i64,
                    ..mail(account, uid, "ana@x.com")
                };
                b.add_incoming(&older, false)?;
            }
            Ok(())
        })
        .unwrap();
    store.set_sync_state(account, r#"{"floor":1}"#).unwrap();
    assert_eq!(copy.follow(&store, "reached back"), 3);

    // Headers refreshed; an answer to an invite, which the list doesn't show.
    let headers = Incoming {
        reply_to: Some(vec!["Help <help@x.com>".into()]),
        ..mail(account, 3, "ana@x.com")
    };
    assert!(store.observe_reply_to(&headers).unwrap());
    assert_eq!(copy.follow(&store, "reply-to"), 1);
    store.set_rsvp(c, "accepted").unwrap();
    assert_eq!(copy.follow(&store, "rsvp"), 0);

    // The newest message deleted, and its id given to the next.
    let newest = store
        .messages()
        .unwrap()
        .iter()
        .map(|m| m.id)
        .max()
        .unwrap();
    store.remove_messages(&[newest]).unwrap();
    let reused = id_of(
        store
            .add_incoming(&mail(account, 70, "dee@s.com"), false)
            .unwrap(),
    );
    assert_eq!(reused, newest);
    assert_eq!(copy.follow(&store, "id reused"), 1);

    // A Microsoft account's mail, then the account removed.
    let work = store
        .add_account(&NewAccount {
            kind: AccountKind::Microsoft,
            email: "me@work.com",
            name: "Me",
            label: "Work",
            config: "{}",
        })
        .unwrap()
        .id;
    let graph = |n: u32| Incoming {
        account_id: work,
        remote_id: Some(format!("r{n}")),
        mailbox: None,
        uid: None,
        ..mail(work, n, "eve@r.com")
    };
    for n in 1..=3 {
        store.add_incoming(&graph(n), false).unwrap();
    }
    copy.follow(&store, "work mail");
    store.observe_remote_flags(work, "r1", false, true).unwrap();
    assert_eq!(copy.follow(&store, "work flags"), 1);
    store.observe_gone_remote(work, "r2").unwrap();
    assert_eq!(copy.follow(&store, "work mail archived"), 1);
    // Its attachments and invite, fetched in a later round.
    let plan = Attachment {
        name: "plan.pdf".into(),
        ..Default::default()
    };
    let invite = r#"{"kind":"invitation"}"#;
    store
        .fill_in_remote(work, "r3", &[plan], Some(invite))
        .unwrap();
    assert_eq!(copy.follow(&store, "work mail filled in"), 1);
    let r3 = copy
        .messages
        .iter()
        .find(|m| m.account_id == work && m.subject == "Message 3");
    let r3 = r3.unwrap();
    assert_eq!(r3.attachment.as_deref(), Some("plan.pdf"));
    assert_eq!(r3.invite_kind.as_deref(), Some("invitation"));
    store.fill_in_remote(work, "r3", &[], None).unwrap();
    assert_eq!(copy.follow(&store, "nothing to fill in"), 0);
    store.remove_account(work).unwrap();
    assert_eq!(copy.follow(&store, "account removed"), 3);
    assert!(copy.messages.iter().all(|m| m.account_id != work));
}

#[test]
fn a_rolled_back_write_is_not_reported_and_the_next_is() {
    let (mut store, account) = imap_store();
    let id = id_of(
        store
            .add_incoming(&mail(account, 1, "ana@x.com"), false)
            .unwrap(),
    );
    let mut copy = UiCopy::read(&store);
    {
        let tx = store.conn.unchecked_transaction().unwrap();
        store.remove_messages(&[id]).unwrap();
        // Inside a transaction the log may still be rolled back.
        assert_eq!(store.messages_marked().unwrap().1, None);
        assert_eq!(store.message_changes(&copy.mark).unwrap(), None);
        drop(tx);
    }
    assert_eq!(copy.follow(&store, "rolled back"), 0);
    store
        .add_incoming(&mail(account, 3, "bo@y.com"), false)
        .unwrap();
    assert_eq!(copy.follow(&store, "after the rollback"), 1);
}

#[test]
fn a_mark_from_before_what_the_log_dropped_reads_everything_again() {
    let (mut store, account) = imap_store();
    store
        .add_incoming(&mail(account, 1, "ana@x.com"), false)
        .unwrap();
    let behind = UiCopy::read(&store);
    store
        .add_incoming(&mail(account, 2, "ana@x.com"), false)
        .unwrap();
    let mut ahead = UiCopy::read(&store);
    store
        .add_incoming(&mail(account, 3, "ana@x.com"), false)
        .unwrap();
    // Reading from its mark drops the log before it, which `behind` needs.
    assert_eq!(ahead.follow(&store, "ahead"), 1);
    assert!(store.message_changes(&behind.mark).unwrap().is_none());
    store
        .add_incoming(&mail(account, 4, "ana@x.com"), false)
        .unwrap();
    assert_eq!(ahead.follow(&store, "ahead again"), 1);
    // Nothing logged since, and the log's last entry kept: still followed.
    assert_eq!(ahead.follow(&store, "nothing new"), 0);
    store
        .add_incoming(&mail(account, 5, "ana@x.com"), false)
        .unwrap();
    assert_eq!(ahead.follow(&store, "after a quiet spell"), 1);
}

#[test]
fn a_mark_from_another_opening_reads_everything_again() {
    let (store, _) = imap_store();
    let (other, _) = imap_store();
    let mark = store.messages_marked().unwrap().1.unwrap();
    assert!(other.message_changes(&mark).unwrap().is_none());
    assert!(store.message_changes(&mark).unwrap().is_some());
}

#[test]
fn another_connections_writes_read_everything_again() {
    let dir = std::env::temp_dir().join(format!("spark-changes-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("test.sqlite");
    let _ = std::fs::remove_file(&path);
    let mut store = Store::open(&path).unwrap();
    let account = store
        .add_account(&NewAccount {
            kind: AccountKind::Imap,
            email: ME,
            name: "Me",
            label: "Personal",
            config: "{}",
        })
        .unwrap()
        .id;
    store
        .add_incoming(&mail(account, 1, "ana@x.com"), false)
        .unwrap();
    let mut copy = UiCopy::read(&store);
    // spark-cli marking it read.
    let other = rusqlite::Connection::open(&path).unwrap();
    other.execute("UPDATE messages SET unread = 0", []).unwrap();
    assert!(store.message_changes(&copy.mark).unwrap().is_none());
    copy = UiCopy::read(&store);
    assert!(!copy.messages[0].unread);
    store
        .add_incoming(&mail(account, 2, "ana@x.com"), false)
        .unwrap();
    assert_eq!(copy.follow(&store, "after reading again"), 1);
    drop((other, store));
    std::fs::remove_dir_all(&dir).unwrap();
}

/// What the log costs a sync: 20,000 messages stored a chunk of 500 at a
/// time (a fifth also in a folder), then a flag changed on each, with the
/// log's triggers and without them.
#[test]
#[ignore = "a timing: cargo test --release -p spark-store -- --ignored --nocapture"]
fn time_syncing_with_and_without_the_log() {
    use std::time::{Duration, Instant};

    fn sync(logged: bool) -> (Duration, Duration) {
        let (mut store, account) = imap_store();
        store
            .replace_folders(account, &[remote("Travel", None)])
            .unwrap();
        let travel = folder(&store, "Travel");
        if !logged {
            let triggers: Vec<String> = store
                .conn
                .prepare("SELECT name FROM temp.sqlite_master WHERE type = 'trigger' AND name LIKE 'log_%'")
                .unwrap()
                .query_map([], |r| r.get(0))
                .unwrap()
                .collect::<Result<_>>()
                .unwrap();
            assert_eq!(triggers.len(), 15);
            for name in triggers {
                store
                    .conn
                    .execute_batch(&format!("DROP TRIGGER temp.{name}"))
                    .unwrap();
            }
        }
        let mail: Vec<Incoming> = (1..=20_000)
            .map(|uid| mail(account, uid, &format!("s{}@x.com", uid % 500)))
            .collect();
        let start = Instant::now();
        for chunk in mail.chunks(500) {
            store
                .batch(|b| {
                    for m in chunk {
                        b.add_incoming(m, false)?;
                        if m.uid.unwrap().is_multiple_of(5) {
                            b.add_folder_message(travel, m)?;
                        }
                    }
                    Ok(())
                })
                .unwrap();
        }
        let storing = start.elapsed();
        let start = Instant::now();
        for chunk in mail.chunks(500) {
            store
                .batch(|b| {
                    for m in chunk {
                        b.observe_flags(account, "INBOX", m.uid.unwrap(), false, false)?;
                    }
                    Ok(())
                })
                .unwrap();
        }
        (storing, start.elapsed())
    }

    for logged in [false, true, false, true] {
        let (storing, flags) = sync(logged);
        let label = if logged {
            "with the log   "
        } else {
            "without the log"
        };
        println!("{label}: storing {storing:?}, flags {flags:?}");
    }
}
