//! The Gatekeeper, Spark's screening of new senders. The user accepts or
//! blocks a sender, or a whole domain, once for every account; mail follows
//! the decision, stored mail included. Some senders need no decision: the
//! user's colleagues, people they've written to, and a few big companies.
//! The mode decides whether new senders are screened at all.

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::{Incoming, MessageState, Result, Store, refused};

/// A decision about a sender or a domain.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Decision {
    Accepted,
    Blocked,
}

impl Decision {
    fn as_str(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Blocked => "blocked",
        }
    }

    fn parse(s: &str) -> Self {
        if s == "blocked" {
            Self::Blocked
        } else {
            Self::Accepted
        }
    }
}

/// Who a decision is about: one address, or everyone at a domain.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Screen {
    Sender(String),
    Domain(String),
}

/// A decision to make, or (`None`) to forget, which makes the sender new
/// again. `Store::decide` returns the ones that undo it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Change {
    pub screen: Screen,
    pub decision: Option<Decision>,
}

/// The decisions that apply to an address: its own, and its domain's. Its
/// own wins. A free mail provider's domain can't be decided.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SenderStatus {
    pub sender: Option<Decision>,
    pub domain: Option<Decision>,
    pub public_domain: bool,
}

/// Where new senders wait, as in Spark's Settings → Gatekeeper.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum GatekeeperMode {
    /// In New Senders on top of the Inbox, out of the list until accepted.
    #[default]
    BeforeInbox,
    /// In the Inbox; the email asks to accept or block when opened.
    InsideInbox,
    /// Everyone is accepted without asking.
    Off,
}

impl GatekeeperMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::BeforeInbox => "beforeInbox",
            Self::InsideInbox => "insideInbox",
            Self::Off => "off",
        }
    }

    fn parse(s: &str) -> Self {
        match s {
            "insideInbox" => Self::InsideInbox,
            "off" => Self::Off,
            _ => Self::BeforeInbox,
        }
    }
}

/// Version 14: decisions for every account rather than per account, per
/// domain too, and app settings kept with the mail (the Gatekeeper mode).
pub(crate) const SCHEMA_GATEKEEPER: &str = "
CREATE TABLE gatekeeper_senders (
    address TEXT PRIMARY KEY COLLATE NOCASE,
    status TEXT NOT NULL
);
CREATE TABLE gatekeeper_domains (
    domain TEXT PRIMARY KEY COLLATE NOCASE,
    status TEXT NOT NULL
);
CREATE TABLE settings (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

-- A sender blocked in one account is blocked in all ('blocked' > 'accepted').
INSERT INTO gatekeeper_senders (address, status)
    SELECT address, MAX(status) FROM senders GROUP BY address;
DROP TABLE senders;
";

/// After `SCHEMA_GATEKEEPER`: stored mail follows the decisions, now that
/// they cover every account, and the senders the rules now accept leave
/// New Senders.
pub(crate) fn migrate(conn: &Connection, now: i64) -> Result<()> {
    let addresses: Vec<String> = {
        let mut stmt = conn.prepare(
            "SELECT DISTINCT from_address FROM messages
             WHERE screening = 1 OR state = 'blocked'
                OR from_address IN (SELECT address FROM gatekeeper_senders WHERE status = 'blocked')",
        )?;
        stmt.query_map([], |r| r.get(0))?.collect::<Result<_>>()?
    };
    for address in addresses {
        resettle(conn, &address, now)?;
    }
    Ok(())
}

/// Free mail providers: sharing one with the user says nothing about a
/// sender, and a decision about one would be about strangers.
const PUBLIC_DOMAINS: &[&str] = &[
    "gmail.com",
    "googlemail.com",
    "outlook.com",
    "hotmail.com",
    "live.com",
    "msn.com",
    "yahoo.com",
    "ymail.com",
    "rocketmail.com",
    "icloud.com",
    "me.com",
    "mac.com",
    "aol.com",
    "proton.me",
    "protonmail.com",
    "pm.me",
    "gmx.com",
    "gmx.net",
    "gmx.de",
    "web.de",
    "mail.com",
    "zoho.com",
    "fastmail.com",
    "hey.com",
    "tutanota.com",
    "tuta.io",
    "yandex.com",
    "yandex.ru",
    "mail.ru",
    "qq.com",
    "163.com",
    "126.com",
    "comcast.net",
    "verizon.net",
    "att.net",
    "sbcglobal.net",
    "btinternet.com",
    "orange.fr",
    "free.fr",
    "t-online.de",
    "libero.it",
    "seznam.cz",
    "naver.com",
];

/// Country versions of the big ones (hotmail.co.uk, yahoo.fr, …), followed
/// by a country's domain (`country_domain`).
const PUBLIC_PREFIXES: &[&str] = &["hotmail.", "outlook.", "live.", "yahoo.", "gmx.", "yandex."];

/// Senders Spark accepts without asking: big companies' notifications.
/// Spark's own (readdle.com, sparkmailapp.com) are left out.
const WELL_KNOWN: &[&str] = &[
    "google.com",
    "facebookmail.com",
    "twitter.com",
    "linkedin.com",
    "apple.com",
    "atlassian.com",
    "microsoft.com",
    "github.com",
];

/// …and their subdomains, where Spark takes those too.
const WELL_KNOWN_PARENTS: &[&str] = &["google.com", "atlassian.com", "insideicloud.icloud.com"];

/// The domain of an address, lowercased; `None` without an `@`.
pub fn domain_of(address: &str) -> Option<String> {
    address
        .rsplit_once('@')
        .map(|(_, domain)| domain.trim().to_lowercase())
        .filter(|d| !d.is_empty())
}

/// A free mail provider's domain (see `PUBLIC_DOMAINS`).
pub fn is_public_domain(domain: &str) -> bool {
    let domain = domain.to_lowercase();
    PUBLIC_DOMAINS.contains(&domain.as_str())
        || PUBLIC_PREFIXES
            .iter()
            .any(|p| domain.strip_prefix(p).is_some_and(country_domain))
}

/// A country's domain, as the providers' country versions end: `fr`,
/// `co.uk`, `com.br`. Not an organisation's (`example.org` after `live.`).
fn country_domain(rest: &str) -> bool {
    let code = |s: &str| s.len() == 2 && s.bytes().all(|b| b.is_ascii_lowercase());
    match rest.split_once('.') {
        None => code(rest),
        Some((second, country)) => matches!(second, "co" | "com") && code(country),
    }
}

fn is_well_known(domain: &str) -> bool {
    WELL_KNOWN.contains(&domain)
        || WELL_KNOWN_PARENTS.iter().any(|parent| {
            domain
                .strip_suffix(parent)
                .is_some_and(|sub| sub.ends_with('.'))
        })
}

impl Store {
    pub fn gatekeeper_mode(&self) -> Result<GatekeeperMode> {
        mode(&self.conn)
    }

    /// Changes the mode. Turning the Gatekeeper off accepts everyone waiting
    /// in it; turning it back on screens only senders who are new from then.
    pub fn set_gatekeeper_mode(&mut self, mode: GatekeeperMode) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO settings (key, value) VALUES ('gatekeeperMode', ?1)
             ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            [mode.as_str()],
        )?;
        if mode == GatekeeperMode::Off {
            // Not those whose domain was decided: that decision stands.
            tx.execute(
                "INSERT OR IGNORE INTO gatekeeper_senders (address, status)
                 SELECT DISTINCT from_address, 'accepted' FROM messages
                 WHERE screening = 1 AND from_address != ''
                   AND NOT EXISTS (SELECT 1 FROM gatekeeper_domains d
                                   WHERE d.domain = substr(from_address, instr(from_address, '@') + 1))",
                [],
            )?;
            tx.execute("UPDATE messages SET screening = 0 WHERE screening = 1", [])?;
        }
        tx.commit()
    }

    pub fn sender_status(&self, address: &str) -> Result<SenderStatus> {
        status(&self.conn, address)
    }

    /// Makes the decisions, and moves stored mail to match: a blocked
    /// sender's inbox mail to the Blocked folder, an accepted one's out of
    /// New Senders and out of Blocked, a new one's back into New Senders.
    /// Returns the changes that undo it. A domain anyone can sign up at
    /// can't be decided as a whole.
    pub fn decide(&mut self, changes: &[Change], now: i64) -> Result<Vec<Change>> {
        let tx = self.conn.transaction()?;
        let mut undo = Vec::new();
        for change in changes {
            let (table, column, key) = match &change.screen {
                Screen::Sender(address) => {
                    if domain_of(address).is_none() {
                        return Err(refused("That isn't an email address."));
                    }
                    ("gatekeeper_senders", "address", address.trim().to_owned())
                }
                Screen::Domain(domain) => {
                    let domain = domain.trim().trim_start_matches('@').to_lowercase();
                    if domain.is_empty() {
                        return Err(refused("That isn't a domain."));
                    }
                    if is_public_domain(&domain) {
                        return Err(refused(&format!(
                            "Anyone can have an address at {domain}, so its senders can only be accepted or blocked one by one."
                        )));
                    }
                    ("gatekeeper_domains", "domain", domain)
                }
            };
            let previous: Option<String> = tx
                .query_row(
                    &format!("SELECT status FROM {table} WHERE {column} = ?1"),
                    [&key],
                    |r| r.get(0),
                )
                .optional()?;
            undo.push(Change {
                screen: change.screen.clone(),
                decision: previous.as_deref().map(Decision::parse),
            });
            match change.decision {
                Some(decision) => tx.execute(
                    &format!(
                        "INSERT INTO {table} ({column}, status) VALUES (?1, ?2)
                         ON CONFLICT ({column}) DO UPDATE SET status = excluded.status"
                    ),
                    params![key, decision.as_str()],
                )?,
                None => tx.execute(&format!("DELETE FROM {table} WHERE {column} = ?1"), [&key])?,
            };
            let addresses: Vec<String> = match &change.screen {
                Screen::Sender(_) => vec![key],
                Screen::Domain(_) => {
                    let mut stmt = tx.prepare(
                        "SELECT DISTINCT from_address FROM messages
                         WHERE substr(from_address, instr(from_address, '@') + 1) = ?1 COLLATE NOCASE",
                    )?;
                    stmt.query_map([&key], |r| r.get(0))?
                        .collect::<Result<_>>()?
                }
            };
            for address in addresses {
                resettle(&tx, &address, now)?;
            }
        }
        tx.commit()?;
        undo.reverse();
        Ok(undo)
    }

    /// Accepts someone without asking, e.g. someone the user wrote to,
    /// unless the user decided about them, or their domain, already (as in
    /// Spark, which skips senders with a status). Returns whether it did.
    pub fn accept_sender(&self, address: &str, now: i64) -> Result<bool> {
        if decided(&self.conn, address)?.is_some() {
            return Ok(false);
        }
        self.conn.execute(
            "INSERT INTO gatekeeper_senders (address, status) VALUES (?1, 'accepted')",
            [address],
        )?;
        resettle(&self.conn, address, now)?;
        Ok(true)
    }

    /// Senders waiting in the Gatekeeper, in any account, with no decision
    /// yet; the latest to write first.
    pub fn screened_senders(&self) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT from_address FROM messages WHERE screening = 1 AND state = 'inbox'
             GROUP BY from_address ORDER BY MAX(date) DESC",
        )?;
        stmt.query_map([], |r| r.get(0))?.collect()
    }
}

/// SQL: whether the decision about a `messages` row's sender (theirs, else
/// their domain's) is to block them.
pub(crate) const SENDER_BLOCKED: &str = "COALESCE(
    (SELECT status FROM gatekeeper_senders g WHERE g.address = messages.from_address),
    (SELECT status FROM gatekeeper_domains d
     WHERE d.domain = substr(messages.from_address, instr(messages.from_address, '@') + 1))
) = 'blocked'";

fn mode(conn: &Connection) -> Result<GatekeeperMode> {
    let value: Option<String> = conn
        .query_row(
            "SELECT value FROM settings WHERE key = 'gatekeeperMode'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    Ok(value
        .as_deref()
        .map(GatekeeperMode::parse)
        .unwrap_or_default())
}

fn status(conn: &Connection, address: &str) -> Result<SenderStatus> {
    let sender: Option<String> = conn
        .query_row(
            "SELECT status FROM gatekeeper_senders WHERE address = ?1",
            [address],
            |r| r.get(0),
        )
        .optional()?;
    let domain: Option<String> = match domain_of(address) {
        Some(domain) => conn
            .query_row(
                "SELECT status FROM gatekeeper_domains WHERE domain = ?1",
                [domain],
                |r| r.get(0),
            )
            .optional()?,
        None => None,
    };
    Ok(SenderStatus {
        sender: sender.as_deref().map(Decision::parse),
        domain: domain.as_deref().map(Decision::parse),
        public_domain: domain_of(address).is_some_and(|d| is_public_domain(&d)),
    })
}

/// The decision that applies to an address: its own, else its domain's.
fn decided(conn: &Connection, address: &str) -> Result<Option<Decision>> {
    let status = status(conn, address)?;
    Ok(status.sender.or(status.domain))
}

/// Whether a sender is accepted without a decision: one of the user's own
/// addresses, a colleague (at the domain of one of the user's accounts, if
/// it isn't a free mail provider's), someone the user has written to, or a
/// well-known company.
fn welcome(conn: &Connection, address: &str) -> Result<bool> {
    let Some(domain) = domain_of(address) else {
        return Ok(true);
    };
    if is_well_known(&domain) {
        return Ok(true);
    }
    let own: Vec<String> = {
        let mut stmt = conn.prepare_cached("SELECT email FROM accounts")?;
        stmt.query_map([], |r| r.get(0))?.collect::<Result<_>>()?
    };
    if own.iter().any(|email| email.eq_ignore_ascii_case(address)) {
        return Ok(true);
    }
    if !is_public_domain(&domain)
        && own
            .iter()
            .any(|email| domain_of(email).as_deref() == Some(&domain))
    {
        return Ok(true);
    }
    wrote_to(conn, address)
}

/// Whether the user has written to `address`: it's on mail of theirs in a
/// Sent folder, or on mail sent from here. Other mail claiming to be from
/// them proves nothing (anyone can write that From), and drafts weren't sent.
/// A recipient is stored as `Name <address>` or the bare address; only that
/// address counts, not one written in the name.
fn wrote_to(conn: &Connection, address: &str) -> Result<bool> {
    let address = address.trim().to_lowercase();
    conn.prepare_cached(
        "SELECT EXISTS (SELECT 1 FROM sent_to WHERE lower(address) = ?1)
             OR EXISTS (
                 SELECT 1 FROM folders f
                 JOIN folder_members fm ON fm.folder_id = f.id
                 JOIN messages m ON m.id = fm.message_id
                 WHERE f.role = 'sent'
                   AND m.from_address IN (SELECT email FROM accounts)
                   AND EXISTS (
                       SELECT 1 FROM json_each(m.to_list) j
                       WHERE lower(j.value) = ?1 OR substr(lower(j.value), -length(?1) - 2) = '<' || ?1 || '>'
                       UNION ALL
                       SELECT 1 FROM json_each(m.cc_list) j
                       WHERE lower(j.value) = ?1 OR substr(lower(j.value), -length(?1) - 2) = '<' || ?1 || '>'
                       UNION ALL
                       SELECT 1 FROM json_each(m.bcc_list) j
                       WHERE lower(j.value) = ?1 OR substr(lower(j.value), -length(?1) - 2) = '<' || ?1 || '>'))",
    )?
    .query_row([address], |r| r.get(0))
}

/// The state and screening of mail arriving in the inbox, by its sender.
/// With `screen_new_senders` (after an account's first sync) and the
/// Gatekeeper on, a new sender waits in it; otherwise they're accepted.
pub(crate) fn arrival(
    conn: &Connection,
    m: &Incoming,
    screen_new_senders: bool,
) -> Result<(MessageState, bool)> {
    let address = m.from_address.as_str();
    if address.is_empty() {
        return Ok((MessageState::Inbox, false));
    }
    Ok(match decided(conn, address)? {
        Some(Decision::Blocked) => (MessageState::Blocked, false),
        Some(Decision::Accepted) => (MessageState::Inbox, false),
        None if welcome(conn, address)? => {
            // The rules may have changed since their other mail arrived.
            release(conn, address)?;
            (MessageState::Inbox, false)
        }
        None if screen_new_senders && mode(conn)? != GatekeeperMode::Off => {
            (MessageState::Inbox, true)
        }
        None => {
            conn.execute(
                "INSERT OR IGNORE INTO gatekeeper_senders (address, status) VALUES (?1, 'accepted')",
                [address],
            )?;
            // Accepted for every account, so their mail waiting in another
            // account's Gatekeeper comes out too.
            release(conn, address)?;
            (MessageState::Inbox, false)
        }
    })
}

/// Takes an accepted sender's mail out of every account's Gatekeeper. Only
/// mail waiting there changes: a decision would have moved it already.
/// (Through the sender index: a first sync gets here for most senders.)
fn release(conn: &Connection, address: &str) -> Result<()> {
    conn.prepare_cached(
        "UPDATE messages SET screening = 0
         WHERE account_id IN (SELECT id FROM accounts) AND from_address = ?1 AND screening = 1",
    )?
    .execute([address])?;
    Ok(())
}

/// A Sent folder sync found stored message `id`: if the user sent it, the
/// people it's to get through the Gatekeeper, as if the user had written to
/// them here. (Read from the store: a sync passes only the location of mail
/// it has already.)
pub(crate) fn sent(conn: &Connection, id: i64) -> Result<()> {
    let recipients: Option<(String, String, String)> = conn
        .query_row(
            "SELECT to_list, cc_list, bcc_list FROM messages
             WHERE id = ?1 AND from_address IN (SELECT email FROM accounts)",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let Some((to, cc, bcc)) = recipients else {
        return Ok(());
    };
    for list in [to, cc, bcc] {
        for entry in crate::from_json(&list) {
            release(conn, &crate::contacts::parse(&entry).address)?;
        }
    }
    Ok(())
}

/// Mail waiting in the Gatekeeper from a new account's own address, and
/// from its colleagues (at its domain, unless anyone can have an address
/// there), gets through, as in Spark.
pub(crate) fn welcome_colleagues(conn: &Connection, email: &str, now: i64) -> Result<()> {
    release(conn, email)?;
    let Some(domain) = domain_of(email).filter(|d| !is_public_domain(d)) else {
        return Ok(());
    };
    let waiting: Vec<String> = {
        let mut stmt = conn.prepare(
            "SELECT DISTINCT from_address FROM messages
             WHERE screening = 1 AND substr(from_address, instr(from_address, '@') + 1) = ?1 COLLATE NOCASE",
        )?;
        stmt.query_map([domain], |r| r.get(0))?
            .collect::<Result<_>>()?
    };
    for address in waiting {
        resettle(conn, &address, now)?;
    }
    Ok(())
}

/// Moves a sender's stored mail to match the decision about them: inbox
/// mail to Blocked, or Blocked mail back to the inbox, screened again if
/// the sender is new. Moves are pushed like the user's own.
fn resettle(conn: &Connection, address: &str, now: i64) -> Result<()> {
    let decision = decided(conn, address)?;
    let screened =
        decision.is_none() && mode(conn)? != GatekeeperMode::Off && !welcome(conn, address)?;
    // Pushed unless the server already has it there (an undo in time).
    let moved = |place: &str| {
        format!(
            "changed_at = CASE WHEN remote_place = '{place}' AND remote_unread = unread
                                    AND remote_pinned = pinned THEN 0 ELSE ?2 END"
        )
    };
    if decision == Some(Decision::Blocked) {
        conn.execute(
            &format!(
                "UPDATE messages SET state = 'blocked', screening = 0, {}
                 WHERE from_address = ?1 AND state = 'inbox'",
                moved("blocked")
            ),
            params![address, now],
        )?;
        return Ok(());
    }
    conn.execute(
        &format!(
            "UPDATE messages SET state = 'inbox', {}
             WHERE from_address = ?1 AND state = 'blocked'",
            moved("inbox")
        ),
        params![address, now],
    )?;
    if screened {
        // Their mail in the server's inbox, snoozed or set aside too, waits again.
        conn.execute(
            "UPDATE messages SET screening = 1
             WHERE from_address = ?1 AND state IN ('inbox', 'snoozed', 'aside')",
            [address],
        )?;
    } else {
        conn.execute(
            "UPDATE messages SET screening = 0 WHERE from_address = ?1 AND screening = 1",
            [address],
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domains_are_public_well_known_or_neither() {
        assert_eq!(domain_of("Ana@Parts.COM").as_deref(), Some("parts.com"));
        assert_eq!(domain_of("nobody"), None);
        assert!(is_public_domain("gmail.com") && is_public_domain("hotmail.co.uk"));
        assert!(is_public_domain("yahoo.com.br") && is_public_domain("gmx.de"));
        assert!(!is_public_domain("livenation.com") && !is_public_domain("contoso.com"));
        assert!(!is_public_domain("live.example.org") && !is_public_domain("outlook.company.com"));
        assert!(is_well_known("github.com") && is_well_known("accounts.google.com"));
        assert!(!is_well_known("notgoogle.com") && !is_well_known("evilgithub.com"));
    }
}
