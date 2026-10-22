//! Turning raw messages into what the store keeps, and Spark's Smart Inbox
//! categories (people, notifications, newsletters) from headers alone.

use mail_parser::{Address, HeaderValue, MessageParser};
use spark_store::{Category, Incoming};

use crate::attachment;

pub use mail_parser::decoders::html::html_to_text;

/// Parses an RFC 5322 message. Location, flags and the account are left for
/// the caller; `date` falls back to the Date header.
pub fn parse_message(raw: &[u8]) -> Option<Incoming> {
    let m = MessageParser::default().parse(raw)?;
    let (from_name, from_address) = m
        .from()
        .and_then(Address::first)
        .map(|a| {
            let address = a.address().unwrap_or_default().to_owned();
            let name = a.name().map(str::trim).filter(|n| !n.is_empty());
            (name.unwrap_or(&address).to_owned(), address)
        })
        .unwrap_or_default();

    let text = m.body_text(0).map(|t| t.into_owned()).unwrap_or_default();
    // body_html also offers text parts converted to HTML; keep real HTML only.
    let html = m
        .html_bodies()
        .any(|part| part.is_text_html())
        .then(|| m.body_html(0).map(|h| h.into_owned()))
        .flatten();
    let attachments = attachment::describe(&m);

    let signals = Signals {
        list: m.header("List-Id").is_some() || m.header("List-Unsubscribe").is_some(),
        bulk: m.header_raw("Precedence").is_some_and(|p| {
            matches!(
                p.trim().to_ascii_lowercase().as_str(),
                "bulk" | "list" | "junk"
            )
        }),
        automated: m
            .header_raw("Auto-Submitted")
            .is_some_and(|v| !v.trim().eq_ignore_ascii_case("no")),
    };

    Some(Incoming {
        message_id: m.message_id().map(bare_id),
        in_reply_to: ids(m.in_reply_to()).into_iter().next(),
        references: Some(ids(m.references()).join(" ")).filter(|r| !r.is_empty()),
        category: categorize(&from_address, signals),
        from_name,
        from_address,
        to: display_list(m.to()),
        cc: display_list(m.cc()),
        subject: m.subject().unwrap_or_default().trim().to_owned(),
        date: m.date().map_or(0, |d| d.to_timestamp() * 1000),
        snippet: snippet(&text),
        text,
        html,
        attachments,
        ..Default::default()
    })
}

/// Header hints that a message was sent by a machine or to a list.
#[derive(Clone, Copy, Debug, Default)]
pub struct Signals {
    /// List-Id or List-Unsubscribe.
    pub list: bool,
    /// Precedence: bulk, list or junk.
    pub bulk: bool,
    /// Auto-Submitted other than "no".
    pub automated: bool,
}

/// Automated senders are notifications even when they carry list headers
/// (GitHub, receipts); other list mail is a newsletter; the rest is people.
pub fn categorize(from_address: &str, signals: Signals) -> Category {
    if signals.automated || automated_sender(from_address) {
        Category::Notifications
    } else if signals.list || signals.bulk {
        Category::Newsletters
    } else {
        Category::People
    }
}

/// Mailboxes that machines send from, by their local part.
pub fn automated_sender(address: &str) -> bool {
    let local = address
        .split('@')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let local = local.split('+').next().unwrap_or_default();
    const EXACT: &[&str] = &[
        "alert",
        "alerts",
        "auto",
        "billing",
        "bounce",
        "bounces",
        "calendar",
        "donotreply",
        "do-not-reply",
        "do_not_reply",
        "invoice",
        "invoices",
        "mailer-daemon",
        "noreply",
        "no-reply",
        "no_reply",
        "notification",
        "notifications",
        "notify",
        "order",
        "orders",
        "postmaster",
        "receipt",
        "receipts",
        "security",
        "shipping",
        "system",
        "updates",
        "verify",
    ];
    EXACT.contains(&local)
        || local.starts_with("noreply")
        || local.starts_with("no-reply")
        || local.starts_with("notifications")
}

/// The first readable text, without quoted replies, on one line.
pub fn snippet(text: &str) -> String {
    let mut out = String::new();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('>') {
            continue;
        }
        // "On Tue, Oct 21, 2026, Ana wrote:" starts the quote.
        if line.ends_with("wrote:") && (line.starts_with("On ") || line.contains(" wrote:")) {
            break;
        }
        for word in line.split_whitespace() {
            if !out.is_empty() {
                out.push(' ');
            }
            out.push_str(word);
        }
        if out.chars().count() >= 200 {
            break;
        }
    }
    match out.char_indices().nth(200) {
        Some((cut, _)) => format!("{}…", out[..cut].trim_end()),
        None => out,
    }
}

/// Message-IDs are stored without angle brackets.
pub fn bare_id(id: &str) -> String {
    id.trim()
        .trim_start_matches('<')
        .trim_end_matches('>')
        .to_owned()
}

fn ids(value: &HeaderValue) -> Vec<String> {
    match value {
        HeaderValue::Text(id) => vec![bare_id(id)],
        HeaderValue::TextList(list) => list.iter().map(|id| bare_id(id)).collect(),
        _ => Vec::new(),
    }
}

fn display_list(address: Option<&Address>) -> Vec<String> {
    let Some(address) = address else {
        return Vec::new();
    };
    address
        .iter()
        .filter_map(|a| {
            let email = a.address()?;
            Some(match a.name().map(str::trim).filter(|n| !n.is_empty()) {
                Some(name) => format!("{name} <{email}>"),
                None => email.to_owned(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const NEWSLETTER: &str = "From: \"The Verge\" <newsletter@theverge.com>\r\n\
To: Me <me@gmail.com>, other@x.com\r\n\
Subject: =?UTF-8?Q?This_week_=E2=80=94_phones?=\r\n\
Date: Tue, 21 Oct 2026 14:00:00 +0000\r\n\
Message-ID: <abc@theverge.com>\r\n\
List-Unsubscribe: <mailto:unsub@theverge.com>\r\n\
MIME-Version: 1.0\r\n\
Content-Type: multipart/alternative; boundary=b\r\n\
\r\n\
--b\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
\r\n\
Hello   readers,\r\n\
this week: phones.\r\n\
--b\r\n\
Content-Type: text/html; charset=utf-8\r\n\
\r\n\
<p>Hello <b>readers</b></p>\r\n\
--b--\r\n";

    #[test]
    fn parses_headers_bodies_and_category() {
        let m = parse_message(NEWSLETTER.as_bytes()).unwrap();
        assert_eq!(m.from_name, "The Verge");
        assert_eq!(m.from_address, "newsletter@theverge.com");
        assert_eq!(m.to, vec!["Me <me@gmail.com>", "other@x.com"]);
        assert_eq!(m.subject, "This week — phones");
        assert_eq!(m.message_id.as_deref(), Some("abc@theverge.com"));
        assert_eq!(m.date, 1_792_591_200_000);
        assert_eq!(m.snippet, "Hello readers, this week: phones.");
        assert!(m.html.as_deref().unwrap().contains("<b>readers</b>"));
        assert_eq!(m.category, Category::Newsletters);
    }

    #[test]
    fn replies_keep_threading_headers_and_attachment_names() {
        let raw = "From: ana@x.com\r\n\
Subject: Re: Plan\r\n\
In-Reply-To: <1@x.com>\r\n\
References: <0@x.com> <1@x.com>\r\n\
Content-Type: multipart/mixed; boundary=m\r\n\
\r\n\
--m\r\n\
Content-Type: text/plain\r\n\
\r\n\
Sounds good.\r\n\
\r\n\
On Mon, Oct 20, 2026 at 9:00 AM Me <me@gmail.com> wrote:\r\n\
> Shall we?\r\n\
--m\r\n\
Content-Type: application/pdf; name=\"plan.pdf\"\r\n\
Content-Disposition: attachment; filename=\"plan.pdf\"\r\n\
Content-Transfer-Encoding: base64\r\n\
\r\n\
JVBERi0=\r\n\
--m--\r\n";
        let m = parse_message(raw.as_bytes()).unwrap();
        assert_eq!(
            m.from_name, "ana@x.com",
            "no display name: the address stands in"
        );
        assert_eq!(m.in_reply_to.as_deref(), Some("1@x.com"));
        assert_eq!(m.references.as_deref(), Some("0@x.com 1@x.com"));
        assert_eq!(m.attachments.len(), 1);
        assert_eq!(m.attachments[0].name, "plan.pdf");
        assert_eq!(m.attachments[0].mime, "application/pdf");
        assert_eq!(m.attachments[0].size, 5);
        assert_eq!(m.snippet, "Sounds good.");
        assert_eq!(m.html, None);
        assert_eq!(m.category, Category::People);
    }

    #[test]
    fn html_only_mail_gets_a_text_version() {
        let raw = "From: Shop <orders@shop.com>\r\n\
Subject: Your order\r\n\
Content-Type: text/html\r\n\
\r\n\
<html><body><h1>Thanks!</h1><p>Order&nbsp;#42 ships today.</p></body></html>\r\n";
        let m = parse_message(raw.as_bytes()).unwrap();
        assert!(m.text.contains("Order"), "{:?}", m.text);
        assert!(m.snippet.starts_with("Thanks!"), "{:?}", m.snippet);
        assert_eq!(m.category, Category::Notifications);
    }

    #[test]
    fn categories_follow_sender_and_headers() {
        let none = Signals::default();
        let list = Signals { list: true, ..none };
        assert_eq!(categorize("ana@x.com", none), Category::People);
        assert_eq!(categorize("news@x.com", list), Category::Newsletters);
        assert_eq!(
            categorize("notifications@github.com", list),
            Category::Notifications
        );
        assert_eq!(
            categorize("no-reply+abc@accounts.google.com", none),
            Category::Notifications
        );
        assert_eq!(
            categorize(
                "ana@x.com",
                Signals {
                    automated: true,
                    ..none
                }
            ),
            Category::Notifications
        );
    }

    #[test]
    fn snippets_are_short_and_skip_quotes() {
        let long = "word ".repeat(100);
        let s = snippet(&long);
        assert!(s.ends_with('…'));
        assert!(s.chars().count() <= 201);
        assert_eq!(snippet("> quoted\nReal text"), "Real text");
    }
}
