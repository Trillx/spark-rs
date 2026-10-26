//! Synthetic IMAP peers exercise destructive commands without a real mailbox.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use spark_mail::imap::{self, Security, Server, SyncState};
use spark_mail::{Error, drafts};
use spark_store::{
    Account, AccountKind, FolderRole, Incoming, MessageState, NewAccount, Patch, RemoteFolder,
    Store,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

struct Peer {
    server: Server,
    commands: Arc<Mutex<Vec<String>>>,
    messages: Arc<Mutex<BTreeMap<u32, bool>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Peer {
    async fn start(capabilities: &'static str) -> Self {
        Self::with_generation(
            capabilities,
            1,
            BTreeMap::from([(1, "target@example.test"), (2, "other@example.test")]),
        )
        .await
    }

    async fn with_generation(
        capabilities: &'static str,
        validity: u32,
        identities: BTreeMap<u32, &'static str>,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let server = Server {
            host: "127.0.0.1".into(),
            port: listener.local_addr().unwrap().port(),
            security: Security::Plain,
            trusted_cert: None,
        };
        let commands = Arc::new(Mutex::new(Vec::new()));
        // UID 2 was marked deleted by another mail client and must survive.
        let messages = Arc::new(Mutex::new(
            identities
                .keys()
                .map(|uid| (*uid, *uid == 2))
                .collect::<BTreeMap<_, _>>(),
        ));
        let recorded = commands.clone();
        let mailbox = messages.clone();
        let task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut write) = stream.into_split();
            write.write_all(b"* OK synthetic server\r\n").await.unwrap();
            let mut lines = BufReader::new(read).lines();
            while let Some(line) = lines.next_line().await.unwrap() {
                let (tag, command) = line.split_once(' ').unwrap();
                recorded.lock().unwrap().push(command.to_owned());
                let upper = command.to_ascii_uppercase();
                let data = if upper.starts_with("LOGIN ") {
                    String::new()
                } else if upper == "CAPABILITY" {
                    format!("* CAPABILITY IMAP4rev1 {capabilities}\r\n")
                } else if upper.starts_with("SELECT ") {
                    format!(
                        "* {} EXISTS\r\n* OK [UIDVALIDITY {validity}] valid\r\n* FLAGS (\\Seen \\Deleted)\r\n",
                        identities.len()
                    )
                } else if upper.starts_with("UID SEARCH ") {
                    let hits = identities
                        .iter()
                        .filter(|(_, id)| id.contains("target@example.test"))
                        .map(|(uid, _)| uid.to_string())
                        .collect::<Vec<_>>()
                        .join(" ");
                    format!("* SEARCH {hits}\r\n")
                } else if upper.starts_with("UID FETCH ") {
                    let mut data = String::new();
                    for (position, (uid, id)) in identities.iter().enumerate() {
                        let header = format!("Message-ID: <{id}>\r\n\r\n");
                        data.push_str(&format!("* {} FETCH (UID {uid} BODY[HEADER.FIELDS (MESSAGE-ID)] {{{}}}\r\n{header})\r\n", position + 1, header.len()));
                    }
                    data
                } else if upper.starts_with("UID STORE ") {
                    let uid = command.split_whitespace().nth(2).unwrap().parse().unwrap();
                    if upper.contains("\\DELETED") {
                        *mailbox.lock().unwrap().get_mut(&uid).unwrap() = true;
                    }
                    String::new()
                } else if upper.starts_with("UID COPY ") || upper.starts_with("UID MOVE ") {
                    String::new()
                } else if upper.starts_with("UID EXPUNGE ") {
                    let uid: u32 = command.split_whitespace().nth(2).unwrap().parse().unwrap();
                    mailbox.lock().unwrap().remove(&uid);
                    "* 1 EXPUNGE\r\n".into()
                } else if upper == "EXPUNGE" {
                    mailbox.lock().unwrap().retain(|_, deleted| !*deleted);
                    "* 1 EXPUNGE\r\n* 1 EXPUNGE\r\n".into()
                } else {
                    panic!("unexpected command: {command}");
                };
                write
                    .write_all(format!("{data}{tag} OK complete\r\n").as_bytes())
                    .await
                    .unwrap();
            }
        });
        Self {
            server,
            commands,
            messages,
            task,
        }
    }

    fn commands(&self) -> Vec<String> {
        self.commands.lock().unwrap().clone()
    }
}

impl Drop for Peer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn account(store: &Store) -> Account {
    store
        .add_account(&NewAccount {
            kind: AccountKind::Imap,
            email: "me@example.test",
            name: "",
            label: "Test",
            config: "{}",
        })
        .unwrap()
}

fn state() -> SyncState {
    SyncState {
        uid_validity: Some(1),
        archive: Some("Archive".into()),
        trash: Some("Trash".into()),
        ..Default::default()
    }
}

fn pending_move() -> (Mutex<Store>, Vec<spark_store::Pending>) {
    let mut store = Store::open_in_memory().unwrap();
    let account = account(&store);
    store
        .add_incoming(
            &Incoming {
                account_id: account.id,
                mailbox: Some("INBOX".into()),
                uid: Some(1),
                message_id: Some("target@example.test".into()),
                ..Default::default()
            },
            false,
        )
        .unwrap();
    let id = store.messages().unwrap()[0].id;
    store
        .update(
            id,
            &Patch {
                state: Some(MessageState::Done),
                unread: Some(true),
                ..Default::default()
            },
            1,
        )
        .unwrap();
    let pending = store.pending(account.id, 100_000, 0).unwrap();
    (Mutex::new(store), pending)
}

fn discarded_draft() -> (Mutex<Store>, Account) {
    discarded_draft_with_id(Some("target@example.test"))
}

fn discarded_draft_with_id(message_id: Option<&str>) -> (Mutex<Store>, Account) {
    let mut store = Store::open_in_memory().unwrap();
    let account = account(&store);
    store
        .replace_folders(
            account.id,
            &[RemoteFolder {
                remote_id: "Drafts".into(),
                parent: None,
                name: "Drafts".into(),
                role: Some(FolderRole::Drafts),
                selectable: true,
            }],
        )
        .unwrap();
    let folder = store.folders().unwrap().remove(0);
    store
        .add_folder_message(
            folder.id,
            &Incoming {
                account_id: account.id,
                mailbox: Some("Drafts".into()),
                uid: Some(1),
                message_id: message_id.map(str::to_owned),
                ..Default::default()
            },
        )
        .unwrap();
    let id = store.messages().unwrap()[0].id;
    assert!(store.discard_draft(id).unwrap());
    (Mutex::new(store), account)
}

#[tokio::test]
async fn fallback_move_only_expunges_its_target() {
    let peer = Peer::start("UIDPLUS").await;
    let (store, pending) = pending_move();
    let mut session = imap::login(&peer.server, "me", "pw").await.unwrap();
    assert_eq!(
        imap::push(&mut session, &store, &mut state(), pending)
            .await
            .unwrap(),
        1
    );
    assert_eq!(*peer.messages.lock().unwrap(), BTreeMap::from([(2, true)]));
    assert!(peer.commands().iter().any(|c| c == "UID EXPUNGE 1"));
    assert!(
        !peer
            .commands()
            .iter()
            .any(|c| c == "EXPUNGE" || c.starts_with("UID SEARCH "))
    );
}

#[tokio::test]
async fn unsupported_move_remains_pending_without_copying_or_flagging() {
    let peer = Peer::start("").await;
    let (store, pending) = pending_move();
    let account_id = store.lock().unwrap().accounts().unwrap()[0].id;
    let mut session = imap::login(&peer.server, "me", "pw").await.unwrap();
    for _ in 0..2 {
        let pending = store
            .lock()
            .unwrap()
            .pending(account_id, 100_000, 0)
            .unwrap();
        assert!(matches!(
            imap::push(&mut session, &store, &mut state(), pending).await,
            Err(Error::Server(_))
        ));
    }
    assert_eq!(pending.len(), 1);
    assert_eq!(
        store
            .lock()
            .unwrap()
            .pending(account_id, 100_000, 0)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        *peer.messages.lock().unwrap(),
        BTreeMap::from([(1, false), (2, true)])
    );
    assert!(
        !peer.commands().iter().any(|c| c.starts_with("UID STORE")
            || c.starts_with("UID COPY")
            || c.contains("EXPUNGE"))
    );
}

#[tokio::test]
async fn draft_cleanup_only_expunges_its_target() {
    let peer = Peer::start("UIDPLUS").await;
    let (store, account) = discarded_draft();
    let mut session = imap::login(&peer.server, "me", "pw").await.unwrap();
    let report = drafts::push_imap(&mut session, &store, &account, &state(), 100_000)
        .await
        .unwrap();
    assert_eq!(report.removed, 1);
    assert!(
        store
            .lock()
            .unwrap()
            .draft_removals(account.id)
            .unwrap()
            .is_empty()
    );
    assert_eq!(*peer.messages.lock().unwrap(), BTreeMap::from([(2, true)]));
    assert!(!peer.commands().iter().any(|c| c == "EXPUNGE"));
}

#[tokio::test]
async fn unsupported_draft_cleanup_keeps_queue_without_flagging() {
    let peer = Peer::start("").await;
    let (store, account) = discarded_draft();
    let mut session = imap::login(&peer.server, "me", "pw").await.unwrap();
    for _ in 0..2 {
        assert!(matches!(
            drafts::push_imap(&mut session, &store, &account, &state(), 100_000).await,
            Err(Error::Server(_))
        ));
    }
    assert_eq!(
        store
            .lock()
            .unwrap()
            .draft_removals(account.id)
            .unwrap()
            .len(),
        1
    );
    assert!(
        store
            .lock()
            .unwrap()
            .draft_uploads(account.id, 100_000)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        *peer.messages.lock().unwrap(),
        BTreeMap::from([(1, false), (2, true)])
    );
    assert!(
        !peer.commands().iter().any(|c| c.starts_with("UID STORE")
            || c.contains("EXPUNGE")
            || c.starts_with("APPEND"))
    );
}

fn renumbered_identities() -> BTreeMap<u32, &'static str> {
    BTreeMap::from([
        (1, "unrelated@example.test"),
        (2, "other@example.test"),
        (3, "target@example.test"),
        // HEADER SEARCH also returns this newer substring match.
        (4, "target@example.test.longer"),
    ])
}

#[tokio::test]
async fn inbox_uid_reset_resolves_exact_message_before_mutation() {
    let peer = Peer::with_generation("UIDPLUS", 2, renumbered_identities()).await;
    let (store, pending) = pending_move();
    let mut session = imap::login(&peer.server, "me", "pw").await.unwrap();
    assert_eq!(
        imap::push(&mut session, &store, &mut state(), pending)
            .await
            .unwrap(),
        1
    );
    let commands = peer.commands();
    assert!(commands.iter().any(|c| c == "UID COPY 3 \"Archive\""));
    assert!(!commands.iter().any(|c| c.starts_with("UID COPY 1 ")
        || c.starts_with("UID COPY 4 ")
        || c.starts_with("UID STORE 1 ")));
    assert_eq!(
        *peer.messages.lock().unwrap(),
        BTreeMap::from([(1, false), (2, true), (4, false)])
    );
}

#[tokio::test]
async fn folder_uid_reset_resolves_exact_message_before_mutation() {
    let peer = Peer::with_generation("UIDPLUS", 2, renumbered_identities()).await;
    let mut store = Store::open_in_memory().unwrap();
    let account = account(&store);
    store
        .replace_folders(
            account.id,
            &[RemoteFolder {
                remote_id: "Travel".into(),
                parent: None,
                name: "Travel".into(),
                role: None,
                selectable: true,
            }],
        )
        .unwrap();
    let folder = store.folders().unwrap().remove(0);
    store
        .set_folder_synced(folder.id, r#"{"uidValidity":1}"#, 1)
        .unwrap();
    store
        .add_folder_message(
            folder.id,
            &Incoming {
                account_id: account.id,
                mailbox: Some("Travel".into()),
                uid: Some(1),
                message_id: Some("target@example.test".into()),
                ..Default::default()
            },
        )
        .unwrap();
    let id = store.messages().unwrap()[0].id;
    store
        .update(
            id,
            &Patch {
                state: Some(MessageState::Done),
                ..Default::default()
            },
            1,
        )
        .unwrap();
    let pending = store.pending(account.id, 100_000, 0).unwrap();
    let store = Mutex::new(store);
    let mut session = imap::login(&peer.server, "me", "pw").await.unwrap();
    assert_eq!(
        imap::push(&mut session, &store, &mut state(), pending)
            .await
            .unwrap(),
        1
    );
    assert!(
        peer.commands()
            .iter()
            .any(|c| c == "UID COPY 3 \"Archive\"")
    );
    assert!(peer.messages.lock().unwrap().contains_key(&1));
}

#[tokio::test]
async fn unknown_uid_generation_requires_message_identity() {
    let peer = Peer::with_generation("UIDPLUS", 2, renumbered_identities()).await;
    let (store, pending) = pending_move();
    let mut state = state();
    state.uid_validity = None;
    let mut session = imap::login(&peer.server, "me", "pw").await.unwrap();
    assert_eq!(
        imap::push(&mut session, &store, &mut state, pending)
            .await
            .unwrap(),
        1
    );
    assert!(
        peer.commands()
            .iter()
            .any(|c| c == "UID COPY 3 \"Archive\"")
    );
    assert!(peer.messages.lock().unwrap().contains_key(&1));
}

#[tokio::test]
async fn reset_uid_without_message_id_never_mutates_reused_uid() {
    let peer = Peer::with_generation("UIDPLUS", 2, renumbered_identities()).await;
    let (store, mut pending) = pending_move();
    let account = store.lock().unwrap().accounts().unwrap()[0].id;
    pending[0].message_id = None;
    let mut session = imap::login(&peer.server, "me", "pw").await.unwrap();
    assert_eq!(
        imap::push(&mut session, &store, &mut state(), pending)
            .await
            .unwrap(),
        0
    );
    assert!(
        store
            .lock()
            .unwrap()
            .pending(account, 100_000, 0)
            .unwrap()
            .is_empty()
    );
    assert!(!peer.commands().iter().any(|c| c.starts_with("UID STORE ")
        || c.starts_with("UID COPY ")
        || c.contains("EXPUNGE")));
}

#[tokio::test]
async fn missing_exact_message_never_moves_a_substring_match() {
    let peer = Peer::with_generation(
        "UIDPLUS",
        2,
        BTreeMap::from([
            (1, "unrelated@example.test"),
            (2, "other@example.test"),
            (4, "target@example.test.longer"),
        ]),
    )
    .await;
    let (store, pending) = pending_move();
    let mut session = imap::login(&peer.server, "me", "pw").await.unwrap();
    assert_eq!(
        imap::push(&mut session, &store, &mut state(), pending)
            .await
            .unwrap(),
        0
    );
    assert!(!peer.commands().iter().any(|c| c.starts_with("UID STORE ")
        || c.starts_with("UID COPY ")
        || c.contains("EXPUNGE")));
}

#[tokio::test]
async fn draft_cleanup_resolves_identity_after_uid_reset() {
    let peer = Peer::with_generation("UIDPLUS", 2, renumbered_identities()).await;
    let (store, account) = discarded_draft();
    let mut session = imap::login(&peer.server, "me", "pw").await.unwrap();
    let report = drafts::push_imap(&mut session, &store, &account, &state(), 100_000)
        .await
        .unwrap();
    assert_eq!(report.removed, 1);
    assert_eq!(
        *peer.messages.lock().unwrap(),
        BTreeMap::from([(1, false), (2, true), (4, false)])
    );
    assert!(peer.commands().iter().any(|c| c == "UID EXPUNGE 3"));
}

#[tokio::test]
async fn draft_cleanup_without_identity_preserves_remote_messages_and_queue() {
    let peer = Peer::with_generation("UIDPLUS", 2, renumbered_identities()).await;
    let (store, account) = discarded_draft_with_id(None);
    let mut session = imap::login(&peer.server, "me", "pw").await.unwrap();
    assert!(matches!(
        drafts::push_imap(&mut session, &store, &account, &state(), 100_000).await,
        Err(Error::Server(_))
    ));
    assert_eq!(
        store
            .lock()
            .unwrap()
            .draft_removals(account.id)
            .unwrap()
            .len(),
        1
    );
    assert!(
        !peer
            .commands()
            .iter()
            .any(|c| c.starts_with("UID STORE ") || c.contains("EXPUNGE"))
    );
}
