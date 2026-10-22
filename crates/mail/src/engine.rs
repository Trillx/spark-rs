//! The engine the apps drive: adding accounts, and per account one round of
//! work at a time (send what's due, push local changes, fetch new mail).
//! Callers run rounds for an account one after another; different accounts
//! can run in parallel.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

use lettre::message::Mailbox;
use serde::{Deserialize, Serialize};
use spark_store::{Account, AccountKind, NewAccount, Store, now_ms};

use crate::attachment::{self, AttachmentData};
use crate::draft::{self, Draft, Original, OutgoingAttachment};
use crate::graph::{self, Graph};
use crate::imap::{self, Server};
use crate::oauth::{self, App};
use crate::{Error, Result, smtp};

/// How long a local change waits before it's pushed: longer than the UI's
/// undo window, so an undone change never reaches the server.
pub const SETTLE_MS: i64 = 6_000;
/// A send that failed on the network is retried after this long.
const SEND_RETRY_MS: i64 = 60_000;

/// Where passwords and refresh tokens live (the OS credential store in the
/// apps). An empty value deletes.
pub trait Secrets: Send + Sync {
    fn get(&self, name: &str) -> Result<Option<String>, String>;
    fn set(&self, name: &str, value: &str) -> Result<(), String>;
}

pub fn password_key(account_id: i64) -> String {
    format!("account:{account_id}:password")
}

pub fn token_key(account_id: i64) -> String {
    format!("account:{account_id}:refresh-token")
}

/// Backend settings, stored as the account's `config` JSON.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum AccountConfig {
    #[serde(rename_all = "camelCase")]
    Imap {
        username: String,
        imap: Server,
        smtp: Server,
    },
    #[serde(rename_all = "camelCase")]
    Microsoft { client_id: String, tenant: String },
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub new: usize,
    pub gone: usize,
    pub pushed: usize,
}

impl Report {
    /// Whether the message list needs reloading.
    pub fn changed(&self) -> bool {
        self.new + self.gone > 0
    }
}

/// The outcome of one queued draft.
#[derive(Debug)]
pub struct SendOutcome {
    pub id: i64,
    /// `Ok(true)` sent; `Ok(false)` will be retried; `Err` failed for good.
    pub result: Result<bool>,
}

pub struct Engine {
    store: Arc<Mutex<Store>>,
    secrets: Arc<dyn Secrets>,
    /// Microsoft access tokens and their expiry (Unix ms), in memory only.
    tokens: Mutex<HashMap<i64, (String, i64)>>,
}

impl Engine {
    pub fn new(store: Arc<Mutex<Store>>, secrets: Arc<dyn Secrets>) -> Self {
        Self {
            store,
            secrets,
            tokens: Mutex::new(HashMap::new()),
        }
    }

    pub fn store(&self) -> MutexGuard<'_, Store> {
        self.store
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Adds an IMAP account after checking that the password works.
    pub async fn add_password_account(
        &self,
        kind: AccountKind,
        email: &str,
        password: &str,
        label: &str,
        imap: Server,
        smtp: Server,
    ) -> Result<Account> {
        let email = email.trim();
        if !email.contains('@') {
            return Err(Error::Config("Enter your full email address.".into()));
        }
        // Google shows app passwords in groups of four with spaces.
        let password: String = if kind == AccountKind::Gmail {
            password.split_whitespace().collect()
        } else {
            password.trim().to_owned()
        };
        let mut session = imap::login(&imap, email, &password).await.map_err(|e| match e {
            Error::Auth(_) if kind == AccountKind::Gmail => Error::Auth(
                "Google didn't accept that app password. Check the address, and create a new app password if needed."
                    .into(),
            ),
            Error::Auth(_) => Error::Auth("The server didn't accept that email and password.".into()),
            other => other,
        })?;
        let _ = session.logout().await;

        let config = AccountConfig::Imap {
            username: email.to_owned(),
            imap,
            smtp,
        };
        let account = self.store().add_account(&NewAccount {
            kind,
            email,
            name: "",
            label: &default_label(label, kind, email),
            config: &to_json(&config),
        })?;
        if let Err(e) = self.secrets.set(&password_key(account.id), &password) {
            self.store().remove_account(account.id)?;
            return Err(Error::Config(format!("Couldn't save the password: {e}")));
        }
        Ok(account)
    }

    /// Signs in to Microsoft in the browser and adds the account.
    pub async fn add_microsoft_account(
        &self,
        client_id: &str,
        tenant: &str,
        label: &str,
        open_browser: impl FnOnce(&str) -> Result<(), String>,
    ) -> Result<Account> {
        let app = App::new(client_id, tenant)?;
        let tokens = oauth::sign_in(&app, open_browser).await?;
        let me = Graph::new(&tokens.access_token).me().await?;
        let email = me
            .address()
            .ok_or_else(|| Error::Server("Microsoft didn't say which mailbox this is.".into()))?
            .to_owned();
        let refresh = tokens.refresh_token.clone().ok_or_else(|| {
            Error::Auth("Microsoft didn't allow staying signed in (no refresh token).".into())
        })?;
        let config = AccountConfig::Microsoft {
            client_id: app.client_id.clone(),
            tenant: app.tenant.clone(),
        };
        let account = self.store().add_account(&NewAccount {
            kind: AccountKind::Microsoft,
            email: &email,
            name: me.display_name.as_deref().unwrap_or_default(),
            label: &default_label(label, AccountKind::Microsoft, &email),
            config: &to_json(&config),
        })?;
        if let Err(e) = self.secrets.set(&token_key(account.id), &refresh) {
            self.store().remove_account(account.id)?;
            return Err(Error::Config(format!("Couldn't save the sign-in: {e}")));
        }
        self.cache_token(account.id, &tokens);
        Ok(account)
    }

    /// Removes the account, its mail and its saved password or sign-in.
    pub fn remove_account(&self, account_id: i64) -> Result<()> {
        self.store().remove_account(account_id)?;
        self.tokens_lock().remove(&account_id);
        let _ = self.secrets.set(&password_key(account_id), "");
        let _ = self.secrets.set(&token_key(account_id), "");
        Ok(())
    }

    /// One round for an account: push changes older than the undo window,
    /// and with `fetch`, bring in new mail. Opens no connection when there
    /// is nothing to do.
    pub async fn run(&self, account_id: i64, fetch: bool) -> Result<Report> {
        let account = self.account(account_id)?;
        let pending = self.store().pending(account_id, now_ms(), SETTLE_MS)?;
        if pending.is_empty() && !fetch {
            return Ok(Report::default());
        }
        let mut report = Report::default();
        match config(&account)? {
            AccountConfig::Imap {
                username,
                imap: server,
                ..
            } => {
                let password = self.password(account_id)?;
                let mut state: imap::SyncState = from_json(&account.sync_state);
                let mut session = imap::login(&server, &username, &password).await?;
                imap::find_folders(&mut session, &mut state).await?;
                report.pushed = imap::push(&mut session, &self.store, &state, pending).await?;
                if fetch {
                    let sync =
                        imap::sync_inbox(&mut session, &self.store, account_id, &mut state).await;
                    // Keep the position even if part of the sync failed.
                    self.store().set_sync_state(account_id, &to_json(&state))?;
                    let sync = sync?;
                    report.new = sync.new;
                    report.gone = sync.gone;
                }
                self.store().set_sync_state(account_id, &to_json(&state))?;
                let _ = session.logout().await;
            }
            AccountConfig::Microsoft { client_id, tenant } => {
                let graph = Graph::new(
                    &self
                        .access_token(account_id, &App::new(&client_id, &tenant)?)
                        .await?,
                );
                report.pushed = graph.push(&self.store, pending).await?;
                if fetch {
                    let mut state: graph::SyncState = from_json(&account.sync_state);
                    let sync = graph
                        .sync_inbox(&self.store, account_id, &mut state)
                        .await?;
                    self.store().set_sync_state(account_id, &to_json(&state))?;
                    report.new = sync.new;
                    report.gone = sync.gone;
                }
            }
        }
        Ok(report)
    }

    /// Sends the account's drafts whose time has come.
    pub async fn send_due(&self, account_id: i64) -> Result<Vec<SendOutcome>> {
        let due = self.store().claim_due_sends(account_id, now_ms())?;
        let mut outcomes = Vec::new();
        for (id, json) in due {
            let result = match serde_json::from_str::<Draft>(&json) {
                Ok(draft) => self.send_draft(account_id, id, &draft).await,
                Err(e) => Err(Error::Config(format!(
                    "The queued message is unreadable: {e}"
                ))),
            };
            let result = match result {
                Ok(()) => {
                    self.store().finish_send(id)?;
                    Ok(true)
                }
                // Offline or the server is busy: keep it queued.
                Err(Error::Network { .. }) => {
                    self.store().retry_send(id, now_ms() + SEND_RETRY_MS)?;
                    Ok(false)
                }
                Err(e) => {
                    self.store().finish_send(id)?;
                    Err(e)
                }
            };
            outcomes.push(SendOutcome { id, result });
        }
        Ok(outcomes)
    }

    async fn send_draft(&self, account_id: i64, outbox_id: i64, draft: &Draft) -> Result<()> {
        let account = self.account(account_id)?;
        let original = draft
            .reply_to
            .map(|id| self.original(id))
            .transpose()?
            .flatten();
        let files = self.outgoing_files(outbox_id, draft).await?;
        match config(&account)? {
            AccountConfig::Imap {
                username,
                smtp: server,
                ..
            } => {
                let from = Mailbox::new(
                    Some(account.name.clone()).filter(|n| !n.is_empty()),
                    account
                        .email
                        .parse()
                        .map_err(|_| Error::Config("The account's address is invalid.".into()))?,
                );
                let message =
                    draft::build_message(from, draft, original.as_ref().map(|(o, _)| o), &files)?;
                smtp::send(&server, &username, &self.password(account_id)?, message).await?;
            }
            AccountConfig::Microsoft { client_id, tenant } => {
                let token = self
                    .access_token(account_id, &App::new(&client_id, &tenant)?)
                    .await?;
                let remote = original.as_ref().and_then(|(_, remote)| remote.as_deref());
                Graph::new(&token).send_draft(draft, remote, &files).await?;
            }
        }
        // People you write to skip the Gatekeeper.
        let store = self.store();
        for field in [&draft.to, &draft.cc, &draft.bcc] {
            for m in draft::recipients(field).unwrap_or_default() {
                store.accept_sender(account_id, m.email.as_ref())?;
            }
        }
        Ok(())
    }

    /// The draft's attachments in order: the copies queued with it, and
    /// forwarded ones fetched again from the server.
    async fn outgoing_files(&self, outbox_id: i64, draft: &Draft) -> Result<Vec<AttachmentData>> {
        let mut queued = self.store().outbox_files(outbox_id)?.into_iter();
        let mut files = Vec::with_capacity(draft.attachments.len());
        for a in &draft.attachments {
            match a {
                OutgoingAttachment::File { name, .. } => {
                    let f = queued.next().ok_or_else(|| {
                        Error::Config(format!("{name} is missing from the outbox."))
                    })?;
                    files.push(AttachmentData {
                        name: f.name,
                        mime: f.mime,
                        bytes: f.data,
                    });
                }
                OutgoingAttachment::Message { message, index, .. } => {
                    files.push(self.attachment(*message, *index).await?);
                }
            }
        }
        Ok(files)
    }

    /// Downloads attachment `index` of a stored message from its server.
    pub async fn attachment(&self, message_id: i64, index: usize) -> Result<AttachmentData> {
        let (at, wanted) = {
            let store = self.store();
            let gone = || Error::Config("That email is no longer here.".into());
            let at = store.location(message_id)?.ok_or_else(gone)?;
            let content = store.content(message_id)?.ok_or_else(gone)?;
            let wanted = content.attachments.get(index).cloned().ok_or_else(|| {
                Error::Config("That attachment isn't part of the email.".into())
            })?;
            (at, wanted)
        };
        let account = self.account(at.account_id)?;
        match config(&account)? {
            AccountConfig::Imap {
                username,
                imap: server,
                ..
            } => {
                let password = self.password(account.id)?;
                let state: imap::SyncState = from_json(&account.sync_state);
                let mut session = imap::login(&server, &username, &password).await?;
                let raw = imap::fetch_raw(&mut session, &state, &at).await;
                let _ = session.logout().await;
                let raw = raw?.ok_or(Error::Gone)?;
                attachment::extract(&raw, index, &wanted.name).ok_or_else(|| {
                    Error::Server(format!(
                        "{} wasn't in the email on the server.",
                        wanted.name
                    ))
                })
            }
            AccountConfig::Microsoft { client_id, tenant } => {
                let remote = at.remote_id.ok_or(Error::Gone)?;
                let token = self
                    .access_token(account.id, &App::new(&client_id, &tenant)?)
                    .await?;
                Graph::new(&token).attachment(&remote, index, &wanted).await
            }
        }
    }

    /// The message being replied to, and its Graph id if it has one.
    fn original(&self, id: i64) -> Result<Option<(Original, Option<String>)>> {
        let store = self.store();
        let (Some(message), Some(headers), Some(content)) =
            (store.message(id)?, store.headers(id)?, store.content(id)?)
        else {
            return Ok(None);
        };
        Ok(Some((
            Original {
                message_id: headers.message_id,
                references: headers.references,
                from: message.from_name,
                text: content.text,
            },
            headers.remote_id,
        )))
    }

    fn account(&self, account_id: i64) -> Result<Account> {
        self.store()
            .account(account_id)?
            .ok_or_else(|| Error::Config("That account was removed.".into()))
    }

    fn password(&self, account_id: i64) -> Result<String> {
        self.secrets
            .get(&password_key(account_id))
            .map_err(|e| Error::Config(format!("Couldn't read the saved password: {e}")))?
            .ok_or_else(|| {
                Error::Auth(
                    "The saved password is missing. Remove the account and add it again.".into(),
                )
            })
    }

    async fn access_token(&self, account_id: i64, app: &App) -> Result<String> {
        if let Some((token, expires_at)) = self.tokens_lock().get(&account_id)
            && *expires_at - 120_000 > now_ms()
        {
            return Ok(token.clone());
        }
        let refresh = self
            .secrets
            .get(&token_key(account_id))
            .map_err(|e| Error::Config(format!("Couldn't read the saved sign-in: {e}")))?
            .ok_or_else(|| {
                Error::Auth(
                    "Sign in to Microsoft again: remove the account and add it back.".into(),
                )
            })?;
        let tokens = oauth::refresh(app, &refresh).await?;
        if let Some(rotated) = &tokens.refresh_token
            && rotated != &refresh
        {
            self.secrets
                .set(&token_key(account_id), rotated)
                .map_err(|e| Error::Config(format!("Couldn't save the sign-in: {e}")))?;
        }
        self.cache_token(account_id, &tokens);
        Ok(tokens.access_token)
    }

    fn cache_token(&self, account_id: i64, tokens: &oauth::Tokens) {
        self.tokens_lock()
            .insert(account_id, (tokens.access_token.clone(), tokens.expires_at));
    }

    fn tokens_lock(&self) -> MutexGuard<'_, HashMap<i64, (String, i64)>> {
        self.tokens
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

pub fn config(account: &Account) -> Result<AccountConfig> {
    serde_json::from_str(&account.config).map_err(|e| {
        Error::Config(format!(
            "The settings for {} are damaged: {e}",
            account.email
        ))
    })
}

/// Gmail's servers, for app-password accounts.
pub fn gmail_servers() -> (Server, Server) {
    (
        Server::new("imap.gmail.com", 993),
        Server::new("smtp.gmail.com", 465),
    )
}

fn default_label(label: &str, kind: AccountKind, email: &str) -> String {
    let label = label.trim();
    if !label.is_empty() {
        return label.to_owned();
    }
    match kind {
        AccountKind::Gmail => "Gmail".into(),
        AccountKind::Microsoft => "Work".into(),
        AccountKind::Imap => email.split('@').nth(1).unwrap_or(email).to_owned(),
    }
}

fn to_json<T: Serialize>(value: &T) -> String {
    serde_json::to_string(value).expect("settings serialize")
}

fn from_json<T: for<'de> Deserialize<'de> + Default>(text: &str) -> T {
    serde_json::from_str(text).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configs_keep_their_shape() {
        let imap = AccountConfig::Imap {
            username: "me@gmail.com".into(),
            imap: Server::new("imap.gmail.com", 993),
            smtp: Server::new("smtp.gmail.com", 465),
        };
        let json = to_json(&imap);
        assert_eq!(
            json,
            r#"{"type":"imap","username":"me@gmail.com","imap":{"host":"imap.gmail.com","port":993,"security":"tls"},"smtp":{"host":"smtp.gmail.com","port":465,"security":"tls"}}"#
        );
        let microsoft = AccountConfig::Microsoft {
            client_id: "c".into(),
            tenant: "t".into(),
        };
        assert_eq!(
            to_json(&microsoft),
            r#"{"type":"microsoft","clientId":"c","tenant":"t"}"#
        );
        assert_eq!(serde_json::from_str::<AccountConfig>(&json).unwrap(), imap);
    }

    #[test]
    fn labels_default_by_kind() {
        assert_eq!(
            default_label(" Personal ", AccountKind::Gmail, "a@gmail.com"),
            "Personal"
        );
        assert_eq!(
            default_label("", AccountKind::Gmail, "a@gmail.com"),
            "Gmail"
        );
        assert_eq!(
            default_label("", AccountKind::Imap, "a@fastmail.com"),
            "fastmail.com"
        );
    }
}
