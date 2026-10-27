//! An email as the prompts quote it.

use crate::mailbox::Message;

/// Headers and paragraphs, for the inside of an `<email>` block.
pub fn email_text(m: &Message) -> String {
    let subject = if m.subject.trim().is_empty() {
        "(no subject)"
    } else {
        m.subject.trim()
    };
    let body: Vec<&str> = m
        .body
        .iter()
        .flatten()
        .map(|p| p.trim())
        .filter(|p| !p.is_empty())
        .collect();
    format!(
        "From: {} <{}>\nDate: {}\nSubject: {subject}\n\n{}",
        m.from,
        m.address,
        utc_date(m.timestamp),
        body.join("\n\n")
    )
}

/// `2026-09-21 14:13 UTC` from Unix milliseconds, without a date library.
pub(crate) fn utc_date(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let (year, month, day) = civil_from_days(secs.div_euclid(86_400));
    let minutes = secs.rem_euclid(86_400) / 60;
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02} UTC",
        minutes / 60,
        minutes % 60
    )
}

/// Days since 1970-01-01 to a proleptic Gregorian date (Howard Hinnant's algorithm).
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use spark_store::{Category, MessageState};

    /// The email the prompt tests quote.
    pub fn message() -> Message {
        Message {
            id: 7,
            account: 2,
            category: Category::People,
            state: MessageState::Inbox,
            unread: true,
            pinned: false,
            priority: false,
            screening: false,
            snoozed_until: None,
            from: "Dana Whitfield".into(),
            address: "dana@example.com".into(),
            reply_to: Vec::new(),
            to: Vec::new(),
            timestamp: 1_790_000_000_000,
            subject: "Q4 budget review".into(),
            snippet: "Can you send".into(),
            body: Some(vec![
                "Hi,".into(),
                "  Can you send the revised numbers by Thursday?  ".into(),
                "".into(),
                "Thanks, Dana".into(),
            ]),
            attachment: None,
            folders: Vec::new(),
            folder: None,
            conversation: None,
            returned_at: None,
            muted: false,
            reminder: None,
            invite_kind: None,
            auto_label: None,
            auto_draft: None,
            content_rev: 0,
        }
    }

    #[test]
    fn quotes_headers_and_trimmed_paragraphs() {
        assert_eq!(
            email_text(&message()),
            "From: Dana Whitfield <dana@example.com>\n\
             Date: 2026-09-21 14:13 UTC\n\
             Subject: Q4 budget review\n\n\
             Hi,\n\nCan you send the revised numbers by Thursday?\n\nThanks, Dana"
        );
    }

    #[test]
    fn an_empty_subject_is_named() {
        let mut m = message();
        m.subject = " ".into();
        assert!(email_text(&m).contains("Subject: (no subject)\n"));
    }

    #[test]
    fn formats_utc_dates() {
        assert_eq!(utc_date(0), "1970-01-01 00:00 UTC");
        assert_eq!(utc_date(951_782_400_000), "2000-02-29 00:00 UTC");
        assert_eq!(utc_date(-1), "1969-12-31 23:59 UTC");
        assert_eq!(utc_date(1_790_000_000_000), "2026-09-21 14:13 UTC");
    }
}
