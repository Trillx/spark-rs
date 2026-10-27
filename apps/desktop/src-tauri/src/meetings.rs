//! The meeting pop-up, Spark's Meeting Pop-Up: a card over every other
//! window from shortly before a meeting (as it starts, or 1 to 15 minutes
//! before, as Settings say) until it ends, with the time left,
//! who's in it, and Join, Copy link, Snooze and Dismiss, and a chime
//! (`chime.rs`) as each meeting appears. Joining leaves the card up without
//! its Join button, as Spark does. A timer here picks the meetings from the
//! stored calendars, and keeps the next couple of hours fresh from the
//! servers whether or not the calendar screen is open. The window
//! (`src/popup.tsx`) only draws them.
//!
//! The window is made when a meeting is due and destroyed when none is. It's
//! made visible and unfocused in one go: on Windows a hidden window that's
//! shown later takes the focus from whatever the user is typing in.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use spark_mail::calendar::{Event, Response};
use spark_mail::invite::{Answer, Person};
use spark_store::now_ms;
use tauri::{
    AppHandle, Emitter, LogicalSize, Manager, PhysicalPosition, State, WebviewUrl, WebviewWindow,
    WebviewWindowBuilder, Window, WindowEvent,
};
use tokio::sync::Notify;

use crate::alerts::OpenCalendar;
use crate::background;
use crate::chime;
use crate::mailbox::{Mailbox, open_external, text};
use crate::prefs::PrefsState;

pub const LABEL: &str = "meeting-popup";

const MINUTE_MS: i64 = 60_000;
/// Snooze brings it back after this long, as in Spark.
const SNOOZE_MS: i64 = MINUTE_MS;
/// Events this long or longer (a conference, a trip) don't pop up.
const LONGEST_MS: i64 = 24 * 60 * 60_000;
const CHECK_EVERY: Duration = Duration::from_secs(5);
const REFRESH_EVERY: Duration = Duration::from_secs(5 * 60);
/// How far ahead each refresh reads.
const REFRESH_AHEAD_MS: i64 = 2 * 60 * 60_000;

/// The window's size until its page measures the cards (logical px).
const WIDTH: f64 = 472.0;
const HEIGHT_PER_CARD: f64 = 105.0;
const HEIGHT_EXTRA: f64 = 32.0;
/// From the top of the screen's work area to the window's top.
const TOP: f64 = 8.0;

/// A meeting as the pop-up shows it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Meeting {
    /// The stored event's; negative for the preview's.
    id: i64,
    title: String,
    /// Unix ms.
    start: i64,
    end: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    call_link: Option<String>,
    /// Who's in it besides the user, the organizer first.
    people: Vec<Person>,
    /// The user joined from the pop-up: the card stays, without Join.
    joined: bool,
}

#[derive(Default)]
pub struct Meetings {
    state: Mutex<PopupState>,
    /// Wakes the timer early: an action, or a changed setting.
    wake: Notify,
    /// The first refresh is over, so what's stored isn't from a past
    /// session: a meeting canceled since then won't pop up.
    refreshed: AtomicBool,
}

#[derive(Default)]
struct PopupState {
    /// Dismissed or joined meetings by event id and start, so a moved
    /// meeting pops up again, kept until they end.
    dismissed: HashMap<(i64, i64), i64>,
    /// Snoozed meetings by event id, until when.
    snoozed: HashMap<i64, i64>,
    /// Meetings joined from the pop-up by event id and start, kept until
    /// they end.
    joined: HashMap<(i64, i64), i64>,
    /// The sample from Settings, until it's dismissed.
    preview: Option<Meeting>,
    /// What the window shows.
    showing: Vec<Meeting>,
}

impl Meetings {
    /// Checks again now, e.g. after the settings changed.
    pub fn changed(&self) {
        self.wake.notify_one();
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, PopupState> {
        self.state.lock().unwrap()
    }

    /// Marks meeting `id` joined until it ends.
    fn join(&self, id: i64) {
        let mut state = self.lock();
        if let Some(m) = state.showing.iter().find(|m| m.id == id) {
            let (key, end) = ((m.id, m.start), m.end);
            state.joined.insert(key, end);
        }
    }

    fn dismiss(&self, id: i64) {
        let mut state = self.lock();
        if id < 0 {
            state.preview = None;
        } else if let Some(m) = state.showing.iter().find(|m| m.id == id) {
            let key = (m.id, m.start);
            let end = m.end;
            state.dismissed.insert(key, end);
        }
    }
}

/// Starts the timer that shows and hides the pop-up, and the refresh of the
/// coming hours.
pub fn start(app: &AppHandle) {
    let handle = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            check(&handle);
            let meetings = handle.state::<Meetings>();
            tokio::select! {
                () = tokio::time::sleep(CHECK_EVERY) => {}
                () = meetings.wake.notified() => {}
            }
        }
    });
    let handle = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            refresh(&handle).await;
            let meetings = handle.state::<Meetings>();
            meetings.refreshed.store(true, Ordering::Relaxed);
            meetings.changed();
            tokio::time::sleep(REFRESH_EVERY).await;
        }
    });
}

/// Reads the coming hours from the servers, so meetings added or moved
/// since the calendar screen was last open pop up, and alert, too.
async fn refresh(app: &AppHandle) {
    let prefs = app.state::<PrefsState>().get();
    if !prefs.meeting_popup && !prefs.event_alerts {
        return;
    }
    let mailbox = app.state::<Mailbox>();
    if !mailbox.live() {
        return;
    }
    let engine = mailbox.live.engine.clone();
    let accounts = engine.store().accounts().unwrap_or_default();
    let now = now_ms();
    for account in accounts {
        let _ = engine
            .refresh_calendar(account.id, now, now + REFRESH_AHEAD_MS)
            .await;
    }
}

/// Works out what the pop-up shows now, and makes, updates or closes it.
fn check(app: &AppHandle) {
    let prefs = app.state::<PrefsState>().get();
    let now = now_ms();
    let meetings = app.state::<Meetings>();
    let found = if prefs.meeting_popup && meetings.refreshed.load(Ordering::Relaxed) {
        stored_meetings(
            &app.state::<Mailbox>(),
            now,
            lead(prefs.meeting_popup_minutes),
            prefs.meeting_popup_guests_only,
        )
    } else {
        Vec::new()
    };
    let (showing, arrived) = {
        let mut state = meetings.lock();
        state.dismissed.retain(|_, end| *end > now);
        state.snoozed.retain(|_, until| *until > now);
        state.joined.retain(|_, end| *end > now);
        let mut showing: Vec<Meeting> = state
            .preview
            .iter()
            .cloned()
            .chain(found.into_iter().filter(|m| {
                !state.dismissed.contains_key(&(m.id, m.start))
                    && !state.snoozed.contains_key(&m.id)
            }))
            .collect();
        for m in &mut showing {
            m.joined = state.joined.contains_key(&(m.id, m.start));
        }
        let arrived = arrived(&state.showing, &showing);
        state.showing = showing.clone();
        (showing, arrived)
    };
    if arrived && prefs.meeting_popup_sound {
        chime::play();
    }
    match app.get_webview_window(LABEL) {
        Some(window) if showing.is_empty() => {
            let _ = window.destroy();
        }
        // Every time, so a page that missed one (still loading) catches up.
        Some(_) => {
            let _ = app.emit_to(LABEL, "meeting-popup-changed", &showing);
        }
        None if !showing.is_empty() => open(app, showing.len()),
        _ => {}
    }
}

/// Whether a meeting shows that didn't before: new, back from a snooze, or
/// moved. One that's only been renamed hasn't arrived.
fn arrived(before: &[Meeting], now: &[Meeting]) -> bool {
    now.iter()
        .any(|m| !before.iter().any(|b| (b.id, b.start) == (m.id, m.start)))
}

/// Makes the window, shown at once without taking the focus, at the top
/// middle of the screen the main window is on.
fn open(app: &AppHandle, cards: usize) {
    let height = HEIGHT_EXTRA + HEIGHT_PER_CARD * cards as f64;
    let monitor = app
        .get_webview_window("main")
        .and_then(|w| w.current_monitor().ok().flatten())
        .or_else(|| app.primary_monitor().ok().flatten());
    let place = monitor.map(|m| {
        let area = m.work_area();
        let scale = m.scale_factor();
        let width = (WIDTH * scale).round() as i32;
        PhysicalPosition::new(
            area.position.x + (area.size.width as i32 - width) / 2,
            area.position.y + (TOP * scale).round() as i32,
        )
        .to_logical::<f64>(scale)
    });
    let mut builder = WebviewWindowBuilder::new(app, LABEL, WebviewUrl::App("popup.html".into()))
        .title("Meeting")
        .inner_size(WIDTH, height)
        .decorations(false)
        .transparent(true)
        .shadow(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .resizable(false)
        .maximizable(false)
        .minimizable(false)
        .focused(false)
        .visible(true);
    if let Some(p) = place {
        builder = builder.position(p.x, p.y);
    }
    if let Err(e) = builder.build() {
        eprintln!("spark-rs: couldn't show the meeting pop-up: {e}");
    }
}

/// How long before a meeting it pops up, from Settings' minutes, which
/// can only be Spark's choices.
fn lead(minutes: u32) -> i64 {
    let minutes = [0, 1, 2, 3, 5, 10, 15]
        .into_iter()
        .rfind(|&m| m <= minutes)
        .unwrap_or(1);
    i64::from(minutes) * MINUTE_MS
}

/// The meetings due at `now`, popping up `lead` before they start, in the
/// user's own calendars of every account, earliest first; a meeting in
/// two of them shows once. Calendars shared by others never pop up.
fn stored_meetings(mailbox: &Mailbox, now: i64, lead: i64, guests_only: bool) -> Vec<Meeting> {
    if !mailbox.live() {
        return Vec::new();
    }
    let store = mailbox.live.engine.store();
    let (Ok(accounts), Ok(calendars), Ok(events)) = (
        store.accounts(),
        store.calendars(),
        store.events_between(now, now + lead + 1),
    ) else {
        return Vec::new();
    };
    events
        .into_iter()
        .filter(|e| calendars.iter().any(|c| c.id == e.calendar_id && c.own))
        .filter_map(|stored| {
            let event = Event::from_json(&stored.data);
            if !due(&event, now, lead, guests_only) {
                return None;
            }
            let me = accounts
                .iter()
                .find(|a| a.id == stored.account_id)
                .map_or("", |a| a.email.as_str());
            Some(meeting(stored.id, event, me))
        })
        .fold(Vec::new(), |mut kept: Vec<Meeting>, m| {
            if !kept.iter().any(|k| same(k, &m)) {
                kept.push(m);
            }
            kept
        })
}

/// Whether two stored events are one meeting, e.g. in two accounts'
/// calendars.
fn same(a: &Meeting, b: &Meeting) -> bool {
    (a.start, a.end, &a.call_link) == (b.start, b.end, &b.call_link)
        && a.title.trim().eq_ignore_ascii_case(b.title.trim())
}

/// Whether `e` pops up at `now`, before dismissing and snoozing: a timed
/// meeting the user hasn't declined, from `lead` before it starts until it
/// ends. `guests_only` leaves out events with no one else and no call,
/// like focus time.
fn due(e: &Event, now: i64, lead: i64, guests_only: bool) -> bool {
    !e.all_day
        && !e.canceled
        && e.response != Response::Declined
        && e.end - e.start < LONGEST_MS
        && e.start - lead <= now
        && now < e.end
        && (!guests_only || !e.attendees.is_empty() || e.call_link.is_some())
}

/// The pop-up's view of event `id`; `me` is the account's address, left
/// out of the people.
fn meeting(id: i64, event: Event, me: &str) -> Meeting {
    let mut people: Vec<Person> = Vec::new();
    for p in event.organizer.into_iter().chain(event.attendees) {
        let seen = people
            .iter()
            .any(|o| o.address.eq_ignore_ascii_case(&p.address));
        if !p.address.eq_ignore_ascii_case(me) && !seen && p.answer != Some(Answer::Declined) {
            people.push(p);
        }
    }
    Meeting {
        id,
        title: event.title,
        start: event.start,
        end: event.end,
        call_link: event.call_link,
        people,
        joined: false,
    }
}

/// The sample Settings shows, starting in a minute.
fn sample(now: i64) -> Meeting {
    let person = |name: &str, address: &str| Person {
        name: name.into(),
        address: address.into(),
        answer: Some(Answer::Accepted),
    };
    let start = now + MINUTE_MS;
    Meeting {
        id: -1,
        title: "Design review: Inbox v3".into(),
        start,
        end: start + 30 * 60_000,
        call_link: Some("https://teams.microsoft.com/l/meetup-join/preview".into()),
        people: vec![
            person("Hannah Cole", "hannah@acme.example"),
            person("Priya Raman", "priya@acme.example"),
            person("Sam Okafor", "sam.okafor@acme.example"),
            person("Kai Tanaka", "kai@acme.example"),
            person("Alex Moreno", "alex@moreno-design.example"),
        ],
        joined: false,
    }
}

/// What the pop-up's buttons do.
#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(tag = "action", rename_all = "camelCase")]
pub enum Act {
    /// Opens the call; the card stays, without Join.
    Join {
        id: i64,
    },
    Snooze {
        id: i64,
    },
    Dismiss {
        id: i64,
    },
    DismissAll,
    /// Brings the main window forward on the meeting in the calendar.
    OpenCalendar {
        id: i64,
    },
    /// Brings the main window forward on the pop-up's settings.
    OpenSettings,
}

#[tauri::command]
pub fn meeting_popup_meetings(meetings: State<'_, Meetings>) -> Vec<Meeting> {
    meetings.lock().showing.clone()
}

#[tauri::command]
pub fn meeting_popup_act(
    app: AppHandle,
    meetings: State<'_, Meetings>,
    act: Act,
) -> Result<(), String> {
    let find = |id: i64| meetings.lock().showing.iter().find(|m| m.id == id).cloned();
    match act {
        Act::Join { id } => {
            if let Some(link) = find(id).and_then(|m| m.call_link)
                && id >= 0
            {
                open_external(link)?;
            }
            meetings.join(id);
        }
        // Snoozing a joined meeting forgets it was joined, as in Spark.
        Act::Snooze { id } => {
            if id < 0 {
                meetings.dismiss(id);
            } else {
                let mut state = meetings.lock();
                state.snoozed.insert(id, now_ms() + SNOOZE_MS);
                state.joined.retain(|(joined, _), _| *joined != id);
            }
        }
        Act::Dismiss { id } => meetings.dismiss(id),
        Act::DismissAll => {
            let ids: Vec<i64> = meetings.lock().showing.iter().map(|m| m.id).collect();
            for id in ids {
                meetings.dismiss(id);
            }
        }
        Act::OpenCalendar { id } => {
            let start = find(id).map_or_else(now_ms, |m| m.start);
            background::show(&app);
            app.emit_to("main", "open-calendar", OpenCalendar::event(id, start))
                .map_err(text)?;
            return Ok(());
        }
        Act::OpenSettings => {
            background::show(&app);
            app.emit_to("main", "open-settings", "notifications")
                .map_err(text)?;
            return Ok(());
        }
    }
    meetings.changed();
    Ok(())
}

/// Sizes the window to its cards (logical px), keeping its middle where it
/// is (the user may have dragged it).
#[tauri::command]
pub fn fit_meeting_popup(window: WebviewWindow, width: f64, height: f64) -> Result<(), String> {
    let scale = window.scale_factor().map_err(text)?;
    let old = window.outer_size().map_err(text)?;
    let at = window.outer_position().map_err(text)?;
    window
        .set_size(LogicalSize::new(width, height))
        .map_err(text)?;
    let new_width = (width * scale).round() as i32;
    if new_width != old.width as i32 {
        let x = at.x + (old.width as i32 - new_width) / 2;
        window
            .set_position(PhysicalPosition::new(x, at.y))
            .map_err(text)?;
    }
    Ok(())
}

/// Shows the sample meeting, for Settings.
#[tauri::command]
pub fn preview_meeting_popup(meetings: State<'_, Meetings>) {
    meetings.lock().preview = Some(sample(now_ms()));
    meetings.changed();
}

/// Closing the pop-up from the keyboard (Alt+F4) dismisses its meetings,
/// rather than letting the timer bring it straight back.
pub fn on_window_event(window: &Window, event: &WindowEvent) {
    if window.label() != LABEL {
        return;
    }
    if let WindowEvent::CloseRequested { api, .. } = event {
        api.prevent_close();
        let _ = meeting_popup_act(
            window.app_handle().clone(),
            window.state::<Meetings>(),
            Act::DismissAll,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: i64 = 60_000;

    fn person(name: &str, address: &str) -> Person {
        Person {
            name: name.into(),
            address: address.into(),
            answer: None,
        }
    }

    fn standup() -> Event {
        Event {
            title: "Standup".into(),
            start: 100 * MIN,
            end: 115 * MIN,
            call_link: Some("https://teams.microsoft.com/l/meetup-join/x".into()),
            organizer: Some(person("Sam Okafor", "sam@acme.example")),
            attendees: vec![person("Me", "me@acme.example")],
            response: Response::Accepted,
            ..Default::default()
        }
    }

    #[test]
    fn pops_up_a_minute_before_until_the_end() {
        let e = standup();
        assert!(!due(&e, 98 * MIN, MIN, true));
        assert!(due(&e, 99 * MIN, MIN, true));
        assert!(due(&e, 114 * MIN, MIN, true));
        assert!(!due(&e, 115 * MIN, MIN, true));
    }

    #[test]
    fn pops_up_as_long_before_as_settings_say() {
        let e = standup();
        assert!(!due(&e, 89 * MIN, 10 * MIN, true));
        assert!(due(&e, 90 * MIN, 10 * MIN, true));
        assert!(!due(&e, 99 * MIN, 0, true), "when it starts");
        assert!(due(&e, 100 * MIN, 0, true));
        assert_eq!(lead(0), 0);
        assert_eq!(lead(5), 5 * MIN);
        assert_eq!(lead(15), 15 * MIN);
        // Only Spark's choices: anything else rounds down to one.
        assert_eq!(lead(4), 3 * MIN);
        assert_eq!(lead(60), 15 * MIN);
    }

    #[test]
    fn leaves_out_declined_canceled_all_day_and_long_events() {
        let now = 100 * MIN;
        let declined = Event {
            response: Response::Declined,
            ..standup()
        };
        let canceled = Event {
            canceled: true,
            ..standup()
        };
        let all_day = Event {
            all_day: true,
            ..standup()
        };
        let trip = Event {
            end: 100 * MIN + LONGEST_MS,
            ..standup()
        };
        for e in [declined, canceled, all_day, trip] {
            assert!(!due(&e, now, MIN, false), "{e:?}");
        }
        let unanswered = Event {
            response: Response::None,
            ..standup()
        };
        assert!(due(&unanswered, now, MIN, true));
    }

    #[test]
    fn guests_only_leaves_out_events_with_no_one_else_and_no_call() {
        let now = 100 * MIN;
        let focus = Event {
            title: "Focus time".into(),
            call_link: None,
            organizer: None,
            attendees: Vec::new(),
            response: Response::Organizer,
            ..standup()
        };
        assert!(!due(&focus, now, MIN, true));
        assert!(due(&focus, now, MIN, false));
        let solo_call = Event {
            call_link: Some("https://zoom.us/j/1".into()),
            ..focus.clone()
        };
        assert!(due(&solo_call, now, MIN, true));
        let with_guest = Event {
            attendees: vec![person("Kai Tanaka", "kai@acme.example")],
            ..focus
        };
        assert!(due(&with_guest, now, MIN, true));
    }

    #[test]
    fn people_leave_out_the_user_repeats_and_those_who_declined() {
        let mut e = standup();
        e.attendees = vec![
            person("Me", "ME@acme.example"),
            person("Sam again", "sam@acme.example"),
            Person {
                answer: Some(Answer::Declined),
                ..person("Kai Tanaka", "kai@acme.example")
            },
            person("Priya Raman", "priya@acme.example"),
        ];
        let m = meeting(7, e, "me@acme.example");
        let names: Vec<&str> = m.people.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["Sam Okafor", "Priya Raman"]);
    }

    #[test]
    fn chimes_for_a_new_moved_or_unsnoozed_meeting_not_a_renamed_one() {
        let standup = meeting(7, standup(), "me@acme.example");
        let moved = Meeting {
            start: standup.start + MIN,
            ..standup.clone()
        };
        let renamed = Meeting {
            title: "Daily standup".into(),
            ..standup.clone()
        };
        let preview = sample(0);
        assert!(arrived(&[], std::slice::from_ref(&standup)));
        assert!(arrived(std::slice::from_ref(&standup), &[moved]));
        assert!(arrived(
            std::slice::from_ref(&standup),
            &[preview, standup.clone()]
        ));
        assert!(!arrived(std::slice::from_ref(&standup), &[renamed]));
        // One dismissed out of two.
        assert!(!arrived(&[sample(0), standup.clone()], &[standup]));
    }

    #[test]
    fn meeting_json_matches_the_ui() {
        let json = serde_json::to_value(meeting(7, standup(), "me@acme.example")).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "id": 7,
                "title": "Standup",
                "start": 100 * MIN,
                "end": 115 * MIN,
                "callLink": "https://teams.microsoft.com/l/meetup-join/x",
                "people": [{ "name": "Sam Okafor", "address": "sam@acme.example" }],
                "joined": false,
            })
        );
    }

    #[test]
    fn one_meeting_in_two_calendars_shows_once() {
        let a = meeting(7, standup(), "me@acme.example");
        let b = Meeting {
            id: 9,
            title: "standup ".into(),
            ..a.clone()
        };
        assert!(same(&a, &b));
        let later = Meeting {
            start: a.start + MIN,
            ..b.clone()
        };
        assert!(!same(&a, &later));
        let other_call = Meeting {
            call_link: None,
            ..b
        };
        assert!(!same(&a, &other_call));
    }

    #[test]
    fn acts_come_from_the_ui_like_this() {
        let act: Act =
            serde_json::from_value(serde_json::json!({ "action": "join", "id": 3 })).unwrap();
        assert!(matches!(act, Act::Join { id: 3 }));
        let act: Act =
            serde_json::from_value(serde_json::json!({ "action": "dismissAll" })).unwrap();
        assert!(matches!(act, Act::DismissAll));
        let act: Act =
            serde_json::from_value(serde_json::json!({ "action": "openCalendar", "id": 3 }))
                .unwrap();
        assert!(matches!(act, Act::OpenCalendar { id: 3 }));
    }
}
