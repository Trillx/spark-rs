//! End-to-end IMAP/SMTP test against a GreenMail server. Skipped unless
//! SPARK_TEST_GREENMAIL names its host (plain IMAP on 3143, SMTP on 3025):
//!
//!   docker network create spark-test
//!   docker run -d --rm --name spark-greenmail --network spark-test \
//!     -e GREENMAIL_OPTS='-Dgreenmail.setup.test.all -Dgreenmail.hostname=0.0.0.0 -Dgreenmail.auth.disabled' \
//!     greenmail/standalone:2.1.5
//!   SPARK_CROSS_DOCKER_ARGS='--network spark-test -e SPARK_TEST_GREENMAIL=spark-greenmail' \
//!     tools/cross/run.sh cargo test -p spark-mail --test greenmail

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_imap::types::Flag;
use futures_util::TryStreamExt;
use lettre::message::header::{ContentType, HeaderName, HeaderValue};
use lettre::message::{Attachment, MultiPart, SinglePart};
use spark_mail::draft::{Draft, OutgoingAttachment};
use spark_mail::engine::{folder_has_older, inbox_has_older};
use spark_mail::imap::{self, Security, Server};
use spark_mail::{Engine, OLDER_STEP, Round, Secrets, drafts, smtp};
use spark_store::{
    AccountKind, Category, FolderRole, MessageState, OutboxFile, Patch, Store, now_ms,
};

#[derive(Default)]
struct MemorySecrets(Mutex<HashMap<String, String>>);

impl Secrets for MemorySecrets {
    fn get(&self, name: &str) -> Result<Option<String>, String> {
        Ok(self.0.lock().unwrap().get(name).cloned())
    }
    fn set(&self, name: &str, value: &str) -> Result<(), String> {
        let mut map = self.0.lock().unwrap();
        if value.is_empty() {
            map.remove(name);
        } else {
            map.insert(name.into(), value.into());
        }
        Ok(())
    }
}

fn plain(host: &str, port: u16) -> Server {
    Server {
        host: host.into(),
        port,
        security: Security::Plain,
    }
}

async fn deliver(smtp: &Server, from: &str, to: &str, subject: &str, list: bool) {
    let mut builder = lettre::Message::builder()
        .from(from.parse().unwrap())
        .to(to.parse().unwrap())
        .subject(subject)
        .message_id(None);
    if list {
        builder = builder.raw_header(HeaderValue::new(
            HeaderName::new_from_ascii_str("List-Unsubscribe"),
            "<mailto:unsub@news.example>".into(),
        ));
    }
    let message = builder.body(format!("Body of {subject}\n")).unwrap();
    smtp::send(smtp, "sender", "pw", message).await.unwrap();
}

fn back_in_time(engine: &Engine, id: i64, patch: Patch) {
    // Older than the undo window, so the next round pushes it.
    engine
        .store()
        .update(id, &patch, now_ms() - 60_000)
        .unwrap();
}

fn find(engine: &Engine, subject: &str) -> spark_store::Message {
    engine
        .store()
        .messages()
        .unwrap()
        .into_iter()
        .find(|m| m.subject == subject)
        .unwrap_or_else(|| panic!("no message {subject:?}"))
}

#[tokio::test]
async fn imap_round_trip() {
    let Ok(host) = std::env::var("SPARK_TEST_GREENMAIL") else {
        eprintln!("skipped: set SPARK_TEST_GREENMAIL to a GreenMail host");
        return;
    };
    let imap = plain(&host, 3143);
    let smtp = plain(&host, 3025);
    let me = format!("me{}@example.com", now_ms());

    deliver(&smtp, "Ana Ruiz <ana@x.com>", &me, "Lunch?", false).await;
    deliver(&smtp, "Bob <bob@y.org>", &me, "Report", false).await;
    deliver(
        &smtp,
        "News <news@news.example>",
        &me,
        "Weekly digest",
        true,
    )
    .await;

    let store = Arc::new(Mutex::new(Store::open_in_memory().unwrap()));
    let engine = Engine::new(store, Arc::new(MemorySecrets::default()));
    let account = engine
        .add_password_account(AccountKind::Imap, &me, "pw", "", imap.clone(), smtp.clone())
        .await
        .unwrap();
    assert_eq!(account.label, "example.com");

    // First sync: everything in, nobody screened, nothing to notify about.
    let report = engine.run(account.id, true).await.unwrap();
    assert_eq!(report.new, 3, "{report:?}");
    assert!(report.arrived.is_empty());
    assert_eq!(
        find(&engine, "Weekly digest").category,
        Category::Newsletters
    );
    let lunch = find(&engine, "Lunch?");
    assert!(lunch.unread && !lunch.screening);
    assert_eq!(lunch.from_name, "Ana Ruiz");

    // A new sender after the first sync waits in the Gatekeeper.
    deliver(&smtp, "Stranger <s@z.net>", &me, "Hi there", false).await;
    let report = engine.run(account.id, true).await.unwrap();
    assert_eq!(report.new, 1);
    let hi = find(&engine, "Hi there");
    assert!(hi.screening);
    assert_eq!(report.arrived, [hi.id]);

    // Done + read is pushed as an archive move with \Seen.
    back_in_time(
        &engine,
        lunch.id,
        Patch {
            state: Some(MessageState::Done),
            unread: Some(false),
            ..Default::default()
        },
    );
    let report = engine.run(account.id, false).await.unwrap();
    assert_eq!(report.pushed, 1);
    let mut check = imap::login(&imap, &me, "pw").await.unwrap();
    let archive = check.select("Archive").await.unwrap();
    assert_eq!(archive.exists, 1);
    let flags: Vec<_> = check
        .fetch("1", "FLAGS")
        .await
        .unwrap()
        .try_collect()
        .await
        .unwrap();
    assert!(flags[0].flags().any(|f| f == Flag::Seen));
    assert_eq!(check.select("INBOX").await.unwrap().exists, 3);

    // A sync now changes nothing: the message stays Done.
    let report = engine.run(account.id, true).await.unwrap();
    assert_eq!((report.new, report.gone), (0, 0));
    assert_eq!(find(&engine, "Lunch?").state, MessageState::Done);

    // Moving it back to the inbox: pushed, then found again by Message-ID.
    back_in_time(
        &engine,
        lunch.id,
        Patch {
            state: Some(MessageState::Inbox),
            ..Default::default()
        },
    );
    assert_eq!(engine.run(account.id, false).await.unwrap().pushed, 1);
    assert_eq!(check.select("INBOX").await.unwrap().exists, 4);
    let report = engine.run(account.id, true).await.unwrap();
    assert_eq!(report.new, 0, "the moved message is the same message");
    assert_eq!(engine.store().messages().unwrap().len(), 4);
    assert_eq!(find(&engine, "Lunch?").state, MessageState::Inbox);

    // Archived on another device: Done here.
    check.select("INBOX").await.unwrap();
    let report_uids = check.uid_search("SUBJECT \"Report\"").await.unwrap();
    let uid = report_uids.into_iter().next().unwrap();
    check.uid_mv(uid.to_string(), "Archive").await.unwrap();
    let report = engine.run(account.id, true).await.unwrap();
    assert_eq!(report.gone, 1);
    assert_eq!(find(&engine, "Report").state, MessageState::Done);

    // Replying to Ana: sent over SMTP, threaded into her conversation, and Ana is accepted.
    let draft = Draft {
        account: account.id,
        to: format!("Me <{me}>"),
        cc: String::new(),
        bcc: String::new(),
        subject: "Re: Lunch?".into(),
        body: "Sure, noon?".into(),
        reply_to: Some(lunch.id),
        send_at: now_ms(),
        attachments: Vec::new(),
        draft_id: None,
        in_reply_to: None,
        references: None,
    };
    let queued = engine
        .store()
        .queue_send(
            account.id,
            &serde_json::to_string(&draft).unwrap(),
            now_ms(),
            &[],
        )
        .unwrap();
    let outcomes = engine.send_due(account.id).await.unwrap();
    assert_eq!(outcomes.len(), 1);
    assert_eq!(outcomes[0].id, queued);
    assert!(
        matches!(outcomes[0].result, Ok(true)),
        "{:?}",
        outcomes[0].result
    );
    engine.run(account.id, true).await.unwrap();
    let reply = find(&engine, "Re: Lunch?");
    assert!(!reply.screening, "your own mail is never screened");
    assert_eq!(reply.conversation, lunch.conversation);
    let content = engine.store().content(reply.id).unwrap().unwrap();
    assert!(
        content.text.starts_with("Sure, noon?"),
        "{:?}",
        content.text
    );
    assert!(
        content.text.contains("> Body of Lunch?"),
        "{:?}",
        content.text
    );

    // A PDF comes in: listed with its size, and downloadable on demand,
    // also after it was archived (found again by Message-ID).
    let pdf = b"%PDF-1.7\n% quote\n".to_vec();
    let quote = lettre::Message::builder()
        .from("Vendor <quotes@vendor.example>".parse().unwrap())
        .to(me.parse().unwrap())
        .subject("Quote 42")
        .message_id(None)
        .multipart(
            MultiPart::mixed()
                .singlepart(SinglePart::plain(String::from("Quote attached.")))
                .singlepart(
                    Attachment::new("quote 42.pdf".into())
                        .body(pdf.clone(), ContentType::parse("application/pdf").unwrap()),
                ),
        )
        .unwrap();
    smtp::send(&smtp, "sender", "pw", quote).await.unwrap();
    engine.run(account.id, true).await.unwrap();
    let quote = find(&engine, "Quote 42");
    assert_eq!(quote.attachment.as_deref(), Some("quote 42.pdf"));
    let listed = engine
        .store()
        .content(quote.id)
        .unwrap()
        .unwrap()
        .attachments;
    assert_eq!(listed[0].mime, "application/pdf");
    assert_eq!(listed[0].size, pdf.len() as u64);
    assert_eq!(engine.attachment(quote.id, 0).await.unwrap().bytes, pdf);
    back_in_time(
        &engine,
        quote.id,
        Patch {
            state: Some(MessageState::Done),
            ..Default::default()
        },
    );
    assert_eq!(engine.run(account.id, false).await.unwrap().pushed, 1);
    assert_eq!(engine.attachment(quote.id, 0).await.unwrap().bytes, pdf);

    // Forwarding it with a file from disk: both go out, in order.
    let forward = Draft {
        account: account.id,
        to: me.clone(),
        cc: String::new(),
        bcc: String::new(),
        subject: "Fwd: Quote 42".into(),
        body: "See the quote.".into(),
        reply_to: None,
        send_at: now_ms(),
        attachments: vec![
            OutgoingAttachment::Message {
                message: quote.id,
                index: 0,
                name: "quote 42.pdf".into(),
                size: pdf.len() as u64,
            },
            OutgoingAttachment::File {
                path: "notes.txt".into(),
                name: "notes.txt".into(),
                size: 5,
            },
        ],
        draft_id: None,
        in_reply_to: None,
        references: None,
    };
    let notes = OutboxFile {
        name: "notes.txt".into(),
        mime: "text/plain".into(),
        data: b"notes".to_vec(),
    };
    engine
        .store()
        .queue_send(
            account.id,
            &serde_json::to_string(&forward).unwrap(),
            now_ms(),
            &[notes],
        )
        .unwrap();
    let outcomes = engine.send_due(account.id).await.unwrap();
    assert!(
        matches!(outcomes[0].result, Ok(true)),
        "{:?}",
        outcomes[0].result
    );
    engine.run(account.id, true).await.unwrap();
    let forwarded = find(&engine, "Fwd: Quote 42");
    let names: Vec<String> = engine
        .store()
        .content(forwarded.id)
        .unwrap()
        .unwrap()
        .attachments
        .into_iter()
        .map(|a| a.name)
        .collect();
    assert_eq!(names, ["quote 42.pdf", "notes.txt"]);
    assert_eq!(engine.attachment(forwarded.id, 0).await.unwrap().bytes, pdf);
    assert_eq!(
        engine.attachment(forwarded.id, 1).await.unwrap().bytes,
        b"notes"
    );

    let _ = check.logout().await;
}

fn raw(message_id: &str, subject: &str) -> String {
    format!(
        "Message-ID: <{message_id}>\r\nFrom: Ana Ruiz <ana@x.com>\r\nTo: me@example.com\r\n\
         Subject: {subject}\r\nDate: Wed, 21 Oct 2026 10:00:00 +0000\r\n\r\n{subject} body\r\n"
    )
}

fn folder_id(engine: &Engine, name: &str) -> i64 {
    engine
        .store()
        .folders()
        .unwrap()
        .into_iter()
        .find(|f| f.name == name)
        .unwrap_or_else(|| panic!("no folder {name:?}"))
        .id
}

#[tokio::test]
async fn imap_folders() {
    let Ok(host) = std::env::var("SPARK_TEST_GREENMAIL") else {
        eprintln!("skipped: set SPARK_TEST_GREENMAIL to a GreenMail host");
        return;
    };
    let imap = plain(&host, 3143);
    let smtp = plain(&host, 3025);
    let me = format!("folders{}@example.com", now_ms());
    deliver(
        &smtp,
        "Ana Ruiz <ana@x.com>",
        &me,
        "Flight to Lisbon",
        false,
    )
    .await;

    let store = Arc::new(Mutex::new(Store::open_in_memory().unwrap()));
    let engine = Engine::new(store, Arc::new(MemorySecrets::default()));
    let account = engine
        .add_password_account(AccountKind::Imap, &me, "pw", "", imap.clone(), smtp.clone())
        .await
        .unwrap();
    engine.run(account.id, true).await.unwrap();

    // Another client files mail: a Travel folder with its own message and
    // a copy of the inbox one (like a Gmail label), plus a Sent folder.
    let mut other = imap::login(&imap, &me, "pw").await.unwrap();
    other.create("Travel").await.unwrap();
    other.create("Sent").await.unwrap();
    other
        .append("Travel", None, None, raw("hotel@x.com", "Hotel booking"))
        .await
        .unwrap();
    other
        .append(
            "Sent",
            Some("(\\Seen)"),
            None,
            raw("reply@x.com", "Re: Flight"),
        )
        .await
        .unwrap();
    other.select("INBOX").await.unwrap();
    other.copy("1", "Travel").await.unwrap();

    let list = Round {
        list_folders: true,
        ..Default::default()
    };
    let report = engine.run_round(account.id, &list).await.unwrap();
    assert!(report.folders_changed);
    let folders = engine.store().folders().unwrap();
    let roles: Vec<(&str, Option<FolderRole>)> =
        folders.iter().map(|f| (f.name.as_str(), f.role)).collect();
    for expected in [
        ("Travel", None),
        ("Sent", Some(FolderRole::Sent)),
        ("Archive", Some(FolderRole::Archive)),
        ("Trash", Some(FolderRole::Trash)),
    ] {
        assert!(roles.contains(&expected), "{expected:?} in {roles:?}");
    }

    // Syncing the folders: Travel's own mail is Filed, the inbox copy stays
    // in the inbox, and Sent mail is Filed too.
    let travel = folder_id(&engine, "Travel");
    let sent = folder_id(&engine, "Sent");
    let sync = Round {
        folders: vec![travel, sent],
        ..Default::default()
    };
    let report = engine.run_round(account.id, &sync).await.unwrap();
    assert_eq!(report.folder_changes, 3, "{report:?}");
    assert!(report.folders_changed, "first sync of the folders");
    assert!(
        report.folder_errors.is_empty(),
        "{:?}",
        report.folder_errors
    );
    let hotel = find(&engine, "Hotel booking");
    assert_eq!(hotel.state, MessageState::Filed);
    assert_eq!(hotel.folders, [travel]);
    let flight = find(&engine, "Flight to Lisbon");
    assert_eq!(flight.state, MessageState::Inbox);
    assert_eq!(flight.folders, [travel]);
    let reply = find(&engine, "Re: Flight");
    assert!(reply.state == MessageState::Filed && !reply.unread);
    assert_eq!(engine.store().messages().unwrap().len(), 3);

    // Synced again: nothing changes.
    let report = engine.run_round(account.id, &sync).await.unwrap();
    assert_eq!(report.folder_changes, 0, "{report:?}");
    assert!(!report.folders_changed);

    // A folder renamed elsewhere while a change to its mail waits: the
    // change is dropped and the account keeps syncing.
    other.create("Gone").await.unwrap();
    other
        .append("Gone", None, None, raw("gone@x.com", "Gone soon"))
        .await
        .unwrap();
    engine.run_round(account.id, &list).await.unwrap();
    let gone = Round {
        folders: vec![folder_id(&engine, "Gone")],
        ..Default::default()
    };
    engine.run_round(account.id, &gone).await.unwrap();
    let doomed = find(&engine, "Gone soon");
    back_in_time(
        &engine,
        doomed.id,
        Patch {
            unread: Some(false),
            ..Default::default()
        },
    );
    other.rename("Gone", "Elsewhere").await.unwrap();
    let report = engine.run(account.id, true).await.unwrap();
    assert_eq!(report.pushed, 0);
    assert!(
        engine
            .store()
            .pending(account.id, now_ms(), 0)
            .unwrap()
            .is_empty()
    );

    // Read in place, then Done: the flag lands in Travel, then it moves to Archive.
    back_in_time(
        &engine,
        hotel.id,
        Patch {
            unread: Some(false),
            ..Default::default()
        },
    );
    assert_eq!(engine.run(account.id, false).await.unwrap().pushed, 1);
    let examined = other.examine("Travel").await.unwrap();
    assert_eq!(examined.exists, 2);
    let seen: Vec<_> = other
        .uid_search("SEEN SUBJECT \"Hotel\"")
        .await
        .unwrap()
        .into_iter()
        .collect();
    assert_eq!(seen.len(), 1);
    // Done takes it out of Travel's list at once; the sync agrees.
    back_in_time(
        &engine,
        hotel.id,
        Patch {
            state: Some(MessageState::Done),
            ..Default::default()
        },
    );
    assert!(find(&engine, "Hotel booking").folders.is_empty());
    assert_eq!(engine.run(account.id, false).await.unwrap().pushed, 1);
    assert_eq!(other.examine("Travel").await.unwrap().exists, 1);
    assert_eq!(other.examine("Archive").await.unwrap().exists, 1);
    let report = engine.run_round(account.id, &sync).await.unwrap();
    assert_eq!(report.folder_changes, 0, "{report:?}");
    assert!(find(&engine, "Hotel booking").folders.is_empty());

    // Folders made, renamed and deleted from here.
    let receipts = engine
        .create_folder(account.id, None, "Reçus")
        .await
        .unwrap();
    let nested = engine
        .create_folder(account.id, Some(receipts), "2026")
        .await
        .unwrap();
    assert_eq!(
        engine.store().folder(nested).unwrap().unwrap().parent_id,
        Some(receipts)
    );
    engine.rename_folder(receipts, "Bills").await.unwrap();
    let renamed = engine.store().folder(receipts).unwrap().unwrap();
    assert_eq!(renamed.name, "Bills");
    let child = engine.store().folder(nested).unwrap().unwrap();
    assert!(child.remote_id.starts_with("Bills"), "{child:?}");
    // The server agrees after a fresh listing, and ids stay.
    engine.run_round(account.id, &list).await.unwrap();
    assert_eq!(folder_id(&engine, "Bills"), receipts);
    assert_eq!(folder_id(&engine, "2026"), nested);
    engine.delete_folder(receipts).await.unwrap();
    assert!(engine.store().folder(nested).unwrap().is_none());
    engine.run_round(account.id, &list).await.unwrap();
    assert!(
        engine
            .store()
            .folders()
            .unwrap()
            .iter()
            .all(|f| f.name != "Bills" && f.name != "2026")
    );

    let _ = other.logout().await;
}

/// How many messages with this subject the server has in `mailbox`.
async fn count(session: &mut imap::Session, mailbox: &str, subject: &str) -> usize {
    session.examine(mailbox).await.unwrap();
    session
        .uid_search(format!("SUBJECT \"{subject}\""))
        .await
        .unwrap()
        .len()
}

#[tokio::test]
async fn imap_moves_between_folders() {
    let Ok(host) = std::env::var("SPARK_TEST_GREENMAIL") else {
        eprintln!("skipped: set SPARK_TEST_GREENMAIL to a GreenMail host");
        return;
    };
    let imap = plain(&host, 3143);
    let smtp = plain(&host, 3025);
    let me = format!("moves{}@example.com", now_ms());
    deliver(
        &smtp,
        "Shop <billing@shop.example>",
        &me,
        "Invoice 7",
        false,
    )
    .await;

    let store = Arc::new(Mutex::new(Store::open_in_memory().unwrap()));
    let engine = Engine::new(store.clone(), Arc::new(MemorySecrets::default()));
    let account = engine
        .add_password_account(AccountKind::Imap, &me, "pw", "", imap.clone(), smtp.clone())
        .await
        .unwrap();
    engine.run(account.id, true).await.unwrap();
    let mut other = imap::login(&imap, &me, "pw").await.unwrap();
    other.create("Receipts").await.unwrap();
    other.create("Travel").await.unwrap();
    let list = Round {
        list_folders: true,
        ..Default::default()
    };
    engine.run_round(account.id, &list).await.unwrap();
    let receipts = folder_id(&engine, "Receipts");
    let travel = folder_id(&engine, "Travel");
    let both = Round {
        fetch: true,
        folders: vec![receipts, travel],
        ..Default::default()
    };
    engine.run_round(account.id, &both).await.unwrap();
    let invoice = find(&engine, "Invoice 7");
    let file_into = |folder: i64| Patch {
        folder: Some(folder),
        ..Default::default()
    };

    // Inbox → Receipts: listed there at once, moved on the next round.
    let moved = engine
        .store()
        .update(invoice.id, &file_into(receipts), now_ms() - 60_000)
        .unwrap()
        .unwrap();
    assert_eq!(moved.state, MessageState::Filed);
    assert_eq!(
        (moved.folder, moved.folders),
        (Some(receipts), vec![receipts])
    );
    let report = engine.run_round(account.id, &both).await.unwrap();
    assert_eq!(report.pushed, 1);
    assert_eq!(count(&mut other, "INBOX", "Invoice 7").await, 0);
    assert_eq!(count(&mut other, "Receipts", "Invoice 7").await, 1);
    let synced = find(&engine, "Invoice 7");
    assert_eq!(
        (synced.folder, synced.folders),
        (Some(receipts), vec![receipts])
    );
    let at = engine.store().location(invoice.id).unwrap().unwrap();
    assert_eq!(at.mailbox.as_deref(), Some("Receipts"));
    assert!(at.uid.is_some(), "found again by the folder sync");

    // Receipts → Travel, with both folders synced before the push: the
    // move still shows, and the server is untouched until it's pushed.
    engine
        .store()
        .update(invoice.id, &file_into(travel), now_ms())
        .unwrap();
    let sync_only = Round {
        folders: vec![receipts, travel],
        ..Default::default()
    };
    let report = engine.run_round(account.id, &sync_only).await.unwrap();
    assert_eq!(report.pushed, 0, "still undoable");
    let waiting = find(&engine, "Invoice 7");
    assert_eq!(
        (waiting.folder, waiting.folders),
        (Some(travel), vec![travel])
    );
    assert_eq!(count(&mut other, "Receipts", "Invoice 7").await, 1);
    let state: imap::SyncState = serde_json::from_str(
        &engine
            .store()
            .account(account.id)
            .unwrap()
            .unwrap()
            .sync_state,
    )
    .unwrap();
    let pending = engine.store().pending(account.id, now_ms(), 0).unwrap();
    let mut session = imap::login(&imap, &me, "pw").await.unwrap();
    assert_eq!(
        imap::push(&mut session, &store, &state, pending)
            .await
            .unwrap(),
        1
    );
    let _ = session.logout().await;
    assert_eq!(count(&mut other, "Receipts", "Invoice 7").await, 0);
    assert_eq!(count(&mut other, "Travel", "Invoice 7").await, 1);
    engine.run_round(account.id, &both).await.unwrap();
    let synced = find(&engine, "Invoice 7");
    assert_eq!(
        (synced.folder, synced.folders),
        (Some(travel), vec![travel])
    );

    // Undo, back to the inbox: the same message, not a new one.
    back_in_time(
        &engine,
        invoice.id,
        Patch {
            state: Some(MessageState::Inbox),
            ..Default::default()
        },
    );
    let report = engine.run_round(account.id, &both).await.unwrap();
    assert_eq!((report.pushed, report.new), (1, 0));
    assert_eq!(count(&mut other, "Travel", "Invoice 7").await, 0);
    assert_eq!(count(&mut other, "INBOX", "Invoice 7").await, 1);
    let back = find(&engine, "Invoice 7");
    assert_eq!(back.state, MessageState::Inbox);
    assert_eq!((back.folder, back.folders), (None, vec![]));
    assert_eq!(engine.store().messages().unwrap().len(), 1);

    // Filed mail marked Done, pushed, then undone: back in its folder.
    back_in_time(&engine, invoice.id, file_into(travel));
    engine.run_round(account.id, &both).await.unwrap();
    back_in_time(
        &engine,
        invoice.id,
        Patch {
            state: Some(MessageState::Done),
            ..Default::default()
        },
    );
    assert_eq!(engine.run_round(account.id, &both).await.unwrap().pushed, 1);
    assert_eq!(count(&mut other, "Travel", "Invoice 7").await, 0);
    assert_eq!(count(&mut other, "Archive", "Invoice 7").await, 1);
    back_in_time(
        &engine,
        invoice.id,
        Patch {
            state: Some(MessageState::Filed),
            folder: Some(travel),
            ..Default::default()
        },
    );
    assert_eq!(engine.run_round(account.id, &both).await.unwrap().pushed, 1);
    assert_eq!(count(&mut other, "Archive", "Invoice 7").await, 0);
    assert_eq!(count(&mut other, "Travel", "Invoice 7").await, 1);
    let filed = find(&engine, "Invoice 7");
    assert_eq!(filed.state, MessageState::Filed);
    assert_eq!((filed.folder, filed.folders), (Some(travel), vec![travel]));
    assert!(
        engine
            .store()
            .pending(account.id, now_ms(), 0)
            .unwrap()
            .is_empty()
    );

    let _ = other.logout().await;
}

/// The Drafts folder's messages as (UID, \Draft and \Seen set, raw text).
async fn drafts_on_server(session: &mut imap::Session) -> Vec<(u32, bool, String)> {
    let exists = session.examine("Drafts").await.unwrap().exists;
    if exists == 0 {
        return Vec::new();
    }
    let fetched: Vec<_> = session
        .fetch("1:*", "(UID FLAGS BODY.PEEK[])")
        .await
        .unwrap()
        .try_collect()
        .await
        .unwrap();
    fetched
        .iter()
        .map(|f| {
            let flags: Vec<_> = f.flags().collect();
            (
                f.uid.unwrap(),
                flags.contains(&Flag::Draft) && flags.contains(&Flag::Seen),
                String::from_utf8_lossy(f.body().unwrap_or_default()).into_owned(),
            )
        })
        .collect()
}

#[tokio::test]
async fn imap_drafts() {
    let Ok(host) = std::env::var("SPARK_TEST_GREENMAIL") else {
        eprintln!("skipped: set SPARK_TEST_GREENMAIL to a GreenMail host");
        return;
    };
    let imap = plain(&host, 3143);
    let smtp = plain(&host, 3025);
    let me = format!("drafts{}@example.com", now_ms());
    deliver(&smtp, "Ana Ruiz <ana@x.com>", &me, "Plans", false).await;

    let store = Arc::new(Mutex::new(Store::open_in_memory().unwrap()));
    let engine = Engine::new(store, Arc::new(MemorySecrets::default()));
    let account = engine
        .add_password_account(AccountKind::Imap, &me, "pw", "", imap.clone(), smtp.clone())
        .await
        .unwrap();
    let first = Round {
        fetch: true,
        list_folders: true,
        ..Default::default()
    };
    engine.run_round(account.id, &first).await.unwrap();
    let plans = find(&engine, "Plans");
    let mut other = imap::login(&imap, &me, "pw").await.unwrap();

    // A reply saved as a draft (past the undo window): the server has no
    // Drafts folder, so one is made, and the draft goes up as \Draft.
    let mut draft = Draft {
        account: account.id,
        to: "Ana Ruiz <ana@x.com>".into(),
        cc: String::new(),
        bcc: "boss@x.com".into(),
        subject: "Re: Plans".into(),
        body: "First thoughts".into(),
        reply_to: Some(plans.id),
        send_at: 0,
        attachments: Vec::new(),
        draft_id: None,
        in_reply_to: None,
        references: None,
    };
    let save = |draft: &Draft| {
        let mut store = engine.store();
        drafts::save(&mut store, &account, draft, &[], now_ms() - 60_000).unwrap()
    };
    let id = save(&draft);
    let report = engine.run(account.id, false).await.unwrap();
    assert_eq!(report.drafts, 1, "{report:?}");
    assert!(
        report.folders_changed && report.folder_errors.is_empty(),
        "{report:?}"
    );
    let drafts_folder = folder_id(&engine, "Drafts");
    assert_eq!(
        engine.store().folder(drafts_folder).unwrap().unwrap().role,
        Some(FolderRole::Drafts)
    );
    let on_server = drafts_on_server(&mut other).await;
    assert_eq!(on_server.len(), 1);
    let (first_uid, draft_and_seen, raw) = &on_server[0];
    assert!(draft_and_seen, "\\Draft \\Seen");
    assert!(
        raw.contains("First thoughts") && raw.contains("Bcc: boss@x.com"),
        "{raw}"
    );
    let plans_id = engine
        .store()
        .headers(plans.id)
        .unwrap()
        .unwrap()
        .message_id
        .unwrap();
    assert!(raw.contains(&format!("In-Reply-To: <{plans_id}>")), "{raw}");
    let listed = find(&engine, "Re: Plans");
    assert_eq!(
        (listed.id, listed.folders.as_slice()),
        (id, [drafts_folder].as_slice())
    );
    assert_eq!(listed.to, ["Ana Ruiz <ana@x.com>"]);

    // Opened again it's the same draft, still a reply; edited and saved,
    // the new copy replaces the old one.
    let opened = engine.open_draft(id).unwrap().unwrap();
    assert_eq!(opened.body, "First thoughts");
    assert_eq!(opened.reply_to, Some(plans.id));
    assert_eq!(opened.draft_id, Some(id));
    draft = Draft {
        body: "Second thoughts".into(),
        ..opened
    };
    assert_eq!(save(&draft), id);
    assert_eq!(engine.run(account.id, false).await.unwrap().drafts, 2);
    let on_server = drafts_on_server(&mut other).await;
    assert_eq!(on_server.len(), 1, "exactly one copy remains");
    assert_ne!(on_server[0].0, *first_uid);
    assert!(on_server[0].2.contains("Second thoughts"));

    // Syncing Drafts finds that copy, without a second row.
    let sync = Round {
        folders: vec![drafts_folder],
        ..Default::default()
    };
    engine.run_round(account.id, &sync).await.unwrap();
    let in_drafts: Vec<_> = engine
        .store()
        .messages()
        .unwrap()
        .into_iter()
        .filter(|m| m.folders.contains(&drafts_folder))
        .collect();
    assert_eq!(in_drafts.len(), 1, "{in_drafts:?}");
    assert_eq!(in_drafts[0].id, id);

    // Sent, then undone: the draft is still there.
    let send = |draft: &Draft| {
        let mut store = engine.store();
        let json = serde_json::to_string(draft).unwrap();
        let queued = store.queue_send(account.id, &json, now_ms(), &[]).unwrap();
        store.hold_draft_for_send(queued, id).unwrap();
        queued
    };
    let queued = send(&draft);
    assert!(
        engine
            .store()
            .messages()
            .unwrap()
            .iter()
            .all(|m| m.id != id)
    );
    assert!(engine.store().cancel_send(queued).unwrap());
    assert_eq!(find(&engine, "Re: Plans").id, id);
    assert_eq!(drafts_on_server(&mut other).await.len(), 1);

    // Sent for real: it goes out threaded, and the draft is deleted.
    send(&draft);
    let outcomes = engine.send_due(account.id).await.unwrap();
    assert!(
        matches!(outcomes[0].result, Ok(true)),
        "{:?}",
        outcomes[0].result
    );
    assert!(engine.store().message(id).unwrap().is_none());
    let report = engine.run(account.id, false).await.unwrap();
    assert_eq!(report.drafts, 1, "{report:?}");
    assert!(drafts_on_server(&mut other).await.is_empty());

    // A new draft discarded after it went up is deleted there too.
    let discarded = save(&Draft {
        reply_to: None,
        subject: "Never mind".into(),
        draft_id: None,
        ..draft.clone()
    });
    engine.run(account.id, false).await.unwrap();
    assert_eq!(drafts_on_server(&mut other).await.len(), 1);
    assert!(engine.store().discard_draft(discarded).unwrap());
    engine.run(account.id, false).await.unwrap();
    assert!(drafts_on_server(&mut other).await.is_empty());

    let _ = other.logout().await;
}

/// Appends messages "Old 0"… to `mailbox`, oldest first.
async fn append_many(session: &mut imap::Session, mailbox: &str, count: usize) {
    for n in 0..count {
        session
            .append(
                mailbox,
                None,
                None,
                raw(&format!("old{n}@x.com"), &format!("Old {n}")),
            )
            .await
            .unwrap();
    }
}

fn account_state(engine: &Engine, account_id: i64) -> imap::SyncState {
    let account = engine.store().account(account_id).unwrap().unwrap();
    serde_json::from_str(&account.sync_state).unwrap()
}

#[tokio::test]
async fn imap_inbox_reaches_back() {
    let Ok(host) = std::env::var("SPARK_TEST_GREENMAIL") else {
        eprintln!("skipped: set SPARK_TEST_GREENMAIL to a GreenMail host");
        return;
    };
    let imap = plain(&host, 3143);
    let smtp = plain(&host, 3025);
    let me = format!("older{}@example.com", now_ms());
    let total = imap::FIRST_SYNC_LIMIT + OLDER_STEP + 20;
    let mut other = imap::login(&imap, &me, "pw").await.unwrap();
    append_many(&mut other, "INBOX", total).await;

    let store = Arc::new(Mutex::new(Store::open_in_memory().unwrap()));
    let engine = Engine::new(store, Arc::new(MemorySecrets::default()));
    let account = engine
        .add_password_account(AccountKind::Imap, &me, "pw", "", imap.clone(), smtp.clone())
        .await
        .unwrap();
    let has_older = || inbox_has_older(&engine.store().account(account.id).unwrap().unwrap());
    assert!(!has_older(), "nothing synced yet");

    // The first sync takes the newest 300, and knows there's more.
    let report = engine.run(account.id, true).await.unwrap();
    assert_eq!(report.new, imap::FIRST_SYNC_LIMIT);
    let first = account_state(&engine, account.id);
    assert!(!first.complete);
    assert!(has_older());

    // Reaching back: the next 100 below the floor, stored quietly.
    let older = Round {
        older_inbox: true,
        ..Default::default()
    };
    let report = engine.run_round(account.id, &older).await.unwrap();
    assert_eq!(report.older, OLDER_STEP, "{report:?}");
    assert!(report.changed());
    assert!(report.arrived.is_empty() && report.new == 0);
    let reached = account_state(&engine, account.id);
    assert!(reached.floor < first.floor);
    assert!(!reached.complete && has_older());
    let old = find(&engine, "Old 20");
    assert!(old.state == MessageState::Inbox && !old.screening);
    let messages = engine.store().messages().unwrap();
    assert!(messages.iter().all(|m| m.subject != "Old 19"));

    // The last 20: nothing older is left.
    let report = engine.run_round(account.id, &older).await.unwrap();
    assert_eq!(report.older, 20);
    assert!(account_state(&engine, account.id).complete);
    assert!(!has_older());
    assert_eq!(engine.store().messages().unwrap().len(), total);

    // The next sync tracks them like the rest: none gone, flags followed.
    other.select("INBOX").await.unwrap();
    let uids = other
        .uid_search("HEADER Message-ID \"old3@x.com\"")
        .await
        .unwrap();
    let uid = uids.into_iter().next().unwrap();
    let _: Vec<_> = other
        .uid_store(uid.to_string(), "+FLAGS.SILENT (\\Seen)")
        .await
        .unwrap()
        .try_collect()
        .await
        .unwrap();
    let report = engine.run(account.id, true).await.unwrap();
    assert_eq!((report.new, report.gone), (0, 0), "{report:?}");
    let old = find(&engine, "Old 3");
    assert!(old.state == MessageState::Inbox && !old.unread);
    assert!(account_state(&engine, account.id).complete);

    // Nothing further back: reaching back changes nothing.
    let report = engine.run_round(account.id, &older).await.unwrap();
    assert_eq!(report.older, 0);
    assert!(!report.changed());

    let _ = other.logout().await;
}

#[tokio::test]
async fn imap_folders_reach_back() {
    let Ok(host) = std::env::var("SPARK_TEST_GREENMAIL") else {
        eprintln!("skipped: set SPARK_TEST_GREENMAIL to a GreenMail host");
        return;
    };
    let imap = plain(&host, 3143);
    let smtp = plain(&host, 3025);
    let me = format!("olderfolders{}@example.com", now_ms());
    let total = imap::FOLDER_LIMIT as usize + 50;
    let mut other = imap::login(&imap, &me, "pw").await.unwrap();
    other.create("Projects").await.unwrap();
    append_many(&mut other, "Projects", total).await;

    let store = Arc::new(Mutex::new(Store::open_in_memory().unwrap()));
    let engine = Engine::new(store, Arc::new(MemorySecrets::default()));
    let account = engine
        .add_password_account(AccountKind::Imap, &me, "pw", "", imap.clone(), smtp.clone())
        .await
        .unwrap();
    let list = Round {
        list_folders: true,
        ..Default::default()
    };
    engine.run_round(account.id, &list).await.unwrap();
    let projects = folder_id(&engine, "Projects");
    let folder = || engine.store().folder(projects).unwrap().unwrap();
    let members = || {
        let messages = engine.store().messages().unwrap();
        messages
            .iter()
            .filter(|m| m.folders.contains(&projects))
            .count()
    };
    assert!(!folder_has_older(&folder()), "not synced yet");

    // Synced: the newest 200, and there's more.
    let sync = Round {
        folders: vec![projects],
        ..Default::default()
    };
    let report = engine.run_round(account.id, &sync).await.unwrap();
    assert_eq!(report.folder_changes, imap::FOLDER_LIMIT as usize);
    assert!(folder_has_older(&folder()));

    // Reaching back brings in the other 50, and the folder list hears that
    // nothing's left.
    let older = Round {
        older_folders: vec![projects],
        ..Default::default()
    };
    let report = engine.run_round(account.id, &older).await.unwrap();
    assert_eq!(report.folder_changes, 50, "{report:?}");
    assert!(report.folders_changed);
    assert!(!folder_has_older(&folder()));
    let state: imap::FolderState = serde_json::from_str(&folder().sync_state).unwrap();
    assert_eq!(state.limit, Some(imap::FOLDER_LIMIT + OLDER_STEP as u32));
    assert_eq!(members(), total);
    assert_eq!(find(&engine, "Old 0").state, MessageState::Filed);

    // Later syncs keep all of it.
    let report = engine.run_round(account.id, &sync).await.unwrap();
    assert_eq!(report.folder_changes, 0, "{report:?}");
    assert!(!report.folders_changed);
    assert_eq!(members(), total);

    let _ = other.logout().await;
}
