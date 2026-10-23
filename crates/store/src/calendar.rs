//! Calendars and their events, as the calendar screen shows them. Events are
//! stored per calendar for the stretches of time fetched so far: fetching a
//! stretch again replaces what's stored in it. What an event says (title,
//! people, answer) is JSON owned by the mail crate; its times are columns so
//! a stretch can be listed.

use std::collections::HashSet;

use rusqlite::{OptionalExtension, Row, params};

use crate::{Result, Store};

/// Version 8: calendars and events.
pub(crate) const SCHEMA_CALENDAR: &str = "
CREATE TABLE calendars (
    id INTEGER PRIMARY KEY,
    account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    remote_id TEXT NOT NULL,
    name TEXT NOT NULL,
    color TEXT,
    is_default INTEGER NOT NULL DEFAULT 0,
    position INTEGER NOT NULL,
    UNIQUE (account_id, remote_id)
);

CREATE TABLE events (
    id INTEGER PRIMARY KEY,
    calendar_id INTEGER NOT NULL REFERENCES calendars(id) ON DELETE CASCADE,
    remote_id TEXT NOT NULL,
    start INTEGER NOT NULL,
    end INTEGER NOT NULL,
    data TEXT NOT NULL,
    UNIQUE (calendar_id, remote_id)
);
CREATE INDEX events_start ON events(start);
";

/// A calendar as a backend listed it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteCalendar {
    pub remote_id: String,
    pub name: String,
    /// Hex, e.g. "#0ea5e9"; `None` takes the account's color.
    pub color: Option<String>,
    pub is_default: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Calendar {
    pub id: i64,
    pub account_id: i64,
    pub remote_id: String,
    pub name: String,
    pub color: Option<String>,
    pub is_default: bool,
}

/// An event (one occurrence of a recurring one) as a backend fetched it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteEvent {
    pub remote_id: String,
    /// Unix ms, [start, end).
    pub start: i64,
    pub end: i64,
    /// JSON owned by the mail crate.
    pub data: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredEvent {
    pub id: i64,
    pub calendar_id: i64,
    pub account_id: i64,
    pub remote_id: String,
    pub start: i64,
    pub end: i64,
    pub data: String,
}

const EVENT_COLUMNS: &str =
    "e.id, e.calendar_id, c.account_id, e.remote_id, e.start, e.end, e.data";

fn event_row(r: &Row) -> rusqlite::Result<StoredEvent> {
    Ok(StoredEvent {
        id: r.get(0)?,
        calendar_id: r.get(1)?,
        account_id: r.get(2)?,
        remote_id: r.get(3)?,
        start: r.get(4)?,
        end: r.get(5)?,
        data: r.get(6)?,
    })
}

impl Store {
    /// Makes `listed` the account's calendars, in order, keeping the ids of
    /// those already stored. Calendars no longer listed go with their events.
    pub fn replace_calendars(&mut self, account_id: i64, listed: &[RemoteCalendar]) -> Result<()> {
        let tx = self.conn.transaction()?;
        for (position, c) in listed.iter().enumerate() {
            tx.execute(
                "INSERT INTO calendars (account_id, remote_id, name, color, is_default, position)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT (account_id, remote_id) DO UPDATE SET
                    name = excluded.name, color = excluded.color,
                    is_default = excluded.is_default, position = excluded.position",
                params![
                    account_id,
                    c.remote_id,
                    c.name,
                    c.color,
                    c.is_default,
                    position as i64
                ],
            )?;
        }
        let keep: HashSet<&str> = listed.iter().map(|c| c.remote_id.as_str()).collect();
        let stored: Vec<(i64, String)> = tx
            .prepare("SELECT id, remote_id FROM calendars WHERE account_id = ?1")?
            .query_map([account_id], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        for (id, remote_id) in stored {
            if !keep.contains(remote_id.as_str()) {
                tx.execute("DELETE FROM calendars WHERE id = ?1", [id])?;
            }
        }
        tx.commit()
    }

    /// Every account's calendars, accounts in order.
    pub fn calendars(&self) -> Result<Vec<Calendar>> {
        self.conn
            .prepare(
                "SELECT c.id, c.account_id, c.remote_id, c.name, c.color, c.is_default
                 FROM calendars c JOIN accounts a ON a.id = c.account_id
                 ORDER BY a.position, c.position",
            )?
            .query_map([], |r| {
                Ok(Calendar {
                    id: r.get(0)?,
                    account_id: r.get(1)?,
                    remote_id: r.get(2)?,
                    name: r.get(3)?,
                    color: r.get(4)?,
                    is_default: r.get(5)?,
                })
            })?
            .collect()
    }

    /// Makes `listed` the calendar's events in [start, end): stored ones
    /// there that aren't listed are gone from the server.
    pub fn replace_events(
        &mut self,
        calendar_id: i64,
        start: i64,
        end: i64,
        listed: &[RemoteEvent],
    ) -> Result<()> {
        let tx = self.conn.transaction()?;
        for e in listed {
            tx.execute(
                "INSERT INTO events (calendar_id, remote_id, start, end, data)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT (calendar_id, remote_id) DO UPDATE SET
                    start = excluded.start, end = excluded.end, data = excluded.data",
                params![calendar_id, e.remote_id, e.start, e.end, e.data],
            )?;
        }
        let keep: HashSet<&str> = listed.iter().map(|e| e.remote_id.as_str()).collect();
        let stored: Vec<(i64, String)> = tx
            .prepare(
                "SELECT id, remote_id FROM events
                 WHERE calendar_id = ?1 AND start < ?3 AND end > ?2",
            )?
            .query_map(params![calendar_id, start, end], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })?
            .collect::<rusqlite::Result<_>>()?;
        for (id, remote_id) in stored {
            if !keep.contains(remote_id.as_str()) {
                tx.execute("DELETE FROM events WHERE id = ?1", [id])?;
            }
        }
        tx.commit()
    }

    /// Events overlapping [start, end), earliest first.
    pub fn events_between(&self, start: i64, end: i64) -> Result<Vec<StoredEvent>> {
        self.conn
            .prepare(&format!(
                "SELECT {EVENT_COLUMNS} FROM events e JOIN calendars c ON c.id = e.calendar_id
                 WHERE e.start < ?2 AND e.end > ?1 ORDER BY e.start, e.end DESC"
            ))?
            .query_map(params![start, end], event_row)?
            .collect()
    }

    pub fn event(&self, id: i64) -> Result<Option<StoredEvent>> {
        self.conn
            .query_row(
                &format!(
                    "SELECT {EVENT_COLUMNS} FROM events e JOIN calendars c ON c.id = e.calendar_id
                     WHERE e.id = ?1"
                ),
                [id],
                event_row,
            )
            .optional()
    }

    /// Rewrites what an event says, e.g. after the user answers it.
    pub fn set_event_data(&self, id: i64, data: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE events SET data = ?2 WHERE id = ?1",
            params![id, data],
        )?;
        Ok(())
    }
}
