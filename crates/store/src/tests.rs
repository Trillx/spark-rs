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
    // Newest first. Words match from their start, punctuation is ignored,
    // and text without letters or digits finds nothing.
    assert_eq!(ids("sender"), [invoice, quote]);
    assert_eq!(ids("brack"), [quote]);
    assert!(ids("racket").is_empty());
    assert_eq!(ids("100%"), [quote]);
    assert!(ids("_").is_empty());
}

fn found(store: &Store, text: &str) -> Vec<i64> {
    store
        .search(text)
        .unwrap()
        .into_iter()
        .map(|m| m.id)
        .collect()
}

/// Fails unless the search index agrees with the messages table.
fn check_search_index(store: &Store) {
    store
        .conn
        .execute(
            "INSERT INTO messages_fts (messages_fts, rank) VALUES ('integrity-check', 1)",
            [],
        )
        .unwrap();
}

#[test]
fn search_ignores_case_and_accents_and_finds_recipients() {
    let (mut store, account) = store_with_account();
    let mut booking = incoming(account, 1, "chef@bistro.fr");
    booking.subject = "Réservation au Café".into();
    booking.to = vec!["Zoë Ortiz <zoe@example.com>".into()];
    booking.text = "Table pour deux, vendredi soir.".into();
    let booking = new_id(store.add_incoming(&booking, false).unwrap());
    for text in [
        "cafe",
        "RESERV",
        "zoe",
        "zoe@example.com",
        "vendredi table",
        "\"pour deux\"",
    ] {
        assert_eq!(found(&store, text), [booking], "{text}");
    }
    // Not the middle of a word, quoted words only in that order, and every word.
    for text in ["afe", "\"deux pour\"", "table lundi"] {
        assert!(found(&store, text).is_empty(), "{text}");
    }
}

#[test]
fn search_takes_operators_and_punctuation_literally() {
    let (mut store, account) = store_with_account();
    let mut m = incoming(account, 1, "ana@parts.com");
    m.text = "Prices AND terms (draft) - see NEAR the end: 50*2".into();
    let id = new_id(store.add_incoming(&m, false).unwrap());
    for text in [
        "AND",
        "(draft)",
        "NEAR(",
        "-see",
        "end:",
        "50*2",
        "\"terms (draft\"",
    ] {
        assert_eq!(found(&store, text), [id], "{text}");
    }
    for text in ["terms OR", "\"", "*", "(", ":", "\"unclosed"] {
        assert!(found(&store, text).is_empty(), "{text}");
    }
}

#[test]
fn queries_parse_like_the_ui() {
    let query = Query::parse("bracket From:Ana \"price list\" has:attachment is:unread q4");
    assert_eq!(
        query,
        Query {
            text: "bracket \"price list\" q4".into(),
            from: Some("ana".into()),
            has_attachment: true,
            unread_only: true,
        }
    );
    assert_eq!(
        query.terms(),
        [
            Term::Word("bracket".into()),
            Term::Phrase("price list".into()),
            Term::Word("q4".into()),
        ]
    );
    // Punctuation alone is no term, and an unclosed quote runs to the end.
    let odd = Query::parse("- (draft) \"price list from:");
    assert_eq!(odd.from, None);
    assert_eq!(
        odd.terms(),
        [
            Term::Word("(draft)".into()),
            Term::Phrase("price list from:".into()),
        ]
    );
}

#[test]
fn search_forgets_removed_mail() {
    let (mut store, account) = store_with_account();
    let mut m = incoming(account, 1, "ana@parts.com");
    m.text = "Bracket quote".into();
    store.add_incoming(&m, false).unwrap();
    assert_eq!(found(&store, "bracket").len(), 1);
    store.remove_account(account).unwrap();
    assert!(found(&store, "bracket").is_empty());
    check_search_index(&store);
}

#[test]
fn version_8_databases_index_their_mail_for_search() {
    let dir = std::env::temp_dir().join(format!("spark-store-v8-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("v8.sqlite");
    let _ = std::fs::remove_file(&path);
    {
        let conn = Connection::open(&path).unwrap();
        for schema in [
            SCHEMA,
            SCHEMA_2,
            SCHEMA_3,
            SCHEMA_4,
            drafts::SCHEMA_DRAFTS,
            conversations::SCHEMA_CONVERSATIONS,
            SCHEMA_7,
            calendar::SCHEMA_CALENDAR,
        ] {
            conn.execute_batch(schema).unwrap();
        }
        conn.pragma_update(None, "user_version", 8).unwrap();
        conn.execute_batch(
            "INSERT INTO accounts (id, kind, email, name, label, color, config, position)
             VALUES (1, 'imap', 'a@b.c', 'A', 'A', '#000', '{}', 0);
             INSERT INTO messages (id, account_id, from_name, from_address, to_list, cc_list,
                 subject, snippet, date, text_body, attachments, category, state, unread, pinned,
                 remote_place, remote_unread, remote_pinned)
             VALUES (1, 1, 'Ana', 'ana@x.com', '[]', '[]', 'Plan', '', 0, 'Budget attached',
                     '[]', 'people', 'inbox', 0, 0, 'inbox', 0, 0);",
        )
        .unwrap();
    }
    let store = Store::open(&path).unwrap();
    assert_eq!(found(&store, "budget"), [1]);
    check_search_index(&store);
    drop(store);
    std::fs::remove_dir_all(&dir).unwrap();
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
fn invites_and_the_answer_sent_stay_with_their_email() {
    let (mut store, account) = store_with_account();
    let plain = new_id(
        store
            .add_incoming(&incoming(account, 1, "a@x.com"), false)
            .unwrap(),
    );
    let invite = Incoming {
        invite: Some("{\"kind\":\"invitation\"}".into()),
        ..incoming(account, 2, "a@x.com")
    };
    let id = new_id(store.add_incoming(&invite, false).unwrap());
    let content = store.content(plain).unwrap().unwrap();
    assert_eq!((content.invite, content.rsvp), (None, None));
    store.set_rsvp(id, "accepted").unwrap();
    let content = store.content(id).unwrap().unwrap();
    assert_eq!(content.invite.as_deref(), Some("{\"kind\":\"invitation\"}"));
    assert_eq!(content.rsvp.as_deref(), Some("accepted"));
}

fn calendar(remote_id: &str, name: &str) -> RemoteCalendar {
    RemoteCalendar {
        remote_id: remote_id.into(),
        name: name.into(),
        color: None,
        is_default: false,
    }
}

fn event(remote_id: &str, start: i64, end: i64) -> RemoteEvent {
    RemoteEvent {
        remote_id: remote_id.into(),
        start,
        end,
        data: format!("{{\"title\":\"{remote_id}\"}}"),
    }
}

#[test]
fn calendars_keep_their_ids_and_go_with_their_events() {
    let (mut store, account) = store_with_account();
    store
        .replace_calendars(
            account,
            &[calendar("a", "Calendar"), calendar("b", "Birthdays")],
        )
        .unwrap();
    let before = store.calendars().unwrap();
    assert_eq!(
        before.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
        ["Calendar", "Birthdays"]
    );
    store
        .replace_events(before[1].id, 0, 100, &[event("x", 10, 20)])
        .unwrap();

    store
        .replace_calendars(account, &[calendar("a", "Work")])
        .unwrap();
    let after = store.calendars().unwrap();
    assert_eq!(after.len(), 1);
    assert_eq!(
        (after[0].id, after[0].name.as_str()),
        (before[0].id, "Work")
    );
    assert!(
        store.events_between(0, 100).unwrap().is_empty(),
        "its events went with it"
    );
}

#[test]
fn fetching_a_stretch_again_replaces_only_what_is_in_it() {
    let (mut store, account) = store_with_account();
    store
        .replace_calendars(account, &[calendar("a", "Calendar")])
        .unwrap();
    let id = store.calendars().unwrap()[0].id;
    store
        .replace_events(
            id,
            0,
            1000,
            &[
                event("early", 10, 20),
                event("late", 900, 950),
                event("long", 500, 2000),
            ],
        )
        .unwrap();
    store
        .replace_events(id, 1000, 3000, &[event("next", 1500, 1600)])
        .unwrap();
    // "early" moved and "long" was deleted on the server; "late" is outside.
    store
        .replace_events(id, 0, 400, &[event("early", 30, 40)])
        .unwrap();
    store
        .replace_events(id, 400, 1200, &[event("late", 900, 950)])
        .unwrap();
    let events = store.events_between(0, 3000).unwrap();
    let listed: Vec<_> = events
        .iter()
        .map(|e| (e.remote_id.as_str(), e.start))
        .collect();
    assert_eq!(listed, [("early", 30), ("late", 900), ("next", 1500)]);
    assert_eq!(events[0].account_id, account);
    assert_eq!(
        store.events_between(950, 1500).unwrap().len(),
        0,
        "[start, end) on both sides"
    );

    store.set_event_data(events[0].id, "{}").unwrap();
    assert_eq!(store.event(events[0].id).unwrap().unwrap().data, "{}");
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

fn remote(
    remote_id: &str,
    parent: Option<&str>,
    name: &str,
    role: Option<FolderRole>,
) -> RemoteFolder {
    RemoteFolder {
        remote_id: remote_id.into(),
        parent: parent.map(Into::into),
        name: name.into(),
        role,
        selectable: true,
    }
}

/// `incoming`, but found in folder `mailbox`.
fn in_folder(account_id: i64, mailbox: &str, uid: u32, message: u32) -> Incoming {
    Incoming {
        mailbox: Some(mailbox.into()),
        uid: Some(uid),
        message_id: Some(format!("<{message}@example.com>")),
        subject: format!("Message {message}"),
        ..incoming(account_id, uid, "a@x.com")
    }
}

fn folder_id(store: &Store, remote_id: &str) -> i64 {
    store
        .folders()
        .unwrap()
        .into_iter()
        .find(|f| f.remote_id == remote_id)
        .unwrap_or_else(|| panic!("no folder {remote_id}"))
        .id
}

#[test]
fn folders_keep_their_ids_across_listings_and_nest() {
    let (mut store, account) = store_with_account();
    let listed = vec![
        remote("Projects", None, "Projects", None),
        remote("Projects/Q4", Some("Projects"), "Q4", None),
        remote(
            "[Gmail]/Sent Mail",
            None,
            "Sent Mail",
            Some(FolderRole::Sent),
        ),
    ];
    assert!(store.replace_folders(account, &listed).unwrap());
    let folders = store.folders().unwrap();
    assert_eq!(folders.len(), 3);
    let (projects, q4) = (folders[0].id, folders[1].id);
    assert_eq!(folders[1].parent_id, Some(projects));
    assert_eq!(folders[1].name, "Q4");
    assert_eq!(folders[2].role, Some(FolderRole::Sent));
    assert_eq!(folders[0].synced_at, None);

    // The same listing changes nothing.
    assert!(!store.replace_folders(account, &listed).unwrap());

    // Reordered, one gone: ids stay, the gone one goes with its mail.
    let only_q4 = new_id(
        store
            .add_folder_message(projects, &in_folder(account, "Projects", 1, 1))
            .unwrap(),
    );
    let reordered = vec![listed[2].clone(), listed[1].clone()];
    assert!(store.replace_folders(account, &reordered).unwrap());
    let folders = store.folders().unwrap();
    assert_eq!(
        folders.iter().map(|f| f.id).collect::<Vec<_>>(),
        [folder_id(&store, "[Gmail]/Sent Mail"), q4]
    );
    assert_eq!(folders[1].parent_id, None, "its parent is gone");
    // On Gmail a label's mail stays in All Mail.
    assert!(store.message(only_q4).unwrap().unwrap().folders.is_empty());
}

#[test]
fn folder_mail_is_filed_unless_the_inbox_has_it() {
    let (mut store, account) = store_with_account();
    store
        .replace_folders(
            account,
            &[
                remote("Travel", None, "Travel", None),
                remote("Receipts", None, "Receipts", None),
                remote(
                    "[Gmail]/All Mail",
                    None,
                    "All Mail",
                    Some(FolderRole::Archive),
                ),
                remote("[Gmail]/Trash", None, "Trash", Some(FolderRole::Trash)),
            ],
        )
        .unwrap();
    let travel = folder_id(&store, "Travel");
    let receipts = folder_id(&store, "Receipts");

    // Only in a folder: Filed there, and listed with it.
    let flight = new_id(
        store
            .add_folder_message(travel, &in_folder(account, "Travel", 5, 100))
            .unwrap(),
    );
    let m = store.message(flight).unwrap().unwrap();
    assert_eq!(m.state, MessageState::Filed);
    assert!(!m.screening, "folder mail never waits in the Gatekeeper");
    assert_eq!(m.folders, [travel]);
    let at = store.location(flight).unwrap().unwrap();
    assert_eq!((at.mailbox.as_deref(), at.uid), (Some("Travel"), Some(5)));
    assert_eq!(at.remote_place, Place::Folder);

    // Inbox mail with a label stays in the inbox, and in the folder too.
    let lunch = new_id(
        store
            .add_incoming(&incoming(account, 1, "a@x.com"), false)
            .unwrap(),
    );
    let added = store
        .add_folder_message(travel, &in_folder(account, "Travel", 6, 1))
        .unwrap();
    assert_eq!(added, Added::Known(lunch));
    let m = store.message(lunch).unwrap().unwrap();
    assert_eq!(m.state, MessageState::Inbox);
    assert_eq!(m.folders, [travel]);
    assert_eq!(
        store.location(lunch).unwrap().unwrap().mailbox.as_deref(),
        Some("INBOX")
    );

    // Archived elsewhere, then found in a folder: it moved there.
    store.observe_gone_uids(account, "INBOX", &[1]).unwrap();
    store
        .add_folder_message(receipts, &in_folder(account, "Receipts", 2, 1))
        .unwrap();
    let m = store.message(lunch).unwrap().unwrap();
    assert_eq!(m.state, MessageState::Filed);
    assert_eq!(
        store.location(lunch).unwrap().unwrap().mailbox.as_deref(),
        Some("Receipts")
    );

    // Another label's folder doesn't take it over.
    store
        .add_folder_message(travel, &in_folder(account, "Travel", 6, 1))
        .unwrap();
    assert_eq!(
        store.location(lunch).unwrap().unwrap().mailbox.as_deref(),
        Some("Receipts")
    );
    let mut both = store.message(lunch).unwrap().unwrap().folders;
    both.sort();
    assert_eq!(both, [travel, receipts]);
    assert_eq!(
        store.member_locations(lunch).unwrap(),
        [("Travel".to_owned(), 6), ("Receipts".to_owned(), 2)]
    );

    // Archive mail is Done; Trash mail is Deleted.
    let all_mail = folder_id(&store, "[Gmail]/All Mail");
    let old = new_id(
        store
            .add_folder_message(all_mail, &in_folder(account, "[Gmail]/All Mail", 9, 200))
            .unwrap(),
    );
    assert_eq!(
        store.message(old).unwrap().unwrap().state,
        MessageState::Done
    );
    let trash = folder_id(&store, "[Gmail]/Trash");
    let binned = new_id(
        store
            .add_folder_message(trash, &in_folder(account, "[Gmail]/Trash", 3, 300))
            .unwrap(),
    );
    assert_eq!(
        store.message(binned).unwrap().unwrap().state,
        MessageState::Deleted
    );

    // Moved into the inbox on another device: back in the Inbox.
    store
        .add_incoming(
            &Incoming {
                message_id: Some("<100@example.com>".into()),
                ..incoming(account, 40, "a@x.com")
            },
            true,
        )
        .unwrap();
    let m = store.message(flight).unwrap().unwrap();
    assert_eq!(m.state, MessageState::Inbox);
    assert_eq!(m.folders, [travel], "the label stays");
}

#[test]
fn folder_membership_is_replaced_by_each_sync() {
    let (mut store, account) = store_with_account();
    store
        .replace_folders(account, &[remote("Travel", None, "Travel", None)])
        .unwrap();
    let travel = folder_id(&store, "Travel");
    let a = new_id(
        store
            .add_folder_message(travel, &in_folder(account, "Travel", 1, 1))
            .unwrap(),
    );
    let b = new_id(
        store
            .add_folder_message(travel, &in_folder(account, "Travel", 2, 2))
            .unwrap(),
    );
    assert_eq!(
        store.folder_uids(travel).unwrap(),
        HashMap::from([(1, a), (2, b)])
    );
    assert_eq!(
        store.set_folder_members(travel, &[(b, Some(2))]).unwrap(),
        1
    );
    assert_eq!(store.folder_member_ids(travel).unwrap(), HashSet::from([b]));
    assert!(store.message(a).unwrap().unwrap().folders.is_empty());

    // A renumbered folder forgets its UIDs.
    store.forget_folder_uids(travel).unwrap();
    assert!(store.folder_uids(travel).unwrap().is_empty());
    assert_eq!(store.location(b).unwrap().unwrap().uid, None);
    store.set_folder_synced(travel, "{\"x\":1}", 77).unwrap();
    let f = store.folder(travel).unwrap().unwrap();
    assert_eq!(
        (f.sync_state.as_str(), f.synced_at),
        ("{\"x\":1}", Some(77))
    );
}

#[test]
fn filed_mail_is_pushed_when_it_leaves_its_folder() {
    let (mut store, account) = store_with_account();
    store
        .replace_folders(account, &[remote("Travel", None, "Travel", None)])
        .unwrap();
    let travel = folder_id(&store, "Travel");
    let id = new_id(
        store
            .add_folder_message(travel, &in_folder(account, "Travel", 1, 1))
            .unwrap(),
    );
    // Read in place: a flag change, no move.
    let read = Patch {
        unread: Some(false),
        ..Default::default()
    };
    store.update(id, &read, 1_000).unwrap();
    let p = &store.pending(account, 9_000, 0).unwrap()[0];
    assert_eq!((p.place, p.remote_place), (Place::Folder, Place::Folder));
    assert_eq!(p.mailbox.as_deref(), Some("Travel"));
    store
        .mark_pushed(
            p,
            Pushed {
                mailbox: Some("Travel"),
                uid: Some(1),
            },
        )
        .unwrap();
    assert!(store.pending(account, 9_000, 0).unwrap().is_empty());

    // Done moves it to the archive.
    let done = Patch {
        state: Some(MessageState::Done),
        ..Default::default()
    };
    store.update(id, &done, 2_000).unwrap();
    let p = &store.pending(account, 9_000, 0).unwrap()[0];
    assert_eq!((p.place, p.remote_place), (Place::Archive, Place::Folder));
    // Undone within the window: nothing to push.
    let undo = Patch {
        state: Some(MessageState::Filed),
        ..Default::default()
    };
    store.update(id, &undo, 3_000).unwrap();
    assert!(store.pending(account, 9_000, 0).unwrap().is_empty());
}

#[test]
fn graph_folder_mail_follows_the_server_and_flags() {
    let (mut store, account) = store_with_account();
    store
        .replace_folders(account, &[remote("AAMk-folder", None, "Clients", None)])
        .unwrap();
    let clients = folder_id(&store, "AAMk-folder");
    let graph = |remote_id: &str| Incoming {
        account_id: account,
        remote_id: Some(remote_id.into()),
        from_address: "a@x.com".into(),
        subject: remote_id.into(),
        unread: true,
        ..Default::default()
    };
    let inbox = new_id(store.add_incoming(&graph("g1"), false).unwrap());
    // Still in the inbox per the server: left alone.
    store.add_folder_message(clients, &graph("g1")).unwrap();
    assert_eq!(
        store.message(inbox).unwrap().unwrap().state,
        MessageState::Inbox
    );
    // Gone from the inbox, then listed in the folder: it moved there.
    store.observe_gone_remote(account, "g1").unwrap();
    store
        .add_folder_message(
            clients,
            &Incoming {
                unread: false,
                ..graph("g1")
            },
        )
        .unwrap();
    let m = store.message(inbox).unwrap().unwrap();
    assert_eq!(m.state, MessageState::Filed);
    assert!(!m.unread);
    store
        .observe_remote_flags(account, "g1", true, true)
        .unwrap();
    let m = store.message(inbox).unwrap().unwrap();
    assert!(m.unread && m.pinned);
}

fn store_with_imap_account() -> (Store, i64) {
    let store = Store::open_in_memory().unwrap();
    let account = store
        .add_account(&NewAccount {
            kind: AccountKind::Imap,
            email: "me@fastmail.com",
            name: "Me",
            label: "Fastmail",
            config: "{}",
        })
        .unwrap();
    (store, account.id)
}

#[test]
fn renaming_and_removing_folders_carry_their_subfolders() {
    let (mut store, account) = store_with_imap_account();
    store
        .replace_folders(
            account,
            &[
                remote("Projects", None, "Projects", None),
                remote("Projects/Q4", Some("Projects"), "Q4", None),
                remote("Projects2", None, "Projects2", None),
            ],
        )
        .unwrap();
    let projects = folder_id(&store, "Projects");
    let q4 = folder_id(&store, "Projects/Q4");
    let plan = new_id(
        store
            .add_folder_message(q4, &in_folder(account, "Projects/Q4", 1, 5))
            .unwrap(),
    );
    store.rename_folder(projects, "Work", "Work").unwrap();
    let f = store.folder(q4).unwrap().unwrap();
    assert_eq!(
        (f.remote_id.as_str(), f.parent_id),
        ("Work/Q4", Some(projects))
    );
    assert_eq!(store.folder(projects).unwrap().unwrap().name, "Work");
    assert_eq!(
        store
            .folder(folder_id(&store, "Projects2"))
            .unwrap()
            .unwrap()
            .remote_id,
        "Projects2"
    );
    assert_eq!(
        store.location(plan).unwrap().unwrap().mailbox.as_deref(),
        Some("Work/Q4")
    );

    // Inbox mail listed in the subfolder outlives it, as does Done mail;
    // filed mail goes with it.
    let lunch = new_id(
        store
            .add_incoming(&incoming(account, 1, "a@x.com"), false)
            .unwrap(),
    );
    store
        .add_folder_message(q4, &in_folder(account, "Work/Q4", 2, 1))
        .unwrap();
    let done = new_id(
        store
            .add_incoming(&incoming(account, 3, "a@x.com"), false)
            .unwrap(),
    );
    store.observe_gone_uids(account, "INBOX", &[3]).unwrap();
    store
        .add_folder_message(q4, &in_folder(account, "Work/Q4", 4, 3))
        .unwrap();
    let marked_done = Patch {
        state: Some(MessageState::Done),
        ..Default::default()
    };
    store.update(done, &marked_done, 1_000).unwrap();
    let p = &store.pending(account, 9_000, 0).unwrap()[0];
    store
        .mark_pushed(
            p,
            Pushed {
                mailbox: None,
                uid: None,
            },
        )
        .unwrap();
    store.remove_folder(projects).unwrap();
    assert_eq!(store.folders().unwrap().len(), 1);
    assert_eq!(store.message(plan).unwrap(), None);
    assert!(store.message(lunch).unwrap().unwrap().folders.is_empty());
    assert_eq!(
        store.message(done).unwrap().unwrap().state,
        MessageState::Done
    );
}

#[test]
fn inbox_mail_first_seen_in_a_folder_still_arrives_as_new() {
    let (mut store, account) = store_with_account();
    store
        .replace_folders(
            account,
            &[remote(
                "[Gmail]/All Mail",
                None,
                "All Mail",
                Some(FolderRole::Archive),
            )],
        )
        .unwrap();
    let all_mail = folder_id(&store, "[Gmail]/All Mail");
    let spam = new_id(
        store
            .add_incoming(&incoming(account, 2, "spam@x.com"), false)
            .unwrap(),
    );
    let blocking = Patch {
        state: Some(MessageState::Blocked),
        ..Default::default()
    };
    store.update(spam, &blocking, 1_000).unwrap();

    // All Mail syncs before the inbox: a stranger's mail and a blocked sender's.
    let in_all_mail = |uid: u32, message: u32, from: &str| Incoming {
        from_address: from.into(),
        ..in_folder(account, "[Gmail]/All Mail", uid, message)
    };
    let stranger = new_id(
        store
            .add_folder_message(all_mail, &in_all_mail(50, 10, "new@y.com"))
            .unwrap(),
    );
    let blocked = new_id(
        store
            .add_folder_message(all_mail, &in_all_mail(51, 11, "spam@x.com"))
            .unwrap(),
    );
    assert_eq!(
        store.message(stranger).unwrap().unwrap().state,
        MessageState::Done
    );

    // Then the inbox has them: new mail, screened or blocked as usual.
    let inbox = |uid: u32, message: u32, from: &str| Incoming {
        message_id: Some(format!("<{message}@example.com>")),
        ..incoming(account, uid, from)
    };
    assert_eq!(
        store
            .add_incoming(&inbox(20, 10, "new@y.com"), true)
            .unwrap(),
        Added::New(stranger)
    );
    let m = store.message(stranger).unwrap().unwrap();
    assert!(m.state == MessageState::Inbox && m.screening);
    assert_eq!(m.folders, [all_mail]);
    assert_eq!(
        store
            .add_incoming(&inbox(21, 11, "spam@x.com"), true)
            .unwrap(),
        Added::New(blocked)
    );
    assert_eq!(
        store.message(blocked).unwrap().unwrap().state,
        MessageState::Blocked
    );
    // Seen in the inbox once, it's known from then on.
    assert_eq!(
        store
            .add_incoming(&inbox(20, 10, "new@y.com"), true)
            .unwrap(),
        Added::Known(stranger)
    );
}

#[test]
fn graph_copies_with_one_message_id_stay_apart() {
    let (mut store, account) = store_with_account();
    store
        .replace_folders(
            account,
            &[remote(
                "sent-id",
                None,
                "Sent Items",
                Some(FolderRole::Sent),
            )],
        )
        .unwrap();
    let sent = folder_id(&store, "sent-id");
    let copy = |remote_id: &str| Incoming {
        account_id: account,
        remote_id: Some(remote_id.into()),
        message_id: Some("same@x.com".into()),
        from_address: "me@contoso.com".into(),
        ..Default::default()
    };
    let received = new_id(store.add_incoming(&copy("inbox-copy"), false).unwrap());
    let filed = new_id(store.add_folder_message(sent, &copy("sent-copy")).unwrap());
    assert_ne!(received, filed);
    assert_eq!(
        store.message(received).unwrap().unwrap().state,
        MessageState::Inbox
    );
    assert_eq!(
        store.message(filed).unwrap().unwrap().state,
        MessageState::Filed
    );
}

#[test]
fn version_2_databases_gain_folders() {
    let dir = std::env::temp_dir().join(format!("spark-store-v2-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("v2.sqlite");
    let _ = std::fs::remove_file(&path);
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        conn.execute_batch(SCHEMA_2).unwrap();
        conn.pragma_update(None, "user_version", 2).unwrap();
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
    assert!(
        store
            .replace_folders(account.id, &[remote("Travel", None, "Travel", None)])
            .unwrap()
    );
    drop(store);
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Records the account's one pending change as pushed: moved to `mailbox`,
/// its new UID not known yet.
fn push_to(store: &Store, account: i64, mailbox: &str) -> Pending {
    let mut pending = store.pending(account, i64::MAX, 0).unwrap();
    assert_eq!(pending.len(), 1, "{pending:?}");
    let p = pending.remove(0);
    store
        .mark_pushed(
            &p,
            Pushed {
                mailbox: Some(mailbox),
                uid: None,
            },
        )
        .unwrap();
    p
}

fn file_into(folder: i64) -> Patch {
    Patch {
        folder: Some(folder),
        ..Default::default()
    }
}

fn store_with_folders(kind: AccountKind) -> (Store, i64, i64, i64) {
    let (mut store, account) = match kind {
        AccountKind::Gmail => store_with_account(),
        _ => store_with_imap_account(),
    };
    store
        .replace_folders(
            account,
            &[
                remote("Travel", None, "Travel", None),
                remote("Receipts", None, "Receipts", None),
                remote("Archive", None, "Archive", Some(FolderRole::Archive)),
                remote("Trash", None, "Trash", Some(FolderRole::Trash)),
                remote("Junk", None, "Junk", Some(FolderRole::Spam)),
            ],
        )
        .unwrap();
    let travel = folder_id(&store, "Travel");
    let receipts = folder_id(&store, "Receipts");
    (store, account, travel, receipts)
}

#[test]
fn inbox_mail_moves_into_a_folder_and_back() {
    let (mut store, account, travel, _) = store_with_folders(AccountKind::Imap);
    let id = new_id(
        store
            .add_incoming(&incoming(account, 1, "a@x.com"), false)
            .unwrap(),
    );
    let m = store
        .update(id, &file_into(travel), 1_000)
        .unwrap()
        .unwrap();
    assert_eq!(m.state, MessageState::Filed);
    assert_eq!((m.folder, m.folders), (Some(travel), vec![travel]));

    let p = &store.pending(account, 9_000, 0).unwrap()[0];
    assert_eq!((p.place, p.remote_place), (Place::Folder, Place::Inbox));
    assert_eq!(p.folder, Some((travel, "Travel".to_owned())));
    assert_eq!((p.mailbox.as_deref(), p.uid), (Some("INBOX"), Some(1)));
    push_to(&store, account, "Travel");
    assert!(store.pending(account, 9_000, 0).unwrap().is_empty());
    let at = store.location(id).unwrap().unwrap();
    assert_eq!((at.mailbox.as_deref(), at.uid), (Some("Travel"), None));
    assert_eq!(at.remote_place, Place::Folder);

    // Travel's next sync finds it under its new UID.
    store
        .add_folder_message(travel, &in_folder(account, "Travel", 8, 1))
        .unwrap();
    let m = store.message(id).unwrap().unwrap();
    assert_eq!((m.state, m.folder), (MessageState::Filed, Some(travel)));
    assert_eq!(store.folder_uids(travel).unwrap(), HashMap::from([(8, id)]));

    // Undo after the push: back to the inbox, out of the folder.
    let back = Patch {
        state: Some(MessageState::Inbox),
        ..Default::default()
    };
    let m = store.update(id, &back, 2_000).unwrap().unwrap();
    assert_eq!((m.folder, m.folders), (None, vec![]));
    let p = &store.pending(account, 9_000, 0).unwrap()[0];
    assert_eq!((p.place, p.remote_place), (Place::Inbox, Place::Folder));
    assert_eq!((p.mailbox.as_deref(), p.uid), (Some("Travel"), Some(8)));
}

#[test]
fn filed_mail_moves_between_folders_and_undo_puts_it_back() {
    let (mut store, account, travel, receipts) = store_with_folders(AccountKind::Imap);
    let id = new_id(
        store
            .add_folder_message(travel, &in_folder(account, "Travel", 5, 1))
            .unwrap(),
    );
    // Found by a folder sync: Filed there, with its home folder.
    assert_eq!(store.message(id).unwrap().unwrap().folder, Some(travel));

    let m = store
        .update(id, &file_into(receipts), 1_000)
        .unwrap()
        .unwrap();
    assert_eq!((m.folder, m.folders), (Some(receipts), vec![receipts]));
    let p = &store.pending(account, 9_000, 0).unwrap()[0];
    assert_eq!((p.place, p.remote_place), (Place::Folder, Place::Folder));
    assert_eq!(p.folder, Some((receipts, "Receipts".to_owned())));
    assert_eq!(p.remote_folder, Some(travel));

    // Undone within the window: nothing to push, back in Travel with its UID.
    let undo = Patch {
        state: Some(MessageState::Filed),
        folder: Some(travel),
        ..Default::default()
    };
    let m = store.update(id, &undo, 2_000).unwrap().unwrap();
    assert_eq!((m.folder, m.folders), (Some(travel), vec![travel]));
    assert!(store.pending(account, 9_000, 0).unwrap().is_empty());
    assert_eq!(store.folder_uids(travel).unwrap(), HashMap::from([(5, id)]));

    // Moved and pushed, then undone: that's a move back.
    store.update(id, &file_into(receipts), 3_000).unwrap();
    push_to(&store, account, "Receipts");
    let m = store.update(id, &undo, 4_000).unwrap().unwrap();
    assert_eq!((m.folder, m.folders), (Some(travel), vec![travel]));
    let p = push_to(&store, account, "Travel");
    assert_eq!(p.folder, Some((travel, "Travel".to_owned())));
    assert_eq!(
        (p.remote_place, p.remote_folder),
        (Place::Folder, Some(receipts))
    );
    assert_eq!((p.mailbox.as_deref(), p.uid), (Some("Receipts"), None));
    assert!(store.pending(account, 9_000, 0).unwrap().is_empty());
}

#[test]
fn folder_syncs_leave_a_pending_move_alone() {
    let (mut store, account, travel, receipts) = store_with_folders(AccountKind::Imap);
    let id = new_id(
        store
            .add_folder_message(travel, &in_folder(account, "Travel", 5, 1))
            .unwrap(),
    );
    store.update(id, &file_into(receipts), 1_000).unwrap();

    // Before the push: Travel still has it, Receipts doesn't yet.
    assert_eq!(
        store
            .add_folder_message(travel, &in_folder(account, "Travel", 5, 1))
            .unwrap(),
        Added::Known(id)
    );
    store.set_folder_members(travel, &[(id, Some(5))]).unwrap();
    assert_eq!(store.set_folder_members(receipts, &[]).unwrap(), 0);
    let m = store.message(id).unwrap().unwrap();
    assert_eq!((m.state, m.folder), (MessageState::Filed, Some(receipts)));
    assert_eq!(m.folders, [receipts]);
    let p = &store.pending(account, 9_000, 0).unwrap()[0];
    assert_eq!((p.mailbox.as_deref(), p.uid), (Some("Travel"), Some(5)));

    // After it: the server says so.
    push_to(&store, account, "Receipts");
    store.set_folder_members(travel, &[]).unwrap();
    store
        .add_folder_message(receipts, &in_folder(account, "Receipts", 9, 1))
        .unwrap();
    store
        .set_folder_members(receipts, &[(id, Some(9))])
        .unwrap();
    let m = store.message(id).unwrap().unwrap();
    assert_eq!((m.folder, m.folders), (Some(receipts), vec![receipts]));
    let at = store.location(id).unwrap().unwrap();
    assert_eq!((at.mailbox.as_deref(), at.uid), (Some("Receipts"), Some(9)));
}

#[test]
fn gmail_label_moves_swap_labels_and_stay_put() {
    let (mut store, account, travel, receipts) = store_with_folders(AccountKind::Gmail);
    let all_mail = folder_id(&store, "Archive");
    // Inbox mail labelled Travel goes to Receipts: the label stays.
    let id = new_id(
        store
            .add_incoming(&incoming(account, 1, "a@x.com"), false)
            .unwrap(),
    );
    store
        .add_folder_message(travel, &in_folder(account, "Travel", 6, 1))
        .unwrap();
    let m = store
        .update(id, &file_into(receipts), 1_000)
        .unwrap()
        .unwrap();
    assert_eq!(m.folder, Some(receipts));
    let mut both = m.folders.clone();
    both.sort();
    assert_eq!(both, [travel, receipts]);

    // Syncs before the push keep both.
    store
        .add_folder_message(travel, &in_folder(account, "Travel", 6, 1))
        .unwrap();
    store.set_folder_members(travel, &[(id, Some(6))]).unwrap();
    store.set_folder_members(receipts, &[]).unwrap();
    let mut both = store.message(id).unwrap().unwrap().folders;
    both.sort();
    assert_eq!(both, [travel, receipts]);

    // The move took it out of the inbox, into Receipts. Travel and All
    // Mail still list it; neither takes it over.
    push_to(&store, account, "Receipts");
    store
        .add_folder_message(travel, &in_folder(account, "Travel", 6, 1))
        .unwrap();
    store
        .add_folder_message(all_mail, &in_folder(account, "Archive", 40, 1))
        .unwrap();
    store
        .add_folder_message(receipts, &in_folder(account, "Receipts", 3, 1))
        .unwrap();
    let m = store.message(id).unwrap().unwrap();
    assert_eq!((m.state, m.folder), (MessageState::Filed, Some(receipts)));
    let at = store.location(id).unwrap().unwrap();
    assert_eq!((at.mailbox.as_deref(), at.uid), (Some("Receipts"), Some(3)));

    // From Receipts to Travel: Receipts' label goes, Travel's stays.
    let m = store
        .update(id, &file_into(travel), 2_000)
        .unwrap()
        .unwrap();
    let mut left = m.folders.clone();
    left.sort();
    assert_eq!(left, [travel, all_mail]);
    // Receipts still has it until the push; its sync doesn't list it again.
    store
        .add_folder_message(receipts, &in_folder(account, "Receipts", 3, 1))
        .unwrap();
    store
        .set_folder_members(receipts, &[(id, Some(3))])
        .unwrap();
    assert!(
        !store
            .message(id)
            .unwrap()
            .unwrap()
            .folders
            .contains(&receipts)
    );
}

#[test]
fn undoing_done_puts_filed_mail_back_in_its_folder() {
    let (mut store, account, travel, _) = store_with_folders(AccountKind::Imap);
    let id = new_id(
        store
            .add_folder_message(travel, &in_folder(account, "Travel", 5, 1))
            .unwrap(),
    );
    let done = Patch {
        state: Some(MessageState::Done),
        ..Default::default()
    };
    let m = store.update(id, &done, 1_000).unwrap().unwrap();
    assert_eq!((m.folder, m.folders), (None, vec![]));
    let p = push_to(&store, account, "Archive");
    assert_eq!((p.place, p.remote_place), (Place::Archive, Place::Folder));

    // Undone after the push, as the UI does it or as an older one did.
    for undo in [
        Patch {
            state: Some(MessageState::Filed),
            folder: Some(travel),
            ..Default::default()
        },
        Patch {
            state: Some(MessageState::Filed),
            ..Default::default()
        },
    ] {
        let m = store.update(id, &undo, 2_000).unwrap().unwrap();
        assert_eq!((m.folder, m.folders), (Some(travel), vec![travel]));
        let p = &store.pending(account, 9_000, 0).unwrap()[0];
        assert_eq!((p.place, p.remote_place), (Place::Folder, Place::Archive));
        assert_eq!(p.folder, Some((travel, "Travel".to_owned())));
        assert_eq!((p.mailbox.as_deref(), p.uid), (Some("Archive"), None));
        store.update(id, &done, 3_000).unwrap();
    }
}

#[test]
fn filing_follows_the_folder_role_and_stays_in_the_account() {
    let (mut store, account, travel, _) = store_with_folders(AccountKind::Imap);
    let id = new_id(
        store
            .add_incoming(&incoming(account, 1, "a@x.com"), false)
            .unwrap(),
    );
    let into = |store: &mut Store, name: &str| {
        let folder = folder_id(store, name);
        store
            .update(id, &file_into(folder), 1_000)
            .unwrap()
            .unwrap()
    };
    assert_eq!(into(&mut store, "Archive").state, MessageState::Done);
    assert_eq!(into(&mut store, "Trash").state, MessageState::Deleted);
    let junk = into(&mut store, "Junk");
    assert_eq!(junk.state, MessageState::Filed);
    assert_eq!(junk.folder, Some(folder_id(&store, "Junk")));

    // Another state clears the home folder.
    let snooze = Patch {
        state: Some(MessageState::Snoozed),
        snoozed_until: Some(9_000),
        ..Default::default()
    };
    let m = store.update(id, &snooze, 2_000).unwrap().unwrap();
    assert_eq!((m.folder, m.folders), (None, vec![]));

    // Not another account's folder, nor one that only holds folders.
    let other = store
        .add_account(&NewAccount {
            kind: AccountKind::Imap,
            email: "me@other.com",
            name: "Me",
            label: "Other",
            config: "{}",
        })
        .unwrap();
    store
        .replace_folders(
            other.id,
            &[RemoteFolder {
                selectable: false,
                ..remote("Clients", None, "Clients", None)
            }],
        )
        .unwrap();
    let clients = store
        .folders()
        .unwrap()
        .into_iter()
        .find(|f| f.account_id == other.id)
        .unwrap()
        .id;
    let err = store.update(id, &file_into(clients), 3_000).unwrap_err();
    assert_eq!(
        err.to_string(),
        "An email can only be moved to a folder of its own account."
    );
    store
        .conn
        .execute("UPDATE folders SET selectable = 0 WHERE id = ?1", [travel])
        .unwrap();
    let err = store.update(id, &file_into(travel), 3_000).unwrap_err();
    assert_eq!(err.to_string(), "That folder only holds other folders.");
    assert_eq!(
        store.message(id).unwrap().unwrap().state,
        MessageState::Snoozed
    );
}

#[test]
fn version_3_databases_gain_home_folders() {
    let dir = std::env::temp_dir().join(format!("spark-store-v3-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("v3.sqlite");
    let _ = std::fs::remove_file(&path);
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        conn.execute_batch(SCHEMA_2).unwrap();
        conn.execute_batch(SCHEMA_3).unwrap();
        conn.pragma_update(None, "user_version", 3).unwrap();
        conn.execute_batch(
            "INSERT INTO accounts (id, kind, email, name, label, color, config, position)
             VALUES (1, 'imap', 'a@b.c', 'A', 'A', '#000', '{}', 0);
             INSERT INTO folders (id, account_id, remote_id, name, position)
             VALUES (10, 1, 'Travel', 'Travel', 0), (11, 1, 'Graph-id', 'Clients', 1);
             INSERT INTO messages (id, account_id, mailbox, uid, from_name, from_address,
                 to_list, cc_list, subject, snippet, date, text_body, attachments, category,
                 state, unread, pinned, remote_place, remote_unread, remote_pinned)
             VALUES
                 (1, 1, 'Travel', 5, '', '', '[]', '[]', 'IMAP', '', 0, '', '[]', 'people',
                  'filed', 0, 0, 'folder', 0, 0),
                 (2, 1, NULL, NULL, '', '', '[]', '[]', 'Graph', '', 0, '', '[]', 'people',
                  'filed', 0, 0, 'folder', 0, 0),
                 (3, 1, 'INBOX', 1, '', '', '[]', '[]', 'Inbox', '', 0, '', '[]', 'people',
                  'inbox', 0, 0, 'inbox', 0, 0);
             INSERT INTO folder_members (folder_id, message_id, uid)
             VALUES (10, 1, 5), (11, 2, NULL), (10, 3, 7);",
        )
        .unwrap();
    }
    let store = Store::open(&path).unwrap();
    let folder = |id: i64| store.message(id).unwrap().unwrap().folder;
    assert_eq!(
        (folder(1), folder(2), folder(3)),
        (Some(10), Some(11), None)
    );
    assert!(store.pending(1, i64::MAX, 0).unwrap().is_empty());
    drop(store);
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Message `uid`, replying to the messages `replies_to` (oldest first).
fn reply(account_id: i64, uid: u32, subject: &str, replies_to: &[u32]) -> Incoming {
    let ids: Vec<String> = replies_to
        .iter()
        .map(|r| format!("{r}@example.com"))
        .collect();
    Incoming {
        subject: subject.into(),
        in_reply_to: ids.last().cloned(),
        references: Some(ids.join(" ")).filter(|r| !r.is_empty()),
        ..incoming(account_id, uid, "ana@example.com")
    }
}

fn conversation(store: &Store, id: i64) -> i64 {
    store.message(id).unwrap().unwrap().conversation
}

fn add(store: &mut Store, m: Incoming) -> i64 {
    new_id(store.add_incoming(&m, false).unwrap())
}

#[test]
fn replies_join_the_conversation_of_what_they_answer() {
    let (mut store, account) = store_with_account();
    let plan = add(&mut store, reply(account, 1, "Plan", &[]));
    let answer = add(&mut store, reply(account, 2, "Re: Plan", &[1]));
    let forward = add(&mut store, reply(account, 3, "Fwd: RE: Plan", &[1, 2]));
    let lunch = add(&mut store, reply(account, 4, "Lunch?", &[]));
    // Replying to the plan about something else starts a conversation of its own.
    let elsewhere = add(&mut store, reply(account, 5, "Re: Budget", &[1, 2]));

    assert_eq!(conversation(&store, plan), plan);
    assert_eq!(conversation(&store, answer), plan);
    assert_eq!(conversation(&store, forward), plan);
    assert_eq!(conversation(&store, lunch), lunch);
    assert_eq!(conversation(&store, elsewhere), elsewhere);
    let listed = store.messages().unwrap();
    assert!(
        listed
            .iter()
            .any(|m| m.id == answer && m.conversation == plan)
    );
}

#[test]
fn mail_that_links_two_conversations_merges_them() {
    let (mut store, account) = store_with_account();
    // Replies to messages that aren't stored, so nothing links them yet.
    let first = add(&mut store, reply(account, 5, "Re: Plan", &[9]));
    let second = add(&mut store, reply(account, 6, "Re: Plan", &[8]));
    assert_ne!(conversation(&store, first), conversation(&store, second));

    let both = add(&mut store, reply(account, 7, "Re: Plan", &[8, 9]));
    for id in [first, second, both] {
        assert_eq!(conversation(&store, id), first);
    }
}

#[test]
fn server_conversations_group_mail_within_one_account() {
    let (mut store, account) = store_with_account();
    let other = store
        .add_account(&NewAccount {
            kind: AccountKind::Microsoft,
            email: "me@outlook.com",
            name: "Me",
            label: "Work",
            config: "{}",
        })
        .unwrap()
        .id;
    let graph = |account_id: i64, uid: u32, subject: &str| Incoming {
        message_id: None,
        thread_id: Some("AAQkADAwATM".into()),
        subject: subject.into(),
        ..incoming(account_id, uid, "ana@example.com")
    };
    let plan = add(&mut store, graph(account, 1, "Plan"));
    // The server keeps a renamed reply in the conversation.
    let renamed = add(&mut store, graph(account, 2, "Updated plan"));
    let other_account = add(&mut store, graph(other, 3, "Plan"));

    assert_eq!(conversation(&store, renamed), plan);
    assert_eq!(conversation(&store, other_account), other_account);
}

#[test]
fn version_5_databases_gain_conversations() {
    let dir = std::env::temp_dir().join(format!("spark-store-v5-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("v5.sqlite");
    let _ = std::fs::remove_file(&path);
    {
        let conn = Connection::open(&path).unwrap();
        for schema in [SCHEMA, SCHEMA_2, SCHEMA_3, SCHEMA_4, drafts::SCHEMA_DRAFTS] {
            conn.execute_batch(schema).unwrap();
        }
        conn.pragma_update(None, "user_version", 5).unwrap();
        conn.execute_batch(
            "INSERT INTO accounts (id, kind, email, name, label, color, config, position)
             VALUES (1, 'imap', 'a@b.c', 'A', 'A', '#000', '{}', 0);
             INSERT INTO messages (id, account_id, message_id, in_reply_to, refs, from_name,
                 from_address, to_list, cc_list, subject, snippet, date, text_body, attachments,
                 category, state, unread, pinned, remote_place, remote_unread, remote_pinned)
             VALUES
                 (1, 1, '1@x.com', NULL, NULL, '', '', '[]', '[]', 'Plan', '', 0, '', '[]',
                  'people', 'inbox', 0, 0, 'inbox', 0, 0),
                 (2, 1, '2@x.com', NULL, NULL, '', '', '[]', '[]', 'Lunch', '', 0, '', '[]',
                  'people', 'inbox', 0, 0, 'inbox', 0, 0),
                 (3, 1, '3@x.com', '1@x.com', '1@x.com', '', '', '[]', '[]', 'Re: Plan', '', 0,
                  '', '[]', 'people', 'inbox', 0, 0, 'inbox', 0, 0);",
        )
        .unwrap();
    }
    let mut store = Store::open(&path).unwrap();
    assert_eq!([1, 2, 3].map(|id| conversation(&store, id)), [1, 2, 1]);
    // Mail arriving later still finds the old conversations.
    let later = add(&mut store, reply(1, 4, "Re: Lunch", &[]));
    assert_eq!(conversation(&store, later), later);
    let threaded = Incoming {
        in_reply_to: Some("2@x.com".into()),
        references: Some("2@x.com".into()),
        ..reply(1, 5, "Re: Lunch", &[])
    };
    let answer = add(&mut store, threaded);
    assert_eq!(conversation(&store, answer), 2);
    drop(store);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn reply_to_refreshes_full_headers_but_survives_flags_only_updates() {
    let (mut store, account) = store_with_account();
    let mut fetched = incoming(account, 1, "no-reply@example.test");
    fetched.reply_to = Some(vec!["Help <help@example.test>".into()]);
    let id = new_id(store.add_incoming(&fetched, false).unwrap());
    assert_eq!(
        store.message(id).unwrap().unwrap().reply_to,
        ["Help <help@example.test>"]
    );

    fetched.reply_to = None;
    fetched.unread = false;
    assert_eq!(
        store.add_incoming(&fetched, false).unwrap(),
        Added::Known(id)
    );
    assert_eq!(
        store.message(id).unwrap().unwrap().reply_to,
        ["Help <help@example.test>"]
    );
    assert!(!store.message(id).unwrap().unwrap().unread);

    fetched.reply_to = Some(vec!["New Help <new-help@example.test>".into()]);
    assert!(store.observe_reply_to(&fetched).unwrap());
    assert!(!store.observe_reply_to(&fetched).unwrap());
    assert_eq!(
        store.content(id).unwrap().unwrap().reply_to,
        ["New Help <new-help@example.test>"]
    );
    fetched.reply_to = Some(Vec::new());
    store.add_incoming(&fetched, false).unwrap();
    assert!(store.message(id).unwrap().unwrap().reply_to.is_empty());
    assert_eq!(
        store.message(id).unwrap().unwrap().from_address,
        "no-reply@example.test"
    );
}

#[test]
fn version_11_databases_gain_reply_to_and_draft_options_without_losing_mail() {
    let dir = std::env::temp_dir().join(format!("spark-store-v11-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("v11.sqlite");
    let _ = std::fs::remove_file(&path);
    {
        let conn = Connection::open(&path).unwrap();
        for schema in [
            SCHEMA,
            SCHEMA_2,
            SCHEMA_3,
            SCHEMA_4,
            drafts::SCHEMA_DRAFTS,
            conversations::SCHEMA_CONVERSATIONS,
            SCHEMA_7,
            calendar::SCHEMA_CALENDAR,
            search::SCHEMA_SEARCH,
            signatures::SCHEMA_SIGNATURES,
            contacts::SCHEMA_CONTACTS,
        ] {
            conn.execute_batch(schema).unwrap();
        }
        conn.pragma_update(None, "user_version", 11).unwrap();
        conn.execute_batch(
            "INSERT INTO accounts (id, kind, email, name, label, color, config, position)
             VALUES (1, 'imap', 'me@example.test', 'Me', 'Test', '#000', '{}', 0);
             INSERT INTO messages (id, account_id, from_name, from_address, to_list, cc_list,
                 subject, snippet, date, text_body, attachments, category, state, unread, pinned,
                 remote_place, remote_unread, remote_pinned)
             VALUES (1, 1, 'Sender', 'sender@example.test', '[]', '[]', 'Saved mail', '', 0,
                 'Still here', '[]', 'people', 'inbox', 0, 0, 'inbox', 0, 0);",
        )
        .unwrap();
    }
    {
        let store = Store::open(&path).unwrap();
        let message = store.message(1).unwrap().unwrap();
        assert_eq!(message.subject, "Saved mail");
        assert!(message.reply_to.is_empty());
        assert_eq!(store.content(1).unwrap().unwrap().text, "Still here");
        assert!(store.content(1).unwrap().unwrap().reply_to.is_empty());
        assert!(store.draft_send_later(1).unwrap().is_none());
    }
    // Reopening a migrated database must not run the DDL twice.
    assert!(Store::open(&path).unwrap().message(1).unwrap().is_some());
    std::fs::remove_dir_all(&dir).unwrap();
}
