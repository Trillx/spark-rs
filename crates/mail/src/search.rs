//! A search put to the servers, for mail it wants that isn't stored yet, as
//! Spark does with every search. Each server gets the query in its own
//! syntax; what they find is stored like synced mail, so the search index
//! has it from then on and the search, run again over the store, shows it.
//!
//! Servers only narrow things down. The store's search decides what shows,
//! so a server query may be looser than the user's: words without their
//! order, or without a filter the server can't express.

use spark_store::{Query, Term};

/// The newest hits stored from each place a server searches.
pub const SERVER_HITS: usize = 100;

/// Whether servers can narrow the search down: it has words or a sender.
/// Filters alone (`is:unread`) would bring in mail at random.
pub fn worth_asking(query: &Query) -> bool {
    !query.terms().is_empty() || query.from.is_some()
}

/// Whether the search needs strings IMAP can only send as literals.
pub fn has_non_ascii(query: &Query) -> bool {
    !query.text.is_ascii() || query.from.as_deref().is_some_and(|f| !f.is_ascii())
}

/// `UID SEARCH` keys for Gmail: the search in Gmail's syntax, every term
/// quoted so operators in it are words, and no drafts. With `literal`, the
/// server takes non-synchronizing literals; without, a non-ASCII search
/// can't be sent. `None` if nothing is left to send.
pub fn gmail_keys(query: &Query, literal: bool) -> Option<String> {
    let mut parts: Vec<String> = query
        .terms()
        .iter()
        .map(|t| format!("\"{}\"", text_of(t).replace(['"', '\\'], " ")))
        .collect();
    if let Some(from) = query.from.as_deref().filter(|f| plain_address(f)) {
        parts.push(format!("from:{from}"));
    }
    if parts.is_empty() {
        return None;
    }
    if query.has_attachment {
        parts.push("has:attachment".into());
    }
    if query.unread_only {
        parts.push("is:unread".into());
    }
    parts.push("-in:drafts".into());
    let raw = parts.join(" ");
    let charset = if raw.is_ascii() { "" } else { "CHARSET UTF-8 " };
    Some(format!("{charset}X-GM-RAW {}", string(&raw, literal)?))
}

/// `UID SEARCH` keys for other servers: `TEXT` per term (servers match it
/// anywhere in the message), the sender and unread mail. Terms that can't
/// be sent (see `gmail_keys`) are left out. `None` if nothing is left.
pub fn imap_keys(query: &Query, literal: bool) -> Option<String> {
    let mut keys: Vec<String> = query
        .terms()
        .iter()
        .filter_map(|t| Some(format!("TEXT {}", string(text_of(t), literal)?)))
        .collect();
    if let Some(from) = query.from.as_deref()
        && let Some(from) = string(from, literal)
    {
        keys.push(format!("FROM {from}"));
    }
    if keys.is_empty() {
        return None;
    }
    if query.unread_only {
        keys.push("UNSEEN".into());
    }
    let keys = keys.join(" ");
    Some(if keys.is_ascii() {
        keys
    } else {
        format!("CHARSET UTF-8 {keys}")
    })
}

/// Microsoft's KQL for `$search`: the terms' words, lowercase so none is an
/// operator, all required, then the sender and attachments. It goes inside
/// a quoted string, so it has no quotes or backslashes. `None` if nothing
/// is left.
pub fn kql(query: &Query) -> Option<String> {
    let mut parts: Vec<String> = query
        .terms()
        .iter()
        .flat_map(|t| {
            text_of(t)
                .split(|c: char| !c.is_alphanumeric())
                .filter(|w| !w.is_empty())
                .map(str::to_lowercase)
                .collect::<Vec<_>>()
        })
        .collect();
    if let Some(from) = query.from.as_deref().filter(|f| plain_address(f)) {
        parts.push(format!("from:{from}"));
    }
    if parts.is_empty() {
        return None;
    }
    if query.has_attachment {
        parts.push("hasAttachments:true".into());
    }
    Some(parts.join(" AND "))
}

fn text_of(term: &Term) -> &str {
    match term {
        Term::Word(text) | Term::Phrase(text) => text,
    }
}

/// A name or address made only of what every server's syntax takes as is.
fn plain_address(text: &str) -> bool {
    text.chars()
        .all(|c| c.is_alphanumeric() || "@._-+".contains(c))
        && text.chars().any(char::is_alphanumeric)
}

/// An IMAP string: quoted when it's ASCII, else a non-synchronizing literal
/// if the server takes them (`LITERAL+` or `LITERAL-`, up to 4 KiB).
fn string(text: &str, literal: bool) -> Option<String> {
    let text: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    if text.is_ascii() {
        Some(crate::imap::quoted(&text))
    } else if literal && text.len() <= 4096 {
        Some(format!("{{{}+}}\r\n{text}", text.len()))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gmail_gets_its_own_syntax_with_every_term_quoted() {
        let query = Query::parse("Invoice \"price list\" OR from:ana@parts.com has:attachment");
        assert_eq!(
            gmail_keys(&query, false).unwrap(),
            r#"X-GM-RAW "\"Invoice\" \"price list\" \"OR\" from:ana@parts.com has:attachment -in:drafts""#
        );
        assert_eq!(
            gmail_keys(&Query::parse("café is:unread"), true).unwrap(),
            "CHARSET UTF-8 X-GM-RAW {28+}\r\n\"café\" is:unread -in:drafts"
        );
        assert_eq!(gmail_keys(&Query::parse("café"), false), None);
        assert_eq!(gmail_keys(&Query::parse("is:unread"), true), None);
        assert_eq!(gmail_keys(&Query::parse("from:\"ana\""), true), None);
    }

    #[test]
    fn other_imap_servers_get_a_text_key_per_term() {
        let query = Query::parse("invoice \"price list\" from:ana is:unread has:attachment");
        assert_eq!(
            imap_keys(&query, false).unwrap(),
            r#"TEXT "invoice" TEXT "price list" FROM "ana" UNSEEN"#
        );
        // What can't be sent is left out, and with it nothing at all.
        assert_eq!(
            imap_keys(&Query::parse("café menu"), false).unwrap(),
            r#"TEXT "menu""#
        );
        assert_eq!(
            imap_keys(&Query::parse("café"), true).unwrap(),
            "CHARSET UTF-8 TEXT {5+}\r\ncafé"
        );
        assert_eq!(imap_keys(&Query::parse("café"), false), None);
        assert_eq!(
            imap_keys(&Query::parse(r"C:\temp"), false).unwrap(),
            r#"TEXT "C:\\temp""#
        );
    }

    #[test]
    fn microsoft_gets_lowercase_words_joined_by_and() {
        let query = Query::parse("Q4 \"price-list\" OR NEAR from:ana@parts.com has:attachment");
        assert_eq!(
            kql(&query).unwrap(),
            "q4 AND price AND list AND or AND near AND from:ana@parts.com AND hasAttachments:true"
        );
        assert_eq!(kql(&Query::parse("Café")).unwrap(), "café");
        assert_eq!(kql(&Query::parse("has:attachment is:unread")), None);
        assert_eq!(kql(&Query::parse("from:a(b)")), None);
    }

    #[test]
    fn only_words_or_a_sender_are_worth_asking() {
        assert!(worth_asking(&Query::parse("invoice")));
        assert!(worth_asking(&Query::parse("from:ana is:unread")));
        assert!(!worth_asking(&Query::parse("is:unread has:attachment")));
        assert!(!worth_asking(&Query::parse("- * has:attachment")));
    }
}
