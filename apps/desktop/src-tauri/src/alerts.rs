//! Calendar alerts, Spark's event notifications: at the alert time Outlook
//! keeps for an event in one of the user's own calendars, a notification
//! says what starts and when ("Starts in 15 minutes at 10:00 AM"), worded as
//! Spark words it. Clicking it opens the event in the calendar.
//!
//! Spark's core snoozes an alert for 5, 10 or 30 minutes, but its Windows
//! notifications have no buttons; here they have Snooze, with those three,
//! and Dismiss. Like Spark, an alert missed while the app was closed or the
//! computer asleep isn't shown late.
//!
//! A timer here looks for alerts that are due among the stored events, and
//! reads the coming two weeks from the servers every hour, since Outlook's
//! reminders go up to two weeks ahead. What was shown or snoozed is saved
//! (`calendar-alerts.json`), so a restart neither repeats an alert nor
//! forgets a snooze.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use std::{fs, io};

use chrono::{DateTime, Datelike, Local, NaiveDate, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use spark_mail::calendar::{Event, Response};
use spark_store::now_ms;
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::Notify;

use crate::background;
use crate::mailbox::Mailbox;
use crate::notify::{self, Click};
use crate::prefs::PrefsState;

const FILE_NAME: &str = "calendar-alerts.json";

const MINUTE_MS: i64 = 60_000;
const DAY_MS: i64 = 24 * 60 * MINUTE_MS;
/// Outlook's longest reminder is two weeks; a day more covers all-day
/// events, whose reminders count from the midnight before.
const AHEAD_MS: i64 = 15 * DAY_MS;
const CHECK_EVERY: Duration = Duration::from_secs(5);
/// How often the due alerts are worked out again from the store; a wake
/// (new events, a snooze, a changed setting) does it sooner.
const SCAN_EVERY_MS: i64 = 60_000;
const REFRESH_EVERY: Duration = Duration::from_secs(60 * 60);

/// An alert shows this long after its time at most: on time, give or take
/// the timer and a slow first refresh, not after a night asleep.
const GRACE_MS: i64 = 2 * MINUTE_MS;
/// Alerts further ahead than this say the day and time, not how long.
const SAY_DAY_MS: i64 = 6 * 60 * MINUTE_MS;

/// The Snooze drop-down, as Spark's snoozes: id, label and minutes. The
/// first is the default, as Spark's automatic snooze is 5 minutes.
const SNOOZES: [(&str, &str, i64); 3] = [
    ("5", "5 minutes", 5),
    ("10", "10 minutes", 10),
    ("30", "30 minutes", 30),
];

#[derive(Default)]
pub struct Alerts {
    state: Mutex<AlertState>,
    /// Wakes the timer early: new events, a snooze, a changed setting.
    wake: Notify,
    /// Reads the coming weeks now: alerts were just turned on.
    reread: Notify,
    /// The first refresh is over, so what's stored isn't from a past
    /// session: a meeting canceled since then won't alert.
    refreshed: AtomicBool,
}

#[derive(Default)]
struct AlertState {
    /// Where `record` is saved; `None` until `start`.
    file: Option<PathBuf>,
    record: Record,
    /// The alerts of the events from yesterday to two weeks ahead, as of
    /// `scanned`.
    coming: Vec<Due>,
    scanned: i64,
}

/// What's saved: alerts shown and snoozed, by `key`, each kept until its
/// event ends.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct Record {
    /// Shown, snoozed or dismissed alerts: their event's end.
    shown: BTreeMap<String, i64>,
    /// Snoozed alerts: when they come back.
    snoozed: BTreeMap<String, i64>,
}

impl Record {
    fn load(path: &Path) -> Self {
        match fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Self::default(),
            Err(e) => {
                eprintln!("spark-rs: couldn't read {}: {e}", path.display());
                Self::default()
            }
        }
    }

    /// Writes a temporary file and renames it over the old one.
    fn save(&self, path: &Path) {
        let temp = path.with_extension("json.tmp");
        let json = serde_json::to_string(self).expect("the alert record serializes");
        let saved = path
            .parent()
            .map_or(Ok(()), fs::create_dir_all)
            .and_then(|()| fs::write(&temp, json))
            .and_then(|()| fs::rename(&temp, path));
        if let Err(e) = saved {
            eprintln!("spark-rs: couldn't save {}: {e}", path.display());
        }
    }

    /// Drops alerts whose events are over, and snoozes missed.
    fn forget_ended(&mut self, now: i64) {
        self.shown.retain(|_, end| *end > now);
        let shown = &self.shown;
        self.snoozed
            .retain(|key, until| shown.contains_key(key) && now < *until + GRACE_MS);
    }
}

/// An event's alert, due at `at`.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Due {
    /// The event's calendar, id there and start: a moved event alerts
    /// again, and loses its snooze.
    key: String,
    at: i64,
    /// The stored event's.
    id: i64,
    event: Event,
}

/// Whether an alert at `at` shows at `now`: on time, not late.
fn on_time(at: i64, now: i64) -> bool {
    at <= now && now < at + GRACE_MS
}

impl Alerts {
    /// Looks again now, e.g. after the settings or the events changed.
    pub fn changed(&self) {
        self.lock().scanned = 0;
        self.wake.notify_one();
    }

    /// Alerts were turned on: reads the coming weeks from the servers now.
    pub fn turned_on(&self) {
        self.reread.notify_one();
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, AlertState> {
        self.state.lock().unwrap()
    }

    /// Records a change to what was shown or snoozed, and saves it.
    fn update(&self, change: impl FnOnce(&mut Record)) {
        let mut state = self.lock();
        change(&mut state.record);
        state.scanned = 0;
        if let Some(path) = &state.file {
            state.record.save(path);
        }
    }
}

/// Starts the timer that shows alerts, and the hourly read of the coming
/// two weeks.
pub fn start(app: &AppHandle, config_dir: &Path) {
    {
        let alerts = app.state::<Alerts>();
        let mut state = alerts.lock();
        let path = config_dir.join(FILE_NAME);
        state.record = Record::load(&path);
        state.file = Some(path);
    }
    let handle = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            check(&handle);
            let alerts = handle.state::<Alerts>();
            tokio::select! {
                () = tokio::time::sleep(CHECK_EVERY) => {}
                () = alerts.wake.notified() => {}
            }
        }
    });
    let handle = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            refresh(&handle).await;
            let alerts = handle.state::<Alerts>();
            alerts.refreshed.store(true, Ordering::Relaxed);
            alerts.changed();
            tokio::select! {
                () = tokio::time::sleep(REFRESH_EVERY) => {}
                () = alerts.reread.notified() => {}
            }
        }
    });
}

/// Reads the coming two weeks from the servers, so alerts set far ahead
/// come on time whether or not the calendar screen was open.
async fn refresh(app: &AppHandle) {
    if !app.state::<PrefsState>().get().event_alerts {
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
            .refresh_calendar(account.id, now - DAY_MS, now + AHEAD_MS)
            .await;
    }
}

/// Shows the alerts that are due.
fn check(app: &AppHandle) {
    let prefs = app.state::<PrefsState>().get();
    let alerts = app.state::<Alerts>();
    if !prefs.event_alerts || !alerts.refreshed.load(Ordering::Relaxed) {
        return;
    }
    let now = now_ms();
    let mailbox = app.state::<Mailbox>();
    if now - alerts.lock().scanned >= SCAN_EVERY_MS {
        let coming = coming(&mailbox, now);
        let mut state = alerts.lock();
        state.coming = coming;
        state.scanned = now;
    }
    // Each with the time it's shown for: its own, or its snooze's.
    let due: Vec<(Due, i64)> = {
        let mut state = alerts.lock();
        state.record.forget_ended(now);
        let record = &state.record;
        state
            .coming
            .iter()
            .filter_map(|d| match record.snoozed.get(&d.key) {
                Some(&until) => on_time(until, now).then(|| (d.clone(), until)),
                None => (!record.shown.contains_key(&d.key) && on_time(d.at, now))
                    .then(|| (d.clone(), d.at)),
            })
            .collect()
    };
    if due.is_empty() {
        return;
    }
    alerts.update(|record| {
        for (d, _) in &due {
            record.snoozed.remove(&d.key);
            record.shown.insert(d.key.clone(), d.event.end);
        }
    });
    for (d, when) in due {
        let xml = toast_xml(&d.event, when, &Local);
        let end = d.event.end;
        let clicked = app.clone();
        let shown = notify::show_xml(app, &xml, Some(end), move |click| {
            on_click(&clicked, &d, &click);
        });
        if let Err(e) = shown {
            eprintln!("spark-rs: couldn't show a calendar alert: {e}");
            return;
        }
    }
}

/// The alerts of the stored events in the user's own calendars, of every
/// account, that haven't ended, earliest first. Calendars others share with
/// them don't alert.
fn coming(mailbox: &Mailbox, now: i64) -> Vec<Due> {
    if !mailbox.live() {
        return Vec::new();
    }
    let store = mailbox.live.engine.store();
    let (Ok(calendars), Ok(events)) = (
        store.calendars(),
        store.events_between(now - DAY_MS, now + AHEAD_MS),
    ) else {
        return Vec::new();
    };
    let mut coming: Vec<Due> = events
        .into_iter()
        .filter(|e| calendars.iter().any(|c| c.id == e.calendar_id && c.own))
        .filter_map(|stored| {
            let event = Event::from_json(&stored.data);
            let at = alert_time(&event, &Local)?;
            if !wanted(&event, now) {
                return None;
            }
            Some(Due {
                key: format!(
                    "{}/{}/{}",
                    stored.calendar_id, stored.remote_id, event.start
                ),
                at,
                id: stored.id,
                event,
            })
        })
        .collect();
    coming.sort_by_key(|d| d.at);
    coming
}

/// When `e`'s alert goes off (Unix ms), if it has one. An all-day event's
/// counts back from the midnight its day starts on the user's clock.
fn alert_time(e: &Event, tz: &impl TimeZone) -> Option<i64> {
    let before = e.reminder? * MINUTE_MS;
    let start = if e.all_day {
        local_midnight(utc_date(e.start)?, tz)?
    } else {
        e.start
    };
    Some(start - before)
}

/// Whether `e` alerts at all: not declined, canceled or over.
fn wanted(e: &Event, now: i64) -> bool {
    !e.canceled && e.response != Response::Declined && now < e.end
}

/// The calendar date an all-day event's UTC-midnight time stands for.
fn utc_date(ms: i64) -> Option<NaiveDate> {
    DateTime::<Utc>::from_timestamp_millis(ms).map(|t| t.date_naive())
}

/// The start of `date` on the clock of `tz` (Unix ms).
fn local_midnight(date: NaiveDate, tz: &impl TimeZone) -> Option<i64> {
    let midnight = date.and_hms_opt(0, 0, 0)?;
    tz.from_local_datetime(&midnight)
        .earliest()
        .map(|t| t.timestamp_millis())
}

/// What the alert says under the event's title at `when`, as Spark words
/// it, on the clock of `tz`.
fn starts(e: &Event, when: i64, tz: &impl TimeZone) -> String {
    let local = |ms: i64| {
        DateTime::<Utc>::from_timestamp_millis(ms).map(|t| t.with_timezone(tz).naive_local())
    };
    let Some(now) = local(when) else {
        return String::new();
    };
    // The year only when it isn't this one.
    let date = |d: NaiveDate| clock::date(&d, d.year() != now.year());
    if e.all_day {
        let Some(day) = utc_date(e.start) else {
            return String::new();
        };
        return match (day - now.date()).num_days() {
            ..=0 => "Starts today".into(),
            1 => "Starts tomorrow".into(),
            _ => format!("Starts on {}", date(day)),
        };
    }
    let left = e.start - when;
    if left < MINUTE_MS {
        return "Starts now".into();
    }
    let Some(start) = local(e.start) else {
        return String::new();
    };
    let at = clock::time(&start);
    if left <= SAY_DAY_MS {
        return format!("Starts in {} at {at}", duration(left));
    }
    match (start.date() - now.date()).num_days() {
        0 => format!("Starts Today at {at}"),
        1 => format!("Starts Tomorrow at {at}"),
        _ => format!("Starts on {} at {at}", date(start.date())),
    }
}

/// "15 minutes", "1 hour", "2 hours 5 minutes"; seconds are dropped.
fn duration(ms: i64) -> String {
    let minutes = ms / MINUTE_MS;
    let (h, m) = (minutes / 60, minutes % 60);
    let plural = |n: i64, unit: &str| format!("{n} {unit}{}", if n == 1 { "" } else { "s" });
    match (h, m) {
        (0, m) => plural(m, "minute"),
        (h, 0) => plural(h, "hour"),
        (h, m) => format!("{} {}", plural(h, "hour"), plural(m, "minute")),
    }
}

/// The toast: Spark's 📅 and the title, when it starts, then Snooze, for
/// as long as picked, and Dismiss. Spark plays the notification sound.
fn toast_xml(e: &Event, when: i64, tz: &impl TimeZone) -> String {
    let title = if e.title.trim().is_empty() {
        "No Title"
    } else {
        e.title.trim()
    };
    let texts = format!(
        "<text>📅 {}</text><text>{}</text>",
        notify::escape(title),
        notify::escape(&starts(e, when, tz))
    );
    let items: String = SNOOZES
        .iter()
        .map(|(id, label, _)| format!(r#"<selection id="{id}" content="{label}"/>"#))
        .collect();
    let actions = format!(
        r#"<actions><input id="snooze" type="selection" defaultInput="{}">{items}</input><action activationType="foreground" arguments="snooze" hint-inputId="snooze" content="Snooze"/><action activationType="system" arguments="dismiss" content="Dismiss"/></actions>"#,
        SNOOZES[0].0
    );
    format!(
        r#"<toast><visual><binding template="ToastGeneric">{texts}</binding></visual>{actions}<audio src="ms-winsoundevent:Notification.Reminder"/></toast>"#
    )
}

/// A click on an alert: Snooze puts it off, anything else opens the event
/// in the calendar.
fn on_click(app: &AppHandle, d: &Due, click: &Click) {
    let alerts = app.state::<Alerts>();
    if click.button == "snooze" {
        let until = now_ms() + snooze_minutes(click.choice.as_deref()) * MINUTE_MS;
        alerts.update(|record| {
            record.snoozed.insert(d.key.clone(), until);
        });
        alerts.wake.notify_one();
        return;
    }
    background::show(app);
    let _ = app.emit_to(
        "main",
        "open-calendar",
        OpenCalendar::event(d.id, d.event.start),
    );
}

/// How long the picked snooze is; 5 minutes if Windows didn't say.
fn snooze_minutes(choice: Option<&str>) -> i64 {
    SNOOZES
        .iter()
        .find(|(id, ..)| Some(*id) == choice)
        .map_or(SNOOZES[0].2, |(.., m)| *m)
}

/// What `open-calendar` asks the main window to show: the day of `at`, and
/// the stored event `event` there.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct OpenCalendar {
    at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    event: Option<i64>,
}

impl OpenCalendar {
    pub fn event(id: i64, start: i64) -> Self {
        Self {
            at: start,
            event: (id >= 0).then_some(id),
        }
    }
}

/// Times and dates the way Windows writes them for the user's region.
mod clock {
    use chrono::{NaiveDate, NaiveDateTime};

    /// "10:00 AM" or "10:00".
    pub fn time(t: &NaiveDateTime) -> String {
        #[cfg(windows)]
        if let Some(text) = windows::time(t) {
            return text;
        }
        t.format("%H:%M").to_string()
    }

    /// "Wed, Oct 28", with ", 2027" if `year`.
    pub fn date(d: &NaiveDate, year: bool) -> String {
        #[cfg(windows)]
        if let Some(text) = windows::date(d, year) {
            return text;
        }
        d.format(if year { "%a, %b %-d, %Y" } else { "%a, %b %-d" })
            .to_string()
    }

    #[cfg(windows)]
    mod windows {
        use chrono::{Datelike, NaiveDate, NaiveDateTime, Timelike};
        use windows::Win32::Foundation::SYSTEMTIME;
        use windows::Win32::Globalization::{
            ENUM_DATE_FORMATS_FLAGS, GetDateFormatEx, GetTimeFormatEx, TIME_NOSECONDS,
        };
        use windows::core::{PCWSTR, w};

        fn system_time(t: &NaiveDateTime) -> SYSTEMTIME {
            SYSTEMTIME {
                wYear: t.year() as u16,
                wMonth: t.month() as u16,
                wDayOfWeek: t.weekday().num_days_from_sunday() as u16,
                wDay: t.day() as u16,
                wHour: t.hour() as u16,
                wMinute: t.minute() as u16,
                wSecond: 0,
                wMilliseconds: 0,
            }
        }

        /// What a Get…FormatEx call wrote into `buf`, without its NUL.
        fn written(buf: &[u16], n: i32) -> Option<String> {
            (n > 1).then(|| String::from_utf16_lossy(&buf[..n as usize - 1]))
        }

        pub fn time(t: &NaiveDateTime) -> Option<String> {
            let mut buf = [0u16; 64];
            let st = system_time(t);
            // The user's locale, without seconds.
            let n = unsafe {
                GetTimeFormatEx(
                    PCWSTR::null(),
                    TIME_NOSECONDS,
                    Some(&st),
                    PCWSTR::null(),
                    Some(&mut buf),
                )
            };
            written(&buf, n)
        }

        pub fn date(d: &NaiveDate, year: bool) -> Option<String> {
            let mut buf = [0u16; 64];
            let st = system_time(&d.and_hms_opt(0, 0, 0)?);
            let n = unsafe {
                GetDateFormatEx(
                    PCWSTR::null(),
                    ENUM_DATE_FORMATS_FLAGS(0),
                    Some(&st),
                    if year {
                        w!("ddd, MMM d, yyyy")
                    } else {
                        w!("ddd, MMM d")
                    },
                    Some(&mut buf),
                    PCWSTR::null(),
                )
            };
            written(&buf, n)
        }
    }
}

#[cfg(test)]
mod tests {
    use chrono::FixedOffset;

    use super::*;

    const MIN: i64 = MINUTE_MS;
    const HOUR: i64 = 60 * MIN;
    /// 2026-10-28T10:00:00Z, a Wednesday.
    const TEN: i64 = 1_793_181_600_000;
    /// Its day, all day.
    const MIDNIGHT: i64 = TEN - 10 * HOUR;

    fn standup() -> Event {
        Event {
            title: "Standup".into(),
            start: TEN,
            end: TEN + 15 * MIN,
            response: Response::Accepted,
            reminder: Some(15),
            ..Default::default()
        }
    }

    fn offsite() -> Event {
        Event {
            title: "Offsite".into(),
            all_day: true,
            start: MIDNIGHT,
            end: MIDNIGHT + DAY_MS,
            reminder: Some(18 * 60),
            ..standup()
        }
    }

    #[test]
    fn alerts_go_off_the_reminder_before_the_start() {
        assert_eq!(alert_time(&standup(), &Utc), Some(TEN - 15 * MIN));
        let at_start = Event {
            reminder: Some(0),
            ..standup()
        };
        assert_eq!(alert_time(&at_start, &Utc), Some(TEN));
        let none = Event {
            reminder: None,
            ..standup()
        };
        assert_eq!(alert_time(&none, &Utc), None);
    }

    #[test]
    fn all_day_alerts_count_from_midnight_on_the_users_clock() {
        // Outlook's 18 hours: 6 AM the day before.
        let six_am = MIDNIGHT - 18 * HOUR;
        assert_eq!(alert_time(&offsite(), &Utc), Some(six_am));
        let pacific = FixedOffset::west_opt(7 * 3600).unwrap();
        assert_eq!(alert_time(&offsite(), &pacific), Some(six_am + 7 * HOUR));
    }

    #[test]
    fn alerts_come_on_time_and_never_late() {
        let at = TEN - 15 * MIN;
        assert!(!on_time(at, at - 1));
        assert!(on_time(at, at));
        assert!(on_time(at, at + MIN), "the timer, a slow first refresh");
        // Missed while the app was closed or the computer asleep: dropped, as in Spark.
        assert!(!on_time(at, at + GRACE_MS));
        assert!(!on_time(at, TEN - 2 * MIN));
    }

    #[test]
    fn declined_canceled_and_ended_events_dont_alert() {
        assert!(wanted(&standup(), TEN));
        let declined = Event {
            response: Response::Declined,
            ..standup()
        };
        let canceled = Event {
            canceled: true,
            ..standup()
        };
        assert!(!wanted(&declined, TEN - 15 * MIN));
        assert!(!wanted(&canceled, TEN - 15 * MIN));
        assert!(!wanted(&standup(), TEN + 15 * MIN));
    }

    #[test]
    fn timed_alerts_say_how_long_up_to_six_hours_then_the_day() {
        let e = standup();
        let says = |when: i64| starts(&e, when, &Utc);
        assert_eq!(says(TEN - 15 * MIN), "Starts in 15 minutes at 10:00");
        // Seconds are dropped, not rounded up.
        assert_eq!(
            says(TEN - 15 * MIN + 1_000),
            "Starts in 14 minutes at 10:00"
        );
        assert_eq!(says(TEN - 90 * MIN), "Starts in 1 hour 30 minutes at 10:00");
        assert_eq!(says(TEN - 6 * HOUR), "Starts in 6 hours at 10:00");
        assert_eq!(says(TEN - 30_000), "Starts now");
        assert_eq!(says(TEN + 3 * MIN), "Starts now", "a snooze past the start");
        let early = Event {
            start: TEN + 10 * HOUR,
            end: TEN + 11 * HOUR,
            ..standup()
        };
        assert_eq!(
            starts(&early, TEN - 3 * HOUR, &Utc),
            "Starts Today at 20:00"
        );
        assert_eq!(starts(&e, TEN - DAY_MS, &Utc), "Starts Tomorrow at 10:00");
        assert_eq!(
            starts(&e, TEN - 7 * DAY_MS, &Utc),
            "Starts on Wed, Oct 28 at 10:00"
        );
        let next_year = Event {
            start: TEN + 70 * DAY_MS,
            end: TEN + 70 * DAY_MS + HOUR,
            ..standup()
        };
        assert_eq!(
            starts(&next_year, TEN, &Utc),
            "Starts on Wed, Jan 6, 2027 at 10:00"
        );
    }

    #[test]
    fn all_day_alerts_say_the_day() {
        let e = offsite();
        assert_eq!(starts(&e, MIDNIGHT + 9 * HOUR, &Utc), "Starts today");
        assert_eq!(starts(&e, MIDNIGHT - 18 * HOUR, &Utc), "Starts tomorrow");
        assert_eq!(
            starts(&e, MIDNIGHT - 7 * DAY_MS, &Utc),
            "Starts on Wed, Oct 28"
        );
        // The day is the user's: 6 PM the evening before in UTC−7 is still the day before.
        let pacific = FixedOffset::west_opt(7 * 3600).unwrap();
        assert_eq!(starts(&e, MIDNIGHT + HOUR, &pacific), "Starts tomorrow");
    }

    #[test]
    fn the_toast_has_sparks_title_and_the_snoozes() {
        let e = Event {
            title: "Q&A <prep>".into(),
            ..standup()
        };
        let xml = toast_xml(&e, TEN - 15 * MIN, &Utc);
        assert!(xml.starts_with(
            r#"<toast><visual><binding template="ToastGeneric"><text>📅 Q&amp;A &lt;prep&gt;</text><text>Starts in 15 minutes at 10:00</text></binding></visual>"#
        ));
        assert!(xml.contains(r#"<input id="snooze" type="selection" defaultInput="5"><selection id="5" content="5 minutes"/><selection id="10" content="10 minutes"/><selection id="30" content="30 minutes"/></input>"#));
        assert!(xml.contains(r#"arguments="snooze" hint-inputId="snooze""#));
        assert!(xml.contains(r#"activationType="system" arguments="dismiss""#));
        let untitled = Event {
            title: " ".into(),
            ..standup()
        };
        assert!(toast_xml(&untitled, TEN, &Utc).contains("<text>📅 No Title</text>"));
    }

    #[test]
    fn snoozes_are_sparks_five_ten_and_thirty_minutes() {
        assert_eq!(snooze_minutes(Some("5")), 5);
        assert_eq!(snooze_minutes(Some("10")), 10);
        assert_eq!(snooze_minutes(Some("30")), 30);
        assert_eq!(snooze_minutes(None), 5, "Spark's automatic snooze");
        assert_eq!(snooze_minutes(Some("start")), 5);
    }

    #[test]
    fn the_record_forgets_ended_events_and_missed_snoozes() {
        let mut record = Record::default();
        record.shown.insert("a".into(), TEN);
        record.shown.insert("b".into(), TEN + DAY_MS);
        record.shown.insert("c".into(), TEN + DAY_MS);
        record.snoozed.insert("a".into(), TEN - MIN);
        record.snoozed.insert("b".into(), TEN + 5 * MIN);
        record.snoozed.insert("c".into(), TEN - 10 * MIN);
        record.forget_ended(TEN);
        assert_eq!(record.shown.keys().collect::<Vec<_>>(), ["b", "c"]);
        assert_eq!(record.snoozed.keys().collect::<Vec<_>>(), ["b"]);
        let json = serde_json::to_string(&record).unwrap();
        assert_eq!(serde_json::from_str::<Record>(&json).unwrap(), record);
        assert_eq!(
            serde_json::from_str::<Record>("{}").unwrap(),
            Record::default()
        );
    }

    #[test]
    fn open_calendar_names_the_event() {
        let json = serde_json::to_value(OpenCalendar::event(7, TEN)).unwrap();
        assert_eq!(json, serde_json::json!({ "at": TEN, "event": 7 }));
        let sample = serde_json::to_value(OpenCalendar::event(-1, TEN)).unwrap();
        assert_eq!(sample, serde_json::json!({ "at": TEN }));
    }
}
