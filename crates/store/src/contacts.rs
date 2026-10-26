//! Who to suggest as a recipient while the user types an address: the
//! people in their stored mail.

use std::collections::{HashMap, HashSet};

use serde::Serialize;

use rusqlite::params;

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
        // Each person as stored on mail, with their score from it and when
        // it was last. Mail from the user's own addresses is mail they sent.
        let mut stmt = self.conn.prepare_cached(
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
        )?;
        let own: Vec<String> = self
            .accounts()?
            .into_iter()
            .map(|a| a.email.to_lowercase())
            .collect();
        let turned_down: HashSet<String> = self
            .conn
            .prepare_cached("SELECT lower(address) FROM contact_exceptions")?
            .query_map([], |r| r.get(0))?
            .collect::<Result<_>>()?;
        let mut people: HashMap<String, Person> = HashMap::new();
        let rows = stmt.query_map(params![now, WEEKS, now - WEEKS * WEEK_MS, WEEK_MS], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, bool>(3)?,
            ))
        })?;
        for row in rows {
            let (entry, score, last, sent) = row?;
            let contact = parse(&entry);
            let key = contact.address.to_lowercase();
            // The user is on mail to them; they're only suggested once they write to themselves.
            if key.is_empty()
                || (!sent && own.contains(&key))
                || turned_down.contains(&key)
                || unanswered(&key)
            {
                continue;
            }
            let person = people.entry(key).or_insert(Person {
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
            // The name they had on the latest mail that gave one.
            if !contact.name.is_empty() && last >= person.named_at {
                person.contact.name = contact.name;
                person.named_at = last;
            }
        }
        let mut people: Vec<Person> = people.into_values().filter(|p| p.matched).collect();
        people.sort_by(|a, b| {
            b.score
                .cmp(&a.score)
                .then(b.last.cmp(&a.last))
                .then_with(|| a.contact.address.cmp(&b.contact.address))
        });
        Ok(people.into_iter().take(limit).map(|p| p.contact).collect())
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
}

struct Person {
    contact: Contact,
    score: i64,
    /// When the latest mail with them was.
    last: i64,
    named_at: i64,
    matched: bool,
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
fn parse(entry: &str) -> Contact {
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

/// Whether every word starts a word of the name. One word may instead be
/// part of the address, though not only of its ending (".com").
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
    use crate::{AccountKind, Incoming, NewAccount};

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

    fn mail(store: &mut Store, from: &str, to: &[&str], days_ago: i64) {
        mail_cc(store, from, to, &[], days_ago);
    }

    fn mail_cc(store: &mut Store, from: &str, to: &[&str], cc: &[&str], days_ago: i64) {
        let uid = store
            .conn
            .query_row("SELECT COUNT(*) FROM messages", [], |r| r.get::<_, u32>(0))
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
            subject: format!("Message {uid}"),
            date: NOW - days_ago * DAY,
            ..Default::default()
        };
        store.add_incoming(&m, false).unwrap();
    }

    fn suggested(store: &Store, query: &str) -> Vec<String> {
        store
            .suggest_contacts(query, NOW, 10)
            .unwrap()
            .into_iter()
            .map(|c| {
                if c.name.is_empty() {
                    c.address
                } else {
                    format!("{} <{}>", c.name, c.address)
                }
            })
            .collect()
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
}
