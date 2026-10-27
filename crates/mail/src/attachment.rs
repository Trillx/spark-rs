//! Attachments: describing a message's files for the store, taking one back
//! out of the raw message when the user opens or forwards it, the images
//! its HTML embeds (`cid:` URLs), and types for files picked from disk.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use mail_parser::{Message, MessageParser, MessagePart, MimeHeaders};
use spark_store::Attachment;

/// Embedded images bigger than this are left out; together they stop at
/// `INLINE_TOTAL_MAX`. They travel to the UI as data URLs.
pub const INLINE_IMAGE_MAX: usize = 5 * 1024 * 1024;
pub const INLINE_TOTAL_MAX: usize = 20 * 1024 * 1024;

/// An attachment with its content.
#[derive(Clone, PartialEq, Eq)]
pub struct AttachmentData {
    pub name: String,
    pub mime: String,
    pub bytes: Vec<u8>,
}

impl std::fmt::Debug for AttachmentData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AttachmentData")
            .field("name", &self.name)
            .field("mime", &self.mime)
            .field("bytes", &self.bytes.len())
            .finish()
    }
}

/// An image the email's HTML shows with `<img src="cid:…">`: a part of the
/// message (an "inline attachment") named by its Content-ID.
#[derive(Clone, PartialEq, Eq)]
pub struct InlineImage {
    /// The Content-ID as `cid_key` gives it.
    pub cid: String,
    /// Always `image/…`.
    pub mime: String,
    pub bytes: Vec<u8>,
}

impl std::fmt::Debug for InlineImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InlineImage")
            .field("cid", &self.cid)
            .field("mime", &self.mime)
            .field("bytes", &self.bytes.len())
            .finish()
    }
}

impl InlineImage {
    /// Its MIME type if it's an image: `mime`, else the one `name` implies.
    pub fn image_type(mime: &str, name: Option<&str>) -> Option<String> {
        let mime = mime.trim().to_ascii_lowercase();
        if mime.starts_with("image/") {
            return Some(mime);
        }
        name.map(mime_for)
            .filter(|m| m.starts_with("image/"))
            .map(str::to_owned)
    }

    pub fn data_url(&self) -> String {
        format!("data:{};base64,{}", self.mime, STANDARD.encode(&self.bytes))
    }
}

/// A Content-ID in the form matching uses: without angle brackets, lowercase.
pub fn cid_key(content_id: &str) -> String {
    content_id
        .trim()
        .trim_start_matches('<')
        .trim_end_matches('>')
        .to_lowercase()
}

/// The Content-IDs (as `cid_key` gives them) that the HTML refers to with
/// `cid:` URLs, each once. The UI's `cidKey` reads them the same way.
pub fn cid_refs(html: &str) -> Vec<String> {
    let lower = html.to_ascii_lowercase();
    let mut refs: Vec<String> = Vec::new();
    for (at, _) in lower.match_indices("cid:") {
        // Only attribute values and CSS url()s, not "cid:" in the text.
        if !matches!(
            lower[..at].chars().next_back(),
            Some('"' | '\'' | '=' | '(')
        ) {
            continue;
        }
        let rest = &html[at + 4..];
        let end = rest
            .find(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | ')' | '>'))
            .unwrap_or(rest.len());
        let key = cid_key(&percent_decode(&rest[..end].replace("&amp;", "&")));
        if !key.is_empty() && !refs.contains(&key) {
            refs.push(key);
        }
    }
    refs
}

/// `%xx` escapes decoded, as in a `cid:` URL; anything else is kept.
pub(crate) fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |b: u8| (b as char).to_digit(16);
        if bytes[i] == b'%'
            && let (Some(&h), Some(&l)) = (bytes.get(i + 1), bytes.get(i + 2))
            && let (Some(h), Some(l)) = (hex(h), hex(l))
        {
            out.push((h * 16 + l) as u8);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The images in a raw message whose Content-IDs are in `wanted`, within
/// the size limits.
pub fn inline_images(raw: &[u8], wanted: &[String]) -> Vec<InlineImage> {
    let Some(m) = MessageParser::default().parse(raw) else {
        return Vec::new();
    };
    images_in(&m, wanted)
}

fn images_in(m: &Message, wanted: &[String]) -> Vec<InlineImage> {
    let mut images: Vec<InlineImage> = Vec::new();
    let mut total = 0;
    for part in &m.parts {
        let Some(cid) = part.content_id().map(cid_key) else {
            continue;
        };
        if !wanted.contains(&cid) || images.iter().any(|i| i.cid == cid) {
            continue;
        }
        let Some(mime) = InlineImage::image_type(&mime_of(part), part.attachment_name()) else {
            continue;
        };
        let bytes = part.contents();
        if bytes.is_empty()
            || bytes.len() > INLINE_IMAGE_MAX
            || total + bytes.len() > INLINE_TOTAL_MAX
        {
            continue;
        }
        total += bytes.len();
        images.push(InlineImage {
            cid,
            mime,
            bytes: bytes.to_vec(),
        });
    }
    images
}

/// The attachments of a parsed message as the store keeps them.
pub fn describe(m: &Message) -> Vec<Attachment> {
    listed(m)
        .map(|(name, mime, part)| Attachment {
            name,
            mime,
            size: part.len() as u64,
            remote_id: None,
        })
        .collect()
}

/// Takes attachment `index`, as `describe` lists them, out of a raw message.
/// `name` guards the position: rows stored by older versions listed only
/// named parts, so if the part at `index` is called something else, the
/// first part with this name is taken instead.
pub fn extract(raw: &[u8], index: usize, name: &str) -> Option<AttachmentData> {
    let m = MessageParser::default().parse(raw)?;
    let list: Vec<_> = listed(&m).collect();
    let (name, mime, part) = list
        .get(index)
        .filter(|(n, ..)| n == name)
        .or_else(|| list.iter().find(|(n, ..)| n == name))?;
    Some(AttachmentData {
        name: name.clone(),
        mime: mime.clone(),
        bytes: part.contents().to_vec(),
    })
}

/// Every attachment `names` lists (as `describe` does, in order) out of a
/// raw message, parsing it once: what `extract` gives for each.
pub fn extract_all(raw: &[u8], names: &[&str]) -> Vec<Option<AttachmentData>> {
    let Some(m) = MessageParser::default().parse(raw) else {
        return names.iter().map(|_| None).collect();
    };
    files_in(&m, names)
}

/// What `extract_all` gives for `names` and `inline_images` for `wanted`,
/// parsing the message once.
pub fn extract_files(
    raw: &[u8],
    names: &[&str],
    wanted: &[String],
) -> (Vec<Option<AttachmentData>>, Vec<InlineImage>) {
    let Some(m) = MessageParser::default().parse(raw) else {
        return (names.iter().map(|_| None).collect(), Vec::new());
    };
    (files_in(&m, names), images_in(&m, wanted))
}

fn files_in(m: &Message, names: &[&str]) -> Vec<Option<AttachmentData>> {
    let list: Vec<_> = listed(m).collect();
    names
        .iter()
        .enumerate()
        .map(|(index, &name)| {
            let (name, mime, part) = list
                .get(index)
                .filter(|(n, ..)| n == name)
                .or_else(|| list.iter().find(|(n, ..)| n == name))?;
            Some(AttachmentData {
                name: name.clone(),
                mime: mime.clone(),
                bytes: part.contents().to_vec(),
            })
        })
        .collect()
}

/// (name, MIME type, part) for each part shown as an attachment. Inline
/// images the HTML refers to aren't attachments; nor are unnamed text parts,
/// which are pieces of the body.
fn listed<'a>(m: &'a Message<'a>) -> impl Iterator<Item = (String, String, &'a MessagePart<'a>)> {
    m.attachments()
        .filter(|part| {
            !part
                .content_disposition()
                .is_some_and(|d| d.is_inline() && part.content_id().is_some())
        })
        .enumerate()
        .filter_map(|(i, part)| {
            let mime = mime_of(part);
            Some((name_of(part, &mime, i)?, mime, part))
        })
}

fn name_of(part: &MessagePart, mime: &str, index: usize) -> Option<String> {
    if let Some(name) = part
        .attachment_name()
        .map(str::trim)
        .filter(|n| !n.is_empty())
    {
        return Some(name.to_owned());
    }
    // A forwarded email attached whole.
    if let Some(inner) = part.message() {
        let subject = inner
            .subject()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("Message");
        return Some(format!("{subject}.eml"));
    }
    if mime.starts_with("text/") {
        return None;
    }
    Some(format!("Attachment {}{}", index + 1, extension_for(mime)))
}

fn mime_of(part: &MessagePart) -> String {
    if part.is_message() {
        return "message/rfc822".into();
    }
    part.content_type()
        .map(|ct| match ct.subtype() {
            Some(sub) => format!("{}/{sub}", ct.ctype()),
            None => ct.ctype().to_owned(),
        })
        .unwrap_or_else(|| "application/octet-stream".into())
        .to_ascii_lowercase()
}

/// The MIME type for a file name, by its extension.
pub fn mime_for(name: &str) -> &'static str {
    let ext = name
        .rsplit_once('.')
        .map(|(_, ext)| ext.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "pdf" => "application/pdf",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "heic" => "image/heic",
        "bmp" => "image/bmp",
        "tif" | "tiff" => "image/tiff",
        "svg" => "image/svg+xml",
        "txt" | "log" => "text/plain",
        "csv" => "text/csv",
        "htm" | "html" => "text/html",
        "md" => "text/markdown",
        "ics" => "text/calendar",
        "vcf" => "text/vcard",
        "json" => "application/json",
        "xml" => "application/xml",
        "rtf" => "application/rtf",
        "zip" => "application/zip",
        "7z" => "application/x-7z-compressed",
        "rar" => "application/vnd.rar",
        "gz" => "application/gzip",
        "tar" => "application/x-tar",
        "doc" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xls" => "application/vnd.ms-excel",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "ppt" => "application/vnd.ms-powerpoint",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "odt" => "application/vnd.oasis.opendocument.text",
        "ods" => "application/vnd.oasis.opendocument.spreadsheet",
        "dxf" => "image/vnd.dxf",
        "dwg" => "image/vnd.dwg",
        "step" | "stp" => "model/step",
        "igs" | "iges" => "model/iges",
        "stl" => "model/stl",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "m4a" => "audio/mp4",
        "mp4" => "video/mp4",
        "mov" => "video/quicktime",
        "eml" => "message/rfc822",
        _ => "application/octet-stream",
    }
}

/// An extension for an unnamed part, so it opens in the right app.
fn extension_for(mime: &str) -> &'static str {
    match mime {
        "application/pdf" => ".pdf",
        "image/png" => ".png",
        "image/jpeg" => ".jpg",
        "image/gif" => ".gif",
        "application/zip" => ".zip",
        "text/calendar" => ".ics",
        _ => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIXED: &str = "From: ana@x.com\r\n\
Subject: Files\r\n\
Content-Type: multipart/mixed; boundary=m\r\n\
\r\n\
--m\r\n\
Content-Type: multipart/related; boundary=r\r\n\
\r\n\
--r\r\n\
Content-Type: text/html\r\n\
\r\n\
<p>See <img src=\"cid:logo\"></p>\r\n\
--r\r\n\
Content-Type: image/png\r\n\
Content-Disposition: inline\r\n\
Content-ID: <logo>\r\n\
Content-Transfer-Encoding: base64\r\n\
\r\n\
iVBORw0KGgo=\r\n\
--r--\r\n\
--m\r\n\
Content-Type: application/pdf; name=\"plan.pdf\"\r\n\
Content-Disposition: attachment; filename=\"plan.pdf\"\r\n\
Content-Transfer-Encoding: base64\r\n\
\r\n\
JVBERi0xLjcK\r\n\
--m\r\n\
Content-Type: application/octet-stream\r\n\
Content-Disposition: attachment\r\n\
Content-Transfer-Encoding: base64\r\n\
\r\n\
AAEC\r\n\
--m\r\n\
Content-Type: message/rfc822\r\n\
\r\n\
From: bob@y.org\r\n\
Subject: Old thread\r\n\
\r\n\
Hello.\r\n\
--m--\r\n";

    #[test]
    fn lists_files_but_not_inline_images() {
        let m = MessageParser::default().parse(MIXED.as_bytes()).unwrap();
        let list = describe(&m);
        let names: Vec<&str> = list.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["plan.pdf", "Attachment 2", "Old thread.eml"]);
        assert_eq!(list[0].mime, "application/pdf");
        assert_eq!(list[0].size, 9, "decoded size");
        assert_eq!(list[2].mime, "message/rfc822");
    }

    #[test]
    fn extracts_by_position_or_by_name() {
        let raw = MIXED.as_bytes();
        let pdf = extract(raw, 0, "plan.pdf").unwrap();
        assert_eq!(pdf.bytes, b"%PDF-1.7\n");
        assert_eq!(pdf.mime, "application/pdf");
        assert_eq!(extract(raw, 1, "Attachment 2").unwrap().bytes, [0, 1, 2]);
        let eml = extract(raw, 2, "Old thread.eml").unwrap();
        assert!(
            String::from_utf8(eml.bytes)
                .unwrap()
                .contains("Subject: Old thread")
        );
        // An older row listed plan.pdf at another position.
        assert_eq!(extract(raw, 5, "plan.pdf").unwrap().name, "plan.pdf");
        assert_eq!(extract(raw, 0, "missing.doc"), None);
    }

    #[test]
    fn finds_the_cid_urls_the_html_uses() {
        let html = "<p>cid: in the text doesn't count.</p>\
            <img src=\"cid:image001.png@01DD62D9.84F42140\">\
            <img src='CID:Logo%20Big' alt=x>\
            <img src=cid:a&amp;b>\
            <td style=\"background:url(cid:bg)\">\
            <img src=\"cid:image001.png@01DD62D9.84F42140\">";
        assert_eq!(
            cid_refs(html),
            ["image001.png@01dd62d9.84f42140", "logo big", "a&b", "bg"]
        );
        assert_eq!(cid_key("<Logo@X>"), "logo@x");
        assert!(cid_refs("<p>No images.</p>").is_empty());
    }

    #[test]
    fn takes_the_wanted_inline_images_out_of_the_raw_message() {
        let raw = MIXED.as_bytes();
        let images = inline_images(raw, &["logo".to_owned()]);
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].cid, "logo");
        assert_eq!(images[0].mime, "image/png");
        assert_eq!(images[0].bytes, b"\x89PNG\r\n\x1a\n");
        assert_eq!(images[0].data_url(), "data:image/png;base64,iVBORw0KGgo=");
        assert!(inline_images(raw, &["other".to_owned()]).is_empty());
    }

    #[test]
    fn files_and_images_come_from_one_parse() {
        let raw = MIXED.as_bytes();
        let names = ["plan.pdf", "missing.doc", "Old thread.eml"];
        let wanted = ["logo".to_owned()];
        let (files, images) = extract_files(raw, &names, &wanted);
        assert_eq!(files, extract_all(raw, &names));
        assert_eq!(files[1], None);
        assert_eq!(images, inline_images(raw, &wanted));
        assert_eq!(images.len(), 1);
        assert_eq!(extract_files(b"", &names, &wanted), (vec![None; 3], vec![]));
    }

    #[test]
    fn inline_images_are_images() {
        assert_eq!(
            InlineImage::image_type("IMAGE/PNG", None).as_deref(),
            Some("image/png")
        );
        assert_eq!(
            InlineImage::image_type("application/octet-stream", Some("image001.jpg")).as_deref(),
            Some("image/jpeg")
        );
        assert_eq!(
            InlineImage::image_type("text/html", Some("page.html")),
            None
        );
        assert_eq!(InlineImage::image_type("", None), None);
    }

    #[test]
    fn types_follow_extensions() {
        assert_eq!(mime_for("Quote.PDF"), "application/pdf");
        assert_eq!(mime_for("part.step"), "model/step");
        assert_eq!(mime_for("no-extension"), "application/octet-stream");
    }
}
