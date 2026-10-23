//! What the calendar screen talks to: the Microsoft 365 calendars and events
//! in the store, refreshed from the servers when the screen asks, answering
//! events, and signing in again to grant the calendar permission. Types here
//! are the UI's shapes (`src/types.ts` mirrors them). The demo calendar
//! lives in the UI, which knows the local time zone its events sit in, so
//! these list nothing until a real account is added.

use serde::Serialize;
use spark_mail::CalendarStatus;
use spark_mail::calendar::Event;
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
        assert!(json.get("location").is_none(), "absent, not null");
    }
}
