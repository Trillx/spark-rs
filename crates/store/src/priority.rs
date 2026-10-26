//! Priority senders. As in Spark, every email from one is Priority: the
//! ones already here when the user adds them, and the ones still to come.
//! Mail that joins a Priority conversation is Priority too, so a reply keeps
//! it on top. Priority stays on this computer; nothing is pushed.

use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};

use crate::{Result, Store, refused};

/// Version 16: the Priority senders, and finding a sender's mail in every
/// account whatever the case of the address.
pub(crate) const SCHEMA_PRIORITY: &str = "
CREATE TABLE priority_senders (
    address TEXT PRIMARY KEY COLLATE NOCASE
);
CREATE INDEX messages_from_address ON messages(from_address COLLATE NOCASE);
";

/// What `Store::prioritize_senders` changed, for `Store::undo_prioritize`:
/// these senders and emails go back to `priority`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PriorityUndo {
    pub senders: Vec<String>,
    pub messages: Vec<i64>,
    pub priority: bool,
}

impl Store {
    /// The Priority senders, in alphabetical order.
    pub fn priority_senders(&self) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT address FROM priority_senders ORDER BY address")?;
        stmt.query_map([], |r| r.get(0))?.collect()
    }

    /// Adds senders to the Priority senders (or with `priority` false, takes
    /// them off), and marks the mail they sent Priority (or not). The user's
    /// own addresses can't be added.
    pub fn prioritize_senders(
        &mut self,
        addresses: &[String],
        priority: bool,
    ) -> Result<PriorityUndo> {
        let tx = self.conn.transaction()?;
        let mut undo = PriorityUndo {
            priority: !priority,
            ..Default::default()
        };
        for address in addresses {
            let address = address.trim();
            if !address.contains('@') {
                return Err(refused("That isn't an email address."));
            }
            let own: bool = tx.query_row(
                "SELECT EXISTS (SELECT 1 FROM accounts WHERE email = ?1 COLLATE NOCASE)",
                [address],
                |r| r.get(0),
            )?;
            if priority && own {
                return Err(refused("That's one of your own addresses."));
            }
            let changed = if priority {
                tx.execute(
                    "INSERT OR IGNORE INTO priority_senders (address) VALUES (?1)",
                    [address],
                )?
            } else {
                tx.execute("DELETE FROM priority_senders WHERE address = ?1", [address])?
            };
            if changed > 0 {
                undo.senders.push(address.to_owned());
            }
            let mut stmt = tx.prepare(
                "UPDATE messages SET priority = ?2
                 WHERE from_address = ?1 COLLATE NOCASE AND priority != ?2 RETURNING id",
            )?;
            for id in stmt.query_map(params![address, priority], |r| r.get(0))? {
                undo.messages.push(id?);
            }
        }
        tx.commit()?;
        Ok(undo)
    }

    /// Puts back what `prioritize_senders` changed.
    pub fn undo_prioritize(&mut self, undo: &PriorityUndo) -> Result<()> {
        let tx = self.conn.transaction()?;
        for address in &undo.senders {
            if undo.priority {
                tx.execute(
                    "INSERT OR IGNORE INTO priority_senders (address) VALUES (?1)",
                    [address],
                )?;
            } else {
                tx.execute("DELETE FROM priority_senders WHERE address = ?1", [address])?;
            }
        }
        for id in &undo.messages {
            tx.execute(
                "UPDATE messages SET priority = ?2 WHERE id = ?1",
                params![id, undo.priority],
            )?;
        }
        tx.commit()
    }
}

/// Marks a newly stored email Priority if a Priority sender sent it, or its
/// conversation is Priority.
pub(crate) fn arrived(conn: &Connection, id: i64) -> Result<()> {
    conn.prepare_cached(
        "UPDATE messages SET priority = 1
         WHERE id = ?1 AND (
             EXISTS (SELECT 1 FROM priority_senders WHERE address = messages.from_address)
             OR EXISTS (
                 SELECT 1 FROM messages o
                 WHERE o.account_id = messages.account_id AND o.conversation = messages.conversation
                   AND o.id != messages.id AND o.priority = 1
                   AND o.state NOT IN ('deleted', 'blocked')))",
    )?
    .execute([id])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::{AccountKind, Added, Category, Incoming, NewAccount, Patch, Store};

    fn store() -> (Store, i64) {
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

    fn arrive(
        store: &mut Store,
        account_id: i64,
        uid: u32,
        from: &str,
        reply_to: Option<u32>,
    ) -> i64 {
        let m = Incoming {
            account_id,
            mailbox: Some("INBOX".into()),
            uid: Some(uid),
            message_id: Some(format!("<{uid}@example.com>")),
            in_reply_to: reply_to.map(|uid| format!("<{uid}@example.com>")),
            from_name: "Sender".into(),
            from_address: from.into(),
            subject: match reply_to {
                Some(uid) => format!("Re: Message {uid}"),
                None => format!("Message {uid}"),
            },
            date: 1_000 + uid as i64,
            unread: true,
            category: Category::People,
            ..Default::default()
        };
        match store.add_incoming(&m, false).unwrap() {
            Added::New(id) => id,
            other => panic!("expected a new message, got {other:?}"),
        }
    }

    fn priority(store: &Store, id: i64) -> bool {
        store.message(id).unwrap().unwrap().priority
    }

    #[test]
    fn a_priority_senders_mail_is_priority_now_and_later() {
        let (mut store, account) = store();
        let old = arrive(&mut store, account, 1, "ana@parts.com", None);
        let other = arrive(&mut store, account, 2, "bo@parts.com", None);

        let undo = store
            .prioritize_senders(&["Ana@Parts.com".into()], true)
            .unwrap();
        assert!(priority(&store, old) && !priority(&store, other));
        assert_eq!(store.priority_senders().unwrap(), ["Ana@Parts.com"]);
        let new = arrive(&mut store, account, 3, "ana@parts.com", None);
        assert!(
            priority(&store, new),
            "new mail from them arrives as Priority"
        );

        store.undo_prioritize(&undo).unwrap();
        assert!(store.priority_senders().unwrap().is_empty());
        assert!(!priority(&store, old));
        assert!(priority(&store, new), "undo leaves mail that came since");
        let later = arrive(&mut store, account, 4, "ana@parts.com", None);
        assert!(!priority(&store, later));
    }

    #[test]
    fn taking_a_sender_off_clears_their_mail_and_undo_puts_it_back() {
        let (mut store, account) = store();
        let first = arrive(&mut store, account, 1, "ana@parts.com", None);
        let second = arrive(&mut store, account, 2, "ana@parts.com", None);
        store
            .prioritize_senders(&["ana@parts.com".into()], true)
            .unwrap();
        // The user took one conversation out of Priority by hand.
        let patch = Patch {
            priority: Some(false),
            ..Default::default()
        };
        store.update(second, &patch, 0).unwrap();

        let undo = store
            .prioritize_senders(&["ana@parts.com".into()], false)
            .unwrap();
        assert!(!priority(&store, first) && !priority(&store, second));
        store.undo_prioritize(&undo).unwrap();
        assert_eq!(store.priority_senders().unwrap(), ["ana@parts.com"]);
        assert!(priority(&store, first));
        assert!(
            !priority(&store, second),
            "undo puts back only what it changed"
        );
    }

    #[test]
    fn replies_to_a_priority_conversation_are_priority() {
        let (mut store, account) = store();
        let first = arrive(&mut store, account, 1, "ana@parts.com", None);
        let patch = Patch {
            priority: Some(true),
            ..Default::default()
        };
        store.update(first, &patch, 0).unwrap();
        let reply = arrive(&mut store, account, 2, "bo@parts.com", Some(1));
        let elsewhere = arrive(&mut store, account, 3, "bo@parts.com", None);
        assert!(priority(&store, reply) && !priority(&store, elsewhere));
    }

    #[test]
    fn only_other_peoples_addresses_can_be_priority_senders() {
        let (mut store, account) = store();
        let mine = arrive(&mut store, account, 1, "me@gmail.com", None);
        for address in ["parts.com", "Me@Gmail.com"] {
            assert!(store.prioritize_senders(&[address.into()], true).is_err());
        }
        assert!(store.priority_senders().unwrap().is_empty());
        assert!(!priority(&store, mine));
    }

    #[test]
    fn a_senders_mail_is_found_through_the_index() {
        let (store, _) = store();
        let plan: String = store
            .conn
            .query_row(
                "EXPLAIN QUERY PLAN UPDATE messages SET priority = 1
                 WHERE from_address = 'ana@parts.com' COLLATE NOCASE AND priority != 1",
                [],
                |r| r.get(3),
            )
            .unwrap();
        assert!(plan.contains("messages_from_address"), "{plan}");
    }
}
