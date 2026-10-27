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
fn relabels_a_conversation_once_the_user_replies() {
    let (mut store, account) = store();
    let id = add(&mut store, &incoming(account, 1, "a@x.com", NOW));
    store
        .set_auto_label(id, Some(AutoLabel::ToRespond), false)
        .unwrap();
    let conversation = store.message(id).unwrap().unwrap().conversation;
    let changed = store
        .relabel_conversation(
            conversation,
            account,
            AutoLabel::ToRespond,
            Some(AutoLabel::Actioned),
            true,
        )
        .unwrap();
    assert_eq!(changed, [id]);
    assert_eq!(store.auto_label(id).unwrap(), Some(AutoLabel::Actioned));
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
    assert!(store.auto_draft_chosen(draft, 1).unwrap());
    assert!(store.untouched_auto_drafts(NOW + 1).unwrap().is_empty());
    assert!(store.auto_draft_chosen(draft, 5).is_err());

    // Gone with the draft.
    store.discard_draft(draft).unwrap();
    assert_eq!(store.message(id).unwrap().unwrap().auto_draft, None);
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
fn labels_have_ids_and_names() {
    for label in AutoLabel::ALL {
        assert_eq!(AutoLabel::parse(label.as_str()), Some(label));
        let id: String = serde_json::from_str(&serde_json::to_string(&label).unwrap()).unwrap();
        assert_eq!(id, label.as_str());
    }
    assert_eq!(AutoLabel::MeetingUpdate.name(), "Meeting update");
    assert_eq!(AutoLabel::parse("other"), None);
}
