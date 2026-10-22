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

use futures_util::TryStreamExt;
use lettre::message::header::{ContentType, HeaderName, HeaderValue};
use lettre::message::{Attachment, MultiPart, SinglePart};
use spark_mail::draft::{Draft, OutgoingAttachment};
use spark_mail::imap::{self, Security, Server};
use spark_mail::{Engine, Secrets, smtp};
use spark_store::{AccountKind, Category, MessageState, OutboxFile, Patch, Store, now_ms};

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

    // First sync: everything in, nobody screened.
    let report = engine.run(account.id, true).await.unwrap();
    assert_eq!(report.new, 3, "{report:?}");
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
    assert!(find(&engine, "Hi there").screening);

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
    assert!(flags[0].flags().any(|f| f == async_imap::types::Flag::Seen));
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

    // Replying to Ana: sent over SMTP, threaded, and Ana is accepted.
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
    let listed = engine.store().content(quote.id).unwrap().unwrap().attachments;
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
