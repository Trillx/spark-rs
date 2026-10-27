//! The Microsoft 365 calendar through Graph: the user's calendars, their
//! events over a stretch of time (each occurrence of a recurring event on its
//! own, as Graph's calendar view expands them) with the reminder Outlook set
//! for each, answering an invitation there, which also tells the organizer,
//! and making, changing and deleting the user's own events.
//!
//! Times are asked for in UTC. All-day events keep their days as written,
//! read as UTC midnights, like all-day invites. Events are written in the
//! user's own time zone (`Zone`), as Outlook writes them.

use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::json;
use spark_store::{RemoteCalendar, RemoteEvent};

use crate::Result;
use crate::graph::{Graph, Page, iso_date, parse_iso_date, path};
use crate::invite::{Answer, Person, meeting_url, without_boilerplate};

const DAY_MS: i64 = 86_400_000;

/// Graph's biggest page of events.
const EVENT_PAGE: usize = 250;

const EVENT_SELECT: &str = "id,subject,start,end,isAllDay,location,onlineMeeting,onlineMeetingUrl,\
     organizer,attendees,responseStatus,responseRequested,isCancelled,isOrganizer,showAs,sensitivity,\
     type,body,webLink,isReminderOn,reminderMinutesBeforeStart,seriesMasterId,isOnlineMeeting";

/// The user's answer to an event.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Response {
    /// Their own event.
    Organizer,
    Accepted,
    Tentative,
    Declined,
    /// Not answered yet, or not asked.
    #[default]
    None,
}

/// How the event shows on the user's schedule.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ShowAs {
    Free,
    Tentative,
    #[default]
    Busy,
    Oof,
    WorkingElsewhere,
    Unknown,
}

/// What an event says, stored as JSON with its times; the UI's
/// `CalendarEvent` without the ids.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Event {
    pub title: String,
    pub start: i64,
    pub end: i64,
    pub all_day: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub call_link: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub web_link: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub organizer: Option<Person>,
    pub attendees: Vec<Person>,
    pub response: Response,
    /// The organizer wants to hear back; answering without telling them
    /// fails otherwise.
    pub response_requested: bool,
    pub canceled: bool,
    pub repeats: bool,
    pub show_as: ShowAs,
    pub private: bool,
    /// Outlook reminds the user this many minutes before the start (0: as
    /// it starts); `None` for no reminder. For an all-day event it counts
    /// back from the midnight its day starts, on the user's clock.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reminder: Option<i64>,
    /// The repeating event an occurrence is of (Graph's series master), for
    /// a change to all of them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub series: Option<String>,
    /// A Teams meeting Outlook made for it (`call_link` is its link).
    pub online_meeting: bool,
}

impl Event {
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("an event serializes")
    }

    pub fn from_json(json: &str) -> Self {
        serde_json::from_str(json).unwrap_or_default()
    }
}

// ---- Graph's shapes ----

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphCalendar {
    id: String,
    #[serde(default)]
    name: String,
    hex_color: Option<String>,
    color: Option<String>,
    #[serde(default)]
    is_default_calendar: bool,
    owner: Option<GraphAddress>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct GraphEvent {
    id: String,
    subject: Option<String>,
    start: Option<GraphTime>,
    end: Option<GraphTime>,
    is_all_day: bool,
    location: Option<GraphLocation>,
    online_meeting: Option<GraphOnlineMeeting>,
    online_meeting_url: Option<String>,
    organizer: Option<GraphPerson>,
    attendees: Vec<GraphAttendee>,
    response_status: Option<GraphStatus>,
    response_requested: Option<bool>,
    is_cancelled: bool,
    is_organizer: bool,
    show_as: Option<String>,
    sensitivity: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
    body: Option<GraphBody>,
    web_link: Option<String>,
    is_reminder_on: bool,
    reminder_minutes_before_start: Option<i64>,
    series_master_id: Option<String>,
    is_online_meeting: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphTime {
    date_time: String,
}

/// What an event says that the calendar view doesn't keep, read before
/// changing it: its people as Graph has them (rooms and each one's
/// required or optional kept), and its whole description.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct GraphEventDetails {
    start: Option<GraphTime>,
    end: Option<GraphTime>,
    is_all_day: bool,
    attendees: Vec<serde_json::Value>,
    body: Option<GraphBody>,
}

/// An event's times, people and description (HTML) as Graph has them
/// (`Graph::event_details`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EventDetails {
    /// Unix ms; an all-day event's as written, read as UTC midnights.
    pub start: i64,
    pub end: i64,
    pub attendees: Vec<serde_json::Value>,
    pub body: String,
}

/// Which occurrences of a repeating event a change is for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Scope {
    /// Just the one (or the event, if it doesn't repeat).
    #[default]
    One,
    /// Every occurrence: the series.
    All,
}

/// The user's time zone, as an event is written in it: its Windows name,
/// which Graph takes, and the wall-clock time there of a Unix ms time.
#[derive(Clone, Copy, Debug)]
pub struct Zone<'a> {
    pub name: &'a str,
    pub wall: fn(i64) -> String,
}

impl Zone<'_> {
    /// UTC, where the user's own zone isn't known.
    pub const UTC: Zone<'static> = Zone {
        name: "UTC",
        wall: utc_wall,
    };

    /// Graph's time for `ms`; an all-day event's day (a UTC midnight)
    /// starts at midnight on the user's clock.
    fn time(&self, ms: i64, all_day: bool) -> serde_json::Value {
        let wall = if all_day {
            utc_wall(ms)
        } else {
            (self.wall)(ms)
        };
        json!({ "dateTime": wall, "timeZone": self.name })
    }
}

/// `ms` as wall-clock time in UTC, without the zone: "2026-10-27T09:30:00".
pub fn utc_wall(ms: i64) -> String {
    iso_date(ms).trim_end_matches('Z').to_owned()
}

/// An event as the editor saves it: all of what it says. Times are as in
/// `Event`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct EventDraft {
    /// The calendar (store id) a new event goes in.
    pub calendar: i64,
    pub title: String,
    pub start: i64,
    pub end: i64,
    pub all_day: bool,
    pub location: String,
    pub notes: String,
    /// Everyone invited, besides the user.
    pub attendees: Vec<Person>,
    /// As `Event::reminder`.
    pub reminder: Option<i64>,
    pub show_as: ShowAs,
    /// With a Teams meeting.
    pub teams: bool,
}

impl EventDraft {
    /// Stored event `e` as the editor would open it, in calendar `calendar`.
    pub fn of(e: &Event, calendar: i64) -> Self {
        Self {
            calendar,
            title: e.title.clone(),
            start: e.start,
            end: e.end,
            all_day: e.all_day,
            location: e.location.clone().unwrap_or_default(),
            notes: e.notes.clone().unwrap_or_default(),
            attendees: e.attendees.clone(),
            reminder: e.reminder,
            show_as: e.show_as,
            teams: e.online_meeting,
        }
    }
}

/// The Graph fields of a new event saying what `draft` says.
pub fn new_event_body(draft: &EventDraft, zone: Zone) -> serde_json::Value {
    let mut body = serde_json::Map::new();
    body.insert("subject".into(), json!(draft.title.trim()));
    body.insert("start".into(), zone.time(draft.start, draft.all_day));
    body.insert("end".into(), zone.time(draft.end, draft.all_day));
    body.insert("isAllDay".into(), json!(draft.all_day));
    if !draft.location.trim().is_empty() {
        body.insert(
            "location".into(),
            json!({ "displayName": draft.location.trim() }),
        );
    }
    if !draft.notes.trim().is_empty() {
        body.insert(
            "body".into(),
            json!({ "contentType": "text", "content": draft.notes.trim() }),
        );
    }
    body.insert(
        "attendees".into(),
        json!(draft.attendees.iter().map(attendee).collect::<Vec<_>>()),
    );
    insert_reminder(&mut body, draft.reminder);
    body.insert("showAs".into(), json!(show_as(draft.show_as)));
    if draft.teams {
        body.insert("isOnlineMeeting".into(), json!(true));
        body.insert("onlineMeetingProvider".into(), json!("teamsForBusiness"));
    }
    body.into()
}

/// The Graph fields to change for an event the editor opened as `before`
/// to say what `draft` says: only what the user changed, so the rest stays
/// as Outlook has it now, even if it changed meanwhile. `details` are the
/// event's as Graph has them now (`Graph::event_details`): the people
/// invited there, rooms and each one's required or optional, have the
/// user's additions and removals applied, and the Teams meeting Outlook put
/// after its notes stays. With `shift`, `details`' times (the series' first
/// occurrence, for every occurrence) move by as much as the draft's moved
/// from `before`'s, start and end each.
pub fn changed_event_body(
    draft: &EventDraft,
    before: &EventDraft,
    details: &EventDetails,
    zone: Zone,
    shift: bool,
) -> serde_json::Value {
    let mut body = serde_json::Map::new();
    if draft.title.trim() != before.title.trim() {
        body.insert("subject".into(), json!(draft.title.trim()));
    }
    if (draft.start, draft.end, draft.all_day) != (before.start, before.end, before.all_day) {
        // The series' own first time moves by as much as this occurrence's
        // did on the clock (days and hours), so its own length stays and a
        // DST change between them doesn't move it an hour.
        let moved = |series: i64, was: i64, now: i64| {
            if !shift {
                zone.time(now, draft.all_day)
            } else if draft.all_day {
                zone.time(series + (now - was), true)
            } else {
                // Clock times, counted as if they were UTC's.
                let clock = |t: i64| parse_iso_date(&format!("{}Z", (zone.wall)(t))).unwrap_or(t);
                let at = clock(series) + (clock(now) - clock(was));
                json!({ "dateTime": utc_wall(at), "timeZone": zone.name })
            }
        };
        body.insert(
            "start".into(),
            moved(details.start, before.start, draft.start),
        );
        body.insert("end".into(), moved(details.end, before.end, draft.end));
        body.insert("isAllDay".into(), json!(draft.all_day));
    }
    if draft.location.trim() != before.location.trim() {
        body.insert(
            "location".into(),
            json!({ "displayName": draft.location.trim() }),
        );
    }
    if draft.notes.trim() != before.notes.trim() {
        // A Teams meeting's joining details stay as Outlook wrote them:
        // without them, Outlook turns the meeting off.
        let content = match with_notes(&details.body, draft.notes.trim()) {
            Some(html) => json!({ "contentType": "html", "content": html }),
            None => json!({ "contentType": "text", "content": draft.notes.trim() }),
        };
        body.insert("body".into(), content);
    }
    let key = |p: &Person| p.address.trim().to_lowercase();
    let wanted: Vec<String> = draft.attendees.iter().map(key).collect();
    let had: Vec<String> = before.attendees.iter().map(key).collect();
    if wanted != had {
        // Those the user took off go, those they added come; everyone else
        // stays as they are now (rooms, required or optional).
        let address = |a: &serde_json::Value| {
            a["emailAddress"]["address"]
                .as_str()
                .unwrap_or_default()
                .trim()
                .to_lowercase()
        };
        let mut attendees: Vec<serde_json::Value> = details
            .attendees
            .iter()
            .filter(|a| {
                a["type"] == "resource"
                    || !had.contains(&address(a))
                    || wanted.contains(&address(a))
            })
            .cloned()
            .collect();
        for p in &draft.attendees {
            if !had.contains(&key(p)) && !attendees.iter().any(|a| address(a) == key(p)) {
                attendees.push(attendee(p));
            }
        }
        body.insert("attendees".into(), json!(attendees));
    }
    if draft.reminder != before.reminder {
        insert_reminder(&mut body, draft.reminder);
    }
    if draft.show_as != before.show_as {
        body.insert("showAs".into(), json!(show_as(draft.show_as)));
    }
    if draft.teams && !before.teams {
        body.insert("isOnlineMeeting".into(), json!(true));
        body.insert("onlineMeetingProvider".into(), json!("teamsForBusiness"));
    }
    body.into()
}

/// `html`, an event's description, with `notes` in place of what comes
/// before the Teams meeting Outlook put in it (found by its rule of
/// underscores or its `-::~:~::~` marker). The meeting is the outermost
/// element around that with nothing to read before it in it; it, and all
/// after it, stays as it is, and the notes are what comes before it in the
/// element around it (Outlook's `WordSection1`, or the body). `None` if
/// there's no meeting in it.
fn with_notes(html: &str, notes: &str) -> Option<String> {
    const VOID: &[&str] = &[
        "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param",
        "source", "track", "wbr",
    ];
    let lower = html.to_ascii_lowercase();
    let marker = [lower.find(&"_".repeat(20)), lower.find("-::~:~::~")]
        .into_iter()
        .flatten()
        .min()?;
    let body = lower
        .find("<body")
        .and_then(|at| lower[at..].find('>').map(|end| at + end + 1))
        .unwrap_or(0);
    if marker < body {
        return None;
    }
    // The elements open where the meeting starts, outermost first: where
    // each starts, and where its content does.
    let mut open: Vec<(usize, usize)> = Vec::new();
    let mut at = body;
    while let Some(start) = lower[at..marker].find('<').map(|i| at + i) {
        let Some(end) = lower[start..].find('>').map(|i| start + i + 1) else {
            break;
        };
        let tag = &lower[start + 1..end - 1];
        let name: String = tag
            .trim_start_matches('/')
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric())
            .collect();
        if tag.starts_with('/') {
            open.pop();
        } else if !tag.starts_with('!') && !tag.ends_with('/') && !VOID.contains(&name.as_str()) {
            open.push((start, end));
        }
        at = end;
        if at >= marker {
            break;
        }
    }
    let (cut, from) = match open
        .iter()
        .position(|&(_, content)| !has_text(&html[content..marker]))
    {
        Some(i) => (open[i].0, if i == 0 { body } else { open[i - 1].1 }),
        None => (marker, open.last().map_or(body, |&(_, content)| content)),
    };
    let escape = |s: &str| {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    };
    let lines: String = notes
        .lines()
        .map(|l| match l.trim_end() {
            "" => "<div><br></div>".to_owned(),
            l => format!("<div>{}</div>", escape(l)),
        })
        .collect();
    let gap = if lines.is_empty() {
        ""
    } else {
        "<div><br></div>"
    };
    Some(format!("{}{lines}{gap}{}", &html[..from], &html[cut..]))
}

/// Whether HTML has something to read in it, not only tags and spaces.
fn has_text(html: &str) -> bool {
    let mut inside = false;
    let mut text = String::new();
    for c in html.chars() {
        match c {
            '<' => inside = true,
            '>' => inside = false,
            c if !inside => text.push(c),
            _ => {}
        }
    }
    !text
        .replace("&nbsp;", " ")
        .replace('\u{a0}', " ")
        .trim()
        .is_empty()
}

fn attendee(p: &Person) -> serde_json::Value {
    let address = p.address.trim();
    let name = Some(p.name.trim()).filter(|n| !n.is_empty() && *n != address);
    json!({
        "emailAddress": { "address": address, "name": name.unwrap_or(address) },
        "type": "required",
    })
}

fn insert_reminder(body: &mut serde_json::Map<String, serde_json::Value>, reminder: Option<i64>) {
    body.insert("isReminderOn".into(), json!(reminder.is_some()));
    if let Some(minutes) = reminder {
        body.insert("reminderMinutesBeforeStart".into(), json!(minutes.max(0)));
    }
}

fn show_as(s: ShowAs) -> &'static str {
    match s {
        ShowAs::Free => "free",
        ShowAs::Tentative => "tentative",
        ShowAs::Busy | ShowAs::Unknown => "busy",
        ShowAs::Oof => "oof",
        ShowAs::WorkingElsewhere => "workingElsewhere",
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphLocation {
    display_name: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphOnlineMeeting {
    join_url: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphPerson {
    #[serde(default)]
    email_address: GraphAddress,
}

#[derive(Debug, Default, Deserialize)]
struct GraphAddress {
    name: Option<String>,
    address: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphAttendee {
    #[serde(rename = "type")]
    kind: Option<String>,
    status: Option<GraphStatus>,
    #[serde(default)]
    email_address: GraphAddress,
}

#[derive(Debug, Deserialize)]
struct GraphStatus {
    response: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GraphBody {
    content: Option<String>,
}

impl Graph {
    /// The user's calendars, as Outlook lists them; `me` is the account's
    /// address, which owns the user's own calendars.
    pub async fn calendars(&self, me: &str) -> Result<Vec<RemoteCalendar>> {
        let mut next = Some(
            "/me/calendars?$top=250&$select=id,name,hexColor,color,isDefaultCalendar,owner"
                .to_owned(),
        );
        let mut calendars = Vec::new();
        while let Some(url) = next.take() {
            let page: Page<GraphCalendar> = self.json(self.request(Method::GET, &url)).await?;
            calendars.extend(page.value.into_iter().map(|c| {
                RemoteCalendar {
                    color: calendar_color(c.hex_color.as_deref(), c.color.as_deref()),
                    // The main calendar is always the user's, whichever of their
                    // addresses Outlook gives as its owner.
                    own: c.is_default_calendar
                        || c.owner
                            .and_then(|o| o.address)
                            .is_some_and(|a| a.trim().eq_ignore_ascii_case(me)),
                    remote_id: c.id,
                    name: c.name,
                    is_default: c.is_default_calendar,
                }
            }));
            next = page.next_link;
        }
        Ok(calendars)
    }

    /// The events of calendar `id` overlapping [start, end) (Unix ms).
    pub async fn events(&self, id: &str, start: i64, end: i64) -> Result<Vec<RemoteEvent>> {
        let mut next = Some(format!(
            "/me/calendars/{}/calendarView?startDateTime={}&endDateTime={}&$top={EVENT_PAGE}&$select={EVENT_SELECT}",
            path(id),
            iso_date(start),
            iso_date(end)
        ));
        let mut events = Vec::new();
        while let Some(url) = next.take() {
            let page: Page<GraphEvent> = self
                .json(
                    self.request(Method::GET, &url)
                        .header("Prefer", "outlook.timezone=\"UTC\"")
                        .header("Prefer", "outlook.body-content-type=\"text\""),
                )
                .await?;
            events.extend(page.value.into_iter().filter_map(|e| {
                let remote_id = e.id.clone();
                let event = to_event(e)?;
                Some(RemoteEvent {
                    remote_id,
                    start: event.start,
                    end: event.end,
                    data: event.to_json(),
                })
            }));
            next = page.next_link;
        }
        Ok(events)
    }

    /// Answers event `id` (an occurrence answers just that occurrence); the
    /// organizer hears about it if they asked to.
    pub async fn answer_event(&self, id: &str, answer: Answer, tell_organizer: bool) -> Result<()> {
        let action = match answer {
            Answer::Accepted => "accept",
            Answer::Tentative => "tentativelyAccept",
            Answer::Declined => "decline",
        };
        self.send(
            self.request(Method::POST, &format!("/me/events/{}/{action}", path(id)))
                .json(&json!({ "sendResponse": tell_organizer })),
        )
        .await?;
        Ok(())
    }

    /// Makes an event in calendar `calendar` (`new_event_body`); Outlook
    /// sends the invitations. Returns its id.
    pub async fn create_event(&self, calendar: &str, body: &serde_json::Value) -> Result<String> {
        #[derive(Deserialize)]
        struct Created {
            id: String,
        }
        let created: Created = self
            .json(
                self.request(
                    Method::POST,
                    &format!("/me/calendars/{}/events", path(calendar)),
                )
                .json(body),
            )
            .await?;
        Ok(created.id)
    }

    /// Changes event `id` (an occurrence, or a series) as `body` says
    /// (`changed_event_body`); Outlook sends the people invited the update.
    pub async fn update_event(&self, id: &str, body: &serde_json::Value) -> Result<()> {
        self.send(
            self.request(Method::PATCH, &format!("/me/events/{}", path(id)))
                .json(body),
        )
        .await?;
        Ok(())
    }

    /// Deletes event `id` (an occurrence, or a series); the people invited
    /// to a meeting the user organized hear it's canceled.
    pub async fn delete_event(&self, id: &str) -> Result<()> {
        self.send(self.request(Method::DELETE, &format!("/me/events/{}", path(id))))
            .await?;
        Ok(())
    }

    /// Event `id`'s times, people and description as Graph has them now.
    pub async fn event_details(&self, id: &str) -> Result<EventDetails> {
        let e: GraphEventDetails = self
            .json(
                self.request(
                    Method::GET,
                    &format!(
                        "/me/events/{}?$select=start,end,isAllDay,attendees,body",
                        path(id)
                    ),
                )
                .header("Prefer", "outlook.timezone=\"UTC\""),
            )
            .await?;
        let all_day = e.is_all_day;
        let time = |t: Option<GraphTime>| {
            let ms = t.and_then(|t| parse_iso_date(&t.date_time))?;
            Some(if all_day { nearest_midnight(ms) } else { ms })
        };
        Ok(EventDetails {
            start: time(e.start).unwrap_or_default(),
            end: time(e.end).unwrap_or_default(),
            attendees: e.attendees,
            body: e.body.and_then(|b| b.content).unwrap_or_default(),
        })
    }
}

/// An event from Graph; `None` without its times.
fn to_event(e: GraphEvent) -> Option<Event> {
    let mut start = parse_iso_date(&e.start?.date_time)?;
    let mut end = parse_iso_date(&e.end?.date_time)?;
    if e.is_all_day {
        // All-day events can come shifted by their own time zone's offset.
        start = nearest_midnight(start);
        end = nearest_midnight(end).max(start + DAY_MS);
    }
    let location = e
        .location
        .and_then(|l| l.display_name)
        .map(|l| l.trim().to_owned())
        .filter(|l| !l.is_empty());
    let body = e.body.and_then(|b| b.content).unwrap_or_default();
    let call_link = e
        .online_meeting
        .and_then(|m| m.join_url)
        .or(e.online_meeting_url)
        .filter(|u| u.starts_with("https://"))
        .or_else(|| location.as_deref().and_then(meeting_url))
        .or_else(|| meeting_url(&body));
    let notes = Some(without_boilerplate(&body)).filter(|n| !n.is_empty());
    let organizer = e.organizer.and_then(|o| person(o.email_address, None));
    let attendees = e
        .attendees
        .into_iter()
        .filter(|a| a.kind.as_deref() != Some("resource"))
        .filter_map(|a| {
            let answer = a
                .status
                .and_then(|s| s.response)
                .as_deref()
                .and_then(answer_of);
            person(a.email_address, answer)
        })
        .filter(|p| {
            organizer
                .as_ref()
                .is_none_or(|o| !o.address.eq_ignore_ascii_case(&p.address))
        })
        .collect();
    let response = if e.is_organizer {
        Response::Organizer
    } else {
        match e.response_status.and_then(|s| s.response).as_deref() {
            Some("organizer") => Response::Organizer,
            Some("accepted") => Response::Accepted,
            Some("tentativelyAccepted") => Response::Tentative,
            Some("declined") => Response::Declined,
            _ => Response::None,
        }
    };
    Some(Event {
        title: e.subject.map(|s| s.trim().to_owned()).unwrap_or_default(),
        start,
        end,
        all_day: e.is_all_day,
        location,
        call_link,
        notes,
        web_link: e.web_link,
        organizer: organizer.map(|mut o| {
            o.answer = Some(Answer::Accepted);
            o
        }),
        attendees,
        response,
        response_requested: e.response_requested.unwrap_or(true),
        canceled: e.is_cancelled,
        repeats: matches!(e.kind.as_deref(), Some("occurrence" | "exception")),
        show_as: match e.show_as.as_deref() {
            Some("free") => ShowAs::Free,
            Some("tentative") => ShowAs::Tentative,
            Some("busy") => ShowAs::Busy,
            Some("oof") => ShowAs::Oof,
            Some("workingElsewhere") => ShowAs::WorkingElsewhere,
            _ => ShowAs::Unknown,
        },
        private: matches!(e.sensitivity.as_deref(), Some("private" | "confidential")),
        reminder: e
            .is_reminder_on
            .then_some(e.reminder_minutes_before_start)
            .flatten()
            .filter(|m| *m >= 0),
        series: e.series_master_id.filter(|id| !id.is_empty()),
        online_meeting: e.is_online_meeting,
    })
}

fn answer_of(response: &str) -> Option<Answer> {
    match response {
        "accepted" | "organizer" => Some(Answer::Accepted),
        "tentativelyAccepted" => Some(Answer::Tentative),
        "declined" => Some(Answer::Declined),
        _ => None,
    }
}

fn person(a: GraphAddress, answer: Option<Answer>) -> Option<Person> {
    let address = a
        .address
        .map(|a| a.trim().to_owned())
        .filter(|a| !a.is_empty())?;
    Some(Person {
        name: a
            .name
            .map(|n| n.trim().to_owned())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| address.clone()),
        address,
        answer,
    })
}

/// The UTC midnight closest to `ms`: an all-day event's day, wherever in
/// UTC−11…UTC+11 it was made.
fn nearest_midnight(ms: i64) -> i64 {
    (ms + DAY_MS / 2).div_euclid(DAY_MS) * DAY_MS
}

/// Outlook's calendar color: its hex value, else its named preset; `None`
/// ("auto") takes the account's color.
fn calendar_color(hex: Option<&str>, preset: Option<&str>) -> Option<String> {
    if let Some(hex) = hex
        .map(str::trim)
        .filter(|h| h.len() == 7 && h.starts_with('#'))
    {
        return Some(hex.to_ascii_lowercase());
    }
    let hex = match preset? {
        "lightBlue" => "#4f9ff0",
        "lightGreen" => "#5fbe7d",
        "lightOrange" => "#f7954f",
        "lightGray" => "#a0a6ad",
        "lightYellow" => "#e3c13c",
        "lightTeal" => "#3fbfb7",
        "lightPink" => "#ef6ea8",
        "lightBrown" => "#b48a62",
        "lightRed" => "#e8605f",
        _ => return None,
    };
    Some(hex.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn graph_event(json: serde_json::Value) -> Option<Event> {
        to_event(serde_json::from_value(json).unwrap())
    }

    #[test]
    fn graph_events_become_what_the_calendar_shows() {
        let event = graph_event(json!({
            "id": "AAMk1",
            "subject": " Q4 planning ",
            "start": { "dateTime": "2026-10-27T17:00:00.0000000", "timeZone": "UTC" },
            "end": { "dateTime": "2026-10-27T17:45:00.0000000", "timeZone": "UTC" },
            "isAllDay": false,
            "location": { "displayName": "Microsoft Teams Meeting" },
            "onlineMeeting": { "joinUrl": "https://teams.microsoft.com/l/meetup-join/abc" },
            "organizer": { "emailAddress": { "name": "Priya Raman", "address": "priya@contoso.com" } },
            "attendees": [
                { "type": "required", "status": { "response": "accepted" },
                  "emailAddress": { "name": "Sam", "address": "sam@contoso.com" } },
                { "type": "required", "status": { "response": "none" },
                  "emailAddress": { "name": "Priya Raman", "address": "priya@contoso.com" } },
                { "type": "resource", "status": { "response": "accepted" },
                  "emailAddress": { "name": "Room 4", "address": "room4@contoso.com" } }
            ],
            "responseStatus": { "response": "tentativelyAccepted" },
            "responseRequested": true,
            "isOrganizer": false,
            "showAs": "tentative",
            "sensitivity": "normal",
            "type": "occurrence",
            "body": { "contentType": "text", "content": "Agenda in the doc.\n\n________________________________________________________________________________\nMicrosoft Teams meeting" },
            "webLink": "https://outlook.office365.com/owa/?itemid=AAMk1",
            "isReminderOn": true,
            "reminderMinutesBeforeStart": 15
        }))
        .unwrap();
        assert_eq!(event.title, "Q4 planning");
        assert_eq!(event.start, parse_iso_date("2026-10-27T17:00:00Z").unwrap());
        assert_eq!(event.end - event.start, 45 * 60_000);
        assert_eq!(
            event.call_link.as_deref(),
            Some("https://teams.microsoft.com/l/meetup-join/abc")
        );
        assert_eq!(event.notes.as_deref(), Some("Agenda in the doc."));
        assert_eq!(event.response, Response::Tentative);
        assert_eq!(event.show_as, ShowAs::Tentative);
        assert!(event.repeats && !event.private && event.response_requested);
        assert_eq!(event.reminder, Some(15));
        let people: Vec<_> = event
            .attendees
            .iter()
            .map(|p| (p.name.as_str(), p.answer))
            .collect();
        assert_eq!(
            people,
            [("Sam", Some(Answer::Accepted))],
            "no organizer twice, no rooms"
        );
        assert_eq!(Event::from_json(&event.to_json()), event);
    }

    #[test]
    fn all_day_events_keep_their_days() {
        // Made in UTC−7, given in UTC: still October 24 to 26.
        let event = graph_event(json!({
            "id": "AAMk2",
            "subject": "Offsite",
            "start": { "dateTime": "2026-10-24T07:00:00.0000000", "timeZone": "UTC" },
            "end": { "dateTime": "2026-10-26T07:00:00.0000000", "timeZone": "UTC" },
            "isAllDay": true,
            "isOrganizer": true,
            "isReminderOn": false,
            "reminderMinutesBeforeStart": 1080
        }))
        .unwrap();
        assert!(event.all_day);
        assert_eq!(event.start, parse_iso_date("2026-10-24T00:00:00Z").unwrap());
        assert_eq!(event.end, parse_iso_date("2026-10-26T00:00:00Z").unwrap());
        assert_eq!(event.response, Response::Organizer);
        assert_eq!(event.call_link, None);
        assert_eq!(event.reminder, None, "no reminder unless it's on");
        assert!(graph_event(json!({ "id": "x", "subject": "No times" })).is_none());
    }

    #[tokio::test]
    async fn calendars_and_their_events_come_page_by_page() {
        fn reply(path: &str) -> Option<serde_json::Value> {
            let event = |id: &str, day: u32| {
                json!({
                    "id": id, "subject": id, "isAllDay": false, "isOrganizer": true,
                    "start": { "dateTime": format!("2026-10-{day}T09:00:00.0000000") },
                    "end": { "dateTime": format!("2026-10-{day}T10:00:00.0000000") }
                })
            };
            Some(match path {
                p if p.starts_with("/me/calendars?") => json!({ "value": [
                    { "id": "c1", "name": "Calendar", "hexColor": "", "color": "lightBlue", "isDefaultCalendar": true,
                      "owner": { "name": "Me", "address": "alias@contoso.com" } },
                    { "id": "c2", "name": "Projects", "isDefaultCalendar": false,
                      "owner": { "name": "Me", "address": "Me@Contoso.com" } },
                    { "id": "c3", "name": "Sam Okafor", "isDefaultCalendar": false,
                      "owner": { "name": "Sam Okafor", "address": "sam@contoso.com" } }
                ] }),
                p if p.starts_with(
                    "/me/calendars/c1/calendarView?startDateTime=2026-10-26T00:00:00Z&endDateTime=2026-11-02T00:00:00Z&",
                ) => json!({ "value": [event("e1", 27)], "@odata.nextLink": "/me/calendars/c1/calendarView?page=2" }),
                "/me/calendars/c1/calendarView?page=2" => json!({ "value": [event("e2", 28)] }),
                "/me/events/e1/tentativelyAccept" => json!({}),
                _ => return None,
            })
        }
        let (base, seen) = crate::graph::tests::stand_in(reply).await;
        let graph = Graph::with_base(&base, "token");
        let calendars = graph.calendars("me@contoso.com").await.unwrap();
        assert_eq!(calendars.len(), 3);
        assert_eq!(calendars[0].color.as_deref(), Some("#4f9ff0"));
        assert!(calendars[0].is_default);
        let own: Vec<_> = calendars.iter().map(|c| c.own).collect();
        assert_eq!(own, [true, true, false], "not Sam's, shared with me");

        let start = parse_iso_date("2026-10-26T00:00:00Z").unwrap();
        let events = graph.events("c1", start, start + 7 * DAY_MS).await.unwrap();
        let ids: Vec<_> = events.iter().map(|e| e.remote_id.as_str()).collect();
        assert_eq!(ids, ["e1", "e2"]);
        assert_eq!(
            events[1].start,
            parse_iso_date("2026-10-28T09:00:00Z").unwrap()
        );
        assert_eq!(Event::from_json(&events[0].data).title, "e1");

        graph
            .answer_event("e1", Answer::Tentative, true)
            .await
            .unwrap();
        assert_eq!(
            seen.lock().unwrap().last().unwrap(),
            "/me/events/e1/tentativelyAccept"
        );
    }

    /// Wall-clock time in a zone 7 hours behind UTC, standing in for the user's.
    fn pacific(ms: i64) -> String {
        utc_wall(ms - 7 * 3_600_000)
    }

    const PACIFIC: Zone<'static> = Zone {
        name: "Pacific Standard Time",
        wall: pacific,
    };

    fn person(name: &str, address: &str) -> Person {
        Person {
            name: name.into(),
            address: address.into(),
            answer: None,
        }
    }

    #[test]
    fn a_new_event_is_written_in_the_users_zone() {
        let start = parse_iso_date("2026-10-27T16:30:00Z").unwrap();
        let draft = EventDraft {
            calendar: 1,
            title: " Design review ".into(),
            start,
            end: start + 3_600_000,
            location: "Room 4".into(),
            notes: "Bring the mocks.".into(),
            attendees: vec![
                person("Sam", "sam@contoso.com"),
                person("", "kai@contoso.com"),
            ],
            reminder: Some(10),
            teams: true,
            ..Default::default()
        };
        let body = new_event_body(&draft, PACIFIC);
        assert_eq!(body["subject"], "Design review");
        assert_eq!(
            body["start"],
            json!({ "dateTime": "2026-10-27T09:30:00", "timeZone": "Pacific Standard Time" })
        );
        assert_eq!(body["end"]["dateTime"], "2026-10-27T10:30:00");
        assert_eq!(body["isAllDay"], false);
        assert_eq!(body["location"]["displayName"], "Room 4");
        assert_eq!(
            body["body"],
            json!({ "contentType": "text", "content": "Bring the mocks." })
        );
        assert_eq!(
            body["attendees"][1],
            json!({ "emailAddress": { "address": "kai@contoso.com", "name": "kai@contoso.com" }, "type": "required" })
        );
        assert_eq!(body["isReminderOn"], true);
        assert_eq!(body["reminderMinutesBeforeStart"], 10);
        assert_eq!(body["showAs"], "busy");
        assert_eq!(body["onlineMeetingProvider"], "teamsForBusiness");

        // An all-day event's days start at midnight on the user's clock.
        let day = parse_iso_date("2026-10-30T00:00:00Z").unwrap();
        let body = new_event_body(
            &EventDraft {
                start: day,
                end: day + DAY_MS,
                all_day: true,
                reminder: None,
                ..draft
            },
            PACIFIC,
        );
        assert_eq!(body["start"]["dateTime"], "2026-10-30T00:00:00");
        assert_eq!(body["end"]["dateTime"], "2026-10-31T00:00:00");
        assert_eq!(body["isReminderOn"], false);
        assert!(body.get("reminderMinutesBeforeStart").is_none());
    }

    #[test]
    fn a_change_sends_only_what_changed() {
        let start = parse_iso_date("2026-10-27T16:00:00Z").unwrap();
        let before = Event {
            title: "Standup".into(),
            start,
            end: start + 900_000,
            notes: Some("Agenda".into()),
            attendees: vec![
                person("Sam", "sam@contoso.com"),
                person("Kai", "kai@contoso.com"),
            ],
            response: Response::Organizer,
            reminder: Some(15),
            online_meeting: true,
            ..Default::default()
        };
        let details = EventDetails {
            start,
            end: start + 900_000,
            attendees: vec![
                json!({ "type": "optional", "emailAddress": { "name": "Sam", "address": "Sam@contoso.com" } }),
                json!({ "type": "required", "emailAddress": { "name": "Kai", "address": "kai@contoso.com" } }),
                json!({ "type": "resource", "emailAddress": { "name": "Room 4", "address": "room4@contoso.com" } }),
            ],
            body: format!(
                "<html><head><style>p {{ margin: 0 }}</style></head><body><div>Agenda</div>\r\n<div>Second line</div>\r\n\
                 <div style=\"width:100%\"><span style=\"opacity:.36\">{}</span>\
                 <div class=\"me-email-text\">Microsoft Teams meeting <a href=\"https://teams.microsoft.com/l/x\">Join</a></div></div></body></html>",
                "_".repeat(80)
            ),
        };
        let unchanged = EventDraft {
            title: before.title.clone(),
            start: before.start,
            end: before.end,
            notes: "Agenda".into(),
            attendees: before.attendees.clone(),
            reminder: Some(15),
            teams: true,
            ..Default::default()
        };
        assert_eq!(
            changed_event_body(
                &unchanged,
                &EventDraft::of(&before, 0),
                &details,
                PACIFIC,
                false
            ),
            json!({})
        );

        // Moved by an hour, Kai off, Alex on, new notes, no reminder.
        let draft = EventDraft {
            start: start + 3_600_000,
            end: start + 3_600_000 + 900_000,
            notes: "New agenda".into(),
            attendees: vec![
                person("Sam", "sam@contoso.com"),
                person("Alex", "alex@contoso.com"),
            ],
            reminder: None,
            ..unchanged.clone()
        };
        let body = changed_event_body(
            &draft,
            &EventDraft::of(&before, 0),
            &details,
            PACIFIC,
            false,
        );
        let keys: Vec<&str> = body
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            keys,
            [
                "attendees",
                "body",
                "end",
                "isAllDay",
                "isReminderOn",
                "start"
            ],
            "{body}"
        );
        assert_eq!(body["start"]["dateTime"], "2026-10-27T10:00:00");
        let attendees: Vec<(&str, &str)> = body["attendees"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| {
                (
                    a["emailAddress"]["address"].as_str().unwrap(),
                    a["type"].as_str().unwrap(),
                )
            })
            .collect();
        assert_eq!(
            attendees,
            [
                ("Sam@contoso.com", "optional"),
                ("room4@contoso.com", "resource"),
                ("alex@contoso.com", "required")
            ],
            "rooms and kinds kept"
        );
        assert_eq!(body["body"]["contentType"], "html");
        let content = body["body"]["content"].as_str().unwrap();
        let meeting = &details.body[details.body.find("<div style=\"width:100%\">").unwrap()..];
        assert_eq!(
            content,
            format!(
                "<html><head><style>p {{ margin: 0 }}</style></head><body><div>New agenda</div><div><br></div>{meeting}"
            ),
            "the meeting stays as Outlook wrote it"
        );
        // Without a meeting in it, the notes are the description.
        let plain = EventDetails {
            body: "<html><body><p>Old</p></body></html>".into(),
            ..details.clone()
        };
        let body = changed_event_body(&draft, &EventDraft::of(&before, 0), &plain, PACIFIC, false);
        assert_eq!(
            body["body"],
            json!({ "contentType": "text", "content": "New agenda" })
        );

        // Every occurrence: the series moves from its own first time.
        let series_start = start - 7 * DAY_MS;
        let series = EventDetails {
            start: series_start,
            end: series_start + 900_000,
            ..details.clone()
        };
        let body = changed_event_body(&draft, &EventDraft::of(&before, 0), &series, PACIFIC, true);
        assert_eq!(
            body["start"]["dateTime"],
            pacific(series_start + 3_600_000).as_str()
        );
        assert_eq!(
            body["end"]["dateTime"],
            pacific(series_start + 4_500_000).as_str()
        );
        // One occurrence was made shorter than the rest; moving it moves the
        // series' own start and end, which keep their length.
        let long_series = EventDetails {
            start: series_start,
            end: series_start + 3_600_000,
            ..series.clone()
        };
        let body = changed_event_body(
            &draft,
            &EventDraft::of(&before, 0),
            &long_series,
            PACIFIC,
            true,
        );
        assert_eq!(
            body["end"]["dateTime"],
            pacific(series_start + 7_200_000).as_str()
        );
    }

    #[test]
    fn changes_made_in_outlook_meanwhile_stay() {
        // The editor opened with Sam and Kai; Kai was taken off in Outlook
        // since, and a room added. The user adds Alex and renames it.
        let start = parse_iso_date("2026-10-27T16:00:00Z").unwrap();
        let before = EventDraft {
            title: "Sync".into(),
            start,
            end: start + 1_800_000,
            location: "Room 1".into(),
            attendees: vec![
                person("Sam", "sam@contoso.com"),
                person("Kai", "kai@contoso.com"),
            ],
            ..Default::default()
        };
        let details = EventDetails {
            start,
            end: start + 1_800_000,
            attendees: vec![
                json!({ "type": "required", "emailAddress": { "address": "sam@contoso.com" } }),
                json!({ "type": "resource", "emailAddress": { "address": "room4@contoso.com" } }),
            ],
            ..Default::default()
        };
        let renamed = EventDraft {
            title: "Weekly sync".into(),
            ..before.clone()
        };
        let body = changed_event_body(&renamed, &before, &details, PACIFIC, false);
        assert_eq!(
            body,
            json!({ "subject": "Weekly sync" }),
            "nothing else is sent back"
        );
        let more = EventDraft {
            attendees: vec![
                person("Sam", "sam@contoso.com"),
                person("Kai", "kai@contoso.com"),
                person("Alex", "alex@contoso.com"),
            ],
            ..before.clone()
        };
        let body = changed_event_body(&more, &before, &details, PACIFIC, false);
        let invited: Vec<&str> = body["attendees"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a["emailAddress"]["address"].as_str().unwrap())
            .collect();
        assert_eq!(
            invited,
            ["sam@contoso.com", "room4@contoso.com", "alex@contoso.com"],
            "Kai stays off"
        );
    }

    #[test]
    fn notes_wrapped_with_the_meeting_are_replaced_not_kept() {
        let meeting = format!(
            "<div style=\"width:100%\"><span>{}</span><div>Microsoft Teams meeting</div></div>",
            "_".repeat(80)
        );
        let html = format!(
            "<html><body lang=EN-US><div class=WordSection1><p class=MsoNormal>Old agenda</p>\
             <p class=MsoNormal>&nbsp;</p>{meeting}</div></body></html>"
        );
        assert_eq!(
            with_notes(&html, "New agenda").unwrap(),
            format!(
                "<html><body lang=EN-US><div class=WordSection1><div>New agenda</div><div><br></div>{meeting}</div></body></html>"
            )
        );
        assert_eq!(
            with_notes(&html, "").unwrap(),
            format!("<html><body lang=EN-US><div class=WordSection1>{meeting}</div></body></html>"),
            "a note cleared is gone"
        );
        assert_eq!(
            with_notes("<html><body><p>Just notes</p></body></html>", "x"),
            None
        );
    }

    /// New York's clock: four hours behind UTC until November 1, 2026 at 2:00, then five.
    fn eastern(ms: i64) -> String {
        let change = parse_iso_date("2026-11-01T06:00:00Z").unwrap();
        utc_wall(ms - if ms < change { 4 } else { 5 } * 3_600_000)
    }

    #[test]
    fn every_occurrence_keeps_its_clock_time_across_a_dst_change() {
        const EASTERN: Zone<'static> = Zone {
            name: "Eastern Standard Time",
            wall: eastern,
        };
        // Weekly 9:00 to 10:00 from October 5; the November 2 one, after the
        // clocks went back, is moved to October 26 at 9:00, for all of them.
        let series_start = parse_iso_date("2026-10-05T13:00:00Z").unwrap();
        let details = EventDetails {
            start: series_start,
            end: series_start + 3_600_000,
            ..Default::default()
        };
        let occurrence = parse_iso_date("2026-11-02T14:00:00Z").unwrap();
        let before = Event {
            title: "Planning".into(),
            start: occurrence,
            end: occurrence + 3_600_000,
            response: Response::Organizer,
            series: Some("series".into()),
            ..Default::default()
        };
        let to = parse_iso_date("2026-10-26T13:00:00Z").unwrap();
        let draft = EventDraft {
            title: "Planning".into(),
            start: to,
            end: to + 3_600_000,
            ..Default::default()
        };
        let body = changed_event_body(&draft, &EventDraft::of(&before, 0), &details, EASTERN, true);
        assert_eq!(
            body["start"],
            json!({ "dateTime": "2026-09-28T09:00:00", "timeZone": "Eastern Standard Time" })
        );
        assert_eq!(body["end"]["dateTime"], "2026-09-28T10:00:00");
    }

    #[tokio::test]
    async fn events_are_made_changed_and_deleted() {
        use crate::graph::tests::{Answer as Reply, serve};
        let (base, seen) = serve(|asked, _| match asked.method.as_str() {
            "POST" => Reply::json(json!({ "id": "new1" })),
            "GET" => Reply::json(json!({
                "start": { "dateTime": "2026-10-20T08:00:00.0000000" },
                "end": { "dateTime": "2026-10-20T09:00:00.0000000" },
                "isAllDay": false,
                "attendees": [],
                "body": { "content": "Hi" }
            })),
            _ => Reply::json(json!({})),
        })
        .await;
        let graph = Graph::with_base(&base, "token");
        let id = graph
            .create_event("cal/1", &json!({ "subject": "x" }))
            .await
            .unwrap();
        assert_eq!(id, "new1");
        let details = graph.event_details("ev1").await.unwrap();
        assert_eq!(
            details.start,
            parse_iso_date("2026-10-20T08:00:00Z").unwrap()
        );
        assert_eq!(details.body, "Hi");
        graph
            .update_event("ev1", &json!({ "subject": "y" }))
            .await
            .unwrap();
        graph.delete_event("ev1").await.unwrap();
        let asked: Vec<(String, String, String)> = seen
            .take()
            .into_iter()
            .map(|a| (a.method, a.path, a.body))
            .collect();
        assert_eq!(asked[0].0, "POST");
        assert_eq!(asked[0].1, "/me/calendars/cal%2F1/events");
        assert_eq!(asked[0].2, r#"{"subject":"x"}"#);
        assert!(
            asked[1].1.starts_with("/me/events/ev1?$select="),
            "{}",
            asked[1].1
        );
        assert_eq!(
            (
                asked[2].0.as_str(),
                asked[2].1.as_str(),
                asked[2].2.as_str()
            ),
            ("PATCH", "/me/events/ev1", r#"{"subject":"y"}"#)
        );
        assert_eq!(
            (asked[3].0.as_str(), asked[3].1.as_str()),
            ("DELETE", "/me/events/ev1")
        );
    }

    #[test]
    fn calendar_colors_come_from_hex_or_preset() {
        assert_eq!(
            calendar_color(Some("#E3008C"), Some("auto")).as_deref(),
            Some("#e3008c")
        );
        assert_eq!(
            calendar_color(Some(""), Some("lightTeal")).as_deref(),
            Some("#3fbfb7")
        );
        assert_eq!(calendar_color(None, Some("auto")), None);
    }
}
