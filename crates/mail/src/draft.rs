//! Outgoing mail: the composer's draft and turning it into a MIME message.

use lettre::message::header::{HeaderName, HeaderValue};
use lettre::message::{Mailbox, Mailboxes};
use serde::{Deserialize, Serialize};

use crate::{Error, Result};

/// What the composer sends. Address fields are comma- or semicolon-separated
/// `Name <address>` or bare addresses.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Draft {
    pub account: i64,
    pub to: String,
    #[serde(default)]
    pub cc: String,
    #[serde(default)]
    pub bcc: String,
    pub subject: String,
    /// Plain text.
    pub body: String,
    /// The message this replies to, for threading.
    #[serde(default)]
    pub reply_to: Option<i64>,
    /// Unix ms: the end of the undo window, or the Send Later time.
    pub send_at: i64,
}

/// The message being replied to.
#[derive(Clone, Debug, Default)]
pub struct Original {
    pub message_id: Option<String>,
    pub references: Option<String>,
    /// "Ana Ruiz" and her text, quoted under the reply.
    pub from: String,
    pub text: String,
}

/// Parses an address field. Empty is fine; anything unparsable is an error
/// that names the bad part.
pub fn recipients(field: &str) -> Result<Vec<Mailbox>> {
    let field = field.replace(';', ",");
    let mut out = Vec::new();
    for part in split_addresses(&field) {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let parsed: Mailboxes = part
            .parse()
            .map_err(|_| Error::Config(format!("\"{part}\" isn't an email address.")))?;
        out.extend(parsed);
    }
    Ok(out)
}

/// Splits on commas outside quotes and angle brackets.
fn split_addresses(field: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let (mut start, mut quoted, mut angle) = (0, false, false);
    for (i, c) in field.char_indices() {
        match c {
            '"' => quoted = !quoted,
            '<' if !quoted => angle = true,
            '>' if !quoted => angle = false,
            ',' if !quoted && !angle => {
                parts.push(&field[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(&field[start..]);
    parts
}

/// The reply text with the original quoted below it.
pub fn quoted_body(body: &str, original: Option<&Original>) -> String {
    let Some(original) = original.filter(|o| !o.text.trim().is_empty()) else {
        return body.to_owned();
    };
    let quote: Vec<String> = original
        .text
        .trim_end()
        .lines()
        .map(|line| {
            if line.is_empty() {
                ">".into()
            } else {
                format!("> {line}")
            }
        })
        .collect();
    format!(
        "{}\n\n{} wrote:\n{}\n",
        body.trim_end(),
        original.from,
        quote.join("\n")
    )
}

/// Builds the MIME message for SMTP.
pub fn build_message(
    from: Mailbox,
    draft: &Draft,
    original: Option<&Original>,
) -> Result<lettre::Message> {
    let to = recipients(&draft.to)?;
    let cc = recipients(&draft.cc)?;
    let bcc = recipients(&draft.bcc)?;
    if to.is_empty() && cc.is_empty() && bcc.is_empty() {
        return Err(Error::Config("Add at least one recipient.".into()));
    }
    // Replies to this message thread by its Message-ID.
    let mut random = [0u8; 16];
    getrandom::fill(&mut random).expect("the OS provides randomness");
    let id: String = random.iter().map(|b| format!("{b:02x}")).collect();
    let mut builder = lettre::Message::builder()
        .message_id(Some(format!("<{id}@{}>", from.email.domain())))
        .from(from)
        .subject(draft.subject.trim());
    for m in to {
        builder = builder.to(m);
    }
    for m in cc {
        builder = builder.cc(m);
    }
    for m in bcc {
        builder = builder.bcc(m);
    }
    if let Some(id) = original.and_then(|o| o.message_id.as_deref()) {
        builder = builder.in_reply_to(format!("<{id}>"));
        let refs = original
            .and_then(|o| o.references.as_deref())
            .unwrap_or_default()
            .split_whitespace()
            .chain([id])
            .map(|r| format!("<{r}>"))
            .collect::<Vec<_>>()
            .join(" ");
        builder = builder.raw_header(HeaderValue::new(
            HeaderName::new_from_ascii_str("References"),
            refs,
        ));
    }
    builder
        .body(quoted_body(&draft.body, original))
        .map_err(|e| Error::Config(format!("Couldn't build the message: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft(to: &str) -> Draft {
        Draft {
            account: 1,
            to: to.into(),
            cc: String::new(),
            bcc: String::new(),
            subject: " Re: Plan ".into(),
            body: "Sounds good.".into(),
            reply_to: None,
            send_at: 0,
        }
    }

    #[test]
    fn address_fields_accept_names_quotes_and_semicolons() {
        let list =
            recipients("Ana Ruiz <ana@x.com>; bob@y.org, \"Doe, John\" <john@z.net>,").unwrap();
        let emails: Vec<String> = list.iter().map(|m| m.email.to_string()).collect();
        assert_eq!(emails, ["ana@x.com", "bob@y.org", "john@z.net"]);
        assert_eq!(list[2].name.as_deref(), Some("Doe, John"));
        assert!(recipients("").unwrap().is_empty());
        let err = recipients("ana@x.com, not an address")
            .unwrap_err()
            .to_string();
        assert!(err.contains("not an address"), "{err}");
    }

    #[test]
    fn replies_thread_and_quote_the_original() {
        let original = Original {
            message_id: Some("1@x.com".into()),
            references: Some("0@x.com".into()),
            from: "Ana Ruiz".into(),
            text: "Shall we?\n\nA.".into(),
        };
        let from: Mailbox = "Me <me@gmail.com>".parse().unwrap();
        let message = build_message(from, &draft("ana@x.com"), Some(&original)).unwrap();
        let raw = String::from_utf8(message.formatted()).unwrap();
        assert!(raw.contains("In-Reply-To: <1@x.com>"), "{raw}");
        assert!(
            raw.contains("@gmail.com>\r\n"),
            "a Message-ID on the sender's domain: {raw}"
        );
        assert!(raw.contains("References: <0@x.com> <1@x.com>"), "{raw}");
        assert!(raw.contains("Subject: Re: Plan\r\n"), "{raw}");
        assert!(
            raw.contains("Sounds good.\r\n\r\nAna Ruiz wrote:\r\n> Shall we?\r\n>\r\n> A."),
            "{raw}"
        );
    }

    #[test]
    fn a_draft_needs_a_recipient() {
        let from: Mailbox = "me@gmail.com".parse().unwrap();
        assert!(build_message(from, &draft(" "), None).is_err());
    }
}
