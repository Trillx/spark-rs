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
    for (message_id, pushes) in by_message(pushes) {
        let pending = lock(store).pending_for(message_id)?;
        let found = match &pending {
            Some(p) => imap::find(session, store, state, p).await?,
            None => None,
        };
        if let Some((_, uid)) = found {
            for push in &pushes {
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
                    Ok(_) => {}
                    // Refused: it won't take it next time either.
                    Err(async_imap::error::Error::No(e)) => {
                        eprintln!("spark-rs: Gmail refused the label {}: {e}", push.name);
                    }
                    Err(e) => return Err(e.into()),
                }
            }
            changed += 1;
        }
        let store = lock(store);
        for push in &pushes {
            store.label_pushed(push)?;
        }
    }
    Ok(changed)
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
            match graph.categories(&remote).await {
                Ok(had) => {
                    let wanted = categorized(&had, &pushes);
                    if wanted != had {
                        match graph.set_categories(&remote, &wanted).await {
                            Ok(()) | Err(Error::Gone) => {}
                            Err(e) => return Err(e),
                        }
                        changed += 1;
                    }
                }
                Err(Error::Gone) => {}
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
                ("GET", "/me/messages/AAMk1?$select=categories") => {
                    Answer::json(json!({ "categories": ["Blue category"] }))
                }
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
        assert!(!store.lock().unwrap().has_label_work(account.id).unwrap());
    }

    #[test]
    fn quotes_label_names() {
        assert_eq!(quoted("To respond"), "\"To respond\"");
        assert_eq!(quoted(r#"a"b\c"#), r#""a\"b\\c""#);
    }
}
