//! The Microsoft 365 calendar through Graph: the user's calendars, their
//! events over a stretch of time (each occurrence of a recurring event on its
//! own, as Graph's calendar view expands them), and answering an invitation
//! there, which also tells the organizer.
//!
//! Times are asked for in UTC. All-day events keep their days as written,
//! read as UTC midnights, like all-day invites.

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
     type,body,webLink";

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
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphTime {
    date_time: String,
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
    /// The user's calendars, as Outlook lists them.
    pub async fn calendars(&self) -> Result<Vec<RemoteCalendar>> {
        let mut next = Some(
            "/me/calendars?$top=250&$select=id,name,hexColor,color,isDefaultCalendar".to_owned(),
        );
        let mut calendars = Vec::new();
        while let Some(url) = next.take() {
            let page: Page<GraphCalendar> = self.json(self.request(Method::GET, &url)).await?;
            calendars.extend(page.value.into_iter().map(|c| RemoteCalendar {
                color: calendar_color(c.hex_color.as_deref(), c.color.as_deref()),
                remote_id: c.id,
                name: c.name,
                is_default: c.is_default_calendar,
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
            "webLink": "https://outlook.office365.com/owa/?itemid=AAMk1"
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
            "isOrganizer": true
        }))
        .unwrap();
        assert!(event.all_day);
        assert_eq!(event.start, parse_iso_date("2026-10-24T00:00:00Z").unwrap());
        assert_eq!(event.end, parse_iso_date("2026-10-26T00:00:00Z").unwrap());
        assert_eq!(event.response, Response::Organizer);
        assert_eq!(event.call_link, None);
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
                    { "id": "c1", "name": "Calendar", "hexColor": "", "color": "lightBlue", "isDefaultCalendar": true }
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
        let calendars = graph.calendars().await.unwrap();
        assert_eq!(calendars.len(), 1);
        assert_eq!(calendars[0].color.as_deref(), Some("#4f9ff0"));
        assert!(calendars[0].is_default);

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
