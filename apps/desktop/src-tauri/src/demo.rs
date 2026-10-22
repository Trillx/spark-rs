//! The demo mailbox shown until a real account is added.
//!
//! The messages come from `demo/mailbox.json`, which the browser preview
//! (`src/api.ts`) also reads. Each entry gives its age in minutes instead of a
//! date, so the demo always looks recent; loading turns that into a timestamp.
//! Changes live in memory only.

use std::sync::Mutex;

use serde::Deserialize;
use spark_store::{AccountKind, Category, MessageState, Patch, now_ms};

use crate::mailbox::{AccountView, Message, SyncView};

/// The demo's two accounts, "personal" and "work" in the JSON.
pub fn accounts() -> Vec<AccountView> {
    let account = |id, kind, email: &str, label: &str, color: &str, provider| AccountView {
        id,
        kind,
        email: email.into(),
        name: "Me".into(),
        label: label.into(),
        color: color.into(),
        provider,
        demo: true,
        sync: SyncView::default(),
    };
    vec![
        account(
            1,
            AccountKind::Gmail,
            "me@gmail.com",
            "Personal",
            "#0ea5e9",
            "Gmail",
        ),
        account(
            2,
            AccountKind::Microsoft,
            "me@outlook.com",
            "Work",
            "#f59e0b",
            "Microsoft 365",
        ),
    ]
}

/// One entry of `demo/mailbox.json`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DemoEntry {
    id: i64,
    account: String,
    category: Category,
    #[serde(default)]
    state: MessageState,
    #[serde(default)]
    unread: bool,
    #[serde(default)]
    pinned: bool,
    #[serde(default)]
    priority: bool,
    #[serde(default)]
    screening: bool,
    from: String,
    address: String,
    age_minutes: i64,
    subject: String,
    snippet: String,
    body: Vec<String>,
    #[serde(default)]
    attachment: Option<String>,
}

pub struct DemoMailbox(Mutex<Vec<Message>>);

impl DemoMailbox {
    pub fn load() -> Self {
        Self(Mutex::new(demo_messages(now_ms())))
    }

    /// Every message; snoozed ones whose time has come are back in the inbox.
    pub fn list(&self) -> Vec<Message> {
        let mut messages = self.0.lock().unwrap();
        wake_snoozed(&mut messages, now_ms());
        messages.clone()
    }

    pub fn get(&self, id: i64) -> Option<Message> {
        self.0.lock().unwrap().iter().find(|m| m.id == id).cloned()
    }

    pub fn update(&self, id: i64, patch: &Patch) -> Result<Message, String> {
        let mut messages = self.0.lock().unwrap();
        let message = messages
            .iter_mut()
            .find(|m| m.id == id)
            .ok_or_else(|| format!("no message with id {id}"))?;
        apply(message, patch);
        Ok(message.clone())
    }
}

/// Same rules as `Store::update`, minus the Gatekeeper's per-sender ones.
fn apply(message: &mut Message, patch: &Patch) {
    if let Some(state) = patch.state {
        message.state = state;
        if state != MessageState::Snoozed {
            message.snoozed_until = None;
        }
    }
    if let Some(until) = patch.snoozed_until {
        message.snoozed_until = Some(until);
    }
    if let Some(unread) = patch.unread {
        message.unread = unread;
    }
    if let Some(pinned) = patch.pinned {
        message.pinned = pinned;
    }
    if let Some(priority) = patch.priority {
        message.priority = priority;
    }
    if let Some(screening) = patch.screening {
        message.screening = screening;
    }
}

fn wake_snoozed(messages: &mut [Message], now: i64) {
    for m in messages {
        if m.state == MessageState::Snoozed && m.snoozed_until.is_some_and(|t| t <= now) {
            m.state = MessageState::Inbox;
            m.snoozed_until = None;
            m.unread = true;
        }
    }
}

/// Parses the demo file, dating each message `ageMinutes` before `now_ms`.
fn demo_messages(now_ms: i64) -> Vec<Message> {
    let entries: Vec<DemoEntry> = serde_json::from_str(include_str!("../../demo/mailbox.json"))
        .expect("demo/mailbox.json is valid");
    entries
        .into_iter()
        .map(|e| Message {
            id: e.id,
            account: if e.account == "work" { 2 } else { 1 },
            category: e.category,
            state: e.state,
            snoozed_until: None,
            unread: e.unread,
            pinned: e.pinned,
            priority: e.priority,
            screening: e.screening,
            from: e.from,
            address: e.address,
            timestamp: now_ms - e.age_minutes * 60_000,
            subject: e.subject,
            snippet: e.snippet,
            body: Some(e.body),
            attachment: e.attachment,
        })
        .collect()
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
        assert!(messages.iter().any(|m| m.account == 2));
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
    fn snoozed_mail_wakes_up_unread() {
        let mut messages = demo_messages(NOW_MS);
        apply(
            &mut messages[0],
            &Patch {
                state: Some(MessageState::Snoozed),
                snoozed_until: Some(NOW_MS + 1_000),
                unread: Some(false),
                ..Default::default()
            },
        );
        wake_snoozed(&mut messages, NOW_MS);
        assert_eq!(messages[0].state, MessageState::Snoozed);
        wake_snoozed(&mut messages, NOW_MS + 1_000);
        assert_eq!(messages[0].state, MessageState::Inbox);
        assert!(messages[0].unread);
        assert_eq!(messages[0].snoozed_until, None);

        apply(
            &mut messages[0],
            &Patch {
                state: Some(MessageState::Done),
                ..Default::default()
            },
        );
        assert_eq!(messages[0].state, MessageState::Done);
    }
}
