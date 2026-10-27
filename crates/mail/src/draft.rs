//! Outgoing mail: the composer's draft and turning it into a MIME message.

use lettre::message::header::{ContentType, HeaderName, HeaderValue};
use lettre::message::{Attachment, Mailbox, Mailboxes, MessageBuilder, MultiPart, SinglePart};
use serde::{Deserialize, Serialize};

use crate::attachment::{AttachmentData, InlineImage};
use crate::signature::{self, Parts, Quote};
use crate::{Error, Result, trackers};

/// Most servers (Gmail, Microsoft 365) refuse messages whose attachments add
/// up to more than this.
pub const MAX_ATTACHMENTS_BYTES: u64 = 25 * 1024 * 1024;

/// What the composer sends. Address fields are comma- or semicolon-separated
/// `Name <address>` or bare addresses.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Draft {
    pub account: i64,
    pub to: String,
    #[serde(default)]
    pub cc: String,
    #[serde(default)]
    pub bcc: String,
    pub subject: String,
    /// Plain text: what's typed, or with `body_html`, its text.
    pub body: String,
    /// The body from the rich editor, as HTML: cleaned when the message is
    /// built, and sent with `body`'s text alongside. `None` for plain text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_html: Option<String>,
    /// The message this replies to, for threading.
    #[serde(default)]
    pub reply_to: Option<i64>,
    /// Unix ms: the end of the undo window, or the Send Later time.
    pub send_at: i64,
    /// The user's schedule selection while editing a draft. Saving leaves
    /// `send_at` at zero; only an explicit Send queues delivery.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub send_later: Option<SendLater>,
    /// Files to attach, in order. Always written, even empty: the composer
    /// reads it as an array.
    #[serde(default)]
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
    /// The signature below the body, as chosen (and maybe edited) for this
    /// message: HTML, cleaned when the message is built. `None` sends
    /// without one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    /// The saved signature it started as. Sending makes it the address's
    /// most recent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature_id: Option<i64>,
    /// A forward's original, below the signature. Plain text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quote: Option<String>,
    /// "Remind me", set once it's sent (see `spark_store::NewReminder`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remind: Option<Remind>,
}

/// Spark's "Remind me" on an email being sent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Remind {
    /// When it comes back (Unix ms).
    pub at: i64,
    /// Only if nobody has replied by then.
    pub if_no_reply: bool,
}

/// A Send Later preset, fixed to the time the user chose it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SendLater {
    pub key: String,
    pub at: i64,
}

/// What a draft sends: its text and, with rich text or a signature, HTML
/// too, plus the images they embed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bodies {
    pub text: String,
    pub html: Option<String>,
    pub images: Vec<InlineImage>,
}

impl Draft {
    /// The signature as it goes out: cleaned, and `None` if nothing's left.
    pub fn clean_signature(&self) -> Option<String> {
        let clean = signature::sanitize(self.signature.as_deref()?);
        (!signature::text(&clean).is_empty() || clean.contains("<img")).then_some(clean)
    }

    /// The body, the signature, then the original: a reply's `original`
    /// quoted, or a forward's `quote`. `body_style` styles the typed text in
    /// the HTML.
    ///
    /// Embedded (`data:`) images become parts of their own.
    pub fn bodies(&self, original: Option<&Original>, body_style: Option<&str>) -> Bodies {
        self.bodies_with(original, body_style, true, false)
    }

    /// The draft as the Drafts folder keeps it: like [`Draft::bodies`]
    /// without a reply quote, and with a template's placeholders still in
    /// it, for the composer to show when it's opened again.
    pub fn draft_bodies(&self, body_style: Option<&str>) -> Bodies {
        self.bodies_with(None, body_style, true, true)
    }

    /// The draft as it's kept here while it's saved: no reply quote, the
    /// images left in its HTML, and its placeholders kept.
    pub fn saved_bodies(&self) -> Bodies {
        self.bodies_with(None, None, false, true)
    }

    fn bodies_with(
        &self,
        original: Option<&Original>,
        body_style: Option<&str>,
        embed: bool,
        draft: bool,
    ) -> Bodies {
        // Placeholders are for the composer; mail that's sent goes without.
        let clean = if draft {
            signature::sanitize_template
        } else {
            signature::sanitize
        };
        // Without tracking pixels pasted in, or quoted in a draft from another app.
        let body_html = self
            .body_html
            .as_deref()
            .map(|html| clean(&trackers::strip(html)));
        let signature = self.clean_signature();
        let original = original.filter(|o| !o.text.trim().is_empty());
        let quoted = original.and_then(|o| o.html.as_deref().map(signature::sanitize_quoted));
        let quote = match (original, &self.quote) {
            (Some(o), _) => Some(Quote::Reply {
                from: &o.from,
                text: &o.text,
                html: quoted.as_deref(),
            }),
            (None, Some(q)) if !q.trim().is_empty() => Some(Quote::Plain(q)),
            _ => None,
        };
        let parts = Parts {
            body: &self.body,
            body_html: body_html.as_deref(),
            signature: signature.as_deref(),
            quote,
        };
        let html = (body_html.is_some() || signature.is_some()).then(|| parts.html(body_style));
        let (html, images) = match html {
            Some(html) if embed => {
                let (html, images) = signature::embed_images(&html);
                (Some(html), images)
            }
            html => (html, Vec::new()),
        };
        Bodies {
            text: parts.text(),
            html,
            images,
        }
    }
}

impl Bodies {
    /// Text and HTML as alternatives, the HTML's images related to it;
    /// `None` for text alone.
    fn alternative(&self) -> Option<MultiPart> {
        let html = SinglePart::html(self.html.clone()?);
        let plain = SinglePart::plain(self.text.clone());
        if self.images.is_empty() {
            return Some(MultiPart::alternative().singlepart(plain).singlepart(html));
        }
        let mut related = MultiPart::related().singlepart(html);
        for image in &self.images {
            related = related.singlepart(
                Attachment::new_inline(image.cid.clone())
                    .body(image.bytes.clone(), content_type(&image.mime)),
            );
        }
        Some(
            MultiPart::alternative()
                .singlepart(plain)
                .multipart(related),
        )
    }

    /// Finishes `builder` with these bodies and `files` attached in order:
    /// a plain text message when that's all there is.
    pub(crate) fn build(
        &self,
        builder: MessageBuilder,
        files: &[AttachmentData],
    ) -> std::result::Result<lettre::Message, lettre::error::Error> {
        let content = self.alternative();
        if files.is_empty() {
            return match content {
                Some(alternative) => builder.multipart(alternative),
                None => builder.body(self.text.clone()),
            };
        }
        let mut mixed = match content {
            Some(alternative) => MultiPart::mixed().multipart(alternative),
            None => MultiPart::mixed().singlepart(SinglePart::plain(self.text.clone())),
        };
        for f in files {
            mixed = mixed.singlepart(
                Attachment::new(f.name.clone()).body(f.bytes.clone(), content_type(&f.mime)),
            );
        }
        builder.multipart(mixed)
    }
}

/// `mime` as a content type, falling back to octet-stream.
pub(crate) fn content_type(mime: &str) -> ContentType {
    ContentType::parse(mime)
        .unwrap_or_else(|_| ContentType::parse("application/octet-stream").expect("valid"))
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

impl Draft {
    /// Refuses a draft whose attachments and embedded images add up to more
    /// than servers take.
    pub fn check_size(&self) -> Result<()> {
        let images = [self.body_html.as_deref(), self.signature.as_deref()]
            .into_iter()
            .flatten()
            .map(embedded_bytes)
            .sum();
        check_attachment_size(&self.attachments, images)
    }
}

/// About how many bytes the `data:` images in `html` decode to.
fn embedded_bytes(html: &str) -> u64 {
    html.match_indices("src=\"data:")
        .filter_map(|(at, _)| {
            let url = &html[at + 5..];
            let data = &url[url.find(',')? + 1..url.find('"')?];
            Some(data.len() as u64 * 3 / 4)
        })
        .sum()
}

/// Refuses attachments that, with `extra` bytes of images, add up to more
/// than servers take.
pub fn check_attachment_size(attachments: &[OutgoingAttachment], extra: u64) -> Result<()> {
    let total: u64 = attachments
        .iter()
        .map(OutgoingAttachment::size)
        .sum::<u64>()
        + extra;
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
    /// "Ana Ruiz" and her text, quoted under the reply, in HTML if it has
    /// one.
    pub from: String,
    pub text: String,
    pub html: Option<String>,
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

/// A new Message-ID at `domain`, with its angle brackets.
pub(crate) fn new_message_id(domain: &str) -> String {
    let mut random = [0u8; 16];
    getrandom::fill(&mut random).expect("the OS provides randomness");
    let id: String = random.iter().map(|b| format!("{b:02x}")).collect();
    format!("<{id}@{domain}>")
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
    let mut builder = lettre::Message::builder()
        .message_id(Some(new_message_id(from.email.domain())))
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
    draft
        .bodies(original, None)
        .build(builder, files)
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
            attachments: Vec::new(),
            draft_id: None,
            in_reply_to: None,
            references: None,
            ..Default::default()
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
            html: None,
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
    fn a_signature_goes_out_as_text_and_html_above_the_quote() {
        let original = Original {
            message_id: Some("1@x.com".into()),
            from: "Ana Ruiz".into(),
            text: "Shall we?".into(),
            ..Default::default()
        };
        let mut d = draft("ana@x.com");
        d.signature =
            Some(r#"<div><b>Me</b></div><div onclick="x()">ACME</div><script>x()</script>"#.into());
        let from: Mailbox = "me@gmail.com".parse().unwrap();
        let raw = build_message(from, &d, Some(&original), &[])
            .unwrap()
            .formatted();
        let parsed = mail_parser::MessageParser::default().parse(&raw).unwrap();
        assert_eq!(
            parsed.body_text(0).unwrap().replace("\r\n", "\n"),
            "Sounds good.\n\nMe\nACME\n\nAna Ruiz wrote:\n> Shall we?\n"
        );
        let html = parsed.body_html(0).unwrap();
        let body = html.find("messageBodySection").unwrap();
        let signature = html
            .find("<div name=\"messageSignatureSection\"><div><b>Me</b></div><div>ACME</div></div>")
            .unwrap();
        let reply = html.find("messageReplySection").unwrap();
        assert!(body < signature && signature < reply, "{html}");
        assert!(
            !html.contains("onclick") && !html.contains("script"),
            "{html}"
        );
        let raw = String::from_utf8_lossy(&raw);
        assert!(raw.contains("multipart/alternative"), "{raw}");
    }

    #[test]
    fn signature_images_travel_as_related_parts() {
        let mut d = draft("ana@x.com");
        d.signature = Some(
            r#"<img src="data:image/png;base64,iVBORw0KGgo=" width="40"><div>Me</div>"#.into(),
        );
        let files = [AttachmentData {
            name: "plan.pdf".into(),
            mime: "application/pdf".into(),
            bytes: b"%PDF".to_vec(),
        }];
        let from: Mailbox = "me@gmail.com".parse().unwrap();
        let raw = build_message(from, &d, None, &files).unwrap().formatted();
        let text = String::from_utf8_lossy(&raw);
        assert!(
            text.contains("multipart/mixed") && text.contains("multipart/related"),
            "{text}"
        );
        let parsed = mail_parser::MessageParser::default().parse(&raw).unwrap();
        let html = parsed.body_html(0).unwrap();
        let cid = html
            .split("src=\"cid:")
            .nth(1)
            .unwrap()
            .split('"')
            .next()
            .unwrap();
        assert!(text.contains(&format!("Content-ID: <{cid}>")), "{text}");
        let listed = crate::attachment::describe(&parsed);
        assert_eq!(
            listed.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(),
            ["plan.pdf"],
            "the image isn't a file"
        );
    }

    #[test]
    fn rich_text_goes_out_as_html_with_its_text_and_images() {
        let original = Original {
            message_id: Some("1@x.com".into()),
            from: "Ana Ruiz".into(),
            text: "Shall we?".into(),
            html: Some(
                r#"<p style="color:red">Shall <b>we</b>?</p><img src="cid:logo@x"><img src="https://x.com/a.png">"#
                    .into(),
            ),
            ..Default::default()
        };
        let mut d = draft("ana@x.com");
        d.body = "ignored".into();
        d.body_html = Some(
            r#"<div>Hi <a href="mailto:bo@x.com">Bo</a>,</div><ul><li><b>one</b></li><li>two</li></ul><ol><li>first</li></ol><div><img src="data:image/png;base64,iVBORw0KGgo=" onerror="x()"></div>"#
                .into(),
        );
        let from: Mailbox = "me@gmail.com".parse().unwrap();
        let raw = build_message(from, &d, Some(&original), &[])
            .unwrap()
            .formatted();
        let parsed = mail_parser::MessageParser::default().parse(&raw).unwrap();
        assert_eq!(
            parsed.body_text(0).unwrap().replace("\r\n", "\n"),
            "Hi Bo,\n- one\n- two\n1. first\n\nAna Ruiz wrote:\n> Shall we?\n"
        );
        let html = parsed.body_html(0).unwrap();
        assert!(
            html.starts_with(r#"<div name="messageBodySection"><div>Hi <a href="mailto:bo@x.com">Bo</a>,</div><ul><li><b>one</b></li>"#),
            "{html}"
        );
        assert!(
            !html.contains("onerror") && !html.contains("data:"),
            "{html}"
        );
        // The original's formatting is quoted, without the image it carried.
        assert!(
            html.contains(r#"<blockquote type="cite" style="margin:0 0 0 .8ex;border-left:1px solid #ccc;padding-left:1ex"><p style="color:red">Shall <b>we</b>?</p><img src="https://x.com/a.png"></blockquote>"#),
            "{html}"
        );
        let text = String::from_utf8_lossy(&raw);
        assert!(
            text.contains("multipart/related"),
            "the body's image is a part: {text}"
        );
        // Without a signature, plain text still goes as plain text.
        let plain = draft("ana@x.com").bodies(None, None);
        assert_eq!((plain.text.as_str(), plain.html), ("Sounds good.", None));
    }

    #[test]
    fn tracking_pixels_in_the_body_stay_behind() {
        let mut d = draft("ana@x.com");
        d.body_html = Some(r#"<p>Hi</p><img src="https://t.x/o.gif" width="1" height="1">"#.into());
        let html = d.bodies(None, None).html.unwrap();
        assert!(
            html.contains("<p>Hi</p>") && !html.contains("o.gif"),
            "{html}"
        );
    }

    #[test]
    fn embedded_images_count_toward_the_size_limit() {
        let mut d = draft("ana@x.com");
        let data = "A".repeat(4 * 1024 * 1024);
        d.body_html = Some(format!(r#"<img src="data:image/png;base64,{data}">"#));
        assert!(d.check_size().is_ok());
        d.attachments = vec![OutgoingAttachment::File {
            path: "x".into(),
            name: "x.bin".into(),
            size: MAX_ATTACHMENTS_BYTES - 1024 * 1024,
        }];
        assert!(d.check_size().is_err(), "3 MB of image and 24 MB of files");
    }

    #[test]
    fn a_forward_keeps_its_original_below_the_signature() {
        let mut d = draft("ana@x.com");
        d.body = "FYI".into();
        d.quote = Some("---------- Forwarded message ----------\nFrom: Bo".into());
        assert_eq!(
            d.bodies(None, None).text,
            "FYI\n\n---------- Forwarded message ----------\nFrom: Bo"
        );
        d.signature = Some("<p>Me</p>".into());
        let bodies = d.bodies(None, None);
        assert_eq!(
            bodies.text,
            "FYI\n\nMe\n\n---------- Forwarded message ----------\nFrom: Bo"
        );
        assert!(bodies.html.unwrap().ends_with("<div name=\"messageReplySection\"><br>---------- Forwarded message ----------<br>From: Bo</div>"));
    }

    #[test]
    fn an_empty_signature_is_left_out() {
        let mut d = draft("ana@x.com");
        d.signature = Some("<div><br></div><script>x</script>".into());
        assert_eq!(d.clean_signature(), None);
        let bodies = d.bodies(None, None);
        assert_eq!((bodies.text.as_str(), bodies.html), ("Sounds good.", None));
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
    fn drafts_without_attachments_still_list_them() {
        // The composer reads `attachments` as an array: opening a draft without it blanked the app.
        let json = serde_json::to_value(draft("ana@x.com")).unwrap();
        assert_eq!(json["attachments"], serde_json::json!([]), "{json}");
    }

    #[test]
    fn attachments_over_25_mb_are_refused() {
        let file = |size| OutgoingAttachment::File {
            path: String::new(),
            name: "big.zip".into(),
            size,
        };
        assert!(check_attachment_size(&[file(MAX_ATTACHMENTS_BYTES)], 0).is_ok());
        let err = check_attachment_size(&[file(MAX_ATTACHMENTS_BYTES), file(1)], 0).unwrap_err();
        assert!(err.to_string().contains("25 MB"), "{err}");
    }
}
