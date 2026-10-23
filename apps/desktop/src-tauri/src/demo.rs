//! The demo mailbox shown until a real account is added.
//!
//! The messages come from `demo/mailbox.json`, which the browser preview
//! (`src/api.ts`) also reads. Each entry gives its age in minutes instead of a
//! date, so the demo always looks recent; loading turns that into a timestamp.
//! Changes live in memory only.

use std::collections::HashMap;
use std::sync::Mutex;

use serde::Deserialize;
use spark_mail::draft::{Draft, OutgoingAttachment};
use spark_mail::{drafts, parse};
use spark_store::{AccountKind, Category, FolderRole, MessageState, Patch, now_ms};

use crate::mailbox::{AccountView, FolderView, Message, SyncView};

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

/// One entry of `demo/folders.json`.
#[derive(Deserialize)]
struct DemoFolder {
    id: i64,
    account: String,
    name: String,
    #[serde(default)]
    parent: Option<i64>,
    #[serde(default)]
    role: Option<FolderRole>,
    members: Vec<i64>,
}

fn demo_folders() -> Vec<DemoFolder> {
    serde_json::from_str(include_str!("../../demo/folders.json"))
        .expect("demo/folders.json is valid")
}

fn account_id(name: &str) -> i64 {
    if name == "work" { 2 } else { 1 }
}

/// The demo accounts' folders, as if synced.
pub fn folders() -> Vec<FolderView> {
    demo_folders()
        .into_iter()
        .map(|f| FolderView {
            id: f.id,
            account: account_id(&f.account),
            name: f.name,
            parent: f.parent,
            role: f.role,
            selectable: true,
            synced: true,
        })
        .collect()
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
    /// Sent mail and drafts say who they're to.
    #[serde(default)]
    to: Vec<String>,
    age_minutes: i64,
    subject: String,
    snippet: String,
    body: Vec<String>,
    #[serde(default)]
    attachment: Option<String>,
}

pub struct DemoMailbox(Mutex<Vec<Message>>, Mutex<DemoDrafts>);

/// Drafts saved in the demo as the composer had them, and drafts being
/// "sent", by outbox id, until their undo window is over.
#[derive(Default)]
struct DemoDrafts {
    saved: HashMap<i64, Draft>,
    sending: HashMap<i64, Message>,
}

impl DemoMailbox {
    pub fn load() -> Self {
        Self(
            Mutex::new(demo_messages(now_ms())),
            Mutex::new(DemoDrafts::default()),
        )
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
        apply(message, patch, &folders())?;
        Ok(message.clone())
    }
}

/// Drafts, like the real ones but in memory.
impl DemoMailbox {
    pub fn save_draft(&self, mut draft: Draft) -> Result<i64, String> {
        let drafts_folder = demo_folders()
            .into_iter()
            .find(|f| account_id(&f.account) == draft.account && f.role == Some(FolderRole::Drafts))
            .ok_or("The demo account has no Drafts folder.")?;
        let address = accounts()
            .into_iter()
            .find(|a| a.id == draft.account)
            .map(|a| a.email)
            .unwrap_or_default();
        let mut messages = self.0.lock().unwrap();
        let id = draft
            .draft_id
            .filter(|id| messages.iter().any(|m| m.id == *id && is_draft(m)))
            .unwrap_or_else(|| messages.iter().map(|m| m.id).max().unwrap_or(0) + 1);
        draft.draft_id = Some(id);
        let saved = Message {
            id,
            account: draft.account,
            category: Category::People,
            state: MessageState::Filed,
            snoozed_until: None,
            unread: false,
            pinned: false,
            priority: false,
            screening: false,
            from: "Me".into(),
            address,
            to: drafts::display_list(&draft.to),
            timestamp: now_ms(),
            subject: draft.subject.trim().to_owned(),
            snippet: parse::snippet(&draft.body),
            body: Some(draft.body.split("\n\n").map(str::to_owned).collect()),
            attachment: draft.attachments.first().map(|a| match a {
                OutgoingAttachment::File { name, .. }
                | OutgoingAttachment::Message { name, .. } => name.clone(),
            }),
            folders: vec![drafts_folder.id],
            folder: Some(drafts_folder.id),
        };
        messages.retain(|m| m.id != id);
        messages.push(saved);
        self.1.lock().unwrap().saved.insert(id, draft);
        Ok(id)
    }

    pub fn open_draft(&self, id: i64) -> Option<Draft> {
        if let Some(saved) = self.1.lock().unwrap().saved.get(&id) {
            return Some(saved.clone());
        }
        let m = self.get(id).filter(is_draft)?;
        Some(Draft {
            account: m.account,
            to: m.to.join(", "),
            cc: String::new(),
            bcc: String::new(),
            subject: m.subject,
            body: m.body.unwrap_or_default().join("\n\n"),
            reply_to: None,
            send_at: 0,
            attachments: Vec::new(),
            draft_id: Some(id),
            in_reply_to: None,
            references: None,
        })
    }

    pub fn discard_draft(&self, id: i64) {
        self.0
            .lock()
            .unwrap()
            .retain(|m| m.id != id || !is_draft(m));
        self.1.lock().unwrap().saved.remove(&id);
    }

    /// Pretends to queue a send; the draft it came from waits out of the list.
    pub fn send(&self, draft: &Draft) -> i64 {
        // Ids only need to be unique.
        let id = now_ms();
        if let Some(saved) = draft.draft_id {
            let mut messages = self.0.lock().unwrap();
            if let Some(i) = messages.iter().position(|m| m.id == saved && is_draft(m)) {
                self.1
                    .lock()
                    .unwrap()
                    .sending
                    .insert(id, messages.remove(i));
            }
        }
        id
    }

    /// Undo send: the draft is back.
    pub fn cancel_send(&self, id: i64) -> bool {
        if let Some(draft) = self.1.lock().unwrap().sending.remove(&id) {
            self.0.lock().unwrap().push(draft);
        }
        true
    }
}

/// In a demo Drafts folder.
fn is_draft(m: &Message) -> bool {
    let drafts: Vec<i64> = demo_folders()
        .into_iter()
        .filter(|f| f.role == Some(FolderRole::Drafts))
        .map(|f| f.id)
        .collect();
    m.folders.iter().any(|id| drafts.contains(id))
}

/// Same rules as `Store::update`, minus the Gatekeeper's per-sender ones.
fn apply(message: &mut Message, patch: &Patch, folders: &[FolderView]) -> Result<(), String> {
    let mut state = patch.state;
    let mut home = message.folder;
    if let Some(id) = patch.folder {
        let folder = folders
            .iter()
            .find(|f| f.id == id)
            .ok_or("That folder is gone.")?;
        if folder.account != message.account {
            return Err("An email can only be moved to a folder of its own account.".into());
        }
        if !folder.selectable {
            return Err("That folder only holds other folders.".into());
        }
        state = Some(match folder.role {
            Some(FolderRole::Archive) => MessageState::Done,
            Some(FolderRole::Trash) => MessageState::Deleted,
            _ => {
                home = Some(id);
                MessageState::Filed
            }
        });
    }
    if let Some(state) = state {
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
    // Listed in its home folder only, as after a folder sync.
    let home = home.filter(|_| message.state == MessageState::Filed);
    if home != message.folder {
        message.folders.retain(|f| Some(*f) != message.folder);
        if let Some(home) = home.filter(|f| !message.folders.contains(f)) {
            message.folders.push(home);
        }
        message.folder = home;
    }
    Ok(())
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
    let folders = demo_folders();
    entries
        .into_iter()
        .map(|e| Message {
            id: e.id,
            account: account_id(&e.account),
            folders: folders
                .iter()
                .filter(|f| f.members.contains(&e.id))
                .map(|f| f.id)
                .collect(),
            folder: None,
            category: e.category,
            state: e.state,
            snoozed_until: None,
            unread: e.unread,
            pinned: e.pinned,
            priority: e.priority,
            screening: e.screening,
            from: e.from,
            address: e.address,
            to: e.to,
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
        // Mail is in the inbox unless it says otherwise, and filed mail is in a folder.
        assert!(messages.iter().all(|m| m.state == MessageState::Inbox
            || (m.state == MessageState::Filed && !m.folders.is_empty())));
        assert!(
            messages
                .iter()
                .any(|m| m.state == MessageState::Filed && !m.to.is_empty())
        );
        assert!(messages.iter().any(|m| m.screening));
        assert!(messages.iter().any(|m| m.priority));
        assert!(messages.iter().any(|m| m.account == 2));
    }

    #[test]
    fn demo_folders_hold_mail_of_their_own_account() {
        let messages = demo_messages(NOW_MS);
        let folders = folders();
        assert!(folders.iter().any(|f| f.parent.is_some()));
        assert!(folders.iter().any(|f| f.role == Some(FolderRole::Sent)));
        for m in &messages {
            for id in &m.folders {
                let folder = folders.iter().find(|f| f.id == *id).expect("a demo folder");
                assert_eq!(
                    folder.account, m.account,
                    "message {} in {}",
                    m.id, folder.name
                );
            }
        }
        assert!(messages.iter().any(|m| !m.folders.is_empty()));
    }

    #[test]
    fn demo_drafts_open_save_and_send() {
        let demo = DemoMailbox::load();
        let listed = demo.list();
        let draft = listed.iter().find(|m| is_draft(m)).expect("a demo draft");
        let mut opened = demo.open_draft(draft.id).unwrap();
        assert_eq!(opened.draft_id, Some(draft.id));
        assert_eq!(opened.to, draft.to.join(", "));
        assert!(
            demo.open_draft(listed.iter().find(|m| !is_draft(m)).unwrap().id)
                .is_none()
        );

        // Saved again: the same row, with the new text.
        opened.body = "Edited\n\nThanks".into();
        assert_eq!(demo.save_draft(opened.clone()).unwrap(), draft.id);
        let saved = demo.get(draft.id).unwrap();
        assert_eq!(saved.body.unwrap(), ["Edited", "Thanks"]);
        assert_eq!(demo.open_draft(draft.id).unwrap().body, "Edited\n\nThanks");

        // A new one goes into its account's Drafts.
        let new = demo
            .save_draft(Draft {
                draft_id: None,
                account: 1,
                ..opened.clone()
            })
            .unwrap();
        assert!(is_draft(&demo.get(new).unwrap()));

        // Sending hides it until the undo window passes; undo brings it back.
        let outbox = demo.send(&opened);
        assert!(demo.get(draft.id).is_none());
        assert!(demo.cancel_send(outbox));
        assert!(demo.get(draft.id).is_some());
        demo.discard_draft(draft.id);
        assert!(demo.get(draft.id).is_none());
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
            &[],
        )
        .unwrap();
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
            &[],
        )
        .unwrap();
        assert_eq!(messages[0].state, MessageState::Done);
    }

    #[test]
    fn demo_mail_moves_between_folders_and_back() {
        let folders = folders();
        let mut messages = demo_messages(NOW_MS);
        // The flight (Personal) is in the inbox, labelled Travel.
        let flight = messages.iter_mut().find(|m| m.id == 14).unwrap();
        assert_eq!(
            (flight.state, flight.folders.clone()),
            (MessageState::Inbox, vec![101])
        );
        let to = |folder: i64| Patch {
            folder: Some(folder),
            ..Default::default()
        };

        apply(flight, &to(102), &folders).unwrap();
        assert_eq!(flight.state, MessageState::Filed);
        assert_eq!(
            (flight.folder, flight.folders.clone()),
            (Some(102), vec![101, 102])
        );
        apply(flight, &to(104), &folders).unwrap();
        assert_eq!(
            (flight.folder, flight.folders.clone()),
            (Some(104), vec![101, 104])
        );

        // Undo sends the old values back.
        let back = Patch {
            state: Some(MessageState::Filed),
            folder: Some(102),
            ..Default::default()
        };
        apply(flight, &back, &folders).unwrap();
        assert_eq!(
            (flight.folder, flight.folders.clone()),
            (Some(102), vec![101, 102])
        );
        let inbox = Patch {
            state: Some(MessageState::Inbox),
            ..Default::default()
        };
        apply(flight, &inbox, &folders).unwrap();
        assert_eq!((flight.state, flight.folder), (MessageState::Inbox, None));
        assert_eq!(flight.folders, [101]);

        // Only folders of its own account.
        let err = apply(flight, &to(201), &folders).unwrap_err();
        assert!(err.contains("own account"), "{err}");
        assert_eq!(flight.state, MessageState::Inbox);
    }
}
