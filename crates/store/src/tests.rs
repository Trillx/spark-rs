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
    let ids = |text: &str| found(&store, text);
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

/// The ids `search` finds, newest first, after checking that `search_ids`
/// finds the same ones.
fn found(store: &Store, text: &str) -> Vec<i64> {
    let found: Vec<i64> = store
        .search(text)
        .unwrap()
        .into_iter()
        .map(|m| m.id)
        .collect();
    let mut ids = store.search_ids(text).unwrap();
    ids.sort_unstable();
    let mut sorted = found.clone();
    sorted.sort_unstable();
    assert_eq!(ids, sorted, "search_ids({text:?})");
    found
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
fn search_ids_come_from_the_index_alone() {
    let (store, account) = store_with_account();
    // A big mailbox where a short word finds nearly everything, one
    // message has a word only in its text, and one is a draft being sent.
    store
        .conn
        .execute(
            "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 50000)
             INSERT INTO messages (account_id, remote_id, from_name, from_address, to_list,
                 cc_list, subject, snippet, date, text_body, attachments, category, state,
                 unread, pinned, remote_place, remote_unread, remote_pinned)
             SELECT ?1, 'r' || i, 'Sender', 'sender@example.test', '[]', '[]',
                 'Weekly report ' || i, '', i,
                 CASE WHEN i = 7 THEN 'The bracket ships Friday.' ELSE '' END,
                 '[]', 'people', 'inbox', 0, 0, 'inbox', 0, 0 FROM n",
            [account],
        )
        .unwrap();
    store
        .conn
        .execute(
            "INSERT INTO outbox (account_id, draft, send_at, draft_message)
             VALUES (?1, '{}', 0, 9)",
            [account],
        )
        .unwrap();
    assert_eq!(found(&store, "bracket"), [7]);
    // Every hit, not a page of them.
    let mut weekly = store.search_ids("we").unwrap();
    weekly.sort_unstable();
    assert_eq!(weekly, (1..=50_000).filter(|&i| i != 9).collect::<Vec<_>>());
    assert_eq!(found(&store, "we").len(), 49_999);

    // No sorting by date, and no folder lookups or columns per message:
    // each hit is one look-up of its id.
    let query = search::fts_query("we").unwrap();
    let (plan, _, sorts, steps) = run_counted(&store, search::SEARCH_IDS, &[query.as_str()]);
    assert!(
        plan.contains("SEARCH messages USING INTEGER PRIMARY KEY (rowid=?)"),
        "{plan}"
    );
    assert!(
        !plan.contains("TEMP B-TREE") && !plan.contains("CORRELATED"),
        "{plan}"
    );
    assert_eq!(sorts, 0, "{plan}");
    // About 20 per hit; with the columns and the date sort it was 90.
    assert!(steps < 30 * 50_000, "{steps} VM steps: {plan}");
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

fn decide(store: &mut Store, screen: Screen, decision: Option<Decision>, now: i64) -> Vec<Change> {
    store.decide(&[Change { screen, decision }], now).unwrap()
}

fn sender(address: &str) -> Screen {
    Screen::Sender(address.into())
}

/// Stores new mail from `from` after the first sync, and returns it.
fn arrive(store: &mut Store, account: i64, uid: u32, from: &str) -> Message {
    let id = new_id(
        store
            .add_incoming(&incoming(account, uid, from), true)
            .unwrap(),
    );
    store.message(id).unwrap().unwrap()
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

    // Accepting the sender takes all their mail out of the Gatekeeper.
    let undo = decide(
        &mut store,
        sender("New@x.com"),
        Some(Decision::Accepted),
        10,
    );
    assert_eq!(
        undo,
        [Change {
            screen: sender("New@x.com"),
            decision: None
        }]
    );
    assert!(!store.message(fresh2).unwrap().unwrap().screening);
    let later = new_id(
        store
            .add_incoming(&incoming(account, 5, "new@x.com"), true)
            .unwrap(),
    );
    assert!(!store.message(later).unwrap().unwrap().screening);

    // Undoing the accept puts the sender back in the Gatekeeper.
    store.decide(&undo, 11).unwrap();
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
    store.accept_sender("S@x.com", 12).unwrap();
    assert!(!store.message(stranger).unwrap().unwrap().screening);
}

#[test]
fn decisions_are_for_every_account() {
    let (mut store, work) = store_with_account();
    let home = store
        .add_account(&NewAccount {
            kind: AccountKind::Imap,
            email: "me@example.org",
            name: "Me",
            label: "Home",
            config: "{}",
        })
        .unwrap()
        .id;
    let at_work = arrive(&mut store, work, 1, "ana@x.com").id;
    let at_home = arrive(&mut store, home, 1, "ana@x.com").id;
    decide(
        &mut store,
        sender("ana@x.com"),
        Some(Decision::Accepted),
        10,
    );
    assert!(!store.message(at_work).unwrap().unwrap().screening);
    assert!(!store.message(at_home).unwrap().unwrap().screening);
    assert!(!arrive(&mut store, home, 2, "ana@x.com").screening);
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
    let undo = decide(
        &mut store,
        sender("spam@x.com"),
        Some(Decision::Blocked),
        10,
    );
    let blocked = store.message(b).unwrap().unwrap();
    assert_eq!(
        (blocked.state, blocked.screening),
        (MessageState::Blocked, false)
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

    // Their mail moves to the Blocked folder.
    let pending = store.pending(account, now_ms() + 1, 0).unwrap();
    let mut moving: Vec<(i64, Place)> = pending.iter().map(|p| (p.id, p.place)).collect();
    moving.sort_by_key(|(id, _)| *id);
    assert_eq!(
        moving,
        [
            (a, Place::Blocked),
            (b, Place::Blocked),
            (c, Place::Blocked)
        ]
    );

    // Undone in time, none of it leaves the inbox: it waits in the Gatekeeper again.
    store.decide(&undo, 11).unwrap();
    for id in [a, c] {
        let back = store.message(id).unwrap().unwrap();
        assert_eq!((back.state, back.screening), (MessageState::Inbox, true));
    }
    assert!(store.pending(account, now_ms() + 1, 0).unwrap().is_empty());

    decide(
        &mut store,
        sender("spam@x.com"),
        Some(Decision::Blocked),
        12,
    );
    for p in &store.pending(account, now_ms() + 1, 0).unwrap() {
        store
            .mark_pushed(
                p,
                Pushed {
                    mailbox: Some("Blocked"),
                    uid: None,
                },
            )
            .unwrap();
    }
    assert!(store.pending(account, now_ms() + 1, 0).unwrap().is_empty());

    // Clearing out the Blocked list keeps the sender blocked, and so does
    // moving one email back to the Inbox.
    let trash = Patch {
        state: Some(MessageState::Deleted),
        ..Default::default()
    };
    store.update(a, &trash, 20).unwrap();
    let inbox = Patch {
        state: Some(MessageState::Inbox),
        ..Default::default()
    };
    store.update(b, &inbox, 21).unwrap();
    let d = new_id(
        store
            .add_incoming(&incoming(account, 4, "spam@x.com"), true)
            .unwrap(),
    );
    assert_eq!(
        store.message(d).unwrap().unwrap().state,
        MessageState::Blocked
    );
    assert_eq!(
        store.sender_status("SPAM@x.com").unwrap(),
        SenderStatus {
            sender: Some(Decision::Blocked),
            domain: None,
            public_domain: false,
        }
    );

    // Accepting them brings their blocked mail back to the Inbox; trashed mail stays.
    decide(
        &mut store,
        sender("spam@x.com"),
        Some(Decision::Accepted),
        30,
    );
    let d = store.message(d).unwrap().unwrap();
    assert_eq!((d.state, d.screening), (MessageState::Inbox, false));
    assert_eq!(
        store.message(a).unwrap().unwrap().state,
        MessageState::Deleted
    );
    // What reached the Blocked folder goes back; what never left the inbox stays put.
    let moving: Vec<(i64, Place)> = store
        .pending(account, now_ms() + 1, 0)
        .unwrap()
        .iter()
        .map(|p| (p.id, p.place))
        .collect();
    assert!(moving.contains(&(c, Place::Inbox)), "{moving:?}");
    assert!(!moving.iter().any(|(id, _)| *id == d.id));
}

#[test]
fn domains_are_decided_as_a_whole_with_senders_as_exceptions() {
    let (mut store, account) = store_with_account();
    let ana = arrive(&mut store, account, 1, "ana@spam.biz").id;
    let bob = arrive(&mut store, account, 2, "bob@spam.biz").id;
    let elsewhere = arrive(&mut store, account, 3, "ana@other.biz").id;
    decide(
        &mut store,
        sender("bob@spam.biz"),
        Some(Decision::Accepted),
        5,
    );

    let undo = decide(
        &mut store,
        Screen::Domain("@Spam.biz".into()),
        Some(Decision::Blocked),
        10,
    );
    assert_eq!(
        store.message(ana).unwrap().unwrap().state,
        MessageState::Blocked
    );
    assert_eq!(
        store.message(bob).unwrap().unwrap().state,
        MessageState::Inbox
    );
    assert!(store.message(elsewhere).unwrap().unwrap().screening);
    let carl = arrive(&mut store, account, 4, "carl@spam.biz").id;
    assert_eq!(
        store.message(carl).unwrap().unwrap().state,
        MessageState::Blocked
    );
    assert_eq!(
        store.sender_status("ana@spam.biz").unwrap(),
        SenderStatus {
            sender: None,
            domain: Some(Decision::Blocked),
            public_domain: false,
        }
    );

    // Accepting one sender lets them through the blocked domain.
    decide(
        &mut store,
        sender("ana@spam.biz"),
        Some(Decision::Accepted),
        11,
    );
    assert_eq!(
        store.message(ana).unwrap().unwrap().state,
        MessageState::Inbox
    );
    assert_eq!(
        store.message(carl).unwrap().unwrap().state,
        MessageState::Blocked
    );

    store.decide(&undo, 12).unwrap();
    let carl = store.message(carl).unwrap().unwrap();
    assert_eq!((carl.state, carl.screening), (MessageState::Inbox, true));

    // Free mail providers can't be decided as a whole.
    let public = store.decide(
        &[Change {
            screen: Screen::Domain("gmail.com".into()),
            decision: Some(Decision::Blocked),
        }],
        13,
    );
    assert!(public.is_err());
}

#[test]
fn colleagues_correspondents_and_big_companies_skip_the_gatekeeper() {
    let mut store = Store::open_in_memory().unwrap();
    let account = store
        .add_account(&NewAccount {
            kind: AccountKind::Microsoft,
            email: "me@contoso.com",
            name: "Me",
            label: "Work",
            config: "{}",
        })
        .unwrap()
        .id;
    let screened =
        |store: &mut Store, uid: u32, from: &str| arrive(store, account, uid, from).screening;
    assert!(!screened(&mut store, 1, "Boss@Contoso.com"), "a colleague");
    assert!(
        !screened(&mut store, 2, "noreply@github.com"),
        "a well-known company"
    );
    assert!(
        !screened(&mut store, 3, "no-reply@accounts.google.com"),
        "a well-known company's subdomain"
    );
    assert!(screened(&mut store, 4, "someone@fabrikam.com"));

    // Someone the user wrote to from another client, found in Sent.
    store
        .replace_folders(
            account,
            &[RemoteFolder {
                remote_id: "Sent Items".into(),
                parent: None,
                name: "Sent Items".into(),
                role: Some(FolderRole::Sent),
                selectable: true,
            }],
        )
        .unwrap();
    let sent = Incoming {
        from_address: "me@contoso.com".into(),
        to: vec!["Dana <Dana@Fabrikam.com>".into()],
        ..in_folder(account, "Sent Items", 9, 9)
    };
    store
        .add_folder_message(folder_id(&store, "Sent Items"), &sent)
        .unwrap();
    assert!(!screened(&mut store, 5, "dana@fabrikam.com"));
    assert!(screened(&mut store, 6, "dan@fabrikam.com"));

    // Mail in the inbox that claims to be from the user proves nothing.
    let forged = Incoming {
        from_address: "me@contoso.com".into(),
        to: vec!["stranger@vendor.org".into()],
        ..incoming(account, 10, "me@contoso.com")
    };
    store.add_incoming(&forged, true).unwrap();
    assert!(screened(&mut store, 11, "stranger@vendor.org"));
}

#[test]
fn turning_the_gatekeeper_off_accepts_everyone_waiting() {
    let (mut store, account) = store_with_account();
    assert_eq!(
        store.gatekeeper_mode().unwrap(),
        GatekeeperMode::BeforeInbox
    );
    let waiting = arrive(&mut store, account, 1, "a@x.com").id;
    store
        .set_gatekeeper_mode(GatekeeperMode::InsideInbox)
        .unwrap();
    assert!(
        store.message(waiting).unwrap().unwrap().screening,
        "inside the inbox, still new"
    );

    store.set_gatekeeper_mode(GatekeeperMode::Off).unwrap();
    assert_eq!(store.gatekeeper_mode().unwrap(), GatekeeperMode::Off);
    assert!(!store.message(waiting).unwrap().unwrap().screening);
    assert!(!arrive(&mut store, account, 2, "b@x.com").screening);

    // Back on, only senders new from then are screened.
    store
        .set_gatekeeper_mode(GatekeeperMode::BeforeInbox)
        .unwrap();
    assert!(!arrive(&mut store, account, 3, "b@x.com").screening);
    assert!(arrive(&mut store, account, 4, "c@x.com").screening);
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
    assert!(store.wake_snoozed(4_999).unwrap().is_empty());
    assert_eq!(store.wake_snoozed(5_000).unwrap(), [id]);
    let m = store.message(id).unwrap().unwrap();
    assert_eq!(m.state, MessageState::Inbox);
    assert!(m.unread);
    assert_eq!(m.snoozed_until, None);
    assert_eq!(m.returned_at, Some(5_000));
}

/// How SQLite runs `sql` with `params`: its query plan, and the rows it
/// stepped through in full scans, its sorts and its VM steps.
fn run_counted<T: rusqlite::ToSql>(
    store: &Store,
    sql: &str,
    params: &[T],
) -> (String, i32, i32, i32) {
    use rusqlite::StatementStatus::{FullscanStep, Sort, VmStep};
    let params = rusqlite::params_from_iter(params);
    let plan = store
        .conn
        .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
        .unwrap()
        .query_map(params.clone(), |r| r.get::<_, String>(3))
        .unwrap()
        .collect::<Result<Vec<_>>>()
        .unwrap()
        .join("; ");
    let mut stmt = store.conn.prepare(sql).unwrap();
    let mut rows = stmt.query(params).unwrap();
    while rows.next().unwrap().is_some() {}
    drop(rows);
    (
        plan,
        stmt.get_status(FullscanStep),
        stmt.get_status(Sort),
        stmt.get_status(VmStep),
    )
}

#[test]
fn idle_timer_checks_only_look_at_pending_and_snoozed_mail() {
    let (mut store, account) = store_with_account();
    // A big mailbox with nothing to push and nothing snoozed.
    store
        .conn
        .execute(
            "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 50000)
             INSERT INTO messages (account_id, remote_id, from_name, from_address, to_list,
                 cc_list, subject, snippet, date, text_body, attachments, category, state,
                 unread, pinned, remote_place, remote_unread, remote_pinned)
             SELECT ?1, 'r' || i, 'Sender', 'sender@example.test', '[]', '[]', 'Message', '',
                 i, '', '[]', 'people', 'inbox', 0, 0, 'inbox', 0, 0 FROM n",
            [account],
        )
        .unwrap();
    let pending = format!("{PENDING_SELECT} {PENDING_SETTLED}");
    let check = |store: &Store, now: i64| {
        let (plan, scanned, sorts, steps) = run_counted(store, &pending, &[account, now]);
        assert!(
            plan.contains("USING INDEX messages_pending (account_id=? AND changed_at<?)"),
            "{plan}"
        );
        assert!(
            !plan.contains("SCAN") && !plan.contains("TEMP B-TREE"),
            "{plan}"
        );
        assert_eq!((scanned, sorts), (0, 0), "{plan}");
        assert!(steps < 1_000, "{steps} VM steps: {plan}");

        let (plan, scanned, _, steps) = run_counted(store, &WAKE_SNOOZED, &[now]);
        assert!(
            plan.contains("INDEX messages_due_snooze (snoozed_until>? AND snoozed_until<?)"),
            "{plan}"
        );
        assert!(!plan.contains("SCAN"), "{plan}");
        assert_eq!(scanned, 0, "{plan}");
        assert!(steps < 1_000, "{steps} VM steps: {plan}");
    };
    // The app never runs ANALYZE, so both with and without statistics.
    check(&store, 100_000);
    store.conn.execute_batch("ANALYZE").unwrap();
    check(&store, 100_000);

    // A few changes waiting to be pushed, some still in the undo window,
    // and a due and a future snooze.
    for (id, at) in [(10, 3_000), (20, 1_000), (30, 2_000)] {
        let read = Patch {
            unread: Some(true),
            ..Default::default()
        };
        store.update(id, &read, at).unwrap();
    }
    for (id, until) in [(40, 5_000), (50, 9_000)] {
        let snooze = Patch {
            state: Some(MessageState::Snoozed),
            snoozed_until: Some(until),
            ..Default::default()
        };
        store.update(id, &snooze, 1_000).unwrap();
    }
    let ids = |pending: Vec<Pending>| pending.into_iter().map(|p| p.id).collect::<Vec<_>>();
    check(&store, 4_000);
    store.conn.execute_batch("ANALYZE").unwrap();
    check(&store, 4_000);
    assert_eq!(ids(store.pending(account, 3_500, 1_000).unwrap()), [20, 30]);
    assert_eq!(
        ids(store.pending(account, 4_000, 1_000).unwrap()),
        [20, 30, 10]
    );

    assert_eq!(store.wake_snoozed(6_000).unwrap(), [40]);
    let woken = store.message(40).unwrap().unwrap();
    assert_eq!(woken.state, MessageState::Inbox);
    assert!(woken.unread);
    assert_eq!(woken.returned_at, Some(5_000));
    assert_eq!(
        store.message(50).unwrap().unwrap().state,
        MessageState::Snoozed
    );
    // Waking marks it unread, which is pushed like any other change.
    assert_eq!(
        ids(store.pending(account, 6_500, 1_000).unwrap()),
        [20, 30, 10]
    );
    assert_eq!(
        ids(store.pending(account, 7_000, 1_000).unwrap()),
        [20, 30, 10, 40]
    );
    check(&store, 10_000);
}

#[test]
fn muting_covers_the_whole_conversation() {
    let (mut store, account) = store_with_account();
    let first = new_id(
        store
            .add_incoming(&incoming(account, 1, "a@x.com"), false)
            .unwrap(),
    );
    let reply = Incoming {
        in_reply_to: Some("<1@example.com>".into()),
        subject: "Re: Message 1".into(),
        ..incoming(account, 2, "b@x.com")
    };
    let second = new_id(store.add_incoming(&reply, false).unwrap());
    let conversation = store.message(first).unwrap().unwrap().conversation;
    assert_eq!(
        store.message(second).unwrap().unwrap().conversation,
        conversation
    );

    store.set_muted(conversation, true).unwrap();
    assert!(store.message(first).unwrap().unwrap().muted);
    assert!(store.message(second).unwrap().unwrap().muted);
    store.set_muted(conversation, false).unwrap();
    assert!(!store.message(second).unwrap().unwrap().muted);

    // Mail that joins a muted conversation to another keeps it muted.
    let plan = |uid, in_reply_to: Option<&str>, references: Option<&str>| Incoming {
        subject: "Re: Plan".into(),
        in_reply_to: in_reply_to.map(Into::into),
        references: references.map(Into::into),
        ..incoming(account, uid, "c@x.com")
    };
    let a = new_id(store.add_incoming(&plan(10, None, None), false).unwrap());
    let b = new_id(
        store
            .add_incoming(&plan(11, Some("<99@example.com>"), None), false)
            .unwrap(),
    );
    let muted = store.message(b).unwrap().unwrap().conversation;
    assert_ne!(store.message(a).unwrap().unwrap().conversation, muted);
    store.set_muted(muted, true).unwrap();
    let c = new_id(
        store
            .add_incoming(
                &plan(12, Some("<11@example.com>"), Some("<10@example.com>")),
                false,
            )
            .unwrap(),
    );
    for id in [a, b, c] {
        assert!(store.message(id).unwrap().unwrap().muted, "message {id}");
    }
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
        own: true,
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
    assert!(before.iter().all(|c| c.own));
    store
        .replace_events(before[1].id, 0, 100, &[event("x", 10, 20)])
        .unwrap();

    let shared = RemoteCalendar {
        own: false,
        ..calendar("a", "Work")
    };
    store.replace_calendars(account, &[shared]).unwrap();
    let after = store.calendars().unwrap();
    assert_eq!(after.len(), 1);
    assert!(!after[0].own, "a refresh says whose it is");
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
fn what_a_sync_couldnt_fetch_is_filled_in_later_and_nothing_else() {
    let mut store = Store::open_in_memory().unwrap();
    let account = store
        .add_account(&NewAccount {
            kind: AccountKind::Microsoft,
            email: "me@contoso.com",
            name: "Me",
            label: "Work",
            config: "{}",
        })
        .unwrap()
        .id;
    let graph = |n: u32| Incoming {
        remote_id: Some(format!("r{n}")),
        mailbox: None,
        uid: None,
        ..incoming(account, n, "ana@contoso.com")
    };
    let id = new_id(store.add_incoming(&graph(1), false).unwrap());
    // Read, flagged and Done here, not sent yet.
    let changed = Patch {
        unread: Some(false),
        pinned: Some(true),
        state: Some(MessageState::Done),
        ..Default::default()
    };
    store.update(id, &changed, 5_000).unwrap();
    let before = store.message(id).unwrap().unwrap();

    let plan = Attachment {
        name: "plan.pdf".into(),
        mime: "application/pdf".into(),
        size: 10,
        remote_id: Some("a1".into()),
    };
    let invite = r#"{"kind":"invitation"}"#;
    assert!(
        store
            .fill_in_remote(account, "r1", std::slice::from_ref(&plan), Some(invite))
            .unwrap()
    );
    let content = store.content(id).unwrap().unwrap();
    assert_eq!(content.attachments, std::slice::from_ref(&plan));
    assert_eq!(content.invite.as_deref(), Some(invite));
    let after = store.message(id).unwrap().unwrap();
    assert_eq!(after.attachment.as_deref(), Some("plan.pdf"));
    assert_eq!(after.invite_kind.as_deref(), Some("invitation"));
    assert_eq!(
        (after.unread, after.pinned, after.state),
        (before.unread, before.pinned, before.state),
        "the user's changes stay"
    );
    assert_eq!(store.pending_for(id).unwrap().map(|p| p.id), Some(id));

    // Nothing to fill in leaves what's there; another account's or a
    // message not stored isn't.
    assert!(store.fill_in_remote(account, "r1", &[], None).unwrap());
    let content = store.content(id).unwrap().unwrap();
    assert_eq!(content.attachments, std::slice::from_ref(&plan));
    assert_eq!(content.invite.as_deref(), Some(invite));
    assert!(
        !store
            .fill_in_remote(account, "r9", std::slice::from_ref(&plan), None)
            .unwrap()
    );
    assert!(
        !store
            .fill_in_remote(account + 1, "r1", &[plan], None)
            .unwrap()
    );
}

/// A message as the background download stores it: no body, its preview
/// as its text, a file and an invite as its listing told of them.
fn head(account_id: i64, uid: u32) -> Incoming {
    Incoming {
        snippet: "Quarterly figures attached".into(),
        text: "Quarterly figures attached".into(),
        attachments: vec![Attachment {
            name: "figures.xlsx".into(),
            ..Default::default()
        }],
        invite: Some(r#"{"kind":"invitation"}"#.into()),
        body: Body::Missing,
        ..incoming(account_id, uid, "ana@parts.com")
    }
}

/// `head`, fetched in full.
fn whole(account_id: i64, uid: u32) -> Incoming {
    Incoming {
        snippet: "Quarterly figures attached, see the totals tab".into(),
        text: "Quarterly figures attached, see the totals tab for margins.".into(),
        html: Some("<p>Quarterly figures attached, see the <b>totals</b> tab</p>".into()),
        attachments: vec![Attachment {
            name: "figures.xlsx".into(),
            mime: "application/vnd.ms-excel".into(),
            size: 2_048,
            remote_id: None,
        }],
        invite: None,
        body: Body::Whole,
        ..head(account_id, uid)
    }
}

#[test]
fn mail_stored_without_its_body_gets_it_once_and_keeps_the_users_changes() {
    let (mut store, account) = store_with_account();
    let id = new_id(store.add_incoming(&head(account, 1), false).unwrap());
    let content = store.content(id).unwrap().unwrap();
    assert!(content.body_missing);
    assert_eq!(
        (content.text.as_str(), content.html.as_deref()),
        ("Quarterly figures attached", None)
    );
    // The row looks as it would whole: a file, an invitation, the preview.
    let row = store.message(id).unwrap().unwrap();
    assert_eq!(row.attachment.as_deref(), Some("figures.xlsx"));
    assert_eq!(row.invite_kind.as_deref(), Some("invitation"));
    // Search has the sender, subject and preview, but not the body yet.
    assert_eq!(found(&store, "quarterly"), [id]);
    assert_eq!(found(&store, "parts.com"), [id]);
    assert!(found(&store, "margins").is_empty());

    let changed = Patch {
        unread: Some(false),
        pinned: Some(true),
        state: Some(MessageState::Done),
        ..Default::default()
    };
    store.update(id, &changed, 5_000).unwrap();
    let before = store.message(id).unwrap().unwrap();

    // A fetch without the body fills in nothing.
    assert!(!store.fill_in_body(id, &head(account, 1)).unwrap());
    assert!(store.content(id).unwrap().unwrap().body_missing);

    assert!(store.fill_in_body(id, &whole(account, 1)).unwrap());
    let content = store.content(id).unwrap().unwrap();
    assert!(!content.body_missing);
    assert!(content.text.ends_with("for margins."));
    assert!(content.html.unwrap().contains("<b>totals</b>"));
    assert_eq!(content.attachments, whole(account, 1).attachments);
    assert_eq!(content.invite, None, "the listing's invite was only a hint");
    let after = store.message(id).unwrap().unwrap();
    assert_eq!(
        after.snippet,
        "Quarterly figures attached, see the totals tab"
    );
    assert_eq!(after.invite_kind, None);
    assert_eq!(
        (after.unread, after.pinned, after.state),
        (before.unread, before.pinned, before.state),
        "the user's changes stay"
    );
    assert_eq!(store.pending_for(id).unwrap().map(|p| p.id), Some(id));
    assert_eq!(found(&store, "margins"), [id]);
    check_search_index(&store);

    // Once it has its body, it's left as it is.
    let mut later = whole(account, 1);
    later.text = "Something else".into();
    assert!(!store.fill_in_body(id, &later).unwrap());
    assert!(
        store
            .content(id)
            .unwrap()
            .unwrap()
            .text
            .ends_with("for margins.")
    );
}

#[test]
fn mail_fetched_whole_elsewhere_fills_in_the_body_stored_without_it() {
    let (mut store, account) = store_with_account();
    let id = new_id(store.add_incoming(&head(account, 1), false).unwrap());
    // Found again by a later fetch of it in full (a search, a renumbered inbox).
    assert_eq!(
        store.add_incoming(&whole(account, 1), true).unwrap(),
        Added::Known(id)
    );
    let content = store.content(id).unwrap().unwrap();
    assert!(!content.body_missing && content.text.ends_with("for margins."));

    // A folder's sync does it too, and one without the body doesn't undo it.
    let other = new_id(store.add_incoming(&head(account, 2), false).unwrap());
    store
        .replace_folders(
            account,
            &[RemoteFolder {
                remote_id: "Projects".into(),
                parent: None,
                name: "Projects".into(),
                role: None,
                selectable: true,
            }],
        )
        .unwrap();
    let folder = store.folders().unwrap()[0].id;
    store
        .add_folder_message(folder, &whole(account, 2))
        .unwrap();
    assert!(!store.content(other).unwrap().unwrap().body_missing);
    store.add_incoming(&head(account, 2), false).unwrap();
    assert!(!store.content(other).unwrap().unwrap().body_missing);
    check_search_index(&store);
}

#[test]
fn content_filled_in_later_is_a_change_to_the_list() {
    let (mut store, account) = store_with_account();
    // Whole, it lists as it did without its body: the same preview, file
    // and (no) invite.
    let listed = Incoming {
        invite: None,
        ..head(account, 1)
    };
    let id = new_id(store.add_incoming(&listed, false).unwrap());
    let (messages, mark) = store.messages_marked().unwrap();
    assert_eq!(messages[0].content_rev, 0);
    let whole = Incoming {
        snippet: listed.snippet.clone(),
        attachments: listed.attachments.clone(),
        ..whole(account, 1)
    };
    assert!(store.fill_in_body(id, &whole).unwrap());
    let changes = store.message_changes(&mark.unwrap()).unwrap().unwrap();
    assert_eq!(changes.messages.len(), 1);
    assert_eq!(changes.messages[0].content_rev, 1);
    assert_eq!(
        Message {
            content_rev: 0,
            ..changes.messages[0].clone()
        },
        messages[0],
        "nothing else of its row changed"
    );

    // An invite a repair fills in moves it again; one it already has doesn't.
    let graph = Incoming {
        remote_id: Some("AAMk-2".into()),
        ..head(account, 2)
    };
    let other = new_id(store.add_incoming(&graph, false).unwrap());
    let invite = r#"{"kind":"invitation","ics":"BEGIN:VCALENDAR"}"#;
    let rev = |store: &Store| store.message(other).unwrap().unwrap().content_rev;
    store
        .fill_in_remote(account, "AAMk-2", &[], Some(invite))
        .unwrap();
    assert_eq!(rev(&store), 1);
    store
        .fill_in_remote(account, "AAMk-2", &[], Some(invite))
        .unwrap();
    assert_eq!(rev(&store), 1);
}

#[test]
fn what_a_folder_sync_knows_of_stored_mail_leaves_its_body_to_come() {
    let (mut store, account) = store_with_account();
    store
        .replace_folders(
            account,
            &[RemoteFolder {
                remote_id: "Projects".into(),
                parent: None,
                name: "Projects".into(),
                role: None,
                selectable: true,
            }],
        )
        .unwrap();
    let folder = store.folders().unwrap()[0].id;

    // IMAP: in a Gmail label too, known by its Message-ID, Reply-To and
    // flags, at its UID there.
    let id = new_id(store.add_incoming(&head(account, 1), false).unwrap());
    let label = Incoming {
        account_id: account,
        mailbox: Some("Projects".into()),
        uid: Some(70),
        message_id: head(account, 1).message_id,
        reply_to: Some(vec![]),
        unread: false,
        ..Default::default()
    };
    assert_eq!(
        store.add_folder_message(folder, &label).unwrap(),
        Added::Known(id)
    );
    // Graph: archived elsewhere, and Archive lists it by its id and flags.
    let graph = Incoming {
        remote_id: Some("AAMk-1".into()),
        ..head(account, 2)
    };
    let other = new_id(store.add_incoming(&graph, false).unwrap());
    let archived = Incoming {
        account_id: account,
        remote_id: Some("AAMk-1".into()),
        reply_to: Some(vec![]),
        unread: true,
        flagged: true,
        ..Default::default()
    };
    assert_eq!(
        store.add_folder_message(folder, &archived).unwrap(),
        Added::Known(other)
    );
    store.add_incoming(&archived, false).unwrap();

    for id in [id, other] {
        let content = store.content(id).unwrap().unwrap();
        assert!(content.body_missing, "opening it still downloads it");
        assert_eq!(content.text, "Quarterly figures attached");
        assert_eq!(content.attachments[0].name, "figures.xlsx");
        assert!(content.invite.is_some());
        let row = store.message(id).unwrap().unwrap();
        assert_eq!(row.snippet, "Quarterly figures attached");
    }
    assert_eq!(found(&store, "quarterly").len(), 2);
    check_search_index(&store);
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
    let _ = spam;
    decide(
        &mut store,
        sender("spam@x.com"),
        Some(Decision::Blocked),
        1_000,
    );

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

#[test]
fn version_12_databases_move_blocked_mail_out_and_share_decisions() {
    let dir = std::env::temp_dir().join(format!("spark-store-v12-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("v12.sqlite");
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
            "ALTER TABLE messages ADD COLUMN reply_to_list TEXT NOT NULL DEFAULT '[]';
             CREATE TABLE draft_options (
                message_id INTEGER PRIMARY KEY REFERENCES messages(id) ON DELETE CASCADE,
                send_later TEXT NOT NULL
             );",
        ] {
            conn.execute_batch(schema).unwrap();
        }
        conn.pragma_update(None, "user_version", 12).unwrap();
        // Spam was blocked in one account and accepted in the other; Ana
        // waits in the first account's Gatekeeper but was accepted in the second.
        conn.execute_batch(
            "INSERT INTO accounts (id, kind, email, name, label, color, config, position)
             VALUES (1, 'imap', 'me@example.test', 'Me', 'One', '#000', '{}', 0),
                    (2, 'imap', 'me@other.test', 'Me', 'Two', '#000', '{}', 1);
             INSERT INTO senders (account_id, address, status)
             VALUES (1, 'spam@x.com', 'blocked'), (2, 'spam@x.com', 'accepted'),
                    (2, 'Ana@y.com', 'accepted');
             INSERT INTO messages (id, account_id, from_name, from_address, to_list, cc_list,
                 subject, snippet, date, text_body, attachments, category, state, unread, pinned,
                 screening, remote_place, remote_unread, remote_pinned)
             VALUES (1, 1, 'Spam', 'spam@x.com', '[]', '[]', 'Offer', '', 0, '', '[]', 'people',
                     'blocked', 0, 0, 0, 'inbox', 0, 0),
                    (2, 2, 'Spam', 'spam@x.com', '[]', '[]', 'Offer', '', 0, '', '[]', 'people',
                     'inbox', 0, 0, 0, 'inbox', 0, 0),
                    (3, 1, 'Ana', 'ana@y.com', '[]', '[]', 'Hi', '', 0, '', '[]', 'people',
                     'inbox', 1, 0, 1, 'inbox', 1, 0);",
        )
        .unwrap();
    }
    let store = Store::open(&path).unwrap();
    let now = now_ms() + 1;
    let moving = |account| -> Vec<(i64, Place)> {
        store
            .pending(account, now, 0)
            .unwrap()
            .iter()
            .map(|p| (p.id, p.place))
            .collect()
    };
    assert_eq!(moving(1), [(1, Place::Blocked)]);
    assert_eq!(moving(2), [(2, Place::Blocked)]);
    assert_eq!(
        store.message(2).unwrap().unwrap().state,
        MessageState::Blocked
    );
    assert!(!store.message(3).unwrap().unwrap().screening);
    assert_eq!(
        store.sender_status("spam@x.com").unwrap().sender,
        Some(Decision::Blocked)
    );
    drop(store);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn emptied_folders_forget_what_the_server_kept_there() {
    let (mut store, account) = store_with_imap_account();
    store
        .replace_folders(
            account,
            &[
                remote("Trash", None, "Trash", Some(FolderRole::Trash)),
                remote("Junk", None, "Junk", Some(FolderRole::Spam)),
            ],
        )
        .unwrap();
    let id_of = |store: &Store, name: &str| {
        store
            .folders()
            .unwrap()
            .into_iter()
            .find(|f| f.name == name)
            .unwrap()
            .id
    };
    let (trash, junk) = (id_of(&store, "Trash"), id_of(&store, "Junk"));
    let in_folder = |store: &mut Store, folder: i64, uid: u32| {
        let m = Incoming {
            mailbox: Some(if folder == trash { "Trash" } else { "Junk" }.into()),
            ..incoming(account, uid, "a@x.com")
        };
        new_id(store.add_folder_message(folder, &m).unwrap())
    };
    let deleted = in_folder(&mut store, trash, 1);
    let spam = in_folder(&mut store, junk, 2);
    let rescued = in_folder(&mut store, junk, 3);
    let kept = new_id(
        store
            .add_incoming(&incoming(account, 4, "b@x.com"), false)
            .unwrap(),
    );
    // Moved out of Junk since it was emptied, not pushed yet.
    let back = Patch {
        state: Some(MessageState::Inbox),
        ..Default::default()
    };
    store.update(rescued, &back, 10).unwrap();

    assert_eq!(store.forget_emptied(junk).unwrap(), 1);
    assert!(store.message(spam).unwrap().is_none());
    assert!(store.message(rescued).unwrap().is_some());
    assert!(store.message(deleted).unwrap().is_some());
    assert_eq!(store.forget_emptied(trash).unwrap(), 1);
    assert!(store.message(deleted).unwrap().is_none());
    assert!(store.message(kept).unwrap().is_some());

    // Where the server has a message, changed or not.
    let p = store.pending_for(rescued).unwrap().unwrap();
    assert_eq!((p.place, p.remote_place), (Place::Inbox, Place::Folder));
    assert_eq!(p.mailbox.as_deref(), Some("Junk"));
    assert!(store.pending_for(spam).unwrap().is_none());
    assert_eq!(store.remove_messages(&[kept, spam]).unwrap(), 1);
}

#[test]
fn a_change_made_while_its_push_is_on_the_way_is_pushed_after_it() {
    let (mut store, account) = store_with_account();
    let id = arrive(&mut store, account, 1, "spam@x.com").id;
    decide(
        &mut store,
        sender("spam@x.com"),
        Some(Decision::Blocked),
        10,
    );
    let moving = store.pending(account, now_ms() + 1, 0).unwrap();
    // The move to Blocked is on its way when the user accepts the sender again.
    decide(
        &mut store,
        sender("spam@x.com"),
        Some(Decision::Accepted),
        20,
    );
    store
        .mark_pushed(
            &moving[0],
            Pushed {
                mailbox: Some("Blocked"),
                uid: None,
            },
        )
        .unwrap();
    let back = store.pending(account, now_ms() + 1, 0).unwrap();
    assert_eq!(back.len(), 1, "the way back is still to push");
    assert_eq!(
        (back[0].id, back[0].place, back[0].remote_place),
        (id, Place::Inbox, Place::Blocked)
    );

    // The same for the user's own moves: Done, then back to the Inbox while it's pushed.
    let other = arrive(&mut store, account, 2, "ana@x.com").id;
    let state = |s| Patch {
        state: Some(s),
        ..Default::default()
    };
    store.update(other, &state(MessageState::Done), 30).unwrap();
    let archiving = store.pending(account, 31, 0).unwrap();
    store
        .update(other, &state(MessageState::Inbox), 40)
        .unwrap();
    for p in archiving.iter().filter(|p| p.id == other) {
        store
            .mark_pushed(
                p,
                Pushed {
                    mailbox: Some("Archive"),
                    uid: None,
                },
            )
            .unwrap();
    }
    let undo = store.pending(account, now_ms() + 1, 0).unwrap();
    assert!(
        undo.iter()
            .any(|p| p.id == other && p.place == Place::Inbox && p.remote_place == Place::Archive),
        "{undo:?}"
    );
}

#[test]
fn senders_accepted_later_leave_the_gatekeeper_everywhere() {
    let (mut store, work) = store_with_account();
    let waiting = arrive(&mut store, work, 1, "ana@x.com").id;
    let boss = arrive(&mut store, work, 2, "boss@contoso.com").id;
    assert!(store.message(boss).unwrap().unwrap().screening);

    // Another account's first sync has mail from Ana, and it's at Contoso.
    let contoso = store
        .add_account(&NewAccount {
            kind: AccountKind::Microsoft,
            email: "me@contoso.com",
            name: "Me",
            label: "Work",
            config: "{}",
        })
        .unwrap()
        .id;
    assert!(
        !store.message(boss).unwrap().unwrap().screening,
        "a colleague now"
    );
    store
        .add_incoming(&incoming(contoso, 3, "ana@x.com"), false)
        .unwrap();
    assert!(!store.message(waiting).unwrap().unwrap().screening);
}

#[test]
fn unsent_drafts_are_not_mail_the_user_wrote() {
    let (mut store, account) = store_with_account();
    store
        .replace_folders(
            account,
            &[RemoteFolder {
                remote_id: "[Gmail]/Drafts".into(),
                parent: None,
                name: "Drafts".into(),
                role: Some(FolderRole::Drafts),
                selectable: true,
            }],
        )
        .unwrap();
    let drafts = folder_id(&store, "[Gmail]/Drafts");
    let draft = Incoming {
        from_address: "me@gmail.com".into(),
        to: vec!["stranger@vendor.org".into()],
        ..in_folder(account, "[Gmail]/Drafts", 7, 7)
    };
    store.add_folder_message(drafts, &draft).unwrap();
    assert!(arrive(&mut store, account, 8, "stranger@vendor.org").screening);
}

#[test]
fn writing_to_someone_from_any_account_lets_their_waiting_mail_through() {
    let (mut store, gmail) = store_with_account();
    let waiting = arrive(&mut store, gmail, 1, "dana@fabrikam.org").id;
    let other = store
        .add_account(&NewAccount {
            kind: AccountKind::Imap,
            email: "me@outlook.com",
            name: "Me",
            label: "Other",
            config: "{}",
        })
        .unwrap()
        .id;
    store
        .replace_folders(
            other,
            &[RemoteFolder {
                remote_id: "Sent".into(),
                parent: None,
                name: "Sent".into(),
                role: Some(FolderRole::Sent),
                selectable: true,
            }],
        )
        .unwrap();
    // The other account's Sent folder syncs, with mail to Dana.
    let sent = Incoming {
        from_address: "me@outlook.com".into(),
        to: vec!["Dana <Dana@Fabrikam.org>".into()],
        ..in_folder(other, "Sent", 2, 2)
    };
    store
        .add_folder_message(folder_id(&store, "Sent"), &sent)
        .unwrap();
    assert!(!store.message(waiting).unwrap().unwrap().screening);

    // Someone accepted that way on arrival lets their mail elsewhere through
    // too: here someone written to from spark-rs, before they were accepted.
    let eve = arrive(&mut store, gmail, 3, "eve@fabrikam.org").id;
    store
        .record_sent(
            &[Contact {
                name: "Eve".into(),
                address: "eve@fabrikam.org".into(),
            }],
            4,
        )
        .unwrap();
    assert!(!arrive(&mut store, other, 5, "eve@fabrikam.org").screening);
    assert!(!store.message(eve).unwrap().unwrap().screening);

    // Sent mail already stored from another folder releases its recipients
    // when the Sent sync finds it (a sync passes only its location then).
    let fay = arrive(&mut store, gmail, 6, "fay@fabrikam.org").id;
    store
        .replace_folders(
            gmail,
            &[RemoteFolder {
                remote_id: "[Gmail]/All Mail".into(),
                parent: None,
                name: "All Mail".into(),
                role: Some(FolderRole::Archive),
                selectable: true,
            }],
        )
        .unwrap();
    let to_fay = Incoming {
        from_address: "me@gmail.com".into(),
        to: vec!["fay@fabrikam.org".into()],
        ..in_folder(gmail, "[Gmail]/All Mail", 7, 7)
    };
    store
        .add_folder_message(folder_id(&store, "[Gmail]/All Mail"), &to_fay)
        .unwrap();
    assert!(
        store.message(fay).unwrap().unwrap().screening,
        "All Mail isn't Sent"
    );
    store
        .replace_folders(
            gmail,
            &[
                RemoteFolder {
                    remote_id: "[Gmail]/All Mail".into(),
                    parent: None,
                    name: "All Mail".into(),
                    role: Some(FolderRole::Archive),
                    selectable: true,
                },
                RemoteFolder {
                    remote_id: "[Gmail]/Sent Mail".into(),
                    parent: None,
                    name: "Sent Mail".into(),
                    role: Some(FolderRole::Sent),
                    selectable: true,
                },
            ],
        )
        .unwrap();
    let located = Incoming {
        account_id: gmail,
        mailbox: Some("[Gmail]/Sent Mail".into()),
        uid: Some(8),
        message_id: to_fay.message_id.clone(),
        ..Default::default()
    };
    store
        .add_folder_message(folder_id(&store, "[Gmail]/Sent Mail"), &located)
        .unwrap();
    assert!(!store.message(fay).unwrap().unwrap().screening);
}

#[test]
fn snoozed_mail_from_a_sender_blocked_meanwhile_wakes_into_blocked() {
    let (mut store, account) = store_with_account();
    let snoozed = new_id(
        store
            .add_incoming(&incoming(account, 1, "spam@x.com"), false)
            .unwrap(),
    );
    let later = Patch {
        state: Some(MessageState::Snoozed),
        snoozed_until: Some(100),
        ..Default::default()
    };
    store.update(snoozed, &later, 1).unwrap();
    decide(&mut store, sender("spam@x.com"), Some(Decision::Blocked), 2);
    assert_eq!(
        store.message(snoozed).unwrap().unwrap().state,
        MessageState::Snoozed
    );
    store.wake_snoozed(101).unwrap();
    assert_eq!(
        store.message(snoozed).unwrap().unwrap().state,
        MessageState::Blocked
    );
    let pending = store.pending(account, now_ms() + 1, 0).unwrap();
    assert!(
        pending
            .iter()
            .any(|p| p.id == snoozed && p.place == Place::Blocked),
        "{pending:?}"
    );

    // Another sender's wakes into the Inbox as before.
    let ana = new_id(
        store
            .add_incoming(&incoming(account, 2, "ana@y.com"), false)
            .unwrap(),
    );
    store.update(ana, &later, 3).unwrap();
    store.wake_snoozed(101).unwrap();
    assert_eq!(
        store.message(ana).unwrap().unwrap().state,
        MessageState::Inbox
    );
}

#[test]
fn reminders_come_back_unless_someone_replied() {
    let (mut store, account) = store_with_imap_account();
    let me = "me@fastmail.com";
    let sent = |uid, subject: &str| Incoming {
        mailbox: Some("Sent".into()),
        from_address: me.into(),
        subject: subject.into(),
        date: 1_000,
        ..incoming(account, uid, me)
    };
    let quiet = new_id(store.add_incoming(&sent(1, "Quiet"), false).unwrap());
    let answered = new_id(store.add_incoming(&sent(2, "Answered"), false).unwrap());
    let reply = Incoming {
        in_reply_to: Some("<2@example.com>".into()),
        subject: "Re: Answered".into(),
        date: 2_000,
        ..incoming(account, 3, "ana@x.com")
    };
    store.add_incoming(&reply, false).unwrap();
    fn remind<'a>(account: i64, message_id: &'a str, subject: &'a str) -> NewReminder<'a> {
        NewReminder {
            account_id: account,
            message_id: Some(message_id),
            reply_to: None,
            subject,
            to: "Ana <ana@x.com>",
            sent_at: 1_000,
            remind_at: 5_000,
            if_no_reply: true,
        }
    }
    let first = store
        .add_reminder(&remind(account, "<1@example.com>", "Quiet"))
        .unwrap();
    store
        .add_reminder(&remind(account, "2@example.com", "Answered"))
        .unwrap();
    assert_eq!(store.reminders().unwrap().len(), 2);

    let synced = HashMap::from([(account, 5_000)]);
    assert!(store.wake_reminders(4_999, &synced).unwrap().is_empty());
    // Not until the account has synced since, so a reply on the server is seen.
    assert!(
        store
            .wake_reminders(5_000, &HashMap::new())
            .unwrap()
            .is_empty()
    );
    let due = store.wake_reminders(5_000, &synced).unwrap();
    assert_eq!(due.len(), 1, "the answered one is gone");
    assert_eq!(
        (due[0].id, due[0].message, due[0].due),
        (first, Some(quiet), true)
    );
    assert_eq!(store.reminders().unwrap().len(), 1);
    let m = store.message(quiet).unwrap().unwrap();
    assert_eq!(m.reminder.map(|r| (r.id, r.at)), Some((first, 5_000)));
    assert!(store.message(answered).unwrap().unwrap().reminder.is_none());

    // Done takes it out of the Inbox; undo brings it back.
    store.finish_reminder(first, true).unwrap();
    assert!(store.message(quiet).unwrap().unwrap().reminder.is_none());
    store.finish_reminder(first, false).unwrap();
    assert!(store.message(quiet).unwrap().unwrap().reminder.is_some());
    // A new time makes it wait again; None removes it.
    store.set_reminder(first, Some(9_000)).unwrap();
    assert!(store.message(quiet).unwrap().unwrap().reminder.is_none());
    assert!(!store.reminder(first).unwrap().unwrap().due);
    store.set_reminder(first, None).unwrap();
    assert!(store.reminders().unwrap().is_empty());

    // Sent where the copy isn't stored yet: due anyway after the grace
    // period, and it finds its email once that's synced.
    let later = store
        .add_reminder(&remind(account, "<5@example.com>", "Later"))
        .unwrap();
    let due = store
        .wake_reminders(5_000 + 10 * 60_000, &HashMap::new())
        .unwrap();
    assert_eq!((due[0].id, due[0].message), (later, None));
    let copy = new_id(store.add_incoming(&sent(5, "Later"), false).unwrap());
    assert!(
        store
            .wake_reminders(5_000 + 10 * 60_000, &synced)
            .unwrap()
            .is_empty()
    );
    assert_eq!(store.reminder(later).unwrap().unwrap().message, Some(copy));
    // A reply that turns up after it came due ends it.
    let reply = Incoming {
        in_reply_to: Some("<5@example.com>".into()),
        subject: "Re: Later".into(),
        date: 3_000,
        ..incoming(account, 6, "ana@x.com")
    };
    store.add_incoming(&reply, false).unwrap();
    store.wake_reminders(5_000 + 10 * 60_000, &synced).unwrap();
    assert!(store.reminder(later).unwrap().is_none());
}

#[test]
fn trashing_an_email_ends_its_reminder() {
    let (mut store, account) = store_with_imap_account();
    let id = new_id(
        store
            .add_incoming(&incoming(account, 1, "me@fastmail.com"), false)
            .unwrap(),
    );
    let reminder = store
        .add_reminder(&NewReminder {
            account_id: account,
            message_id: Some("1@example.com"),
            reply_to: None,
            subject: "Message 1",
            to: "ana@x.com",
            sent_at: 0,
            remind_at: 10,
            if_no_reply: false,
        })
        .unwrap();
    store
        .wake_reminders(10, &HashMap::from([(account, 10)]))
        .unwrap();
    assert!(store.message(id).unwrap().unwrap().reminder.is_some());
    let trash = Patch {
        state: Some(MessageState::Deleted),
        ..Default::default()
    };
    store.update(id, &trash, 20).unwrap();
    assert!(store.message(id).unwrap().unwrap().reminder.is_none());
    assert!(store.reminders().unwrap().iter().all(|r| r.id != reminder));
}

#[test]
fn scheduled_sends_can_move_or_come_back_out() {
    let (mut store, account) = store_with_imap_account();
    let later = r#"{"account":1,"to":"a@x.com","subject":"Later","body":"","sendAt":9000,"sendLater":{"key":"tomorrow","at":9000},"attachments":[]}"#;
    let now =
        r#"{"account":1,"to":"a@x.com","subject":"Now","body":"","sendAt":10,"attachments":[]}"#;
    let scheduled = store.queue_send(account, later, 9_000, &[]).unwrap();
    store.queue_send(account, now, 10, &[]).unwrap();
    let listed = store.scheduled_sends().unwrap();
    assert_eq!(listed.iter().map(|s| s.0).collect::<Vec<_>>(), [scheduled]);

    assert!(store.reschedule_send(scheduled, Some(20_000), 0).unwrap());
    let (_, _, draft, send_at) = store.scheduled_sends().unwrap().remove(0);
    assert_eq!(send_at, 20_000);
    let draft: serde_json::Value = serde_json::from_str(&draft).unwrap();
    assert_eq!(draft["sendLater"]["at"], 20_000);
    assert_eq!(draft["sendAt"], 20_000);

    // Sent now: no longer scheduled, and due.
    assert!(store.reschedule_send(scheduled, None, 30).unwrap());
    assert!(store.scheduled_sends().unwrap().is_empty());
    assert_eq!(store.claim_due_sends(account, 30).unwrap().len(), 2);
    // Once it's sending it's no longer queued.
    assert_eq!(store.queued_send(scheduled).unwrap(), None);
    let again = store.queue_send(account, later, 9_000, &[]).unwrap();
    let (owner, draft) = store.queued_send(again).unwrap().unwrap();
    assert_eq!(owner, account);
    assert!(draft.contains("\"Later\""));
}

#[test]
fn turning_the_gatekeeper_off_keeps_a_blocked_domain_blocked() {
    let (mut store, account) = store_with_account();
    let snoozed = arrive(&mut store, account, 1, "spam@spam.biz").id;
    let later = Patch {
        state: Some(MessageState::Snoozed),
        snoozed_until: Some(100),
        ..Default::default()
    };
    store.update(snoozed, &later, 1).unwrap();
    let other = arrive(&mut store, account, 2, "ads@spam.biz").id;
    decide(
        &mut store,
        Screen::Domain("spam.biz".into()),
        Some(Decision::Blocked),
        2,
    );
    assert_eq!(
        store.message(other).unwrap().unwrap().state,
        MessageState::Blocked
    );

    store.set_gatekeeper_mode(GatekeeperMode::Off).unwrap();
    assert_eq!(store.sender_status("spam@spam.biz").unwrap().sender, None);
    store.wake_snoozed(101).unwrap();
    assert_eq!(
        store.message(snoozed).unwrap().unwrap().state,
        MessageState::Blocked
    );
}

#[test]
fn a_senders_category_holds_for_their_mail_now_and_later() {
    let (mut store, account) = store_with_account();
    let news = |uid| Incoming {
        category: Category::Newsletters,
        ..incoming(account, uid, "news@shop.example")
    };
    let first = new_id(store.add_incoming(&news(1), false).unwrap());
    let other = new_id(
        store
            .add_incoming(&incoming(account, 2, "ana@x.com"), false)
            .unwrap(),
    );

    assert_eq!(
        store
            .set_sender_category("NEWS@shop.example", Some(Category::People))
            .unwrap(),
        None
    );
    assert_eq!(
        store.message(first).unwrap().unwrap().category,
        Category::People
    );
    let later = new_id(store.add_incoming(&news(3), false).unwrap());
    assert_eq!(
        store.message(later).unwrap().unwrap().category,
        Category::People
    );
    assert_eq!(
        store.message(other).unwrap().unwrap().category,
        Category::People
    );

    // Undone: back to what the headers say.
    assert_eq!(
        store
            .set_sender_category("news@shop.example", None)
            .unwrap(),
        Some(Category::People)
    );
    assert_eq!(
        store.message(first).unwrap().unwrap().category,
        Category::Newsletters
    );
    assert_eq!(
        store.message(later).unwrap().unwrap().category,
        Category::Newsletters
    );
}

#[test]
fn messages_say_what_kind_of_invite_they_carry() {
    let (mut store, account) = store_with_account();
    let invited = Incoming {
        invite: Some(r#"{"kind":"invitation","uid":"x"}"#.into()),
        ..incoming(account, 1, "ana@x.com")
    };
    let id = new_id(store.add_incoming(&invited, false).unwrap());
    let plain = new_id(
        store
            .add_incoming(&incoming(account, 2, "bo@x.com"), false)
            .unwrap(),
    );
    assert_eq!(
        store.message(id).unwrap().unwrap().invite_kind.as_deref(),
        Some("invitation")
    );
    assert_eq!(store.message(plain).unwrap().unwrap().invite_kind, None);
}

#[test]
fn undoing_a_block_keeps_a_new_senders_snoozed_mail_waiting() {
    let (mut store, account) = store_with_account();
    let snoozed = arrive(&mut store, account, 1, "new@x.com").id;
    let later = Patch {
        state: Some(MessageState::Snoozed),
        snoozed_until: Some(100),
        ..Default::default()
    };
    store.update(snoozed, &later, 1).unwrap();
    let undo = decide(&mut store, sender("new@x.com"), Some(Decision::Blocked), 2);
    store.decide(&undo, 3).unwrap();
    store.wake_snoozed(101).unwrap();
    let woke = store.message(snoozed).unwrap().unwrap();
    assert_eq!((woke.state, woke.screening), (MessageState::Inbox, true));
}

#[test]
fn a_new_accounts_own_address_leaves_the_gatekeeper() {
    let mut store = Store::open_in_memory().unwrap();
    let work = store
        .add_account(&NewAccount {
            kind: AccountKind::Microsoft,
            email: "me@contoso.com",
            name: "Me",
            label: "Work",
            config: "{}",
        })
        .unwrap()
        .id;
    let from_home = arrive(&mut store, work, 1, "me.at.home@gmail.com").id;
    assert!(store.message(from_home).unwrap().unwrap().screening);
    store
        .add_account(&NewAccount {
            kind: AccountKind::Gmail,
            email: "Me.At.Home@gmail.com",
            name: "Me",
            label: "Home",
            config: "{}",
        })
        .unwrap();
    assert!(!store.message(from_home).unwrap().unwrap().screening);
}

#[test]
fn writing_to_someone_at_a_blocked_domain_keeps_them_blocked() {
    let (mut store, account) = store_with_account();
    decide(
        &mut store,
        Screen::Domain("vendor.org".into()),
        Some(Decision::Blocked),
        1,
    );
    // The user writes to Ann, or a Sent search says they had, after the block.
    store.accept_sender("ann@vendor.org", 2).unwrap();
    assert_eq!(store.sender_status("ann@vendor.org").unwrap().sender, None);
    assert_eq!(
        arrive(&mut store, account, 1, "ann@vendor.org").state,
        MessageState::Blocked
    );
}

#[test]
fn undoing_an_accept_puts_a_new_senders_snoozed_mail_back_in_the_gatekeeper() {
    let (mut store, account) = store_with_account();
    let snoozed = arrive(&mut store, account, 1, "new@x.com").id;
    let later = Patch {
        state: Some(MessageState::Snoozed),
        snoozed_until: Some(100),
        ..Default::default()
    };
    store.update(snoozed, &later, 1).unwrap();
    arrive(&mut store, account, 2, "new@x.com");
    let undo = decide(&mut store, sender("new@x.com"), Some(Decision::Accepted), 2);
    assert!(!store.message(snoozed).unwrap().unwrap().screening);
    store.decide(&undo, 3).unwrap();
    store.wake_snoozed(101).unwrap();
    let woke = store.message(snoozed).unwrap().unwrap();
    assert_eq!((woke.state, woke.screening), (MessageState::Inbox, true));
}

#[test]
fn only_the_address_mail_went_to_counts_not_one_in_its_name() {
    let (mut store, account) = store_with_account();
    store
        .replace_folders(
            account,
            &[RemoteFolder {
                remote_id: "[Gmail]/Sent Mail".into(),
                parent: None,
                name: "Sent Mail".into(),
                role: Some(FolderRole::Sent),
                selectable: true,
            }],
        )
        .unwrap();
    // As stored for `"Ann <ann@old-vendor.org>" <ann@new-vendor.org>`.
    let sent = Incoming {
        from_address: "me@gmail.com".into(),
        to: vec!["Ann <ann@old-vendor.org> <ann@new-vendor.org>".into()],
        ..in_folder(account, "[Gmail]/Sent Mail", 7, 7)
    };
    store
        .add_folder_message(folder_id(&store, "[Gmail]/Sent Mail"), &sent)
        .unwrap();
    assert!(arrive(&mut store, account, 8, "ann@old-vendor.org").screening);
    assert!(!arrive(&mut store, account, 9, "ann@new-vendor.org").screening);
}

#[test]
fn templates_keep_their_order_and_note_when_they_were_used() {
    let mut store = Store::open_in_memory().unwrap();
    let new = |name| NewTemplate {
        name,
        subject: "",
        html: "<div>Hi</div>",
        text: "Hi",
    };
    let a = store.add_template(new("Thanks")).unwrap();
    let b = store
        .add_template(NewTemplate {
            subject: "Intro",
            ..new("Intro")
        })
        .unwrap();
    let c = store.add_template(new("Follow up")).unwrap();
    let names = |store: &Store| -> Vec<String> {
        store
            .templates()
            .unwrap()
            .into_iter()
            .map(|t| t.name)
            .collect()
    };
    assert_eq!(names(&store), ["Thanks", "Intro", "Follow up"]);
    store.order_templates(&[c.id, a.id]).unwrap();
    assert_eq!(names(&store), ["Follow up", "Thanks", "Intro"]);
    assert!(
        store
            .update_template(
                b.id,
                NewTemplate {
                    name: "Introduction",
                    ..new("")
                }
            )
            .unwrap()
    );
    store.template_used(b.id, 5).unwrap();
    let b = store.template(b.id).unwrap().unwrap();
    assert_eq!(
        (b.name.as_str(), b.subject.as_str(), b.used_at),
        ("Introduction", "", Some(5))
    );
    store.remove_template(a.id).unwrap();
    assert!(!store.update_template(a.id, new("Gone")).unwrap());
    assert_eq!(names(&store), ["Follow up", "Introduction"]);
}

#[test]
fn a_message_keeps_its_files_key_and_a_reused_id_gets_another() {
    let (mut store, account) = store_with_account();
    let first = arrive(&mut store, account, 1, "ana@x.com").id;
    assert_eq!(
        store.files_key(first, "aaa").unwrap().as_deref(),
        Some("aaa")
    );
    assert_eq!(
        store.files_key(first, "bbb").unwrap().as_deref(),
        Some("aaa"),
        "made once"
    );
    assert_eq!(
        store.files_keys(account).unwrap(),
        [(first, "aaa".to_owned())]
    );
    store.remove_messages(&[first]).unwrap();
    assert_eq!(store.files_key(first, "ccc").unwrap(), None);
    // SQLite gives the last id to the next row once it's free.
    let next = arrive(&mut store, account, 2, "bo@x.com").id;
    assert_eq!(next, first);
    assert_eq!(
        store.files_key(next, "ddd").unwrap().as_deref(),
        Some("ddd")
    );
}

#[test]
fn version_21_databases_index_pending_and_snoozed_mail() {
    let dir = std::env::temp_dir().join(format!("spark-store-v21-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("v21.sqlite");
    let _ = std::fs::remove_file(&path);
    Store::open(&path)
        .unwrap()
        .conn
        .execute_batch(
            "DROP INDEX messages_pending;
             DROP INDEX messages_due_snooze;
             ALTER TABLE messages DROP COLUMN body_missing;
             -- This opening's change log watches it.
             DROP TRIGGER temp.log_message_changed;
             ALTER TABLE messages DROP COLUMN content_rev;
             PRAGMA user_version = 21;",
        )
        .unwrap();
    let indexes = |store: &Store| -> Vec<String> {
        let mut stmt = store
            .conn
            .prepare(
                "SELECT name FROM sqlite_master
                 WHERE name IN ('messages_pending', 'messages_due_snooze') ORDER BY name",
            )
            .unwrap();
        stmt.query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_>>()
            .unwrap()
    };
    assert_eq!(
        indexes(&Store::open(&path).unwrap()),
        ["messages_due_snooze", "messages_pending"]
    );
    // Reopening a migrated database must not create them twice.
    assert_eq!(indexes(&Store::open(&path).unwrap()).len(), 2);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn version_25_databases_have_every_body() {
    let dir = std::env::temp_dir().join(format!("spark-store-v25-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("v25.sqlite");
    let _ = std::fs::remove_file(&path);
    let id = {
        let (mut store, account) = {
            let store = Store::open(&path).unwrap();
            let account = store
                .add_account(&NewAccount {
                    kind: AccountKind::Imap,
                    email: "a@b.c",
                    name: "A",
                    label: "A",
                    config: "{}",
                })
                .unwrap();
            (store, account.id)
        };
        let id = new_id(
            store
                .add_incoming(&incoming(account, 1, "a@x.com"), false)
                .unwrap(),
        );
        store
            .conn
            .execute_batch(
                "ALTER TABLE messages DROP COLUMN body_missing;
                 DROP TRIGGER temp.log_message_changed;
                 ALTER TABLE messages DROP COLUMN content_rev;
                 PRAGMA user_version = 25;",
            )
            .unwrap();
        id
    };
    let store = Store::open(&path).unwrap();
    assert!(!store.content(id).unwrap().unwrap().body_missing);
    assert_eq!(store.message(id).unwrap().unwrap().content_rev, 0);
    drop(store);
    // Reopening a migrated database doesn't add the column twice.
    assert!(
        !Store::open(&path)
            .unwrap()
            .content(id)
            .unwrap()
            .unwrap()
            .body_missing
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Counts the store's commits from now on, statements committed on their
/// own included.
fn commits(store: &Store) -> &'static std::sync::atomic::AtomicUsize {
    use std::ffi::{c_int, c_void};
    use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};

    unsafe extern "C" fn committed(count: *mut c_void) -> c_int {
        // SAFETY: `count` is the counter below, which is never freed.
        unsafe { (*count.cast::<AtomicUsize>()).fetch_add(1, SeqCst) };
        0
    }
    let count: &'static AtomicUsize = Box::leak(Box::new(AtomicUsize::new(0)));
    // SAFETY: the connection is open, and the counter outlives it.
    unsafe {
        rusqlite::ffi::sqlite3_commit_hook(
            store.conn.handle(),
            Some(committed),
            std::ptr::from_ref(count).cast_mut().cast(),
        );
    }
    count
}

/// Counts the rows written to the database from now on, leaving out the
/// temporary tables that keep track of them (`changes`' log), which
/// `total_changes` counts too.
fn rows_written(store: &Store) -> &'static std::sync::atomic::AtomicUsize {
    use std::ffi::{CStr, c_char, c_int, c_void};
    use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};

    unsafe extern "C" fn written(
        count: *mut c_void,
        _: c_int,
        db: *const c_char,
        _: *const c_char,
        _: i64,
    ) {
        // SAFETY: SQLite passes the database's name, and `count` is the
        // counter below, which is never freed.
        unsafe {
            if CStr::from_ptr(db) == c"main" {
                (*count.cast::<AtomicUsize>()).fetch_add(1, SeqCst);
            }
        }
    }
    let count: &'static AtomicUsize = Box::leak(Box::new(AtomicUsize::new(0)));
    // SAFETY: the connection is open, and the counter outlives it.
    unsafe {
        rusqlite::ffi::sqlite3_update_hook(
            store.conn.handle(),
            Some(written),
            std::ptr::from_ref(count).cast_mut().cast(),
        );
    }
    count
}

#[test]
fn a_thousand_message_import_commits_once_a_chunk() {
    use std::sync::atomic::Ordering::SeqCst;

    let (mut store, account) = store_with_account();
    let commits = commits(&store);
    let mail: Vec<Incoming> = (1..=1_000)
        .map(|uid| incoming(account, uid, &format!("s{}@x.com", uid % 40)))
        .collect();
    for chunk in mail.chunks(25) {
        store
            .batch(|b| {
                for m in chunk {
                    b.add_incoming(m, false)?;
                }
                Ok(())
            })
            .unwrap();
    }
    assert_eq!(commits.load(SeqCst), 40);
    assert_eq!(store.messages().unwrap().len(), 1_000);
    assert_eq!(found(&store, "message 1000").len(), 1);

    // On its own, a message is a commit of its own.
    store
        .add_incoming(&incoming(account, 1_001, "a@x.com"), false)
        .unwrap();
    assert_eq!(commits.load(SeqCst), 41);
}

#[test]
fn a_batchs_sync_state_commits_with_its_mail_or_not_at_all() {
    let (mut store, account) = store_with_account();
    store
        .batch(|b| {
            b.add_incoming(&incoming(account, 1, "a@x.com"), false)?;
            assert_eq!(b.sync_state(account)?.as_deref(), Some("{}"));
            b.set_sync_state(account, r#"{"n":1}"#)?;
            assert_eq!(b.sync_state(account)?.as_deref(), Some(r#"{"n":1}"#));
            assert_eq!(b.sync_state(account + 1)?, None, "no such account");
            Ok(())
        })
        .unwrap();
    assert_eq!(store.messages().unwrap().len(), 1);
    let state = |store: &Store| store.account(account).unwrap().unwrap().sync_state;
    assert_eq!(state(&store), r#"{"n":1}"#);

    // One that fails after writing both keeps neither.
    let failed = store.batch(|b| {
        b.add_incoming(&incoming(account, 2, "a@x.com"), false)?;
        b.set_sync_state(account, r#"{"n":2}"#)?;
        Err::<(), _>(Error::InvalidQuery)
    });
    assert!(failed.is_err());
    assert_eq!(store.messages().unwrap().len(), 1);
    assert_eq!(state(&store), r#"{"n":1}"#);
}

#[test]
fn flags_the_server_reported_before_write_nothing() {
    use std::sync::atomic::Ordering::SeqCst;

    let (mut store, account) = store_with_account();
    let rows = rows_written(&store);
    let mail: Vec<Incoming> = (1..=1_000)
        .map(|uid| incoming(account, uid, "a@x.com"))
        .collect();
    store
        .batch(|b| {
            for m in &mail {
                b.add_incoming(m, false)?;
            }
            Ok(())
        })
        .unwrap();
    // Every third read elsewhere, every seventh flagged.
    let flags: Vec<(u32, bool, bool)> = (1..=1_000)
        .map(|uid| (uid, uid % 3 != 0, uid % 7 == 0))
        .collect();
    let reconcile = |store: &mut Store| {
        let before = rows.load(SeqCst);
        for chunk in flags.chunks(500) {
            store
                .batch(|b| {
                    for (uid, unread, flagged) in chunk {
                        b.observe_flags(account, "INBOX", *uid, *unread, *flagged)?;
                    }
                    Ok(())
                })
                .unwrap();
        }
        rows.load(SeqCst) - before
    };
    // Only the rows whose flags changed: 333 read, 142 flagged, 47 both.
    assert_eq!(reconcile(&mut store), 428);
    assert_eq!(reconcile(&mut store), 0);
    let read = store
        .messages()
        .unwrap()
        .iter()
        .filter(|m| !m.unread)
        .count();
    assert_eq!(read, 333);

    // The same for a message seen again in full, and Graph's flags.
    let written = rows.load(SeqCst);
    store
        .add_incoming(
            &Incoming {
                unread: false,
                ..incoming(account, 3, "a@x.com")
            },
            true,
        )
        .unwrap();
    assert_eq!(rows.load(SeqCst), written);
    let graph = Incoming {
        account_id: account,
        remote_id: Some("g1".into()),
        from_address: "a@x.com".into(),
        unread: true,
        ..Default::default()
    };
    let id = new_id(store.add_incoming(&graph, false).unwrap());
    let written = rows.load(SeqCst);
    assert_eq!(store.add_incoming(&graph, false).unwrap(), Added::Known(id));
    store
        .observe_remote_flags(account, "g1", true, false)
        .unwrap();
    assert_eq!(rows.load(SeqCst), written);
    store
        .observe_remote_flags(account, "g1", false, false)
        .unwrap();
    assert_eq!(rows.load(SeqCst), written + 1);
    assert!(!store.message(id).unwrap().unwrap().unread);
}

#[test]
fn a_folder_synced_again_unchanged_writes_nothing() {
    let (mut store, account) = store_with_account();
    store
        .replace_folders(account, &[remote("Travel", None, "Travel", None)])
        .unwrap();
    let travel = folder_id(&store, "Travel");
    let listing: Vec<Incoming> = (1..=50)
        .map(|uid| in_folder(account, "Travel", uid, uid))
        .collect();
    let store_listing = |store: &mut Store| {
        store
            .batch(|b| {
                listing
                    .iter()
                    .map(|m| match b.add_folder_message(travel, m)? {
                        Added::New(id) | Added::Known(id) => Ok((id, m.uid)),
                    })
                    .collect::<Result<Vec<_>>>()
            })
            .unwrap()
    };

    let members = store_listing(&mut store);
    // The batch listed them in the folder: making them its members again
    // writes nothing.
    let written = store.conn.total_changes();
    assert_eq!(store.set_folder_members(travel, &members).unwrap(), 0);
    assert_eq!(store.conn.total_changes(), written);
    assert_eq!(store.folder_member_ids(travel).unwrap().len(), 50);

    let written = store.conn.total_changes();
    let members = store_listing(&mut store);
    assert_eq!(store.set_folder_members(travel, &members).unwrap(), 0);
    assert_eq!(store.conn.total_changes(), written);

    // A new UID for a member is still taken, the one listed last.
    let first = members[0].0;
    store
        .set_folder_members(travel, &[(first, Some(70)), (first, Some(71))])
        .unwrap();
    assert_eq!(
        store.folder_uids(travel).unwrap(),
        HashMap::from([(71, first)])
    );
}

#[test]
fn a_failed_batch_stores_none_of_its_chunk() {
    let (mut store, account) = store_with_account();
    let read = new_id(
        store
            .add_incoming(&incoming(account, 10, "old@x.com"), false)
            .unwrap(),
    );
    store
        .conn
        .execute_batch(
            "CREATE TRIGGER reject_message_3 BEFORE INSERT ON messages
             WHEN new.subject = 'Message 3'
             BEGIN SELECT RAISE(ABORT, 'cannot store message 3'); END;",
        )
        .unwrap();
    let chunk: Vec<Incoming> = (1..=4)
        .map(|uid| incoming(account, uid, &format!("new{uid}@x.com")))
        .collect();
    let store_chunk = |store: &mut Store| {
        store.batch(|b| {
            b.observe_flags(account, "INBOX", 10, false, true)?;
            chunk
                .iter()
                .map(|m| b.add_incoming(m, false))
                .collect::<Result<Vec<_>>>()
        })
    };
    assert!(store_chunk(&mut store).is_err());

    // Nothing of it stayed: not the messages before the failure, their
    // threading, search index or senders, nor the flags it observed.
    let ids: Vec<i64> = store.messages().unwrap().iter().map(|m| m.id).collect();
    assert_eq!(ids, [read]);
    let count = |sql: &str| -> i64 { store.conn.query_row(sql, [], |r| r.get(0)).unwrap() };
    assert_eq!(
        count(&format!(
            "SELECT COUNT(*) FROM message_keys WHERE message_id != {read}"
        )),
        0
    );
    assert_eq!(
        count("SELECT COUNT(*) FROM gatekeeper_senders WHERE address LIKE 'new%'"),
        0
    );
    assert!(found(&store, "message 2").is_empty());
    let m = store.message(read).unwrap().unwrap();
    assert!(m.unread && !m.pinned);

    // Tried again, it's stored whole, once.
    store
        .conn
        .execute_batch("DROP TRIGGER reject_message_3")
        .unwrap();
    let added = store_chunk(&mut store).unwrap();
    assert!(added.iter().all(|a| matches!(a, Added::New(_))));
    assert_eq!(store.messages().unwrap().len(), 5);
    assert_eq!(found(&store, "message 3").len(), 1);
    assert!(store.message(read).unwrap().unwrap().pinned);
    assert_eq!(store_chunk(&mut store).unwrap().len(), 4);
    assert_eq!(store.messages().unwrap().len(), 5, "no duplicates");
}

#[test]
fn batched_observations_keep_pending_local_changes() {
    let (mut store, account) = store_with_account();
    let mail: Vec<Incoming> = (1..=3)
        .map(|uid| incoming(account, uid, "a@x.com"))
        .collect();
    let ids: Vec<i64> = store
        .batch(|b| {
            mail.iter()
                .map(|m| b.add_incoming(m, false))
                .collect::<Result<Vec<_>>>()
        })
        .unwrap()
        .into_iter()
        .map(new_id)
        .collect();
    // Read here; and Done here, both not pushed yet.
    let read = Patch {
        unread: Some(false),
        ..Default::default()
    };
    store.update(ids[0], &read, 1_000).unwrap();
    let done = Patch {
        state: Some(MessageState::Done),
        ..Default::default()
    };
    store.update(ids[1], &done, 1_000).unwrap();

    let observe = |store: &mut Store| {
        store
            .batch(|b| {
                // The server: all still unread, the first flagged.
                b.observe_flags(account, "INBOX", 1, true, true)?;
                b.observe_flags(account, "INBOX", 3, true, false)?;
                // The second seen in the inbox again, in full.
                b.add_incoming(&mail[1], true)
            })
            .unwrap()
    };
    assert_eq!(observe(&mut store), Added::Known(ids[1]));
    assert_eq!(observe(&mut store), Added::Known(ids[1]));
    let first = store.message(ids[0]).unwrap().unwrap();
    assert!(!first.unread, "the local read is waiting to be pushed");
    assert!(first.pinned, "the flag follows the server");
    assert_eq!(
        store.message(ids[1]).unwrap().unwrap().state,
        MessageState::Done
    );
    let pending: Vec<i64> = store
        .pending(account, 9_000, 0)
        .unwrap()
        .iter()
        .map(|p| p.id)
        .collect();
    assert_eq!(pending, [ids[0], ids[1]]);
}

#[test]
fn graph_mail_keyed_anew_is_stored_once() {
    let (mut store, account_id) = store_with_account();
    let graph = |remote_id: &str, n: u32| Incoming {
        mailbox: None,
        uid: None,
        remote_id: Some(remote_id.into()),
        ..incoming(account_id, n, "ana@example.com")
    };
    let add = |store: &mut Store, remote_id: &str, n: u32| {
        new_id(store.add_incoming(&graph(remote_id, n), false).unwrap())
    };
    // A sync's copy of mail 1, and a search's under another id; mail 2, 3
    // and 4 only a search's.
    let synced = add(&mut store, "immutable-1", 1);
    let twin = add(&mut store, "rest-1", 1);
    let alone = add(&mut store, "rest-2", 2);
    let gone = add(&mut store, "rest-3", 3);
    let busy = add(&mut store, "rest-4", 4);
    let priority = Patch {
        priority: Some(true),
        ..Default::default()
    };
    store.update(twin, &priority, 10).unwrap();
    let reminder = store
        .add_reminder(&NewReminder {
            account_id,
            message_id: None,
            reply_to: Some(twin),
            subject: "Re: Message 1",
            to: "ana@example.com",
            sent_at: 10,
            remind_at: 20,
            if_no_reply: true,
        })
        .unwrap();
    let read = Patch {
        unread: Some(false),
        ..Default::default()
    };
    store.update(busy, &read, 10).unwrap();
    let settled: Vec<i64> = store
        .settled_remote_ids(account_id)
        .unwrap()
        .into_iter()
        .map(|(id, _)| id)
        .collect();
    assert_eq!(
        settled,
        [synced, twin, alone, gone],
        "busy has a change to push"
    );

    // The search's copy goes, its priority and reminder kept on the sync's.
    assert!(
        store
            .rekey_remote(account_id, twin, Some("immutable-1"))
            .unwrap()
    );
    assert!(store.message(twin).unwrap().is_none());
    assert!(store.message(synced).unwrap().unwrap().priority);
    let reply_to: Option<i64> = store
        .conn
        .query_row(
            "SELECT reply_to FROM reminders WHERE id = ?1",
            [reminder],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(reply_to, Some(synced));

    // Mail stored once takes its new id; mail the server lost goes.
    assert!(
        store
            .rekey_remote(account_id, alone, Some("immutable-2"))
            .unwrap()
    );
    assert_eq!(
        store.location(alone).unwrap().unwrap().remote_id.as_deref(),
        Some("immutable-2")
    );
    assert!(store.rekey_remote(account_id, gone, None).unwrap());
    assert!(store.message(gone).unwrap().is_none());

    // A change waiting to be pushed keeps it as it is.
    assert!(!store.rekey_remote(account_id, busy, None).unwrap());
    assert!(store.message(busy).unwrap().is_some());
    assert!(store.has_remote_id(account_id, "rest-4").unwrap());
}
