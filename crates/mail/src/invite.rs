//! Calendar invitations that arrive by email (iCalendar, RFC 5545, sent the
//! iMIP way, RFC 6047): the event an invitation, reply or cancellation
//! carries, for the card the thread shows in place of the body, and
//! answering an invitation by email with a REPLY (RFC 5546).
//!
//! Times become Unix ms with the time zones the calendar defines itself
//! (Outlook and Apple send them); Google sends UTC. A zone the calendar
//! doesn't define leaves the time floating: shown as written.

use lettre::message::header::{ContentType, HeaderName, HeaderValue};
use lettre::message::{Attachment, Mailbox, MultiPart, SinglePart};
use mail_parser::{Message, MessageParser, MimeHeaders};
use serde::{Deserialize, Serialize};

use crate::draft::new_message_id;
use crate::graph::{civil_from_days, days_from_civil};
use crate::{Error, Result};

const PRODID: &str = "-//spark-rs//Spark-rs Mail//EN";

/// What the calendar part is for, by its METHOD.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Kind {
    /// REQUEST: an invitation, or an update to one, to answer.
    Invitation,
    /// REPLY: an attendee's answer.
    Reply,
    /// CANCEL.
    Cancellation,
    /// COUNTER: an attendee proposes another time.
    Proposal,
    /// PUBLISH, or no METHOD: an event to look at.
    #[default]
    Event,
}

/// An attendee's answer (PARTSTAT).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Answer {
    Accepted,
    Tentative,
    Declined,
}

impl Answer {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Tentative => "tentative",
            Self::Declined => "declined",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "accepted" => Self::Accepted,
            "tentative" => Self::Tentative,
            "declined" => Self::Declined,
            _ => return None,
        })
    }

    fn from_partstat(value: &str) -> Option<Self> {
        match value.trim().to_ascii_uppercase().as_str() {
            "ACCEPTED" => Some(Self::Accepted),
            "TENTATIVE" => Some(Self::Tentative),
            "DECLINED" => Some(Self::Declined),
            _ => None,
        }
    }

    fn partstat(self) -> &'static str {
        match self {
            Self::Accepted => "ACCEPTED",
            Self::Tentative => "TENTATIVE",
            Self::Declined => "DECLINED",
        }
    }
}

/// The organizer or an attendee.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Person {
    /// Their CN, else the address.
    pub name: String,
    pub address: String,
    /// `None` until they answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<Answer>,
}

/// The event an email's calendar part carries, as the store keeps it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Invite {
    pub kind: Kind,
    pub uid: String,
    pub sequence: i64,
    pub summary: String,
    /// Unix ms. All-day and floating times are the times as written, read
    /// as UTC.
    pub start: i64,
    pub end: Option<i64>,
    pub all_day: bool,
    /// No time zone: the times are the same wherever the reader is.
    pub floating: bool,
    /// A recurring event (RRULE or RDATE); answering answers the series.
    pub repeats: bool,
    pub location: Option<String>,
    pub description: Option<String>,
    /// A video call to join (Meet, Teams, Zoom, …).
    pub call_link: Option<String>,
    pub organizer: Option<Person>,
    /// Everyone else invited, rooms left out.
    pub attendees: Vec<Person>,
    /// STATUS:CANCELLED, or a cancellation.
    pub canceled: bool,
    /// The calendar text, for answering.
    pub ics: String,
}

/// What the thread shows for an invite email.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Card {
    pub kind: Kind,
    pub summary: String,
    pub start: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end: Option<i64>,
    pub all_day: bool,
    pub floating: bool,
    pub repeats: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub call_link: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub organizer: Option<Person>,
    pub attendees: Vec<Person>,
    pub canceled: bool,
    /// The user's answer to an invitation: the one sent from here, else
    /// the one the invitation records.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub answer: Option<Answer>,
    /// An invitation the user can answer: someone else's, and still on.
    pub can_answer: bool,
}

impl Invite {
    /// The invite in an email's first calendar part: a `text/calendar`
    /// alternative of the body, or an `.ics` attachment.
    pub fn find(m: &Message) -> Option<Self> {
        m.parts.iter().find_map(|part| {
            let mime = part
                .content_type()
                .map(|ct| format!("{}/{}", ct.ctype(), ct.subtype().unwrap_or_default()))
                .unwrap_or_default()
                .to_ascii_lowercase();
            let ics = part
                .attachment_name()
                .is_some_and(|n| n.trim().to_ascii_lowercase().ends_with(".ics"));
            if mime != "text/calendar" && mime != "application/ics" && !ics {
                return None;
            }
            Self::parse(&String::from_utf8_lossy(part.contents()))
        })
    }

    /// The invite in a raw RFC 5322 message.
    pub fn from_raw(raw: &[u8]) -> Option<Self> {
        Self::find(&MessageParser::default().parse(raw)?)
    }

    /// Reads a calendar's first event.
    pub fn parse(ics: &str) -> Option<Self> {
        let cal = parse_calendar(ics)?;
        let event = cal.child("VEVENT")?;
        let kind = method(&cal);
        let start = event.prop("DTSTART").and_then(|p| time(p, &cal))?;
        let end = match event.prop("DTEND").and_then(|p| time(p, &cal)) {
            Some(end) => Some(end.ms),
            None => match event.prop("DURATION").and_then(|p| duration_ms(&p.value)) {
                Some(d) => Some(start.ms + d),
                // An all-day event without an end is that one day.
                None => start.all_day.then_some(start.ms + DAY_MS),
            },
        };
        let organizer = event.prop("ORGANIZER").map(person);
        let mut attendees: Vec<Person> = event
            .props
            .iter()
            .filter(|p| p.name == "ATTENDEE")
            .filter(|p| {
                !p.param("CUTYPE").is_some_and(|t| {
                    t.eq_ignore_ascii_case("ROOM") || t.eq_ignore_ascii_case("RESOURCE")
                })
            })
            .map(person)
            .collect();
        // Google lists the organizer among the attendees too; Outlook
        // doesn't, and an organizer is going unless they say otherwise.
        let organizer = organizer.map(|mut o| {
            if let Some(i) = attendees.iter().position(|a| same(&a.address, &o.address)) {
                o.answer = attendees.remove(i).answer;
            }
            o.answer = o.answer.or(Some(Answer::Accepted));
            o
        });
        let location = event.text("LOCATION");
        let description = event.text("DESCRIPTION");
        let call_link = call_link(event, location.as_deref(), description.as_deref());
        let description = description
            .map(|d| without_boilerplate(&d))
            .filter(|d| !d.is_empty());
        Some(Self {
            kind,
            uid: event.text("UID").unwrap_or_default(),
            sequence: event
                .prop("SEQUENCE")
                .and_then(|p| p.value.trim().parse().ok())
                .unwrap_or(0),
            summary: event.text("SUMMARY").unwrap_or_default(),
            start: start.ms,
            end,
            all_day: start.all_day,
            floating: start.floating,
            repeats: event.prop("RRULE").is_some() || event.prop("RDATE").is_some(),
            location,
            description,
            call_link,
            organizer,
            attendees,
            canceled: kind == Kind::Cancellation
                || event
                    .prop("STATUS")
                    .is_some_and(|p| p.value.trim().eq_ignore_ascii_case("CANCELLED")),
            ics: ics.to_owned(),
        })
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("an invite serializes")
    }

    pub fn from_json(json: &str) -> Option<Self> {
        serde_json::from_str(json).ok()
    }

    /// The card for the account `me` (its address), with `sent`, the
    /// answer already sent from here.
    pub fn card(&self, me: &str, sent: Option<Answer>) -> Card {
        let invitation = self.kind == Kind::Invitation;
        let mut attendees = self.attendees.clone();
        let mine = attendees.iter_mut().find(|a| same(&a.address, me));
        let answer = if invitation {
            sent.or(mine.as_ref().and_then(|a| a.answer))
        } else {
            None
        };
        if let (Some(mine), Some(sent)) = (mine, sent.filter(|_| invitation)) {
            mine.answer = Some(sent);
        }
        let organizing = self
            .organizer
            .as_ref()
            .is_some_and(|o| same(&o.address, me));
        Card {
            kind: self.kind,
            summary: self.summary.clone(),
            start: self.start,
            end: self.end,
            all_day: self.all_day,
            floating: self.floating,
            repeats: self.repeats,
            location: self.location.clone(),
            description: self.description.clone(),
            call_link: self.call_link.clone(),
            organizer: self.organizer.clone(),
            attendees,
            canceled: self.canceled,
            answer,
            can_answer: invitation && !self.canceled && !organizing && self.organizer.is_some(),
        }
    }
}

/// The REPLY (RFC 5546) to invitation `ics` from attendee `me` (`name` is
/// their display name), stamped `now` (Unix ms). It names the event by UID,
/// RECURRENCE-ID and SEQUENCE, and repeats its times and title for the
/// organizer's mail app. `None` if `ics` isn't an invitation.
pub fn reply(ics: &str, me: &str, name: &str, answer: Answer, now: i64) -> Option<String> {
    let cal = parse_calendar(ics)?;
    if method(&cal) != Kind::Invitation {
        return None;
    }
    let event = cal.child("VEVENT")?;
    let mut lines: Vec<String> = [
        "BEGIN:VCALENDAR",
        &format!("PRODID:{PRODID}"),
        "VERSION:2.0",
        "CALSCALE:GREGORIAN",
        "METHOD:REPLY",
    ]
    .map(str::to_owned)
    .into();
    // The time zones its times are in.
    let zones: Vec<&str> = event.props.iter().filter_map(|p| p.param("TZID")).collect();
    for zone in cal.children.iter().filter(|c| {
        c.name == "VTIMEZONE"
            && c.prop("TZID")
                .is_some_and(|t| zones.contains(&t.value.trim()))
    }) {
        zone.write(&mut lines);
    }
    lines.push("BEGIN:VEVENT".into());
    for name in [
        "UID",
        "RECURRENCE-ID",
        "SEQUENCE",
        "DTSTART",
        "DTEND",
        "DURATION",
        "SUMMARY",
        "LOCATION",
        "ORGANIZER",
    ] {
        if let Some(p) = event.prop(name) {
            lines.push(p.line.clone());
        }
    }
    lines.push(format!("DTSTAMP:{}", utc_stamp(now)));
    let mine = event
        .props
        .iter()
        .find(|p| p.name == "ATTENDEE" && same(mailto(&p.value), me));
    lines.push(attendee_line(mine, me, name, answer));
    lines.push("END:VEVENT".into());
    lines.push("END:VCALENDAR".into());
    Some(lines.iter().map(|l| fold(l)).collect())
}

/// The email that carries `reply` to `organizer`: a line of text with the
/// REPLY as its calendar alternative (RFC 6047), and attached as reply.ics
/// as well, as Google does. `in_reply_to` threads it under the invitation's
/// Message-ID.
pub fn reply_message(
    from: Mailbox,
    organizer: Mailbox,
    title: &str,
    answer: Answer,
    reply: &str,
    in_reply_to: Option<&str>,
) -> Result<lettre::Message> {
    let who = from.name.clone().unwrap_or_else(|| from.email.to_string());
    let (prefix, verb) = match answer {
        Answer::Accepted => ("Accepted", "accepted"),
        Answer::Tentative => ("Tentative", "tentatively accepted"),
        Answer::Declined => ("Declined", "declined"),
    };
    let title = Some(title.trim())
        .filter(|t| !t.is_empty())
        .unwrap_or("No subject");
    let mut builder = lettre::Message::builder()
        .message_id(Some(new_message_id(from.email.domain())))
        .from(from)
        .to(organizer)
        .subject(format!("{prefix}: {title}"));
    if let Some(id) = in_reply_to {
        builder = builder
            .in_reply_to(format!("<{id}>"))
            .raw_header(HeaderValue::new(
                HeaderName::new_from_ascii_str("References"),
                format!("<{id}>"),
            ));
    }
    let calendar = ContentType::parse("text/calendar; charset=utf-8; method=REPLY").expect("valid");
    let ics = ContentType::parse("application/ics").expect("valid");
    let body = MultiPart::mixed()
        .multipart(
            MultiPart::alternative()
                .singlepart(SinglePart::plain(format!(
                    "{who} has {verb} this invitation.\n"
                )))
                .singlepart(
                    SinglePart::builder()
                        .header(calendar)
                        .body(reply.to_owned()),
                ),
        )
        .singlepart(Attachment::new("reply.ics".into()).body(reply.to_owned(), ics));
    builder
        .multipart(body)
        .map_err(|e| Error::Config(format!("Couldn't build the reply: {e}")))
}

// ---- reading iCalendar ----

const DAY_MS: i64 = 86_400_000;

/// A content line: `NAME;PARAM=value:value`, unfolded.
#[derive(Debug)]
struct Prop {
    /// Uppercase.
    name: String,
    /// Names uppercase; values unquoted, several joined by commas.
    params: Vec<(String, String)>,
    value: String,
    /// The whole line, for copying it into a reply.
    line: String,
}

impl Prop {
    fn param(&self, name: &str) -> Option<&str> {
        self.params
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }
}

#[derive(Debug, Default)]
struct Component {
    name: String,
    props: Vec<Prop>,
    children: Vec<Component>,
}

impl Component {
    fn prop(&self, name: &str) -> Option<&Prop> {
        self.props.iter().find(|p| p.name == name)
    }

    fn child(&self, name: &str) -> Option<&Component> {
        self.children.iter().find(|c| c.name == name)
    }

    /// A text property, unescaped; `None` if blank.
    fn text(&self, name: &str) -> Option<String> {
        self.prop(name)
            .map(|p| unescape(&p.value))
            .filter(|t| !t.is_empty())
    }

    fn write(&self, lines: &mut Vec<String>) {
        lines.push(format!("BEGIN:{}", self.name));
        lines.extend(self.props.iter().map(|p| p.line.clone()));
        for child in &self.children {
            child.write(lines);
        }
        lines.push(format!("END:{}", self.name));
    }
}

/// The VCALENDAR in `text`. Unclosed components (a cut-off file) still count.
fn parse_calendar(text: &str) -> Option<Component> {
    let mut stack = vec![Component::default()];
    for line in unfold(text) {
        let Some(p) = parse_line(&line) else {
            continue;
        };
        match p.name.as_str() {
            "BEGIN" => stack.push(Component {
                name: p.value.trim().to_ascii_uppercase(),
                ..Default::default()
            }),
            "END" if stack.len() > 1 => {
                let done = stack.pop()?;
                stack.last_mut()?.children.push(done);
            }
            "END" => {}
            _ => stack.last_mut()?.props.push(p),
        }
    }
    while stack.len() > 1 {
        let done = stack.pop()?;
        stack.last_mut()?.children.push(done);
    }
    stack
        .pop()?
        .children
        .into_iter()
        .find(|c| c.name == "VCALENDAR")
}

fn method(cal: &Component) -> Kind {
    match cal
        .prop("METHOD")
        .map(|p| p.value.trim().to_ascii_uppercase())
        .as_deref()
    {
        Some("REQUEST") => Kind::Invitation,
        Some("REPLY") => Kind::Reply,
        Some("CANCEL") => Kind::Cancellation,
        Some("COUNTER") => Kind::Proposal,
        _ => Kind::Event,
    }
}

/// Lines with their continuations (a leading space or tab) joined on.
fn unfold(text: &str) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for line in text.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if let (Some(rest), Some(last)) = (line.strip_prefix([' ', '\t']), lines.last_mut()) {
            last.push_str(rest);
        } else if !line.trim().is_empty() {
            lines.push(line.to_owned());
        }
    }
    lines
}

fn parse_line(line: &str) -> Option<Prop> {
    let name_end = line.find([';', ':'])?;
    let name = line[..name_end].trim().to_ascii_uppercase();
    let mut rest = &line[name_end..];
    let mut params = Vec::new();
    while let Some(after) = rest.strip_prefix(';') {
        let eq = after.find(['=', ':', ';'])?;
        let param = after[..eq].trim().to_ascii_uppercase();
        let mut v = after[eq..].strip_prefix('=').unwrap_or(&after[eq..]);
        let mut value = String::new();
        loop {
            if let Some(quoted) = v.strip_prefix('"') {
                let close = quoted.find('"').unwrap_or(quoted.len());
                value.push_str(&quoted[..close]);
                v = quoted.get(close + 1..).unwrap_or_default();
            } else {
                let end = v.find([',', ';', ':']).unwrap_or(v.len());
                value.push_str(&v[..end]);
                v = &v[end..];
            }
            match v.strip_prefix(',') {
                Some(next) => {
                    value.push(',');
                    v = next;
                }
                None => break,
            }
        }
        params.push((param, value));
        rest = v;
    }
    Some(Prop {
        name,
        params,
        value: rest.strip_prefix(':')?.to_owned(),
        line: line.to_owned(),
    })
}

/// TEXT values: `\n` is a line break, and `\,` `\;` `\\` stand for themselves.
fn unescape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.next() {
                Some('n' | 'N') => out.push('\n'),
                Some(other) => out.push(other),
                None => {}
            },
            c => out.push(c),
        }
    }
    out.trim().to_owned()
}

/// The address in a `mailto:` value.
fn mailto(value: &str) -> &str {
    let v = value.trim();
    match v.get(..7) {
        Some(scheme) if scheme.eq_ignore_ascii_case("mailto:") => v[7..].trim(),
        _ => v,
    }
}

fn same(a: &str, b: &str) -> bool {
    a.trim().eq_ignore_ascii_case(b.trim())
}

fn person(p: &Prop) -> Person {
    let address = mailto(&p.value).to_owned();
    Person {
        name: p
            .param("CN")
            .map(|n| n.trim().to_owned())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| address.clone()),
        address,
        answer: p.param("PARTSTAT").and_then(Answer::from_partstat),
    }
}

// ---- times ----

struct Time {
    ms: i64,
    all_day: bool,
    floating: bool,
}

/// A DATE or DATE-TIME value: its day (days since 1970), the seconds into
/// it (`None` for a DATE), and whether it's UTC.
fn stamp(value: &str) -> Option<(i64, Option<i64>, bool)> {
    let value = value.trim();
    let (date, clock) = match value.split_once(['T', 't']) {
        Some((d, t)) => (d, Some(t)),
        None => (value, None),
    };
    let digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
    if date.len() != 8 || !digits(date) {
        return None;
    }
    let day = days_from_civil(
        date[..4].parse().ok()?,
        date[4..6].parse().ok()?,
        date[6..8].parse().ok()?,
    );
    let Some(clock) = clock else {
        return Some((day, None, false));
    };
    let utc = clock.ends_with(['Z', 'z']);
    let clock = clock.trim_end_matches(['Z', 'z']);
    if clock.len() < 6 || !digits(&clock[..6]) {
        return None;
    }
    let field = |r: std::ops::Range<usize>| clock[r].parse::<i64>().ok();
    let secs = field(0..2)? * 3600 + field(2..4)? * 60 + field(4..6)?;
    Some((day, Some(secs), utc))
}

fn time(p: &Prop, cal: &Component) -> Option<Time> {
    let (day, secs, utc) = stamp(&p.value)?;
    let wall = day * 86_400 + secs.unwrap_or(0);
    let at = |ms, all_day, floating| {
        Some(Time {
            ms,
            all_day,
            floating,
        })
    };
    if secs.is_none() {
        return at(wall * 1000, true, false);
    }
    if utc {
        return at(wall * 1000, false, false);
    }
    if let Some(zone) = p.param("TZID")
        && let Some(offset) = zone_offset(cal, zone, wall).or_else(|| utc_zone(zone).then_some(0))
    {
        return at((wall - offset) * 1000, false, false);
    }
    at(wall * 1000, false, true)
}

fn utc_zone(tzid: &str) -> bool {
    matches!(
        tzid.trim().to_ascii_lowercase().as_str(),
        "utc" | "gmt" | "z" | "etc/utc" | "etc/gmt" | "coordinated universal time"
    )
}

/// The UTC offset in seconds of zone `tzid`, as the calendar's VTIMEZONE
/// defines it, at wall-clock time `wall` (seconds, read as UTC): that of
/// the latest STANDARD or DAYLIGHT onset before it.
fn zone_offset(cal: &Component, tzid: &str, wall: i64) -> Option<i64> {
    let zone = cal.children.iter().find(|c| {
        c.name == "VTIMEZONE"
            && c.prop("TZID")
                .is_some_and(|t| t.value.trim() == tzid.trim())
    })?;
    let year = civil_from_days(wall.div_euclid(86_400)).0;
    // (onset, offset): the latest onset so far, and the earliest one,
    // whose "from" offset applies before every onset.
    let mut latest: Option<(i64, i64)> = None;
    let mut earliest: Option<(i64, i64)> = None;
    for part in zone
        .children
        .iter()
        .filter(|c| c.name == "STANDARD" || c.name == "DAYLIGHT")
    {
        let Some(to) = part.prop("TZOFFSETTO").and_then(|p| offset(&p.value)) else {
            continue;
        };
        let from = part
            .prop("TZOFFSETFROM")
            .and_then(|p| offset(&p.value))
            .unwrap_or(to);
        let Some((day, secs, _)) = part.prop("DTSTART").and_then(|p| stamp(&p.value)) else {
            continue;
        };
        let secs = secs.unwrap_or(0);
        let first = day * 86_400 + secs;
        let mut onsets = vec![first];
        if let Some(rule) = part.prop("RRULE") {
            onsets.extend(
                [year - 1, year]
                    .into_iter()
                    .filter_map(|y| yearly_onset(&rule.value, y, secs))
                    .filter(|&at| at >= first),
            );
        }
        for rdate in part.props.iter().filter(|p| p.name == "RDATE") {
            onsets.extend(
                rdate
                    .value
                    .split(',')
                    .filter_map(stamp)
                    .map(|(d, s, _)| d * 86_400 + s.unwrap_or(secs)),
            );
        }
        for at in onsets.into_iter().filter(|&at| at <= wall) {
            if latest.is_none_or(|(l, _)| at > l) {
                latest = Some((at, to));
            }
        }
        if earliest.is_none_or(|(e, _)| first < e) {
            earliest = Some((first, from));
        }
    }
    latest.or(earliest).map(|(_, offset)| offset)
}

/// `+0100`, `-0500` or `+053000` in seconds.
fn offset(value: &str) -> Option<i64> {
    let v = value.trim();
    let (sign, digits) = match v.split_at_checked(1)? {
        ("+", d) => (1, d),
        ("-", d) => (-1, d),
        _ => return None,
    };
    if digits.len() < 4 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let n = |r: std::ops::Range<usize>| digits.get(r).and_then(|s| s.parse::<i64>().ok());
    Some(sign * (n(0..2)? * 3600 + n(2..4)? * 60 + n(4..6).unwrap_or(0)))
}

/// When a yearly time zone rule (`FREQ=YEARLY;BYMONTH=3;BYDAY=2SU`, or
/// `-1SU`, `BYMONTHDAY=…`) starts in `year`, at `secs` into the day, as
/// wall-clock seconds; `None` past its UNTIL.
fn yearly_onset(rule: &str, year: i64, secs: i64) -> Option<i64> {
    let get = |key: &str| {
        rule.split(';').find_map(|part| {
            let (k, v) = part.split_once('=')?;
            k.trim().eq_ignore_ascii_case(key).then(|| v.trim())
        })
    };
    if !get("FREQ")?.eq_ignore_ascii_case("YEARLY") {
        return None;
    }
    let month: i64 = get("BYMONTH")?.split(',').next()?.parse().ok()?;
    let month_days: Vec<i64> = get("BYMONTHDAY")
        .map(|d| d.split(',').filter_map(|n| n.trim().parse().ok()).collect())
        .unwrap_or_default();
    let day = match get("BYDAY") {
        Some(by_day) => {
            let by_day = by_day.split(',').next()?.trim();
            let (nth, code) = by_day.split_at_checked(by_day.len().checked_sub(2)?)?;
            let weekday = ["MO", "TU", "WE", "TH", "FR", "SA", "SU"]
                .iter()
                .position(|d| d.eq_ignore_ascii_case(code))? as i64;
            let nth: i64 = match nth {
                "" | "+" => get("BYSETPOS").and_then(|p| p.parse().ok()).unwrap_or(0),
                n => n.trim_start_matches('+').parse().ok()?,
            };
            if nth == 0 {
                // `BYMONTHDAY=8,…,14;BYDAY=SU`: the listed day that's a Sunday.
                month_days
                    .iter()
                    .map(|&d| days_from_civil(year, month, d))
                    .find(|&d| weekday_of(d) == weekday)?
            } else {
                nth_weekday(year, month, weekday, nth)
            }
        }
        None => days_from_civil(year, month, *month_days.first()?),
    };
    let at = day * 86_400 + secs;
    if let Some((until_day, until_secs, _)) = get("UNTIL").and_then(stamp)
        && at > until_day * 86_400 + until_secs.unwrap_or(86_399)
    {
        return None;
    }
    Some(at)
}

/// Monday is 0; 1970-01-01 was a Thursday.
fn weekday_of(day: i64) -> i64 {
    (day + 3).rem_euclid(7)
}

/// The `nth` `weekday` of the month (counting from its end when negative).
fn nth_weekday(year: i64, month: i64, weekday: i64, nth: i64) -> i64 {
    if nth > 0 {
        let first = days_from_civil(year, month, 1);
        first + (weekday - weekday_of(first)).rem_euclid(7) + 7 * (nth - 1)
    } else {
        let (y, m) = if month == 12 {
            (year + 1, 1)
        } else {
            (year, month + 1)
        };
        let last = days_from_civil(y, m, 1) - 1;
        last - (weekday_of(last) - weekday).rem_euclid(7) - 7 * (-nth - 1)
    }
}

/// `PT1H30M`, `P1D`, `P2W` in ms.
fn duration_ms(value: &str) -> Option<i64> {
    let v = value.trim();
    let (sign, v) = match v.strip_prefix('-') {
        Some(rest) => (-1, rest),
        None => (1, v.trim_start_matches('+')),
    };
    let v = v.strip_prefix(['P', 'p'])?;
    let mut total = 0;
    let mut number = String::new();
    for c in v.chars() {
        match c.to_ascii_uppercase() {
            '0'..='9' => number.push(c),
            'T' => {}
            unit => {
                let n: i64 = number.parse().ok()?;
                number.clear();
                total += n * match unit {
                    'W' => 7 * 86_400,
                    'D' => 86_400,
                    'H' => 3600,
                    'M' => 60,
                    'S' => 1,
                    _ => return None,
                };
            }
        }
    }
    Some(sign * total * 1000)
}

/// `20261023T171500Z` from Unix ms.
fn utc_stamp(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let (y, m, d) = civil_from_days(secs.div_euclid(86_400));
    let t = secs.rem_euclid(86_400);
    format!(
        "{y:04}{m:02}{d:02}T{:02}{:02}{:02}Z",
        t / 3600,
        t % 3600 / 60,
        t % 60
    )
}

// ---- descriptions and call links ----

/// Calendars append a block of joining instructions to the description
/// (Google between `-::~:~::~` rules, Teams and Zoom under a rule of
/// underscores); the card shows the call link instead.
pub(crate) fn without_boilerplate(description: &str) -> String {
    let lines: Vec<&str> = description.lines().collect();
    let cut = lines
        .iter()
        .position(|l| {
            let l = l.trim();
            l.starts_with("-::~:~::~") || (l.len() >= 20 && l.chars().all(|c| c == '_'))
        })
        .unwrap_or(lines.len());
    lines[..cut].join("\n").trim().to_owned()
}

/// The event's call link: the one its calendar names (Google Meet, Teams,
/// RFC 7986 CONFERENCE), else a meeting URL in its location or description.
fn call_link(
    event: &Component,
    location: Option<&str>,
    description: Option<&str>,
) -> Option<String> {
    for name in [
        "X-GOOGLE-CONFERENCE",
        "X-MICROSOFT-SKYPETEAMSMEETINGURL",
        "X-MICROSOFT-ONLINEMEETINGEXTERNALLINK",
        "CONFERENCE",
    ] {
        if let Some(url) = event
            .props
            .iter()
            .filter(|p| p.name == name)
            .map(|p| p.value.trim())
            .find(|v| v.starts_with("https://"))
        {
            return Some(url.to_owned());
        }
    }
    location
        .and_then(meeting_url)
        .or_else(|| description.and_then(meeting_url))
}

/// Hosts whose links are video calls.
const MEETING_HOSTS: &[&str] = &[
    "meet.google.com",
    "teams.microsoft.com",
    "teams.live.com",
    "zoom.us",
    "zoomgov.com",
    "webex.com",
    "gotomeeting.com",
    "goto.com",
    "whereby.com",
    "chime.aws",
    "bluejeans.com",
];

/// The first URL in `text` on a meeting host (or a subdomain of one).
pub(crate) fn meeting_url(text: &str) -> Option<String> {
    text.match_indices("https://").find_map(|(at, _)| {
        let rest = &text[at..];
        let end = rest
            .find(|c: char| c.is_whitespace() || matches!(c, '<' | '>' | '"' | '\'' | ')' | ']'))
            .unwrap_or(rest.len());
        let url = rest[..end].trim_end_matches(['.', ',', ';']);
        let host = url["https://".len()..]
            .split(['/', '?', '#', ':'])
            .next()?
            .to_ascii_lowercase();
        MEETING_HOSTS
            .iter()
            .any(|h| host == *h || host.ends_with(&format!(".{h}")))
            .then(|| url.to_owned())
    })
}

// ---- writing ----

fn attendee_line(original: Option<&Prop>, me: &str, name: &str, answer: Answer) -> String {
    let mut line = String::from("ATTENDEE");
    let mut named = false;
    for (param, value) in original.map(|p| p.params.as_slice()).unwrap_or_default() {
        if matches!(param.as_str(), "CN" | "CUTYPE" | "ROLE") {
            named |= param == "CN";
            line.push_str(&format!(";{param}={}", param_value(value)));
        }
    }
    if !named && !name.trim().is_empty() {
        line.push_str(&format!(";CN={}", param_value(name.trim())));
    }
    let address = original.map_or(me, |p| mailto(&p.value));
    line.push_str(&format!(";PARTSTAT={}:mailto:{address}", answer.partstat()));
    line
}

fn param_value(value: &str) -> String {
    let value = value.replace('"', "");
    if value.contains([':', ';', ',']) {
        format!("\"{value}\"")
    } else {
        value
    }
}

/// A content line folded at 75 octets, with its CRLF.
fn fold(line: &str) -> String {
    let mut out = String::with_capacity(line.len() + 8);
    let mut width = 0;
    for c in line.chars() {
        if width + c.len_utf8() > 75 {
            out.push_str("\r\n ");
            width = 1;
        }
        out.push(c);
        width += c.len_utf8();
    }
    out.push_str("\r\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Outlook's way: a Windows zone name the calendar defines itself.
    const OUTLOOK: &str = "BEGIN:VCALENDAR\r\n\
METHOD:REQUEST\r\n\
PRODID:Microsoft Exchange Server 2010\r\n\
VERSION:2.0\r\n\
BEGIN:VTIMEZONE\r\n\
TZID:Pacific Standard Time\r\n\
BEGIN:STANDARD\r\n\
DTSTART:16010101T020000\r\n\
TZOFFSETFROM:-0700\r\n\
TZOFFSETTO:-0800\r\n\
RRULE:FREQ=YEARLY;INTERVAL=1;BYDAY=1SU;BYMONTH=11\r\n\
END:STANDARD\r\n\
BEGIN:DAYLIGHT\r\n\
DTSTART:16010101T020000\r\n\
TZOFFSETFROM:-0800\r\n\
TZOFFSETTO:-0700\r\n\
RRULE:FREQ=YEARLY;INTERVAL=1;BYDAY=2SU;BYMONTH=3\r\n\
END:DAYLIGHT\r\n\
END:VTIMEZONE\r\n\
BEGIN:VEVENT\r\n\
ORGANIZER;CN=Priya Raman:mailto:priya@contoso.com\r\n\
ATTENDEE;ROLE=REQ-PARTICIPANT;PARTSTAT=NEEDS-ACTION;RSVP=TRUE;CN=Me:mailto:\r\n \
me@contoso.com\r\n\
ATTENDEE;ROLE=OPT-PARTICIPANT;PARTSTAT=ACCEPTED;CN=\"Lee, Sam\":mailto:sam@co\r\n \
ntoso.com\r\n\
ATTENDEE;CUTYPE=ROOM;CN=Room 4:mailto:room4@contoso.com\r\n\
DESCRIPTION;LANGUAGE=en-US:Let's plan Q4\\, together.\\n\\n__________________\r\n \
______________________________________________________________\\nMicrosoft \r\n \
Teams meeting\\nJoin: https://teams.microsoft.com/l/meetup-join/19%3ameeting\r\n \
_abc\\n\r\n\
UID:040000008200E00074C5B7101A82E008\r\n\
SUMMARY;LANGUAGE=en-US:Q4 planning\r\n\
DTSTART;TZID=Pacific Standard Time:20261027T100000\r\n\
DTEND;TZID=Pacific Standard Time:20261027T104500\r\n\
SEQUENCE:2\r\n\
LOCATION;LANGUAGE=en-US:Microsoft Teams Meeting\r\n\
X-MICROSOFT-SKYPETEAMSMEETINGURL:https://teams.microsoft.com/l/meetup-join/19\r\n \
%3ameeting_abc\r\n\
RRULE:FREQ=WEEKLY;BYDAY=TU\r\n\
END:VEVENT\r\n\
END:VCALENDAR\r\n";

    fn utc(y: i64, m: i64, d: i64, h: i64, min: i64) -> i64 {
        (days_from_civil(y, m, d) * 86_400 + h * 3600 + min * 60) * 1000
    }

    #[test]
    fn outlook_invitations_read_with_their_own_time_zone() {
        let invite = Invite::parse(OUTLOOK).unwrap();
        assert_eq!(invite.kind, Kind::Invitation);
        assert_eq!(invite.summary, "Q4 planning");
        assert_eq!(invite.sequence, 2);
        // October 27 is still daylight time: UTC-7.
        assert_eq!(invite.start, utc(2026, 10, 27, 17, 0));
        assert_eq!(invite.end, Some(utc(2026, 10, 27, 17, 45)));
        assert!(!invite.all_day && !invite.floating && invite.repeats);
        assert_eq!(
            invite.description.as_deref(),
            Some("Let's plan Q4, together.")
        );
        assert_eq!(
            invite.call_link.as_deref(),
            Some("https://teams.microsoft.com/l/meetup-join/19%3ameeting_abc")
        );
        let organizer = invite.organizer.as_ref().unwrap();
        assert_eq!(
            (organizer.name.as_str(), organizer.address.as_str()),
            ("Priya Raman", "priya@contoso.com")
        );
        assert_eq!(
            organizer.answer,
            Some(Answer::Accepted),
            "going unless they say otherwise"
        );
        let people: Vec<_> = invite
            .attendees
            .iter()
            .map(|a| (a.name.as_str(), a.address.as_str(), a.answer))
            .collect();
        assert_eq!(
            people,
            [
                ("Me", "me@contoso.com", None),
                ("Lee, Sam", "sam@contoso.com", Some(Answer::Accepted)),
            ],
            "rooms aren't people"
        );
    }

    #[test]
    fn standard_time_follows_the_rule_of_the_year() {
        let winter = OUTLOOK.replace("20261027T10", "20261201T10");
        assert_eq!(
            Invite::parse(&winter).unwrap().start,
            utc(2026, 12, 1, 18, 0)
        );
        // 2027's switch is on March 14: the day before is still UTC-8.
        let spring = OUTLOOK.replace("20261027T10", "20270313T10");
        assert_eq!(
            Invite::parse(&spring).unwrap().start,
            utc(2027, 3, 13, 18, 0)
        );
        let after = OUTLOOK.replace("20261027T10", "20270315T10");
        assert_eq!(
            Invite::parse(&after).unwrap().start,
            utc(2027, 3, 15, 17, 0)
        );
    }

    #[test]
    fn google_invitations_are_in_utc_with_their_meet_link() {
        let ics = "BEGIN:VCALENDAR\r\nPRODID:-//Google Inc//Google Calendar 70.9054//EN\r\nVERSION:2.0\r\n\
METHOD:REQUEST\r\nBEGIN:VEVENT\r\nDTSTART:20261024T140000Z\r\nDTEND:20261024T150000Z\r\n\
DTSTAMP:20261020T120000Z\r\nORGANIZER;CN=ana@gmail.com:mailto:ana@gmail.com\r\nUID:abc123@google.com\r\n\
ATTENDEE;CUTYPE=INDIVIDUAL;ROLE=REQ-PARTICIPANT;PARTSTAT=ACCEPTED;RSVP=TRUE;CN=ana@gmail.com;X-NUM-GUESTS=0:mailto:ana@gmail.com\r\n\
ATTENDEE;CUTYPE=INDIVIDUAL;ROLE=REQ-PARTICIPANT;PARTSTAT=NEEDS-ACTION;RSVP=TRUE;CN=me@gmail.com;X-NUM-GUESTS=0:mailto:me@gmail.com\r\n\
X-GOOGLE-CONFERENCE:https://meet.google.com/abc-defg-hij\r\n\
DESCRIPTION:Bring ideas\\n\\n-::~:~::~:~:~:~:~:~:~:~:~:~:~:~:~:~:~:~:~:~:~:~:~:~:~:~:~:~:~:~:~:~:~:~::~:~::-\\nJoin with Google Meet: https://meet.google.com/abc-defg-hij\r\n\
SEQUENCE:0\r\nSTATUS:CONFIRMED\r\nSUMMARY:Demo\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let invite = Invite::parse(ics).unwrap();
        assert_eq!(invite.start, utc(2026, 10, 24, 14, 0));
        assert_eq!(
            invite.call_link.as_deref(),
            Some("https://meet.google.com/abc-defg-hij")
        );
        assert_eq!(invite.description.as_deref(), Some("Bring ideas"));
        let organizer = invite.organizer.as_ref().unwrap();
        assert_eq!(
            organizer.answer,
            Some(Answer::Accepted),
            "taken from the attendee list"
        );
        assert_eq!(invite.attendees.len(), 1, "the organizer shows once");

        let card = invite.card("ME@gmail.com", None);
        assert!(card.can_answer);
        assert_eq!(card.answer, None);
        let answered = invite.card("me@gmail.com", Some(Answer::Declined));
        assert_eq!(answered.answer, Some(Answer::Declined));
        assert_eq!(answered.attendees[0].answer, Some(Answer::Declined));
        assert!(
            !invite.card("ana@gmail.com", None).can_answer,
            "your own event"
        );
    }

    #[test]
    fn all_day_and_floating_times_stay_as_written() {
        let ics = "BEGIN:VCALENDAR\nMETHOD:PUBLISH\nBEGIN:VEVENT\nUID:1\nSUMMARY:Holiday\n\
DTSTART;VALUE=DATE:20261225\nEND:VEVENT\nEND:VCALENDAR\n";
        let invite = Invite::parse(ics).unwrap();
        assert_eq!(invite.kind, Kind::Event);
        assert!(invite.all_day);
        assert_eq!(
            (invite.start, invite.end),
            (utc(2026, 12, 25, 0, 0), Some(utc(2026, 12, 26, 0, 0)))
        );

        let ics = "BEGIN:VCALENDAR\nMETHOD:CANCEL\nBEGIN:VEVENT\nUID:2\nSUMMARY:Lunch\n\
DTSTART;TZID=Europe/Paris:20261103T120000\nDURATION:PT1H30M\nEND:VEVENT\nEND:VCALENDAR\n";
        let invite = Invite::parse(ics).unwrap();
        assert_eq!(invite.kind, Kind::Cancellation);
        assert!(
            invite.canceled && invite.floating,
            "a zone the calendar doesn't define"
        );
        assert_eq!(invite.end, Some(utc(2026, 11, 3, 13, 30)));
        assert!(!invite.card("me@x.com", None).can_answer);
    }

    #[test]
    fn invites_are_found_in_the_body_or_attached() {
        let raw = format!(
            "From: Priya <priya@contoso.com>\r\nTo: me@contoso.com\r\nSubject: Q4 planning\r\n\
Content-Type: multipart/alternative; boundary=b\r\n\r\n--b\r\nContent-Type: text/plain\r\n\r\nSee you there\r\n\
--b\r\nContent-Type: text/calendar; charset=utf-8; method=REQUEST\r\nContent-Transfer-Encoding: base64\r\n\r\n{}\r\n--b--\r\n",
            base64::Engine::encode(&base64::engine::general_purpose::STANDARD, OUTLOOK)
        );
        assert_eq!(
            Invite::from_raw(raw.as_bytes()).unwrap().summary,
            "Q4 planning"
        );
        let attached = format!(
            "From: a@x.com\r\nSubject: x\r\nContent-Type: multipart/mixed; boundary=m\r\n\r\n--m\r\n\
Content-Type: text/plain\r\n\r\nHi\r\n--m\r\nContent-Type: application/octet-stream; name=invite.ics\r\n\
Content-Disposition: attachment; filename=invite.ics\r\n\r\n{OUTLOOK}\r\n--m--\r\n"
        );
        assert!(Invite::from_raw(attached.as_bytes()).is_some());
        assert!(Invite::from_raw(b"From: a@x.com\r\nSubject: x\r\n\r\nNo calendar").is_none());
    }

    #[test]
    fn replies_name_the_event_and_only_the_one_answering() {
        let now = utc(2026, 10, 23, 17, 15);
        let ics = reply(OUTLOOK, "Me@Contoso.com", "Me", Answer::Tentative, now).unwrap();
        let reply = Invite::parse(&ics).unwrap();
        assert_eq!(reply.kind, Kind::Reply);
        assert_eq!(reply.uid, "040000008200E00074C5B7101A82E008");
        assert_eq!(reply.sequence, 2);
        assert_eq!(
            reply.start,
            utc(2026, 10, 27, 17, 0),
            "its time zone came along"
        );
        assert_eq!(reply.attendees.len(), 1);
        assert_eq!(reply.attendees[0].address, "me@contoso.com");
        assert_eq!(reply.attendees[0].answer, Some(Answer::Tentative));
        assert!(ics.contains("\r\nDTSTAMP:20261023T171500Z\r\n"), "{ics}");
        assert!(!ics.contains("RSVP="));
        assert!(ics.lines().all(|l| l.len() <= 75), "{ics}");
        assert!(
            super::reply(&ics, "me@contoso.com", "Me", Answer::Accepted, now).is_none(),
            "a reply isn't answered"
        );
    }

    #[test]
    fn someone_not_listed_answers_under_their_own_address() {
        let ics = reply(
            OUTLOOK,
            "list@contoso.com",
            "Kai \"K\" Lee",
            Answer::Accepted,
            0,
        )
        .unwrap();
        assert!(
            ics.contains("ATTENDEE;CN=Kai K Lee;PARTSTAT=ACCEPTED:mailto:list@contoso.com"),
            "{ics}"
        );
    }

    #[test]
    fn the_reply_email_carries_the_calendar_twice() {
        let ics = reply(OUTLOOK, "me@contoso.com", "Me", Answer::Accepted, 0).unwrap();
        let message = reply_message(
            "Me <me@contoso.com>".parse().unwrap(),
            "Priya Raman <priya@contoso.com>".parse().unwrap(),
            "Q4 planning",
            Answer::Accepted,
            &ics,
            Some("abc@contoso.com"),
        )
        .unwrap();
        let raw = String::from_utf8(message.formatted()).unwrap();
        assert!(raw.contains("Subject: Accepted: Q4 planning"), "{raw}");
        assert!(raw.contains("In-Reply-To: <abc@contoso.com>"));
        assert!(raw.contains("method=REPLY"), "{raw}");
        assert!(raw.contains("filename=\"reply.ics\""), "{raw}");
        assert!(raw.contains("Me has accepted this invitation."));
        let parsed = MessageParser::default().parse(raw.as_bytes()).unwrap();
        assert_eq!(Invite::find(&parsed).unwrap().kind, Kind::Reply);
    }

    #[test]
    fn meeting_links_are_found_by_host() {
        assert_eq!(
            meeting_url("Join: https://us02web.zoom.us/j/123?pwd=x. Thanks").as_deref(),
            Some("https://us02web.zoom.us/j/123?pwd=x")
        );
        assert_eq!(meeting_url("https://example.com/zoom.us"), None);
        assert_eq!(duration_ms("P1DT2H"), Some(26 * 3_600_000));
        assert_eq!(offset("+0530"), Some(19_800));
    }
}
