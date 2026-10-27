//! Tracking pixels: tiny or hidden remote images whose only job is to tell
//! the sender when, where and on what an email was opened. They come out of
//! every email before it's shown, printed or quoted in a reply, whatever the
//! Load remote images setting says.
//!
//! Spark removes `<img>` tags whose `width` and `height` attributes are both
//! exactly `"1"`, except its own read receipts. This goes further: no
//! exceptions; sizes with units, unquoted or set in CSS, and 0×0 images;
//! images their own style hides; and the open-tracking addresses of common
//! mail services, whatever size they claim.

use std::borrow::Cow;

/// Open-tracking addresses: a host (and its subdomains, any host if empty)
/// and how the path starts, lowercase.
const OPEN_TRACKERS: &[(&str, &str)] = &[
    // Mailchimp and Mandrill
    ("list-manage.com", "/track/open.php"),
    ("mandrillapp.com", "/track/open.php"),
    // SendGrid, also on senders' own domains
    ("", "/wf/open"),
    // Amazon SES
    ("awstrack.me", "/i0/"),
    // HubSpot Sales
    ("", "/e1t/o/"),
    // Salesforce Marketing Cloud
    ("exct.net", "/open.aspx"),
    // Marketo
    ("", "/trk?t=1&mid="),
    // Oracle Eloqua
    ("", "/e/footerimages/footerimage"),
    // Constant Contact
    ("rs6.net", "/on.jsp"),
    // Mailjet
    ("mjt.lu", "/oo/"),
    // Brevo
    ("", "/tr/op/"),
    // Postmark
    ("pstmrk.it", "/open"),
    // Customer.io
    ("track.customer.io", "/e/o/"),
    // Read receipts: Mailtrack, Streak, Superhuman, Yesware
    ("mailtrack.io", "/trace/"),
    ("mailfoogae.appspot.com", "/"),
    ("r.superhuman.com", "/"),
    ("t.yesware.com", "/"),
];

/// `html` without its tracking pixels.
pub fn strip(html: &str) -> Cow<'_, str> {
    // ASCII lowercasing keeps the byte offsets.
    let lower = html.to_ascii_lowercase();
    let mut out = String::new();
    let (mut last, mut pos) = (0, 0);
    while let Some(i) = lower[pos..].find('<') {
        let at = pos + i;
        // What's in a comment never loads.
        if lower[at..].starts_with("<!--") {
            pos = lower[at + 4..]
                .find("-->")
                .map_or(lower.len(), |e| at + 4 + e + 3);
            continue;
        }
        let rest = &html[at + 1..];
        if !lower[at + 1..].starts_with("img")
            || !rest[3..].starts_with(|c: char| c.is_ascii_whitespace() || c == '/' || c == '>')
        {
            pos = at + 1;
            continue;
        }
        let rest = &rest[3..];
        let Some((attributes, len)) = Attributes::parse(rest) else {
            break;
        };
        let end = at + 4 + len;
        if attributes.is_tracker() {
            out.push_str(&html[last..at]);
            last = end;
        }
        pos = end;
    }
    if last == 0 {
        return Cow::Borrowed(html);
    }
    out.push_str(&html[last..]);
    Cow::Owned(out)
}

/// An `<img>` tag's attributes, names lowercase, values with their common
/// entities decoded.
struct Attributes(Vec<(String, String)>);

impl Attributes {
    /// The attributes of a tag from just after its name, and its length to
    /// the `>` that ends it; `None` if nothing does.
    fn parse(tag: &str) -> Option<(Self, usize)> {
        let b = tag.as_bytes();
        let mut list = Vec::new();
        let mut i = 0;
        loop {
            while i < b.len() && (b[i].is_ascii_whitespace() || b[i] == b'/') {
                i += 1;
            }
            match b.get(i) {
                None => return None,
                Some(b'>') => return Some((Self(list), i + 1)),
                _ => {}
            }
            let start = i;
            while i < b.len() && !b[i].is_ascii_whitespace() && !matches!(b[i], b'/' | b'>' | b'=')
            {
                i += 1;
            }
            if i == start {
                // A stray "=".
                i += 1;
                continue;
            }
            let name = tag[start..i].to_ascii_lowercase();
            let mut j = i;
            while j < b.len() && b[j].is_ascii_whitespace() {
                j += 1;
            }
            if b.get(j) != Some(&b'=') {
                list.push((name, String::new()));
                continue;
            }
            j += 1;
            while j < b.len() && b[j].is_ascii_whitespace() {
                j += 1;
            }
            let value = match *b.get(j)? {
                quote @ (b'"' | b'\'') => {
                    let len = tag[j + 1..].find(quote as char)?;
                    i = j + 1 + len + 1;
                    &tag[j + 1..j + 1 + len]
                }
                _ => {
                    let from = j;
                    while j < b.len() && !b[j].is_ascii_whitespace() && b[j] != b'>' {
                        j += 1;
                    }
                    i = j;
                    &tag[from..j]
                }
            };
            list.push((name, decode(value)));
        }
    }

    /// The first value given for `name`, as browsers take it.
    fn get(&self, name: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }

    fn has(&self, name: &str) -> bool {
        self.get(name).is_some()
    }

    /// A remote image that tracks opens: at an open-tracking address, at
    /// most 1×1 pixels, or hidden by its own style with nothing to show it
    /// again (no class or id for a stylesheet to reach it by, no real size).
    fn is_tracker(&self) -> bool {
        let Some(url) = self.get("src").and_then(remote) else {
            return false;
        };
        if tracks_opens(&url) {
            return true;
        }
        let style = self.get("style").unwrap_or_default();
        let width =
            css_length(style, "width").or_else(|| self.get("width").and_then(attribute_length));
        let height =
            css_length(style, "height").or_else(|| self.get("height").and_then(attribute_length));
        let tiny = |side: Option<f64>| side.is_some_and(|px| px <= 1.0);
        if tiny(width) && tiny(height) {
            return true;
        }
        let small = |side: Option<f64>| side.is_none_or(|px| px <= 1.0);
        let hidden = self.has("hidden") || hides(style);
        hidden && !self.has("class") && !self.has("id") && small(width) && small(height)
    }
}

/// The entities a URL or a style is likely to carry.
fn decode(value: &str) -> String {
    if !value.contains('&') {
        return value.into();
    }
    value
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&#x27;", "'")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

/// An http(s) or protocol-relative URL, lowercase and without its scheme.
fn remote(src: &str) -> Option<String> {
    // Browsers drop tabs and newlines anywhere in a URL.
    let url: String = src
        .trim()
        .chars()
        .filter(|c| !matches!(c, '\t' | '\n' | '\r'))
        .collect::<String>()
        .to_ascii_lowercase();
    ["https://", "http://", "//"]
        .iter()
        .find_map(|scheme| url.strip_prefix(scheme))
        .map(str::to_owned)
}

/// Whether a URL (lowercase, without its scheme) is an open-tracking address.
fn tracks_opens(url: &str) -> bool {
    let end = url.find(['/', '?', '#', '\\']).unwrap_or(url.len());
    let authority = &url[..end];
    let host = authority.rsplit('@').next().unwrap_or_default();
    let host = host.split(':').next().unwrap_or_default();
    let path = &url[end..];
    OPEN_TRACKERS.iter().any(|(domain, start)| {
        let on_host = domain.is_empty()
            || host == *domain
            || host
                .strip_suffix(*domain)
                .is_some_and(|sub| sub.ends_with('.'));
        on_host && path.starts_with(start)
    })
}

/// A `width` or `height` attribute in pixels, as browsers read it ("1px"
/// is 1); a percentage counts as large.
fn attribute_length(value: &str) -> Option<f64> {
    let value = value.trim_start();
    let digits = value
        .find(|c: char| !c.is_ascii_digit() && c != '.')
        .unwrap_or(value.len());
    let px: f64 = value[..digits].parse().ok()?;
    Some(if value[digits..].starts_with('%') {
        f64::INFINITY
    } else {
        px
    })
}

/// The last value a style gives a property, lowercase, without `!important`.
fn css_value(style: &str, property: &str) -> Option<String> {
    style
        .split(';')
        .filter_map(|declaration| declaration.split_once(':'))
        .rfind(|(name, _)| name.trim().eq_ignore_ascii_case(property))
        .map(|(_, value)| {
            let value = value.trim().to_ascii_lowercase();
            match value.find('!') {
                Some(bang) => value[..bang].trim_end().to_owned(),
                None => value,
            }
        })
}

/// A style's width or height in pixels. Other units, percentages and
/// keywords count as large, unless the length is 0.
fn css_length(style: &str, property: &str) -> Option<f64> {
    let value = css_value(style, property)?;
    let digits = value
        .find(|c: char| !c.is_ascii_digit() && c != '.')
        .unwrap_or(value.len());
    let Ok(number) = value[..digits].parse::<f64>() else {
        return Some(f64::INFINITY);
    };
    let unit = value[digits..].trim();
    Some(if number == 0.0 || unit.is_empty() || unit == "px" {
        number
    } else {
        f64::INFINITY
    })
}

/// Whether a style keeps its element from showing.
fn hides(style: &str) -> bool {
    let opacity = css_value(style, "opacity")
        .and_then(|v| v.trim_end_matches('%').parse::<f64>().ok())
        .is_some_and(|o| o == 0.0);
    css_value(style, "display").as_deref() == Some("none")
        || matches!(
            css_value(style, "visibility").as_deref(),
            Some("hidden" | "collapse")
        )
        || opacity
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kept(html: &str) -> bool {
        strip(html) == html
    }

    #[test]
    fn spark_s_one_pixel_images_go() {
        assert_eq!(
            strip(
                r#"<p>Hi</p><img src="https://t.example.com/o.gif" width="1" height="1" border="0"><p>Bye</p>"#
            ),
            "<p>Hi</p><p>Bye</p>"
        );
        assert_eq!(
            strip(r#"<IMG height="1" SRC="http://t.example.com/o.gif" width="1"/>"#),
            ""
        );
        // Spark spares its own read receipts; here nothing is spared.
        assert_eq!(
            strip(r#"<img src="https://sparkmailapp.com/r/x.gif" width="1" height="1">"#),
            ""
        );
    }

    #[test]
    fn sizes_spark_misses_count_too() {
        for tag in [
            r#"<img src="https://t.example.com/o" width=1 height=1>"#,
            r#"<img src='https://t.example.com/o' width='1px' height='1px'>"#,
            r#"<img width="0" height="0" class="mailtrack-img" src="https://t.example.com/o.png">"#,
            r#"<img src="//t.example.com/o.gif" style="width: 1px; height: 1px !important">"#,
            r#"<img src="https://t.example.com/o.gif" width="600" height="400" style="width:0;height:0">"#,
            "<img\nsrc=\"https://t.example.com/o.gif\"\nwidth=\"1\"\nheight=\"1\"\n>",
        ] {
            assert_eq!(strip(tag), "", "{tag}");
        }
    }

    #[test]
    fn hidden_images_go_unless_something_could_show_them() {
        assert_eq!(
            strip(r#"<img src="https://t.example.com/o.gif" style="display:none">"#),
            ""
        );
        assert_eq!(
            strip(r#"<img src="https://t.example.com/o.gif" style="visibility: hidden">"#),
            ""
        );
        assert_eq!(
            strip(r#"<img src="https://t.example.com/o.gif" style="opacity:0">"#),
            ""
        );
        assert_eq!(
            strip(r#"<img src="https://t.example.com/o.gif" hidden>"#),
            ""
        );
        // A picture a stylesheet reveals on narrow screens, by its class, or one with a real size.
        assert!(kept(
            r#"<img class="mobile-only" src="https://x.com/hero.jpg" style="display:none">"#
        ));
        assert!(kept(
            r#"<img src="https://x.com/hero.jpg" width="600" style="display:none">"#
        ));
    }

    #[test]
    fn open_tracking_addresses_go_whatever_their_size() {
        for src in [
            "https://x.us1.list-manage.com/track/open.php?u=1&amp;id=2&amp;e=3",
            "https://u123.ct.sendgrid.net/wf/open?upn=abc",
            "https://links.shop.example/wf/open?upn=abc",
            "https://abc.r.us-east-1.awstrack.me/I0/0100/xyz",
            "https://mktoweb.example/trk?t=1&amp;mid=abc",
            "https://mailtrack.io/trace/mail/abc.png?u=1",
            "https://r.superhuman.com/abc.gif",
        ] {
            let tag = format!(r#"<img src="{src}" width="600" height="300">"#);
            assert_eq!(strip(&tag), "", "{src}");
        }
        assert!(kept(
            r#"<img src="https://list-manage.com.example/track/open.php">"#
        ));
        assert!(kept(
            r#"<img src="https://notlist-manage.com/track/open.php">"#
        ));
    }

    #[test]
    fn pictures_stay() {
        for html in [
            r#"<img src="https://x.com/logo.png" width="120" height="40">"#,
            r#"<img src="https://x.com/photo.jpg">"#,
            // A spacer: one pixel wide, but tall.
            r#"<img src="https://x.com/spacer.gif" width="1" height="20">"#,
            r#"<img src="https://x.com/rule.gif" width="100%" height="1">"#,
            r#"<img src="https://x.com/a.gif" width="1" height="1" style="width:100%">"#,
            // Not remote, so nothing to track.
            r#"<img src="cid:dot@x" width="1" height="1">"#,
            r#"<img src="data:image/gif;base64,R0lGOD" width="1" height="1">"#,
            r#"<img width="1" height="1">"#,
        ] {
            assert!(kept(html), "{html}");
        }
    }

    #[test]
    fn the_rest_of_the_html_is_untouched() {
        let html =
            "<p>Café <imgur> 1 < 2 &amp; <img alt=\"a > b\" src=\"https://x.com/p.png\"></p>";
        assert!(matches!(strip(html), Cow::Borrowed(_)));
        assert_eq!(
            strip("<b>ü</b><img src=\"https://t.example.com/o\" width=1 height=1><i>é</i>"),
            "<b>ü</b><i>é</i>"
        );
        // A tag that never ends stays as it was.
        assert!(kept(
            r#"<p>x</p><img src="https://t.example.com/o" width="1" height="1""#
        ));
        // Nor is a comment looked into: its quote mark doesn't swallow what follows.
        assert!(kept(
            r#"<!-- <img src="https://t.example.com/o" width=1 height=1 alt="it's --><p>Real</p><img src="https://x.com/p.png" alt="x">"#
        ));
        // The first of two `src` attributes is the one that loads.
        assert!(kept(
            r#"<img src="https://x.com/p.png" src="https://t.example.com/wf/open">"#
        ));
    }
}
