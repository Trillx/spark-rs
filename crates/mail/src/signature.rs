//! Signatures and rich text in outgoing mail. A signature, like the body
//! from the rich editor, is HTML the user wrote or pasted, cleaned here
//! before it's kept or sent. Mail with either goes out as text and HTML. The
//! HTML is laid out in sections (body, signature, then the quoted original),
//! named the way Spark names them, which is also how a saved draft's body
//! and signature are found again.

use std::collections::{HashMap, HashSet};

use ammonia::{Builder, UrlRelative};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use spark_store::{DefaultSignature, Signature, Store};

use crate::attachment::{self, InlineImage};
use crate::parse::html_to_text;
use crate::trackers;

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

/// Keeps what signatures and bodies are made of (text formatting, inline
/// styles, links, images and tables) and drops the rest: scripts, styles sheets, event
/// handlers, forms and frames. Links keep http(s), mailto and tel; images
/// keep http(s), embedded raster images and `cid:` parts. Colors from
/// Outlook's dark mode are turned back (see [`undo_dark_mode`]).
pub fn sanitize(html: &str) -> String {
    clean(html, false)
}

/// The placeholders a template can hold (Spark's `TemplatePlaceholder`).
pub const PLACEHOLDERS: &[&str] = &[
    "custom",
    "recipientFirstName",
    "recipientLastName",
    "recipientFullName",
    "myFirstName",
    "myLastName",
    "myFullName",
];

/// A template's HTML: cleaned like a signature, keeping its placeholders,
/// `<span data-placeholder="…" data-label="…">`. Mail that's sent still
/// loses them ([`sanitize`]).
pub fn sanitize_template(html: &str) -> String {
    clean(html, true)
}

fn clean(html: &str, placeholders: bool) -> String {
    let mut tag_attributes: HashMap<&str, HashSet<&str>> = TAG_ATTRIBUTES
        .iter()
        .map(|(tag, attributes)| (*tag, attributes.iter().copied().collect()))
        .collect();
    if placeholders {
        tag_attributes.insert("span", ["data-placeholder", "data-label"].into());
    }
    Builder::default()
        .tags(TAGS.iter().copied().collect())
        .generic_attributes(GENERIC_ATTRIBUTES.iter().copied().collect())
        .tag_attributes(tag_attributes)
        .url_schemes(["http", "https", "mailto", "tel", "data", "cid"].into())
        .url_relative(UrlRelative::Deny)
        .link_rel(None)
        .attribute_filter(|element, attribute, value| {
            if attribute == "style" {
                return Some(undo_dark_mode(value).into());
            }
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
                ("span", "data-placeholder") => PLACEHOLDERS.contains(&value),
                _ => true,
            };
            keep.then(|| value.into())
        })
        .clean(html)
        .to_string()
        .trim()
        .to_owned()
}

/// The lightness (CIELAB) Outlook's dark mode gives white, its background:
/// #333's. Black text gets white's.
const DARK_BACKGROUND: f64 = 21.25;

/// An inline style with Outlook's dark mode taken out. A signature copied
/// from Outlook in dark mode comes with its text lightened and backgrounds
/// darkened, each color marked `!important`: on the light sheet a signature
/// is shown on, and most recipients read it on, black text comes out white.
/// Those colors get their lightness back, with the same hue. Only text that
/// would get darker and backgrounds that would get lighter are changed.
/// The signature editor does the same as it's pasted (`undoDarkMode`).
pub fn undo_dark_mode(style: &str) -> String {
    style
        .split(';')
        .map(|declaration| undo_dark_declaration(declaration).unwrap_or_else(|| declaration.into()))
        .collect::<Vec<_>>()
        .join(";")
}

fn undo_dark_declaration(declaration: &str) -> Option<String> {
    let (name, value) = declaration.split_once(':')?;
    let text = match name.trim().to_ascii_lowercase().as_str() {
        "color" => true,
        "background" | "background-color" => false,
        _ => return None,
    };
    let value = value.trim().to_ascii_lowercase();
    let color = value
        .strip_suffix("important")?
        .trim_end()
        .strip_suffix('!')?
        .trim_end();
    let (rgb, alpha) = parse_color(color)?;
    // Dark mode maps lightness L to (100 - L) scaled into DARK_BACKGROUND..100.
    let [dark, a, b] = lab(rgb);
    let light = 100.0 - (dark - DARK_BACKGROUND) / (1.0 - DARK_BACKGROUND / 100.0);
    ((light < dark) == text).then(|| {
        let [r, g, b] = rgb_from_lab([light, a, b]);
        match alpha {
            Some(alpha) => format!("{name}: rgba({r}, {g}, {b}, {alpha})"),
            None => format!("{name}: rgb({r}, {g}, {b})"),
        }
    })
}

/// `#rgb`, `#rrggbb`, `rgb()` or `rgba()`, with any alpha as it's written.
fn parse_color(color: &str) -> Option<([u8; 3], Option<&str>)> {
    if let Some(hex) = color.strip_prefix('#') {
        if !hex.bytes().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        let hex = match hex.len() {
            3 => hex.chars().flat_map(|c| [c, c]).collect(),
            6 => hex.to_owned(),
            _ => return None,
        };
        let n = u32::from_str_radix(&hex, 16).ok()?;
        return Some(([(n >> 16) as u8, (n >> 8) as u8, n as u8], None));
    }
    let args = color
        .strip_prefix("rgba(")
        .or_else(|| color.strip_prefix("rgb("))?
        .strip_suffix(')')?;
    let parts: Vec<&str> = args
        .split([',', '/', ' '])
        .filter(|p| !p.is_empty())
        .collect();
    let channel = |p: &str| {
        p.parse::<f64>()
            .ok()
            .map(|v| v.round().clamp(0.0, 255.0) as u8)
    };
    match parts[..] {
        [r, g, b] => Some(([channel(r)?, channel(g)?, channel(b)?], None)),
        [r, g, b, alpha] => Some(([channel(r)?, channel(g)?, channel(b)?], Some(alpha))),
        _ => None,
    }
}

/// sRGB (D65) to CIELAB.
fn lab(rgb: [u8; 3]) -> [f64; 3] {
    let [r, g, b] = rgb.map(|c| {
        let c = f64::from(c) / 255.0;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    });
    let x = (0.4124564 * r + 0.3575761 * g + 0.1804375 * b) / 0.95047;
    let y = 0.2126729 * r + 0.7151522 * g + 0.0721750 * b;
    let z = (0.0193339 * r + 0.1191920 * g + 0.9503041 * b) / 1.08883;
    let f = |t: f64| {
        if t > 216.0 / 24389.0 {
            t.cbrt()
        } else {
            (24389.0 / 27.0 * t + 16.0) / 116.0
        }
    };
    let (fx, fy, fz) = (f(x), f(y), f(z));
    [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
}

/// CIELAB to sRGB, clipped to what sRGB can show.
fn rgb_from_lab([l, a, b]: [f64; 3]) -> [u8; 3] {
    let fy = (l + 16.0) / 116.0;
    let f = |t: f64| {
        if t > 6.0 / 29.0 {
            t.powi(3)
        } else {
            (116.0 * t - 16.0) * 27.0 / 24389.0
        }
    };
    let (x, y, z) = (
        f(fy + a / 500.0) * 0.95047,
        f(fy),
        f(fy - b / 200.0) * 1.08883,
    );
    [
        3.2404542 * x - 1.5371385 * y - 0.4985314 * z,
        -0.9692660 * x + 1.8760108 * y + 0.0415560 * z,
        0.0556434 * x - 0.2040259 * y + 1.0572252 * z,
    ]
    .map(|c| {
        let c = if c <= 0.0031308 {
            12.92 * c
        } else {
            1.055 * c.powf(1.0 / 2.4) - 0.055
        };
        (c * 255.0).round().clamp(0.0, 255.0) as u8
    })
}

/// An original's HTML as a reply quotes it: cleaned, without its tracking
/// pixels, and without the images it carries as parts of its own (`cid:`),
/// which don't go along.
pub fn sanitize_quoted(html: &str) -> String {
    let clean = sanitize(&trackers::strip(html));
    let lower = clean.to_ascii_lowercase();
    let mut out = String::with_capacity(clean.len());
    let mut last = 0;
    for (at, _) in lower.match_indices("<img") {
        let Some(len) = lower[at..].find('>') else {
            break;
        };
        if at >= last && lower[at..at + len].contains("src=\"cid:") {
            out.push_str(&clean[last..at]);
            last = at + len + 1;
        }
    }
    out.push_str(&clean[last..]);
    out
}

/// The rich editor's body as plain text, for the text/plain version: like
/// [`text`], with each list item marked ("- ", or "1. " when numbered).
pub fn body_text(html: &str) -> String {
    text(&list_markers(html))
}

/// `html` with each list item's marker written in after its `<li>`.
fn list_markers(html: &str) -> String {
    // ASCII lowercasing keeps the byte offsets.
    let lower = html.to_ascii_lowercase();
    let mut out = String::with_capacity(html.len());
    // The lists the scan is in: `None` bulleted, else the items numbered so far.
    let mut lists: Vec<Option<usize>> = Vec::new();
    let (mut last, mut pos) = (0, 0);
    while let Some(i) = lower[pos..].find('<') {
        let at = pos + i;
        let Some(len) = lower[at..].find('>') else {
            break;
        };
        let end = at + len + 1;
        let name: String = lower[at + 1..]
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '/')
            .collect();
        match name.as_str() {
            "ul" => lists.push(None),
            "ol" => lists.push(Some(0)),
            "/ul" | "/ol" => {
                lists.pop();
            }
            "li" => {
                out.push_str(&html[last..end]);
                match lists.last_mut() {
                    Some(Some(n)) => {
                        *n += 1;
                        out.push_str(&format!("{n}. "));
                    }
                    _ => out.push_str("- "),
                }
                last = end;
            }
            _ => {}
        }
        pos = end;
    }
    out.push_str(&html[last..]);
    out
}

/// The signature as plain text, line by line: for the text/plain version,
/// and to name it in lists. Empty for one that's only an image.
pub fn text(html: &str) -> String {
    // html_to_text only breaks lines at <br> and </p>. A block's end breaks
    // the line too, unless it's already broken, and so does a list's start.
    const BREAK: char = '\u{e000}';
    let mut blocks = html.to_owned();
    for tag in [
        "<ul",
        "<ol",
        // A block that starts after text on its line (the editor's second
        // line, `Hello<div>world</div>`) starts a new one.
        "<div",
        "<p",
        "<li",
        "<h1",
        "<h2",
        "<h3",
        "<h4",
        "<h5",
        "<h6",
        "<blockquote",
        "<table",
        "<tr",
        "<center",
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

/// The other way round: pictures a message shows by `cid:` put back in as
/// `data:` URLs from its parts, `images`, as a draft from the server goes
/// back in the editor. Ones that aren't among them stay as they are.
pub fn inline_parts(html: &str, images: &[InlineImage]) -> String {
    let lower = html.to_ascii_lowercase();
    let mut out = String::with_capacity(html.len());
    let mut last = 0;
    for (at, _) in lower.match_indices("cid:") {
        // As `attachment::cid_refs` reads them: attribute values and url()s.
        if at < last
            || !matches!(
                lower[..at].chars().next_back(),
                Some('"' | '\'' | '=' | '(')
            )
        {
            continue;
        }
        let rest = &html[at + 4..];
        let end = rest
            .find(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | ')' | '>'))
            .unwrap_or(rest.len());
        let key = attachment::cid_key(&attachment::percent_decode(
            &rest[..end].replace("&amp;", "&"),
        ));
        if let Some(image) = images.iter().find(|i| i.cid == key) {
            out.push_str(&html[last..at]);
            out.push_str(&image.data_url());
            last = at + 4 + end;
        }
    }
    out.push_str(&html[last..]);
    out
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
    /// Plain text, unless there's `body_html`.
    pub body: &'a str,
    /// The body from the rich editor, as sanitized HTML.
    pub body_html: Option<&'a str>,
    /// Sanitized HTML.
    pub signature: Option<&'a str>,
    pub quote: Option<Quote<'a>>,
}

pub enum Quote<'a> {
    /// Text that goes out as it is (a forward's header and original).
    Plain(&'a str),
    /// A reply's original: `from` wrote `text`, and the HTML version
    /// quotes its `html` if it has one (see [`sanitize_quoted`]).
    Reply {
        from: &'a str,
        text: &'a str,
        html: Option<&'a str>,
    },
}

impl Quote<'_> {
    fn text(&self) -> String {
        match self {
            Self::Plain(text) => text.trim_end().to_owned(),
            Self::Reply { from, text, .. } => {
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
            Self::Reply { from, text, html } => format!(
                "{} wrote:<br><blockquote type=\"cite\" style=\"margin:0 0 0 .8ex;border-left:1px solid #ccc;padding-left:1ex\">{}</blockquote>",
                escape(from),
                html.map_or_else(|| escape(text.trim_end()), str::to_owned)
            ),
        }
    }
}

impl Parts<'_> {
    /// The text/plain version: the body, the signature's text, then the quote.
    pub fn text(&self) -> String {
        let body = match self.body_html {
            Some(html) => body_text(html),
            None => self.body.trim_end().to_owned(),
        };
        let mut parts = vec![body];
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
        let (body, empty) = match self.body_html {
            Some(body) => (
                body.trim().to_owned(),
                text(body).is_empty() && !body.contains("<img"),
            ),
            None => (
                escape(self.body.trim_end()),
                self.body.trim_end().is_empty(),
            ),
        };
        let mut html = format!("<div name=\"{BODY_SECTION}\"{style}>{body}");
        if !empty {
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
    fn colors_from_outlooks_dark_mode_are_turned_back() {
        // As copied from Outlook in dark mode: black text, a purple name and a blue link.
        let clean = sanitize(
            r#"<p style="margin: 0in; color: rgb(255, 255, 255) !important;"><span style="font-size: 14pt; color: rgb(234, 161, 255) !important;"><b>Ana Ruiz</b></span></p><a href="https://x.com" style="color: rgb(127, 174, 255) !important; text-decoration: none">x.com</a>"#,
        );
        assert!(
            clean.contains(r#"<p style="margin: 0in; color: rgb(0, 0, 0);">"#),
            "{clean}"
        );
        assert!(
            clean.contains(r#"style="font-size: 14pt; color: rgb(107, 42, 128);""#),
            "{clean}"
        );
        assert!(
            clean.contains(r#"style="color: rgb(0, 88, 160); text-decoration: none""#),
            "{clean}"
        );
        assert_eq!(sanitize(&clean), clean, "it's done once");
        assert_eq!(
            undo_dark_mode("background-color:#333333!important;color:#FFF !IMPORTANT"),
            "background-color: rgb(255, 255, 255);color: rgb(0, 0, 0)"
        );
        assert_eq!(
            undo_dark_mode("background: rgba(51, 51, 51, 0.5) !important"),
            "background: rgba(255, 255, 255, 0.5)"
        );
    }

    #[test]
    fn colors_only_turn_back_toward_a_light_sheet() {
        for style in [
            // Not from dark mode.
            "color: rgb(255, 255, 255)",
            // Dark text and light backgrounds read fine already.
            "color: #333 !important",
            "background-color: rgb(255, 255, 255) !important",
            // Not a color it knows.
            "color: white !important",
            "background: url(data:image/png;base64,AA==) !important",
        ] {
            assert_eq!(undo_dark_mode(style), style);
        }
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
            body_html: None,
            signature: Some("<div>Ana</div><div>ACME</div>"),
            quote: Some(Quote::Reply {
                from: "Bo <b@x.com>",
                text: "Shall we?\n\nB.",
                html: None,
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
            body_html: None,
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
    fn a_line_typed_after_the_first_is_a_line_of_its_own() {
        assert_eq!(body_text("Hello<div>world</div>"), "Hello\nworld");
        assert_eq!(body_text("Hi<p>there</p>"), "Hi\nthere");
        assert_eq!(text("<b>Ana</b> Ruiz<br>ACME"), "Ana Ruiz\nACME");
    }

    #[test]
    fn pictures_shown_by_cid_come_back_as_data() {
        let images = [InlineImage {
            cid: "logo@x".into(),
            mime: "image/png".into(),
            bytes: b"png".to_vec(),
        }];
        assert_eq!(
            inline_parts(
                r#"<img src="cid:Logo@x"> cid:logo@x <img src="cid:gone@x">"#,
                &images
            ),
            r#"<img src="data:image/png;base64,cG5n"> cid:logo@x <img src="cid:gone@x">"#
        );
    }

    #[test]
    fn rich_bodies_read_as_text_with_their_list_markers() {
        assert_eq!(
            body_text(
                "<div>Plan:</div><ol><li>Draft</li><li>Send<ul><li>to <b>Ana</b></li></ul></li></ol><div><br></div><div>Thanks&nbsp;all</div>"
            ),
            "Plan:\n1. Draft\n2. Send\n- to Ana\n\nThanks all"
        );
        let parts = Parts {
            body: "",
            body_html: Some("<div><br></div>"),
            signature: Some("<div>Ana</div>"),
            quote: None,
        };
        assert_eq!(parts.text(), "Ana");
        assert_eq!(
            parts.html(None),
            "<div name=\"messageBodySection\"><div><br></div></div><div name=\"messageSignatureSection\"><div>Ana</div></div>",
            "an empty body gets no gap"
        );
    }

    #[test]
    fn templates_keep_their_placeholders_and_mail_loses_them() {
        let html = r#"<div>Hi <span data-placeholder="recipientFirstName" data-label="Recipient's first name" onclick="x()">Recipient's first name</span>, <span data-placeholder="evil">x</span></div>"#;
        assert_eq!(
            sanitize_template(html),
            r#"<div>Hi <span data-placeholder="recipientFirstName" data-label="Recipient's first name">Recipient's first name</span>, <span>x</span></div>"#
        );
        assert_eq!(
            sanitize(html),
            "<div>Hi <span>Recipient's first name</span>, <span>x</span></div>"
        );
    }

    #[test]
    fn quoted_html_leaves_out_images_that_dont_go_along() {
        assert_eq!(
            sanitize_quoted(
                r#"<div>Hi<IMG SRC="CID:a@b" alt="logo"></div><img src="https://x/y.png"><script>x</script>"#
            ),
            r#"<div>Hi</div><img src="https://x/y.png">"#
        );
    }

    #[test]
    fn quoted_html_leaves_out_tracking_pixels() {
        // Forwarded, it would tell the sender each time the next reader opened it.
        assert_eq!(
            sanitize_quoted(
                r#"<p>Sale</p><img src="https://x.list-manage.com/track/open.php?u=1" height="1" width="1"><img src="https://x/y.png">"#
            ),
            r#"<p>Sale</p><img src="https://x/y.png">"#
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
