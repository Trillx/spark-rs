//! Attachments: describing a message's files for the store, taking one back
//! out of the raw message when the user opens or forwards it, and types for
//! files picked from disk.

use mail_parser::{Message, MessageParser, MessagePart, MimeHeaders};
use spark_store::Attachment;

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
        assert!(String::from_utf8(eml.bytes).unwrap().contains("Subject: Old thread"));
        // An older row listed plan.pdf at another position.
        assert_eq!(extract(raw, 5, "plan.pdf").unwrap().name, "plan.pdf");
        assert_eq!(extract(raw, 0, "missing.doc"), None);
    }

    #[test]
    fn types_follow_extensions() {
        assert_eq!(mime_for("Quote.PDF"), "application/pdf");
        assert_eq!(mime_for("part.step"), "model/step");
        assert_eq!(mime_for("no-extension"), "application/octet-stream");
    }
}
