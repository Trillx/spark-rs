use spark_mail::parse::parse_message;

#[test]
fn incoming_reply_to_preserves_multiple_destinations_without_changing_sender_or_threading() {
    let raw = b"From: No Reply <no-reply@example.test>\r\nReply-To: Help Desk <help@example.test>, alternate@example.test\r\nTo: Me <me@example.test>\r\nMessage-ID: <message@example.test>\r\nIn-Reply-To: <previous@example.test>\r\nReferences: <previous@example.test>\r\nSubject: Ticket\r\n\r\nReply to the help desk.\r\n";
    let incoming = parse_message(raw).unwrap();
    assert_eq!(
        incoming.reply_to.unwrap(),
        ["Help Desk <help@example.test>", "alternate@example.test"]
    );
    assert_eq!(incoming.from_name, "No Reply");
    assert_eq!(incoming.from_address, "no-reply@example.test");
    assert_eq!(
        incoming.in_reply_to.as_deref(),
        Some("previous@example.test")
    );
    assert_eq!(incoming.message_id.as_deref(), Some("message@example.test"));
}

#[test]
fn reply_to_survives_incoming_message_storage_separately_from_from_and_threading() {
    let mut store = spark_store::Store::open_in_memory().unwrap();
    let account = store
        .add_account(&spark_store::NewAccount {
            kind: spark_store::AccountKind::Imap,
            email: "me@example.test",
            name: "Me",
            label: "Test",
            config: "{}",
        })
        .unwrap();
    let mut incoming = parse_message(b"From: No Reply <no-reply@example.test>\r\nReply-To: Help Desk <help@example.test>\r\nMessage-ID: <message@example.test>\r\nIn-Reply-To: <previous@example.test>\r\n\r\nMessage text").unwrap();
    incoming.account_id = account.id;
    store.add_incoming(&incoming, false).unwrap();
    let message = store.messages().unwrap().remove(0);
    assert_eq!(message.reply_to, ["Help Desk <help@example.test>"]);
    assert_eq!(
        store.content(message.id).unwrap().unwrap().reply_to,
        message.reply_to
    );
    assert_eq!(message.from_address, "no-reply@example.test");
    assert_eq!(
        store.in_reply_to(message.id).unwrap().as_deref(),
        Some("previous@example.test")
    );
    assert_eq!(
        store
            .headers(message.id)
            .unwrap()
            .unwrap()
            .message_id
            .as_deref(),
        Some("message@example.test")
    );
}

async fn graph_sync_with_reply_to(legacy: bool) {
    use spark_mail::graph::{GRAPH, Graph, SyncState, first_delta_url};
    use std::sync::Mutex;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let peer = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
            let n = stream.read(&mut chunk).await.unwrap();
            assert!(n > 0);
            buf.extend_from_slice(&chunk[..n]);
        }
        let request = String::from_utf8(buf).unwrap();
        let mut message = serde_json::json!({
            "id": "graph-message", "isRead": true,
            "from": { "emailAddress": { "name": "No Reply", "address": "no-reply@example.test" } },
            "internetMessageId": "<graph-message@example.test>",
        });
        // Graph omits properties not named in $select.
        if request.contains("replyTo") {
            message["replyTo"] = serde_json::json!([
                { "emailAddress": { "name": "Help Desk", "address": "help@example.test" } },
                { "emailAddress": { "address": "alternate@example.test" } }
            ]);
        }
        let body =
            serde_json::json!({ "value": [message], "@odata.deltaLink": "http://localhost/next" })
                .to_string();
        stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
        request
    });
    let store = spark_store::Store::open_in_memory().unwrap();
    let account = store
        .add_account(&spark_store::NewAccount {
            kind: spark_store::AccountKind::Microsoft,
            email: "me@example.test",
            name: "Me",
            label: "Test",
            config: "{}",
        })
        .unwrap();
    let mut store = store;
    if legacy {
        store
            .add_incoming(
                &spark_store::Incoming {
                    account_id: account.id,
                    remote_id: Some("graph-message".into()),
                    from_name: "No Reply".into(),
                    from_address: "no-reply@example.test".into(),
                    date: 2_000,
                    ..Default::default()
                },
                false,
            )
            .unwrap();
        for (remote, date) in [("removed-before-refresh", 3_000), ("older-cache", 500)] {
            store
                .add_incoming(
                    &spark_store::Incoming {
                        account_id: account.id,
                        remote_id: Some(remote.into()),
                        date,
                        subject: remote.into(),
                        ..Default::default()
                    },
                    false,
                )
                .unwrap();
        }
    }
    let store = Mutex::new(store);
    let old_link = if legacy {
        format!("{base}/legacy-delta?$deltatoken=old")
    } else {
        first_delta_url(1_000).replace(GRAPH, &base)
    };
    let mut state: SyncState = serde_json::from_value(serde_json::json!({
        "deltaLink": old_link, "since": 1_000, "olderThan": 500, "complete": true,
        "replyToSelected": !legacy,
    }))
    .unwrap();
    let report = Graph::with_base(&base, "synthetic-token")
        .sync_inbox(&store, account.id, &mut state)
        .await
        .unwrap();
    let request = peer.await.unwrap();
    let messages = store.lock().unwrap().messages().unwrap();
    let message = messages
        .iter()
        .find(|m| m.from_address == "no-reply@example.test")
        .unwrap();
    assert_eq!(
        message.reply_to,
        ["Help Desk <help@example.test>", "alternate@example.test"],
        "request: {request}"
    );
    assert_eq!(message.from_address, "no-reply@example.test");
    assert_eq!(state.since, Some(1_000));
    assert_eq!(state.older_than, Some(500));
    assert!(state.complete);
    if legacy {
        assert!(report.new.is_empty());
        assert_eq!(report.headers, 1, "backfilled headers must refresh the UI");
        assert_eq!(
            report.gone, 1,
            "the replacement snapshot must reconcile removed messages"
        );
        assert_eq!(
            messages
                .iter()
                .find(|m| m.subject == "removed-before-refresh")
                .unwrap()
                .state,
            spark_store::MessageState::Done
        );
        assert_eq!(
            messages
                .iter()
                .find(|m| m.subject == "older-cache")
                .unwrap()
                .state,
            spark_store::MessageState::Inbox,
            "older mail stays outside the delta window"
        );
    }
}

#[tokio::test]
async fn graph_fetch_requests_and_stores_reply_to_as_display_recipients() {
    graph_sync_with_reply_to(false).await;
}

#[tokio::test]
async fn legacy_graph_delta_restarts_once_without_losing_fetch_bounds() {
    graph_sync_with_reply_to(true).await;
}

#[tokio::test]
async fn cached_imap_mail_gains_reply_to_without_downloading_the_body() {
    use spark_mail::imap::{self, Security, Server, SyncState};
    use std::sync::Mutex;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let server = Server {
        host: "127.0.0.1".into(),
        port: listener.local_addr().unwrap().port(),
        security: Security::Plain,
        trusted_cert: None,
    };
    let peer = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let (read, mut write) = stream.into_split();
        write.write_all(b"* OK synthetic IMAP\r\n").await.unwrap();
        let mut lines = BufReader::new(read).lines();
        let mut commands = Vec::new();
        while let Some(line) = lines.next_line().await.unwrap() {
            let (tag, command) = line.split_once(' ').unwrap();
            commands.push(command.to_owned());
            let data = if command.starts_with("LOGIN ") {
                String::new()
            } else if command.starts_with("SELECT ") {
                "* 1 EXISTS\r\n* OK [UIDVALIDITY 1] valid\r\n* OK [UIDNEXT 2] next\r\n".into()
            } else if command == "FETCH 1 (UID)" {
                "* 1 FETCH (UID 1)\r\n".into()
            } else if command == "UID SEARCH UID 1:*" {
                "* SEARCH 1\r\n".into()
            } else if command.starts_with("UID FETCH ") {
                if command.contains("REPLY-TO") {
                    let header = "Message-ID: <cached@example.test>\r\nReply-To: Help <help@example.test>\r\n\r\n";
                    format!(
                        "* 1 FETCH (UID 1 FLAGS () BODY[HEADER.FIELDS (MESSAGE-ID REPLY-TO)] {{{}}}\r\n{header})\r\n",
                        header.len()
                    )
                } else {
                    "* 1 FETCH (UID 1 FLAGS ())\r\n".into()
                }
            } else {
                panic!("unexpected command {command}");
            };
            write
                .write_all(format!("{data}{tag} OK done\r\n").as_bytes())
                .await
                .unwrap();
            // The flags come last, after the headers.
            if command.starts_with("UID FETCH ") && !command.contains("REPLY-TO") {
                return commands;
            }
        }
        commands
    });
    let mut store = spark_store::Store::open_in_memory().unwrap();
    let account = store
        .add_account(&spark_store::NewAccount {
            kind: spark_store::AccountKind::Imap,
            email: "me@example.test",
            name: "Me",
            label: "Test",
            config: "{}",
        })
        .unwrap();
    store
        .add_incoming(
            &spark_store::Incoming {
                account_id: account.id,
                mailbox: Some("INBOX".into()),
                uid: Some(1),
                message_id: Some("cached@example.test".into()),
                from_address: "no-reply@example.test".into(),
                ..Default::default()
            },
            false,
        )
        .unwrap();
    let store = Mutex::new(store);
    let mut session = imap::login(&server, "me", "pw").await.unwrap();
    let mut state = SyncState {
        uid_validity: Some(1),
        floor: Some(1),
        ..Default::default()
    };
    let report = imap::sync_inbox(&mut session, &store, account.id, &mut state)
        .await
        .unwrap();
    assert!(report.new.is_empty());
    let commands = peer.await.unwrap();
    let message = store.lock().unwrap().messages().unwrap().remove(0);
    assert_eq!(
        message.reply_to,
        ["Help <help@example.test>"],
        "commands: {commands:?}"
    );
    assert!(!commands.iter().any(|c| c.contains("BODY.PEEK[]")));
}

#[test]
fn missing_or_empty_reply_to_keeps_an_empty_list_for_sender_fallback() {
    for raw in [
        b"From: Sender <sender@example.test>\r\n\r\nText".as_slice(),
        b"From: Sender <sender@example.test>\r\nReply-To:\r\n\r\nText".as_slice(),
    ] {
        let parsed = parse_message(raw).unwrap();
        assert_eq!(parsed.reply_to, Some(Vec::new()));
        assert_eq!(parsed.from_address, "sender@example.test");
    }
}
