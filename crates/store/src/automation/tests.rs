use super::*;
use crate::{
    AccountKind, Added, Category, DraftSave, FolderRole, Incoming, NewAccount, RemoteFolder,
};

const DAY: i64 = 86_400_000;
const NOW: i64 = 1_800_000_000_000;

fn store() -> (Store, i64) {
    let store = Store::open_in_memory().unwrap();
    let account = store
        .add_account(&NewAccount {
            kind: AccountKind::Gmail,
            email: "me@example.com",
            name: "Me",
            label: "Personal",
            config: "{}",
        })
        .unwrap();
    (store, account.id)
}

fn incoming(account_id: i64, uid: u32, from: &str, date: i64) -> Incoming {
    Incoming {
        account_id,
        mailbox: Some("INBOX".into()),
        uid: Some(uid),
        message_id: Some(format!("{uid}@example.com")),
        from_name: "Sender".into(),
        from_address: from.into(),
        subject: format!("Message {uid}"),
        date,
        unread: true,
        category: Category::People,
        ..Default::default()
    }
}

fn add(store: &mut Store, m: &Incoming) -> i64 {
    match store.add_incoming(m, false).unwrap() {
        Added::New(id) => id,
        other => panic!("expected a new message, got {other:?}"),
    }
}

fn on(account_id: i64) -> AutomationSettings {
    AutomationSettings {
        label_accounts: [(account_id, NOW)].into(),
        draft_accounts: [(account_id, NOW)].into(),
        ..Default::default()
    }
}

fn ids(candidates: Vec<Candidate>) -> Vec<i64> {
    candidates.into_iter().map(|c| c.id).collect()
}

#[test]
fn settings_default_to_off_and_round_trip() {
    let (mut store, account) = store();
    let settings = store.automation_settings().unwrap();
    assert!(!settings.is_on());
    assert_eq!(settings.cleanup_days, Some(7));

    let mut settings = on(account);
    settings.labels_off = vec![AutoLabel::News];
    settings.model = Some("small-model".into());
    store.set_automation_settings(&settings).unwrap();
    assert_eq!(store.automation_settings().unwrap(), settings);
    assert!(settings.applies(account, AutoLabel::ToRespond));
    assert!(!settings.applies(account, AutoLabel::News));
}

#[test]
fn candidates_are_new_inbox_mail_from_others_newest_first() {
    let (mut store, account) = store();
    let old = add(&mut store, &incoming(account, 1, "a@x.com", NOW - 8 * DAY));
    let week = add(&mut store, &incoming(account, 2, "b@x.com", NOW - 6 * DAY));
    let fresh = add(&mut store, &incoming(account, 3, "c@x.com", NOW + 1000));
    let own = add(
        &mut store,
        &incoming(account, 4, "Me@Example.com", NOW + 2000),
    );
    let settings = on(account);
    let found = ids(store.automation_candidates(&settings, NOW, 10).unwrap());
    assert_eq!(found, [fresh, week]);
    assert!(!found.contains(&old) && !found.contains(&own));

    // Drafts only: just the mail since Auto-Drafts came on, less an hour.
    let drafts_only = AutomationSettings {
        draft_accounts: [(account, NOW)].into(),
        ..Default::default()
    };
    assert_eq!(
        ids(store.automation_candidates(&drafts_only, NOW, 10).unwrap()),
        [fresh]
    );
    // Off: nothing.
    assert!(
        store
            .automation_candidates(&AutomationSettings::default(), NOW, 10)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        ids(store.automation_candidates(&settings, NOW, 1).unwrap()),
        [fresh]
    );
}

#[test]
fn a_verdict_is_kept_and_its_label_applied_once() {
    let (mut store, account) = store();
    let id = add(&mut store, &incoming(account, 1, "a@x.com", NOW));
    let settings = on(account);
    let verdict = Verdict {
        label: Some(AutoLabel::ToRespond),
        needs_reply: true,
    };
    assert!(
        store
            .record_verdict(id, verdict, &settings, true, NOW)
            .unwrap()
    );
    assert_eq!(store.verdict(id).unwrap(), Some(verdict));
    assert_eq!(store.auto_label(id).unwrap(), Some(AutoLabel::ToRespond));
    assert_eq!(
        store.message(id).unwrap().unwrap().auto_label,
        Some(AutoLabel::ToRespond)
    );
    assert!(
        store
            .automation_candidates(&settings, NOW, 10)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        store.label_pushes(account).unwrap(),
        [LabelPush {
            message_id: id,
            name: "To respond".into(),
            adding: true
        }]
    );
    assert!(store.has_label_work(account).unwrap());
}

#[test]
fn a_label_turned_off_or_chosen_by_the_user_isnt_applied() {
    let (mut store, account) = store();
    let a = add(&mut store, &incoming(account, 1, "a@x.com", NOW));
    let b = add(&mut store, &incoming(account, 2, "b@x.com", NOW));
    let mut settings = on(account);
    settings.labels_off = vec![AutoLabel::Marketing];
    let marketing = Verdict {
        label: Some(AutoLabel::Marketing),
        needs_reply: false,
    };
    assert!(
        !store
            .record_verdict(a, marketing, &settings, true, NOW)
            .unwrap()
    );
    assert_eq!(store.auto_label(a).unwrap(), None);

    store
        .set_auto_label(b, Some(AutoLabel::Fyi), false)
        .unwrap();
    let respond = Verdict {
        label: Some(AutoLabel::ToRespond),
        needs_reply: true,
    };
    store
        .record_verdict(b, respond, &settings, true, NOW)
        .unwrap();
    assert_eq!(store.auto_label(b).unwrap(), Some(AutoLabel::Fyi));
    assert!(store.label_pushes(account).unwrap().is_empty());
}

#[test]
fn changing_a_label_takes_the_old_one_off_the_server() {
    let (mut store, account) = store();
    let id = add(&mut store, &incoming(account, 1, "a@x.com", NOW));
    store
        .set_auto_label(id, Some(AutoLabel::ToRespond), true)
        .unwrap();
    let added = store.label_pushes(account).unwrap();
    store.label_pushed(&added[0]).unwrap();
    assert!(!store.has_label_work(account).unwrap());

    assert!(
        store
            .set_auto_label(id, Some(AutoLabel::Fyi), true)
            .unwrap()
    );
    assert!(
        !store
            .set_auto_label(id, Some(AutoLabel::Fyi), true)
            .unwrap()
    );
    let pushes = store.label_pushes(account).unwrap();
    assert_eq!(
        pushes,
        [
            LabelPush {
                message_id: id,
                name: "To respond".into(),
                adding: false
            },
            LabelPush {
                message_id: id,
                name: "FYI".into(),
                adding: true
            },
        ]
    );
    // Taking it off before the add went up turns the add around.
    store.set_auto_label(id, None, true).unwrap();
    let pushes = store.label_pushes(account).unwrap();
    assert!(pushes.iter().all(|p| !p.adding));
    assert_eq!(store.auto_label(id).unwrap(), None);
    assert!(store.set_auto_label(999, None, true).is_err());
}

#[test]
fn a_push_changed_meanwhile_stays_queued() {
    let (mut store, account) = store();
    let id = add(&mut store, &incoming(account, 1, "a@x.com", NOW));
    store
        .set_auto_label(id, Some(AutoLabel::Fyi), true)
        .unwrap();
    let sent = store.label_pushes(account).unwrap().remove(0);
    store.set_auto_label(id, None, true).unwrap();
    store.label_pushed(&sent).unwrap();
    assert_eq!(
        store.label_pushes(account).unwrap(),
        [LabelPush {
            message_id: id,
            name: "FYI".into(),
            adding: false
        }]
    );
}

#[test]
fn turning_labels_on_applies_what_ai_said_of_the_past_week() {
    let (mut store, account) = store();
    let recent = add(&mut store, &incoming(account, 1, "a@x.com", NOW - DAY));
    let old = add(&mut store, &incoming(account, 2, "b@x.com", NOW - 30 * DAY));
    let drafts_only = AutomationSettings {
        draft_accounts: [(account, NOW - 40 * DAY)].into(),
        ..Default::default()
    };
    store.set_automation_settings(&drafts_only).unwrap();
    let news = Verdict {
        label: Some(AutoLabel::News),
        needs_reply: false,
    };
    for id in [recent, old] {
        store
            .record_verdict(id, news, &drafts_only, true, NOW)
            .unwrap();
        assert_eq!(store.auto_label(id).unwrap(), None);
    }

    let mut both = drafts_only.clone();
    both.label_accounts.insert(account, NOW);
    store.set_automation_settings(&both).unwrap();
    assert_eq!(store.auto_label(recent).unwrap(), Some(AutoLabel::News));
    assert_eq!(store.auto_label(old).unwrap(), None);
    assert_eq!(store.label_pushes(account).unwrap().len(), 1);
}

#[test]
fn failed_looks_are_retried_a_few_times() {
    let (mut store, account) = store();
    let id = add(&mut store, &incoming(account, 1, "a@x.com", NOW));
    let settings = on(account);
    store.record_check_failed(id, NOW).unwrap();
    assert!(
        store
            .automation_candidates(&settings, NOW, 10)
            .unwrap()
            .is_empty()
    );
    let later = NOW + 6 * 60_000;
    assert_eq!(
        ids(store.automation_candidates(&settings, later, 10).unwrap()),
        [id]
    );
    store.record_check_failed(id, later).unwrap();
    store.record_check_failed(id, later + DAY).unwrap();
    // Given up on: never again.
    assert!(
        store
            .automation_candidates(&settings, later + 10 * DAY, 10)
            .unwrap()
            .is_empty()
    );
    assert_eq!(store.verdict(id).unwrap().map(|v| v.label), Some(None));
}

#[test]
fn a_reply_sent_answers_the_conversation_before_it_syncs() {
    let (mut store, account) = store();
    let settings = on(account);
    let first = add(&mut store, &incoming(account, 1, "a@x.com", NOW - 120_000));
    let mut follow_up = incoming(account, 2, "a@x.com", NOW - 60_000);
    follow_up.subject = "Re: Message 1".into();
    follow_up.in_reply_to = Some("<1@example.com>".into());
    let follow_up = add(&mut store, &follow_up);
    let conversation = |id| store.message(id).unwrap().unwrap().conversation;
    assert_eq!(conversation(first), conversation(follow_up));
    let respond = Verdict {
        label: Some(AutoLabel::ToRespond),
        needs_reply: true,
    };
    store
        .record_verdict(first, respond, &settings, true, NOW)
        .unwrap();
    assert_eq!(store.draft_candidates(&settings, NOW, 10).unwrap(), [first]);
    assert_eq!(
        ids(store.automation_candidates(&settings, NOW, 10).unwrap()),
        [follow_up]
    );
    // The user's reply went out; Sent hasn't synced, so it isn't here.
    let mut changed = store.user_replied(follow_up, &settings, true, NOW).unwrap();
    changed.sort();
    assert_eq!(changed, [first, follow_up]);
    for id in [first, follow_up] {
        assert_eq!(store.auto_label(id).unwrap(), Some(AutoLabel::Actioned));
    }
    assert!(
        store
            .automation_candidates(&settings, NOW, 10)
            .unwrap()
            .is_empty()
    );
    assert!(
        store
            .draft_candidates(&settings, NOW, 10)
            .unwrap()
            .is_empty()
    );
    assert!(!store.reply_due(first).unwrap());
    // What AI said of the follow-up, asked before the reply went, changes nothing.
    assert!(
        !store
            .record_verdict(follow_up, respond, &settings, true, NOW)
            .unwrap()
    );
    assert_eq!(
        store.auto_label(follow_up).unwrap(),
        Some(AutoLabel::Actioned)
    );
    assert!(
        store
            .draft_candidates(&settings, NOW, 10)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn failures_coming_back_after_a_reply_dont_undo_it() {
    let (mut store, account) = store();
    let settings = on(account);
    let respond = Verdict {
        label: Some(AutoLabel::ToRespond),
        needs_reply: true,
    };
    // AI was sorting one, and writing a reply to another, when the user's
    // replies went out; both then failed.
    let sorting = add(&mut store, &incoming(account, 1, "a@x.com", NOW - 60_000));
    let writing = add(&mut store, &incoming(account, 2, "b@x.com", NOW - 60_000));
    store
        .record_verdict(writing, respond, &settings, true, NOW)
        .unwrap();
    store.user_replied(sorting, &settings, true, NOW).unwrap();
    store.user_replied(writing, &settings, true, NOW).unwrap();
    store.record_check_failed(sorting, NOW).unwrap();
    store.draft_failed(writing, NOW).unwrap();
    let later = NOW + DAY;
    assert!(
        store
            .automation_candidates(&settings, later, 10)
            .unwrap()
            .is_empty()
    );
    assert!(
        store
            .draft_candidates(&settings, later, 10)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn turning_drafts_on_leaves_mail_already_answered() {
    let (mut store, account) = store();
    let labels_only = AutomationSettings {
        label_accounts: [(account, NOW)].into(),
        ..Default::default()
    };
    let respond = Verdict {
        label: Some(AutoLabel::ToRespond),
        needs_reply: true,
    };
    let id = add(&mut store, &incoming(account, 1, "a@x.com", NOW - 60_000));
    store.set_automation_settings(&labels_only).unwrap();
    store
        .record_verdict(id, respond, &labels_only, true, NOW)
        .unwrap();
    // Answered before Sent synced, then Auto-Drafts turned on within the hour.
    store.user_replied(id, &labels_only, true, NOW).unwrap();
    let both = on(account);
    store.set_automation_settings(&both).unwrap();
    assert!(store.draft_candidates(&both, NOW, 10).unwrap().is_empty());
}

#[test]
fn turning_labels_on_after_a_reply_doesnt_say_to_respond() {
    let (mut store, account) = store();
    let drafts_only = AutomationSettings {
        draft_accounts: [(account, NOW)].into(),
        ..Default::default()
    };
    let respond = Verdict {
        label: Some(AutoLabel::ToRespond),
        needs_reply: true,
    };
    let id = add(&mut store, &incoming(account, 1, "a@x.com", NOW - 60_000));
    store.set_automation_settings(&drafts_only).unwrap();
    store
        .record_verdict(id, respond, &drafts_only, true, NOW)
        .unwrap();
    // Answered before Sent synced, then Auto-Labels turned on.
    store.user_replied(id, &drafts_only, true, NOW).unwrap();
    store.set_automation_settings(&on(account)).unwrap();
    assert_eq!(store.auto_label(id).unwrap(), Some(AutoLabel::Actioned));
}

#[test]
fn a_reply_sent_with_actioned_off_leaves_no_label() {
    let (mut store, account) = store();
    let id = add(&mut store, &incoming(account, 1, "a@x.com", NOW - 60_000));
    let mut settings = on(account);
    // Inbox automation off for the account: nothing changes.
    let off = AutomationSettings::default();
    assert!(store.user_replied(id, &off, true, NOW).unwrap().is_empty());
    assert_eq!(
        ids(store.automation_candidates(&settings, NOW, 10).unwrap()),
        [id]
    );
    settings.labels_off = vec![AutoLabel::Actioned];
    assert!(
        store
            .user_replied(id, &settings, true, NOW)
            .unwrap()
            .is_empty()
    );
    assert_eq!(store.auto_label(id).unwrap(), None);
    assert!(
        store
            .automation_candidates(&settings, NOW, 10)
            .unwrap()
            .is_empty()
    );
}

fn with_drafts_folder(store: &mut Store, account: i64) {
    store
        .replace_folders(
            account,
            &[RemoteFolder {
                remote_id: "[Gmail]/Drafts".into(),
                parent: None,
                name: "Drafts".into(),
                role: Some(FolderRole::Drafts),
                selectable: true,
            }],
        )
        .unwrap();
}

/// Saves draft `id` again with `text`, as the composer would.
fn resave(store: &mut Store, account: i64, id: i64, text: &str) {
    store
        .save_draft(
            Some(id),
            &DraftSave {
                account_id: account,
                draft: "{}",
                send_later: None,
                from_name: "Me",
                from_address: "me@example.com",
                to: &["a@x.com".to_owned()],
                cc: &[],
                bcc: &[],
                subject: "Re: Message 1",
                snippet: text,
                text,
                html: None,
                in_reply_to: Some("1@example.com"),
                references: None,
                attachments: &[],
                files: &[],
            },
            NOW,
        )
        .unwrap();
}

fn save_reply(store: &mut Store, account: i64, in_reply_to: &str, text: &str) -> i64 {
    store
        .save_draft(
            None,
            &DraftSave {
                account_id: account,
                draft: "{}",
                send_later: None,
                from_name: "Me",
                from_address: "me@example.com",
                to: &["a@x.com".to_owned()],
                cc: &[],
                bcc: &[],
                subject: "Re: Message 1",
                snippet: text,
                text,
                html: None,
                in_reply_to: Some(in_reply_to),
                references: None,
                attachments: &[],
                files: &[],
            },
            NOW,
        )
        .unwrap()
}

#[test]
fn auto_drafts_show_on_the_email_they_reply_to() {
    let (mut store, account) = store();
    with_drafts_folder(&mut store, account);
    let id = add(&mut store, &incoming(account, 1, "a@x.com", NOW));
    assert!(!store.conversation_has_draft(id).unwrap());
    let options = vec![
        DraftOption {
            title: "Accept".into(),
            body: "Tuesday works.\n\nThanks".into(),
        },
        DraftOption {
            title: "Decline".into(),
            body: "I can't make it.".into(),
        },
    ];
    let draft = save_reply(
        &mut store,
        account,
        "1@example.com",
        "Tuesday works.\r\n\r\nThanks",
    );
    store.add_auto_draft(draft, id, &options, 0, NOW).unwrap();
    assert_eq!(store.message(id).unwrap().unwrap().auto_draft, Some(draft));
    assert!(store.conversation_has_draft(id).unwrap());
    let found = store.auto_draft(draft).unwrap().unwrap();
    assert_eq!(found.options, options);
    assert_eq!(store.auto_draft_for(id).unwrap(), Some(found));
    // The draft isn't something to look at itself.
    assert!(!ids(store.automation_candidates(&on(account), NOW, 10).unwrap()).contains(&draft));

    // Untouched: cleaned up after the period. Edited: kept.
    assert_eq!(store.untouched_auto_drafts(NOW + 1).unwrap(), [draft]);
    assert!(store.untouched_auto_drafts(NOW).unwrap().is_empty());
    // Another option picked: saved saying it, and still untouched.
    resave(&mut store, account, draft, "I can't make it.");
    assert!(store.auto_draft_chosen(draft, 1).unwrap());
    assert_eq!(store.auto_draft(draft).unwrap().unwrap().chosen, 1);
    assert_eq!(store.untouched_auto_drafts(NOW + 1).unwrap(), [draft]);
    resave(&mut store, account, draft, "I can't make it, sorry.");
    assert!(store.untouched_auto_drafts(NOW + 1).unwrap().is_empty());
    assert!(store.auto_draft_chosen(draft, 5).is_err());

    // Gone with the draft.
    store.discard_draft(draft).unwrap();
    assert_eq!(store.message(id).unwrap().unwrap().auto_draft, None);
}

#[test]
fn an_auto_draft_deleted_in_another_app_answers_nothing() {
    let (mut store, account) = store();
    with_drafts_folder(&mut store, account);
    let id = add(&mut store, &incoming(account, 1, "a@x.com", NOW));
    let draft = save_reply(&mut store, account, "1@example.com", "Sure.");
    let option = [DraftOption {
        title: String::new(),
        body: "Sure.".into(),
    }];
    store.add_auto_draft(draft, id, &option, 0, NOW).unwrap();
    let mut later = incoming(account, 2, "a@x.com", NOW + 1000);
    later.subject = "Re: Message 1".into();
    later.in_reply_to = Some("<1@example.com>".into());
    let later = add(&mut store, &later);
    assert!(store.conversation_has_draft(later).unwrap());
    // Uploaded, then out of Drafts here after a sync, its row left behind.
    store
        .conn
        .execute_batch(&format!(
            "DELETE FROM draft_uploads WHERE message_id = {draft};
             DELETE FROM folder_members WHERE message_id = {draft};"
        ))
        .unwrap();
    assert!(store.message(draft).unwrap().is_some());
    // Drafts holds more than its listing here: it may only be past it.
    let drafts = store
        .folders()
        .unwrap()
        .into_iter()
        .find(|f| f.role == Some(FolderRole::Drafts))
        .unwrap()
        .id;
    assert!(store.conversation_has_draft(later).unwrap());
    store
        .set_folder_synced(drafts, r#"{"limit":200,"complete":false}"#, NOW)
        .unwrap();
    assert!(store.conversation_has_draft(later).unwrap());
    assert_eq!(store.message(id).unwrap().unwrap().auto_draft, Some(draft));
    // Drafts was listed whole: it was deleted, in Outlook say. Forgotten as
    // the sync is recorded, the email it replied to shows none (the message
    // list hears) and can get another.
    let mark = store.messages_marked().unwrap().1.unwrap();
    store
        .set_folder_synced(drafts, r#"{"limit":200,"complete":true}"#, NOW)
        .unwrap();
    assert!(!store.conversation_has_draft(later).unwrap());
    assert_eq!(store.message(id).unwrap().unwrap().auto_draft, None);
    assert_eq!(store.auto_draft_for(id).unwrap(), None);
    let changes = store.message_changes(&mark).unwrap().unwrap();
    assert!(changes.messages.iter().any(|m| m.id == id));
}

#[test]
fn a_draft_deleted_in_another_app_wasnt_a_reply() {
    let (mut store, account) = store();
    with_drafts_folder(&mut store, account);
    let drafts_only = AutomationSettings {
        draft_accounts: [(account, NOW)].into(),
        ..Default::default()
    };
    let respond = Verdict {
        label: Some(AutoLabel::ToRespond),
        needs_reply: true,
    };
    let id = add(&mut store, &incoming(account, 1, "a@x.com", NOW - 60_000));
    store.set_automation_settings(&drafts_only).unwrap();
    store
        .record_verdict(id, respond, &drafts_only, true, NOW)
        .unwrap();
    let draft = save_reply(&mut store, account, "1@example.com", "Sure.");
    let option = [DraftOption {
        title: String::new(),
        body: "Sure.".into(),
    }];
    store.add_auto_draft(draft, id, &option, 0, NOW).unwrap();
    assert!(!store.answered_after(id).unwrap());
    // Deleted in Outlook: out of Drafts here once listed whole, its row left
    // behind, unsent.
    store
        .conn
        .execute_batch(&format!(
            "DELETE FROM draft_uploads WHERE message_id = {draft};
             DELETE FROM folder_members WHERE message_id = {draft};"
        ))
        .unwrap();
    let drafts = store
        .folders()
        .unwrap()
        .into_iter()
        .find(|f| f.role == Some(FolderRole::Drafts))
        .unwrap()
        .id;
    store
        .set_folder_synced(drafts, r#"{"complete":true}"#, NOW)
        .unwrap();
    assert!(store.message(draft).unwrap().is_some());
    assert!(!store.answered_after(id).unwrap());
    // Auto-Labels turned on: AI's To respond stands.
    store.set_automation_settings(&on(account)).unwrap();
    assert_eq!(store.auto_label(id).unwrap(), Some(AutoLabel::ToRespond));
}

#[test]
fn a_draft_in_trash_wasnt_a_reply_but_one_in_sent_was() {
    let (mut store, account) = store();
    let folder = |name: &str, role| RemoteFolder {
        remote_id: name.into(),
        parent: None,
        name: name.into(),
        role: Some(role),
        selectable: true,
    };
    store
        .replace_folders(
            account,
            &[
                folder("Drafts", FolderRole::Drafts),
                folder("Trash", FolderRole::Trash),
                folder("Sent", FolderRole::Sent),
                folder("All Mail", FolderRole::Archive),
            ],
        )
        .unwrap();
    let role = |store: &Store, role| {
        store
            .folders()
            .unwrap()
            .into_iter()
            .find(|f| f.role == Some(role))
            .unwrap()
            .id
    };
    let (trash, sent) = (
        role(&store, FolderRole::Trash),
        role(&store, FolderRole::Sent),
    );
    let all_mail = role(&store, FolderRole::Archive);
    let id = add(&mut store, &incoming(account, 1, "a@x.com", NOW - 60_000));
    let draft = save_reply(&mut store, account, "1@example.com", "Sure.");
    // Out of Drafts and into Trash: Deleted Items (Microsoft) leaves it
    // deleted; an IMAP Trash, filed there.
    let moved = |store: &Store, sql: &str| {
        store
            .conn
            .execute_batch(&format!(
                "DELETE FROM draft_uploads WHERE message_id = {draft};
                 DELETE FROM folder_members WHERE message_id = {draft};
                 {sql}"
            ))
            .unwrap();
    };
    moved(
        &store,
        &format!("UPDATE messages SET state = 'deleted' WHERE id = {draft};"),
    );
    assert!(!store.answered_after(id).unwrap());
    moved(
        &store,
        &format!(
            "UPDATE messages SET state = 'filed' WHERE id = {draft};
             INSERT INTO folder_members (folder_id, message_id) VALUES ({trash}, {draft});"
        ),
    );
    assert!(!store.answered_after(id).unwrap());
    // Gmail's All Mail lists drafts too, and may not have been synced again
    // since: Trash still wins.
    moved(
        &store,
        &format!(
            "INSERT INTO folder_members (folder_id, message_id) VALUES ({trash}, {draft});
             INSERT INTO folder_members (folder_id, message_id) VALUES ({all_mail}, {draft});"
        ),
    );
    assert!(!store.answered_after(id).unwrap());
    // Sent, as a listing of Sent finds it: a reply.
    moved(
        &store,
        &format!("INSERT INTO folder_members (folder_id, message_id) VALUES ({sent}, {draft});"),
    );
    assert!(store.answered_after(id).unwrap());
}

#[test]
fn a_users_own_reply_draft_counts_too() {
    let (mut store, account) = store();
    with_drafts_folder(&mut store, account);
    let id = add(&mut store, &incoming(account, 1, "a@x.com", NOW));
    let other = add(&mut store, &incoming(account, 2, "b@x.com", NOW));
    save_reply(&mut store, account, "1@example.com", "On it");
    assert!(store.conversation_has_draft(id).unwrap());
    assert!(!store.conversation_has_draft(other).unwrap());
}

#[test]
fn mail_the_user_answered_later_needs_no_draft() {
    let (mut store, account) = store();
    let id = add(&mut store, &incoming(account, 1, "a@x.com", NOW));
    assert!(!store.answered_after(id).unwrap());
    assert!(store.conversation_before(id, 5).unwrap().is_empty());
    let mut reply = incoming(account, 2, "me@example.com", NOW + 1000);
    reply.subject = "Re: Message 1".into();
    reply.in_reply_to = Some("<1@example.com>".into());
    let reply = add(&mut store, &reply);
    assert!(store.answered_after(id).unwrap());
    assert_eq!(store.conversation_before(reply, 5).unwrap(), [id]);
}

#[test]
fn label_changes_reach_the_message_list() {
    let (mut store, account) = store();
    let id = add(&mut store, &incoming(account, 1, "a@x.com", NOW));
    let mark = store.messages_marked().unwrap().1.unwrap();
    store
        .set_auto_label(id, Some(AutoLabel::Comment), false)
        .unwrap();
    let changes = store.message_changes(&mark).unwrap().unwrap();
    assert_eq!(changes.messages.len(), 1);
    assert_eq!(changes.messages[0].auto_label, Some(AutoLabel::Comment));
}

#[test]
fn excluded_senders_and_instructions() {
    let (store, account) = store();
    store.set_auto_draft_excluded("Pete@x.com", true).unwrap();
    store.set_auto_draft_excluded("pete@x.com", true).unwrap();
    assert!(store.is_auto_draft_excluded("PETE@x.com").unwrap());
    assert_eq!(store.auto_draft_excluded().unwrap(), ["Pete@x.com"]);
    store.set_auto_draft_excluded("pete@x.com", false).unwrap();
    assert!(store.auto_draft_excluded().unwrap().is_empty());
    assert!(store.set_auto_draft_excluded("pete", true).is_err());

    let a = store
        .add_draft_instruction(" Never use an em dash ", None)
        .unwrap();
    let b = store
        .add_draft_instruction("Answer in German", Some(account))
        .unwrap();
    assert!(store.add_draft_instruction("  ", None).is_err());
    let list = store.draft_instructions().unwrap();
    assert_eq!(list, [a.clone(), b]);
    assert_eq!(list[0].text, "Never use an em dash");
    store.remove_draft_instruction(a.id).unwrap();
    assert_eq!(store.draft_instructions().unwrap().len(), 1);
}

#[test]
fn a_removed_account_leaves_the_settings() {
    let (mut store, account) = store();
    store.set_automation_settings(&on(account)).unwrap();
    store.remove_account(account).unwrap();
    let settings = store.automation_settings().unwrap();
    assert!(!settings.is_on());
    // A new account given the same id starts with it off.
    let again = store
        .add_account(&NewAccount {
            kind: AccountKind::Imap,
            email: "new@example.com",
            name: "New",
            label: "New",
            config: "{}",
        })
        .unwrap();
    assert!(!store.automation_settings().unwrap().labels(again.id));
}

#[test]
fn cleanup_keeps_a_draft_the_user_added_to() {
    let (mut store, account) = store();
    with_drafts_folder(&mut store, account);
    let id = add(&mut store, &incoming(account, 1, "a@x.com", NOW));
    let options = vec![DraftOption {
        title: String::new(),
        body: "Tuesday works.".into(),
    }];
    let draft = save_reply(
        &mut store,
        account,
        "1@example.com",
        "Tuesday works.\n\n-- \nMe",
    );
    store.add_auto_draft(draft, id, &options, 0, NOW).unwrap();
    assert_eq!(store.untouched_auto_drafts(NOW + 1).unwrap(), [draft]);
    // Added to, the start still matches: it's edited all the same.
    resave(
        &mut store,
        account,
        draft,
        "Tuesday works. Please bring the signed contract.\n\n-- \nMe",
    );
    assert!(store.untouched_auto_drafts(NOW + 1).unwrap().is_empty());
}

#[test]
fn a_label_the_user_took_off_stays_off() {
    let (mut store, account) = store();
    let id = add(&mut store, &incoming(account, 1, "a@x.com", NOW - DAY));
    let settings = on(account);
    store.set_automation_settings(&settings).unwrap();
    let news = Verdict {
        label: Some(AutoLabel::News),
        needs_reply: false,
    };
    store
        .record_verdict(id, news, &settings, true, NOW)
        .unwrap();
    assert!(store.set_auto_label(id, None, true).unwrap());
    assert_eq!(store.auto_label(id).unwrap(), None);
    // Labels off and on again: the past week is labeled, but not this one.
    store
        .set_automation_settings(&AutomationSettings::default())
        .unwrap();
    store.set_automation_settings(&settings).unwrap();
    assert_eq!(store.auto_label(id).unwrap(), None);
    // A verdict recorded again doesn't bring it back either.
    store
        .record_verdict(id, news, &settings, true, NOW)
        .unwrap();
    assert_eq!(store.auto_label(id).unwrap(), None);
    assert!(
        store
            .label_pushes(account)
            .unwrap()
            .iter()
            .all(|p| !p.adding)
    );
    // Still the user's to give one.
    assert!(
        store
            .set_auto_label(id, Some(AutoLabel::Fyi), true)
            .unwrap()
    );
    assert_eq!(store.auto_label(id).unwrap(), Some(AutoLabel::Fyi));
}

#[test]
fn turning_labels_on_leaves_deleted_mail_alone() {
    let (mut store, account) = store();
    let id = add(&mut store, &incoming(account, 1, "a@x.com", NOW - DAY));
    let drafts_only = AutomationSettings {
        draft_accounts: [(account, NOW)].into(),
        ..Default::default()
    };
    store.set_automation_settings(&drafts_only).unwrap();
    let news = Verdict {
        label: Some(AutoLabel::News),
        needs_reply: false,
    };
    store
        .record_verdict(id, news, &drafts_only, true, NOW)
        .unwrap();
    store
        .update(
            id,
            &crate::Patch {
                state: Some(crate::MessageState::Deleted),
                ..Default::default()
            },
            NOW,
        )
        .unwrap();
    store.set_automation_settings(&on(account)).unwrap();
    assert_eq!(store.auto_label(id).unwrap(), None);
    assert!(!store.has_label_work(account).unwrap());
}

#[test]
fn the_label_queue_is_read_from_the_queue_not_the_mailbox() {
    let (store, account) = store();
    for query in [LABEL_WORK, LABEL_PUSHES] {
        let plan: Vec<String> = store
            .conn
            .prepare(&format!("EXPLAIN QUERY PLAN {query}"))
            .unwrap()
            .query_map([account], |r| r.get(3))
            .unwrap()
            .collect::<Result<_>>()
            .unwrap();
        let table = plan
            .iter()
            .find(|p| {
                ["SCAN p", "SEARCH p", "SCAN m", "SEARCH m"]
                    .iter()
                    .any(|t| p.starts_with(t))
            })
            .unwrap();
        assert!(
            table.starts_with("SCAN p"),
            "starts from the queue: {plan:?}"
        );
    }
    // Looking for new mail reads the recent mail by date, not the account.
    let plan: Vec<String> = store
        .conn
        .prepare(&format!("EXPLAIN QUERY PLAN {CANDIDATES}"))
        .unwrap()
        .query_map(params![account, NOW, NOW, 8], |r| r.get(3))
        .unwrap()
        .collect::<Result<_>>()
        .unwrap();
    let messages = plan.iter().find(|p| p.contains(" m ")).unwrap();
    assert!(messages.contains("messages_date"), "{plan:?}");
}

#[test]
fn replies_due_wait_until_written_or_given_up() {
    let (mut store, account) = store();
    let settings = on(account);
    let respond = Verdict {
        label: Some(AutoLabel::ToRespond),
        needs_reply: true,
    };
    let mut due = Vec::new();
    for uid in 1..=5 {
        let id = add(
            &mut store,
            &incoming(account, uid, "a@x.com", NOW + uid as i64),
        );
        store
            .record_verdict(id, respond, &settings, true, NOW)
            .unwrap();
        due.push(id);
    }
    let fyi = add(&mut store, &incoming(account, 9, "b@x.com", NOW));
    let no_reply = Verdict {
        label: Some(AutoLabel::Fyi),
        needs_reply: false,
    };
    store
        .record_verdict(fyi, no_reply, &settings, true, NOW)
        .unwrap();
    // All five wait, newest first, however many a round takes.
    assert_eq!(
        store.draft_candidates(&settings, NOW, 3).unwrap(),
        [due[4], due[3], due[2]]
    );
    assert_eq!(store.draft_candidates(&settings, NOW, 10).unwrap().len(), 5);

    store.draft_settled(due[4]).unwrap();
    assert!(
        !store
            .draft_candidates(&settings, NOW, 10)
            .unwrap()
            .contains(&due[4])
    );
    // A failure waits, then is tried again, a few times at most.
    store.draft_failed(due[3], NOW).unwrap();
    assert!(
        !store
            .draft_candidates(&settings, NOW, 10)
            .unwrap()
            .contains(&due[3])
    );
    assert!(
        store
            .draft_candidates(&settings, NOW + 6 * 60_000, 10)
            .unwrap()
            .contains(&due[3])
    );
    store.draft_failed(due[3], NOW).unwrap();
    store.draft_failed(due[3], NOW).unwrap();
    assert!(
        !store
            .draft_candidates(&settings, NOW + 10 * DAY, 10)
            .unwrap()
            .contains(&due[3])
    );

    // Auto-Drafts off: nothing is due, and none is queued for new verdicts.
    let labels_only = AutomationSettings {
        label_accounts: [(account, NOW)].into(),
        ..Default::default()
    };
    assert!(
        store
            .draft_candidates(&labels_only, NOW, 10)
            .unwrap()
            .is_empty()
    );
    let later = add(&mut store, &incoming(account, 20, "c@x.com", NOW + 100));
    store
        .record_verdict(later, respond, &labels_only, true, NOW)
        .unwrap();
    assert!(
        !store
            .draft_candidates(&settings, NOW, 10)
            .unwrap()
            .contains(&later)
    );
}

#[test]
fn a_reply_for_a_new_sender_waits_until_they_are_accepted() {
    let (mut store, account) = store();
    let settings = on(account);
    let mut screened = incoming(account, 1, "new@x.com", NOW);
    screened.message_id = Some("1@example.com".into());
    let id = match store.add_incoming(&screened, true).unwrap() {
        crate::Added::New(id) => id,
        other => panic!("{other:?}"),
    };
    assert!(store.message(id).unwrap().unwrap().screening);
    let respond = Verdict {
        label: Some(AutoLabel::ToRespond),
        needs_reply: true,
    };
    store
        .record_verdict(id, respond, &settings, true, NOW)
        .unwrap();
    // Waiting in the Gatekeeper: no reply yet, but it isn't forgotten.
    assert!(
        store
            .draft_candidates(&settings, NOW, 10)
            .unwrap()
            .is_empty()
    );
    let mut for_new_senders = settings.clone();
    for_new_senders.gatekeeper_drafts = true;
    assert_eq!(
        store.draft_candidates(&for_new_senders, NOW, 10).unwrap(),
        [id]
    );
    store.accept_sender("new@x.com", NOW).unwrap();
    assert_eq!(store.draft_candidates(&settings, NOW, 10).unwrap(), [id]);
}

#[test]
fn an_option_can_be_picked_only_while_the_draft_is_untouched() {
    let (mut store, account) = store();
    with_drafts_folder(&mut store, account);
    let id = add(&mut store, &incoming(account, 1, "a@x.com", NOW));
    let options = vec![
        DraftOption {
            title: "Yes".into(),
            body: "Yes.".into(),
        },
        DraftOption {
            title: "No".into(),
            body: "No.".into(),
        },
    ];
    let draft = save_reply(&mut store, account, "1@example.com", "Yes.");
    assert!(
        !store.auto_draft_untouched(draft).unwrap(),
        "not an auto-draft yet"
    );
    store.add_auto_draft(draft, id, &options, 0, NOW).unwrap();
    assert!(store.auto_draft_untouched(draft).unwrap());
    resave(&mut store, account, draft, "Yes, see you there.");
    assert!(!store.auto_draft_untouched(draft).unwrap());
}

#[test]
fn a_draft_changed_in_another_app_is_kept() {
    let (mut store, account) = store();
    with_drafts_folder(&mut store, account);
    let id = add(&mut store, &incoming(account, 1, "a@x.com", NOW));
    let options = vec![DraftOption {
        title: String::new(),
        body: "Yes.".into(),
    }];
    let draft = save_reply(&mut store, account, "1@example.com", "Yes.");
    store.add_auto_draft(draft, id, &options, 0, NOW).unwrap();
    assert_eq!(store.untouched_auto_drafts(NOW + 1).unwrap(), [draft]);
    store.auto_draft_edited(draft).unwrap();
    assert!(store.untouched_auto_drafts(NOW + 1).unwrap().is_empty());
    assert!(!store.auto_draft_untouched(draft).unwrap());
}

#[test]
fn version_23_databases_gain_the_reply_queue() {
    let dir = std::env::temp_dir().join(format!("spark-store-v23-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("v23.sqlite");
    let _ = std::fs::remove_file(&path);
    let (account, id) = {
        let mut store = Store::open(&path).unwrap();
        let account = store
            .add_account(&NewAccount {
                kind: AccountKind::Imap,
                email: "me@example.com",
                name: "Me",
                label: "Me",
                config: "{}",
            })
            .unwrap()
            .id;
        let id = add(&mut store, &incoming(account, 1, "a@x.com", NOW));
        store
            .conn
            .execute_batch(
                "DROP INDEX automation_draft_due;
                 ALTER TABLE automation_checked DROP COLUMN draft_failures;
                 ALTER TABLE automation_checked DROP COLUMN draft_due;
                 PRAGMA user_version = 23;",
            )
            .unwrap();
        store
            .conn
            .execute(
                "INSERT INTO automation_checked (message_id, checked_at, needs_reply) VALUES (?1, ?2, 1)",
                params![id, NOW],
            )
            .unwrap();
        (account, id)
    };
    let mut store = Store::open(&path).unwrap();
    let settings = on(account);
    // Checked before: no reply due, as before the queue.
    assert!(
        store
            .draft_candidates(&settings, NOW, 10)
            .unwrap()
            .is_empty()
    );
    // Mail looked at now queues its reply.
    let new = add(&mut store, &incoming(account, 2, "b@x.com", NOW));
    let respond = Verdict {
        label: None,
        needs_reply: true,
    };
    store
        .record_verdict(new, respond, &settings, false, NOW)
        .unwrap();
    assert_eq!(store.draft_candidates(&settings, NOW, 10).unwrap(), [new]);
    drop(store);
    // Opening it again changes nothing.
    assert_eq!(
        Store::open(&path)
            .unwrap()
            .draft_candidates(&settings, NOW, 10)
            .unwrap(),
        [new]
    );
    assert_ne!(new, id);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn cleanup_keeps_a_draft_given_a_file_or_other_recipients() {
    let (mut store, account) = store();
    with_drafts_folder(&mut store, account);
    let id = add(&mut store, &incoming(account, 1, "a@x.com", NOW));
    let options = vec![DraftOption {
        title: String::new(),
        body: "Yes.".into(),
    }];
    let save = |store: &mut Store, existing: Option<i64>, to: &str, files: &[crate::Attachment]| {
        store
            .save_draft(
                existing,
                &DraftSave {
                    account_id: account,
                    draft: "{}",
                    send_later: None,
                    from_name: "Me",
                    from_address: "me@example.com",
                    to: &[to.to_owned()],
                    cc: &[],
                    bcc: &[],
                    subject: "Re: Message 1",
                    snippet: "Yes.",
                    text: "Yes.",
                    html: None,
                    in_reply_to: Some("1@example.com"),
                    references: None,
                    attachments: files,
                    files: &[],
                },
                NOW,
            )
            .unwrap()
    };
    let draft = save(&mut store, None, "a@x.com", &[]);
    store.add_auto_draft(draft, id, &options, 0, NOW).unwrap();
    assert_eq!(store.untouched_auto_drafts(NOW + 1).unwrap(), [draft]);
    // The same text, with Bob added.
    save(&mut store, Some(draft), "a@x.com, bob@x.com", &[]);
    assert!(store.untouched_auto_drafts(NOW + 1).unwrap().is_empty());
    assert!(!store.auto_draft_untouched(draft).unwrap());

    let other = add(&mut store, &incoming(account, 2, "c@x.com", NOW));
    let draft = save(&mut store, None, "c@x.com", &[]);
    store
        .add_auto_draft(draft, other, &options, 0, NOW)
        .unwrap();
    // The same text, with a file.
    let file = crate::Attachment {
        name: "plan.pdf".into(),
        mime: "application/pdf".into(),
        size: 3,
        remote_id: None,
    };
    save(&mut store, Some(draft), "c@x.com", &[file]);
    assert!(store.untouched_auto_drafts(NOW + 1).unwrap().is_empty());
}

#[test]
fn turning_drafts_on_writes_replies_ai_just_said_were_needed() {
    let (mut store, account) = store();
    let labels_only = AutomationSettings {
        label_accounts: [(account, NOW - DAY)].into(),
        ..Default::default()
    };
    store.set_automation_settings(&labels_only).unwrap();
    let recent = add(
        &mut store,
        &incoming(account, 1, "a@x.com", NOW - 10 * 60_000),
    );
    let old = add(&mut store, &incoming(account, 2, "b@x.com", NOW - DAY));
    let respond = Verdict {
        label: Some(AutoLabel::ToRespond),
        needs_reply: true,
    };
    for id in [recent, old] {
        store
            .record_verdict(id, respond, &labels_only, true, NOW)
            .unwrap();
    }
    let mut both = labels_only.clone();
    both.draft_accounts.insert(account, NOW);
    assert!(store.draft_candidates(&both, NOW, 10).unwrap().is_empty());
    store.set_automation_settings(&both).unwrap();
    // Within the hour before: due. Older: no.
    assert_eq!(store.draft_candidates(&both, NOW, 10).unwrap(), [recent]);
}

#[test]
fn version_24_databases_gain_server_versions() {
    let dir = std::env::temp_dir().join(format!("spark-store-v24-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("v24.sqlite");
    let _ = std::fs::remove_file(&path);
    Store::open(&path)
        .unwrap()
        .conn
        .execute_batch(
            "ALTER TABLE auto_drafts DROP COLUMN server_version;
             PRAGMA user_version = 24;",
        )
        .unwrap();
    let (mut store, account) = {
        let store = Store::open(&path).unwrap();
        let account = store
            .add_account(&NewAccount {
                kind: AccountKind::Microsoft,
                email: "me@contoso.com",
                name: "Me",
                label: "Work",
                config: "{}",
            })
            .unwrap()
            .id;
        (store, account)
    };
    with_drafts_folder(&mut store, account);
    let id = add(&mut store, &incoming(account, 1, "a@x.com", NOW));
    let draft = save_reply(&mut store, account, "1@example.com", "Yes.");
    let options = [DraftOption {
        title: String::new(),
        body: "Yes.".into(),
    }];
    store.add_auto_draft(draft, id, &options, 0, NOW).unwrap();
    assert_eq!(store.auto_draft_server_version(draft).unwrap(), None);
    store.auto_draft_uploaded(draft, Some("v1")).unwrap();
    assert_eq!(
        store.auto_draft_server_version(draft).unwrap().as_deref(),
        Some("v1")
    );
    drop(store);
    // Opening it again changes nothing.
    let store = Store::open(&path).unwrap();
    assert_eq!(
        store.auto_draft_server_version(draft).unwrap().as_deref(),
        Some("v1")
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn mail_answered_before_ai_looked_is_actioned_not_to_respond() {
    let (mut store, account) = store();
    let id = add(&mut store, &incoming(account, 1, "a@x.com", NOW - DAY));
    let mut reply = incoming(account, 2, "me@example.com", NOW - DAY + 1000);
    reply.subject = "Re: Message 1".into();
    reply.in_reply_to = Some("<1@example.com>".into());
    add(&mut store, &reply);
    let respond = Verdict {
        label: Some(AutoLabel::ToRespond),
        needs_reply: true,
    };
    let settings = on(account);
    store
        .record_verdict(id, respond, &settings, true, NOW)
        .unwrap();
    assert_eq!(store.auto_label(id).unwrap(), Some(AutoLabel::Actioned));
    // Actioned turned off: no label rather than To respond.
    let other = add(&mut store, &incoming(account, 3, "c@x.com", NOW - DAY));
    let mut answer = incoming(account, 4, "me@example.com", NOW - DAY + 1000);
    answer.subject = "Re: Message 3".into();
    answer.in_reply_to = Some("<3@example.com>".into());
    add(&mut store, &answer);
    let mut no_actioned = settings.clone();
    no_actioned.labels_off = vec![AutoLabel::Actioned];
    store
        .record_verdict(other, respond, &no_actioned, true, NOW)
        .unwrap();
    assert_eq!(store.auto_label(other).unwrap(), None);
}

#[test]
fn labels_have_ids_and_names() {
    for label in AutoLabel::ALL {
        assert_eq!(AutoLabel::parse(label.as_str()), Some(label));
        let id: String = serde_json::from_str(&serde_json::to_string(&label).unwrap()).unwrap();
        assert_eq!(id, label.as_str());
    }
    assert_eq!(AutoLabel::MeetingUpdate.name(), "Meeting update");
    assert_eq!(AutoLabel::parse("other"), None);
}
