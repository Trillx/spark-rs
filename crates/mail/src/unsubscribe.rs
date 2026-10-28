//! Leaving a mailing list, as its `List-Unsubscribe` header (RFC 2369)
//! offers: a one-click POST where `List-Unsubscribe-Post` says it takes one
//! (RFC 8058), else an email to the list's address, else a web page for the
//! user to finish on.

use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use serde::Serialize;

use crate::{Error, Result};

/// What a `List-Unsubscribe` header offers.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Options {
    /// An https link that unsubscribes when POSTed to.
    pub one_click: Option<String>,
    /// A `mailto:` link.
    pub mailto: Option<String>,
    /// A web page to unsubscribe on.
    pub web: Option<String>,
}

/// How unsubscribing went.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Unsubscribed {
    /// The list's one-click link took it.
    Done,
    /// The list wants an email; the caller sends it from the account.
    Email {
        to: String,
        subject: String,
        body: String,
    },
    /// Only a web page: the caller opens it.
    Page { url: String },
}

/// The links in a `List-Unsubscribe` header, with `List-Unsubscribe-Post`.
/// One-click needs https (RFC 8058).
pub fn options(list_unsubscribe: &str, post: Option<&str>) -> Options {
    let one_click = post.is_some_and(|p| {
        p.split_whitespace()
            .collect::<String>()
            .eq_ignore_ascii_case("List-Unsubscribe=One-Click")
    });
    let mut found = Options::default();
    let links = list_unsubscribe
        .split(',')
        .filter_map(|part| part.trim().strip_prefix('<')?.strip_suffix('>'))
        .map(|link| link.split_whitespace().collect::<String>());
    for link in links {
        let lower = link.to_ascii_lowercase();
        if lower.starts_with("mailto:") {
            found.mailto.get_or_insert(link);
        } else if lower.starts_with("https://") && one_click {
            found.one_click.get_or_insert(link);
        } else if lower.starts_with("https://") || lower.starts_with("http://") {
            found.web.get_or_insert(link);
        }
    }
    found
}

/// Whether a `List-Unsubscribe` header gives a way to leave that
/// `unsubscribe` can take: a web link, or a mailto with an address.
pub fn offered(list_unsubscribe: &str) -> bool {
    let found = options(list_unsubscribe, None);
    found.web.is_some() || found.mailto.as_deref().and_then(email).is_some()
}

/// Unsubscribes the best way `options` offers. A one-click link that fails
/// falls back to the others.
pub async fn unsubscribe(options: Options) -> Result<Unsubscribed> {
    let mut failed = None;
    if let Some(url) = &options.one_click {
        match one_click(url).await {
            Ok(()) => return Ok(Unsubscribed::Done),
            Err(e) => failed = Some(e),
        }
    }
    if let Some(mailto) = &options.mailto
        && let Some(email) = email(mailto)
    {
        return Ok(email);
    }
    if let Some(url) = options.web.or(options.one_click) {
        return Ok(Unsubscribed::Page { url });
    }
    Err(failed.unwrap_or_else(|| {
        Error::Config("This email doesn't say how to unsubscribe from it.".into())
    }))
}

/// RFC 8058's POST: no cookies or credentials, just the one field. A list
/// chooses where it goes, so it only goes over https to a public address,
/// fixed when the name is looked up (no redirects, no proxy): a newsletter
/// can't use it to reach this computer or the local network.
async fn one_click(url: &str) -> Result<()> {
    let refused = || Error::Config("The list's one-click unsubscribe link isn't usable.".into());
    let parsed = url::Url::parse(url).map_err(|_| refused())?;
    if parsed.scheme() != "https" || !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(refused());
    }
    let port = parsed.port_or_known_default().unwrap_or(443);
    let (host, addresses): (String, Vec<SocketAddr>) = match parsed.host() {
        Some(url::Host::Domain(domain)) => {
            let found = tokio::net::lookup_host((domain, port))
                .await
                .map_err(|e| Error::network(domain, e))?
                .collect();
            (domain.to_owned(), found)
        }
        Some(url::Host::Ipv4(ip)) => (ip.to_string(), vec![SocketAddr::new(ip.into(), port)]),
        Some(url::Host::Ipv6(ip)) => (ip.to_string(), vec![SocketAddr::new(ip.into(), port)]),
        None => return Err(refused()),
    };
    if addresses.is_empty() || !addresses.iter().all(|a| is_public(a.ip())) {
        return Err(Error::Config(format!(
            "{host} isn't on the public internet, so spark-rs won't unsubscribe there."
        )));
    }
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .resolve_to_addrs(&host, &addresses)
        .build()
        .map_err(|e| Error::network(&host, e))?;
    let response = client
        .post(parsed)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body("List-Unsubscribe=One-Click")
        .send()
        .await
        .map_err(|e| Error::network(&host, e))?;
    if response.status().is_success() {
        Ok(())
    } else {
        Err(Error::Server(format!(
            "{host} didn't take the unsubscribe request ({}).",
            response.status()
        )))
    }
}

/// Whether an address is on the public internet: not this computer, the
/// local network, link-local, shared (carrier NAT), multicast or reserved.
fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            !(v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_broadcast()
                || v4.is_documentation()
                || v4.is_unspecified()
                || v4.is_multicast()
                || a == 0
                || a >= 240
                || (a == 100 && (64..128).contains(&b)))
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_public(IpAddr::V4(v4));
            }
            let [first, second, ..] = v6.segments();
            !(v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (first & 0xfe00) == 0xfc00
                || (first & 0xffc0) == 0xfe80
                || (first & 0xffc0) == 0xfec0
                || (first == 0x2001 && second == 0x0db8))
        }
    }
}

/// A `mailto:` link as the email to send: its address, and the subject
/// and body it asks for ("unsubscribe" if none).
fn email(mailto: &str) -> Option<Unsubscribed> {
    let rest = &mailto["mailto:".len()..];
    let (address, query) = rest.split_once('?').unwrap_or((rest, ""));
    let to = decode(address);
    if !to.contains('@') {
        return None;
    }
    let mut subject = None;
    let mut body = None;
    for pair in query.split('&') {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        match key.to_ascii_lowercase().as_str() {
            "subject" => subject = Some(decode(value)),
            "body" => body = Some(decode(value)),
            _ => {}
        }
    }
    Some(Unsubscribed::Email {
        to,
        subject: subject
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "unsubscribe".into()),
        body: body.unwrap_or_else(|| "unsubscribe".into()),
    })
}

/// Percent-decoding as in URLs, but `+` stays a plus (addresses use it).
fn decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |b: u8| (b as char).to_digit(16);
        if bytes[i] == b'%'
            && let (Some(&a), Some(&b)) = (bytes.get(i + 1), bytes.get(i + 2))
            && let (Some(high), Some(low)) = (hex(a), hex(b))
        {
            out.push((high * 16 + low) as u8);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_click_needs_https_and_the_post_header() {
        let header = "<mailto:leave@list.example?subject=Leave%20me>, <https://list.example/u/1>";
        let with_post = options(header, Some("List-Unsubscribe=One-Click"));
        assert_eq!(
            with_post.one_click.as_deref(),
            Some("https://list.example/u/1")
        );
        assert_eq!(with_post.web, None);
        let without = options(header, None);
        assert_eq!(without.one_click, None);
        assert_eq!(without.web.as_deref(), Some("https://list.example/u/1"));
        assert!(without.mailto.is_some());
        // Plain http never posts.
        let http = options(
            "<http://list.example/u>",
            Some("List-Unsubscribe=One-Click"),
        );
        assert_eq!(
            (http.one_click, http.web.as_deref()),
            (None, Some("http://list.example/u"))
        );
    }

    #[test]
    fn folded_headers_and_junk_are_tolerated() {
        let found = options("<https://list.example/\r\n u/2>,  garbage, <ftp://x>", None);
        assert_eq!(found.web.as_deref(), Some("https://list.example/u/2"));
        assert_eq!(found.mailto, None);
    }

    #[test]
    fn mailto_links_become_the_email_they_ask_for() {
        assert_eq!(
            email("mailto:leave+42@list.example?subject=Leave%20me&body=bye"),
            Some(Unsubscribed::Email {
                to: "leave+42@list.example".into(),
                subject: "Leave me".into(),
                body: "bye".into(),
            })
        );
        assert_eq!(
            email("mailto:u%40list.example"),
            Some(Unsubscribed::Email {
                to: "u@list.example".into(),
                subject: "unsubscribe".into(),
                body: "unsubscribe".into(),
            })
        );
        assert_eq!(email("mailto:nobody"), None);
    }

    #[test]
    fn offered_needs_a_link_it_can_use() {
        assert!(offered("<https://list.example/u/1>"));
        assert!(offered("<http://list.example/u/1>"));
        assert!(offered("<mailto:leave@list.example>"));
        assert!(offered("<mailto:nobody>, <https://list.example/u>"));
        assert!(!offered("<mailto:nobody>"));
        assert!(!offered("<ftp://list.example/u>"));
        assert!(!offered("https://list.example/u"));
        assert!(!offered(""));
    }

    #[test]
    fn only_public_addresses_count() {
        for ip in [
            "127.0.0.1",
            "10.1.2.3",
            "192.168.0.1",
            "172.16.5.4",
            "169.254.1.1",
            "100.64.0.1",
            "0.0.0.0",
        ] {
            assert!(!is_public(ip.parse().unwrap()), "{ip}");
        }
        for ip in [
            "::1",
            "fd00::1",
            "fe80::1",
            "::ffff:127.0.0.1",
            "2001:db8::1",
        ] {
            assert!(!is_public(ip.parse().unwrap()), "{ip}");
        }
        for ip in ["93.184.216.34", "2606:4700::1111", "100.128.0.1"] {
            assert!(is_public(ip.parse().unwrap()), "{ip}");
        }
    }

    #[tokio::test]
    async fn one_click_refuses_private_hosts_and_credentials() {
        for url in [
            "https://127.0.0.1/unsubscribe",
            "https://[::1]/unsubscribe",
            "https://localhost/unsubscribe",
            "https://user:pw@list.example/unsubscribe",
            "http://list.example/unsubscribe",
        ] {
            assert!(
                matches!(one_click(url).await, Err(Error::Config(_))),
                "{url}"
            );
        }
    }

    #[tokio::test]
    async fn without_links_there_is_nothing_to_do() {
        assert!(unsubscribe(Options::default()).await.is_err());
        let page = unsubscribe(Options {
            web: Some("https://list.example/u".into()),
            ..Default::default()
        })
        .await
        .unwrap();
        assert_eq!(
            page,
            Unsubscribed::Page {
                url: "https://list.example/u".into()
            }
        );
    }
}
