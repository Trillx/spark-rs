//! Sign-in code detection (`spark_mail::sign_in_code`) over a set of
//! sender-style emails, written for it rather than taken from anyone's mail:
//! 30 with a code to find and 30 without one (receipts, deliveries,
//! invoices, promotions, invites, security alerts, bookings, transfers).
//! Each fix to the heuristic keeps all of them right.

use serde::Deserialize;
use spark_mail::sign_in_code::find;

#[derive(Deserialize)]
struct Email {
    what: String,
    subject: String,
    text: String,
    code: Option<String>,
}

#[test]
fn finds_the_codes_and_only_them() {
    let emails: Vec<Email> = serde_json::from_str(include_str!("sign_in_codes.json")).unwrap();
    assert_eq!(emails.len(), 60);
    let wrong: Vec<String> = emails
        .iter()
        .filter_map(|e| {
            let found = find(&e.subject, &e.text, None);
            (found != e.code).then(|| format!("{}: expected {:?}, found {found:?}", e.what, e.code))
        })
        .collect();
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}
