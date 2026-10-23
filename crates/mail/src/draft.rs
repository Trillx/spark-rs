//! Outgoing mail: the composer's draft and turning it into a MIME message.

use lettre::message::header::{ContentType, HeaderName, HeaderValue};
use lettre::message::{Attachment, Mailbox, Mailboxes, MultiPart, SinglePart};
use serde::{Deserialize, Serialize};

use crate::attachment::AttachmentData;
use crate::{Error, Result};

/// Most servers (Gmail, Microsoft 365) refuse messages whose attachments add
/// up to more than this.
pub const MAX_ATTACHMENTS_BYTES: u64 = 25 * 1024 * 1024;

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
    /// Files to attach, in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<OutgoingAttachment>,
    /// The saved draft (its message id) this was opened from or saved as:
    /// its server copy is deleted once this is sent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draft_id: Option<i64>,
    /// Threading for a reply whose original isn't stored here (a draft from
    /// the server): the original's Message-ID and References.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_reply_to: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub references: Option<String>,
}

/// A file on an outgoing message.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum OutgoingAttachment {
    /// A file from this computer. The app copies it into the outbox when the
    /// draft is queued; `path` is kept so an undone send can show it again.
    File { path: String, name: String, size: u64 },
    /// Attachment `index` of a stored message, fetched again from the server
    /// when the draft goes out: a forward keeps the original's files.
    Message {
        message: i64,
        index: usize,
        name: String,
        size: u64,
    },
}

impl OutgoingAttachment {
    pub fn size(&self) -> u64 {
        match self {
            Self::File { size, .. } | Self::Message { size, .. } => *size,
        }
    }
}

/// Refuses attachments that add up to more than servers take.
pub fn check_attachment_size(attachments: &[OutgoingAttachment]) -> Result<()> {
    let total: u64 = attachments.iter().map(OutgoingAttachment::size).sum();
    if total > MAX_ATTACHMENTS_BYTES {
        return Err(Error::Config(format!(
            "The attachments add up to {:.1} MB; mail servers take up to 25 MB.",
            total as f64 / (1024.0 * 1024.0)
        )));
    }
    Ok(())
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
pub(crate) fn split_addresses(field: &str) -> Vec<&str> {
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

/// Builds the MIME message for SMTP, with `files` attached in order.
pub fn build_message(
    from: Mailbox,
    draft: &Draft,
    original: Option<&Original>,
    files: &[AttachmentData],
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
    let body = quoted_body(&draft.body, original);
    let built = if files.is_empty() {
        builder.body(body)
    } else {
        let mut mixed = MultiPart::mixed().singlepart(SinglePart::plain(body));
        for f in files {
            let content_type = ContentType::parse(&f.mime)
                .unwrap_or_else(|_| ContentType::parse("application/octet-stream").expect("valid"));
            mixed = mixed.singlepart(Attachment::new(f.name.clone()).body(f.bytes.clone(), content_type));
        }
        builder.multipart(mixed)
    };
    built.map_err(|e| Error::Config(format!("Couldn't build the message: {e}")))
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
            attachments: Vec::new(),
            draft_id: None,
            in_reply_to: None,
            references: None,
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
        let message = build_message(from, &draft("ana@x.com"), Some(&original), &[]).unwrap();
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
        assert!(build_message(from, &draft(" "), None, &[]).is_err());
    }

    #[test]
    fn files_go_out_as_a_mixed_multipart() {
        let from: Mailbox = "me@gmail.com".parse().unwrap();
        let files = [
            AttachmentData {
                name: "plan.pdf".into(),
                mime: "application/pdf".into(),
                bytes: b"%PDF-1.7
".to_vec(),
            },
            AttachmentData {
                name: "Résumé.txt".into(),
                mime: "not a type".into(),
                bytes: b"hi".to_vec(),
            },
        ];
        let message = build_message(from, &draft("ana@x.com"), None, &files).unwrap();
        let raw = message.formatted();
        let parsed = mail_parser::MessageParser::default().parse(&raw).unwrap();
        assert_eq!(parsed.body_text(0).unwrap().trim(), "Sounds good.");
        let list = crate::attachment::describe(&parsed);
        assert_eq!(list.len(), 2, "{}", String::from_utf8_lossy(&raw));
        assert_eq!(list[0].name, "plan.pdf");
        assert_eq!(list[0].mime, "application/pdf");
        assert_eq!(list[1].name, "Résumé.txt", "non-ASCII names survive");
        assert_eq!(list[1].mime, "application/octet-stream", "bad types fall back");
        let pdf = crate::attachment::extract(&raw, 0, "plan.pdf").unwrap();
        assert_eq!(pdf.bytes, b"%PDF-1.7
");
    }

    #[test]
    fn drafts_keep_their_attachments_through_json() {
        let mut d = draft("ana@x.com");
        d.attachments = vec![
            OutgoingAttachment::File {
                path: r"C:\x\plan.pdf".into(),
                name: "plan.pdf".into(),
                size: 10,
            },
            OutgoingAttachment::Message {
                message: 7,
                index: 1,
                name: "photo.jpg".into(),
                size: 20,
            },
        ];
        let json = serde_json::to_string(&d).unwrap();
        assert!(json.contains(r#"{"kind":"message","message":7,"index":1,"name":"photo.jpg","size":20}"#), "{json}");
        assert_eq!(serde_json::from_str::<Draft>(&json).unwrap(), d);
        // Drafts queued before attachments existed still read.
        let old = r#"{"account":1,"to":"a@x.com","subject":"s","body":"b","sendAt":0}"#;
        assert!(serde_json::from_str::<Draft>(old).unwrap().attachments.is_empty());
    }

    #[test]
    fn attachments_over_25_mb_are_refused() {
        let file = |size| OutgoingAttachment::File {
            path: String::new(),
            name: "big.zip".into(),
            size,
        };
        assert!(check_attachment_size(&[file(MAX_ATTACHMENTS_BYTES)]).is_ok());
        let err = check_attachment_size(&[file(MAX_ATTACHMENTS_BYTES), file(1)]).unwrap_err();
        assert!(err.to_string().contains("25 MB"), "{err}");
    }
}
