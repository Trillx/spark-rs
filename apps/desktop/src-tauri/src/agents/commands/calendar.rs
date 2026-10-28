//! The calendar: `events`, `event` and `availability`. Calendars come from
//! Microsoft 365 accounts; each command reads the range from the server
//! first, so what it lists is current. Event IDs are `e` and a number
//! (`e42`), so `event rsvp` can tell them from an invitation email's ID.

use chrono::{DateTime, Datelike, Duration, Local, NaiveDate, NaiveTime, TimeZone, Weekday};
use serde_json::{Value, json};
use spark_mail::calendar::{Event, EventDraft, Scope, ShowAs, Zone};
use spark_mail::invite::{Answer, Person};
use spark_store::{Calendar, StoredEvent, now_ms};

use super::{
    Args, Done, Env, Fail, Reply, count, db, is_date, iso, message_id, parse_time, refuse, text,
    when,
};
use crate::agents::Access;
use crate::agents::tools::Effect;
use crate::calendar::{local_wall, zone_name};

const DAY: i64 = 86_400_000;
/// The working day free slots are looked for in, as Spark does.
const WORK_START: u32 = 8;
const WORK_END: u32 = 20;
/// Shorter gaps aren't offered as free time.
const MIN_SLOT: i64 = 15 * 60_000;

/// An all-day date as the calendar keeps it: midnight UTC on that date
/// (`2026-11-05`, `05/11/2026` or `2026/11/05`; a time's own date).
fn utc_day(text: &str) -> Result<i64, Fail> {
    let text = text.trim();
    let date = ["%Y-%m-%d", "%d/%m/%Y", "%Y/%m/%d"]
        .iter()
        .find_map(|f| NaiveDate::parse_from_str(text, f).ok())
        .or_else(|| {
            Local
                .timestamp_millis_opt(parse_time(text, false).ok()?)
                .single()
                .map(|t| t.date_naive())
        })
        .ok_or_else(|| Fail::Error(format!("{text} isn't a date. Write yyyy-MM-dd.")))?;
    Ok(date.and_time(NaiveTime::MIN).and_utc().timestamp_millis())
}

/// The UTC date of an all-day boundary.
fn utc_date(ms: i64) -> NaiveDate {
    DateTime::from_timestamp_millis(ms)
        .map(|t| t.date_naive())
        .unwrap_or_default()
}

/// When an event begins and ends on this computer's clock. All-day events
/// are kept as UTC midnights of their dates, which are local midnights here.
fn span(e: &Event) -> (i64, i64) {
    if !e.all_day {
        return (e.start, e.end);
    }
    let local = |ms: i64| {
        let day = utc_date(ms).and_time(NaiveTime::MIN);
        Local
            .from_local_datetime(&day)
            .earliest()
            .map_or(ms, |t| t.timestamp_millis())
    };
    (local(e.start), local(e.end))
}

/// An event's start and end for JSON: times, or for an all-day event the
/// first and the last day it covers.
fn bounds(start: i64, end: i64, all_day: bool) -> (String, String) {
    if all_day {
        let first = utc_date(start);
        (
            first.to_string(),
            utc_date(end - DAY).max(first).to_string(),
        )
    } else {
        (iso(start), iso(end))
    }
}

/// How the history names a range.
fn range_name(a: &Args, start: i64, end: i64) -> String {
    if a.flag("tomorrow") {
        "tomorrow".into()
    } else if a.flag("week") {
        "this week".into()
    } else if a.str("start").is_some() || a.str("end").is_some() {
        format!("{} – {}", &when(start)[..10], &when(end - 1)[..10])
    } else {
        "today".into()
    }
}

/// The range an `events` or `availability` call asks for: today from now
/// (the default), tomorrow, the rest of this week, or start to end.
fn range(a: &Args) -> Result<(i64, i64), Fail> {
    let now = Local::now();
    let midnight = |days: i64| {
        let day = now.date_naive() + Duration::days(days);
        Local
            .from_local_datetime(&day.and_time(NaiveTime::MIN))
            .earliest()
            .map_or(now.timestamp_millis() + days * DAY, |t| {
                t.timestamp_millis()
            })
    };
    let shortcuts = ["today", "tomorrow", "week"]
        .iter()
        .filter(|k| a.flag(k))
        .count();
    let custom = a.str("start").is_some() || a.str("end").is_some();
    if shortcuts > 1 || (shortcuts == 1 && custom) {
        return Err("Pass one of today, tomorrow, week, or start and end.".into());
    }
    if a.flag("tomorrow") {
        return Ok((midnight(1), midnight(2)));
    }
    if a.flag("week") {
        let left = 7 - i64::from(now.weekday().num_days_from_monday());
        return Ok((now.timestamp_millis(), midnight(left)));
    }
    if custom {
        let start = match a.str("start") {
            Some(s) => parse_time(s, false)?,
            None => now.timestamp_millis(),
        };
        let end = match a.str("end") {
            // A date as the end includes that day.
            Some(e) => parse_time(e, true)?,
            None => start + DAY,
        };
        if end <= start {
            return Err("end must come after start.".into());
        }
        if end - start > 92 * DAY {
            return Err("Ask for at most three months at a time.".into());
        }
        return Ok((start, end));
    }
    Ok((now.timestamp_millis(), midnight(1)))
}

/// The calendars agents may see, with their accounts' addresses, after
/// reading [start, end) from the servers. Problems say which accounts
/// couldn't be read.
async fn fetch(
    env: &Env<'_>,
    start: i64,
    end: i64,
) -> Result<(Vec<(Calendar, String, Access)>, Vec<String>), Fail> {
    let shared = env.readable()?;
    let mut problems = Vec::new();
    for (account, _) in &shared {
        if account.kind != spark_store::AccountKind::Microsoft {
            continue;
        }
        if let Err(e) = env.engine.refresh_calendar(account.id, start, end).await {
            problems.push(format!(
                "{}'s calendar couldn't be read just now ({e}); this is what spark-rs had.",
                account.email
            ));
        }
    }
    // Read again: the grants may have changed while the servers answered.
    let shared = env.readable()?;
    let calendars = env.store().calendars().map_err(db)?;
    let visible = calendars
        .into_iter()
        .filter_map(|c| {
            let (account, access) = shared.iter().find(|(a, _)| a.id == c.account_id)?;
            Some((c, account.email.clone(), *access))
        })
        .collect();
    Ok((visible, problems))
}

fn event_json(e: &StoredEvent, event: &Event, calendar: &str, account: &str) -> Value {
    let mut v = json!({
        "id": format!("e{}", e.id),
        "title": event.title,
        "start": bounds(event.start, event.end, event.all_day).0,
        "end": bounds(event.start, event.end, event.all_day).1,
        "all_day": event.all_day,
        "calendar": calendar,
        "account": account,
        "response": serde_json::to_value(event.response).unwrap_or_default(),
        "show_as": serde_json::to_value(event.show_as).unwrap_or_default(),
    });
    let mut put = |key: &str, value: Value| {
        v[key] = value;
    };
    if let Some(l) = &event.location {
        put("location", json!(l));
    }
    if let Some(l) = &event.call_link {
        put("call_link", json!(l));
    }
    if let Some(o) = &event.organizer {
        put("organizer", json!(person(o)));
    }
    if !event.attendees.is_empty() {
        put(
            "attendees",
            json!(event.attendees.iter().map(person).collect::<Vec<_>>()),
        );
    }
    if let Some(n) = event.notes.as_deref().filter(|n| !n.trim().is_empty()) {
        put("description", json!(super::cut(n.to_owned(), 4_000)));
    }
    if event.canceled {
        put("canceled", json!(true));
    }
    if event.repeats {
        put("repeats", json!(true));
    }
    if let Some(r) = event.reminder {
        put("alert_minutes_before", json!(r));
    }
    v
}

fn person(p: &Person) -> String {
    let who = if p.name.is_empty() || p.name.eq_ignore_ascii_case(&p.address) {
        p.address.clone()
    } else {
        format!("{} <{}>", p.name, p.address)
    };
    match p.answer {
        Some(answer) => format!("{who} ({})", answer.as_str()),
        None => who,
    }
}

pub async fn events(env: &Env<'_>, a: &Args<'_>) -> Done {
    let (start, end) = range(a)?;
    let (calendars, problems) = fetch(env, start, end).await?;
    let wanted = a.str("in");
    let picked: Vec<&(Calendar, String, Access)> = calendars
        .iter()
        .filter(|(c, email, _)| match wanted {
            None => true,
            Some(w) => match w.split_once(':') {
                Some((who, name)) => {
                    who.eq_ignore_ascii_case(email)
                        && name.trim().eq_ignore_ascii_case(c.name.trim())
                }
                None => w.eq_ignore_ascii_case(email),
            },
        })
        .collect();
    if let Some(w) = wanted
        && picked.is_empty()
    {
        return Err(Fail::Error(format!(
            "There's no calendar {w}. accounts lists them."
        )));
    }
    // A day either side: all-day events are kept in UTC days.
    let stored = env
        .store()
        .events_between(start - DAY, end + DAY)
        .map_err(db)?;
    let mut found: Vec<(i64, &StoredEvent, Event)> = stored
        .iter()
        .map(|e| {
            let event = Event::from_json(&e.data);
            (span(&event).0, e, event)
        })
        .filter(|(_, _, event)| {
            let (s, e) = span(event);
            s < end && e > start
        })
        .collect();
    found.sort_by_key(|(at, e, _)| (*at, e.id));
    let list: Vec<Value> = found
        .iter()
        .filter_map(|(_, e, event)| {
            let (c, email, _) = picked.iter().find(|(c, _, _)| c.id == e.calendar_id)?;
            Some(event_json(e, event, &c.name, email))
        })
        .collect();
    let value =
        json!({ "start": iso(start), "end": iso(end), "events": list, "problems": problems });
    let summary = match wanted {
        Some(w) => format!("checked {w}'s calendar for {}", range_name(a, start, end)),
        None => format!("checked your calendar for {}", range_name(a, start, end)),
    };
    Ok(Reply::new(value.clone(), text::events(&value)).said(summary))
}

/// `e42`, or `42`, as an event ID.
fn event_id(text: &str) -> Result<i64, Fail> {
    let n = text.trim().trim_start_matches(['e', 'E']);
    n.parse().ok().filter(|&id: &i64| id > 0).ok_or_else(|| {
        Fail::Error(format!(
            "{text} isn't an event ID. They look like e42 and come from events."
        ))
    })
}

fn zone() -> (String, fn(i64) -> String) {
    (zone_name(), local_wall)
}

pub async fn event(env: &Env<'_>, a: &Args<'_>) -> Done {
    let mode = a
        .str("mode")
        .ok_or("Pass mode: create, update, delete or rsvp.")?;
    let has_attendees = !a.addresses("add").is_empty() || !a.addresses("remove").is_empty();
    match mode {
        "create" => create(env, a).await,
        "update" => update(env, a).await,
        "delete" | "rsvp" if has_attendees => {
            Err("add and remove go with create and update only.".into())
        }
        "delete" => delete(env, a).await,
        "rsvp" => rsvp(env, a).await,
        other => Err(Fail::Error(format!(
            "event's mode is create, update, delete or rsvp, not {other}."
        ))),
    }
}

/// The calendar a new event goes in: `email` (its default) or `email:Name`;
/// else the first writable account's default.
fn target(env: &Env, calendars: &[Calendar], wanted: Option<&str>) -> Result<Calendar, Fail> {
    let shared = env.readable()?;
    let own = |c: &&Calendar| c.own;
    match wanted {
        Some(w) => {
            let (who, name) = match w.split_once(':') {
                Some((who, name)) => (who.trim(), Some(name.trim())),
                None => (w.trim(), None),
            };
            let account = env.account_named(who)?;
            let mine: Vec<&Calendar> = calendars.iter().filter(|c| c.account_id == account.id).filter(own).collect();
            let found = match name {
                Some(name) => mine.into_iter().find(|c| c.name.trim().eq_ignore_ascii_case(name)),
                None => mine.iter().find(|c| c.is_default).or(mine.first()).copied(),
            };
            found.cloned().ok_or_else(|| Fail::Error(format!("There's no calendar of yours called {w}. accounts lists them.")))
        }
        None => shared
            .iter()
            .filter(|(_, access)| *access >= Access::Send)
            .find_map(|(account, _)| {
                let mine: Vec<&Calendar> = calendars.iter().filter(|c| c.account_id == account.id).filter(own).collect();
                mine.iter().find(|c| c.is_default).or(mine.first()).copied().cloned()
            })
            .ok_or_else(|| {
                Fail::Blocked(
                    "No calendar lets agents make events: that needs send access to a Microsoft 365 account, in spark-rs > Settings > Agents."
                        .into(),
                )
            }),
    }
}

/// Fields `create` and `update` share.
fn apply_fields(a: &Args, draft: &mut EventDraft, creating: bool) -> Result<(), Fail> {
    if let Some(title) = a.str("title") {
        draft.title = title.to_owned();
    }
    if let Some(d) = a.raw("description") {
        draft.notes = d.to_owned();
    }
    if let Some(l) = a.raw("location") {
        draft.location = l.trim().to_owned();
    }
    let all_day = a.flag("all_day") || a.str("start").is_some_and(is_date);
    let length = (draft.end - draft.start).max(0);
    let was_all_day = draft.all_day;
    if let Some(s) = a.str("start") {
        draft.all_day = all_day;
        if all_day {
            // All-day dates are UTC midnights, as the calendar keeps them.
            draft.start = utc_day(s)?;
            let days = if was_all_day {
                (length / DAY).max(1)
            } else {
                1
            };
            draft.end = draft.start + days * DAY;
        } else {
            draft.start = parse_time(s, false)?;
            draft.end = if creating || length == 0 || was_all_day {
                draft.start + 30 * 60_000
            } else {
                draft.start + length
            };
        }
    } else if a.flag("all_day") && !was_all_day {
        // A timed event made all-day covers its own date.
        draft.all_day = true;
        draft.start = utc_day(&super::when(draft.start))?;
        draft.end = draft.start + DAY;
    }
    if let Some(e) = a.str("end") {
        // An all-day end is the last day it covers.
        draft.end = if draft.all_day {
            utc_day(e)? + DAY
        } else {
            parse_time(e, false)?
        };
    }
    if draft.end <= draft.start {
        return Err("end must come after start.".into());
    }
    if let Some(alerts) = a.str("alerts") {
        let first = alerts.split(',').next().unwrap_or_default().trim();
        draft.reminder = Some(
            match first.strip_suffix('s').and_then(|n| n.parse::<i64>().ok()) {
                Some(seconds) => seconds / 60,
                None => {
                    let starts = if draft.all_day {
                        span(&Event {
                            start: draft.start,
                            end: draft.end,
                            all_day: true,
                            ..Event::default()
                        })
                        .0
                    } else {
                        draft.start
                    };
                    ((starts - parse_time(first, false)?) / 60_000).max(0)
                }
            },
        );
    }
    if let Some(show) = a.str("show_as") {
        draft.show_as = match show.to_lowercase().replace(['-', '_', ' '], "").as_str() {
            "free" => ShowAs::Free,
            "busy" => ShowAs::Busy,
            "tentative" => ShowAs::Tentative,
            "oof" | "away" | "outofoffice" => ShowAs::Oof,
            "workingelsewhere" => ShowAs::WorkingElsewhere,
            other => {
                return Err(Fail::Error(format!(
                    "show_as is free, busy, tentative, oof or working-elsewhere, not {other}."
                )));
            }
        };
    }
    match a.str("video_conference") {
        None => {}
        Some("auto" | "teams") => draft.teams = true,
        Some(other) => {
            return Err(Fail::Error(format!(
                "spark-rs adds Teams calls to Microsoft 365 events; it can't add a {other} call yet."
            )));
        }
    }
    for address in a.addresses("remove") {
        draft
            .attendees
            .retain(|p| !p.address.eq_ignore_ascii_case(&address));
    }
    for address in a.addresses("add") {
        if spark_store::domain_of(&address).is_none() {
            return Err(Fail::Error(format!("{address} isn't an email address.")));
        }
        if !draft
            .attendees
            .iter()
            .any(|p| p.address.eq_ignore_ascii_case(&address))
        {
            draft.attendees.push(Person {
                name: address.clone(),
                address,
                answer: None,
            });
        }
    }
    Ok(())
}

async fn create(env: &Env<'_>, a: &Args<'_>) -> Done {
    if a.str("start").is_none() {
        return Err("Pass start: when the event begins, e.g. 2026-07-01T12:00.".into());
    }
    let calendars = env.store().calendars().map_err(db)?;
    let calendar = target(env, &calendars, a.str("calendar"))?;
    let account = env.account(calendar.account_id)?;
    let mut draft = EventDraft {
        calendar: calendar.id,
        title: "New event".into(),
        ..EventDraft::default()
    };
    apply_fields(a, &mut draft, true)?;
    // As the app's editor makes them: an all-day event doesn't block the day.
    if draft.all_day && a.str("show_as").is_none() {
        draft.show_as = ShowAs::Free;
    }
    let (name, wall) = zone();
    let zone = Zone {
        name: &name,
        wall,
        shift: 0,
    };
    env.prefs.with_agent_access(|grants| {
        refuse(env.agent, grants, &account, Access::Send, "make events")
    })?;
    env.engine
        .create_event(&draft, zone)
        .await
        .map_err(|e| Fail::Error(format!("Microsoft didn't take the event: {e}")))?;
    let _ = env
        .engine
        .refresh_calendar(account.id, draft.start - DAY, draft.end + DAY)
        .await;
    let made = env
        .store()
        .events_between(draft.start, draft.end)
        .map_err(db)?
        .into_iter()
        .filter(|e| e.calendar_id == calendar.id && e.start == draft.start)
        .find(|e| Event::from_json(&e.data).title == draft.title);
    let invited = draft.attendees.len();
    let starts = if draft.all_day {
        format!("{}, all day", utc_date(draft.start))
    } else {
        when(draft.start)
    };
    let mut out = format!(
        "Event \"{}\" made in {}:{}, {starts}.",
        draft.title, account.email, calendar.name
    );
    if let Some(e) = &made {
        out.push_str(&format!("\nID: e{}", e.id));
    }
    if invited > 0 {
        out.push_str(&format!(
            "\nOutlook invited {}.",
            count(invited, "person", "people")
        ));
    }
    Ok(Reply {
        summary: format!("made the event \"{}\"", draft.title),
        targets: made.iter().map(|e| format!("e{}", e.id)).collect(),
        effect: Some(Effect::Changed {
            summary: format!("made the event \"{}\"", draft.title),
        }),
        ..Reply::new(
            json!({
                "id": made.map(|e| format!("e{}", e.id)),
                "title": draft.title,
                "calendar": format!("{}:{}", account.email, calendar.name),
                "start": bounds(draft.start, draft.end, draft.all_day).0,
                "end": bounds(draft.start, draft.end, draft.all_day).1,
                "all_day": draft.all_day,
                "invited": invited,
            }),
            out,
        )
    })
}

/// A stored event of the user's own calendars, with its account.
fn own_event(env: &Env, a: &Args) -> Result<(StoredEvent, Event, Calendar), Fail> {
    let id = a
        .str("event_id")
        .or_else(|| a.str("id"))
        .ok_or("Pass event_id: an event ID from events, such as e42.")?;
    let id = event_id(id)?;
    let stored = env
        .store()
        .event(id)
        .map_err(db)?
        .ok_or_else(|| Fail::Error(format!("There's no event e{id}.")))?;
    let calendar = env
        .store()
        .calendars()
        .map_err(db)?
        .into_iter()
        .find(|c| c.id == stored.calendar_id)
        .ok_or("That event's calendar is no longer here.")?;
    let account = env.account(stored.account_id)?;
    env.need(&account, Access::Read, "read the calendar")?;
    if !calendar.own {
        return Err(Fail::Error(format!(
            "e{id} is in a calendar shared with the user, which spark-rs can't change."
        )));
    }
    let event = Event::from_json(&stored.data);
    Ok((stored, event, calendar))
}

async fn update(env: &Env<'_>, a: &Args<'_>) -> Done {
    let (stored, event, calendar) = own_event(env, a)?;
    let account = env.account(stored.account_id)?;
    let before = EventDraft::of(&event, calendar.id);
    let mut draft = before.clone();
    apply_fields(a, &mut draft, false)?;
    if draft == before {
        return Err("Nothing to change: pass title, start, end, location, description, alerts, video_conference, add or remove.".into());
    }
    let (name, wall) = zone();
    let zone = Zone {
        name: &name,
        wall,
        shift: 0,
    };
    env.prefs.with_agent_access(|grants| {
        refuse(env.agent, grants, &account, Access::Send, "change events")
    })?;
    env.engine
        .update_event(stored.id, &draft, Some(&before), Scope::One, zone)
        .await
        .map_err(|e| Fail::Error(format!("Microsoft didn't take the change: {e}")))?;
    let _ = env
        .engine
        .refresh_calendar(
            account.id,
            draft.start.min(before.start) - DAY,
            draft.end.max(before.end) + DAY,
        )
        .await;
    let told = draft.attendees.len();
    let mut out = format!("Event e{} \"{}\" updated.", stored.id, draft.title);
    // An alert is the user's own: changing only that tells no one.
    let only_alert = EventDraft {
        reminder: before.reminder,
        ..draft.clone()
    } == before;
    if told > 0 && !only_alert {
        out.push_str(&format!(
            " Outlook tells the {} invited.",
            count(told, "person", "people")
        ));
    }
    Ok(Reply {
        summary: format!("changed the event \"{}\"", draft.title),
        targets: vec![format!("e{}", stored.id)],
        effect: Some(Effect::Changed {
            summary: format!("changed the event \"{}\"", draft.title),
        }),
        ..Reply::new(
            json!({ "id": format!("e{}", stored.id), "title": draft.title, "start": bounds(draft.start, draft.end, draft.all_day).0, "end": bounds(draft.start, draft.end, draft.all_day).1, "attendees": told }),
            out,
        )
    })
}

async fn delete(env: &Env<'_>, a: &Args<'_>) -> Done {
    let (stored, event, _) = own_event(env, a)?;
    let account = env.account(stored.account_id)?;
    let (name, wall) = zone();
    let zone = Zone {
        name: &name,
        wall,
        shift: 0,
    };
    env.prefs.with_agent_access(|grants| {
        refuse(env.agent, grants, &account, Access::Send, "delete events")
    })?;
    env.engine
        .delete_event(stored.id, Scope::One, zone)
        .await
        .map_err(|e| Fail::Error(format!("Microsoft didn't delete it: {e}")))?;
    let _ = env
        .engine
        .refresh_calendar(account.id, event.start - DAY, event.end + DAY)
        .await;
    let mut out = format!("Event e{} \"{}\" deleted.", stored.id, event.title);
    if !event.attendees.is_empty() {
        out.push_str(&format!(
            " Outlook tells the {} invited it's canceled.",
            count(event.attendees.len(), "person", "people")
        ));
    }
    Ok(Reply {
        summary: format!("deleted the event \"{}\"", event.title),
        targets: vec![format!("e{}", stored.id)],
        effect: Some(Effect::Changed {
            summary: format!("deleted the event \"{}\"", event.title),
        }),
        ..Reply::new(
            json!({ "deleted": format!("e{}", stored.id), "title": event.title }),
            out,
        )
    })
}

async fn rsvp(env: &Env<'_>, a: &Args<'_>) -> Done {
    let id = a
        .str("event_id")
        .or_else(|| a.str("id"))
        .ok_or("Pass event_id: an event ID (e42) or an invitation email's ID.")?;
    let answer = match a.str("status").map(str::to_lowercase).as_deref() {
        Some("accept" | "accepted" | "yes") => Answer::Accepted,
        Some("decline" | "declined" | "no") => Answer::Declined,
        Some("maybe" | "tentative") => Answer::Tentative,
        _ => return Err("Pass status: accept, decline or maybe.".into()),
    };
    let word = match answer {
        Answer::Accepted => "accepted",
        Answer::Declined => "declined",
        Answer::Tentative => "answered maybe to",
    };
    if id.starts_with(['e', 'E']) {
        let (stored, event, _) = own_event(env, a)?;
        let account = env.account(stored.account_id)?;
        if event.response == spark_mail::calendar::Response::Organizer {
            return Err(Fail::Error(format!(
                "e{} is the user's own event; there's nothing to answer.",
                stored.id
            )));
        }
        env.prefs.with_agent_access(|grants| {
            refuse(
                env.agent,
                grants,
                &account,
                Access::Send,
                "answer invitations",
            )
        })?;
        env.engine
            .answer_event(stored.id, answer)
            .await
            .map_err(|e| Fail::Error(format!("Microsoft didn't take the answer: {e}")))?;
        let summary = format!("{word} \"{}\"", event.title);
        return Ok(Reply {
            summary: summary.clone(),
            targets: vec![format!("e{}", stored.id)],
            effect: Some(Effect::Changed {
                summary: summary.clone(),
            }),
            ..Reply::new(
                json!({ "id": format!("e{}", stored.id), "status": answer.as_str() }),
                format!("{}. The organizer hears back.", super::capitalize(&summary)),
            )
        });
    }
    let message = message_id(id)?;
    let (m, account) = env.message(message)?;
    let has_invite = env
        .store()
        .content(message)
        .map_err(db)?
        .is_some_and(|c| c.invite.is_some());
    if !has_invite {
        return Err(Fail::Error(format!("Email {message} isn't an invitation.")));
    }
    env.prefs.with_agent_access(|grants| {
        refuse(
            env.agent,
            grants,
            &account,
            Access::Send,
            "answer invitations",
        )
    })?;
    env.engine
        .answer_invite(message, answer)
        .await
        .map_err(|e| Fail::Error(format!("The answer couldn't be sent: {e}")))?;
    let summary = format!("{word} \"{}\"", m.subject);
    Ok(Reply {
        summary: summary.clone(),
        targets: vec![message.to_string()],
        effect: Some(Effect::Changed {
            summary: summary.clone(),
        }),
        ..Reply::new(
            json!({ "id": message, "status": answer.as_str() }),
            format!(
                "{}. The organizer gets an email with the answer.",
                super::capitalize(&summary)
            ),
        )
    })
}

pub async fn availability(env: &Env<'_>, a: &Args<'_>) -> Done {
    let (start, end) = range(a)?;
    let (calendars, mut problems) = fetch(env, start, end).await?;
    let attendees = a.addresses("attendees");
    if !attendees.is_empty() {
        problems.push(format!(
            "spark-rs can't see other people's calendars yet, so these are only the user's free times; check with {} too.",
            attendees.join(", ")
        ));
    }
    let own: Vec<i64> = calendars
        .iter()
        .filter(|(c, _, _)| c.own)
        .map(|(c, _, _)| c.id)
        .collect();
    let mut busy: Vec<(i64, i64)> = env
        .store()
        .events_between(start, end)
        .map_err(db)?
        .into_iter()
        .filter(|e| own.contains(&e.calendar_id))
        .filter_map(|e| {
            let event = Event::from_json(&e.data);
            let free = event.canceled
                || event.show_as == spark_mail::calendar::ShowAs::Free
                || event.response == spark_mail::calendar::Response::Declined;
            (!free).then_some(span(&event))
        })
        .collect();
    busy.sort();
    let now = now_ms();
    let mut slots = Vec::new();
    let mut day = Local
        .timestamp_millis_opt(start.max(now))
        .single()
        .map(|t| t.date_naive())
        .ok_or("That range isn't on this computer's clock.")?;
    loop {
        let at = |hour: u32| {
            Local
                .from_local_datetime(&day.and_hms_opt(hour, 0, 0).expect("a whole hour"))
                .earliest()
                .map(|t| t.timestamp_millis())
        };
        let (Some(open), Some(close)) = (at(WORK_START), at(WORK_END)) else {
            break;
        };
        if open >= end {
            break;
        }
        if !matches!(day.weekday(), Weekday::Sat | Weekday::Sun) {
            let mut from = open.max(start).max(now);
            let until = close.min(end);
            for &(b_start, b_end) in &busy {
                if b_end <= from || b_start >= until {
                    continue;
                }
                if b_start - from >= MIN_SLOT {
                    slots.push((from, b_start));
                }
                from = from.max(b_end);
            }
            if until - from >= MIN_SLOT {
                slots.push((from, until));
            }
        }
        day = match day.succ_opt() {
            Some(next) => next,
            None => break,
        };
    }
    let list: Vec<Value> = slots
        .iter()
        .map(|&(s, e)| json!({ "start": iso(s), "end": iso(e), "minutes": (e - s) / 60_000 }))
        .collect();
    let mut out = String::new();
    if list.is_empty() {
        out.push_str("No free time between 08:00 and 20:00 on weekdays in that range.\n");
    }
    for &(s, e) in &slots {
        out.push_str(&format!("{} – {}\n", when(s), &when(e)[11..]));
    }
    for p in &problems {
        out.push_str(&format!("Note: {p}\n"));
    }
    let summary = match attendees.as_slice() {
        [] => format!("checked free time for {}", range_name(a, start, end)),
        people => format!(
            "checked free time for {} with {}",
            range_name(a, start, end),
            people.join(", ")
        ),
    };
    Ok(Reply::new(
        json!({ "start": iso(start), "end": iso(end), "free": list, "problems": problems }),
        out.trim_end().to_owned(),
    )
    .said(summary))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_day_dates_are_utc_midnights_and_local_days() {
        let mut draft = EventDraft::default();
        let args = serde_json::json!({ "start": "2026-11-05", "end": "2026-11-06" });
        apply_fields(&Args(&args), &mut draft, true).unwrap();
        let midnight = NaiveDate::from_ymd_opt(2026, 11, 5)
            .unwrap()
            .and_time(NaiveTime::MIN)
            .and_utc()
            .timestamp_millis();
        assert!(draft.all_day);
        assert_eq!(draft.start, midnight);
        assert_eq!(
            draft.end,
            midnight + 2 * DAY,
            "the end date is the last day"
        );
        assert_eq!(
            bounds(draft.start, draft.end, true),
            ("2026-11-05".into(), "2026-11-06".into())
        );
        // On this computer's clock the event is the local days.
        let event = Event {
            start: draft.start,
            end: draft.end,
            all_day: true,
            ..Event::default()
        };
        let local = Local
            .from_local_datetime(
                &NaiveDate::from_ymd_opt(2026, 11, 5)
                    .unwrap()
                    .and_time(NaiveTime::MIN),
            )
            .earliest()
            .unwrap();
        assert_eq!(span(&event).0, local.timestamp_millis());
        // A timed start stays local.
        let mut timed = EventDraft::default();
        apply_fields(
            &Args(&serde_json::json!({ "start": "2026-11-05T09:00" })),
            &mut timed,
            true,
        )
        .unwrap();
        assert!(!timed.all_day);
        assert_eq!(timed.end - timed.start, 30 * 60_000);
    }

    #[test]
    fn event_ids_take_an_e() {
        assert_eq!(event_id("e42").unwrap(), 42);
        assert_eq!(event_id("42").unwrap(), 42);
        assert!(event_id("e").is_err());
        assert!(event_id("x9").is_err());
    }
}
