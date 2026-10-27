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
//!
//! The HTML is read as the message frame reads it (an HTML5 parser, scripting
//! off), so comments, `<style>`, entities and `srcset` count as they do there.

use std::borrow::Cow;

use dom_query::{Document, NodeRef};

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

/// `html` without its tracking pixels; as it was if it has none.
pub fn strip(html: &str) -> Cow<'_, str> {
    // Without an image there's nothing to parse. (`<image>` is read as `<img>`.)
    let images = html.as_bytes().windows(3).any(|w| {
        w[0] == b'<' && w[1].eq_ignore_ascii_case(&b'i') && w[2].eq_ignore_ascii_case(&b'm')
    });
    if !images {
        return Cow::Borrowed(html);
    }
    let doc = Document::from(html);
    let mut removed = false;
    for img in doc.select("img").nodes() {
        if is_tracker(img) {
            picture(img).as_ref().unwrap_or(img).remove_from_parent();
            removed = true;
        }
    }
    if removed {
        Cow::Owned(doc.html().to_string())
    } else {
        Cow::Borrowed(html)
    }
}

/// The `<picture>` an image is the fallback of, whose `<source>`s it loads from.
fn picture<'a>(img: &NodeRef<'a>) -> Option<NodeRef<'a>> {
    img.parent()
        .filter(|p| p.node_name().as_deref() == Some("picture"))
}

/// A remote image that tracks opens: loaded from an open-tracking address,
/// at most 1×1 pixels, or hidden by its own style with nothing to show it
/// again (no class or id for a stylesheet to reach it by, no real size).
fn is_tracker(img: &NodeRef) -> bool {
    let attr = |name: &str| img.attr(name).map(|v| v.to_string());
    let mut srcsets: Vec<String> = attr("srcset").into_iter().collect();
    if let Some(picture) = picture(img) {
        srcsets.extend(
            picture
                .children()
                .iter()
                .filter(|c| c.node_name().as_deref() == Some("source"))
                .filter_map(|c| c.attr("srcset").map(|v| v.to_string())),
        );
    }
    let sources: Vec<String> = attr("src")
        .as_deref()
        .and_then(remote)
        .into_iter()
        .chain(srcsets.iter().flat_map(|s| srcset_urls(s)))
        .collect();
    if sources.is_empty() {
        return false;
    }
    if sources.iter().any(|url| tracks_opens(url)) {
        return true;
    }
    let style = attr("style").unwrap_or_default();
    let width =
        css_length(&style, "width").or_else(|| attr("width").as_deref().and_then(attribute_length));
    let height = css_length(&style, "height")
        .or_else(|| attr("height").as_deref().and_then(attribute_length));
    let tiny = |side: Option<f64>| side.is_some_and(|px| px <= 1.0);
    if tiny(width) && tiny(height) {
        return true;
    }
    let small = |side: Option<f64>| side.is_none_or(|px| px <= 1.0);
    let hidden = img.has_attr("hidden") || hides(&style);
    hidden && !img.has_attr("class") && !img.has_attr("id") && small(width) && small(height)
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

/// The remote URLs a `srcset` offers. A comma inside a URL splits it here,
/// but its host and path start stay whole.
fn srcset_urls(srcset: &str) -> impl Iterator<Item = String> + '_ {
    srcset
        .split(|c: char| c.is_ascii_whitespace() || c == ',')
        .filter_map(remote)
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

/// The value a style gives a property, lowercase, as the cascade picks it:
/// the last `!important` one, else the last one.
fn css_value(style: &str, property: &str) -> Option<String> {
    let mut normal = None;
    let mut important = None;
    for (name, value) in style.split(';').filter_map(|d| d.split_once(':')) {
        if !name.trim().eq_ignore_ascii_case(property) {
            continue;
        }
        let value = value.trim().to_ascii_lowercase();
        match value.split_once('!') {
            Some((value, flag)) if flag.trim() == "important" => {
                important = Some(value.trim_end().to_owned())
            }
            Some((value, _)) => normal = Some(value.trim_end().to_owned()),
            None => normal = Some(value),
        }
    }
    important.or(normal)
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
        matches!(strip(html), Cow::Borrowed(_))
    }

    /// Whether `strip` takes out the image in `html` and keeps the text around it.
    fn stripped(html: &str) -> bool {
        let out = strip(html);
        Document::from(out.as_ref())
            .select("img, source")
            .is_empty()
            && out.contains("Before")
            && out.contains("After")
    }

    fn around(tag: &str) -> String {
        format!("<p>Before</p>{tag}<p>After</p>")
    }

    #[test]
    fn spark_s_one_pixel_images_go() {
        assert!(stripped(&around(
            r#"<img src="https://t.example.com/o.gif" width="1" height="1" border="0">"#
        )));
        assert!(stripped(&around(
            r#"<IMG height="1" SRC="http://t.example.com/o.gif" width="1"/>"#
        )));
        // Spark spares its own read receipts; here nothing is spared.
        assert!(stripped(&around(
            r#"<img src="https://sparkmailapp.com/r/x.gif" width="1" height="1">"#
        )));
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
            r#"<image src="https://t.example.com/o.gif" width="1" height="1">"#,
        ] {
            assert!(stripped(&around(tag)), "{tag}");
        }
    }

    #[test]
    fn important_wins_as_in_the_cascade() {
        // Shown at 600×300, whatever the later declarations say.
        assert!(kept(&around(
            r#"<img src="https://x.com/hero.jpg" style="width:600px!important;width:1px;height:300px !important;height:1px">"#
        )));
        assert!(stripped(&around(
            r#"<img src="https://t.example.com/o.gif" style="width:600px;width:1px!important;height:1px">"#
        )));
        assert!(kept(&around(
            r#"<img src="https://x.com/a.jpg" style="display:none;display:block!important">"#
        )));
    }

    #[test]
    fn hidden_images_go_unless_something_could_show_them() {
        for tag in [
            r#"<img src="https://t.example.com/o.gif" style="display:none">"#,
            r#"<img src="https://t.example.com/o.gif" style="visibility: hidden">"#,
            r#"<img src="https://t.example.com/o.gif" style="opacity:0">"#,
            r#"<img src="https://t.example.com/o.gif" hidden>"#,
        ] {
            assert!(stripped(&around(tag)), "{tag}");
        }
        // A picture a stylesheet reveals on narrow screens, by its class, or one with a real size.
        assert!(kept(&around(
            r#"<img class="mobile-only" src="https://x.com/hero.jpg" style="display:none">"#
        )));
        assert!(kept(&around(
            r#"<img src="https://x.com/hero.jpg" width="600" style="display:none">"#
        )));
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
            assert!(stripped(&around(&tag)), "{src}");
        }
        assert!(kept(
            r#"<img src="https://list-manage.com.example/track/open.php">"#
        ));
        assert!(kept(
            r#"<img src="https://notlist-manage.com/track/open.php">"#
        ));
    }

    #[test]
    fn every_source_an_image_loads_from_counts() {
        // The pixel only in `srcset`, or in the `<source>` of a `<picture>`.
        assert!(stripped(&around(
            r#"<img width="1" height="1" srcset="https://t.example.com/o.gif 1x">"#
        )));
        assert!(stripped(&around(
            r#"<img srcset="https://x.com/a.jpg 1x, https://t.example.com/wf/open?upn=1 2x">"#
        )));
        assert!(stripped(&around(
            r#"<picture><source srcset="https://t.example.com/o.gif"><img src="data:image/gif;base64,R0lGOD" width="1" height="1"></picture>"#
        )));
        assert!(kept(&around(
            r#"<img srcset="https://x.com/a.jpg 1x, https://x.com/a@2x.jpg 2x" width="300" height="200">"#
        )));
    }

    #[test]
    fn markup_is_read_as_the_frame_reads_it() {
        for tag in [
            // An entity in the scheme still makes a web address.
            r#"<img width="1" height="1" src="https&#58;//t.example.com/o.gif">"#,
            r#"<img width="1" height="1" src="&#x68;ttps://t.example.com/o.gif">"#,
            // `<!-->` is a whole (empty) comment; what follows it is real.
            r#"<!--><img src="https://t.example.com/o.gif" width="1" height="1">"#,
            // A quote mark in a style sheet doesn't open an attribute.
            r#"<style>/* <img alt=" */</style><img src="https://t.example.com/o.gif" width="1" height="1">"#,
            // With scripts off, as in the frame, <noscript> holds markup.
            r#"<noscript><img src="https://t.example.com/o.gif" width="1" height="1"></noscript>"#,
        ] {
            assert!(stripped(&around(tag)), "{tag}");
        }
        // What's in a comment never loads, and its quote mark doesn't swallow what follows.
        assert!(kept(
            r#"<!-- <img src="https://t.example.com/o" width=1 height=1 alt="it's --><p>Real</p><img src="https://x.com/p.png" alt="x">"#
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
            // The first of two `src` attributes is the one that loads.
            r#"<img src="https://x.com/p.png" src="https://t.example.com/wf/open">"#,
        ] {
            assert!(kept(&around(html)), "{html}");
        }
    }

    #[test]
    fn mail_without_trackers_comes_back_as_it_was() {
        let html =
            "<p>Café <imgur> 1 < 2 &amp; <img alt=\"a > b\" src=\"https://x.com/p.png\"></p>";
        assert!(matches!(strip(html), Cow::Borrowed(s) if s == html));
        assert!(kept("<p>No pictures at all</p>"));
        // A tag that never ends isn't an image.
        assert!(kept(
            r#"<p>x</p><img src="https://t.example.com/o" width="1" height="1""#
        ));
        let out = strip("<b>ü</b><img src=\"https://t.example.com/o\" width=1 height=1><i>é</i>");
        assert!(out.contains("<b>ü</b><i>é</i>"), "{out}");
    }
}
