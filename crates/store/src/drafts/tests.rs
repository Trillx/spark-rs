use super::*;
use crate::{AccountKind, FolderRole, NewAccount, RemoteFolder, SCHEMA, SCHEMA_2, SCHEMA_3};

/// An IMAP account with a Drafts folder; returns the account and folder ids.
fn store_with_drafts() -> (Store, i64, i64) {
    let mut store = Store::open_in_memory().unwrap();
    let account = store
        .add_account(&NewAccount {
            kind: AccountKind::Imap,
            email: "me@example.com",
            name: "Me",
            label: "Work",
            config: "{}",
        })
        .unwrap();
    store
        .replace_folders(
            account.id,
            &[RemoteFolder {
                remote_id: "Drafts".into(),
                parent: None,
                name: "Drafts".into(),
                role: Some(FolderRole::Drafts),
                selectable: true,
            }],
        )
        .unwrap();
    let folder = store.folders().unwrap()[0].id;
    (store, account.id, folder)
}

fn save(store: &mut Store, account_id: i64, existing: Option<i64>, body: &str, now: i64) -> i64 {
    let to = ["Ana Ruiz <ana@x.com>".to_owned()];
    let files = [OutboxFile {
        name: "plan.pdf".into(),
        mime: "application/pdf".into(),
        data: body.as_bytes().to_vec(),
    }];
    let attachments = [Attachment {
        name: "plan.pdf".into(),
        mime: "application/pdf".into(),
        size: body.len() as u64,
        remote_id: None,
    }];
    store
        .save_draft(
            existing,
            &DraftSave {
                account_id,
                draft: &format!("{{\"body\":\"{body}\"}}"),
                from_name: "Me",
                from_address: "me@example.com",
                to: &to,
                cc: &[],
                bcc: &["boss@x.com".to_owned()],
                subject: "Plan",
                snippet: body,
                text: body,
                html: None,
                in_reply_to: Some("1@x.com"),
                references: Some("0@x.com 1@x.com"),
                attachments: &attachments,
                files: &files,
            },
            now,
        )
        .unwrap()
}

fn imap_copy(uid: u32, message_id: &str) -> DraftCopy {
    DraftCopy {
        mailbox: Some("Drafts".into()),
        uid: Some(uid),
        remote_id: None,
        message_id: Some(message_id.into()),
    }
}

fn upload_of(store: &Store, account_id: i64) -> DraftUpload {
    let mut uploads = store.draft_uploads(account_id, i64::MAX).unwrap();
    assert_eq!(uploads.len(), 1, "{uploads:?}");
    uploads.remove(0)
}

#[test]
fn a_saved_draft_lists_in_drafts_and_waits_to_upload() {
    let (mut store, account_id, drafts) = store_with_drafts();
    let id = save(&mut store, account_id, None, "First go", 1_000);

    let listed = store.messages().unwrap();
    assert_eq!(listed.len(), 1);
    let m = &listed[0];
    assert_eq!((m.id, m.state, m.unread), (id, MessageState::Filed, false));
    assert_eq!(m.folders, [drafts]);
    assert_eq!(m.to, ["Ana Ruiz <ana@x.com>"]);
    assert_eq!(m.attachment.as_deref(), Some("plan.pdf"));
    assert_eq!(store.content(id).unwrap().unwrap().bcc, ["boss@x.com"]);
    assert_eq!(store.in_reply_to(id).unwrap().as_deref(), Some("1@x.com"));
    assert!(store.is_draft(id).unwrap());
    assert_eq!(
        store.draft(id).unwrap().as_deref(),
        Some(r#"{"body":"First go"}"#)
    );
    assert_eq!(store.draft_files(id).unwrap()[0].data, b"First go");

    // Quick saves settle before going up.
    assert!(store.draft_uploads(account_id, 999).unwrap().is_empty());
    assert!(!store.has_draft_work(account_id, 999).unwrap());
    assert!(store.has_draft_work(account_id, 1_000).unwrap());
    let upload = upload_of(&store, account_id);
    assert_eq!((upload.message_id, upload.copy), (id, None));

    // Saving again updates the same row and queued save.
    let again = save(&mut store, account_id, Some(id), "Second go", 1_000);
    assert_eq!(again, id);
    assert_eq!(store.messages().unwrap().len(), 1);
    let upload = upload_of(&store, account_id);
    assert_eq!(upload.draft, r#"{"body":"Second go"}"#);
    assert_eq!(
        upload.saved_at, 1_001,
        "a later save in the same ms still orders"
    );
    assert_eq!(store.draft_files(id).unwrap().len(), 1);
    assert_eq!(store.draft_files(id).unwrap()[0].data, b"Second go");
}

#[test]
fn uploads_replace_the_previous_server_copy() {
    let (mut store, account_id, drafts) = store_with_drafts();
    let id = save(&mut store, account_id, None, "v1", 1_000);
    let upload = upload_of(&store, account_id);
    store
        .draft_uploaded(&upload, &imap_copy(5, "a@example.com"), &upload.attachments)
        .unwrap();
    assert!(store.draft(id).unwrap().is_none());
    assert!(store.draft_files(id).unwrap().is_empty());
    assert!(
        store.draft_removals(account_id).unwrap().is_empty(),
        "nothing replaced"
    );
    assert_eq!(store.folder_uids(drafts).unwrap()[&5], id);
    let at = store.location(id).unwrap().unwrap();
    assert_eq!((at.mailbox.as_deref(), at.uid), (Some("Drafts"), Some(5)));

    // Edited and saved: the next upload replaces UID 5, which is queued to go.
    save(&mut store, account_id, Some(id), "v2", 2_000);
    let upload = upload_of(&store, account_id);
    assert_eq!(upload.copy, Some(imap_copy(5, "a@example.com")));
    store
        .draft_uploaded(&upload, &imap_copy(6, "b@example.com"), &[])
        .unwrap();
    let removals = store.draft_removals(account_id).unwrap();
    assert_eq!(removals.len(), 1);
    assert_eq!(removals[0].copy, imap_copy(5, "a@example.com"));
    assert_eq!(store.folder_uids(drafts).unwrap()[&6], id);
    assert!(store.message(id).unwrap().unwrap().attachment.is_none());
    store.draft_removed(removals[0].id).unwrap();
    assert!(!store.has_draft_work(account_id, i64::MAX).unwrap());
}

#[test]
fn a_save_during_an_upload_stays_queued() {
    let (mut store, account_id, _) = store_with_drafts();
    let id = save(&mut store, account_id, None, "v1", 1_000);
    let upload = upload_of(&store, account_id);
    save(&mut store, account_id, Some(id), "v2", 1_500);
    store
        .draft_uploaded(&upload, &imap_copy(5, "a@example.com"), &[])
        .unwrap();
    let next = upload_of(&store, account_id);
    assert_eq!(next.draft, r#"{"body":"v2"}"#);
    assert_eq!(next.copy, Some(imap_copy(5, "a@example.com")));
}

#[test]
fn folder_syncs_keep_drafts_not_uploaded_yet() {
    let (mut store, account_id, drafts) = store_with_drafts();
    let id = save(&mut store, account_id, None, "v1", 1_000);
    assert_eq!(store.set_folder_members(drafts, &[]).unwrap(), 0);
    assert_eq!(store.message(id).unwrap().unwrap().folders, [drafts]);

    // Once uploaded, the server's list decides.
    let upload = upload_of(&store, account_id);
    store
        .draft_uploaded(&upload, &imap_copy(5, "a@example.com"), &[])
        .unwrap();
    assert_eq!(store.set_folder_members(drafts, &[]).unwrap(), 1);
}

#[test]
fn discarding_deletes_the_draft_and_queues_its_server_copy() {
    let (mut store, account_id, _) = store_with_drafts();
    // Never uploaded: nothing for the server.
    let local = save(&mut store, account_id, None, "local", 1_000);
    assert!(store.discard_draft(local).unwrap());
    assert!(store.message(local).unwrap().is_none());
    assert!(store.draft(local).unwrap().is_none());
    assert!(!store.has_draft_work(account_id, i64::MAX).unwrap());

    let id = save(&mut store, account_id, None, "v1", 1_000);
    let upload = upload_of(&store, account_id);
    store
        .draft_uploaded(&upload, &imap_copy(5, "a@example.com"), &[])
        .unwrap();
    assert!(store.discard_draft(id).unwrap());
    assert!(store.messages().unwrap().is_empty());
    let removals = store.draft_removals(account_id).unwrap();
    assert_eq!(removals[0].copy, imap_copy(5, "a@example.com"));

    // Other mail isn't a draft.
    let inbox = match store
        .add_incoming(
            &Incoming {
                account_id,
                mailbox: Some("INBOX".into()),
                uid: Some(1),
                from_address: "ana@x.com".into(),
                ..Default::default()
            },
            false,
        )
        .unwrap()
    {
        crate::Added::New(id) => id,
        other => panic!("{other:?}"),
    };
    assert!(!store.is_draft(inbox).unwrap());
    assert!(!store.discard_draft(inbox).unwrap());
    assert!(store.message(inbox).unwrap().is_some());
}

#[test]
fn a_draft_discarded_while_uploading_loses_its_new_copy() {
    let (mut store, account_id, _) = store_with_drafts();
    let id = save(&mut store, account_id, None, "v1", 1_000);
    let upload = upload_of(&store, account_id);
    store.discard_draft(id).unwrap();
    store
        .draft_uploaded(&upload, &imap_copy(7, "c@example.com"), &[])
        .unwrap();
    let removals = store.draft_removals(account_id).unwrap();
    assert_eq!(removals.len(), 1);
    assert_eq!(removals[0].copy, imap_copy(7, "c@example.com"));
}

#[test]
fn graph_drafts_update_in_place() {
    let (mut store, account_id, drafts) = store_with_drafts();
    let id = save(&mut store, account_id, None, "v1", 1_000);
    let graph = |remote: &str| DraftCopy {
        remote_id: Some(remote.into()),
        ..Default::default()
    };
    let upload = upload_of(&store, account_id);
    store.draft_uploaded(&upload, &graph("AAMk1"), &[]).unwrap();
    assert_eq!(
        store.headers(id).unwrap().unwrap().remote_id.as_deref(),
        Some("AAMk1")
    );
    assert_eq!(store.message(id).unwrap().unwrap().folders, [drafts]);

    // Updated (PATCH): same id, nothing to delete.
    save(&mut store, account_id, Some(id), "v2", 2_000);
    let upload = upload_of(&store, account_id);
    assert_eq!(
        upload.copy.as_ref().unwrap().remote_id.as_deref(),
        Some("AAMk1")
    );
    store.draft_uploaded(&upload, &graph("AAMk1"), &[]).unwrap();
    assert!(store.draft_removals(account_id).unwrap().is_empty());

    // Made anew (it had files): the old one goes.
    save(&mut store, account_id, Some(id), "v3", 3_000);
    let upload = upload_of(&store, account_id);
    store.draft_uploaded(&upload, &graph("AAMk2"), &[]).unwrap();
    let removals = store.draft_removals(account_id).unwrap();
    assert_eq!(removals[0].copy.remote_id.as_deref(), Some("AAMk1"));
}

#[test]
fn search_follows_draft_edits_and_discards() {
    let (mut store, account_id, _) = store_with_drafts();
    let id = save(&mut store, account_id, None, "quarterly", 1_000);
    let found = |store: &Store, text: &str| store.search(text).unwrap().len();
    assert_eq!(found(&store, "quarterly"), 1);
    let id = save(&mut store, account_id, Some(id), "yearly", 2_000);
    assert_eq!(found(&store, "quarterly"), 0);
    assert_eq!(found(&store, "yearly"), 1);
    store.discard_draft(id).unwrap();
    assert_eq!(found(&store, "yearly"), 0);
}

#[test]
fn drafts_being_sent_are_hidden_and_not_uploaded() {
    let (mut store, account_id, _) = store_with_drafts();
    let id = save(&mut store, account_id, None, "v1", 1_000);
    let outbox = store.queue_send(account_id, "{}", 9_000, &[]).unwrap();
    store.hold_draft_for_send(outbox, id).unwrap();
    assert!(store.messages().unwrap().is_empty());
    assert!(store.search("Plan").unwrap().is_empty());
    assert!(
        store
            .draft_uploads(account_id, i64::MAX)
            .unwrap()
            .is_empty()
    );
    assert!(!store.has_draft_work(account_id, i64::MAX).unwrap());

    // Undo send: it's back, and still goes up.
    assert!(store.cancel_send(outbox).unwrap());
    assert_eq!(store.messages().unwrap().len(), 1);
    assert_eq!(store.search("Plan").unwrap().len(), 1);
    assert_eq!(store.draft_uploads(account_id, i64::MAX).unwrap().len(), 1);

    // Sent: the draft goes for good.
    let outbox = store.queue_send(account_id, "{}", 9_000, &[]).unwrap();
    store.hold_draft_for_send(outbox, id).unwrap();
    assert!(store.discard_draft(id).unwrap());
    store.finish_send(outbox).unwrap();
    assert!(store.messages().unwrap().is_empty());
}

#[test]
fn a_draft_moved_to_another_account_starts_over() {
    let (mut store, account_id, _) = store_with_drafts();
    let other = store
        .add_account(&NewAccount {
            kind: AccountKind::Gmail,
            email: "me@gmail.com",
            name: "Me",
            label: "Personal",
            config: "{}",
        })
        .unwrap()
        .id;
    let id = save(&mut store, account_id, None, "v1", 1_000);
    let upload = upload_of(&store, account_id);
    store
        .draft_uploaded(&upload, &imap_copy(5, "a@example.com"), &[])
        .unwrap();
    let moved = save(&mut store, other, Some(id), "v2", 2_000);
    // (SQLite may hand the new row the old one's id.)
    let listed = store.messages().unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!((listed[0].id, listed[0].account_id), (moved, other));
    assert!(store.location(moved).unwrap().unwrap().uid.is_none());
    assert_eq!(
        store.draft_removals(account_id).unwrap()[0].copy,
        imap_copy(5, "a@example.com")
    );
    assert_eq!(store.draft_uploads(other, i64::MAX).unwrap().len(), 1);
}

#[test]
fn synced_drafts_are_found_in_the_drafts_folder() {
    let (mut store, account_id, drafts) = store_with_drafts();
    let synced = store
        .add_folder_message(
            drafts,
            &Incoming {
                account_id,
                mailbox: Some("Drafts".into()),
                uid: Some(3),
                message_id: Some("d@x.com".into()),
                in_reply_to: Some("1@x.com".into()),
                subject: "From another client".into(),
                ..Default::default()
            },
        )
        .unwrap();
    let id = match synced {
        crate::Added::New(id) | crate::Added::Known(id) => id,
    };
    assert!(store.is_draft(id).unwrap());
    assert_eq!(
        store.find_message_id(account_id, "d@x.com").unwrap(),
        Some(id)
    );
    assert!(store.discard_draft(id).unwrap());
    assert_eq!(
        store.draft_removals(account_id).unwrap()[0].copy,
        imap_copy(3, "d@x.com")
    );
}

#[test]
fn version_3_databases_gain_the_draft_queue() {
    let dir = std::env::temp_dir().join(format!("spark-store-drafts-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("v3.sqlite");
    let _ = std::fs::remove_file(&path);
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        conn.execute_batch(SCHEMA_2).unwrap();
        conn.execute_batch(SCHEMA_3).unwrap();
        conn.pragma_update(None, "user_version", 3).unwrap();
        conn.execute(
            "INSERT INTO accounts (kind, email, name, label, color, config, position)
             VALUES ('imap', 'a@b.c', 'A', 'A', '#000', '{}', 0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO messages (account_id, from_name, from_address, to_list, cc_list,
                subject, snippet, date, text_body, attachments, category, state, unread,
                pinned, remote_place, remote_unread, remote_pinned)
             VALUES (1, 'Ana', 'ana@x.com', '[\"me@b.c\"]', '[]', 'Old', '', 1, '', '[]',
                'people', 'inbox', 0, 0, 'inbox', 0, 0)",
            [],
        )
        .unwrap();
    }
    let mut store = Store::open(&path).unwrap();
    let old = store.messages().unwrap();
    assert_eq!(old[0].to, ["me@b.c"]);
    assert!(store.content(old[0].id).unwrap().unwrap().bcc.is_empty());
    let id = save(&mut store, 1, None, "new", 5);
    assert_eq!(store.draft_uploads(1, 5).unwrap()[0].message_id, id);
    drop(store);
    std::fs::remove_dir_all(&dir).unwrap();
}
