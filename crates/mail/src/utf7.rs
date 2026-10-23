//! IMAP's modified UTF-7 for mailbox names (RFC 3501 section 5.1.3):
//! printable ASCII stands for itself, `&` is `&-`, and anything else is
//! UTF-16 in base64 (with `,` for `/`) between `&` and `-`.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD_NO_PAD;

/// A mailbox name as the user sees it. Malformed runs are kept as they are.
pub fn decode(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut rest = name;
    while let Some(start) = rest.find('&') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let Some(end) = after.find('-') else {
            out.push_str(&rest[start..]);
            return out;
        };
        let run = &after[..end];
        if run.is_empty() {
            out.push('&');
        } else {
            match STANDARD_NO_PAD.decode(run.replace(',', "/")) {
                Ok(bytes) if bytes.len() % 2 == 0 => {
                    let units: Vec<u16> = bytes
                        .chunks(2)
                        .map(|pair| u16::from_be_bytes([pair[0], pair[1]]))
                        .collect();
                    out.push_str(&String::from_utf16_lossy(&units));
                }
                _ => out.push_str(&rest[start..start + end + 2]),
            }
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

/// A name the user typed, as the server wants it.
pub fn encode(name: &str) -> String {
    fn flush(units: &mut Vec<u16>, out: &mut String) {
        if units.is_empty() {
            return;
        }
        let bytes: Vec<u8> = units.iter().flat_map(|u| u.to_be_bytes()).collect();
        out.push('&');
        out.push_str(&STANDARD_NO_PAD.encode(bytes).replace('/', ","));
        out.push('-');
        units.clear();
    }
    let mut out = String::with_capacity(name.len());
    let mut units = Vec::new();
    for c in name.chars() {
        if (' '..='~').contains(&c) {
            flush(&mut units, &mut out);
            if c == '&' {
                out.push_str("&-");
            } else {
                out.push(c);
            }
        } else {
            let mut buf = [0u16; 2];
            units.extend_from_slice(c.encode_utf16(&mut buf));
        }
    }
    flush(&mut units, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip() {
        for (plain, wire) in [
            ("Entwürfe", "Entw&APw-rfe"),
            ("Tom & Jerry", "Tom &- Jerry"),
            ("日本語", "&ZeVnLIqe-"),
            ("Reçus 📎", "Re&AOc-us &2D3czg-"),
            ("INBOX/Sub", "INBOX/Sub"),
        ] {
            assert_eq!(encode(plain), wire, "{plain}");
            assert_eq!(decode(wire), plain, "{wire}");
        }
    }

    #[test]
    fn malformed_runs_are_left_alone() {
        assert_eq!(decode("A&B"), "A&B");
        assert_eq!(decode("A&!!-B"), "A&!!-B");
    }
}
