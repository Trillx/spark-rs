//! Sign-in codes: the one-time code an email carries to prove it's you
//! ("Your verification code is 482913"), found as mail arrives so its
//! notification and the list can offer to copy it, as Gmail does.
//!
//! It's a guess from the words around the code, made on the device: no AI,
//! nothing sent anywhere. A miss only costs a copy button, while a wrong
//! find puts a number on the clipboard and can notify about mail that
//! wouldn't have, so when in doubt it finds nothing.

use mail_parser::decoders::html::html_to_text;

/// How much of an email's text is read, in bytes: codes come near the top.
const LOOK: usize = 6000;
/// How much of its HTML is read again when its text runs words together.
const LOOK_HTML: usize = 200_000;
/// How far after the word "code" its code may be, in characters.
const AFTER: usize = 100;
/// The same, for a code on a line of its own, as HTML emails show it big.
const AFTER_OWN_LINE: usize = 300;
/// How far before the word "code" it may be when the words between
/// introduce it ("482913 is your Instagram code").
const BEFORE: usize = 60;
/// How far before it may be otherwise ("482913. This code expires…").
const BEFORE_LOOSE: usize = 20;
/// How near a word naming a code the text must say it's about signing in,
/// when the subject doesn't: a "Sign in" link in a receipt's footer
/// doesn't make its order code one.
const ABOUT_NEAR: usize = 300;

/// Words that say an email is about signing in or proving who you are;
/// without one, a number near "code" is a door code, an order or an offer.
const ABOUT_SIGNING_IN: &[&str] = &[
    "verif",
    "sign in",
    "sign-in",
    "signin",
    "signing in",
    "log in",
    "login",
    "log-in",
    "logging in",
    "one-time",
    "one time",
    "otp",
    "passcode",
    "2fa",
    "mfa",
    "two-factor",
    "two factor",
    "2-step",
    "two-step",
    "authenticat",
    "security code",
    "do not share",
    "don't share",
    "never share",
    "confirm your",
    "identity",
    "access your account",
    "access your workspace",
    // Spanish, Portuguese, French, Italian, German, Dutch.
    "código",
    "vérif",
    "codice",
    "bestätig",
    "anmeld",
    "einmal",
    "inlog",
];

/// Words that say less: an account's terse code ("Your code is 751936. It
/// expires in 5 minutes.") has only these, but so do shops' offers ("Your
/// code is STYLE25"). With only these, only a number is taken.
const ABOUT_A_CODE: &[&str] = &[
    "your code is",
    "is your code",
    "enter the code",
    "enter this code",
    "use this code",
    "use the following code",
    "the code below",
];

/// Meeting invites carry passcodes and IDs that aren't for signing in.
const MEETINGS: &[&str] = &[
    "meeting id",
    "meeting-id",
    "conference id",
    "webinar id",
    "join zoom",
    "zoom meeting",
    "teams meeting",
    "join the meeting",
    "dial-in",
    "dial in",
];

/// Near a code, these make it a shop's. Banner lines ("Free shipping on
/// orders over £50") aren't: they sit under sign-in codes too.
const PROMOTIONS: &[&str] = &[
    "% off",
    "promo",
    "coupon",
    "discount",
    "voucher",
    "gift card",
    "checkout",
    "redeem",
    "referral",
];

/// Words that make "checkout" an offer's, and a bare number code a shop's.
const SAVINGS: &[&str] = &[
    "save", "saving", "savings", "price", "prices", "deal", "deals", "discount", "off", "treat",
    "birthday", "sale", "offer", "purchase",
];

/// Whom a code is handed to when it's not for signing in ("share this code
/// with your driver", "your driver will ask for it"). Mail that names them
/// near a code is about handing it over, however it words that.
const RECIPIENTS: &[&str] = &[
    "driver",
    "courier",
    "delivery partner",
    "delivery agent",
    "delivery associate",
    "delivery executive",
    "delivery person",
    "delivery boy",
    "delivery staff",
];
/// Words before a name that make it someone's, not an app's: "your Driver".
const DETERMINERS: &[&str] = &[
    "your", "the", "our", "a", "an", "my", "his", "her", "their", "this", "that",
];

/// After one of `RECIPIENTS`, these make it the user's own account: "your
/// driver account", "the courier portal", "driver's license".
const THEIR_OWN: &[&str] = &[
    "account",
    "app",
    "portal",
    "profile",
    "dashboard",
    "hub",
    "login",
    "log",
    "sign",
    "license",
    "licence",
];

/// Words ending in "code" that aren't one to type.
const NOT_CODES: &[&str] = &[
    "unicode",
    "barcode",
    "barcodes",
    "zipcode",
    "postcode",
    "decode",
    "decodes",
    "encode",
    "encodes",
    "opcode",
    "geocode",
    "transcode",
    "shortcode",
    "qrcode",
    "dresscode",
    "promocode",
    "promocodes",
    "pseudocode",
    "hardcode",
    "recode",
    "sourcecode",
];

/// "… code" that isn't for signing in.
const OTHER_CODES: &[&str] = &[
    "promo",
    "promotional",
    "promotion",
    "discount",
    "coupon",
    "voucher",
    "gift",
    "referral",
    "offer",
    "redemption",
    "zip",
    "postal",
    "post",
    "area",
    "country",
    "dialing",
    "source",
    "error",
    "status",
    "response",
    "tracking",
    "reference",
    "booking",
    "reservation",
    "order",
    "ticket",
    "product",
    "qr",
    "bar",
    "dress",
    "invite",
    "invitation",
    "sale",
    "bonus",
    "reward",
    "rewards",
    "tax",
    "swift",
    "sort",
    "routing",
    "branch",
    "airport",
    "color",
    "colour",
    "html",
    "sample",
    "example",
    "customer",
    "client",
    "member",
    "membership",
    "loyalty",
    "invoice",
    "billing",
    "pickup",
    "delivery",
    "parcel",
    "package",
    "shipment",
    "locker",
    "item",
    "bic",
    "iban",
];

/// Words before "code" (or "PIN") that make it one for signing in, whatever
/// comes before them: in "Your Shop verification code" the brand is no
/// qualifier, though "SWIFT BIC code" has one.
const SIGN_IN_CODES: &[&str] = &[
    "verification",
    "verify",
    "security",
    "login",
    "signin",
    "authentication",
    "auth",
    "passcode",
    "otp",
    "mfa",
    "fa",
    "temporary",
    "time",
    "onetime",
];

/// What a code is for that isn't signing in, however it's named: these
/// count two words back too ("order confirmation code", "delivery
/// verification code"), where a brand might stand ("Your Shop verification
/// code"), so they're only ones no sign-in code is named after.
const PURPOSES: &[&str] = &[
    "order",
    "delivery",
    "pickup",
    "parcel",
    "package",
    "shipment",
    "tracking",
    "locker",
    "booking",
    "reservation",
    "ticket",
    "invoice",
    "promo",
    "discount",
    "coupon",
    "voucher",
    "gift",
    "referral",
];

/// Currencies, as words before or after an amount ("INR 5000", "5000 USD").
const CURRENCIES: &[&str] = &[
    "inr", "rs", "rupees", "usd", "eur", "gbp", "cad", "aud", "nzd", "jpy", "cny", "rmb", "chf",
    "sek", "nok", "dkk", "pln", "czk", "huf", "rub", "brl", "mxn", "ars", "clp", "cop", "zar",
    "ngn", "kes", "egp", "aed", "sar", "qar", "ils", "sgd", "hkd", "twd", "krw", "idr", "php",
    "thb", "myr", "vnd", "pkr", "bdt", "lkr", "npr", "dollars", "euros", "pounds", "yen",
];

/// Words after a number that make it a quantity, not a code.
const UNITS: &[&str] = &[
    "minutes", "minute", "mins", "min", "hours", "hour", "hrs", "days", "day", "seconds", "secs",
    "sec", "years", "percent", "points", "pts", "items", "miles", "km", "kg", "lbs", "mb", "gb",
    "kb",
];

/// The code in an email with this subject, text and HTML, as the user
/// would type it: "G-482913" and "482 913" give "482913". This reads
/// anyone's mail as it syncs, so a slip in it finds nothing rather than
/// stopping the sync.
pub fn find(subject: &str, text: &str, html: Option<&str>) -> Option<String> {
    std::panic::catch_unwind(|| guess(subject, text, html))
        .ok()
        .flatten()
}

fn guess(subject: &str, text: &str, html: Option<&str>) -> Option<String> {
    let text = head(text, LOOK);
    let subject_lc = subject.to_ascii_lowercase();
    let text_lc = text.to_ascii_lowercase();
    let says = |lc: &str| about(lc) || about_a_code(lc);
    if !(says(&subject_lc) || says(&text_lc)) || MEETINGS.iter().any(|w| text_lc.contains(w)) {
        return None;
    }
    let numbers_only = !(about(&subject_lc) || about(&text_lc));
    // A subject naming a code for something else ("Your delivery
    // verification code") speaks for the email, whatever the text calls it.
    let named = anchors(&subject_lc);
    if named.iter().any(|a| a.other) && named.iter().all(|a| a.other) {
        return None;
    }
    // A subject about signing in vouches for the whole email; otherwise the
    // text has to say so near the code.
    let vouched = about(&subject_lc);
    // The likelier of the subject's and the text's, the subject's if they tie.
    [
        best(subject, &subject_lc, true, numbers_only),
        best(text, &text_lc, vouched, numbers_only),
    ]
    .into_iter()
    .flatten()
    .min_by_key(|&(score, _)| score)
    .or_else(|| {
        // HTML without spaces between its tags runs words together
        // ("code</td><td>482913" reads "code482913"): read it again
        // with a space at every tag.
        let spaced = spaced(html?);
        best(&spaced, &spaced.to_ascii_lowercase(), vouched, numbers_only)
    })
    .map(|(_, code)| code)
    // A code the subject gives is the text's too: if the text or the HTML
    // hands it over ("Your one-time password: 681492" … "share this code
    // with your driver") or offers it at checkout, it's not for signing in.
    .filter(|code| {
        !handed_over_in(text, code) && !html.is_some_and(|html| handed_over_in(&spaced(html), code))
    })
}

/// The start of `html` as text, with a space at every tag.
fn spaced(html: &str) -> String {
    let text = html_to_text(&head(html, LOOK_HTML).replace('<', " <"));
    head(&text, LOOK).to_owned()
}

/// Whether `src` makes `code` one to hand over or a shop's: where it shows
/// it ("681 492"), or where it talks of a code if it doesn't show it (an
/// offer then in the same sentence: "enter the code from the subject at
/// checkout", not "manage your gift cards" further on).
fn handed_over_in(src: &str, code: &str) -> bool {
    let lc = src.to_ascii_lowercase();
    let shown: Vec<(usize, usize)> = candidates(src)
        .into_iter()
        .filter(|c| c.code == code)
        .map(|c| (c.start, c.end))
        .collect();
    if shown.is_empty() {
        return anchors(&lc)
            .iter()
            .any(|a| offered_in_sentence(&lc, a.start) || handed_over(src, &lc, a.start, a.end));
    }
    shown
        .into_iter()
        .any(|(start, end)| promoted(&lc, start, end) || handed_over(src, &lc, start, end))
}

/// Whether `lc` (lower case) says it's about signing in.
fn about(lc: &str) -> bool {
    ABOUT_SIGNING_IN.iter().any(|w| lc.contains(w))
}

/// Whether `lc` (lower case) says only that it gives a code, maybe with a
/// name in it: "your Instagram code is", "is your Facebook code".
fn about_a_code(lc: &str) -> bool {
    if ABOUT_A_CODE.iter().any(|w| lc.contains(w)) {
        return true;
    }
    let words: Vec<&str> = lc
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    words
        .windows(4)
        .any(|w| matches!(w, ["your", _, "code", "is"] | ["is", "your", _, "code"]))
}

/// The start of `s`, up to `max` bytes, cut at a character.
fn head(s: &str, max: usize) -> &str {
    &s[..floor(s, max)]
}

fn floor(s: &str, mut i: usize) -> usize {
    if i >= s.len() {
        return s.len();
    }
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn ceil(s: &str, mut i: usize) -> usize {
    if i >= s.len() {
        return s.len();
    }
    while !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

/// A word that names a code: "code", "passcode", "Bestätigungscode", "OTP".
#[derive(Debug)]
struct Anchor {
    start: usize,
    end: usize,
    /// For something else: "order confirmation code", "customer code". Its
    /// number isn't one to sign in with, whatever other words name it.
    other: bool,
}

/// Something that could be the code, by byte position in the text.
#[derive(Debug)]
struct Candidate {
    start: usize,
    end: usize,
    /// As the user would type it.
    code: String,
    /// Letters and digits ("2F4KD"), which ordinary text has more of than
    /// codes, so they need the words right before them to say so.
    mixed: bool,
    /// Four digits that could be a year.
    year: bool,
}

/// The best candidate in `src` (`lc` is it in ASCII lower case), the one
/// closest to a word that names a code, with its score (lower is likelier).
/// Unless `vouched`, only words with talk of signing in near them count.
/// With `numbers_only` (the email only says it gives a code), "your code is"
/// counts as that talk, and only numbers are taken.
fn best(src: &str, lc: &str, vouched: bool, numbers_only: bool) -> Option<(usize, String)> {
    let (others, mut anchors): (Vec<Anchor>, Vec<Anchor>) =
        anchors(lc).into_iter().partition(|a| a.other);
    if !vouched {
        anchors.retain(|a| {
            let near =
                &lc[floor(lc, a.start.saturating_sub(ABOUT_NEAR))..ceil(lc, a.end + ABOUT_NEAR)];
            about(near) || numbers_only && about_a_code(near)
        });
    }
    if anchors.is_empty() {
        return None;
    }
    let mut best: Option<(usize, &Candidate)> = None;
    let candidates = candidates(src);
    for c in &candidates {
        if numbers_only && (c.mixed || saving_near(lc, c.start, c.end))
            || promoted(lc, c.start, c.end)
            || handed_over(src, lc, c.start, c.end)
        {
            continue;
        }
        let Some(likely) = anchors.iter().filter_map(|a| score(src, lc, a, c)).min() else {
            continue;
        };
        // The number an "order confirmation code" names stays its, though a
        // later "use this code" names it too.
        if let Some(other) = others.iter().filter_map(|a| score(src, lc, a, c)).min()
            && other <= likely
        {
            continue;
        }
        if best.is_none_or(|(s, _)| likely < s) {
            best = Some((likely, c));
        }
    }
    best.map(|(score, c)| (score, c.code.clone()))
}

/// How far `c` is from anchor `a`, lower being likelier; `None` if it's
/// too far, or the words between don't fit.
fn score(src: &str, lc: &str, a: &Anchor, c: &Candidate) -> Option<usize> {
    if c.start >= a.end {
        let gap = &lc[a.end..c.start];
        let far = gap.chars().count();
        // "Code: 482913", "code is 482913".
        if far <= 30
            && matches!(
                letters(gap).as_str(),
                "" | "is" | "es" | "est" | "ist" | "é" | "è"
            )
        {
            return Some(0);
        }
        let own_line = own_line(src, c);
        if far > if own_line { AFTER_OWN_LINE } else { AFTER } {
            return None;
        }
        // Past the end of a sentence it's something else, unless it stands
        // on its own, as codes are shown.
        if !own_line && (c.mixed || c.year || ends_sentence(gap)) {
            return None;
        }
        // "Your passcode for a payment of 5000 is 482913": the number the
        // sentence gives as its answer beats one nearer the word.
        if introduces(gap) {
            return Some(5 + far / 10);
        }
        Some(far)
    } else if c.end <= a.start {
        let gap = &lc[c.end..a.start];
        let far = gap.chars().count();
        let words = gap.trim_start();
        // "482913 is your Instagram code", "Use 482913 as your login code".
        if far <= BEFORE
            && ["is ", "as ", "es ", "est ", "ist ", "è "]
                .iter()
                .any(|w| words.starts_with(w))
            && !ends_sentence(gap)
        {
            return Some(1);
        }
        if far > BEFORE_LOOSE || c.mixed || c.year || ends_sentence(gap) {
            return None;
        }
        Some(far + 20)
    } else {
        None
    }
}

/// Whether `gap` ends by giving what comes next: "… is", "…:".
fn introduces(gap: &str) -> bool {
    let gap = gap.trim_end();
    gap.ends_with(':')
        || gap
            .rsplit(|c: char| !c.is_alphabetic())
            .next()
            .is_some_and(|w| {
                matches!(
                    w,
                    "is" | "are" | "es" | "est" | "ist" | "lautet" | "è" | "é"
                )
            })
}

/// Only the letters of `s`.
fn letters(s: &str) -> String {
    s.chars().filter(|c| c.is_alphabetic()).collect()
}

/// Whether `gap` crosses the end of a sentence.
fn ends_sentence(gap: &str) -> bool {
    let mut chars = gap.chars().peekable();
    while let Some(c) = chars.next() {
        if matches!(c, '.' | '!' | '?') && chars.peek().is_some_and(|n| n.is_whitespace()) {
            return true;
        }
    }
    false
}

/// Whether `c` is all its line holds.
fn own_line(src: &str, c: &Candidate) -> bool {
    let start = src[..c.start].rfind('\n').map_or(0, |i| i + 1);
    let end = src[c.end..].find('\n').map_or(src.len(), |i| c.end + i);
    src[start..c.start].trim().is_empty() && src[c.end..end].trim().is_empty()
}

/// Whether saving words are near `start..end` of `lc`: with only "your code
/// is" to go on, "Your code is 846275 … a special birthday price" is a
/// shop's.
fn saving_near(lc: &str, start: usize, end: usize) -> bool {
    lc[floor(lc, start.saturating_sub(150))..ceil(lc, end + 150)]
        .split(|c: char| !c.is_alphanumeric())
        .any(|w| SAVINGS.contains(&w))
}

/// Whether the words near `start..end` of `lc` make what's there a shop's code.
fn promoted(lc: &str, start: usize, end: usize) -> bool {
    let from = floor(lc, start.saturating_sub(150));
    let near = &lc[from..ceil(lc, end + 150)];
    PROMOTIONS
        .iter()
        .any(|w| near.match_indices(w).any(|(at, _)| offer(lc, from + at, w)))
}

/// Whether offer word `w` at `at` of `lc` offers something. "Checkout" is
/// where an offer's code goes, unless the sentence is about signing in or
/// what it gives ("faster checkout") and saves nothing: "enter code 846291
/// at checkout to sign in to Shop Pay", not "sign in and use code SPEED25
/// in our express checkout to save".
fn offer(lc: &str, at: usize, w: &str) -> bool {
    if w != "checkout" {
        return true;
    }
    let line = sentence(lc, at);
    let benefit = [
        "faster",
        "quicker",
        "quick",
        "express",
        "speedy",
        "easier",
        "seamless",
        "accelerated",
    ]
    .iter()
    .any(|b| lc[..at].trim_end().ends_with(b));
    let saves = line
        .split(|c: char| !c.is_alphanumeric())
        .any(|w| SAVINGS.contains(&w));
    saves || !(benefit || about(line))
}

/// Whether the sentence around `at` of `lc`, or the one after it going on
/// about the code ("Use the code in the subject. Redeem it for 30% off"), is
/// an offer's.
fn offered_in_sentence(lc: &str, at: usize) -> bool {
    let offered = |line: &str| {
        let from = line.as_ptr() as usize - lc.as_ptr() as usize;
        PROMOTIONS
            .iter()
            .any(|w| line.match_indices(w).any(|(i, _)| offer(lc, from + i, w)))
    };
    let line = sentence(lc, at);
    let end = line.as_ptr() as usize - lc.as_ptr() as usize + line.len();
    // Past the sentence's end and the spaces and breaks after it.
    let after = lc[end..]
        .find(|c: char| !matches!(c, '.' | '!' | '?') && !c.is_whitespace())
        .map(|i| end + i);
    // The next sentence counts when it goes on about the code: "Redeem it
    // for 30% off", not "Manage your gift cards in your account".
    let about_it = |next: &str| {
        next.split(|c: char| !c.is_alphanumeric())
            .any(|w| matches!(w, "it" | "code" | "codes"))
    };
    offered(line)
        || after.is_some_and(|after| {
            let next = sentence(lc, after);
            about_it(next) && offered(next)
        })
}

/// Whether a driver, courier or delivery person is named near `start..end`
/// of `src` (`lc` is it in lower case), making what's there a code to hand
/// over. Not when it's the user: their own account ("sign in to your driver
/// account"), what they sign in as ("Dear valued delivery partner", "sign
/// in as a DoorDash driver"), or the app they sign in to (`app_name`).
fn handed_over(src: &str, lc: &str, start: usize, end: usize) -> bool {
    let from = floor(lc, start.saturating_sub(150));
    let near = &lc[from..ceil(lc, end + 150)];
    RECIPIENTS.iter().any(|r| {
        near.match_indices(r).any(|(at, _)| {
            let at = from + at;
            let next = next_word(lc, at + r.len());
            !(next.is_some_and(|w| THEIR_OWN.contains(&w))
                || as_the_user(lc, at)
                || app_name(src, lc, at, next))
        })
    })
}

/// The word of `lc` from position `at` on, past a possessive ("driver's
/// license" gives "license").
fn next_word(lc: &str, at: usize) -> Option<&str> {
    let rest = &lc[at..];
    let mut words = rest
        .split(|c: char| !c.is_alphabetic())
        .filter(|w| !w.is_empty());
    match words.next() {
        Some("s") if rest.starts_with('\'') || rest.starts_with('’') => words.next(),
        w => w,
    }
}

/// Whether the recipient at `at` (followed by word `next`) is part of an
/// app's name the user signs in to: capitalized after a brand ("Uber
/// Driver", not "your Driver"), and either naming a code ("Uber Driver
/// verification code") or what's signed in to or opened ("sign in to Bolt
/// Driver", "Welcome back to Uber Driver", "Open Bolt Driver").
fn app_name(src: &str, lc: &str, at: usize, next: Option<&str>) -> bool {
    if !src[at..].starts_with(|c: char| c.is_ascii_uppercase()) {
        return false;
    }
    let lead = src[..at].trim_end();
    let brand_at = lead
        .char_indices()
        .rev()
        .find(|&(_, c)| !c.is_alphanumeric())
        .map_or(0, |(i, c)| i + c.len_utf8());
    let brand = &lead[brand_at..];
    let branded = brand.starts_with(|c: char| c.is_ascii_uppercase())
        && !DETERMINERS.contains(&brand.to_ascii_lowercase().as_str());
    let names_a_code = next.is_some_and(|w| {
        SIGN_IN_CODES.contains(&w) || matches!(w, "code" | "otp" | "pin" | "passcode")
    });
    let signed_in_to = {
        let before = lc[..brand_at].trim_end();
        [
            "sign in to",
            "log in to",
            "login to",
            "sign into",
            "log into",
            "signing in to",
            "logging in to",
            "welcome to",
            "welcome back to",
            "open",
            "opening",
        ]
        .iter()
        .any(|w| before.ends_with(w))
    };
    branded && (names_a_code || signed_in_to)
}

/// Whether the words before position `at` of `lc` make what's there the
/// user: a greeting opening its line ("Dear valued delivery partner",
/// "Hello, delivery partner!", not "Say hello to your driver" or "Hi Sam,
/// your driver …"), or "as a …", "as a DoorDash …" in its clause.
fn as_the_user(lc: &str, at: usize) -> bool {
    let line = &lc[lc[..at].rfind(['\n', '.', '!', '?']).map_or(0, |i| i + 1)..at];
    let words: Vec<&str> = line
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    // A comma may follow the greeting ("Hello, delivery partner"), not a
    // name after it ("Hi Sam, driver Alex …" greets Sam).
    let to_someone = line
        .trim_start()
        .split_once(|c: char| !c.is_alphanumeric())
        .is_some_and(|(_, rest)| {
            rest.split_once(',')
                .is_some_and(|(name, _)| !name.trim().is_empty())
        });
    let greeted = words.len() <= 3
        && words
            .first()
            .is_some_and(|w| matches!(*w, "dear" | "hi" | "hello" | "hey"))
        && !words[1..].iter().any(|w| DETERMINERS.contains(w))
        && !to_someone;
    let clause = &line[line.rfind([',', ';']).map_or(0, |i| i + 1)..];
    let words: Vec<&str> = clause.split_whitespace().collect();
    let recent = &words[words.len().saturating_sub(3)..];
    greeted
        || recent
            .windows(2)
            .any(|pair| pair[0] == "as" && matches!(pair[1], "a" | "an"))
}

/// The sentence of `lc` around position `at`.
fn sentence(lc: &str, at: usize) -> &str {
    let ends = |c: char| matches!(c, '.' | '!' | '?' | '\n');
    let start = lc[..at].rfind(ends).map_or(0, |i| i + 1);
    let end = lc[at..].find(ends).map_or(lc.len(), |i| at + i);
    &lc[start..end]
}

/// The words in `lc` that name a code.
fn anchors(lc: &str) -> Vec<Anchor> {
    let mut words: Vec<(usize, usize)> = Vec::new();
    let mut start = None;
    for (i, ch) in lc.char_indices() {
        match (ch.is_alphabetic(), start) {
            (true, None) => start = Some(i),
            (false, Some(s)) => {
                words.push((s, i));
                start = None;
            }
            _ => {}
        }
    }
    if let Some(s) = start {
        words.push((s, lc.len()));
    }
    let word = |j: usize| &lc[words[j].0..words[j].1];
    // The word before word `k` in the same phrase: only a space, a hyphen or
    // so between them, or with `strict` only spaces and hyphens.
    let previous = |k: usize, strict: bool| {
        let j = k.checked_sub(1)?;
        let gap = &lc[words[j].1..words[k].0];
        let joined = if strict {
            gap.chars().all(|c| c.is_whitespace() || c == '-')
        } else {
            gap.len() <= 3
        };
        joined.then_some(j)
    };
    // Whether word `j` makes a code one for signing in: "verification",
    // "one-time", "sign-in".
    let sign_in = |j: Option<usize>| {
        j.is_some_and(|j| {
            SIGN_IN_CODES.contains(&word(j))
                || word(j) == "in"
                    && previous(j, true).is_some_and(|i| matches!(word(i), "sign" | "log"))
        })
    };
    let mut anchors = Vec::new();
    for (k, &(start, end)) in words.iter().enumerate() {
        let here = word(k);
        let before = previous(k, false);
        // And the one before that: "order confirmation code".
        let two_before = before.and_then(|j| previous(j, true));
        let is = |j: Option<usize>, list: &[&str]| j.is_some_and(|j| list.contains(&word(j)));
        let names_a_code = if here.ends_with("code") || here.ends_with("codes") {
            !NOT_CODES.contains(&here)
        } else {
            matches!(
                here,
                "otp"
                    | "passcode"
                    | "passcodes"
                    | "código"
                    | "codigo"
                    | "códigos"
                    | "codice"
                    | "kod"
                    | "kode"
                    | "kód"
            ) || (here == "password" && is(before, &["time", "temporary", "onetime"]))
                // A PIN says it's for signing in: "one-time PIN", "sign-in PIN".
                || (here == "pin" && sign_in(before))
        };
        let other_before = match before.map(word) {
            // In India a PIN code is a postal code, and hotels give PIN codes
            // for doors, unless it's a "one-time PIN code".
            Some("pin") => !sign_in(two_before),
            _ => is(before, OTHER_CODES),
        };
        if names_a_code {
            anchors.push(Anchor {
                start,
                end,
                other: other_before
                    || is(two_before, PURPOSES)
                    || is(two_before, OTHER_CODES) && !sign_in(before),
            });
        }
    }
    anchors
}

/// Everything in `src` shaped like a code: 4–8 digits, maybe in groups
/// ("482 913", "4829-1357") or one a box ("4 8 2 9 1 3"), maybe after a
/// letter or two ("G-482913"); or 4–10 capitals and digits ("2F4KD",
/// "4K7-9QX"). Dates, times, amounts, phone numbers and the numbers in
/// links aren't.
fn candidates(src: &str) -> Vec<Candidate> {
    let bytes = src.as_bytes();
    // Runs of ASCII letters and digits.
    let mut runs: Vec<(usize, usize)> = Vec::new();
    let mut start = None;
    for (i, &b) in bytes.iter().enumerate() {
        match (b.is_ascii_alphanumeric(), start) {
            (true, None) => start = Some(i),
            (false, Some(s)) => {
                runs.push((s, i));
                start = None;
            }
            _ => {}
        }
    }
    if let Some(s) = start {
        runs.push((s, bytes.len()));
    }
    let digits = |r: (usize, usize)| bytes[r.0..r.1].iter().all(u8::is_ascii_digit);
    let at = |i: usize| bytes.get(i).copied();
    let before = |i: usize| i.checked_sub(1).map(|j| bytes[j]);

    let capitals = |r: (usize, usize)| {
        bytes[r.0..r.1]
            .iter()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
    };
    let mixed = |chars: &[u8]| {
        chars.iter().any(u8::is_ascii_digit) && chars.iter().any(u8::is_ascii_uppercase)
    };

    let mut found = Vec::new();
    let mut i = 0;
    while i < runs.len() {
        let run = runs[i];
        // Two groups of 3–4 capitals and digits: "4K7-9QX", "123-4K7".
        if let Some(&next) = runs.get(i + 1)
            && at(run.1) == Some(b'-')
            && next.0 == run.1 + 1
            && (3..=4).contains(&(run.1 - run.0))
            && (3..=4).contains(&(next.1 - next.0))
            && capitals(run)
            && capitals(next)
            && mixed(&bytes[run.0..next.1])
        {
            if clear_before(bytes, run.0) && clear_after(bytes, next.1) {
                found.push(Candidate {
                    start: run.0,
                    end: next.1,
                    code: src[run.0..next.1].to_owned(),
                    mixed: true,
                    year: false,
                });
            }
            i += 2;
            continue;
        }
        if digits(run) {
            // Digits in a box each: 4–8 of them, a few spaces apart.
            if run.1 - run.0 == 1 {
                let mut j = i;
                while j + 1 < runs.len()
                    && runs[j + 1].1 - runs[j + 1].0 == 1
                    && digits(runs[j + 1])
                    && runs[j + 1].0 - runs[j].1 <= 3
                    && src[runs[j].1..runs[j + 1].0]
                        .chars()
                        .all(char::is_whitespace)
                {
                    j += 1;
                }
                if (4..=8).contains(&(j - i + 1))
                    && clear_before(bytes, run.0)
                    && clear_after(bytes, runs[j].1)
                {
                    found.push(Candidate {
                        start: run.0,
                        end: runs[j].1,
                        code: runs[i..=j].iter().map(|r| &src[r.0..r.1]).collect(),
                        mixed: false,
                        year: false,
                    });
                    i = j + 1;
                    continue;
                }
            }
            // Digits joined by single separators: one number, a date, a
            // time, an amount, a phone number or a code in groups.
            let mut j = i;
            let mut joints = Vec::new();
            while j + 1 < runs.len() && runs[j + 1].0 - runs[j].1 == 1 && digits(runs[j + 1]) {
                let joint = bytes[runs[j].1];
                if !matches!(joint, b' ' | b'-' | b'.' | b',' | b'/' | b':') {
                    break;
                }
                joints.push(joint);
                j += 1;
            }
            let groups: Vec<usize> = runs[i..=j].iter().map(|r| r.1 - r.0).collect();
            let total: usize = groups.iter().sum();
            let grouped = match groups.as_slice() {
                [_] => true,
                [a, rest @ ..] => {
                    joints.iter().all(|&joint| joint == joints[0])
                        && matches!(joints[0], b' ' | b'-')
                        && rest.iter().all(|g| g == a)
                        // "48 29 13", but "03-11-26" is a date.
                        && (*a == 3 || *a == 4 || (*a == 2 && groups.len() == 3 && joints[0] == b' '))
                }
                [] => false,
            };
            let (start, end) = (run.0, runs[j].1);
            // "G-482913": Google's code, typed without the G.
            let prefix = (before(start) == Some(b'-'))
                .then(|| i.checked_sub(1).map(|k| runs[k]))
                .flatten()
                .filter(|p| {
                    p.1 + 1 == start
                        && (1..=3).contains(&(p.1 - p.0))
                        && bytes[p.0..p.1].iter().all(u8::is_ascii_uppercase)
                        && clear_before(bytes, p.0)
                });
            let clear = match prefix {
                Some(_) => true,
                None => clear_before(bytes, start),
            };
            if grouped
                && (4..=8).contains(&total)
                && clear
                && clear_after(bytes, end)
                && !amount_or_quantity(src, prefix.map_or(start, |p| p.0), end)
            {
                let code: String = runs[i..=j].iter().map(|r| &src[r.0..r.1]).collect();
                let year = total == 4 && (1900..=2099).contains(&code.parse::<u32>().unwrap_or(0));
                found.push(Candidate {
                    start: prefix.map_or(start, |p| p.0),
                    end,
                    code,
                    mixed: false,
                    year,
                });
            }
            i = j + 1;
            continue;
        }
        // Capitals and digits.
        if capitals(run)
            && (4..=10).contains(&(run.1 - run.0))
            && mixed(&bytes[run.0..run.1])
            && clear_before(bytes, run.0)
            && clear_after(bytes, run.1)
        {
            found.push(Candidate {
                start: run.0,
                end: run.1,
                code: src[run.0..run.1].to_owned(),
                mixed: true,
                year: false,
            });
        }
        i += 1;
    }
    found
}

/// Whether the words around a number at `start..end` make it an amount
/// ("INR 5000", "Rs. 5000", "INR" wrapped onto the line before, "5000
/// USD") or a quantity ("1440 minutes"). A currency that's another amount's
/// doesn't count: one after an amount ("5000 INR\n482913"), or one followed
/// by an amount ("482913 INR 5000"). Words after it count on its own line.
fn amount_or_quantity(src: &str, start: usize, end: usize) -> bool {
    let before = src[..start].trim_end_matches(|c: char| c.is_whitespace() || c == '.');
    let word_before = before
        .rsplit(|c: char| !c.is_ascii_alphabetic())
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    // An amount of its own: a number alone before it on its line ("5000",
    // "$5,000.00"), not a masked card ("XXXX1234") or one on the line above.
    let rest = &before[..before.len() - word_before.len()];
    let after_a_number = rest[rest.rfind('\n').map_or(0, |i| i + 1)..]
        .split_whitespace()
        .last()
        .map(|w| w.trim_start_matches(['-', '+', '$', '€', '£', '¥', '₹']))
        .is_some_and(|w| {
            w.contains(|c: char| c.is_ascii_digit())
                && w.chars()
                    .all(|c| c.is_ascii_digit() || matches!(c, ',' | '.'))
        });
    let line_after = &src[end..src[end..].find('\n').map_or(src.len(), |i| end + i)];
    let after = line_after.trim_start();
    let word_after = after
        .split(|c: char| !c.is_ascii_alphabetic())
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let then_a_number = after[word_after.len()..]
        .trim_start()
        .starts_with(|c: char| c.is_ascii_digit());
    CURRENCIES.contains(&word_before.as_str()) && !after_a_number
        || CURRENCIES.contains(&word_after.as_str()) && !then_a_number
        || UNITS.contains(&word_after.as_str())
}

/// Nothing before position `i` makes it part of something else: an amount,
/// a phone number, a link, an address, a hashtag or issue number.
fn clear_before(bytes: &[u8], i: usize) -> bool {
    let Some(prev) = i.checked_sub(1).map(|j| bytes[j]) else {
        return true;
    };
    match prev {
        b'+' | b'#' | b'$' | b'/' | b'=' | b'?' | b'&' | b'@' | b'.' | b'_' | b'\\' | b'%' => false,
        // "order-482913" (a short prefix, as in "G-482913", is the caller's).
        b'-' => !i
            .checked_sub(2)
            .is_some_and(|j| bytes[j].is_ascii_alphanumeric()),
        // Currency signs other than '$' take more than a byte: €, £, ¥, ₹.
        _ => {
            let tail = &bytes[i.saturating_sub(3)..i];
            !["€", "£", "¥", "₹"]
                .iter()
                .any(|sign| tail.ends_with(sign.as_bytes()))
        }
    }
}

/// Nothing after position `i` makes it part of something else.
fn clear_after(bytes: &[u8], i: usize) -> bool {
    let Some(&next) = bytes.get(i) else {
        return true;
    };
    let then = bytes.get(i + 1).copied();
    match next {
        b'%' | b'@' | b'_' | b'/' | b'\\' => false,
        // Into another word or number: "482913-abc", "482913.jpg", "4,5".
        b'-' | b'.' | b',' | b':' => !then.is_some_and(|b| b.is_ascii_alphanumeric()),
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn code(subject: &str, text: &str) -> Option<String> {
        find(subject, text, None)
    }

    #[test]
    fn finds_the_codes_big_senders_send() {
        // Google
        assert_eq!(
            code(
                "G-482913 is your Google verification code",
                "G-482913 is your Google verification code.\n\nDon't share this code with anyone."
            )
            .as_deref(),
            Some("482913")
        );
        // Microsoft
        assert_eq!(
            code(
                "Microsoft account security code",
                "Microsoft account\nSecurity code\nPlease use the following security code for the \
                 Microsoft account jo*****@outlook.com.\nSecurity code: 4829135\nIf you don't \
                 recognize the Microsoft account jo*****@outlook.com, you can click here to remove \
                 your email address from that account.\nThanks,\nThe Microsoft account team"
            )
            .as_deref(),
            Some("4829135")
        );
        // GitHub: the code is well down the email.
        assert_eq!(
            code(
                "[GitHub] Please verify your device",
                "Hey octocat!\n\nA sign in attempt requires further verification because we did \
                 not recognize your device. To complete the sign in, enter the verification code \
                 on the unrecognized device.\n\nDevice: Chrome on Windows\nVerification code: \
                 482913\n\nIf you did not attempt to sign in to your account, your password may be \
                 compromised. Visit https://github.com/settings/security to create a new, strong \
                 password for your GitHub account."
            )
            .as_deref(),
            Some("482913")
        );
        // Apple: on a line of its own.
        assert_eq!(
            code(
                "Verify your Apple Account email address",
                "Your verification code is:\n\n482913\n\nIf you didn't ask for a code, you can \
                 ignore this email."
            )
            .as_deref(),
            Some("482913")
        );
        // Instagram: in the subject, before the word.
        assert_eq!(
            code(
                "482913 is your Instagram code",
                "Hi octocat, Someone tried to log in to your Instagram account. If this was you, \
                 please use the following code to confirm your identity:\n\n482913"
            )
            .as_deref(),
            Some("482913")
        );
        assert_eq!(
            code("", "Use 4829135 as Microsoft account security code").as_deref(),
            Some("4829135")
        );
        // Amazon
        assert_eq!(
            code(
                "Your Amazon verification code",
                "To verify your identity, please use the following code:\n\n482913\n\nAmazon takes \
                 your account security very seriously."
            )
            .as_deref(),
            Some("482913")
        );
    }

    #[test]
    fn finds_codes_written_in_groups_or_with_letters() {
        assert_eq!(
            code(
                "Your login code",
                "Your login code is 482 913. It expires in 10 minutes."
            )
            .as_deref(),
            Some("482913")
        );
        assert_eq!(
            code("Sign in", "Your one-time code: 4829 1357").as_deref(),
            Some("48291357")
        );
        // Each digit in a box of its own.
        assert_eq!(
            code(
                "Verify your email",
                "Your verification code\n4 8 2 9 1 3\nThis code expires in 10 minutes."
            )
            .as_deref(),
            Some("482913")
        );
        // Slack-style, a few lines down, on a line of its own.
        assert_eq!(
            code(
                "Slack confirmation code: 482-913",
                "Confirm your email address\nYour confirmation code is below — enter it in your \
                 open browser window and we'll help you get signed in.\n\n482-913\n\nIf you didn't \
                 request this email, there's nothing to worry about."
            )
            .as_deref(),
            Some("482913")
        );
        // Steam Guard: capitals and digits.
        assert_eq!(
            code(
                "Your Steam account: Access from new computer",
                "Here is the Steam Guard code you need to login to account octocat:\n\n2F4KD\n\nThis \
                 email was generated because of a login attempt from a new computer."
            )
            .as_deref(),
            Some("2F4KD")
        );
        assert_eq!(
            code("", "Your verification code is 4K7-9QX").as_deref(),
            Some("4K7-9QX")
        );
        // A bank's, after an amount.
        assert_eq!(
            code(
                "",
                "Your one-time passcode (OTP) for the transaction of $1,234.00 at ACME STORE is \
                 482913. Do not share it with anyone."
            )
            .as_deref(),
            Some("482913")
        );
    }

    #[test]
    fn finds_codes_in_other_languages() {
        assert_eq!(
            code(
                "Tu código de verificación",
                "Tu código de verificación es 482913"
            )
            .as_deref(),
            Some("482913")
        );
        assert_eq!(
            code("Anmeldung", "Ihr Bestätigungscode lautet: 482913").as_deref(),
            Some("482913")
        );
        assert_eq!(
            code("", "Votre code de vérification est 482913").as_deref(),
            Some("482913")
        );
    }

    #[test]
    fn reads_html_that_runs_words_together() {
        let html = "<html><head><style>.c{color:#FF0000}</style></head><body><table><tr><td>Your \
                    verification code</td><td>482913</td></tr></table></body></html>";
        let text = html_to_text(html);
        assert_eq!(code("Verify", &text), None, "the text reads {text:?}");
        assert_eq!(find("Verify", &text, Some(html)).as_deref(), Some("482913"));
    }

    #[test]
    fn a_year_needs_the_words_right_before_it() {
        assert_eq!(
            code("", "Your verification code: 2024").as_deref(),
            Some("2024")
        );
        assert_eq!(
            code("", "Your verification code expires soon, 2026 Example Inc").as_deref(),
            None
        );
    }

    #[test]
    fn leaves_mail_that_only_mentions_numbers_and_codes() {
        let none = [
            (
                "20% off everything",
                "Use code SAVE20 at checkout. Offer expires Sunday.",
            ),
            (
                "Your discount code",
                "Your discount code is WELCOME10. Sign in to use it.",
            ),
            (
                "Invitation: Weekly sync",
                "Join Zoom Meeting\nhttps://zoom.us/j/81234567890?pwd=abc\n\nMeeting ID: 812 3456 \
                 7890\nPasscode: 482913",
            ),
            (
                "Your order",
                "Thanks for your order! Order number: 112-4829135-1234567. Sign in to track it.",
            ),
            (
                "Shipped",
                "Your tracking code is 1Z999AA10123456784. Log in to see where it is.",
            ),
            (
                "Verify your email",
                "Please verify your email address by clicking the link below. This link expires in \
                 24 hours.\n\n© 2026 Example Inc, 1600 Main Street",
            ),
            (
                "Sign in to Example",
                "Click the button below to sign in. This link and code expire in 10 minutes.\n\n\
                 Example Inc, 1600 Amphitheatre Parkway, Mountain View, CA 94043",
            ),
            (
                "Your sign-in code",
                "Didn't request this code? Call us at 1-800-555-0199 or +44 20 7946 0958.",
            ),
            (
                "Sign-in attempt",
                "Your sign-in code was requested on 03/11/2026 at 14:35 from 192.168.1.20.",
            ),
            ("Door", "Hey! The door code is 4829, see you at 7."),
            (
                "Re: [octo/repo] Fix login code path (#4821)",
                "@octocat requested your review on: octo/repo#4821 Fix login code path\n\nYou can \
                 view, comment on, or merge this pull request online at: \
                 https://github.com/octo/repo/pull/4821",
            ),
        ];
        for (subject, text) in none {
            assert_eq!(code(subject, text), None, "{subject}: {text}");
        }
    }

    #[test]
    fn prefers_the_code_to_an_amount_or_a_date() {
        // An amount in a currency's letters.
        assert_eq!(
            code(
                "Your one-time passcode",
                "Your one-time passcode for a payment of INR 5000 is 482913. Do not share it with \
                 anyone."
            )
            .as_deref(),
            Some("482913")
        );
        // An amount with no currency: the sentence gives the code as its answer.
        assert_eq!(
            code(
                "",
                "Your OTP for the transaction of 5000 at Amazon is 482913."
            )
            .as_deref(),
            Some("482913")
        );
        assert_eq!(
            code("Sign in", "Your sign-in code expires in 1440 minutes.").as_deref(),
            None
        );
        // A date in pairs of digits.
        assert_eq!(
            code(
                "Security alert: sign-in code requested",
                "Your sign-in code was requested on 03-11-26 at 14:35."
            )
            .as_deref(),
            None
        );
        assert_eq!(
            code("", "Your verification code is 48 29 13").as_deref(),
            Some("482913")
        );
    }

    #[test]
    fn a_code_in_two_groups_may_start_with_digits() {
        assert_eq!(
            code(
                "Your verification code",
                "Your verification code is 123-4K7"
            )
            .as_deref(),
            Some("123-4K7")
        );
    }

    #[test]
    fn leaves_codes_that_are_for_something_else_however_the_email_ends() {
        let none = [
            (
                "Your order confirmation",
                "Your order confirmation code is 482913. Sign in to view your order.",
            ),
            (
                "Invoice 1042",
                "Customer code: 482913\nSign in to pay online.",
            ),
            (
                "Your table",
                &format!(
                    "Welcome! Your table code is 482913.\n\n{}\n\nSign in to manage your booking.",
                    "We look forward to seeing you. ".repeat(12)
                ),
            ),
        ];
        for (subject, text) in none {
            assert_eq!(code(subject, text), None, "{subject}: {text}");
        }
    }

    #[test]
    fn a_code_named_for_something_else_stays_so() {
        assert_eq!(
            code(
                "Your order confirmation",
                "Your order confirmation code\n\n482913\n\nUse this code to collect your order."
            ),
            None
        );
    }

    #[test]
    fn a_currency_on_the_next_line_or_before_another_amount_is_not_the_codes() {
        assert_eq!(
            code(
                "Your one-time passcode",
                "Your OTP is 482913\nINR 5000 will be charged to your card."
            )
            .as_deref(),
            Some("482913")
        );
        assert_eq!(
            code(
                "Your one-time passcode",
                "Your OTP is 482913 INR 5000 at ACME"
            )
            .as_deref(),
            Some("482913")
        );
        assert_eq!(
            code("Your one-time passcode", "Your OTP is 5000 INR").as_deref(),
            None
        );
    }

    #[test]
    fn a_brand_before_a_verification_code_is_no_qualifier() {
        assert_eq!(
            code(
                "Your Shop verification code",
                "Your Shop verification code is 482913. Enter this code to sign in."
            )
            .as_deref(),
            Some("482913")
        );
        assert_eq!(
            code("", "Your App Store verification code is 482913.").as_deref(),
            Some("482913")
        );
        assert_eq!(
            code("", "Your Customer verification code is 482913.").as_deref(),
            Some("482913")
        );
    }

    #[test]
    fn a_currency_wrapped_onto_the_line_before_still_makes_an_amount() {
        assert_eq!(
            code(
                "Your one-time passcode",
                "Your one-time passcode for a payment of INR\n5000\n\n482913\n\nDo not share this \
                 code with anyone."
            )
            .as_deref(),
            Some("482913")
        );
        // A currency after an amount is that amount's, not the next line's.
        assert_eq!(
            code(
                "Your one-time passcode",
                "Amount: 5000 INR\nYour one-time passcode:\n482913"
            )
            .as_deref(),
            Some("482913")
        );
    }

    #[test]
    fn a_purpose_before_verification_still_makes_it_something_else() {
        assert_eq!(
            code(
                "Your delivery verification code",
                "Your delivery verification code is 482913. Share this code with your delivery \
                 partner."
            ),
            None
        );
        assert_eq!(
            code(
                "",
                "Your order verification code is 482913. Sign in to track it."
            ),
            None
        );
    }

    #[test]
    fn a_masked_card_before_an_amount_leaves_it_an_amount() {
        for card in ["XXXX1234", "xxxx1234"] {
            let text = format!(
                "Your one-time passcode for this payment:\nCard: {card}\nINR 5000\n\n482913\n\nDo \
                 not share this code with anyone."
            );
            assert_eq!(
                code("Your one-time passcode", &text).as_deref(),
                Some("482913"),
                "{card}"
            );
        }
    }

    #[test]
    fn bank_and_hotel_codes_are_not_for_signing_in() {
        let none = [
            (
                "Your wire transfer instructions",
                "Sign in to Online Banking to send a wire.\nSWIFT BIC code: CHASUS33\nUse this code \
                 for international wire transfers.",
            ),
            (
                "Your booking is confirmed",
                "Your reservation at City Hotel is confirmed.\nConfirmation number: 4829135701\nPIN \
                 code: 5837\nSign in to manage your booking.",
            ),
        ];
        for (subject, text) in none {
            assert_eq!(code(subject, text), None, "{subject}: {text}");
        }
        assert_eq!(
            code("", "Your one-time PIN is 482913. Don't share it.").as_deref(),
            Some("482913")
        );
        assert_eq!(
            code("", "Your verification PIN code is 482913.").as_deref(),
            Some("482913")
        );
    }

    #[test]
    fn a_code_to_give_the_driver_is_not_for_signing_in() {
        let text = "Your one-time code is 681492. Please share this code with your driver when your \
                    package arrives.";
        assert_eq!(code("Your delivery verification code", text), None);
        assert_eq!(code("Your package arrives today", text), None);
    }

    #[test]
    fn handing_over_takes_words_saying_so() {
        assert_eq!(
            code(
                "Your order is out for delivery",
                "Your one-time code is 719483. Please give this code to the delivery person."
            ),
            None
        );
        // A driver's own sign-in code is still one.
        assert_eq!(
            code(
                "Your Uber Driver verification code",
                "Your Uber Driver verification code is 5821. Enter this code to sign in to your \
                 driver account."
            )
            .as_deref(),
            Some("5821")
        );
        assert_eq!(
            code("", "Your sign-in PIN code is 583719.").as_deref(),
            Some("583719")
        );
    }

    /// Mail in any script, with every kind of space, never trips it up:
    /// `guess` itself, not `find`'s safety net.
    #[test]
    fn reads_any_text_without_panicking() {
        let pieces = [
            "Your",
            "Uber",
            "Driver",
            "Delivery Partner",
            "verification",
            "code",
            "Code:",
            "is",
            "9437",
            "482 913",
            "G-482913",
            "4K7-9QX",
            "2F4KD",
            "INR",
            "₹5,000",
            "-£1,250.00",
            "€",
            "PIN",
            "sign-in",
            "one-time",
            "OTP",
            "Dear",
            "Hello,",
            "as a",
            "share",
            "with",
            "your",
            "the",
            "driver's",
            "checkout",
            "faster",
            "% off",
            "Welcome back to",
            "Open",
            "é",
            "日本語",
            "🙂",
            "—",
            "’s",
            "ß",
            "İ",
            "\u{301}",
            ".",
            ",",
            "!",
            ":",
            ";",
            "#",
            "$",
            "/",
            "@",
            "XXXX1234",
            "03-11-26",
            "14:35",
        ];
        let gaps = [" ", "\u{a0}", "", "\n", "\u{2009}", "\t", "\u{3000}"];
        let mut seed: u64 = 0x2545_f491_4f6c_dd1d;
        let mut next = |n: usize| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % n as u64) as usize
        };
        for _ in 0..20_000 {
            let mut text = String::new();
            for _ in 0..(5 + next(25)) {
                text.push_str(pieces[next(pieces.len())]);
                text.push_str(gaps[next(gaps.len())]);
            }
            let cut = &text[..floor(&text, text.len() / 3)];
            guess(cut, &text, None);
            guess("", &text, Some(&text));
        }
    }

    #[test]
    fn cuts_long_text_at_a_character() {
        let long = "é".repeat(LOOK);
        assert_eq!(code("Your verification code", &long), None);
        assert_eq!(head("aé", 2), "a");
    }
}
