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
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_imap::types::Flag;
use futures_util::TryStreamExt;
use lettre::message::header::{ContentType, HeaderName, HeaderValue};
use lettre::message::{Attachment, MultiPart, SinglePart};
use spark_mail::draft::{Draft, OutgoingAttachment};
use spark_mail::engine::{folder_has_older, inbox_has_older};
use spark_mail::imap::{self, Security, Server};
use spark_mail::{Engine, OLDER_STEP, Round, Secrets, Watch, drafts, smtp};
use spark_store::{
    AccountKind, Category, Change, Decision, FolderRole, MessageState, OutboxFile, Patch, Screen,
    Store, now_ms,
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
        trusted_cert: None,
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
    let secrets = Arc::new(MemorySecrets::default());
    let engine = Engine::new(store.clone(), secrets.clone());
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
    // The signature goes between the reply and the quote, its logo as an inline part.
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
        signature: Some(
            r#"<div><b>Me</b></div><div>ACME</div><img src="data:image/png;base64,iVBORw0KGgo=">"#
                .into(),
        ),
        ..Default::default()
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
        content
            .text
            .replace("\r\n", "\n")
            .starts_with("Sure, noon?\n\nMe\nACME\n\n"),
        "{:?}",
        content.text
    );
    let html = content.html.as_deref().unwrap_or_default();
    assert!(
        html.contains("<div name=\"messageSignatureSection\"><div><b>Me</b></div><div>ACME</div><img src=\"cid:"),
        "{html}"
    );
    assert!(content.attachments.is_empty(), "the logo isn't a file");
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
        ..Default::default()
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

    // Kept once fetched: the whole email comes, so both its files are kept.
    let files = std::env::temp_dir().join(format!("spark-greenmail-files-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&files);
    let cached = Engine::new(store.clone(), secrets.clone()).with_attachment_cache(files.clone());
    assert_eq!(cached.prefetch_attachments(forwarded.id).await.unwrap(), 2);
    assert_eq!(
        cached.prefetch_attachments(forwarded.id).await.unwrap(),
        0,
        "both are kept"
    );
    assert_eq!(
        cached.attachment(forwarded.id, 1).await.unwrap().bytes,
        b"notes"
    );
    // Save as .eml: the email as the server has it.
    let raw = cached.raw_message(forwarded.id).await.unwrap();
    assert!(String::from_utf8_lossy(&raw).contains("Subject: Fwd: Quote 42"));
    let _ = std::fs::remove_dir_all(&files);

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
    let mut state: imap::SyncState = serde_json::from_str(
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
        imap::push(&mut session, &store, &mut state, pending)
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
        ..Default::default()
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

    // Reaching back: the next 100 below the floor, stored quietly. GreenMail
    // can't send the start of a text as IMAP says to (no space before its
    // literal), which loses the connection: the first try fails, and asks
    // for less from then on.
    let older = Round {
        older_inbox: true,
        ..Default::default()
    };
    assert!(engine.run_round(account.id, &older).await.is_err());
    let after = account_state(&engine, account.id);
    assert_eq!(after.heads, imap::Heads::Structure);
    assert_eq!(after.floor, first.floor, "nothing passed");
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

/// Changes a flag of the message with `message_id` in `mailbox`, as another
/// client does.
async fn store_flag(session: &mut imap::Session, mailbox: &str, message_id: &str, change: &str) {
    session.select(mailbox).await.unwrap();
    let uids = session
        .uid_search(format!("HEADER Message-ID \"{message_id}\""))
        .await
        .unwrap();
    let uid = uids.into_iter().next().unwrap();
    let _: Vec<_> = session
        .uid_store(uid.to_string(), change)
        .await
        .unwrap()
        .try_collect()
        .await
        .unwrap();
}

/// Deletes the message with `message_id` in `mailbox` for good, as another
/// client does.
async fn expunge(session: &mut imap::Session, mailbox: &str, message_id: &str) {
    store_flag(session, mailbox, message_id, "+FLAGS.SILENT (\\Deleted)").await;
    let _: Vec<_> = session
        .expunge()
        .await
        .unwrap()
        .try_collect()
        .await
        .unwrap();
}

#[tokio::test]
async fn imap_follows_changes_made_elsewhere() {
    let Ok(host) = std::env::var("SPARK_TEST_GREENMAIL") else {
        eprintln!("skipped: set SPARK_TEST_GREENMAIL to a GreenMail host");
        return;
    };
    let imap = plain(&host, 3143);
    let smtp = plain(&host, 3025);
    let me = format!("elsewhere{}@example.com", now_ms());
    let mut other = imap::login(&imap, &me, "pw").await.unwrap();
    append_many(&mut other, "INBOX", 5).await;
    other.create("Projects").await.unwrap();
    for n in 0..3 {
        let (id, subject) = (format!("project{n}@x.com"), format!("Project {n}"));
        other
            .append("Projects", None, None, raw(&id, &subject))
            .await
            .unwrap();
    }

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
    assert_eq!(engine.run_round(account.id, &first).await.unwrap().new, 5);
    let projects = folder_id(&engine, "Projects");
    let both = Round {
        fetch: true,
        folders: vec![projects],
        ..Default::default()
    };
    let report = engine.run_round(account.id, &both).await.unwrap();
    assert_eq!(report.folder_changes, 3, "{report:?}");
    let report = engine.run_round(account.id, &both).await.unwrap();
    assert!(!report.changed(), "a quiet round: {report:?}");
    eprintln!("modseq: {:?}", account_state(&engine, account.id).modseq);
    let in_projects = |subject: &str| find(&engine, subject).folders.contains(&projects);

    // Read and flagged elsewhere, in the inbox and the folder.
    store_flag(&mut other, "INBOX", "old1@x.com", "+FLAGS.SILENT (\\Seen)").await;
    store_flag(
        &mut other,
        "INBOX",
        "old2@x.com",
        "+FLAGS.SILENT (\\Flagged)",
    )
    .await;
    store_flag(
        &mut other,
        "Projects",
        "project1@x.com",
        "+FLAGS.SILENT (\\Seen \\Flagged)",
    )
    .await;
    let report = engine.run_round(account.id, &both).await.unwrap();
    assert_eq!((report.new, report.gone), (0, 0), "{report:?}");
    assert!(!find(&engine, "Old 1").unread);
    assert!(find(&engine, "Old 2").pinned && find(&engine, "Old 2").unread);
    let project = find(&engine, "Project 1");
    assert!(project.pinned && !project.unread);

    // Marked unread again.
    store_flag(&mut other, "INBOX", "old1@x.com", "-FLAGS.SILENT (\\Seen)").await;
    engine.run_round(account.id, &both).await.unwrap();
    assert!(find(&engine, "Old 1").unread);

    // Deleted for good elsewhere: gone from the inbox and from the folder.
    expunge(&mut other, "INBOX", "old3@x.com").await;
    expunge(&mut other, "Projects", "project0@x.com").await;
    let report = engine.run_round(account.id, &both).await.unwrap();
    assert_eq!(report.gone, 1, "{report:?}");
    assert_eq!(report.folder_changes, 1, "{report:?}");
    assert_eq!(find(&engine, "Old 3").state, MessageState::Done);
    assert!(!in_projects("Project 0") && in_projects("Project 2"));

    // New mail still comes in.
    other
        .append("INBOX", None, None, raw("arrived@x.com", "Arrived"))
        .await
        .unwrap();
    let report = engine.run_round(account.id, &both).await.unwrap();
    assert_eq!(report.new, 1, "{report:?}");
    assert!(find(&engine, "Arrived").unread);

    // A cache from before incremental syncs: its first round fetches the
    // stored headers once and follows changes as before.
    let strip = |state: &str, keys: &[&str]| {
        let mut json: serde_json::Value = serde_json::from_str(state).unwrap();
        for key in keys {
            assert!(
                json.as_object_mut().unwrap().remove(*key).is_some(),
                "{key}"
            );
        }
        json.to_string()
    };
    let account_json = engine
        .store()
        .account(account.id)
        .unwrap()
        .unwrap()
        .sync_state;
    let old = strip(&account_json, &["replyToFetched", "modseq"]);
    engine.store().set_sync_state(account.id, &old).unwrap();
    let folder = engine.store().folder(projects).unwrap().unwrap();
    let old = strip(&folder.sync_state, &["replyToFetched"]);
    engine
        .store()
        .set_folder_synced(projects, &old, now_ms())
        .unwrap();
    store_flag(&mut other, "INBOX", "old4@x.com", "+FLAGS.SILENT (\\Seen)").await;
    store_flag(
        &mut other,
        "Projects",
        "project2@x.com",
        "+FLAGS.SILENT (\\Flagged)",
    )
    .await;
    let report = engine.run_round(account.id, &both).await.unwrap();
    assert!(!report.changed(), "{report:?}");
    assert!(!find(&engine, "Old 4").unread);
    assert!(find(&engine, "Project 2").pinned);
    let state = account_state(&engine, account.id);
    assert!(state.reply_to_fetched && state.floor.is_some());
    let report = engine.run_round(account.id, &both).await.unwrap();
    assert!(!report.changed(), "{report:?}");

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

#[tokio::test]
async fn imap_searches_reach_past_the_sync() {
    let Ok(host) = std::env::var("SPARK_TEST_GREENMAIL") else {
        eprintln!("skipped: set SPARK_TEST_GREENMAIL to a GreenMail host");
        return;
    };
    let imap = plain(&host, 3143);
    let smtp = plain(&host, 3025);
    let me = format!("search{}@example.com", now_ms());
    let mut other = imap::login(&imap, &me, "pw").await.unwrap();
    // An invoice older than the first sync reaches, and 300 newer mails.
    let invoice = raw("invoice@x.com", "Quarterly invoice");
    other.append("INBOX", None, None, invoice).await.unwrap();
    append_many(&mut other, "INBOX", imap::FIRST_SYNC_LIMIT).await;

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
    assert!(engine.store().search("invoice").unwrap().is_empty());
    // Archived elsewhere, in the Archive folder the first round made.
    let archived = raw("archived@x.com", "Archived invoice");
    other.append("Archive", None, None, archived).await.unwrap();

    // Both are found and stored where they are; the archive folder isn't synced.
    assert_eq!(
        engine.search_server(account.id, "invoice").await.unwrap(),
        2
    );
    let quarterly = find(&engine, "Quarterly invoice");
    assert!(quarterly.state == MessageState::Inbox && !quarterly.screening);
    assert_eq!(find(&engine, "Archived invoice").state, MessageState::Done);
    assert_eq!(engine.store().search("invoice").unwrap().len(), 2);

    // Asked again, the server finds nothing new.
    assert_eq!(
        engine.search_server(account.id, "invoice").await.unwrap(),
        0
    );
    assert_eq!(engine.search_server(account.id, "Old").await.unwrap(), 0);
    // Words from no mail, filters alone and senders that aren't there find nothing.
    for query in ["zebra", "is:unread", "invoice from:bob"] {
        assert_eq!(
            engine.search_server(account.id, query).await.unwrap(),
            0,
            "{query}"
        );
    }

    // The inbox sync leaves it be: it's below the floor, not gone.
    let report = engine.run(account.id, true).await.unwrap();
    assert_eq!((report.new, report.gone), (0, 0), "{report:?}");
    assert_eq!(
        find(&engine, "Quarterly invoice").state,
        MessageState::Inbox
    );

    // Reaching back gets past it without fetching it again.
    let older = Round {
        older_inbox: true,
        ..Default::default()
    };
    let report = engine.run_round(account.id, &older).await.unwrap();
    assert_eq!(report.older, 0, "{report:?}");
    assert!(account_state(&engine, account.id).complete);
    assert_eq!(
        engine.store().messages().unwrap().len(),
        imap::FIRST_SYNC_LIMIT + 2
    );

    let _ = other.logout().await;
}

#[tokio::test]
async fn imap_blocked_mail_moves_to_the_blocked_folder() {
    let Ok(host) = std::env::var("SPARK_TEST_GREENMAIL") else {
        eprintln!("skipped: set SPARK_TEST_GREENMAIL to a GreenMail host");
        return;
    };
    let imap = plain(&host, 3143);
    let smtp = plain(&host, 3025);
    let me = format!("blocked{}@example.com", now_ms());
    deliver(&smtp, "Ana <ana@x.com>", &me, "Known", false).await;

    let store = Arc::new(Mutex::new(Store::open_in_memory().unwrap()));
    let engine = Engine::new(store.clone(), Arc::new(MemorySecrets::default()));
    let account = engine
        .add_password_account(AccountKind::Imap, &me, "pw", "", imap.clone(), smtp.clone())
        .await
        .unwrap();
    engine.run(account.id, true).await.unwrap();

    deliver(&smtp, "Spam <spam@z.net>", &me, "Offer 1", false).await;
    deliver(&smtp, "Spam <spam@z.net>", &me, "Offer 2", false).await;
    engine.run(account.id, true).await.unwrap();
    let offer = find(&engine, "Offer 1");
    assert!(offer.screening);

    // Blocking the sender moves their mail into a new Blocked folder.
    let block = Change {
        screen: Screen::Sender("spam@z.net".into()),
        decision: Some(Decision::Blocked),
    };
    engine.store().decide(&[block], now_ms() - 60_000).unwrap();
    assert_eq!(engine.run(account.id, false).await.unwrap().pushed, 2);
    let mut other = imap::login(&imap, &me, "pw").await.unwrap();
    assert_eq!(other.select("Blocked").await.unwrap().exists, 2);
    assert_eq!(other.select("INBOX").await.unwrap().exists, 1);
    assert_eq!(
        account_state(&engine, account.id).blocked.as_deref(),
        Some("Blocked")
    );

    // Syncing again leaves it blocked, and the folder list shows it as Blocked.
    let round = Round {
        fetch: true,
        list_folders: true,
        ..Default::default()
    };
    let report = engine.run_round(account.id, &round).await.unwrap();
    assert_eq!((report.new, report.gone), (0, 0));
    assert_eq!(find(&engine, "Offer 2").state, MessageState::Blocked);
    let folder = engine
        .store()
        .folders()
        .unwrap()
        .into_iter()
        .find(|f| f.account_id == account.id && f.remote_id == "Blocked")
        .unwrap();
    assert_eq!(folder.role, Some(FolderRole::Blocked));

    // New mail from them is blocked on arrival, and moved once it settles.
    deliver(&smtp, "Spam <spam@z.net>", &me, "Offer 3", false).await;
    engine.run(account.id, true).await.unwrap();
    assert_eq!(find(&engine, "Offer 3").state, MessageState::Blocked);
    let mut state = account_state(&engine, account.id);
    let pending = engine.store().pending(account.id, now_ms() + 1, 0).unwrap();
    let mut session = imap::login(&imap, &me, "pw").await.unwrap();
    assert_eq!(
        imap::push(&mut session, &store, &mut state, pending)
            .await
            .unwrap(),
        1
    );
    let _ = session.logout().await;
    assert_eq!(other.select("Blocked").await.unwrap().exists, 3);

    // One email back to the Inbox from Blocked: moved back on the server.
    back_in_time(
        &engine,
        offer.id,
        Patch {
            state: Some(MessageState::Inbox),
            ..Default::default()
        },
    );
    assert_eq!(engine.run(account.id, false).await.unwrap().pushed, 1);
    assert_eq!(other.select("INBOX").await.unwrap().exists, 2);
    assert_eq!(other.select("Blocked").await.unwrap().exists, 2);
}

#[tokio::test]
async fn imap_deletes_for_good() {
    let Ok(host) = std::env::var("SPARK_TEST_GREENMAIL") else {
        eprintln!("skipped: set SPARK_TEST_GREENMAIL to a GreenMail host");
        return;
    };
    let imap = plain(&host, 3143);
    let smtp = plain(&host, 3025);
    let me = format!("purge{}@example.com", now_ms());
    for subject in ["Keep", "Trash me", "Junk A", "Back out"] {
        deliver(&smtp, "Ana Ruiz <ana@x.com>", &me, subject, false).await;
    }

    let store = Arc::new(Mutex::new(Store::open_in_memory().unwrap()));
    let engine = Engine::new(store, Arc::new(MemorySecrets::default()));
    let account = engine
        .add_password_account(AccountKind::Imap, &me, "pw", "", imap.clone(), smtp.clone())
        .await
        .unwrap();
    engine.run(account.id, true).await.unwrap();
    let mut other = imap::login(&imap, &me, "pw").await.unwrap();
    other.create("Junk").await.unwrap();
    // Mail in Junk that was never downloaded here goes too.
    other
        .append("Junk", None, None, raw("never-seen@x.com", "Unseen junk"))
        .await
        .unwrap();
    let list = Round {
        list_folders: true,
        ..Default::default()
    };
    engine.run_round(account.id, &list).await.unwrap();
    let junk = folder_id(&engine, "Junk");
    let spam = engine.store().folder(junk).unwrap().unwrap();
    assert_eq!(spam.role, Some(FolderRole::Spam));

    // Delete Forever on mail in Trash: gone from the server and from here.
    let trashed = find(&engine, "Trash me");
    back_in_time(
        &engine,
        trashed.id,
        Patch {
            state: Some(MessageState::Deleted),
            ..Default::default()
        },
    );
    assert_eq!(engine.run(account.id, false).await.unwrap().pushed, 1);
    assert_eq!(count(&mut other, "Trash", "Trash me").await, 1);
    assert_eq!(engine.delete_forever(&[trashed.id]).await.unwrap(), 1);
    assert_eq!(count(&mut other, "Trash", "Trash me").await, 0);
    assert!(engine.store().message(trashed.id).unwrap().is_none());

    // Two into Spam: one pushed, one moved back out before its push.
    let spam_patch = Patch {
        folder: Some(junk),
        ..Default::default()
    };
    let junk_a = find(&engine, "Junk A");
    let back_out = find(&engine, "Back out");
    back_in_time(&engine, junk_a.id, spam_patch.clone());
    back_in_time(&engine, back_out.id, spam_patch);
    assert_eq!(engine.run(account.id, false).await.unwrap().pushed, 2);
    engine
        .store()
        .update(
            back_out.id,
            &Patch {
                state: Some(MessageState::Inbox),
                ..Default::default()
            },
            now_ms(),
        )
        .unwrap();

    // Emptying Spam pushes that move first, so only what Spam shows goes.
    assert_eq!(engine.empty_folder(junk).await.unwrap(), 2);
    assert_eq!(other.examine("Junk").await.unwrap().exists, 0);
    assert!(engine.store().message(junk_a.id).unwrap().is_none());
    assert_eq!(count(&mut other, "INBOX", "Back out").await, 1);
    assert_eq!(find(&engine, "Back out").state, MessageState::Inbox);
    assert_eq!(find(&engine, "Keep").state, MessageState::Inbox);
    assert_eq!(count(&mut other, "INBOX", "Keep").await, 1);
}

#[tokio::test]
async fn imap_people_written_to_elsewhere_skip_the_gatekeeper() {
    let Ok(host) = std::env::var("SPARK_TEST_GREENMAIL") else {
        eprintln!("skipped: set SPARK_TEST_GREENMAIL to a GreenMail host");
        return;
    };
    let imap = plain(&host, 3143);
    let smtp = plain(&host, 3025);
    let me = format!("sent{}@example.com", now_ms());
    deliver(&smtp, "Ana <ana@x.com>", &me, "Known", false).await;
    // The user wrote to Dana from their phone: it's in Sent, not in spark-rs.
    let mut other = imap::login(&imap, &me, "pw").await.unwrap();
    other.create("Sent").await.unwrap();
    let to_dana = format!(
        "Message-ID: <to-dana@x.com>\r\nFrom: {me}\r\nTo: Dana <dana@fabrikam.org>\r\nSubject: Hello\r\n\r\nHi Dana\r\n"
    );
    other.append("Sent", None, None, to_dana).await.unwrap();

    let store = Arc::new(Mutex::new(Store::open_in_memory().unwrap()));
    let engine = Engine::new(store, Arc::new(MemorySecrets::default()));
    let account = engine
        .add_password_account(AccountKind::Imap, &me, "pw", "", imap.clone(), smtp.clone())
        .await
        .unwrap();
    engine.run(account.id, true).await.unwrap();

    // Dana replies before spark-rs knows where Sent is: she waits for now.
    deliver(&smtp, "Dana <dana@fabrikam.org>", &me, "Re: Hello", false).await;
    deliver(&smtp, "Eve <eve@fabrikam.org>", &me, "Cold pitch", false).await;
    let report = engine.run(account.id, true).await.unwrap();
    assert_eq!(report.new, 2);
    assert!(find(&engine, "Re: Hello").screening);

    // A later round with nothing new still asks Sent about the senders waiting.
    let later = Round {
        fetch: true,
        list_folders: true,
        ..Default::default()
    };
    let report = engine.run_round(account.id, &later).await.unwrap();
    assert_eq!(report.new, 0);
    assert!(!find(&engine, "Re: Hello").screening);
    assert!(find(&engine, "Cold pitch").screening);
}

#[tokio::test]
async fn imap_blocked_folder_renamed_elsewhere_is_made_again() {
    let Ok(host) = std::env::var("SPARK_TEST_GREENMAIL") else {
        eprintln!("skipped: set SPARK_TEST_GREENMAIL to a GreenMail host");
        return;
    };
    let imap = plain(&host, 3143);
    let smtp = plain(&host, 3025);
    let me = format!("renamed{}@example.com", now_ms());
    deliver(&smtp, "Spam <spam@z.net>", &me, "Offer 1", false).await;
    let store = Arc::new(Mutex::new(Store::open_in_memory().unwrap()));
    let engine = Engine::new(store.clone(), Arc::new(MemorySecrets::default()));
    let account = engine
        .add_password_account(AccountKind::Imap, &me, "pw", "", imap.clone(), smtp.clone())
        .await
        .unwrap();
    engine.run(account.id, true).await.unwrap();
    let block = Change {
        screen: Screen::Sender("spam@z.net".into()),
        decision: Some(Decision::Blocked),
    };
    engine.store().decide(&[block], now_ms() - 60_000).unwrap();
    assert_eq!(engine.run(account.id, false).await.unwrap().pushed, 1);

    // Another client renames Blocked, then more spam arrives.
    let mut other = imap::login(&imap, &me, "pw").await.unwrap();
    other.rename("Blocked", "Old spam").await.unwrap();
    deliver(&smtp, "Spam <spam@z.net>", &me, "Offer 2", false).await;
    engine.run(account.id, true).await.unwrap();
    let push = || async {
        let mut state = account_state(&engine, account.id);
        let pending = engine.store().pending(account.id, now_ms() + 1, 0).unwrap();
        let mut session = imap::login(&imap, &me, "pw").await.unwrap();
        let pushed = imap::push(&mut session, &store, &mut state, pending)
            .await
            .unwrap();
        engine
            .store()
            .set_sync_state(account.id, &serde_json::to_string(&state).unwrap())
            .unwrap();
        pushed
    };
    // The move finds the folder gone and waits; the next push makes it again.
    assert_eq!(push().await, 0);
    assert_eq!(
        engine
            .store()
            .pending(account.id, now_ms() + 1, 0)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(push().await, 1);
    assert_eq!(other.select("INBOX").await.unwrap().exists, 0);
    assert_eq!(other.select("Blocked").await.unwrap().exists, 1);
    assert_eq!(other.select("Old spam").await.unwrap().exists, 1);
}

#[tokio::test]
async fn imap_sent_folders_of_every_account_let_senders_through() {
    let Ok(host) = std::env::var("SPARK_TEST_GREENMAIL") else {
        eprintln!("skipped: set SPARK_TEST_GREENMAIL to a GreenMail host");
        return;
    };
    let imap = plain(&host, 3143);
    let smtp = plain(&host, 3025);
    let stamp = now_ms();
    let (home, work) = (
        format!("home{stamp}@example.com"),
        format!("work{stamp}@example.org"),
    );
    // The user wrote to Dana from home, on their phone.
    let mut at_home = imap::login(&imap, &home, "pw").await.unwrap();
    at_home.create("Sent").await.unwrap();
    let to_dana = format!(
        "Message-ID: <to-dana-{stamp}@x.com>\r\nFrom: {home}\r\nTo: dana@fabrikam.org\r\nSubject: Hello\r\n\r\nHi\r\n"
    );
    at_home.append("Sent", None, None, to_dana).await.unwrap();
    let mut at_work = imap::login(&imap, &work, "pw").await.unwrap();
    at_work.create("Sent").await.unwrap();

    let store = Arc::new(Mutex::new(Store::open_in_memory().unwrap()));
    let engine = Engine::new(store, Arc::new(MemorySecrets::default()));
    let add = async |email: &str| {
        engine
            .add_password_account(
                AccountKind::Imap,
                email,
                "pw",
                "",
                imap.clone(),
                smtp.clone(),
            )
            .await
            .unwrap()
            .id
    };
    let (home_id, work_id) = (add(&home).await, add(&work).await);
    let round = Round {
        fetch: true,
        list_folders: true,
        ..Default::default()
    };
    engine.run_round(home_id, &round).await.unwrap();
    engine.run_round(work_id, &round).await.unwrap();

    // Dana writes to work, where the Sent folder has nothing for her.
    deliver(&smtp, "Dana <dana@fabrikam.org>", &work, "Re: Hello", false).await;
    let report = engine.run(work_id, true).await.unwrap();
    assert_eq!((report.new, report.accepted), (1, 0));
    assert!(find(&engine, "Re: Hello").screening);

    // Home's next round finds her in its Sent, and says the list changed.
    let report = engine.run(home_id, true).await.unwrap();
    assert_eq!(report.accepted, 1);
    assert!(report.changed());
    assert!(!find(&engine, "Re: Hello").screening);
}

#[tokio::test]
async fn imap_accepted_mail_comes_back_from_a_renamed_blocked_folder() {
    let Ok(host) = std::env::var("SPARK_TEST_GREENMAIL") else {
        eprintln!("skipped: set SPARK_TEST_GREENMAIL to a GreenMail host");
        return;
    };
    let imap = plain(&host, 3143);
    let smtp = plain(&host, 3025);
    let me = format!("unrenamed{}@example.com", now_ms());
    deliver(&smtp, "Spam <spam@z.net>", &me, "Offer", false).await;
    let store = Arc::new(Mutex::new(Store::open_in_memory().unwrap()));
    let engine = Engine::new(store, Arc::new(MemorySecrets::default()));
    let account = engine
        .add_password_account(AccountKind::Imap, &me, "pw", "", imap.clone(), smtp.clone())
        .await
        .unwrap();
    engine.run(account.id, true).await.unwrap();
    let decide = |decision| {
        let change = Change {
            screen: Screen::Sender("spam@z.net".into()),
            decision: Some(decision),
        };
        engine.store().decide(&[change], now_ms() - 60_000).unwrap();
    };
    decide(Decision::Blocked);
    assert_eq!(engine.run(account.id, false).await.unwrap().pushed, 1);

    // Another client renames Blocked; then the user accepts the sender here.
    let mut other = imap::login(&imap, &me, "pw").await.unwrap();
    other.rename("Blocked", "Old spam").await.unwrap();
    decide(Decision::Accepted);
    assert_eq!(engine.run(account.id, false).await.unwrap().pushed, 1);
    assert_eq!(other.select("INBOX").await.unwrap().exists, 1);
    assert_eq!(other.select("Old spam").await.unwrap().exists, 0);
    assert!(
        engine
            .store()
            .pending(account.id, now_ms() + 1, 0)
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn imap_sent_search_needs_the_exact_address() {
    let Ok(host) = std::env::var("SPARK_TEST_GREENMAIL") else {
        eprintln!("skipped: set SPARK_TEST_GREENMAIL to a GreenMail host");
        return;
    };
    let imap = plain(&host, 3143);
    let me = format!("exact{}@example.com", now_ms());
    let mut session = imap::login(&imap, &me, "pw").await.unwrap();
    session.create("Sent").await.unwrap();
    let to_joann = format!(
        "Message-ID: <to-joann@x.com>\r\nFrom: {me}\r\nTo: Jo Ann <joann@vendor.org>\r\nSubject: Hi\r\n\r\nHi\r\n"
    );
    session.append("Sent", None, None, to_joann).await.unwrap();
    let senders = ["ann@vendor.org".to_owned(), "JoAnn@vendor.org".to_owned()];
    let found = imap::written_to(&mut session, "Sent", &senders)
        .await
        .unwrap();
    assert_eq!(found, ["JoAnn@vendor.org"]);
}

/// Long enough for GreenMail to tell about new mail.
const IDLE_WAIT: Duration = Duration::from_secs(20);

#[tokio::test]
async fn imap_idle_reports_new_mail() {
    let Ok(host) = std::env::var("SPARK_TEST_GREENMAIL") else {
        eprintln!("skipped: set SPARK_TEST_GREENMAIL to a GreenMail host");
        return;
    };
    let imap = plain(&host, 3143);
    let smtp = plain(&host, 3025);
    let me = format!("idle{}@example.com", now_ms());
    deliver(&smtp, "Ana Ruiz <ana@x.com>", &me, "First", false).await;
    let mut session = imap::login(&imap, &me, "pw").await.unwrap();
    assert!(imap::can_idle(&mut session).await.unwrap());
    imap::examine_inbox(&mut session).await.unwrap();

    // Mail that arrives during the IDLE.
    let (idled, ()) = tokio::join!(
        tokio::time::timeout(
            IDLE_WAIT,
            imap::idle(session, std::future::pending(), imap::IDLE_RENEW)
        ),
        async {
            tokio::time::sleep(Duration::from_millis(500)).await;
            deliver(&smtp, "Bob <bob@y.org>", &me, "Second", false).await;
        },
    );
    let Ok(Ok(imap::Idle::Changed(session))) = idled else {
        panic!("IDLE didn't report the new mail");
    };
    // What the server said about it as the IDLE ended.
    while session.unsolicited_responses.try_recv().is_ok() {}

    // Mail that arrives between two IDLEs.
    deliver(&smtp, "Bob <bob@y.org>", &me, "Third", false).await;
    let idled = tokio::time::timeout(
        IDLE_WAIT,
        imap::idle(session, std::future::pending(), imap::IDLE_RENEW),
    )
    .await;
    let Ok(Ok(imap::Idle::Changed(session))) = idled else {
        panic!("IDLE missed mail that came before it began");
    };

    let stopped =
        tokio::time::timeout(IDLE_WAIT, imap::idle(session, async {}, imap::IDLE_RENEW)).await;
    assert!(matches!(stopped, Ok(Ok(imap::Idle::Stopped))));
}

#[tokio::test]
async fn imap_engine_watches_the_inbox() {
    let Ok(host) = std::env::var("SPARK_TEST_GREENMAIL") else {
        eprintln!("skipped: set SPARK_TEST_GREENMAIL to a GreenMail host");
        return;
    };
    let imap = plain(&host, 3143);
    let smtp = plain(&host, 3025);
    let me = format!("watch{}@example.com", now_ms());
    deliver(&smtp, "Ana Ruiz <ana@x.com>", &me, "First", false).await;
    let store = Arc::new(Mutex::new(Store::open_in_memory().unwrap()));
    let engine = Engine::new(store, Arc::new(MemorySecrets::default()));
    let account = engine
        .add_password_account(AccountKind::Imap, &me, "pw", "", imap, smtp.clone())
        .await
        .unwrap();

    let restart = tokio::sync::Notify::new();
    let watching = AtomicBool::new(false);
    let changes = AtomicUsize::new(0);
    let seen = tokio::sync::Notify::new();
    let watch = engine.watch_inbox(account.id, &restart, &watching, || {
        changes.fetch_add(1, Ordering::Relaxed);
        seen.notify_one();
    });
    let (watched, ()) = tokio::join!(tokio::time::timeout(2 * IDLE_WAIT, watch), async {
        while !watching.load(Ordering::Relaxed) {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        // Mail from before the watch began is fetched as it connects.
        assert_eq!(changes.load(Ordering::Relaxed), 1);
        seen.notified().await;
        deliver(&smtp, "Bob <bob@y.org>", &me, "Second", false).await;
        tokio::time::timeout(IDLE_WAIT, seen.notified())
            .await
            .expect("the watch didn't see the new mail");
        assert_eq!(changes.load(Ordering::Relaxed), 2);
        restart.notify_one();
    });
    assert_eq!(
        watched.expect("the watch didn't stop").unwrap(),
        Watch::Restart
    );
}

/// An INTERNALDATE for APPEND, `days` before now.
fn days_ago(days: i64) -> String {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let secs = now_ms() / 1000 - days * 86_400;
    // Howard Hinnant's civil_from_days.
    let z = secs.div_euclid(86_400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "\"{day:02}-{}-{year} 12:00:00 +0000\"",
        MONTHS[month as usize - 1]
    )
}

/// Mail with a file, whose name is written as RFC 2231 has it, and a long
/// text whose last word is past its preview.
fn with_file(message_id: &str, subject: &str) -> String {
    let filler = "and so on ".repeat(40);
    format!(
        "Message-ID: <{message_id}>\r\nFrom: Ana Ruiz <ana@x.com>\r\nTo: me@example.com\r\n\
         Subject: {subject}\r\nMIME-Version: 1.0\r\n\
         Content-Type: multipart/mixed; boundary=m\r\n\r\n\
         --m\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n\
         Here are the figures, {filler}zanzibar.\r\n\
         --m\r\nContent-Type: application/pdf\r\n\
         Content-Disposition: attachment; filename*=utf-8''R%C3%A9sum%C3%A9.pdf\r\n\
         Content-Transfer-Encoding: base64\r\n\r\nJVBERi0xLjQK\r\n--m--\r\n"
    )
}

/// A meeting request.
fn invitation(message_id: &str, subject: &str) -> String {
    format!(
        "Message-ID: <{message_id}>\r\nFrom: Ana Ruiz <ana@x.com>\r\nTo: me@example.com\r\n\
         Subject: {subject}\r\nMIME-Version: 1.0\r\n\
         Content-Type: multipart/alternative; boundary=a\r\n\r\n\
         --a\r\nContent-Type: text/plain\r\n\r\nJoin us.\r\n\
         --a\r\nContent-Type: text/calendar; charset=utf-8; method=REQUEST\r\n\r\n\
         BEGIN:VCALENDAR\r\nMETHOD:REQUEST\r\nBEGIN:VEVENT\r\nUID:review@x.com\r\n\
         SUMMARY:Review\r\nDTSTART:20261103T120000Z\r\nDTEND:20261103T130000Z\r\n\
         ORGANIZER;CN=Ana Ruiz:mailto:ana@x.com\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n\
         --a--\r\n"
    )
}

#[tokio::test]
async fn imap_inbox_downloads_a_year_in_the_background() {
    let Ok(host) = std::env::var("SPARK_TEST_GREENMAIL") else {
        eprintln!("skipped: set SPARK_TEST_GREENMAIL to a GreenMail host");
        return;
    };
    let imap = plain(&host, 3143);
    let smtp = plain(&host, 3025);
    let me = format!("year{}@example.com", now_ms());
    let today = imap::FIRST_SYNC_LIMIT;
    // Oldest first, so UIDs go up with the dates: 120 from 700 to 581 days
    // ago, 150 within the year (330 to 32 days ago), and 300 from today.
    const ANCIENT: usize = 120;
    const YEAR: usize = 150;
    let mut other = imap::login(&imap, &me, "pw").await.unwrap();
    for n in 0..ANCIENT {
        let content = raw(&format!("ancient{n}@x.com"), &format!("Ancient {n}"));
        let date = days_ago(700 - n as i64);
        other
            .append("INBOX", None, Some(&date), content)
            .await
            .unwrap();
    }
    for n in 0..YEAR {
        let (id, subject) = (format!("year{n}@x.com"), format!("Year {n}"));
        let content = match n {
            120 => with_file(&id, &subject),
            121 => invitation(&id, &subject),
            // Found by a word only its body has.
            122 => raw(&id, &subject).replace(" body", " quetzal"),
            _ => raw(&id, &subject),
        };
        let date = days_ago(330 - 2 * n as i64);
        other
            .append("INBOX", None, Some(&date), content)
            .await
            .unwrap();
    }
    for n in 0..today {
        let content = raw(&format!("today{n}@x.com"), &format!("Today {n}"));
        other.append("INBOX", None, None, content).await.unwrap();
    }
    let _ = other.logout().await;

    let dir = std::env::temp_dir().join(format!("spark-greenmail-year-{}", now_ms()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("mail.sqlite");
    let secrets = Arc::new(MemorySecrets::default());
    let open = || {
        let store = Store::open(&path).unwrap();
        Engine::new(Arc::new(Mutex::new(store)), secrets.clone())
    };
    let engine = open();
    let account = engine
        .add_password_account(AccountKind::Imap, &me, "pw", "", imap.clone(), smtp.clone())
        .await
        .unwrap();
    assert!(!engine.backfill_due(account.id), "before the first sync");
    let report = engine.run(account.id, true).await.unwrap();
    assert_eq!(report.new, today);
    assert!(engine.backfill_due(account.id));

    // The first step finds where the year starts on the server. GreenMail
    // can't send the start of a text as IMAP says to: the step fails, asks
    // for less from then on, and waits before the next.
    assert!(engine.backfill(account.id).await.is_err());
    let state = account_state(&engine, account.id);
    assert!(
        state.window.is_some() && state.floor > state.window,
        "{state:?}"
    );
    assert_eq!(state.heads, imap::Heads::Structure);
    assert!(!engine.backfill_due(account.id), "it waits after failing");

    // A restart goes on from where it got to: the 100 before today's mail,
    // quietly, without their bodies.
    drop(engine);
    let engine = open();
    assert!(engine.backfill_due(account.id));
    let report = engine.backfill(account.id).await.unwrap();
    assert_eq!(report.older, OLDER_STEP, "{report:?}");
    assert!(report.arrived.is_empty() && report.new == 0);
    assert!(!engine.backfill_due(account.id), "it pauses between steps");
    let year = find(&engine, "Year 140");
    assert!(year.state == MessageState::Inbox && !year.screening);
    assert_eq!(year.snippet, "", "no preview from GreenMail");
    let content = engine.store().content(year.id).unwrap().unwrap();
    assert!(content.body_missing && content.html.is_none());
    // As a whole email lists: its file, and as an invitation.
    let file = find(&engine, "Year 120");
    assert_eq!(file.attachment.as_deref(), Some("Résumé.pdf"));
    assert_eq!(
        find(&engine, "Year 121").invite_kind.as_deref(),
        Some("invitation")
    );
    // Search has its sender and subject, not its text yet.
    let found = |engine: &Engine, text: &str| -> Vec<String> {
        let found = engine.store().search(text).unwrap();
        found.into_iter().map(|m| m.subject).collect()
    };
    assert_eq!(found(&engine, "\"Year 120\""), ["Year 120"]);
    assert!(found(&engine, "zanzibar").is_empty());

    // After another restart: the rest of the year, and the 50 beyond it the
    // step reaches.
    drop(engine);
    let engine = open();
    assert!(engine.backfill_due(account.id));
    let report = engine.backfill(account.id).await.unwrap();
    assert_eq!(report.older, OLDER_STEP);
    let stored = engine.store().messages().unwrap();
    assert_eq!(stored.len(), today + YEAR + 50);
    for n in 0..YEAR {
        let subject = format!("Year {n}");
        assert!(stored.iter().any(|m| m.subject == subject), "{subject}");
    }
    assert!(stored.iter().all(|m| m.subject != "Ancient 69"));

    // That's the year: it stops, after a restart too.
    drop(engine);
    let engine = open();
    assert!(!engine.backfill_due(account.id));
    assert_eq!(engine.backfill(account.id).await.unwrap().older, 0);
    let has_older = || inbox_has_older(&engine.store().account(account.id).unwrap().unwrap());
    assert!(has_older());

    // Scrolling still reaches back past it, the same way.
    let older = Round {
        older_inbox: true,
        ..Default::default()
    };
    let report = engine.run_round(account.id, &older).await.unwrap();
    assert_eq!(report.older, ANCIENT - 50);
    assert!(!has_older());
    let ancient = find(&engine, "Ancient 0");
    assert!(
        engine
            .store()
            .content(ancient.id)
            .unwrap()
            .unwrap()
            .body_missing
    );

    // A copy in a folder (as in a Gmail label) joins it as it is, without
    // its body, which opening it still downloads.
    let mut other = imap::login(&imap, &me, "pw").await.unwrap();
    other.create("Projects").await.unwrap();
    other
        .append("Projects", None, None, raw("year140@x.com", "Year 140"))
        .await
        .unwrap();
    let _ = other.logout().await;
    let list = Round {
        list_folders: true,
        ..Default::default()
    };
    engine.run_round(account.id, &list).await.unwrap();
    let projects = Round {
        folders: vec![folder_id(&engine, "Projects")],
        ..Default::default()
    };
    engine.run_round(account.id, &projects).await.unwrap();
    let copied = find(&engine, "Year 140");
    assert_eq!(copied.folders, [folder_id(&engine, "Projects")]);
    let content = engine.store().content(copied.id).unwrap().unwrap();
    assert!(
        content.body_missing && content.text.is_empty(),
        "{content:?}"
    );

    // A server search finds one by its body: it gets it, for the store's
    // search to find too.
    assert!(found(&engine, "quetzal").is_empty());
    assert_eq!(
        engine.search_server(account.id, "quetzal").await.unwrap(),
        1
    );
    assert_eq!(found(&engine, "quetzal"), ["Year 122"]);
    assert_eq!(
        engine.search_server(account.id, "quetzal").await.unwrap(),
        0
    );

    // Opened, an email gets its body, files and invite; what the user did
    // with it stays.
    let read = Patch {
        unread: Some(false),
        pinned: Some(true),
        ..Default::default()
    };
    engine.store().update(file.id, &read, now_ms()).unwrap();
    assert!(engine.fetch_body(file.id).await.unwrap());
    let content = engine.store().content(file.id).unwrap().unwrap();
    assert!(!content.body_missing);
    assert!(
        content.text.trim_end().ends_with("zanzibar."),
        "{:?}",
        content.text
    );
    let names: Vec<&str> = content
        .attachments
        .iter()
        .map(|a| a.name.as_str())
        .collect();
    assert_eq!(names, ["Résumé.pdf"]);
    let opened = find(&engine, "Year 120");
    assert!(!opened.unread && opened.pinned);
    assert_eq!(opened.attachment, file.attachment, "it looked as it does");
    assert!(
        opened
            .snippet
            .starts_with("Here are the figures, and so on")
    );
    assert_eq!(found(&engine, "zanzibar"), ["Year 120"]);
    assert!(!engine.fetch_body(file.id).await.unwrap(), "once");

    let meeting = find(&engine, "Year 121");
    assert!(engine.fetch_body(meeting.id).await.unwrap());
    let content = engine.store().content(meeting.id).unwrap().unwrap();
    let invite = spark_mail::invite::Invite::from_json(content.invite.as_deref().unwrap()).unwrap();
    assert_eq!(invite.summary, "Review");
    assert!(!invite.ics.is_empty());

    // Gone from the server: it says so, and still waits for its body.
    let mut other = imap::login(&imap, &me, "pw").await.unwrap();
    other.select("INBOX").await.unwrap();
    let uids = other
        .uid_search("HEADER Message-ID \"year3@x.com\"")
        .await
        .unwrap();
    let uid = uids.into_iter().next().unwrap();
    let _: Vec<_> = other
        .uid_store(uid.to_string(), "+FLAGS.SILENT (\\Deleted)")
        .await
        .unwrap()
        .try_collect()
        .await
        .unwrap();
    let _: Vec<_> = other.expunge().await.unwrap().try_collect().await.unwrap();
    let _ = other.logout().await;
    let gone = find(&engine, "Year 3");
    let fetched = engine.fetch_body(gone.id).await;
    assert!(
        matches!(fetched, Err(spark_mail::Error::Gone)),
        "{fetched:?}"
    );
    assert!(
        engine
            .store()
            .content(gone.id)
            .unwrap()
            .unwrap()
            .body_missing
    );

    drop(engine);
    let _ = std::fs::remove_dir_all(&dir);
}
