//! Demo mailbox the UI talks to until the real sync engine exists.
//!
//! The messages come from `demo/mailbox.json`, which the browser preview
//! (`src/api.ts`) also reads. Each entry gives its age in minutes instead of a
//! date, so the demo always looks recent; loading turns that into a timestamp.
//! Changes live in memory only.

use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Account {
    Personal,
    Work,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Category {
    People,
    Notifications,
    Newsletters,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MessageState {
    #[default]
    Inbox,
    Done,
    Snoozed,
    /// Set Aside: parked out of the inbox without a date.
    Aside,
    Deleted,
    Blocked,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Message {
    pub id: u32,
    pub account: Account,
    pub category: Category,
    #[serde(default)]
    pub state: MessageState,
    #[serde(default)]
    pub unread: bool,
    #[serde(default)]
    pub pinned: bool,
    #[serde(default)]
    pub priority: bool,
    /// From a sender not yet accepted through the Gatekeeper.
    #[serde(default)]
    pub screening: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snoozed_until: Option<String>,
    pub from: String,
    pub address: String,
    /// When the message arrived, in Unix milliseconds.
    #[serde(default)]
    pub timestamp: i64,
    pub subject: String,
    pub snippet: String,
    pub body: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attachment: Option<String>,
}

/// Fields the UI may change; absent fields stay as they are.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessagePatch {
    state: Option<MessageState>,
    snoozed_until: Option<String>,
    unread: Option<bool>,
    pinned: Option<bool>,
    priority: Option<bool>,
    screening: Option<bool>,
}

impl MessagePatch {
    /// Leaving the snoozed state clears the snooze date.
    fn apply(self, message: &mut Message) {
        if let Some(state) = self.state {
            message.state = state;
            if state != MessageState::Snoozed {
                message.snoozed_until = None;
            }
        }
        if let Some(until) = self.snoozed_until {
            message.snoozed_until = Some(until);
        }
        if let Some(unread) = self.unread {
            message.unread = unread;
        }
        if let Some(pinned) = self.pinned {
            message.pinned = pinned;
        }
        if let Some(priority) = self.priority {
            message.priority = priority;
        }
        if let Some(screening) = self.screening {
            message.screening = screening;
        }
    }
}

/// One entry of `demo/mailbox.json`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DemoEntry {
    age_minutes: i64,
    #[serde(flatten)]
    message: Message,
}

pub struct DemoMailbox(Mutex<Vec<Message>>);

impl DemoMailbox {
    pub fn load() -> Self {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as i64);
        Self(Mutex::new(demo_messages(now_ms)))
    }

    pub fn get(&self, id: u32) -> Option<Message> {
        self.0.lock().unwrap().iter().find(|m| m.id == id).cloned()
    }
}

/// Parses the demo file, dating each message `ageMinutes` before `now_ms`.
fn demo_messages(now_ms: i64) -> Vec<Message> {
    let entries: Vec<DemoEntry> = serde_json::from_str(include_str!("../../demo/mailbox.json"))
        .expect("demo/mailbox.json is valid");
    entries
        .into_iter()
        .map(|entry| Message {
            timestamp: now_ms - entry.age_minutes * 60_000,
            ..entry.message
        })
        .collect()
}

#[tauri::command]
pub fn list_messages(mailbox: tauri::State<'_, DemoMailbox>) -> Vec<Message> {
    mailbox.0.lock().unwrap().clone()
}

#[tauri::command]
pub fn update_message(
    mailbox: tauri::State<'_, DemoMailbox>,
    id: u32,
    patch: MessagePatch,
) -> Result<Message, String> {
    let mut messages = mailbox.0.lock().unwrap();
    let message = messages
        .iter_mut()
        .find(|m| m.id == id)
        .ok_or_else(|| format!("no message with id {id}"))?;
    patch.apply(message);
    Ok(message.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW_MS: i64 = 1_790_000_000_000;

    #[test]
    fn demo_mailbox_parses_with_defaults() {
        let messages = demo_messages(NOW_MS);
        assert!(messages.len() >= 12);
        assert!(messages.iter().all(|m| m.state == MessageState::Inbox));
        assert!(messages.iter().any(|m| m.screening));
        assert!(messages.iter().any(|m| m.priority));
    }

    #[test]
    fn ages_become_timestamps_before_load_time() {
        let raw: Vec<serde_json::Value> =
            serde_json::from_str(include_str!("../../demo/mailbox.json")).unwrap();
        let messages = demo_messages(NOW_MS);
        for (entry, message) in raw.iter().zip(&messages) {
            let age = entry["ageMinutes"]
                .as_i64()
                .expect("every entry has ageMinutes");
            assert_eq!(message.timestamp, NOW_MS - age * 60_000);
        }

        let json = serde_json::to_value(&messages[0]).unwrap();
        assert_eq!(json["timestamp"], messages[0].timestamp);
        assert!(json.get("ageMinutes").is_none());
    }

    #[test]
    fn leaving_snoozed_clears_the_snooze_date() {
        let mut message = demo_messages(NOW_MS).remove(0);
        MessagePatch {
            state: Some(MessageState::Snoozed),
            snoozed_until: Some("Tomorrow".into()),
            ..Default::default()
        }
        .apply(&mut message);
        assert_eq!(message.snoozed_until.as_deref(), Some("Tomorrow"));

        MessagePatch {
            state: Some(MessageState::Inbox),
            ..Default::default()
        }
        .apply(&mut message);
        assert_eq!(message.state, MessageState::Inbox);
        assert_eq!(message.snoozed_until, None);
    }
}
