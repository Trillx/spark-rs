//! Auto-Labels on the server (see `spark_store::automation`): a Gmail label
//! over IMAP, or an Outlook category on Microsoft 365, so the mail shows
//! them in the account's other apps too. Other IMAP servers have nothing
//! like it; their labels stay on this computer.

use std::collections::BTreeMap;
use std::sync::{Mutex, MutexGuard};

use async_imap::types::Fetch;
use futures_util::TryStreamExt;
use spark_store::{LabelPush, Store};

use crate::graph::Graph;
use crate::imap::{self, Session, SyncState};
use crate::{Error, Result};

/// Sends account `account_id`'s queued labels to its Gmail server. Returns
/// how many messages changed. A message the server no longer has, or a
/// label it refuses, is dropped from the queue; the rest wait for the next
/// round if the connection fails.
pub async fn push_imap(
    session: &mut Session,
    store: &Mutex<Store>,
    state: &SyncState,
    account_id: i64,
) -> Result<usize> {
    let pushes = lock(store).label_pushes(account_id)?;
    if pushes.is_empty() {
        return Ok(0);
    }
    if !session.capabilities().await?.has_str("X-GM-EXT-1") {
        // Not Gmail: nowhere to put them.
        let store = lock(store);
        for push in &pushes {
            store.label_pushed(push)?;
        }
        return Ok(0);
    }
    // Gmail makes a label it's asked to add, but not every version of it
    // has: one that's missing is made first.
    let mut names: Vec<&str> = pushes
        .iter()
        .filter(|p| p.adding)
        .map(|p| p.name.as_str())
        .collect();
    names.sort_unstable();
    names.dedup();
    for name in names {
        if !imap::exists(session, name).await? {
            match session.create(name).await {
                Ok(()) | Err(async_imap::error::Error::No(_)) => {}
                Err(e) => return Err(e.into()),
            }
        }
    }
    let mut changed = 0;
    for (message_id, mut pushes) in by_message(pushes) {
        // Additions first: taking a label off takes the message out of that
        // label's mailbox, which may be where it was found.
        pushes.sort_by_key(|p| !p.adding);
        let pending = lock(store).pending_for(message_id)?;
        let mut found = match &pending {
            Some(p) => imap::find(session, store, state, p).await?,
            None => None,
        };
        // Not where it was known to be (its label renamed or deleted in
        // another app, say): All Mail has it whatever its labels.
        if found.is_none()
            && let (Some(p), Some(all)) = (&pending, &state.archive)
        {
            found = imap::search(session, all, p).await?;
        }
        let mut done = false;
        for push in &pushes {
            let Some((_, uid)) = found else {
                break;
            };
            let mut stored = store_label(session, uid, push).await?;
            if stored == Stored::Missing {
                // Not at that UID any more: moved or relabeled elsewhere
                // since the last sync. All Mail has it whatever its labels.
                found = match (&pending, &state.archive) {
                    (Some(p), Some(all)) => imap::search(session, all, p).await?,
                    _ => None,
                };
                if let Some((_, uid)) = found {
                    stored = store_label(session, uid, push).await?;
                }
            }
            done |= stored == Stored::Done;
        }
        changed += usize::from(done);
        // Done, refused, or the message is gone: off the queue.
        let store = lock(store);
        for push in &pushes {
            store.label_pushed(push)?;
        }
    }
    Ok(changed)
}

/// How a label went on or off a message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stored {
    Done,
    /// The server said OK but changed no message: nothing has that UID in
    /// the selected mailbox now.
    Missing,
    /// It won't take it next time either.
    Refused,
}

/// Puts `push`'s label on message `uid` of the selected mailbox, or takes it
/// off. The answer lists the message it changed, or none if it isn't there.
async fn store_label(session: &mut Session, uid: u32, push: &LabelPush) -> Result<Stored> {
    let op = if push.adding { "+" } else { "-" };
    let item = format!("{op}X-GM-LABELS ({})", quoted(&push.name));
    let stored: async_imap::error::Result<Vec<Fetch>> = async {
        session
            .uid_store(uid.to_string(), item)
            .await?
            .try_collect()
            .await
    }
    .await;
    match stored {
        Ok(fetched) if fetched.iter().any(|f| f.uid == Some(uid)) => Ok(Stored::Done),
        Ok(_) => Ok(Stored::Missing),
        Err(async_imap::error::Error::No(e)) => {
            eprintln!("spark-rs: Gmail refused the label {}: {e}", push.name);
            Ok(Stored::Refused)
        }
        Err(e) => Err(e.into()),
    }
}

/// Sends account `account_id`'s queued labels to Microsoft 365 as Outlook
/// categories, keeping the categories the message had. Returns how many
/// messages changed.
pub async fn push_graph(graph: &Graph, store: &Mutex<Store>, account_id: i64) -> Result<usize> {
    let pushes = lock(store).label_pushes(account_id)?;
    let mut changed = 0;
    for (message_id, pushes) in by_message(pushes) {
        let remote = lock(store)
            .pending_for(message_id)?
            .and_then(|p| p.remote_id);
        if let Some(remote) = remote {
            match set_categories(graph, &remote, &pushes).await {
                Ok(Some(true)) => changed += 1,
                Ok(Some(false)) | Err(Error::Gone) => {}
                // Changed under it every time: the next round tries again.
                Ok(None) => continue,
                Err(e) => return Err(e),
            }
        }
        let store = lock(store);
        for push in &pushes {
            store.label_pushed(push)?;
        }
    }
    Ok(changed)
}

/// Times a message's categories are read again when another app changed
/// them between the read and the write.
const CATEGORY_TRIES: usize = 3;

/// Applies `pushes` to message `remote`'s categories without losing one
/// another app adds meanwhile. Returns whether they changed, or `None` if
/// the message kept changing under it.
async fn set_categories(graph: &Graph, remote: &str, pushes: &[LabelPush]) -> Result<Option<bool>> {
    for _ in 0..CATEGORY_TRIES {
        let (had, version) = graph.categories(remote).await?;
        let wanted = categorized(&had, pushes);
        if wanted == had {
            return Ok(Some(false));
        }
        if graph
            .set_categories(remote, &wanted, version.as_deref())
            .await?
        {
            return Ok(Some(true));
        }
    }
    Ok(None)
}

/// The pushes of each message, in the order they were queued.
fn by_message(pushes: Vec<LabelPush>) -> BTreeMap<i64, Vec<LabelPush>> {
    let mut grouped: BTreeMap<i64, Vec<LabelPush>> = BTreeMap::new();
    for push in pushes {
        grouped.entry(push.message_id).or_default().push(push);
    }
    grouped
}

/// A message's categories `had`, with `pushes` added and taken off.
fn categorized(had: &[String], pushes: &[LabelPush]) -> Vec<String> {
    let mut categories = had.to_vec();
    for push in pushes {
        let present = categories
            .iter()
            .position(|c| c.eq_ignore_ascii_case(&push.name));
        match (push.adding, present) {
            (true, None) => categories.push(push.name.clone()),
            (false, Some(i)) => {
                categories.remove(i);
            }
            _ => {}
        }
    }
    categories
}

/// An IMAP quoted string.
fn quoted(text: &str) -> String {
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}

fn lock(store: &Mutex<Store>) -> MutexGuard<'_, Store> {
    store
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn push(message_id: i64, name: &str, adding: bool) -> LabelPush {
        LabelPush {
            message_id,
            name: name.into(),
            adding,
        }
    }

    #[test]
    fn categories_keep_the_users_own() {
        let had = vec!["Red category".to_owned(), "To respond".to_owned()];
        let pushes = [push(1, "to respond", false), push(1, "Actioned", true)];
        assert_eq!(categorized(&had, &pushes), ["Red category", "Actioned"]);
        // Already there, or already gone: nothing changes.
        let pushes = [push(1, "Red category", true), push(1, "FYI", false)];
        assert_eq!(categorized(&had, &pushes), had);
    }

    #[test]
    fn groups_pushes_by_message() {
        let grouped = by_message(vec![
            push(2, "FYI", true),
            push(1, "News", true),
            push(2, "To respond", false),
        ]);
        assert_eq!(grouped.len(), 2);
        assert_eq!(grouped[&2].len(), 2);
        assert_eq!(grouped[&2][1].name, "To respond");
    }

    #[tokio::test]
    async fn outlook_categories_follow_the_label() {
        use crate::graph::tests::{Answer, serve};
        use serde_json::json;
        use spark_store::{AccountKind, AutoLabel, Incoming, NewAccount};

        let mut store = Store::open_in_memory().unwrap();
        let account = store
            .add_account(&NewAccount {
                kind: AccountKind::Microsoft,
                email: "me@contoso.com",
                name: "Me",
                label: "Work",
                config: "{}",
            })
            .unwrap();
        let mut ids = Vec::new();
        for remote in ["AAMk1", "AAMk2"] {
            let added = store
                .add_incoming(
                    &Incoming {
                        account_id: account.id,
                        remote_id: Some(remote.into()),
                        from_address: "dana@contoso.com".into(),
                        subject: "Budget".into(),
                        ..Default::default()
                    },
                    false,
                )
                .unwrap();
            let spark_store::Added::New(id) = added else {
                panic!("new");
            };
            ids.push(id);
        }
        store
            .set_auto_label(ids[0], Some(AutoLabel::ToRespond), true)
            .unwrap();
        // The second is gone from the server by now.
        store
            .set_auto_label(ids[1], Some(AutoLabel::News), true)
            .unwrap();
        let (base, seen) = serve(
            |asked, _| match (asked.method.as_str(), asked.path.as_str()) {
                ("GET", "/me/messages/AAMk1?$select=categories") => Answer::json(
                    json!({ "categories": ["Blue category"], "@odata.etag": "W/\"v1\"" }),
                ),
                ("PATCH", "/me/messages/AAMk1") => Answer::json(json!({})),
                _ => Answer::status(404),
            },
        )
        .await;
        let graph = Graph::with_base(&base, "token");
        let store = Mutex::new(store);
        assert_eq!(push_graph(&graph, &store, account.id).await.unwrap(), 1);
        let asked = seen.take();
        let patch = asked.iter().find(|a| a.method == "PATCH").unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&patch.body).unwrap(),
            json!({ "categories": ["Blue category", "To respond"] })
        );
        assert_eq!(patch.if_match.as_deref(), Some("W/\"v1\""));
        assert!(!store.lock().unwrap().has_label_work(account.id).unwrap());
    }

    #[tokio::test]
    async fn a_category_added_in_outlook_meanwhile_is_kept() {
        use crate::graph::tests::{Answer, serve};
        use serde_json::json;
        use spark_store::{AccountKind, AutoLabel};

        let (mut store, account, id) = one_email(AccountKind::Microsoft);
        store
            .set_auto_label(id, Some(AutoLabel::News), true)
            .unwrap();
        // The user adds a category in Outlook between the read and the write.
        let (base, seen) = serve(|asked, attempt| match (asked.method.as_str(), attempt) {
            ("GET", 0) => Answer::json(json!({ "categories": ["Existing"], "@odata.etag": "v1" })),
            ("PATCH", 0) => Answer::status(412),
            ("GET", _) => {
                Answer::json(json!({ "categories": ["Existing", "Mine"], "@odata.etag": "v2" }))
            }
            ("PATCH", _) if asked.if_match.as_deref() == Some("v2") => Answer::json(json!({})),
            _ => Answer::status(412),
        })
        .await;
        let graph = Graph::with_base(&base, "token");
        let store = Mutex::new(store);
        assert_eq!(push_graph(&graph, &store, account).await.unwrap(), 1);
        let patches: Vec<_> = seen
            .take()
            .into_iter()
            .filter(|a| a.method == "PATCH")
            .collect();
        assert_eq!(patches.len(), 2);
        assert_eq!(patches[0].if_match.as_deref(), Some("v1"));
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&patches[1].body).unwrap(),
            json!({ "categories": ["Existing", "Mine", "News"] })
        );
        assert!(!store.lock().unwrap().has_label_work(account).unwrap());
    }

    #[tokio::test]
    async fn a_message_that_keeps_changing_waits_for_the_next_round() {
        use crate::graph::tests::{Answer, serve};
        use serde_json::json;
        use spark_store::{AccountKind, AutoLabel};

        let (mut store, account, id) = one_email(AccountKind::Microsoft);
        store
            .set_auto_label(id, Some(AutoLabel::News), true)
            .unwrap();
        let (base, _) = serve(|asked, _| match asked.method.as_str() {
            "GET" => Answer::json(json!({ "categories": [], "@odata.etag": "v1" })),
            _ => Answer::status(412),
        })
        .await;
        let graph = Graph::with_base(&base, "token");
        let store = Mutex::new(store);
        assert_eq!(push_graph(&graph, &store, account).await.unwrap(), 0);
        assert!(store.lock().unwrap().has_label_work(account).unwrap());
    }

    /// An account of `kind` with one inbox email, UID 10, `<1@example.com>`.
    fn one_email(kind: spark_store::AccountKind) -> (Store, i64, i64) {
        email_in(kind, "INBOX", 10)
    }

    /// `one_email`, known to be at `uid` in `mailbox`.
    fn email_in(kind: spark_store::AccountKind, mailbox: &str, uid: u32) -> (Store, i64, i64) {
        use spark_store::{Added, Incoming, NewAccount};
        let mut store = Store::open_in_memory().unwrap();
        let account = store
            .add_account(&NewAccount {
                kind,
                email: "me@example.com",
                name: "Me",
                label: "Me",
                config: "{}",
            })
            .unwrap();
        let Added::New(id) = store
            .add_incoming(
                &Incoming {
                    account_id: account.id,
                    remote_id: Some("AAMk1".into()),
                    mailbox: Some(mailbox.into()),
                    uid: Some(uid),
                    message_id: Some("<1@example.com>".into()),
                    from_address: "dana@example.com".into(),
                    subject: "Budget".into(),
                    ..Default::default()
                },
                false,
            )
            .unwrap()
        else {
            panic!("new");
        };
        (store, account.id, id)
    }

    /// A scripted Gmail: `reply` gives the untagged lines for each command
    /// (without its tag), and every command then gets OK. Returns a session
    /// on it, and the commands it saw once the session is dropped.
    async fn scripted_gmail(
        reply: impl Fn(&str) -> String + Send + 'static,
    ) -> (Session, tokio::task::JoinHandle<Vec<String>>) {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut io = BufReader::new(stream);
            io.get_mut()
                .write_all(b"* OK Gimap ready\r\n")
                .await
                .unwrap();
            let mut commands = Vec::new();
            loop {
                let mut line = String::new();
                if io.read_line(&mut line).await.unwrap_or(0) == 0 {
                    break;
                }
                let (tag, command) = line.trim_end().split_once(' ').unwrap();
                commands.push(command.to_owned());
                let untagged = if command == "CAPABILITY" {
                    "* CAPABILITY IMAP4rev1 UIDPLUS MOVE X-GM-EXT-1\r\n".to_owned()
                } else {
                    reply(command)
                };
                // "NO" refuses the command.
                let answer = if untagged == "NO" {
                    format!("{tag} NO [NONEXISTENT] no such mailbox\r\n")
                } else {
                    format!("{untagged}{tag} OK done\r\n")
                };
                if io.get_mut().write_all(answer.as_bytes()).await.is_err() {
                    break;
                }
            }
            commands
        });
        let server_at = imap::Server {
            host: "127.0.0.1".into(),
            port,
            security: imap::Security::Plain,
            trusted_cert: None,
        };
        (imap::login(&server_at, "me", "pw").await.unwrap(), server)
    }

    /// Answers SELECT with UIDVALIDITY 1, and LIST with the labels there are.
    fn gmail_basics(command: &str) -> Option<String> {
        if command.starts_with("SELECT ") {
            Some("* 1 EXISTS\r\n* OK [UIDVALIDITY 1] UIDs valid\r\n".into())
        } else if command.starts_with("LIST ") {
            Some("* LIST (\\HasNoChildren) \"/\" \"News\"\r\n* LIST (\\HasNoChildren) \"/\" \"FYI\"\r\n* LIST (\\HasNoChildren) \"/\" \"To respond\"\r\n".into())
        } else {
            None
        }
    }

    #[tokio::test]
    async fn a_gmail_message_moved_meanwhile_is_found_in_all_mail() {
        use spark_store::{AccountKind, AutoLabel};
        let (mut store, account, id) = one_email(AccountKind::Gmail);
        store
            .set_auto_label(id, Some(AutoLabel::News), true)
            .unwrap();
        let store = Mutex::new(store);
        // Archived in another app: INBOX's UID 10 is gone, All Mail has it as 77.
        let (mut session, server) = scripted_gmail(|command| {
            if let Some(basic) = gmail_basics(command) {
                return basic;
            }
            if command.starts_with("UID STORE 77 ") {
                "* 5 FETCH (UID 77 X-GM-LABELS (News))\r\n".into()
            } else if command.starts_with("UID SEARCH ") {
                "* SEARCH 77\r\n".into()
            } else if command.starts_with("UID FETCH 77 ") {
                let header = "Message-ID: <1@example.com>\r\n\r\n";
                format!(
                    "* 5 FETCH (UID 77 BODY[HEADER.FIELDS (MESSAGE-ID)] {{{}}}\r\n{header})\r\n",
                    header.len()
                )
            } else {
                // UID STORE 10: nothing has that UID now, so nothing changes.
                String::new()
            }
        })
        .await;
        let state = SyncState {
            uid_validity: Some(1),
            archive: Some("[Gmail]/All Mail".into()),
            ..Default::default()
        };
        assert_eq!(
            push_imap(&mut session, &store, &state, account)
                .await
                .unwrap(),
            1
        );
        assert!(!store.lock().unwrap().has_label_work(account).unwrap());
        drop(session);
        let commands = server.await.unwrap();
        assert!(
            commands
                .iter()
                .any(|c| c.starts_with("UID STORE 10 +X-GM-LABELS"))
        );
        assert!(commands.iter().any(|c| c == "SELECT \"[Gmail]/All Mail\""));
        assert!(
            commands
                .iter()
                .any(|c| c.starts_with("UID STORE 77 +X-GM-LABELS (\"News\")"))
        );
    }

    #[tokio::test]
    async fn a_gmail_message_not_where_it_was_is_found_in_all_mail() {
        use spark_store::{AccountKind, AutoLabel};
        // Known only in a label's mailbox, which was deleted in Gmail since.
        let (mut store, account, id) = email_in(AccountKind::Gmail, "Old label", 5);
        store
            .set_auto_label(id, Some(AutoLabel::News), true)
            .unwrap();
        let store = Mutex::new(store);
        let selected = std::sync::Mutex::new(String::new());
        let (mut session, server) = scripted_gmail(move |command| {
            if let Some(mailbox) = command.strip_prefix("SELECT ") {
                if mailbox == "\"Old label\"" {
                    return "NO".into();
                }
                *selected.lock().unwrap() = mailbox.to_owned();
            }
            if let Some(basic) = gmail_basics(command) {
                return basic;
            }
            let in_all_mail = *selected.lock().unwrap() == "\"[Gmail]/All Mail\"";
            if command.starts_with("UID SEARCH ") {
                if in_all_mail {
                    "* SEARCH 77\r\n".into()
                } else {
                    "* SEARCH\r\n".into()
                }
            } else if command.starts_with("UID FETCH 77 ") {
                let header = "Message-ID: <1@example.com>\r\n\r\n";
                format!(
                    "* 5 FETCH (UID 77 BODY[HEADER.FIELDS (MESSAGE-ID)] {{{}}}\r\n{header})\r\n",
                    header.len()
                )
            } else if command.starts_with("UID STORE 77 ") {
                "* 5 FETCH (UID 77 X-GM-LABELS (News))\r\n".into()
            } else {
                String::new()
            }
        })
        .await;
        let state = SyncState {
            uid_validity: Some(1),
            archive: Some("[Gmail]/All Mail".into()),
            ..Default::default()
        };
        assert_eq!(
            push_imap(&mut session, &store, &state, account)
                .await
                .unwrap(),
            1
        );
        assert!(!store.lock().unwrap().has_label_work(account).unwrap());
        drop(session);
        let commands = server.await.unwrap();
        assert!(
            commands
                .iter()
                .any(|c| c.starts_with("UID STORE 77 +X-GM-LABELS (\"News\")"))
        );
    }

    #[tokio::test]
    async fn a_new_gmail_label_goes_on_before_the_old_one_comes_off() {
        use spark_store::{AccountKind, AutoLabel};
        let (mut store, account, id) = one_email(AccountKind::Gmail);
        store
            .set_auto_label(id, Some(AutoLabel::ToRespond), true)
            .unwrap();
        for push in store.label_pushes(account).unwrap() {
            store.label_pushed(&push).unwrap();
        }
        store
            .set_auto_label(id, Some(AutoLabel::Fyi), true)
            .unwrap();
        let store = Mutex::new(store);
        let (mut session, server) = scripted_gmail(|command| {
            if let Some(basic) = gmail_basics(command) {
                return basic;
            }
            if command.starts_with("UID STORE 10 ") {
                "* 1 FETCH (UID 10 X-GM-LABELS (FYI))\r\n".into()
            } else {
                String::new()
            }
        })
        .await;
        let state = SyncState {
            uid_validity: Some(1),
            ..Default::default()
        };
        assert_eq!(
            push_imap(&mut session, &store, &state, account)
                .await
                .unwrap(),
            1
        );
        drop(session);
        let stores: Vec<String> = server
            .await
            .unwrap()
            .into_iter()
            .filter(|c| c.starts_with("UID STORE"))
            .collect();
        assert_eq!(
            stores,
            [
                "UID STORE 10 +X-GM-LABELS (\"FYI\")",
                "UID STORE 10 -X-GM-LABELS (\"To respond\")"
            ]
        );
    }

    #[test]
    fn quotes_label_names() {
        assert_eq!(quoted("To respond"), "\"To respond\"");
        assert_eq!(quoted(r#"a"b\c"#), r#""a\"b\\c""#);
    }
}
