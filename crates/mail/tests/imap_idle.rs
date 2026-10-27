//! Synthetic IMAP peers for IDLE's failure paths: a server that stops
//! answering, and one that sends keepalives.

use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use spark_mail::engine::password_key;
use spark_mail::imap::{self, Security, Server};
use spark_mail::{AccountConfig, Engine, Secrets, Watch};
use spark_store::{AccountKind, NewAccount, Store};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::sync::Notify;

#[derive(Default)]
struct MemorySecrets(Mutex<HashMap<String, String>>);

impl Secrets for MemorySecrets {
    fn get(&self, name: &str) -> Result<Option<String>, String> {
        Ok(self.0.lock().unwrap().get(name).cloned())
    }
    fn set(&self, name: &str, value: &str) -> Result<(), String> {
        self.0.lock().unwrap().insert(name.into(), value.into());
        Ok(())
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Peer {
    /// Logs in, then never answers again.
    SilentAfterLogin,
    /// Answers, and sends "* OK Still here" every 200 ms while idling.
    Keepalives,
}

/// Serves one connection the way `peer` says.
async fn serve(peer: Peer) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let server = Server {
        host: "127.0.0.1".into(),
        port: listener.local_addr().unwrap().port(),
        security: Security::Plain,
        trusted_cert: None,
    };
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let (read, mut write) = stream.into_split();
        write.write_all(b"* OK synthetic server\r\n").await.unwrap();
        let mut lines = BufReader::new(read).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let (tag, command) = line.split_once(' ').unwrap();
            let command = command.to_ascii_uppercase();
            let reply = if command.starts_with("LOGIN ") {
                format!("{tag} OK logged in\r\n")
            } else if peer == Peer::SilentAfterLogin {
                continue;
            } else if command == "CAPABILITY" {
                format!("* CAPABILITY IMAP4rev1 IDLE\r\n{tag} OK done\r\n")
            } else if command.starts_with("EXAMINE ") {
                format!("* 0 EXISTS\r\n* OK [UIDVALIDITY 1] ok\r\n{tag} OK [READ-ONLY] done\r\n")
            } else if command == "IDLE" {
                write.write_all(b"+ idling\r\n").await.unwrap();
                loop {
                    tokio::select! {
                        line = lines.next_line() => match line {
                            Ok(Some(done)) => {
                                assert_eq!(done, "DONE");
                                break;
                            }
                            // Dropped mid-IDLE.
                            _ => return,
                        },
                        () = tokio::time::sleep(Duration::from_millis(200)) => {
                            write.write_all(b"* OK Still here\r\n").await.unwrap();
                        }
                    }
                }
                format!("{tag} OK IDLE done\r\n")
            } else {
                format!("{tag} OK done\r\n")
            };
            write.write_all(reply.as_bytes()).await.unwrap();
        }
    });
    server
}

#[tokio::test]
async fn a_restart_ends_a_watch_on_a_server_that_stopped_answering() {
    let server = serve(Peer::SilentAfterLogin).await;
    let store = Store::open_in_memory().unwrap();
    let config = serde_json::to_string(&AccountConfig::Imap {
        username: "me@example.test".into(),
        imap: server.clone(),
        smtp: server,
    })
    .unwrap();
    let account = store
        .add_account(&NewAccount {
            kind: AccountKind::Imap,
            email: "me@example.test",
            name: "",
            label: "Test",
            config: &config,
        })
        .unwrap();
    let secrets = MemorySecrets::default();
    secrets.set(&password_key(account.id), "pw").unwrap();
    let engine = Engine::new(Arc::new(Mutex::new(store)), Arc::new(secrets));

    let restart = Notify::new();
    let watching = AtomicBool::new(false);
    let watch = engine.watch_inbox(account.id, &restart, &watching, || {});
    let (watched, ()) = tokio::join!(
        tokio::time::timeout(Duration::from_secs(10), watch),
        async {
            tokio::time::sleep(Duration::from_millis(300)).await;
            restart.notify_one();
        }
    );
    let watched = watched.expect("the watch kept waiting for the server");
    assert_eq!(watched.unwrap(), Watch::Restart);
}

#[tokio::test]
async fn keepalives_dont_put_off_renewing_the_idle() {
    let server = serve(Peer::Keepalives).await;
    let mut session = imap::login(&server, "me@example.test", "pw").await.unwrap();
    assert!(imap::can_idle(&mut session).await.unwrap());
    imap::examine_inbox(&mut session).await.unwrap();
    let renew = Duration::from_secs(1);
    let idled = tokio::time::timeout(
        Duration::from_secs(10),
        imap::idle(session, std::future::pending(), renew),
    )
    .await;
    let Ok(Ok(imap::Idle::Quiet(session))) = idled else {
        panic!("the IDLE wasn't renewed");
    };
    // And the session is ready for the next one.
    let stopped = imap::idle(session, async {}, renew).await;
    assert!(matches!(stopped, Ok(imap::Idle::Stopped)));
}
