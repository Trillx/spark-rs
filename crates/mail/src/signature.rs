//! Signatures in outgoing mail. A signature is HTML the user wrote or
//! pasted, cleaned here before it's kept or sent. Mail with a signature goes
//! out as text and HTML. The HTML is laid out in sections (body, signature,
//! then the quoted original), named the way Spark names them, which is also
//! how a saved draft's signature is found again.

use std::collections::{HashMap, HashSet};

use ammonia::{Builder, UrlRelative};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use spark_store::{DefaultSignature, Signature, Store};

use crate::attachment::InlineImage;
use crate::parse::html_to_text;

pub const BODY_SECTION: &str = "messageBodySection";
pub const SIGNATURE_SECTION: &str = "messageSignatureSection";
pub const REPLY_SECTION: &str = "messageReplySection";

/// Embedded images a signature may carry, each at most this big.
pub const MAX_IMAGE_BYTES: usize = 1024 * 1024;

const TAGS: &[&str] = &[
    "a",
    "abbr",
    "b",
    "big",
    "blockquote",
    "br",
    "caption",
    "center",
    "code",
    "col",
    "colgroup",
    "div",
    "em",
    "font",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "hr",
    "i",
    "img",
    "li",
    "ol",
    "p",
    "pre",
    "s",
    "small",
    "span",
    "strike",
    "strong",
    "sub",
    "sup",
    "table",
    "tbody",
    "td",
    "tfoot",
    "th",
    "thead",
    "tr",
    "u",
    "ul",
];
const GENERIC_ATTRIBUTES: &[&str] = &[
    "align", "bgcolor", "color", "dir", "height", "style", "title", "valign", "width",
];
const TAG_ATTRIBUTES: &[(&str, &[&str])] = &[
    ("a", &["href"]),
    ("img", &["src", "alt", "border"]),
    ("font", &["face", "size"]),
    ("table", &["border", "cellpadding", "cellspacing"]),
    ("td", &["colspan", "rowspan", "nowrap"]),
    ("th", &["colspan", "rowspan", "nowrap"]),
    ("col", &["span"]),
    ("colgroup", &["span"]),
];
const IMAGE_TYPES: &[&str] = &[
    "image/png",
    "image/jpeg",
    "image/gif",
    "image/webp",
    "image/bmp",
];

/// Keeps what signatures are made of (text formatting, inline styles, links,
/// images and tables) and drops the rest: scripts, styles sheets, event
/// handlers, forms and frames. Links keep http(s), mailto and tel; images
/// keep http(s), embedded raster images and `cid:` parts.
pub fn sanitize(html: &str) -> String {
    let tag_attributes: HashMap<&str, HashSet<&str>> = TAG_ATTRIBUTES
        .iter()
        .map(|(tag, attributes)| (*tag, attributes.iter().copied().collect()))
        .collect();
    Builder::default()
        .tags(TAGS.iter().copied().collect())
        .generic_attributes(GENERIC_ATTRIBUTES.iter().copied().collect())
        .tag_attributes(tag_attributes)
        .url_schemes(["http", "https", "mailto", "tel", "data", "cid"].into())
        .url_relative(UrlRelative::Deny)
        .link_rel(None)
        .attribute_filter(|element, attribute, value| {
            let url = value.trim_start().to_ascii_lowercase();
            let keep = match (element, attribute) {
                ("img", "src") => {
                    url.starts_with("http:")
                        || url.starts_with("https:")
                        || url.starts_with("cid:")
                        || IMAGE_TYPES
                            .iter()
                            .any(|t| url.starts_with(&format!("data:{t};base64,")))
                }
                ("a", "href") => !url.starts_with("data:") && !url.starts_with("cid:"),
                _ => true,
            };
            keep.then(|| value.into())
        })
        .clean(html)
        .to_string()
        .trim()
        .to_owned()
}

/// The signature as plain text, line by line: for the text/plain version,
/// and to name it in lists. Empty for one that's only an image.
pub fn text(html: &str) -> String {
    // html_to_text only breaks lines at <br> and </p>. A block's end breaks
    // the line too, unless it's already broken.
    const BREAK: char = '\u{e000}';
    let mut blocks = html.to_owned();
    for tag in [
        "</div>",
        "</li>",
        "</tr>",
        "</table>",
        "<hr",
        "</h1>",
        "</h2>",
        "</h3>",
        "</h4>",
        "</h5>",
        "</h6>",
        "</blockquote>",
    ] {
        blocks = insert_before(&blocks, tag, BREAK);
    }
    for tag in ["</td>", "</th>"] {
        blocks = insert_before(&blocks, tag, ' ');
    }
    let mut text = String::new();
    for c in html_to_text(&blocks).chars() {
        match c {
            BREAK => {
                text.truncate(text.trim_end_matches(' ').len());
                if !text.is_empty() && !text.ends_with('\n') {
                    text.push('\n');
                }
            }
            '\u{a0}' => text.push(' '),
            c => text.push(c),
        }
    }
    let mut lines: Vec<&str> = Vec::new();
    for line in text.lines().map(str::trim) {
        // One blank line at most, and none to start with.
        if line.is_empty() && lines.last().is_none_or(|l| l.is_empty()) {
            continue;
        }
        lines.push(line);
    }
    while lines.last() == Some(&"") {
        lines.pop();
    }
    lines.join("\n")
}

/// `html` with `c` before each `tag`, in any case.
fn insert_before(html: &str, tag: &str, c: char) -> String {
    // ASCII lowercasing keeps the byte offsets.
    let lower = html.to_ascii_lowercase();
    let mut out = String::with_capacity(html.len());
    let mut last = 0;
    for (at, _) in lower.match_indices(tag) {
        out.push_str(&html[last..at]);
        out.push(c);
        last = at;
    }
    out.push_str(&html[last..]);
    out
}

/// Plain text as HTML: escaped, with its line breaks.
pub fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace("\r\n", "\n")
        .replace('\n', "<br>")
}

/// Moves the signature's embedded (`data:`) images into parts of their own,
/// which is how mail carries them: the HTML refers to them by `cid:`. The
/// same image twice is one part.
pub fn embed_images(html: &str) -> (String, Vec<InlineImage>) {
    const START: &str = "src=\"data:";
    let mut out = String::with_capacity(html.len());
    let mut images: Vec<InlineImage> = Vec::new();
    let mut rest = html;
    while let Some(at) = rest.find(START) {
        let url_start = at + "src=\"".len();
        let Some(len) = rest[url_start..].find('"') else {
            break;
        };
        let url = &rest[url_start..url_start + len];
        out.push_str(&rest[..url_start]);
        match decode_data_url(url) {
            Some((mime, bytes)) => {
                let content_id = match images.iter().find(|i| i.bytes == bytes) {
                    Some(same) => same.cid.clone(),
                    None => {
                        let cid = format!("{}@spark-rs", random_hex());
                        images.push(InlineImage {
                            cid: cid.clone(),
                            mime,
                            bytes,
                        });
                        cid
                    }
                };
                out.push_str("cid:");
                out.push_str(&content_id);
            }
            None => out.push_str(url),
        }
        rest = &rest[url_start + len..];
    }
    out.push_str(rest);
    (out, images)
}

fn decode_data_url(url: &str) -> Option<(String, Vec<u8>)> {
    let (meta, data) = url.strip_prefix("data:")?.split_once(',')?;
    let mime = meta.strip_suffix(";base64")?.to_ascii_lowercase();
    if !IMAGE_TYPES.contains(&mime.as_str()) {
        return None;
    }
    let data: String = data.chars().filter(|c| !c.is_whitespace()).collect();
    let bytes = STANDARD.decode(data).ok()?;
    Some((mime, bytes))
}

fn random_hex() -> String {
    let mut random = [0u8; 12];
    getrandom::fill(&mut random).expect("the OS provides randomness");
    random.iter().map(|b| format!("{b:02x}")).collect()
}

/// What a message says, in its three parts. `quote` is the original below
/// the signature: a forward's, or a reply's "… wrote:" and quoted text.
pub struct Parts<'a> {
    pub body: &'a str,
    /// Sanitized HTML.
    pub signature: Option<&'a str>,
    pub quote: Option<Quote<'a>>,
}

pub enum Quote<'a> {
    /// Text that goes out as it is (a forward's header and original).
    Plain(&'a str),
    /// A reply's original: `from` wrote `text`.
    Reply { from: &'a str, text: &'a str },
}

impl Quote<'_> {
    fn text(&self) -> String {
        match self {
            Self::Plain(text) => text.trim_end().to_owned(),
            Self::Reply { from, text } => {
                let quoted: Vec<String> = text
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
                format!("{from} wrote:\n{}\n", quoted.join("\n"))
            }
        }
    }

    fn html(&self) -> String {
        match self {
            Self::Plain(text) => escape(text.trim_end()),
            Self::Reply { from, text } => format!(
                "{} wrote:<br><blockquote type=\"cite\" style=\"margin:0 0 0 .8ex;border-left:1px solid #ccc;padding-left:1ex\">{}</blockquote>",
                escape(from),
                escape(text.trim_end())
            ),
        }
    }
}

impl Parts<'_> {
    /// The text/plain version: the body, the signature's text, then the quote.
    pub fn text(&self) -> String {
        let mut parts = vec![self.body.trim_end().to_owned()];
        if let Some(signature) = self.signature.map(text).filter(|t| !t.is_empty()) {
            parts.push(signature);
        }
        if let Some(quote) = &self.quote {
            parts.push(quote.text());
        }
        // A forward with nothing typed above it starts at its header.
        parts.retain(|p| !p.is_empty());
        parts.join("\n\n")
    }

    /// The HTML version, in sections. `body_style` styles the typed text,
    /// e.g. in Outlook's default font.
    pub fn html(&self, body_style: Option<&str>) -> String {
        let style = body_style
            .map(|s| format!(" style=\"{s}\""))
            .unwrap_or_default();
        let body = self.body.trim_end();
        let mut html = format!("<div name=\"{BODY_SECTION}\"{style}>{}", escape(body));
        if !body.is_empty() {
            html.push_str("<br><br>");
        }
        html.push_str("</div>");
        if let Some(signature) = self.signature {
            html.push_str(&format!(
                "<div name=\"{SIGNATURE_SECTION}\">{signature}</div>"
            ));
        }
        if let Some(quote) = &self.quote {
            html.push_str(&format!(
                "<div name=\"{REPLY_SECTION}\"><br>{}</div>",
                quote.html()
            ));
        }
        html
    }
}

/// The inside of the `<div name="…">` section in `html`, if it has one.
pub fn section<'a>(html: &'a str, name: &str) -> Option<&'a str> {
    let lower = html.to_ascii_lowercase();
    let name = name.to_ascii_lowercase();
    let at = [
        format!("name=\"{name}\""),
        format!("name='{name}'"),
        format!("name={name}"),
    ]
    .iter()
    .find_map(|n| lower.find(n.as_str()))?;
    let open = lower[..at].rfind("<div")?;
    let start = open + lower[open..].find('>')? + 1;
    let mut depth = 1;
    let mut pos = start;
    loop {
        let next_open = find_tag(&lower, pos, "<div");
        let next_close = lower[pos..].find("</div").map(|i| pos + i)?;
        match next_open {
            Some(o) if o < next_close => {
                depth += 1;
                pos = o + 4;
            }
            _ => {
                depth -= 1;
                if depth == 0 {
                    return Some(&html[start..next_close]);
                }
                pos = next_close + 5;
            }
        }
    }
}

/// The next `<div` tag (not `<divider`) at or after `from`.
fn find_tag(lower: &str, from: usize, tag: &str) -> Option<usize> {
    let mut pos = from;
    while let Some(i) = lower[pos..].find(tag) {
        let at = pos + i;
        match lower[at + tag.len()..].chars().next() {
            Some(c) if c.is_whitespace() || c == '>' || c == '/' => return Some(at),
            _ => pos = at + tag.len(),
        }
    }
    None
}

/// Where `needle` is in `haystack` as byte offsets, comparing only what
/// isn't whitespace: the text of a signature found in a message's text,
/// however the lines broke there.
pub fn find_text(haystack: &str, needle: &str) -> Option<(usize, usize)> {
    let needle: Vec<char> = needle.chars().filter(|c| !c.is_whitespace()).collect();
    if needle.is_empty() {
        return None;
    }
    let hay: Vec<(usize, char)> = haystack
        .char_indices()
        .filter(|(_, c)| !c.is_whitespace())
        .collect();
    let at = hay
        .windows(needle.len())
        .position(|w| w.iter().map(|(_, c)| *c).eq(needle.iter().copied()))?;
    let (start, _) = hay[at];
    let (last, c) = hay[at + needle.len() - 1];
    Some((start, last + c.len_utf8()))
}

/// The signature a reply to message `original` from `mailbox` starts with:
/// the saved one the user signed with last in that conversation, else the
/// address's default. An address set to No Signature gets none.
pub fn for_reply(
    store: &Store,
    mailbox: &str,
    original: i64,
) -> spark_store::Result<Option<Signature>> {
    if store.default_signature(mailbox)? == DefaultSignature::None {
        return Ok(None);
    }
    let saved = store.signatures()?;
    for id in store.sent_in_conversation(original, mailbox, 5)? {
        let Some(content) = store.content(id)? else {
            continue;
        };
        if let Some(found) = signed_with(&saved, &content.text, content.html.as_deref()) {
            return Ok(Some(found.clone()));
        }
    }
    store.signature_for(mailbox)
}

/// Which saved signature a message carries: the one in its signature
/// section, else the longest whose text it has.
fn signed_with<'a>(
    saved: &'a [Signature],
    text: &str,
    html: Option<&str>,
) -> Option<&'a Signature> {
    let squeeze = |t: &str| t.chars().filter(|c| !c.is_whitespace()).collect::<String>();
    if let Some(section) = html.and_then(|h| section(h, SIGNATURE_SECTION)) {
        let found = squeeze(&self::text(section));
        return saved
            .iter()
            .find(|s| !found.is_empty() && squeeze(&s.text) == found);
    }
    saved
        .iter()
        .filter(|s| find_text(text, &s.text).is_some())
        .max_by_key(|s| squeeze(&s.text).len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizing_keeps_formatting_and_drops_what_could_run() {
        let clean = sanitize(
            r#"<script>alert(1)</script><style>p{}</style><div style="color:#333" onclick="x()"><b>Ana</b> <a href="javascript:alert(1)">bad</a> <a href="https://x.com" target="_blank">site</a> <a href="mailto:ana@x.com">mail</a> <a href="data:text/html,hi">data</a><img src="https://x.com/logo.png" onerror="x()" width="80"><img src="data:image/png;base64,iVBORw0KGgo="><img src="data:image/svg+xml;base64,PHN2Zz4="><iframe src="https://x.com"></iframe><form><input></form></div>"#,
        );
        assert!(
            !clean.contains("script") && !clean.contains("alert") && !clean.contains("onclick"),
            "{clean}"
        );
        assert!(
            !clean.contains("onerror") && !clean.contains("iframe") && !clean.contains("input"),
            "{clean}"
        );
        assert!(
            !clean.contains("p{}") && !clean.contains("svg") && !clean.contains("text/html"),
            "{clean}"
        );
        assert!(
            clean.contains(r##"<div style="color:#333"><b>Ana</b>"##),
            "{clean}"
        );
        assert!(
            clean.contains(r#"<a href="https://x.com">site</a>"#),
            "{clean}"
        );
        assert!(
            clean.contains(r#"<a href="mailto:ana@x.com">mail</a>"#),
            "{clean}"
        );
        assert!(
            clean.contains(r#"<img src="https://x.com/logo.png" width="80">"#),
            "{clean}"
        );
        assert!(
            clean.contains(r#"<img src="data:image/png;base64,iVBORw0KGgo=">"#),
            "{clean}"
        );
    }

    #[test]
    fn signatures_read_as_text_line_by_line() {
        let html = "<div><div><b>Ana Ruiz</b></div><div>Head of Sales&nbsp;| ACME</div></div>\
                    <p>+1 555 0100</p><table><tr><td>a</td><td>b</td></tr></table><div><br></div><div>x</div>";
        assert_eq!(
            text(html),
            "Ana Ruiz\nHead of Sales | ACME\n+1 555 0100\na b\n\nx"
        );
        assert_eq!(text(r#"<img src="data:image/png;base64,AA==">"#), "");
    }

    #[test]
    fn embedded_images_become_parts() {
        let png = STANDARD.encode(b"\x89PNG one");
        let html = format!(
            r#"<img src="data:image/png;base64,{png}"><img src="https://x.com/a.png"><img src="data:image/png;base64,{png}">"#
        );
        let (out, images) = embed_images(&html);
        assert_eq!(images.len(), 1, "the same image is one part");
        assert_eq!(images[0].mime, "image/png");
        assert_eq!(images[0].bytes, b"\x89PNG one");
        let cid = format!("cid:{}", images[0].cid);
        assert_eq!(out.matches(&cid).count(), 2, "{out}");
        assert!(out.contains(r#"src="https://x.com/a.png""#), "{out}");
        assert!(images[0].cid.ends_with("@spark-rs"));
        let (same, none) = embed_images("<b>Hi</b>");
        assert_eq!((same.as_str(), none.len()), ("<b>Hi</b>", 0));
    }

    #[test]
    fn parts_go_body_signature_quote() {
        let parts = Parts {
            body: "Sounds good.\n",
            signature: Some("<div>Ana</div><div>ACME</div>"),
            quote: Some(Quote::Reply {
                from: "Bo <b@x.com>",
                text: "Shall we?\n\nB.",
            }),
        };
        assert_eq!(
            parts.text(),
            "Sounds good.\n\nAna\nACME\n\nBo <b@x.com> wrote:\n> Shall we?\n>\n> B.\n"
        );
        let html = parts.html(None);
        assert_eq!(
            html,
            "<div name=\"messageBodySection\">Sounds good.<br><br></div>\
             <div name=\"messageSignatureSection\"><div>Ana</div><div>ACME</div></div>\
             <div name=\"messageReplySection\"><br>Bo &lt;b@x.com&gt; wrote:<br><blockquote type=\"cite\" style=\"margin:0 0 0 .8ex;border-left:1px solid #ccc;padding-left:1ex\">Shall we?<br><br>B.</blockquote></div>"
        );
        let forward = Parts {
            body: "",
            signature: None,
            quote: Some(Quote::Plain("---------- Forwarded message ----------\nHi")),
        };
        assert_eq!(
            forward.text(),
            "---------- Forwarded message ----------\nHi"
        );
        assert!(
            forward
                .html(Some("font-size:11pt"))
                .starts_with("<div name=\"messageBodySection\" style=\"font-size:11pt\"></div>")
        );
    }

    #[test]
    fn sections_are_found_with_their_nested_divs() {
        let html = r#"<html><body><div name="messageBodySection">Hi<br></div><div name="messageSignatureSection"><div><div>Ana</div></div><div>ACME</div></div><DIV NAME="messageReplySection">q</DIV></body></html>"#;
        assert_eq!(
            section(html, SIGNATURE_SECTION),
            Some("<div><div>Ana</div></div><div>ACME</div>")
        );
        assert_eq!(section(html, REPLY_SECTION), Some("q"));
        assert_eq!(section(html, BODY_SECTION), Some("Hi<br>"));
        assert_eq!(section("<div>no sections</div>", SIGNATURE_SECTION), None);
    }

    #[test]
    fn replies_sign_like_the_last_message_sent_in_the_conversation() {
        let mut store = Store::open_in_memory().unwrap();
        let account = store
            .add_account(&spark_store::NewAccount {
                kind: spark_store::AccountKind::Imap,
                email: "me@x.com",
                name: "Me",
                label: "x",
                config: "{}",
            })
            .unwrap();
        let work = store
            .add_signature("<div>Ana Ruiz</div><div>ACME</div>", "Ana Ruiz\nACME", 1)
            .unwrap();
        let home = store.add_signature("<div>Ana</div>", "Ana", 2).unwrap();
        let mut add =
            |message_id: &str, in_reply_to: Option<&str>, from: &str, text: &str, date: i64| {
                let added = store
                    .add_incoming(
                        &spark_store::Incoming {
                            account_id: account.id,
                            uid: Some(date as u32),
                            mailbox: Some("INBOX".into()),
                            message_id: Some(message_id.into()),
                            in_reply_to: in_reply_to.map(Into::into),
                            from_address: from.into(),
                            subject: "Plan".into(),
                            date,
                            text: text.into(),
                            ..Default::default()
                        },
                        false,
                    )
                    .unwrap();
                match added {
                    spark_store::Added::New(id) | spark_store::Added::Known(id) => id,
                }
            };
        let first = add("1@x.com", None, "bo@x.com", "Shall we?", 10);
        add(
            "2@x.com",
            Some("1@x.com"),
            "me@x.com",
            "Yes.\n\nAna Ruiz ACME\n\n> Shall we?",
            20,
        );
        let latest = add("3@x.com", Some("2@x.com"), "bo@x.com", "Great.", 30);
        let other = add("4@x.com", None, "cy@x.com", "Hello", 40);
        // Ana is in both signatures; the longer one is the one she used.
        assert_eq!(
            for_reply(&store, "me@x.com", latest).unwrap(),
            Some(work.clone())
        );
        assert_eq!(
            for_reply(&store, "ME@x.com", first).unwrap(),
            Some(work.clone())
        );
        assert_eq!(
            for_reply(&store, "me@x.com", other).unwrap(),
            Some(home.clone()),
            "else the default"
        );
        store
            .set_default_signature("me@x.com", DefaultSignature::None)
            .unwrap();
        assert_eq!(for_reply(&store, "me@x.com", latest).unwrap(), None);
    }

    #[test]
    fn signature_text_is_found_however_its_lines_broke() {
        let hay = "Thanks!\n\nAna Ruiz Head of\nSales\n\n> quoted";
        let (start, end) = find_text(hay, "Ana Ruiz\nHead of Sales").unwrap();
        assert_eq!(&hay[start..end], "Ana Ruiz Head of\nSales");
        assert_eq!(
            find_text("AnaRuizHead ofSales", "Ana Ruiz\nHead of Sales"),
            Some((0, 19))
        );
        assert_eq!(find_text(hay, "Bo"), None);
        assert_eq!(find_text(hay, " \n"), None);
    }
}
