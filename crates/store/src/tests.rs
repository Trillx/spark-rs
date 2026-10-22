use super::*;

fn store_with_account() -> (Store, i64) {
    let store = Store::open_in_memory().unwrap();
    let account = store
        .add_account(&NewAccount {
            kind: AccountKind::Gmail,
            email: "me@gmail.com",
            name: "Me",
            label: "Personal",
            config: "{}",
        })
        .unwrap();
    (store, account.id)
}

fn incoming(account_id: i64, uid: u32, from: &str) -> Incoming {
    Incoming {
        account_id,
        mailbox: Some("INBOX".into()),
        uid: Some(uid),
        message_id: Some(format!("<{uid}@example.com>")),
        from_name: "Sender".into(),
        from_address: from.into(),
        subject: format!("Message {uid}"),
        date: 1_000 + uid as i64,
        unread: true,
        category: Category::People,
        ..Default::default()
    }
}

fn new_id(added: Added) -> i64 {
    match added {
        Added::New(id) => id,
        other => panic!("expected a new message, got {other:?}"),
    }
}

#[test]
fn bundled_sqlite_has_fts5() {
    let store = Store::open_in_memory().unwrap();
    assert!(store.has_fts5().unwrap());
}

#[test]
fn opens_file_database_in_wal_mode_and_reopens() {
    let dir = std::env::temp_dir().join(format!("spark-store-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("test.sqlite");
    let _ = std::fs::remove_file(&path);
    {
        let store = Store::open(&path).unwrap();
        let mode: String = store
            .conn
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
        store
            .add_account(&NewAccount {
                kind: AccountKind::Imap,
                email: "a@b.c",
                name: "A",
                label: "A",
                config: "{}",
            })
            .unwrap();
    }
    // The schema isn't created twice.
    let store = Store::open(&path).unwrap();
    assert_eq!(store.accounts().unwrap().len(), 1);
    drop(store);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn accounts_get_colors_in_turn_and_removal_takes_their_mail() {
    let (mut store, first) = store_with_account();
    let second = store
        .add_account(&NewAccount {
            kind: AccountKind::Microsoft,
            email: "me@work.com",
            name: "Me",
            label: "Work",
            config: r#"{"clientId":"x"}"#,
        })
        .unwrap();
    let accounts = store.accounts().unwrap();
    assert_eq!(accounts.len(), 2);
    assert_ne!(accounts[0].color, accounts[1].color);
    assert_eq!(second.kind, AccountKind::Microsoft);
    assert!(
        store
            .add_account(&NewAccount {
                kind: AccountKind::Gmail,
                email: "ME@gmail.com",
                name: "",
                label: "",
                config: "{}",
            })
            .is_err(),
        "the same address can't be added twice"
    );

    store
        .add_incoming(&incoming(first, 1, "x@y.z"), false)
        .unwrap();
    store
        .add_incoming(&incoming(second.id, 1, "x@y.z"), false)
        .unwrap();
    store.remove_account(first).unwrap();
    let left = store.messages().unwrap();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].account_id, second.id);
}

#[test]
fn incoming_mail_is_stored_once_and_found_again_after_leaving_the_inbox() {
    let (mut store, account) = store_with_account();
    let id = new_id(
        store
            .add_incoming(&incoming(account, 7, "a@x.com"), false)
            .unwrap(),
    );
    assert_eq!(
        store
            .add_incoming(&incoming(account, 7, "a@x.com"), false)
            .unwrap(),
        Added::Known(id)
    );
    assert_eq!(store.uids(account, "INBOX").unwrap(), HashSet::from([7]));

    // Archived elsewhere: it becomes Done here.
    store.observe_gone_uids(account, "INBOX", &[7]).unwrap();
    assert_eq!(
        store.message(id).unwrap().unwrap().state,
        MessageState::Done
    );
    assert!(store.uids(account, "INBOX").unwrap().is_empty());

    // Moved back to the inbox elsewhere: new UID, same Message-ID.
    let mut back = incoming(account, 9, "a@x.com");
    back.message_id = Some("<7@example.com>".into());
    assert_eq!(store.add_incoming(&back, true).unwrap(), Added::Known(id));
    let m = store.message(id).unwrap().unwrap();
    assert_eq!(m.state, MessageState::Inbox);
    assert_eq!(store.uids(account, "INBOX").unwrap(), HashSet::from([9]));
    assert_eq!(store.messages().unwrap().len(), 1);
}

#[test]
fn search_looks_in_senders_subjects_and_bodies() {
    let (mut store, account) = store_with_account();
    let mut quote = incoming(account, 1, "ana@parts.com");
    quote.text = "The 100% steel bracket ships Friday.".into();
    let quote = new_id(store.add_incoming(&quote, false).unwrap());
    let invoice = new_id(
        store
            .add_incoming(&incoming(account, 2, "billing@vendor.com"), false)
            .unwrap(),
    );
    let ids = |text: &str| -> Vec<i64> {
        store
            .search(text)
            .unwrap()
            .into_iter()
            .map(|m| m.id)
            .collect()
    };
    assert_eq!(ids("STEEL"), [quote]);
    assert_eq!(ids("vendor.com"), [invoice]);
    assert_eq!(ids("message 2"), [invoice]);
    // Newest first, and LIKE's wildcards are plain characters.
    assert_eq!(ids("sender"), [invoice, quote]);
    assert_eq!(ids("100%"), [quote]);
    assert!(ids("_").is_empty());
}

#[test]
fn new_senders_wait_in_the_gatekeeper_after_the_first_sync() {
    let (mut store, account) = store_with_account();
    // First sync: everyone already in the inbox is accepted.
    let known = new_id(
        store
            .add_incoming(&incoming(account, 1, "old@x.com"), false)
            .unwrap(),
    );
    assert!(!store.message(known).unwrap().unwrap().screening);

    let again = new_id(
        store
            .add_incoming(&incoming(account, 2, "old@x.com"), true)
            .unwrap(),
    );
    let fresh = new_id(
        store
            .add_incoming(&incoming(account, 3, "new@x.com"), true)
            .unwrap(),
    );
    let fresh2 = new_id(
        store
            .add_incoming(&incoming(account, 4, "new@x.com"), true)
            .unwrap(),
    );
    assert!(!store.message(again).unwrap().unwrap().screening);
    assert!(store.message(fresh).unwrap().unwrap().screening);

    // Accepting one message accepts the sender.
    let patch = Patch {
        screening: Some(false),
        ..Default::default()
    };
    store.update(fresh, &patch, 10).unwrap();
    assert!(!store.message(fresh2).unwrap().unwrap().screening);
    let later = new_id(
        store
            .add_incoming(&incoming(account, 5, "new@x.com"), true)
            .unwrap(),
    );
    assert!(!store.message(later).unwrap().unwrap().screening);

    // Undoing the accept puts the sender back in the Gatekeeper.
    let undo = Patch {
        screening: Some(true),
        ..Default::default()
    };
    store.update(fresh, &undo, 11).unwrap();
    assert!(store.message(fresh2).unwrap().unwrap().screening);

    // The account's own address is never screened.
    let own = new_id(
        store
            .add_incoming(&incoming(account, 6, "me@gmail.com"), true)
            .unwrap(),
    );
    assert!(!store.message(own).unwrap().unwrap().screening);

    // Writing to someone accepts them.
    let stranger = new_id(
        store
            .add_incoming(&incoming(account, 8, "s@x.com"), true)
            .unwrap(),
    );
    store.accept_sender(account, "S@x.com").unwrap();
    assert!(!store.message(stranger).unwrap().unwrap().screening);
}

#[test]
fn blocking_a_sender_blocks_their_mail_now_and_later() {
    let (mut store, account) = store_with_account();
    let a = new_id(
        store
            .add_incoming(&incoming(account, 1, "spam@x.com"), true)
            .unwrap(),
    );
    let b = new_id(
        store
            .add_incoming(&incoming(account, 2, "spam@x.com"), true)
            .unwrap(),
    );
    let block = Patch {
        state: Some(MessageState::Blocked),
        screening: Some(false),
        ..Default::default()
    };
    store.update(a, &block, 10).unwrap();
    assert_eq!(
        store.message(b).unwrap().unwrap().state,
        MessageState::Blocked
    );
    let c = new_id(
        store
            .add_incoming(&incoming(account, 3, "spam@x.com"), true)
            .unwrap(),
    );
    assert_eq!(
        store.message(c).unwrap().unwrap().state,
        MessageState::Blocked
    );
    // Blocking is local: nothing to push.
    assert!(store.pending(account, 1_000, 0).unwrap().is_empty());
}

#[test]
fn local_changes_wait_for_the_undo_window_then_get_pushed_once() {
    let (mut store, account) = store_with_account();
    let id = new_id(
        store
            .add_incoming(&incoming(account, 1, "a@x.com"), false)
            .unwrap(),
    );

    let done = Patch {
        state: Some(MessageState::Done),
        unread: Some(false),
        ..Default::default()
    };
    store.update(id, &done, 1_000).unwrap();
    assert!(
        store.pending(account, 3_000, 5_000).unwrap().is_empty(),
        "still undoable"
    );
    let pending = store.pending(account, 6_000, 5_000).unwrap();
    assert_eq!(pending.len(), 1);
    let p = &pending[0];
    assert_eq!((p.place, p.unread), (Place::Archive, false));
    assert_eq!((p.remote_place, p.remote_unread), (Place::Inbox, true));
    assert_eq!(p.uid, Some(1));

    store
        .mark_pushed(
            p,
            Pushed {
                mailbox: None,
                uid: None,
            },
        )
        .unwrap();
    assert!(store.pending(account, 9_000, 0).unwrap().is_empty());

    // Undo within the window: back where the server already is, so nothing to push.
    let id2 = new_id(
        store
            .add_incoming(&incoming(account, 2, "a@x.com"), false)
            .unwrap(),
    );
    store.update(id2, &done, 1_000).unwrap();
    let undo = Patch {
        state: Some(MessageState::Inbox),
        unread: Some(true),
        ..Default::default()
    };
    store.update(id2, &undo, 2_000).unwrap();
    assert!(store.pending(account, 9_000, 0).unwrap().is_empty());

    // Snooze, Set Aside and priority are local only.
    let snooze = Patch {
        state: Some(MessageState::Snoozed),
        snoozed_until: Some(50_000),
        priority: Some(true),
        ..Default::default()
    };
    store.update(id2, &snooze, 3_000).unwrap();
    assert!(store.pending(account, 9_000, 0).unwrap().is_empty());
}

#[test]
fn a_change_made_while_pushing_stays_pending() {
    let (mut store, account) = store_with_account();
    let id = new_id(
        store
            .add_incoming(&incoming(account, 1, "a@x.com"), false)
            .unwrap(),
    );
    let read = Patch {
        unread: Some(false),
        ..Default::default()
    };
    store.update(id, &read, 1_000).unwrap();
    let p = store.pending(account, 1_000, 0).unwrap().remove(0);
    let pin = Patch {
        pinned: Some(true),
        ..Default::default()
    };
    store.update(id, &pin, 1_500).unwrap();
    store
        .mark_pushed(
            &p,
            Pushed {
                mailbox: Some("INBOX"),
                uid: Some(1),
            },
        )
        .unwrap();
    let still = store.pending(account, 2_000, 0).unwrap();
    assert_eq!(still.len(), 1);
    assert!(still[0].pinned && !still[0].remote_pinned);
}

#[test]
fn server_flags_win_unless_a_local_change_is_waiting() {
    let (mut store, account) = store_with_account();
    let a = new_id(
        store
            .add_incoming(&incoming(account, 1, "a@x.com"), false)
            .unwrap(),
    );
    let b = new_id(
        store
            .add_incoming(&incoming(account, 2, "a@x.com"), false)
            .unwrap(),
    );
    store
        .update(
            b,
            &Patch {
                pinned: Some(true),
                ..Default::default()
            },
            1_000,
        )
        .unwrap();
    store
        .observe_flags(account, "INBOX", 1, false, true)
        .unwrap();
    store
        .observe_flags(account, "INBOX", 2, false, false)
        .unwrap();
    let a = store.message(a).unwrap().unwrap();
    assert!(!a.unread && a.pinned);
    let b = store.message(b).unwrap().unwrap();
    assert!(!b.unread, "read elsewhere");
    assert!(b.pinned, "the local pin is still waiting to be pushed");
}

#[test]
fn snoozed_mail_comes_back_unread_when_its_time_comes() {
    let (mut store, account) = store_with_account();
    let id = new_id(
        store
            .add_incoming(&incoming(account, 1, "a@x.com"), false)
            .unwrap(),
    );
    let snooze = Patch {
        state: Some(MessageState::Snoozed),
        snoozed_until: Some(5_000),
        unread: Some(false),
        ..Default::default()
    };
    store.update(id, &snooze, 1_000).unwrap();
    assert_eq!(store.wake_snoozed(4_999).unwrap(), 0);
    assert_eq!(store.wake_snoozed(5_000).unwrap(), 1);
    let m = store.message(id).unwrap().unwrap();
    assert_eq!(m.state, MessageState::Inbox);
    assert!(m.unread);
    assert_eq!(m.snoozed_until, None);
}

#[test]
fn graph_messages_are_keyed_by_remote_id() {
    let (mut store, account) = store_with_account();
    let mut m = Incoming {
        account_id: account,
        remote_id: Some("AAMk1".into()),
        from_address: "a@x.com".into(),
        to: vec!["Me <me@gmail.com>".into()],
        html: Some("<p>Hi</p>".into()),
        text: "Hi".into(),
        attachments: vec![
            Attachment {
                name: "a.pdf".into(),
                mime: "application/pdf".into(),
                size: 1234,
                remote_id: Some("att1".into()),
            },
            Attachment {
                name: "b.png".into(),
                ..Default::default()
            },
        ],
        unread: true,
        ..Default::default()
    };
    let id = new_id(store.add_incoming(&m, false).unwrap());
    m.unread = false;
    assert_eq!(store.add_incoming(&m, false).unwrap(), Added::Known(id));
    let row = store.message(id).unwrap().unwrap();
    assert!(!row.unread);
    assert_eq!(row.attachment.as_deref(), Some("a.pdf"));
    let content = store.content(id).unwrap().unwrap();
    assert_eq!(content.html.as_deref(), Some("<p>Hi</p>"));
    assert_eq!(content.attachments, m.attachments);
    assert_eq!(content.to, vec!["Me <me@gmail.com>"]);

    assert_eq!(store.observe_gone_remote(account, "AAMk1").unwrap(), 1);
    assert_eq!(
        store.message(id).unwrap().unwrap().state,
        MessageState::Done
    );
}

#[test]
fn queued_sends_can_be_cancelled_until_claimed() {
    let (mut store, account) = store_with_account();
    let a = store.queue_send(account, "{\"a\":1}", 5_000, &[]).unwrap();
    let b = store.queue_send(account, "{\"b\":1}", 5_000, &[]).unwrap();
    assert!(store.claim_due_sends(account, 4_000).unwrap().is_empty());
    assert!(store.cancel_send(a).unwrap());
    assert!(!store.cancel_send(a).unwrap());

    let due = store.claim_due_sends(account, 5_000).unwrap();
    assert_eq!(due, vec![(b, "{\"b\":1}".to_owned())]);
    assert!(!store.cancel_send(b).unwrap(), "already sending");
    assert!(store.claim_due_sends(account, 9_000).unwrap().is_empty());

    // A crash mid-send puts it back in the queue.
    assert_eq!(store.requeue_interrupted_sends().unwrap(), 1);
    assert_eq!(store.claim_due_sends(account, 9_000).unwrap().len(), 1);
    store.finish_send(b).unwrap();
    assert_eq!(store.requeue_interrupted_sends().unwrap(), 0);
}

#[test]
fn queued_files_stay_with_their_draft_until_it_is_gone() {
    let (mut store, account) = store_with_account();
    let files = [
        OutboxFile {
            name: "plan.pdf".into(),
            mime: "application/pdf".into(),
            data: b"%PDF-1.7".to_vec(),
        },
        OutboxFile {
            name: "photo.jpg".into(),
            mime: "image/jpeg".into(),
            data: vec![0xff, 0xd8, 0xff],
        },
    ];
    let id = store.queue_send(account, "{}", 5_000, &files).unwrap();
    assert_eq!(store.outbox_files(id).unwrap(), files);

    // Undo (cancel) and a finished send both drop the copies.
    assert!(store.cancel_send(id).unwrap());
    assert!(store.outbox_files(id).unwrap().is_empty());
    let id = store.queue_send(account, "{}", 5_000, &files).unwrap();
    store.claim_due_sends(account, 5_000).unwrap();
    store.finish_send(id).unwrap();
    let left: i64 = store
        .conn
        .query_row("SELECT COUNT(*) FROM outbox_files", [], |r| r.get(0))
        .unwrap();
    assert_eq!(left, 0);
}

#[test]
fn attachments_stored_as_bare_names_still_read() {
    let (mut store, account) = store_with_account();
    let id = new_id(
        store
            .add_incoming(&incoming(account, 1, "a@x.com"), false)
            .unwrap(),
    );
    // Rows written before sizes and types were kept.
    store
        .conn
        .execute(
            "UPDATE messages SET attachments = '[\"old.pdf\"]' WHERE id = ?1",
            [id],
        )
        .unwrap();
    let content = store.content(id).unwrap().unwrap();
    assert_eq!(
        content.attachments,
        vec![Attachment {
            name: "old.pdf".into(),
            ..Default::default()
        }]
    );
    assert_eq!(
        store.message(id).unwrap().unwrap().attachment.as_deref(),
        Some("old.pdf")
    );
}

#[test]
fn locations_say_where_the_server_keeps_a_message() {
    let (mut store, account) = store_with_account();
    let id = new_id(
        store
            .add_incoming(&incoming(account, 7, "a@x.com"), false)
            .unwrap(),
    );
    let at = store.location(id).unwrap().unwrap();
    assert_eq!(at.account_id, account);
    assert_eq!((at.mailbox.as_deref(), at.uid), (Some("INBOX"), Some(7)));
    assert_eq!(at.remote_place, Place::Inbox);
    assert_eq!(store.location(id + 100).unwrap(), None);
}

#[test]
fn version_1_databases_gain_the_outbox_files_table() {
    let dir = std::env::temp_dir().join(format!("spark-store-v1-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("v1.sqlite");
    let _ = std::fs::remove_file(&path);
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        conn.pragma_update(None, "user_version", 1).unwrap();
    }
    let mut store = Store::open(&path).unwrap();
    let account = store
        .add_account(&NewAccount {
            kind: AccountKind::Imap,
            email: "a@b.c",
            name: "A",
            label: "A",
            config: "{}",
        })
        .unwrap();
    let file = OutboxFile {
        name: "a.txt".into(),
        mime: "text/plain".into(),
        data: b"hi".to_vec(),
    };
    let id = store
        .queue_send(account.id, "{}", 0, std::slice::from_ref(&file))
        .unwrap();
    assert_eq!(store.outbox_files(id).unwrap(), vec![file]);
    drop(store);
    std::fs::remove_dir_all(&dir).unwrap();
}
