//! Who to suggest as a recipient while the user types an address: the
//! people in their stored mail.
//!
//! Finding them reads all the mail, so the store keeps them (`People`)
//! from one query to the next, until a write that could change who they
//! are (`WATCH_CONTACTS`). A query only matches and scores the people
//! kept. Scores change with time, so they're kept with the dates of their
//! mail and scored when asked.

use std::collections::{HashMap, HashSet};

use serde::Serialize;

use rusqlite::{params, types::Type};

use crate::{Result, Store};

/// A person to suggest, with the name to show for them ("" when none).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Contact {
    pub name: String,
    pub address: String,
}

/// Version 11: who the user wrote to from here, and who never to suggest.
pub(crate) const SCHEMA_CONTACTS: &str = "
-- Recipients of mail sent from here: they're suggested before the
-- account's Sent folder (which may never sync) has the mail.
CREATE TABLE sent_to (
    name TEXT NOT NULL,
    address TEXT NOT NULL,
    date INTEGER NOT NULL
);

-- Addresses the user asked never to suggest.
CREATE TABLE contact_exceptions (
    address TEXT PRIMARY KEY COLLATE NOCASE
);
";

/// Counts the writes made here that can change who `suggest_contacts`
/// finds: mail added or removed, a message's sender, recipients or date
/// changing, mail sent from here (`sent_to`), and the user's own
/// addresses. Flags and the like don't count. Temporary, so they're made
/// each time the store opens and aren't part of the schema. Commits by
/// other connections show in `PRAGMA data_version` instead, and turned-down
/// addresses are read by each query.
pub(crate) const WATCH_CONTACTS: &str = "
CREATE TEMP TABLE contacts_revision (n INTEGER NOT NULL);
INSERT INTO contacts_revision (n) VALUES (0);

CREATE TEMP TRIGGER contacts_mail_added AFTER INSERT ON main.messages BEGIN
    UPDATE contacts_revision SET n = n + 1;
END;
CREATE TEMP TRIGGER contacts_mail_removed AFTER DELETE ON main.messages BEGIN
    UPDATE contacts_revision SET n = n + 1;
END;
CREATE TEMP TRIGGER contacts_mail_changed
AFTER UPDATE OF from_name, from_address, to_list, cc_list, bcc_list, date ON main.messages BEGIN
    UPDATE contacts_revision SET n = n + 1;
END;

CREATE TEMP TRIGGER contacts_sent_added AFTER INSERT ON main.sent_to BEGIN
    UPDATE contacts_revision SET n = n + 1;
END;
CREATE TEMP TRIGGER contacts_sent_removed AFTER DELETE ON main.sent_to BEGIN
    UPDATE contacts_revision SET n = n + 1;
END;
CREATE TEMP TRIGGER contacts_sent_changed AFTER UPDATE ON main.sent_to BEGIN
    UPDATE contacts_revision SET n = n + 1;
END;

CREATE TEMP TRIGGER contacts_account_added AFTER INSERT ON main.accounts BEGIN
    UPDATE contacts_revision SET n = n + 1;
END;
CREATE TEMP TRIGGER contacts_account_removed AFTER DELETE ON main.accounts BEGIN
    UPDATE contacts_revision SET n = n + 1;
END;
CREATE TEMP TRIGGER contacts_account_changed AFTER UPDATE OF email ON main.accounts BEGIN
    UPDATE contacts_revision SET n = n + 1;
END;
";

/// Each person as written on stored mail, with the dates of that mail and
/// the latest, and how it counts: 2 on mail the user sent, 1 as the sender
/// of mail they got, 0 as someone else on it (without dates: they count
/// while the latest is recent). Mail from the user's own addresses is mail
/// they sent. A column read here must be one `WATCH_CONTACTS` watches.
const PEOPLE: &str = "WITH mine AS (
         SELECT m.* FROM messages m WHERE m.from_address IN (SELECT email FROM accounts)
     ), theirs AS (
         SELECT m.* FROM messages m WHERE m.from_address NOT IN (SELECT email FROM accounts)
     ), sent AS (
         SELECT j.value AS entry, m.date FROM mine m, json_each(m.to_list) j
         UNION ALL SELECT j.value, m.date FROM mine m, json_each(m.cc_list) j
         UNION ALL SELECT j.value, m.date FROM mine m, json_each(m.bcc_list) j
         UNION ALL SELECT CASE WHEN name = '' THEN address ELSE name || ' <' || address || '>' END, date
             FROM sent_to
     ), seen AS (
         SELECT j.value AS entry, m.date FROM theirs m, json_each(m.to_list) j
         UNION ALL SELECT j.value, m.date FROM theirs m, json_each(m.cc_list) j
     )
     SELECT entry, group_concat(date), MAX(date), 2 FROM sent GROUP BY entry
     UNION ALL
     SELECT CASE WHEN from_name = '' THEN from_address ELSE from_name || ' <' || from_address || '>' END,
            group_concat(date), MAX(date), 1
     FROM theirs GROUP BY from_name, from_address
     UNION ALL
     SELECT entry, NULL, MAX(date), 0 FROM seen GROUP BY entry";

/// How far back writing to or hearing from someone counts, in weeks. A
/// week's mail counts for more the more recent it is (Spark's window).
const WEEKS: i64 = 78;
const WEEK_MS: i64 = 7 * 24 * 60 * 60 * 1000;

impl Store {
    /// The people matching `query`, best first: the people the user writes
    /// to, then the people who write to them, then others on mail they got
    /// (lately). The more mail and the more recent, the higher.
    pub fn suggest_contacts(&self, query: &str, now: i64, limit: usize) -> Result<Vec<Contact>> {
        let words: Vec<String> = query.split_whitespace().map(fold).collect();
        if words.is_empty() {
            return Ok(Vec::new());
        }
        let turned_down: HashSet<String> = self
            .conn
            .prepare_cached("SELECT lower(address) FROM contact_exceptions")?
            .query_map([], |r| r.get(0))?
            .collect::<Result<_>>()?;
        self.with_people(|people| people.suggest(&words, &turned_down, now, limit))
    }

    /// The people the user writes to most, then the people who write to
    /// them, best first: `suggest_contacts` with nothing typed yet.
    pub fn top_contacts(&self, now: i64, limit: usize) -> Result<Vec<Contact>> {
        let turned_down: HashSet<String> = self
            .conn
            .prepare_cached("SELECT lower(address) FROM contact_exceptions")?
            .query_map([], |r| r.get(0))?
            .collect::<Result<_>>()?;
        self.with_people(|people| people.suggest(&[], &turned_down, now, limit))
    }

    /// Stops suggesting `address`.
    pub fn never_suggest(&self, address: &str) -> Result<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO contact_exceptions (address) VALUES (?1)",
            [address.trim()],
        )?;
        Ok(())
    }

    /// Notes mail sent from here to `recipients` at `date`.
    pub fn record_sent(&self, recipients: &[Contact], date: i64) -> Result<()> {
        let mut stmt = self
            .conn
            .prepare_cached("INSERT INTO sent_to (name, address, date) VALUES (?1, ?2, ?3)")?;
        for c in recipients {
            stmt.execute(params![c.name, c.address, date])?;
        }
        Ok(())
    }

    /// `f` of the people kept, found again first if a write since may
    /// have changed them.
    fn with_people<T>(&self, f: impl FnOnce(&People) -> T) -> Result<T> {
        let revision = self.contacts_revision()?;
        // A rollback would take the revision back but not the people
        // found in the transaction, so they aren't kept.
        if !self.conn.is_autocommit() {
            return Ok(f(&self.find_people(revision)?));
        }
        let mut kept = self.people.borrow_mut();
        let people = match kept.take() {
            Some(people) if people.revision == revision => kept.insert(people),
            _ => kept.insert(self.find_people(revision)?),
        };
        Ok(f(people))
    }

    /// How far the writes that change who's suggested have got.
    fn contacts_revision(&self) -> Result<Revision> {
        Ok(Revision {
            here: self
                .conn
                .prepare_cached("SELECT n FROM temp.contacts_revision")?
                .query_row([], |r| r.get(0))?,
            elsewhere: self
                .conn
                .prepare_cached("PRAGMA data_version")?
                .query_row([], |r| r.get(0))?,
        })
    }

    /// Everyone on the stored mail who may be suggested.
    fn find_people(&self, revision: Revision) -> Result<People> {
        let own: Vec<String> = self
            .accounts()?
            .into_iter()
            .map(|a| a.email.to_lowercase())
            .collect();
        let mut people: HashMap<String, Vec<Entry>> = HashMap::new();
        let mut stmt = self.conn.prepare(PEOPLE)?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
            ))
        })?;
        for row in rows {
            let (entry, dates, last, role) = row?;
            let role = match role {
                2 => Role::Sent,
                1 => Role::Sender,
                _ => Role::Seen,
            };
            let contact = parse(&entry);
            let key = contact.address.to_lowercase();
            // The user is on mail to them; they're only suggested once they write to themselves.
            if key.is_empty() || (role != Role::Sent && own.contains(&key)) || unanswered(&key) {
                continue;
            }
            people.entry(key).or_default().push(Entry {
                name: fold(&contact.name),
                address: fold(&contact.address),
                contact,
                role,
                dates: newest_first(dates.as_deref())?,
                last,
            });
        }
        Ok(People { revision, people })
    }
}

/// Everyone who may be suggested, as found in the stored mail at
/// `revision`: by lowercase address, the ways they're written on the mail,
/// in the order `PEOPLE` gave them.
pub(crate) struct People {
    revision: Revision,
    people: HashMap<String, Vec<Entry>>,
}

/// Where the writes that change who's suggested were: the count of those
/// made here (`WATCH_CONTACTS`), and other connections' commits.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Revision {
    here: i64,
    elsewhere: i64,
}

/// A way a person is written on the stored mail, and that mail.
struct Entry {
    contact: Contact,
    /// `contact`'s name and address folded, for matching.
    name: String,
    address: String,
    role: Role,
    /// When the mail was, newest first. None for `Role::Seen`.
    dates: Vec<i64>,
    /// When the latest mail was.
    last: i64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Role {
    /// On mail the user sent.
    Sent,
    /// The sender of mail the user got.
    Sender,
    /// Someone else on mail the user got: they score nothing, and count
    /// while the mail is recent.
    Seen,
}

impl People {
    /// The `limit` best at `now` of those every one of `words` (folded)
    /// matches, but not those `turned_down`: what reading the mail at `now`
    /// would find.
    fn suggest(
        &self,
        words: &[String],
        turned_down: &HashSet<String>,
        now: i64,
        limit: usize,
    ) -> Vec<Contact> {
        let since = now.saturating_sub(WEEKS * WEEK_MS);
        let mut found: Vec<Person> = Vec::new();
        for (key, entries) in &self.people {
            let counted = || {
                entries
                    .iter()
                    .filter(|e| e.role != Role::Seen || e.last >= since)
            };
            if turned_down.contains(key) || !counted().any(|e| e.matches(words)) {
                continue;
            }
            let mut person: Option<Person> = None;
            let mut named_at = 0;
            for entry in counted() {
                let p = person.get_or_insert(Person {
                    name: "",
                    address: &entry.contact.address,
                    score: 0,
                    last: 0,
                });
                p.score += entry.score(now);
                p.last = p.last.max(entry.last);
                // The name they had on the latest mail that gave one.
                if !entry.contact.name.is_empty() && entry.last >= named_at {
                    p.name = &entry.contact.name;
                    named_at = entry.last;
                }
            }
            found.extend(person);
        }
        found.sort_by(|a, b| {
            b.score
                .cmp(&a.score)
                .then(b.last.cmp(&a.last))
                .then_with(|| a.address.cmp(b.address))
        });
        found
            .into_iter()
            .take(limit)
            .map(|p| Contact {
                name: p.name.to_owned(),
                address: p.address.to_owned(),
            })
            .collect()
    }
}

impl Entry {
    /// Whether every word starts a word of the name. One word may instead
    /// be part of the address, though not only of its ending (".com").
    fn matches(&self, words: &[String]) -> bool {
        let in_name = words.iter().all(|w| {
            self.name
                .split(|c: char| !c.is_alphanumeric())
                .any(|p| p.starts_with(w.as_str()))
        });
        in_name
            || match words {
                [word] => {
                    let before_ending = self
                        .address
                        .rsplit_once('.')
                        .map_or(self.address.as_str(), |(rest, _)| rest);
                    self.address.starts_with(word.as_str()) || before_ending.contains(word.as_str())
                }
                _ => false,
            }
    }

    /// What its mail counts for at `now`: each `WEEKS + 1`, less a week for
    /// every whole week since it (dividing as SQLite does), down to none.
    /// Mail the user sent counts three times.
    fn score(&self, now: i64) -> i64 {
        let times = match self.role {
            Role::Sent => 3,
            Role::Sender => 1,
            Role::Seen => return 0,
        };
        // Newest first, so after the first that counts for nothing none do.
        let weeks: i64 = self
            .dates
            .iter()
            .map(|&date| WEEKS + 1 - now.saturating_sub(date) / WEEK_MS)
            .take_while(|&n| n > 0)
            .sum();
        times * weeks
    }
}

/// A person a query finds, as it scores them so far.
struct Person<'a> {
    name: &'a str,
    address: &'a str,
    score: i64,
    /// When the latest mail with them was.
    last: i64,
}

/// The dates `group_concat` listed, newest first.
fn newest_first(list: Option<&str>) -> Result<Vec<i64>> {
    let mut dates = list
        .map_or(Ok(Vec::new()), |list| {
            list.split(',').map(str::parse::<i64>).collect()
        })
        .map_err(|e| rusqlite::Error::FromSqlConversionFailure(1, Type::Text, Box::new(e)))?;
    dates.sort_unstable_by(|a, b| b.cmp(a));
    Ok(dates)
}

/// Whether `address` (lowercase) is a machine's that takes no replies,
/// like noreply@ or mailer-daemon@.
fn unanswered(address: &str) -> bool {
    let local: String = address
        .split('@')
        .next()
        .unwrap_or_default()
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect();
    local.contains("noreply")
        || local.contains("donotreply")
        || local == "mailerdaemon"
        || local == "postmaster"
}

/// A recipient as the store keeps it: `Name <address>` or the bare address.
pub(crate) fn parse(entry: &str) -> Contact {
    match entry.rsplit_once('<') {
        Some((name, address)) => Contact {
            name: name.trim().trim_matches('"').trim().to_owned(),
            address: address.trim_end_matches('>').trim().to_owned(),
        },
        None => Contact {
            name: String::new(),
            address: entry.trim().to_owned(),
        },
    }
}

/// Lowercase without accents, so "Núñez" is "nunez".
fn fold(text: &str) -> String {
    const PLAIN: [(&str, char); 19] = [
        ("àáâãäåāăą", 'a'),
        ("çćĉċč", 'c'),
        ("ďđ", 'd'),
        ("èéêëēĕėęě", 'e'),
        ("ĝğġģ", 'g'),
        ("ĥħ", 'h'),
        ("ìíîïĩīĭįı", 'i'),
        ("ĵ", 'j'),
        ("ķ", 'k'),
        ("ĺļľŀł", 'l'),
        ("ñńņňŉ", 'n'),
        ("òóôõöøōŏő", 'o'),
        ("ŕŗř", 'r'),
        ("śŝşšș", 's'),
        ("ţťŧț", 't'),
        ("ùúûüũūŭůűų", 'u'),
        ("ŵ", 'w'),
        ("ýÿŷ", 'y'),
        ("źżž", 'z'),
    ];
    let mut out = String::with_capacity(text.len());
    for c in text.to_lowercase().chars() {
        match c {
            // Combining accents, as in a decomposed "é".
            '\u{300}'..='\u{36f}' => {}
            'ß' => out.push_str("ss"),
            'æ' => out.push_str("ae"),
            'œ' => out.push_str("oe"),
            'þ' => out.push_str("th"),
            'ð' => out.push('d'),
            _ => out.push(
                PLAIN
                    .iter()
                    .find(|(from, _)| from.contains(c))
                    .map_or(c, |&(_, to)| to),
            ),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AccountKind, Added, DraftSave, Incoming, MessageState, NewAccount, Patch};

    const DAY: i64 = 24 * 60 * 60 * 1000;
    const NOW: i64 = 1_000 * DAY;

    fn store() -> Store {
        let store = Store::open_in_memory().unwrap();
        store
            .add_account(&NewAccount {
                kind: AccountKind::Gmail,
                email: "me@gmail.com",
                name: "Me",
                label: "Personal",
                config: "{}",
            })
            .unwrap();
        store
    }

    fn mail(store: &mut Store, from: &str, to: &[&str], days_ago: i64) -> i64 {
        mail_cc(store, from, to, &[], days_ago)
    }

    fn mail_cc(store: &mut Store, from: &str, to: &[&str], cc: &[&str], days_ago: i64) -> i64 {
        mail_at(store, from, to, cc, &[], NOW - days_ago * DAY)
    }

    /// Stores mail from `from` to the others at `date`; gives its id.
    fn mail_at(
        store: &mut Store,
        from: &str,
        to: &[&str],
        cc: &[&str],
        bcc: &[&str],
        date: i64,
    ) -> i64 {
        let uid = store
            .conn
            .query_row("SELECT COALESCE(MAX(id), 0) FROM messages", [], |r| {
                r.get::<_, u32>(0)
            })
            .unwrap()
            + 1;
        let (from_name, from_address) = match from.split_once(" <") {
            Some((name, address)) => (name.to_owned(), address.trim_end_matches('>').to_owned()),
            None => (String::new(), from.to_owned()),
        };
        let m = Incoming {
            account_id: 1,
            mailbox: Some("INBOX".into()),
            uid: Some(uid),
            message_id: Some(format!("<{uid}@example.com>")),
            from_name,
            from_address,
            to: to.iter().map(|s| s.to_string()).collect(),
            cc: cc.iter().map(|s| s.to_string()).collect(),
            bcc: bcc.iter().map(|s| s.to_string()).collect(),
            subject: format!("Message {uid}"),
            date,
            ..Default::default()
        };
        match store.add_incoming(&m, false).unwrap() {
            Added::New(id) | Added::Known(id) => id,
        }
    }

    fn suggested(store: &Store, query: &str) -> Vec<String> {
        suggested_at(store, query, NOW)
    }

    /// What's suggested for `query` at `now`, which must be what reading
    /// the mail finds.
    fn suggested_at(store: &Store, query: &str, now: i64) -> Vec<String> {
        let found = store.suggest_contacts(query, now, 10).unwrap();
        assert_eq!(
            found,
            read_from_mail(store, query, now, 10),
            "{query:?} at {now}"
        );
        found.into_iter().map(shown).collect()
    }

    fn shown(c: Contact) -> String {
        if c.name.is_empty() {
            c.address
        } else {
            format!("{} <{}>", c.name, c.address)
        }
    }

    /// `suggest_contacts` as it was before it kept people between
    /// queries: everyone read from the mail and ranked on each query.
    fn read_from_mail(store: &Store, query: &str, now: i64, limit: usize) -> Vec<Contact> {
        struct Found {
            contact: Contact,
            score: i64,
            last: i64,
            named_at: i64,
            matched: bool,
        }
        fn matches(words: &[String], contact: &Contact) -> bool {
            let name = fold(&contact.name);
            let parts: Vec<&str> = name.split(|c: char| !c.is_alphanumeric()).collect();
            let in_name = words
                .iter()
                .all(|w| parts.iter().any(|p| p.starts_with(w.as_str())));
            in_name
                || match words {
                    [word] => {
                        let address = fold(&contact.address);
                        let before_ending = address
                            .rsplit_once('.')
                            .map_or(address.as_str(), |(rest, _)| rest);
                        address.starts_with(word.as_str()) || before_ending.contains(word.as_str())
                    }
                    _ => false,
                }
        }
        let words: Vec<String> = query.split_whitespace().map(fold).collect();
        if words.is_empty() {
            return Vec::new();
        }
        let mut stmt = store
            .conn
            .prepare(
                "WITH mine AS (
                 SELECT m.* FROM messages m WHERE m.from_address IN (SELECT email FROM accounts)
             ), theirs AS (
                 SELECT m.* FROM messages m WHERE m.from_address NOT IN (SELECT email FROM accounts)
             ), sent AS (
                 SELECT j.value AS entry, m.date FROM mine m, json_each(m.to_list) j
                 UNION ALL SELECT j.value, m.date FROM mine m, json_each(m.cc_list) j
                 UNION ALL SELECT j.value, m.date FROM mine m, json_each(m.bcc_list) j
                 UNION ALL SELECT CASE WHEN name = '' THEN address ELSE name || ' <' || address || '>' END, date
                     FROM sent_to
             ), seen AS (
                 SELECT j.value AS entry, m.date FROM theirs m, json_each(m.to_list) j WHERE m.date >= ?3
                 UNION ALL SELECT j.value, m.date FROM theirs m, json_each(m.cc_list) j WHERE m.date >= ?3
             )
             SELECT entry, 3 * SUM(MAX(0, ?2 + 1 - (?1 - date) / ?4)), MAX(date), 1 FROM sent GROUP BY entry
             UNION ALL
             SELECT CASE WHEN from_name = '' THEN from_address ELSE from_name || ' <' || from_address || '>' END,
                    SUM(MAX(0, ?2 + 1 - (?1 - date) / ?4)), MAX(date), 0
             FROM theirs GROUP BY from_name, from_address
             UNION ALL
             SELECT entry, 0, MAX(date), 0 FROM seen GROUP BY entry",
            )
            .unwrap();
        let own: Vec<String> = store
            .accounts()
            .unwrap()
            .into_iter()
            .map(|a| a.email.to_lowercase())
            .collect();
        let turned_down: HashSet<String> = store
            .conn
            .prepare("SELECT lower(address) FROM contact_exceptions")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_>>()
            .unwrap();
        let mut people: HashMap<String, Found> = HashMap::new();
        let rows = stmt
            .query_map(params![now, WEEKS, now - WEEKS * WEEK_MS, WEEK_MS], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, bool>(3)?,
                ))
            })
            .unwrap();
        for row in rows {
            let (entry, score, last, sent) = row.unwrap();
            let contact = parse(&entry);
            let key = contact.address.to_lowercase();
            if key.is_empty()
                || (!sent && own.contains(&key))
                || turned_down.contains(&key)
                || unanswered(&key)
            {
                continue;
            }
            let person = people.entry(key).or_insert(Found {
                contact: Contact {
                    name: String::new(),
                    address: contact.address.clone(),
                },
                score: 0,
                last: 0,
                named_at: 0,
                matched: false,
            });
            person.score += score;
            person.last = person.last.max(last);
            person.matched |= matches(&words, &contact);
            if !contact.name.is_empty() && last >= person.named_at {
                person.contact.name = contact.name;
                person.named_at = last;
            }
        }
        let mut people: Vec<Found> = people.into_values().filter(|p| p.matched).collect();
        people.sort_by(|a, b| {
            b.score
                .cmp(&a.score)
                .then(b.last.cmp(&a.last))
                .then_with(|| a.contact.address.cmp(&b.contact.address))
        });
        people.into_iter().take(limit).map(|p| p.contact).collect()
    }

    /// Empties the people kept, leaving them current: a query that then
    /// finds nobody used them without reading the mail again.
    fn empty_kept(store: &Store) {
        store
            .people
            .borrow_mut()
            .as_mut()
            .expect("people are kept")
            .people
            .clear();
    }

    /// What's suggested for `query` from the people kept, if still current.
    fn from_kept(store: &Store, query: &str) -> Vec<Contact> {
        store.suggest_contacts(query, NOW, 10).unwrap()
    }

    #[test]
    fn someone_you_wrote_to_is_found_by_the_start_of_their_name() {
        let mut store = store();
        mail(
            &mut store,
            "Me <me@gmail.com>",
            &["Ana Ruiz <ar@example.com>"],
            3,
        );
        assert_eq!(suggested(&store, "an"), ["Ana Ruiz <ar@example.com>"]);
        assert_eq!(suggested(&store, "Ruiz"), ["Ana Ruiz <ar@example.com>"]);
        assert!(
            suggested(&store, "uiz").is_empty(),
            "names match from the start of a word"
        );
    }

    #[test]
    fn one_word_also_matches_any_part_of_the_address_but_its_ending() {
        let mut store = store();
        mail(
            &mut store,
            "Me <me@gmail.com>",
            &["Ana Ruiz <ana.ruiz@example.com>", "ops@acme.io"],
            3,
        );
        assert_eq!(
            suggested(&store, "ruiz@ex"),
            ["Ana Ruiz <ana.ruiz@example.com>"]
        );
        assert_eq!(suggested(&store, "ACME"), ["ops@acme.io"]);
        assert_eq!(suggested(&store, "ops@acme.io"), ["ops@acme.io"]);
        assert!(
            suggested(&store, "com").is_empty(),
            "everyone's address ends in .com"
        );
        assert!(
            suggested(&store, "ana acme").is_empty(),
            "several words match names only"
        );
    }

    #[test]
    fn case_and_accents_are_ignored() {
        let mut store = store();
        mail(
            &mut store,
            "Me <me@gmail.com>",
            &["José Núñez <jn@example.com>", "Zoë Ørsted <zo@example.com>"],
            3,
        );
        assert_eq!(
            suggested(&store, "jose nunez"),
            ["José Núñez <jn@example.com>"]
        );
        assert_eq!(suggested(&store, "NÚÑ"), ["José Núñez <jn@example.com>"]);
        assert_eq!(suggested(&store, "zoe"), ["Zoë Ørsted <zo@example.com>"]);
        assert_eq!(suggested(&store, "orst"), ["Zoë Ørsted <zo@example.com>"]);
    }

    #[test]
    fn people_you_write_to_come_first_then_people_who_write_to_you_then_others_on_your_mail() {
        let mut store = store();
        mail_cc(
            &mut store,
            "Anna Wrote <anna.w@x.com>",
            &["me@gmail.com"],
            &["Anna Copied <anna.c@x.com>"],
            1,
        );
        mail(
            &mut store,
            "Me <me@gmail.com>",
            &["Anna Sent <anna.s@x.com>"],
            70,
        );
        mail_cc(
            &mut store,
            "Me <me@gmail.com>",
            &["someone@y.com"],
            &["Anna Cc <anna.cc@x.com>"],
            7 * 30,
        );
        assert_eq!(
            suggested(&store, "anna"),
            [
                "Anna Sent <anna.s@x.com>",
                "Anna Cc <anna.cc@x.com>",
                "Anna Wrote <anna.w@x.com>",
                "Anna Copied <anna.c@x.com>",
            ]
        );
    }

    #[test]
    fn people_you_write_to_often_and_lately_come_first() {
        let mut store = store();
        mail(
            &mut store,
            "Me <me@gmail.com>",
            &["Ben Ancient <ancient@x.com>"],
            7 * 100,
        );
        mail(
            &mut store,
            "Me <me@gmail.com>",
            &["Ben Old <old@x.com>"],
            7 * 60,
        );
        mail(&mut store, "Me <me@gmail.com>", &["Ben New <new@x.com>"], 7);
        for _ in 0..3 {
            mail(
                &mut store,
                "Me <me@gmail.com>",
                &["Ben Often <often@x.com>"],
                7 * 30,
            );
        }
        assert_eq!(
            suggested(&store, "ben"),
            [
                "Ben Often <often@x.com>",
                "Ben New <new@x.com>",
                "Ben Old <old@x.com>",
                "Ben Ancient <ancient@x.com>",
            ]
        );
        assert_eq!(store.suggest_contacts("ben", NOW, 2).unwrap().len(), 2);
    }

    #[test]
    fn people_written_to_from_here_count_before_the_sent_folder_syncs() {
        let mut store = store();
        mail(
            &mut store,
            "Cleo Wrote <cleo.w@x.com>",
            &["me@gmail.com"],
            1,
        );
        let cleo = Contact {
            name: "Cleo Park".into(),
            address: "cleo@x.com".into(),
        };
        store.record_sent(&[cleo], NOW - 7 * DAY).unwrap();
        assert_eq!(
            suggested(&store, "cleo"),
            ["Cleo Park <cleo@x.com>", "Cleo Wrote <cleo.w@x.com>"]
        );
    }

    #[test]
    fn sending_to_a_bare_address_counts_when_searching_its_learned_name() {
        let mut store = store();
        for _ in 0..3 {
            mail(&mut store, "Me <me@gmail.com>", &["favorite@x.com"], 1);
        }
        mail(
            &mut store,
            "Ana Ruiz <favorite@x.com>",
            &["me@gmail.com"],
            0,
        );
        mail(
            &mut store,
            "Me <me@gmail.com>",
            &["Ana Other <other@x.com>"],
            0,
        );
        assert_eq!(
            suggested(&store, "ana"),
            ["Ana Ruiz <favorite@x.com>", "Ana Other <other@x.com>"]
        );
    }

    #[test]
    fn addresses_the_user_turned_down_and_no_reply_ones_are_never_suggested() {
        let mut store = store();
        mail(
            &mut store,
            "Me <me@gmail.com>",
            &["Dana Old <dana@old.com>", "Dana New <dana@new.com>"],
            1,
        );
        store.never_suggest("DANA@old.com").unwrap();
        assert_eq!(suggested(&store, "dana"), ["Dana New <dana@new.com>"]);

        for from in [
            "Updates <noreply@github.com>",
            "Updates <no-reply@bank.com>",
            "Updates <do_not_reply@shop.com>",
            "Updates <donotreply@x.com>",
            "Updates <mailer-daemon@x.com>",
        ] {
            mail(&mut store, from, &["me@gmail.com"], 1);
        }
        mail(
            &mut store,
            "Updates Team <team@x.com>",
            &["me@gmail.com"],
            1,
        );
        assert_eq!(suggested(&store, "updates"), ["Updates Team <team@x.com>"]);
    }

    const HOUR: i64 = 60 * 60 * 1000;

    /// Mail mixing the ways a ranking can tie or change: the same people
    /// written several ways and in other cases, the user's two addresses,
    /// mail sent from here, turned-down and no-reply addresses, many mails
    /// at one moment, dates over two years and some ahead of `NOW`.
    fn mixed_mailbox() -> Store {
        let mut store = store();
        store
            .add_account(&NewAccount {
                kind: AccountKind::Imap,
                email: "Me@Work.org",
                name: "Me",
                label: "Work",
                config: "{}",
            })
            .unwrap();
        let people = [
            "Ana Ruiz <ana@x.com>",
            "ana ruiz <ANA@x.com>",
            "Ana R. <ana@x.com>",
            "ana@x.com",
            "\"Ruiz, Ana\" <Ana@X.com>",
            "José Núñez <jose@y.es>",
            "Jose <JOSE@y.es>",
            "Ben Old <ben@old.com>",
            "Ben <ben@new.com>",
            "Anna Bell <anna@bell.io>",
            "Ann Lee <ann@lee.org>",
            "Lee Ann <lee@ann.org>",
            "Zoë Ørsted <zoe@dk.dk>",
            "ops@acme.io",
            "Dana <dana@turned.down>",
            "Shipping <noreply@ship.com>",
            "Me <me@gmail.com>",
            "Me at work <me@work.org>",
        ];
        let mut seed: u64 = 1;
        let mut pick = |n: usize| {
            seed = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (seed >> 33) as usize % n
        };
        for _ in 0..300 {
            // A third from the user.
            let from = match pick(3) {
                0 => people[16 + pick(2)],
                _ => people[pick(people.len())],
            };
            let to: Vec<&str> = (0..1 + pick(2))
                .map(|_| people[pick(people.len())])
                .collect();
            let cc: Vec<&str> = (0..pick(3)).map(|_| people[pick(people.len())]).collect();
            let bcc: Vec<&str> = (0..pick(2)).map(|_| people[pick(people.len())]).collect();
            // Every six hours, so many share a moment and a week's end.
            let date = NOW + 2 * WEEK_MS - pick(110 * 7 * 4) as i64 * 6 * HOUR;
            mail_at(&mut store, from, &to, &cc, &bcc, date);
        }
        for _ in 0..20 {
            let to = parse(people[pick(people.len())]);
            let date = NOW - pick(100 * 7 * 4) as i64 * 6 * HOUR;
            store.record_sent(&[to], date).unwrap();
        }
        store.never_suggest("DANA@turned.down").unwrap();
        store
    }

    #[test]
    fn the_people_kept_suggest_what_reading_the_mail_would() {
        let store = mixed_mailbox();
        assert!(store.suggest_contacts("a", NOW, 100).unwrap().len() > 5);
        let queries = [
            "a", "an", "ana", "Ana R", "ruiz ana", "ANA@X", "x", "jose", "núñez", "ben", "b",
            "lee", "ann", "zoe", "ops", "me", "dana", "ship", "e", "o", "nobody",
        ];
        let times = [
            0,
            1,
            -1,
            6 * HOUR - 1,
            6 * HOUR,
            DAY,
            WEEK_MS - 1,
            WEEK_MS,
            3 * WEEK_MS + 5 * HOUR,
            26 * WEEK_MS,
            80 * WEEK_MS,
        ];
        for now in times.map(|t| NOW + t) {
            for query in queries {
                for limit in [1, 4, 1000] {
                    assert_eq!(
                        store.suggest_contacts(query, now, limit).unwrap(),
                        read_from_mail(&store, query, now, limit),
                        "{query:?} at {now}, {limit} of them"
                    );
                }
            }
        }
    }

    #[test]
    fn different_queries_over_the_same_mail_use_the_people_kept() {
        let mut store = store();
        mail(
            &mut store,
            "Me <me@gmail.com>",
            &["Ana Ruiz <ar@x.com>", "Ben Old <bo@x.com>"],
            3,
        );
        assert_eq!(suggested(&store, "ana"), ["Ana Ruiz <ar@x.com>"]);
        empty_kept(&store);
        for query in ["ben", "ana", "ruiz a", "x"] {
            assert!(
                from_kept(&store, query).is_empty(),
                "{query:?} read the mail"
            );
        }
    }

    #[test]
    fn reading_pinning_or_archiving_mail_keeps_the_people() {
        let mut store = store();
        let id = mail(&mut store, "Ana Ruiz <ar@x.com>", &["me@gmail.com"], 3);
        assert_eq!(suggested(&store, "ana"), ["Ana Ruiz <ar@x.com>"]);
        let read = Patch {
            unread: Some(false),
            pinned: Some(true),
            ..Default::default()
        };
        store.update(id, &read, NOW).unwrap();
        assert_eq!(suggested(&store, "ana"), ["Ana Ruiz <ar@x.com>"]);

        empty_kept(&store);
        let done = Patch {
            state: Some(MessageState::Done),
            unread: Some(true),
            pinned: Some(false),
            ..Default::default()
        };
        store.update(id, &done, NOW).unwrap();
        assert!(from_kept(&store, "ana").is_empty(), "read the mail");
    }

    #[test]
    fn mail_added_or_removed_shows_on_the_next_query() {
        let mut store = store();
        mail(&mut store, "Me <me@gmail.com>", &["Ana Ruiz <ar@x.com>"], 3);
        assert!(suggested(&store, "ben").is_empty());
        let id = mail(&mut store, "Ben Old <bo@x.com>", &["me@gmail.com"], 1);
        assert_eq!(suggested(&store, "ben"), ["Ben Old <bo@x.com>"]);
        store.remove_messages(&[id]).unwrap();
        assert!(suggested(&store, "ben").is_empty());
        assert_eq!(suggested(&store, "ana"), ["Ana Ruiz <ar@x.com>"]);
    }

    #[test]
    fn a_message_whose_people_or_date_change_shows_on_the_next_query() {
        let mut store = store();
        // A draft is mail from the user: who it's to counts.
        let to = ["Cleo Park <cleo@x.com>".to_owned()];
        let mut draft = DraftSave {
            account_id: 1,
            draft: "{}",
            send_later: None,
            from_name: "Me",
            from_address: "me@gmail.com",
            to: &to,
            cc: &[],
            bcc: &[],
            subject: "Plan",
            snippet: "",
            text: "",
            html: None,
            in_reply_to: None,
            references: None,
            attachments: &[],
            files: &[],
        };
        let id = store.save_draft(None, &draft, NOW).unwrap();
        assert_eq!(suggested(&store, "cleo"), ["Cleo Park <cleo@x.com>"]);
        let renamed = ["Cleo Lane <cleo@x.com>".to_owned()];
        draft.to = &renamed;
        store.save_draft(Some(id), &draft, NOW).unwrap();
        assert_eq!(suggested(&store, "cleo"), ["Cleo Lane <cleo@x.com>"]);

        let got = mail(&mut store, "Dan Fox <dan@x.com>", &["me@gmail.com"], 2);
        let sent = mail(&mut store, "Me <me@gmail.com>", &["Eve <eve@x.com>"], 2);
        let old = mail(
            &mut store,
            "Me <me@gmail.com>",
            &["Ivo Date <ivo@x.com>"],
            400,
        );
        mail(
            &mut store,
            "Me <me@gmail.com>",
            &["Ida Date <ida@x.com>"],
            2,
        );
        assert_eq!(
            suggested(&store, "date"),
            ["Ida Date <ida@x.com>", "Ivo Date <ivo@x.com>"]
        );
        for (id, column, value, query, expected) in [
            (
                got,
                "from_name",
                "Dan Wolf",
                "dan",
                ["Dan Wolf <dan@x.com>"],
            ),
            (
                got,
                "from_address",
                "dan@wolf.com",
                "dan",
                ["Dan Wolf <dan@wolf.com>"],
            ),
            (
                sent,
                "to_list",
                r#"["Eve Hart <eve@x.com>"]"#,
                "eve",
                ["Eve Hart <eve@x.com>"],
            ),
            (
                sent,
                "cc_list",
                r#"["Finn <finn@x.com>"]"#,
                "finn",
                ["Finn <finn@x.com>"],
            ),
            (
                sent,
                "bcc_list",
                r#"["Gus <gus@x.com>"]"#,
                "gus",
                ["Gus <gus@x.com>"],
            ),
        ] {
            store
                .conn
                .execute(
                    &format!("UPDATE messages SET {column} = ?2 WHERE id = ?1"),
                    params![id, value],
                )
                .unwrap();
            assert_eq!(suggested(&store, query), expected, "{column}");
        }
        store
            .conn
            .execute(
                "UPDATE messages SET date = ?2 WHERE id = ?1",
                params![old, NOW - DAY],
            )
            .unwrap();
        assert_eq!(
            suggested(&store, "date"),
            ["Ivo Date <ivo@x.com>", "Ida Date <ida@x.com>"]
        );
    }

    #[test]
    fn an_address_of_the_users_added_or_removed_shows_on_the_next_query() {
        let mut store = store();
        mail(
            &mut store,
            "Me Elsewhere <me@work.org>",
            &["Kim Cho <kim@work.org>"],
            2,
        );
        mail(&mut store, "Kai Cho <kai@x.com>", &["me@gmail.com"], 1);
        // Someone else's mail: Kai wrote to the user, Kim was only on it.
        assert_eq!(suggested(&store, "me"), ["Me Elsewhere <me@work.org>"]);
        assert_eq!(
            suggested(&store, "cho"),
            ["Kai Cho <kai@x.com>", "Kim Cho <kim@work.org>"]
        );
        let work = store
            .add_account(&NewAccount {
                kind: AccountKind::Imap,
                email: "me@work.org",
                name: "Me",
                label: "Work",
                config: "{}",
            })
            .unwrap();
        // Now the user wrote to Kim.
        assert!(suggested(&store, "me").is_empty());
        assert_eq!(
            suggested(&store, "cho"),
            ["Kim Cho <kim@work.org>", "Kai Cho <kai@x.com>"]
        );
        store.remove_account(work.id).unwrap();
        assert_eq!(suggested(&store, "me"), ["Me Elsewhere <me@work.org>"]);
    }

    #[test]
    fn turning_someone_down_or_writing_to_them_from_here_shows_on_the_next_query() {
        let mut store = store();
        mail(
            &mut store,
            "Me <me@gmail.com>",
            &["Dana Old <dana@old.com>", "Dana New <dana@new.com>"],
            1,
        );
        assert_eq!(
            suggested(&store, "dana"),
            ["Dana New <dana@new.com>", "Dana Old <dana@old.com>"]
        );
        store.never_suggest("DANA@old.com").unwrap();
        assert_eq!(suggested(&store, "dana"), ["Dana New <dana@new.com>"]);
        let lim = Contact {
            name: "Dana Lim".into(),
            address: "dana@lim.com".into(),
        };
        store.record_sent(&[lim], NOW).unwrap();
        assert_eq!(
            suggested(&store, "dana"),
            ["Dana Lim <dana@lim.com>", "Dana New <dana@new.com>"]
        );
    }

    #[test]
    fn scores_and_recent_mail_follow_the_time_without_a_write() {
        let mut store = store();
        // In its last week of counting at NOW: 3 × 1.
        let last_week = NOW - 79 * WEEK_MS + 1;
        mail_at(
            &mut store,
            "Me <me@gmail.com>",
            &["Sam Xu <xu@x.com>"],
            &[],
            &[],
            last_week,
        );
        // From 77 weeks before: 2.
        let sam_yi = "Sam Yi <yi@x.com>";
        mail_at(
            &mut store,
            sam_yi,
            &["me@gmail.com"],
            &[],
            &[],
            NOW - 77 * WEEK_MS,
        );
        // Only on mail, as long ago as counts.
        let on_it = ["Sam Ng <ng@x.com>"];
        mail_at(
            &mut store,
            "Other <o@x.com>",
            &on_it,
            &[],
            &[],
            NOW - WEEKS * WEEK_MS,
        );
        let all = [
            "Sam Xu <xu@x.com>",
            "Sam Yi <yi@x.com>",
            "Sam Ng <ng@x.com>",
        ];
        assert_eq!(suggested_at(&store, "sam", NOW), all);
        assert_eq!(
            suggested_at(&store, "sam", NOW + 1),
            ["Sam Yi <yi@x.com>", "Sam Xu <xu@x.com>"]
        );
        // And back, should the clock be.
        assert_eq!(suggested_at(&store, "sam", NOW), all);
    }

    #[test]
    fn another_connections_writes_show_on_the_next_query() {
        let dir = std::env::temp_dir().join(format!("spark-contacts-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.sqlite");
        let _ = std::fs::remove_file(&path);
        let store = Store::open(&path).unwrap();
        let ana = Contact {
            name: "Ana Ruiz".into(),
            address: "ar@x.com".into(),
        };
        store.record_sent(&[ana], NOW).unwrap();
        assert_eq!(suggested(&store, "ruiz"), ["Ana Ruiz <ar@x.com>"]);
        let other = rusqlite::Connection::open(&path).unwrap();
        other
            .execute(
                "INSERT INTO sent_to (name, address, date) VALUES ('Bo Ruiz', 'bo@x.com', ?1)",
                [NOW],
            )
            .unwrap();
        assert_eq!(
            suggested(&store, "ruiz"),
            ["Ana Ruiz <ar@x.com>", "Bo Ruiz <bo@x.com>"]
        );
        drop((other, store));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn people_found_in_a_transaction_are_not_kept() {
        let mut store = store();
        mail(&mut store, "Me <me@gmail.com>", &["Ana Ruiz <ar@x.com>"], 1);
        assert_eq!(suggested(&store, "ruiz"), ["Ana Ruiz <ar@x.com>"]);
        let tx = store.conn.unchecked_transaction().unwrap();
        let bo = Contact {
            name: "Bo Ruiz".into(),
            address: "bo@x.com".into(),
        };
        store.record_sent(&[bo], NOW).unwrap();
        assert_eq!(
            suggested(&store, "ruiz"),
            ["Bo Ruiz <bo@x.com>", "Ana Ruiz <ar@x.com>"]
        );
        drop(tx);
        // As many writes as in the rolled-back transaction.
        let cy = Contact {
            name: "Cy Ruiz".into(),
            address: "cy@x.com".into(),
        };
        store.record_sent(&[cy], NOW).unwrap();
        assert_eq!(
            suggested(&store, "ruiz"),
            ["Cy Ruiz <cy@x.com>", "Ana Ruiz <ar@x.com>"]
        );
    }

    /// The audit's case (PERF-04): 50,000 messages, a fifth of them from
    /// the user, each with one To and one Cc among some 5,000 people over
    /// the last 78 weeks.
    #[test]
    #[ignore = "a timing: cargo test --release -p spark-store -- --ignored --nocapture"]
    fn time_finding_people_and_answering_queries() {
        use std::time::{Duration, Instant};

        fn median(mut runs: Vec<Duration>) -> Duration {
            runs.sort();
            runs[runs.len() / 2]
        }

        let mut store = store();
        let first = [
            "Ana", "Ben", "Cleo", "Dan", "Eve", "Finn", "Gus", "Hana", "Ivo", "José", "Kim", "Lee",
            "Mia", "Noa", "Olga", "Pia", "Quinn", "Rui", "Sam", "Tom", "Uma", "Vic", "Wen", "Xu",
            "Yara", "Zoë",
        ];
        let syllables = [
            "ba", "ce", "di", "fo", "gu", "ha", "ke", "li", "mo", "nu", "pa", "re", "si", "to",
            "vu",
        ];
        let person = |i: usize| {
            let last = format!(
                "{}{}{}",
                syllables[i / 26 % 15].to_uppercase(),
                syllables[i / 26 / 15 % 15],
                syllables[i % 15]
            );
            let name = format!("{} {last}", first[i % 26]);
            let address = format!("{}.{}@d{}.com", first[i % 26], last, i % 50).to_lowercase();
            format!("{name} <{address}>")
        };
        let mut seed: u64 = 1;
        let mut pick = |n: usize| {
            seed = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (seed >> 33) as usize % n
        };
        let body = "Lorem ipsum dolor sit amet. ".repeat(23);
        let tx = store.conn.transaction().unwrap();
        for i in 0..50_000 {
            let (from_name, from_address) = if i % 5 == 0 {
                ("Me".to_owned(), "me@gmail.com".to_owned())
            } else {
                let p = parse(&person(pick(5_000)));
                (p.name, p.address)
            };
            tx.execute(
                "INSERT INTO messages (account_id, from_name, from_address, to_list, cc_list,
                    subject, snippet, date, text_body, attachments, category, state, unread,
                    pinned, remote_place, remote_unread, remote_pinned)
                 VALUES (1, ?1, ?2, ?3, ?4, 'Hello', '', ?5, ?6, '[]', 'people', 'inbox', 0,
                    0, 'inbox', 0, 0)",
                params![
                    from_name,
                    from_address,
                    serde_json::to_string(&[person(pick(5_000))]).unwrap(),
                    serde_json::to_string(&[person(pick(5_000))]).unwrap(),
                    NOW - pick((WEEKS * WEEK_MS / 1000) as usize) as i64 * 1000,
                    body,
                ],
            )
            .unwrap();
        }
        tx.commit().unwrap();

        let reading = median(
            (0..3)
                .map(|_| {
                    let start = Instant::now();
                    read_from_mail(&store, "ana", NOW, 20);
                    start.elapsed()
                })
                .collect(),
        );
        let finding = median(
            (0..3)
                .map(|_| {
                    *store.people.borrow_mut() = None;
                    let start = Instant::now();
                    store.suggest_contacts("ana", NOW, 20).unwrap();
                    start.elapsed()
                })
                .collect(),
        );
        let (people, entries) = {
            let kept = store.people.borrow();
            let kept = kept.as_ref().unwrap();
            (
                kept.people.len(),
                kept.people.values().map(Vec::len).sum::<usize>(),
            )
        };
        println!("{people} people written {entries} ways");
        println!("before, reading the mail per query: {reading:?}");
        println!("finding the people (and one query):  {finding:?}");
        for query in ["a", "an", "ana", "ana b", "zzz"] {
            let kept = median(
                (0..21)
                    .map(|_| {
                        let start = Instant::now();
                        store.suggest_contacts(query, NOW, 20).unwrap();
                        start.elapsed()
                    })
                    .collect(),
            );
            let found = store.suggest_contacts(query, NOW, 1_000_000).unwrap().len();
            println!("query {query:?} from the people kept: {kept:?} ({found} match)");
        }
        assert_eq!(
            store.suggest_contacts("ana", NOW, 20).unwrap(),
            read_from_mail(&store, "ana", NOW, 20)
        );
    }
}
