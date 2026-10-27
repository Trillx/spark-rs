//! What the calendar screen talks to: the Microsoft 365 calendars and events
//! in the store, refreshed from the servers when the screen asks, answering
//! events, making, changing and deleting the user's own, and signing in
//! again to grant the calendar permission. Types here are the UI's shapes
//! (`src/types.ts` mirrors them). The demo calendar lives in the UI, which
//! knows the local time zone its events sit in, so these list nothing until
//! a real account is added.

use serde::Serialize;
use spark_mail::CalendarStatus;
use spark_mail::calendar::{Event, EventDraft, Scope, Zone};
use spark_mail::invite::Answer;
use tauri::{AppHandle, Emitter, State};

use crate::mailbox::{AccountView, Mailbox, text};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CalendarView {
    id: i64,
    account: i64,
    name: String,
    /// Its own color, else its account's.
    color: String,
    is_default: bool,
    /// The user's own, where they can make and change events; not someone
    /// else's shared with them.
    own: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CalendarAccountState {
    account: i64,
    #[serde(flatten)]
    status: CalendarStatus,
}

#[derive(Debug, Serialize)]
pub struct CalendarList {
    calendars: Vec<CalendarView>,
    accounts: Vec<CalendarAccountState>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EventView {
    id: i64,
    calendar: i64,
    account: i64,
    #[serde(flatten)]
    event: Event,
}

#[tauri::command]
pub fn list_calendars(mailbox: State<'_, Mailbox>) -> Result<CalendarList, String> {
    let mut list = CalendarList {
        calendars: Vec::new(),
        accounts: Vec::new(),
    };
    if !mailbox.live() {
        return Ok(list);
    }
    let engine = &mailbox.live.engine;
    let (accounts, calendars) = {
        let store = engine.store();
        (
            store.accounts().map_err(text)?,
            store.calendars().map_err(text)?,
        )
    };
    for c in calendars {
        let Some(account) = accounts.iter().find(|a| a.id == c.account_id) else {
            continue;
        };
        list.calendars.push(CalendarView {
            id: c.id,
            account: c.account_id,
            name: c.name,
            color: c.color.unwrap_or_else(|| account.color.clone()),
            is_default: c.is_default,
            own: c.own,
        });
    }
    list.accounts = accounts
        .iter()
        .map(|a| CalendarAccountState {
            account: a.id,
            status: engine.calendar_status(a),
        })
        .collect();
    Ok(list)
}

/// Stored events overlapping [start, end) (Unix ms).
#[tauri::command]
pub fn list_events(
    mailbox: State<'_, Mailbox>,
    start: i64,
    end: i64,
) -> Result<Vec<EventView>, String> {
    if !mailbox.live() {
        return Ok(Vec::new());
    }
    let stored = mailbox
        .live
        .engine
        .store()
        .events_between(start, end)
        .map_err(text)?;
    Ok(stored
        .into_iter()
        .map(|e| EventView {
            id: e.id,
            calendar: e.calendar_id,
            account: e.account_id,
            event: Event::from_json(&e.data),
        })
        .collect())
}

/// Fetches every account's calendar for [start, end). An account that
/// fails says so in `list_calendars` instead.
#[tauri::command]
pub async fn refresh_calendars(
    mailbox: State<'_, Mailbox>,
    start: i64,
    end: i64,
) -> Result<(), String> {
    if !mailbox.live() {
        return Ok(());
    }
    let engine = &mailbox.live.engine;
    let accounts = engine.store().accounts().map_err(text)?;
    for account in accounts {
        let _ = engine.refresh_calendar(account.id, start, end).await;
    }
    Ok(())
}

/// Answers an event in its calendar; resolves once the server has it.
#[tauri::command]
pub async fn answer_event(
    mailbox: State<'_, Mailbox>,
    id: i64,
    answer: Answer,
) -> Result<(), String> {
    if !mailbox.live() {
        return Ok(());
    }
    mailbox
        .live
        .engine
        .answer_event(id, answer)
        .await
        .map_err(text)
}

/// Makes an event (no `id`), or changes the user's stored event `id`
/// (`scope`: that occurrence or the whole series; `before`: as the editor
/// opened it, so only what the user changed is sent), then reads its
/// account's calendar for [start, end) (what the screen shows) again.
/// Resolves once the server has it; Outlook mails the people invited.
#[tauri::command]
pub async fn save_event(
    mailbox: State<'_, Mailbox>,
    id: Option<i64>,
    draft: EventDraft,
    before: Option<EventDraft>,
    scope: Option<Scope>,
    start: i64,
    end: i64,
) -> Result<(), String> {
    if !mailbox.live() {
        return Ok(());
    }
    let engine = &mailbox.live.engine;
    let account = match id {
        Some(id) => event_account(&mailbox, id)?,
        None => engine
            .store()
            .calendars()
            .map_err(text)?
            .into_iter()
            .find(|c| c.id == draft.calendar)
            .map(|c| c.account_id)
            .ok_or("That calendar is no longer here.")?,
    };
    let name = zone_name();
    let zone = Zone {
        name: &name,
        wall: local_wall,
    };
    match id {
        Some(id) => engine
            .update_event(id, &draft, before.as_ref(), scope.unwrap_or_default(), zone)
            .await
            .map_err(text)?,
        None => engine.create_event(&draft, zone).await.map_err(text)?,
    }
    let _ = engine.refresh_calendar(account, start, end).await;
    Ok(())
}

/// Deletes the user's stored event `id` (`scope`: that occurrence or the
/// whole series), then reads its account's calendar for [start, end) again.
#[tauri::command]
pub async fn delete_event(
    mailbox: State<'_, Mailbox>,
    id: i64,
    scope: Option<Scope>,
    start: i64,
    end: i64,
) -> Result<(), String> {
    if !mailbox.live() {
        return Ok(());
    }
    let account = event_account(&mailbox, id)?;
    let engine = &mailbox.live.engine;
    engine
        .delete_event(id, scope.unwrap_or_default())
        .await
        .map_err(text)?;
    let _ = engine.refresh_calendar(account, start, end).await;
    Ok(())
}

fn event_account(mailbox: &Mailbox, id: i64) -> Result<i64, String> {
    Ok(mailbox
        .live
        .engine
        .store()
        .event(id)
        .map_err(text)?
        .ok_or("That event is no longer here.")?
        .account_id)
}

/// The user's time zone by Windows' name for it, which Graph takes; UTC
/// where that isn't known.
fn zone_name() -> String {
    #[cfg(windows)]
    {
        let key = windows_registry::LOCAL_MACHINE
            .open(r"SYSTEM\CurrentControlSet\Control\TimeZoneInformation")
            .and_then(|k| k.get_string("TimeZoneKeyName"));
        if let Ok(name) = key {
            let name = name.trim_end_matches('\0').trim();
            if !name.is_empty() {
                return name.to_owned();
            }
        }
    }
    "UTC".to_owned()
}

/// `ms` on the user's clock, without the zone: "2026-10-27T09:30:00".
fn local_wall(ms: i64) -> String {
    #[cfg(windows)]
    {
        use chrono::TimeZone;
        if let Some(t) = chrono::Local.timestamp_millis_opt(ms).single() {
            return t.format("%Y-%m-%dT%H:%M:%S").to_string();
        }
    }
    spark_mail::calendar::utc_wall(ms)
}

/// Signs in to Microsoft again for an existing account, in the system
/// browser, granting the calendar permission. `cancel_sign_in` stops it.
#[tauri::command]
pub async fn reconnect_account(
    app: AppHandle,
    mailbox: State<'_, Mailbox>,
    id: i64,
) -> Result<AccountView, String> {
    let cancelled = mailbox.live.sign_in_cancel();
    let sign_in = mailbox.live.engine.reconnect_microsoft_account(id, |url| {
        open::that_detached(url).map_err(|e| e.to_string())
    });
    let account = tokio::select! {
        result = sign_in => result.map_err(text)?,
        _ = cancelled => return Err("Sign-in cancelled.".into()),
    };
    let _ = app.emit("accounts-changed", ());
    Ok(mailbox.account_view(account))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shapes `src/types.ts` expects.
    #[test]
    fn calendar_json_matches_the_ui() {
        let state = CalendarAccountState {
            account: 2,
            status: CalendarStatus::Error {
                message: "Offline".into(),
            },
        };
        assert_eq!(
            serde_json::to_value(&state).unwrap(),
            serde_json::json!({ "account": 2, "status": "error", "message": "Offline" })
        );
        let ready = CalendarAccountState {
            account: 1,
            status: CalendarStatus::SignIn,
        };
        assert_eq!(
            serde_json::to_value(&ready).unwrap(),
            serde_json::json!({ "account": 1, "status": "signIn" })
        );
        let event = EventView {
            id: 7,
            calendar: 3,
            account: 2,
            event: Event {
                title: "Standup".into(),
                start: 1,
                end: 2,
                ..Default::default()
            },
        };
        let json = serde_json::to_value(&event).unwrap();
        for key in [
            "id",
            "calendar",
            "account",
            "title",
            "start",
            "end",
            "allDay",
            "attendees",
            "canceled",
            "repeats",
            "private",
        ] {
            assert!(json.get(key).is_some(), "{key} in {json}");
        }
        assert_eq!(json["response"], "none");
        assert_eq!(json["showAs"], "busy");
        assert_eq!(json["onlineMeeting"], false);
        assert!(json.get("location").is_none(), "absent, not null");
        assert!(json.get("series").is_none(), "absent, not null");
    }

    /// What the editor sends, as `src/types.ts` writes it.
    #[test]
    fn the_editors_draft_reads_as_the_ui_writes_it() {
        let draft: EventDraft = serde_json::from_value(serde_json::json!({
            "calendar": 3,
            "title": "Review",
            "start": 1000,
            "end": 2000,
            "allDay": false,
            "location": "",
            "notes": "",
            "attendees": [{ "name": "", "address": "sam@contoso.com" }],
            "reminder": null,
            "showAs": "free",
            "teams": true
        }))
        .unwrap();
        assert_eq!(draft.attendees[0].address, "sam@contoso.com");
        assert_eq!(draft.reminder, None);
        assert!(draft.teams);
        let scope: Scope = serde_json::from_value(serde_json::json!("all")).unwrap();
        assert_eq!(scope, Scope::All);
    }
}
