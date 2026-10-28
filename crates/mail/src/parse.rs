//! Turning raw messages into what the store keeps, and Spark's Smart Inbox
//! categories (people, notifications, newsletters) from headers alone.

use mail_parser::{Address, HeaderValue, MessageParser, MimeHeaders};
use spark_store::{Body, Category, Incoming};

use crate::attachment;
use crate::invite::Invite;
use crate::unsubscribe;

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
        reply_to: Some(display_list(m.reply_to())),
        to: display_list(m.to()),
        cc: display_list(m.cc()),
        bcc: display_list(m.bcc()),
        subject: m.subject().unwrap_or_default().trim().to_owned(),
        date: m.date().map_or(0, |d| d.to_timestamp() * 1000),
        snippet: snippet(&text),
        text,
        html,
        attachments,
        invite: Invite::find(&m).map(|i| i.to_json()),
        unsubscribe: Some(
            m.header_raw("List-Unsubscribe")
                .is_some_and(unsubscribe::offered),
        ),
        body: Body::Whole,
        ..Default::default()
    })
}

/// Parses a message fetched without its body (`Body::Missing`):
/// `header`, its header or the fields of it a sync reads, and `text`, the
/// start of what follows. `cut`: the message goes on past `text`. Of the
/// text only the preview is kept, as its text too, for the search index.
/// Text that's cut is read leniently: its last line, which may be cut in
/// two, is left out, and the multiparts it leaves open are closed, so the
/// parts it has decode. Attachments and the invite are left for the
/// caller, which knows them from the message's structure.
pub fn parse_head(header: &[u8], text: &[u8], cut: bool) -> Option<Incoming> {
    let mut raw = header.to_vec();
    if !raw.ends_with(b"\n\n") && !raw.ends_with(b"\r\n\r\n") {
        raw.extend_from_slice(if raw.ends_with(b"\n") {
            b"\r\n"
        } else {
            b"\r\n\r\n"
        });
    }
    let text = match text.iter().rposition(|&b| b == b'\n') {
        Some(end) if cut => &text[..=end],
        None if cut => &[],
        _ => text,
    };
    raw.extend_from_slice(text);
    if cut && let Some(m) = MessageParser::default().parse(&raw) {
        let mut open: Vec<String> = m
            .parts
            .iter()
            .filter_map(|part| {
                let ct = part.content_type()?;
                ct.ctype()
                    .eq_ignore_ascii_case("multipart")
                    .then(|| ct.attribute("boundary"))
                    .flatten()
                    .map(str::to_owned)
            })
            .filter(|b| !contains(text, format!("--{b}--").as_bytes()))
            .collect();
        // Innermost first: they come after the ones they're in.
        open.reverse();
        for boundary in open {
            raw.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
        }
    }
    let mut m = parse_message(&raw)?;
    // A character cut in two at the end of a base64 line.
    let preview = m.snippet.trim_end_matches(['\u{fffd}', '…']).trim_end();
    m.snippet = if m.snippet.ends_with('…') {
        format!("{preview}…")
    } else {
        preview.to_owned()
    };
    m.text = m.snippet.clone();
    m.html = None;
    m.attachments = Vec::new();
    m.invite = None;
    m.body = Body::Missing;
    Some(m)
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
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

/// The Message-ID in a block of headers, without angle brackets.
pub fn message_id(headers: &[u8]) -> Option<String> {
    MessageParser::default()
        .parse_headers(headers)?
        .message_id()
        .map(bare_id)
}

/// Reply-To addresses from a fetched block of headers. None means the
/// server did not provide parseable headers; an empty list means no Reply-To.
pub fn reply_to(headers: &[u8]) -> Option<Vec<String>> {
    let parsed = MessageParser::default().parse_headers(headers)?;
    Some(display_list(parsed.reply_to()))
}

/// The addresses a block of headers is to (To, Cc and Bcc), lowercased.
pub fn recipient_addresses(headers: &[u8]) -> Vec<String> {
    let Some(parsed) = MessageParser::default().parse_headers(headers) else {
        return Vec::new();
    };
    [parsed.to(), parsed.cc(), parsed.bcc()]
        .into_iter()
        .flatten()
        .flat_map(|list| {
            list.iter()
                .filter_map(|a| a.address().map(str::to_lowercase))
        })
        .collect()
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

    #[test]
    fn recipients_are_whole_addresses() {
        let headers = b"To: Jo Ann <JoAnn@Vendor.org>, bob@x.com\r\nCc: \"C\" <c@y.org>\r\n\r\n";
        assert_eq!(
            recipient_addresses(headers),
            ["joann@vendor.org", "bob@x.com", "c@y.org"]
        );
    }

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
        assert_eq!(m.unsubscribe, Some(true));
    }

    #[test]
    fn unsubscribing_is_known_from_the_header_alone() {
        // A receipt from a no-reply sender is a notification, but its list
        // can still be left.
        let receipt = "From: Shop <noreply@shop.com>\r\n\
Subject: Deals\r\n\
List-Unsubscribe: <https://shop.com/u/1>\r\n\
\r\n\
Hi\r\n";
        let m = parse_message(receipt.as_bytes()).unwrap();
        assert_eq!(m.category, Category::Notifications);
        assert_eq!(m.unsubscribe, Some(true));
        let unusable = "From: news@x.com\r\nList-Unsubscribe: <mailto:nobody>\r\n\r\nHi\r\n";
        assert_eq!(
            parse_message(unusable.as_bytes()).unwrap().unsubscribe,
            Some(false)
        );
        let plain = "From: ana@x.com\r\nSubject: Hi\r\n\r\nHi\r\n";
        assert_eq!(
            parse_message(plain.as_bytes()).unwrap().unsubscribe,
            Some(false)
        );
        let head = parse_head(
            b"From: news@x.com\r\nList-Unsubscribe: <mailto:u@x.com>\r\n",
            b"Hi",
            false,
        )
        .unwrap();
        assert_eq!(head.unsubscribe, Some(true));
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

    /// The fields a sync asks for of mail it fetches without its body.
    const HEAD: &str = "From: Ana Ruiz <ana@x.com>\r\n\
To: me@x.com\r\n\
Subject: Figures\r\n\
Date: Tue, 21 Oct 2025 14:00:00 +0000\r\n\
Message-ID: <fig@x.com>\r\n\
MIME-Version: 1.0\r\n";

    fn head(content_type: &str) -> Vec<u8> {
        format!("{HEAD}Content-Type: {content_type}\r\n\r\n").into_bytes()
    }

    #[test]
    fn mail_without_its_body_keeps_its_headers_and_preview() {
        let m = parse_head(
            &head("text/plain"),
            b"Hello Ana,\r\nthe figures.\r\n",
            false,
        )
        .unwrap();
        assert_eq!(
            (m.from_name.as_str(), m.subject.as_str()),
            ("Ana Ruiz", "Figures")
        );
        assert_eq!(m.message_id.as_deref(), Some("fig@x.com"));
        assert_eq!(m.date, 1_761_055_200_000);
        assert_eq!(m.snippet, "Hello Ana, the figures.");
        assert_eq!(m.text, m.snippet, "only the preview, for search");
        assert!(m.body == Body::Missing && m.html.is_none() && m.attachments.is_empty());

        // The fields come without the blank line that ends them, or with
        // nothing after them at all.
        let fields = format!("{HEAD}Content-Type: text/plain\r\n");
        let m = parse_head(fields.as_bytes(), b"Short.", false).unwrap();
        assert_eq!(m.snippet, "Short.");
        let m = parse_head(&head("text/plain"), b"", true).unwrap();
        assert_eq!((m.subject.as_str(), m.snippet.as_str()), ("Figures", ""));
    }

    #[test]
    fn a_cut_multipart_still_decodes_its_first_text() {
        // Base64 in an alternative, cut in the middle of a line.
        let text = b"--b\r\nContent-Type: text/plain; charset=utf-8\r\n\
Content-Transfer-Encoding: base64\r\n\r\n\
SGVsbG8gdGhlcmUsIHRoaXMgaXMg\r\ndGhlIHN0YXJ0IG9mIGEgbG9uZyB0ZXh0IGFiY";
        let m = parse_head(&head("multipart/alternative; boundary=b"), text, true).unwrap();
        assert_eq!(m.snippet, "Hello there, this is");

        // Nested in a mixed message, with HTML after it.
        let text = b"--m\r\nContent-Type: multipart/alternative; boundary=\"a\"\r\n\r\n\
--a\r\nContent-Type: text/plain; charset=utf-8\r\n\
Content-Transfer-Encoding: base64\r\n\r\n\
SGVsbG8gdGhlcmUsIHRoaXMgaXMg\r\ndGhlIHN0YXJ0IG9mIGEgbG9uZyB0ZXh0IGFi\r\nY2Rl";
        let m = parse_head(&head("multipart/mixed; boundary=m"), text, true).unwrap();
        assert_eq!(
            m.snippet,
            "Hello there, this is the start of a long text ab"
        );

        // Cut in the boundary line after the text: none of it shows.
        let text = b"--bbbbbbbb\r\nContent-Type: text/plain\r\n\r\nSome text.\r\n--bbbb";
        let m = parse_head(
            &head("multipart/alternative; boundary=bbbbbbbb"),
            text,
            true,
        )
        .unwrap();
        assert_eq!(m.snippet, "Some text.");

        // Cut before any text.
        let text = b"This is a multi-part message in MIME format.\r\n\r\n--m\r\nContent-Ty";
        let m = parse_head(&head("multipart/mixed; boundary=m"), text, true).unwrap();
        assert_eq!(m.snippet, "");
    }

    #[test]
    fn cut_text_loses_its_last_line_not_its_characters() {
        // Quoted-printable cut in the middle of an escape.
        let text = b"Caf=C3=A9 au lait,\r\ntr=C3=A8s bon=\r\n et encore =C3";
        let m = parse_head(
            &head("text/plain; charset=utf-8\r\nContent-Transfer-Encoding: quoted-printable"),
            text,
            true,
        )
        .unwrap();
        assert_eq!(m.snippet, "Café au lait, très bon");

        // Base64 whose last whole line ends half way into a character.
        let text = b"Q2Fmw6kgYXUgbGFpdCwgdHLD\r\nqHMgYm9uIGV0";
        let m = parse_head(
            &head("text/plain; charset=utf-8\r\nContent-Transfer-Encoding: base64"),
            text,
            true,
        )
        .unwrap();
        assert_eq!(m.snippet, "Café au lait, tr");

        // HTML only: its words, not its markup.
        let text = b"<html><head><style>p{color:red}</style></head><body><p>Hi Ana, the figures</p>\r\n<p>More text that goes on and o";
        let m = parse_head(&head("text/html; charset=utf-8"), text, true).unwrap();
        assert_eq!(m.snippet, "Hi Ana, the figures");
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
