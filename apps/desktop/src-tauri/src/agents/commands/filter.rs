//! What `emails` and `search` list: a scope (`Inbox`, `user@example.com`,
//! `user@example.com:Archive`, a folder of the user's) and Spark's
//! Gmail-style filter (`from:ana is:unread newer_than:7d`), with `-` before
//! an operator to leave out what it matches.

use std::collections::{HashMap, HashSet};

use chrono::{Datelike, Local, TimeZone};
use spark_store::{
    Account, Category, Folder, FolderRole, GatekeeperMode, Message, MessageState, Store,
};

use super::{Fail, db, parse_time};

/// A list every account has, as spark-rs shows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum List {
    Inbox,
    NewSenders,
    Snoozed,
    SetAside,
    Done,
    Pinned,
    Sent,
    Drafts,
    Spam,
    Trash,
    Blocked,
}

impl List {
    pub const ALL: [List; 11] = [
        List::Inbox,
        List::NewSenders,
        List::Pinned,
        List::Snoozed,
        List::SetAside,
        List::Done,
        List::Sent,
        List::Drafts,
        List::Spam,
        List::Trash,
        List::Blocked,
    ];

    pub fn name(self) -> &'static str {
        match self {
            List::Inbox => "Inbox",
            List::NewSenders => "New Senders",
            List::Snoozed => "Snoozed",
            List::SetAside => "Set Aside",
            List::Done => "Done",
            List::Pinned => "Pinned",
            List::Sent => "Sent",
            List::Drafts => "Drafts",
            List::Spam => "Spam",
            List::Trash => "Trash",
            List::Blocked => "Blocked",
        }
    }

    fn parse(name: &str) -> Option<List> {
        let key: String = name
            .chars()
            .filter(|c| c.is_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect();
        Some(match key.as_str() {
            "inbox" => List::Inbox,
            "newsenders" | "gatekeeper" | "screener" => List::NewSenders,
            "snoozed" => List::Snoozed,
            "setaside" | "aside" => List::SetAside,
            "done" | "archive" | "archived" | "allmail" => List::Done,
            "pinned" | "starred" | "flagged" => List::Pinned,
            "sent" | "sentmail" | "sentitems" => List::Sent,
            "drafts" | "draft" => List::Drafts,
            "spam" | "junk" | "junkemail" => List::Spam,
            "trash" | "deleted" | "deleteditems" | "bin" => List::Trash,
            "blocked" => List::Blocked,
            _ => return None,
        })
    }

    fn role(self) -> Option<FolderRole> {
        match self {
            List::Sent => Some(FolderRole::Sent),
            List::Drafts => Some(FolderRole::Drafts),
            List::Spam => Some(FolderRole::Spam),
            _ => None,
        }
    }
}

/// Where to look.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Place {
    List(List),
    /// Folders of the user's own, by id.
    Folders(Vec<i64>),
    /// Every folder but Trash, Spam and Blocked, as Spark's search field.
    Everywhere,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Scope {
    /// Accounts to look in; the shared accounts when the scope names none.
    pub accounts: Vec<i64>,
    pub place: Place,
    /// As the agent wrote it, or the default's name.
    pub label: String,
}

/// What the store knows that scopes and filters need, read once.
pub struct View {
    pub accounts: Vec<Account>,
    pub folders: Vec<Folder>,
    pub messages: Vec<Message>,
    pub screen_before: bool,
    /// Folder id → its path, `Parent/Child`.
    paths: HashMap<i64, String>,
}

impl View {
    pub fn read(store: &Store, accounts: Vec<Account>) -> Result<Self, Fail> {
        let folders = store.folders().map_err(db)?;
        let ids: HashSet<i64> = accounts.iter().map(|a| a.id).collect();
        let messages = store
            .messages()
            .map_err(db)?
            .into_iter()
            .filter(|m| ids.contains(&m.account_id))
            .collect();
        let screen_before = store.gatekeeper_mode().map_err(db)? == GatekeeperMode::BeforeInbox;
        let paths = folders.iter().map(|f| (f.id, path(&folders, f))).collect();
        Ok(Self {
            accounts,
            folders,
            messages,
            screen_before,
            paths,
        })
    }

    pub fn account(&self, id: i64) -> Option<&Account> {
        self.accounts.iter().find(|a| a.id == id)
    }

    pub fn path(&self, folder: i64) -> &str {
        self.paths.get(&folder).map_or("", String::as_str)
    }

    /// The folder with `role` in `account`.
    pub fn role_folder(&self, account: i64, role: FolderRole) -> Option<&Folder> {
        self.folders
            .iter()
            .find(|f| f.account_id == account && f.role == Some(role))
    }

    fn in_folder(m: &Message, folder: i64) -> bool {
        m.folder == Some(folder) || m.folders.contains(&folder)
    }

    /// Whether `m` is in `list`.
    pub fn is_in(&self, m: &Message, list: List) -> bool {
        match list {
            List::Inbox => m.state == MessageState::Inbox && !(m.screening && self.screen_before),
            List::NewSenders => m.state == MessageState::Inbox && m.screening,
            List::Snoozed => m.state == MessageState::Snoozed,
            List::SetAside => m.state == MessageState::Aside,
            List::Done => m.state == MessageState::Done,
            List::Trash => m.state == MessageState::Deleted,
            List::Blocked => m.state == MessageState::Blocked,
            List::Pinned => {
                m.pinned && !matches!(m.state, MessageState::Deleted | MessageState::Blocked)
            }
            List::Sent | List::Drafts | List::Spam => {
                let role = list.role().expect("role lists have roles");
                self.role_folder(m.account_id, role)
                    .is_some_and(|f| Self::in_folder(m, f.id))
            }
        }
    }

    pub fn in_scope(&self, m: &Message, scope: &Scope) -> bool {
        if !scope.accounts.contains(&m.account_id) {
            return false;
        }
        match &scope.place {
            Place::List(list) => self.is_in(m, *list),
            Place::Folders(ids) => ids.iter().any(|&f| Self::in_folder(m, f)),
            Place::Everywhere => {
                !matches!(m.state, MessageState::Deleted | MessageState::Blocked)
                    && !self.is_in(m, List::Spam)
            }
        }
    }

    /// Where `m` is, by the name `folders` gives it.
    pub fn folder_name(&self, m: &Message) -> String {
        let list = match m.state {
            MessageState::Inbox if m.screening && self.screen_before => List::NewSenders,
            MessageState::Inbox => List::Inbox,
            MessageState::Snoozed => List::Snoozed,
            MessageState::Aside => List::SetAside,
            MessageState::Done => List::Done,
            MessageState::Deleted => List::Trash,
            MessageState::Blocked => List::Blocked,
            MessageState::Filed => {
                let folder = m.folder.or_else(|| m.folders.first().copied());
                return match folder.and_then(|id| self.folders.iter().find(|f| f.id == id)) {
                    Some(f) => match f.role {
                        Some(FolderRole::Sent) => "Sent".into(),
                        Some(FolderRole::Drafts) => "Drafts".into(),
                        Some(FolderRole::Spam) => "Spam".into(),
                        _ => self.path(f.id).to_owned(),
                    },
                    None => "Filed".into(),
                };
            }
        };
        list.name().into()
    }

    /// Resolves one folder argument, or the default when there's none.
    pub fn scope(&self, arg: Option<&str>, default: Place) -> Result<Scope, Fail> {
        let all: Vec<i64> = self.accounts.iter().map(|a| a.id).collect();
        let Some(arg) = arg.map(str::trim).filter(|a| !a.is_empty()) else {
            let label = match &default {
                Place::List(list) => list.name().to_owned(),
                _ => "All folders".to_owned(),
            };
            return Ok(Scope {
                accounts: all,
                place: default,
                label,
            });
        };
        let (account, folder) = match arg.split_once(':') {
            Some((who, folder)) if who.contains('@') => (Some(who.trim()), folder.trim()),
            _ if arg.contains('@') && !arg.contains('/') => (Some(arg), ""),
            _ => (None, arg),
        };
        let accounts = match account {
            Some(email) => {
                let found = self
                    .accounts
                    .iter()
                    .find(|a| a.email.eq_ignore_ascii_case(email))
                    .ok_or_else(|| {
                        Fail::Error(format!(
                            "There's no account {email} shared with agents. `accounts` lists them."
                        ))
                    })?;
                vec![found.id]
            }
            None => all,
        };
        let label = arg.to_owned();
        if folder.is_empty() {
            // An account alone: its inbox, or for search all its folders.
            return Ok(Scope {
                accounts,
                place: match default {
                    Place::Everywhere => Place::Everywhere,
                    _ => Place::List(List::Inbox),
                },
                label,
            });
        }
        if let Some(list) = List::parse(folder) {
            return Ok(Scope {
                accounts,
                place: Place::List(list),
                label,
            });
        }
        let wanted = folder.to_lowercase();
        let ids: Vec<i64> = self
            .folders
            .iter()
            .filter(|f| accounts.contains(&f.account_id) && f.role.is_none())
            .filter(|f| self.path(f.id).to_lowercase() == wanted || f.name.to_lowercase() == wanted)
            .map(|f| f.id)
            .collect();
        if ids.is_empty() {
            return Err(Fail::Error(format!(
                "There's no folder {arg}. `folders` lists them."
            )));
        }
        Ok(Scope {
            accounts,
            place: Place::Folders(ids),
            label,
        })
    }

    /// `email:Path` for a folder, as `folders` lists it.
    pub fn qualified(&self, folder: &Folder) -> String {
        let email = self
            .account(folder.account_id)
            .map_or("", |a| a.email.as_str());
        format!("{email}:{}", self.path(folder.id))
    }
}

/// `Parent/Child` for a nested folder.
fn path(folders: &[Folder], folder: &Folder) -> String {
    let mut parts = vec![folder.name.as_str()];
    let mut parent = folder.parent_id;
    // A cycle in bad data mustn't loop forever.
    for _ in 0..32 {
        let Some(p) = parent.and_then(|id| folders.iter().find(|f| f.id == id)) else {
            break;
        };
        parts.push(&p.name);
        parent = p.parent_id;
    }
    parts.reverse();
    parts.join("/")
}

/// One operator of a filter.
#[derive(Clone, Debug, PartialEq)]
enum Term {
    From(String),
    To(String),
    Cc(String),
    Subject(String),
    /// Received at or after (Unix ms).
    After(i64),
    Before(i64),
    Has(Has),
    Is(Is),
    Category(Kind),
    Filename(String),
    /// Free words, found through the search index.
    Words(String),
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Has {
    Attachment,
    Document,
    Spreadsheet,
    Presentation,
    Reminder,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Is {
    Unread,
    Read,
    Pinned,
    Unreplied,
    Priority,
    Muted,
    Snoozed,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Kind {
    Priority,
    People,
    Notifications,
    Newsletters,
    Invitation,
    InvitationResponse,
}

/// A parsed `--filter`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Filter {
    terms: Vec<(Term, bool)>,
}

impl Filter {
    pub fn parse(text: &str) -> Result<Self, Fail> {
        let mut filter = Filter::default();
        let mut words = Vec::new();
        for token in tokens(text) {
            let (negated, token) = match token.strip_prefix('-') {
                Some(rest) if rest.contains(':') => (true, rest.to_owned()),
                _ => (false, token),
            };
            let Some((op, value)) = token.split_once(':').filter(|(op, _)| is_operator(op)) else {
                words.push(token);
                continue;
            };
            let value = value.trim_matches('"').trim().to_owned();
            if value.is_empty() {
                return Err(Fail::Error(format!("{op}: needs a value.")));
            }
            let lower = value.to_lowercase();
            let term = match op.to_lowercase().as_str() {
                "from" => Term::From(lower),
                "to" => Term::To(lower),
                "cc" => Term::Cc(lower),
                "subject" => Term::Subject(lower),
                "filename" => Term::Filename(lower),
                "after" => Term::After(parse_time(&value, false)?),
                "before" => Term::Before(parse_time(&value, false)?),
                "newer_than" => Term::After(ago(&lower)?),
                "older_than" => Term::Before(ago(&lower)?),
                "has" => Term::Has(match lower.as_str() {
                    "attachment" | "attachments" => Has::Attachment,
                    "document" => Has::Document,
                    "spreadsheet" => Has::Spreadsheet,
                    "presentation" => Has::Presentation,
                    "reminder" => Has::Reminder,
                    _ => return Err(unknown("has", &value)),
                }),
                "is" => Term::Is(match lower.as_str() {
                    "unread" => Is::Unread,
                    "read" => Is::Read,
                    "pinned" | "starred" => Is::Pinned,
                    "unreplied" => Is::Unreplied,
                    "priority" | "important" => Is::Priority,
                    "muted" => Is::Muted,
                    "snoozed" => Is::Snoozed,
                    _ => return Err(unknown("is", &value)),
                }),
                "category" => Term::Category(match lower.as_str() {
                    "priority" => Kind::Priority,
                    "personal" | "people" => Kind::People,
                    "notification" | "notifications" => Kind::Notifications,
                    "newsletter" | "newsletters" => Kind::Newsletters,
                    "invitation" | "invitations" | "invites" => Kind::Invitation,
                    "invitation_response" | "invitation_responses" | "responses" => {
                        Kind::InvitationResponse
                    }
                    _ => return Err(unknown("category", &value)),
                }),
                other => {
                    return Err(Fail::Error(format!(
                        "spark-rs doesn't know the {other}: operator."
                    )));
                }
            };
            filter.terms.push((term, negated));
        }
        if !words.is_empty() {
            filter.terms.push((Term::Words(words.join(" ")), false));
        }
        Ok(filter)
    }

    /// Free words to look up in the search index, if any.
    pub fn words(&self) -> Option<&str> {
        self.terms.iter().find_map(|(t, _)| match t {
            Term::Words(w) => Some(w.as_str()),
            _ => None,
        })
    }

    /// Whether the filter needs each message's recipients and files.
    fn needs_content(&self) -> bool {
        self.terms.iter().any(|(t, _)| {
            matches!(
                t,
                Term::Cc(_)
                    | Term::Filename(_)
                    | Term::Has(Has::Document | Has::Spreadsheet | Has::Presentation)
            )
        })
    }

    /// The messages of `candidates` it matches. `found` is what the search
    /// index found for its words, if it has any.
    pub fn apply<'m>(
        &self,
        store: &Store,
        mailbox: &'m View,
        candidates: Vec<&'m Message>,
        found: Option<&HashSet<i64>>,
        now: i64,
    ) -> Result<Vec<&'m Message>, Fail> {
        let own: HashSet<String> = mailbox
            .accounts
            .iter()
            .map(|a| a.email.to_lowercase())
            .collect();
        let replied = if self
            .terms
            .iter()
            .any(|(t, _)| *t == Term::Is(Is::Unreplied))
        {
            replied(mailbox, &own)
        } else {
            HashSet::new()
        };
        let mut out = Vec::new();
        for m in candidates {
            let mut content = None;
            let mut matched = true;
            for (term, negated) in &self.terms {
                let hit = match term {
                    Term::Words(_) => found.is_none_or(|f| f.contains(&m.id)),
                    Term::Cc(_)
                    | Term::Filename(_)
                    | Term::Has(Has::Document | Has::Spreadsheet | Has::Presentation) => {
                        if content.is_none() && self.needs_content() {
                            content = store.content(m.id).map_err(db)?;
                        }
                        content_matches(term, content.as_ref())
                    }
                    _ => matches(term, m, &replied, &own, now),
                };
                if hit == *negated {
                    matched = false;
                    break;
                }
            }
            if matched {
                out.push(m);
            }
        }
        Ok(out)
    }
}

fn is_operator(op: &str) -> bool {
    matches!(
        op.to_lowercase().as_str(),
        "from"
            | "to"
            | "cc"
            | "subject"
            | "filename"
            | "after"
            | "before"
            | "newer_than"
            | "older_than"
            | "has"
            | "is"
            | "category"
    )
}

fn unknown(op: &str, value: &str) -> Fail {
    Fail::Error(format!("spark-rs doesn't know {op}:{value}."))
}

/// `7d`, `2w`, `3m`, `1y` before now.
fn ago(text: &str) -> Result<i64, Fail> {
    let bad = || Fail::Error(format!("{text} isn't a span such as 7d, 2w, 3m or 1y."));
    let (n, unit) = text.split_at(text.len().saturating_sub(1));
    let n: i64 = n.parse().map_err(|_| bad())?;
    let now = Local::now();
    let then = match unit {
        "d" => now - chrono::Duration::days(n),
        "w" => now - chrono::Duration::weeks(n),
        "m" => now
            .checked_sub_months(chrono::Months::new(n.clamp(0, 1200) as u32))
            .ok_or_else(bad)?,
        "y" => now
            .with_year(now.year() - n.clamp(0, 100) as i32)
            .or_else(|| {
                Local
                    .timestamp_millis_opt(now.timestamp_millis() - n * 365 * 86_400_000)
                    .single()
            })
            .ok_or_else(bad)?,
        _ => return Err(bad()),
    };
    Ok(then.timestamp_millis())
}

/// Splits at spaces, keeping `"quoted words"` together.
fn tokens(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    for c in text.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                current.push(c);
            }
            c if c.is_whitespace() && !quoted => {
                if !current.is_empty() {
                    out.push(std::mem::take(&mut current));
                }
            }
            c => current.push(c),
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

fn matches(
    term: &Term,
    m: &Message,
    replied: &HashSet<i64>,
    own: &HashSet<String>,
    now: i64,
) -> bool {
    match term {
        Term::From(x) => format!("{} {}", m.from_name, m.from_address)
            .to_lowercase()
            .contains(x),
        Term::To(x) => m.to.iter().any(|t| t.to_lowercase().contains(x)),
        Term::Subject(x) => m.subject.to_lowercase().contains(x),
        Term::After(t) => m.date >= *t,
        Term::Before(t) => m.date < *t,
        Term::Has(Has::Attachment) => m.attachment.is_some(),
        Term::Has(Has::Reminder) => m.reminder.is_some(),
        Term::Is(Is::Unread) => m.unread,
        Term::Is(Is::Read) => !m.unread,
        Term::Is(Is::Pinned) => m.pinned,
        Term::Is(Is::Priority) => m.priority,
        Term::Is(Is::Muted) => m.muted,
        Term::Is(Is::Snoozed) => {
            m.state == MessageState::Snoozed && m.snoozed_until.is_some_and(|t| t > now)
        }
        Term::Is(Is::Unreplied) => {
            !own.contains(&m.from_address.to_lowercase()) && !replied.contains(&m.id)
        }
        Term::Category(kind) => match kind {
            Kind::Priority => m.priority,
            Kind::People => m.category == Category::People,
            Kind::Notifications => m.category == Category::Notifications,
            Kind::Newsletters => m.category == Category::Newsletters,
            Kind::Invitation => m.invite_kind.as_deref() == Some("invitation"),
            Kind::InvitationResponse => m.invite_kind.as_deref() == Some("reply"),
        },
        Term::Words(_)
        | Term::Cc(_)
        | Term::Filename(_)
        | Term::Has(Has::Document | Has::Spreadsheet | Has::Presentation) => true,
    }
}

fn content_matches(term: &Term, content: Option<&spark_store::Content>) -> bool {
    let Some(c) = content else {
        return false;
    };
    let names = || {
        c.attachments
            .iter()
            .map(|a| (a.name.to_lowercase(), a.mime.to_lowercase()))
    };
    match term {
        Term::Cc(x) => c.cc.iter().any(|t| t.to_lowercase().contains(x)),
        Term::Filename(x) => names().any(|(n, _)| n.contains(x)),
        Term::Has(Has::Document) => names().any(|(n, m)| {
            ends(
                &n,
                &[".pdf", ".doc", ".docx", ".odt", ".rtf", ".txt", ".pages"],
            ) || m.contains("pdf")
                || m.contains("wordprocessing")
                || m == "application/msword"
        }),
        Term::Has(Has::Spreadsheet) => names().any(|(n, m)| {
            ends(&n, &[".xls", ".xlsx", ".ods", ".csv", ".numbers"])
                || m.contains("spreadsheet")
                || m.contains("excel")
        }),
        Term::Has(Has::Presentation) => names().any(|(n, m)| {
            ends(&n, &[".ppt", ".pptx", ".odp", ".key"])
                || m.contains("presentation")
                || m.contains("powerpoint")
        }),
        _ => true,
    }
}

fn ends(name: &str, extensions: &[&str]) -> bool {
    extensions.iter().any(|e| name.ends_with(e))
}

/// Messages someone answered from one of the user's accounts: a later
/// message of theirs in the same conversation.
fn replied(mailbox: &View, own: &HashSet<String>) -> HashSet<i64> {
    let mut last_own: HashMap<i64, i64> = HashMap::new();
    for m in &mailbox.messages {
        if own.contains(&m.from_address.to_lowercase()) {
            let at = last_own.entry(m.conversation).or_insert(m.date);
            *at = (*at).max(m.date);
        }
    }
    mailbox
        .messages
        .iter()
        .filter(|m| last_own.get(&m.conversation).is_some_and(|&at| at > m.date))
        .map(|m| m.id)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_parse_operators_quotes_and_negation() {
        let f = Filter::parse(r#"from:Ana subject:"quarterly report" -is:read budget"#).unwrap();
        assert_eq!(
            f.terms,
            vec![
                (Term::From("ana".into()), false),
                (Term::Subject("quarterly report".into()), false),
                (Term::Is(Is::Read), true),
                (Term::Words("budget".into()), false),
            ]
        );
        assert_eq!(f.words(), Some("budget"));
        assert!(Filter::parse("is:sideways").is_err());
        assert!(Filter::parse("from:").is_err());
        // A word with a colon that isn't an operator is just a word.
        assert_eq!(
            Filter::parse("re:budget").unwrap().words(),
            Some("re:budget")
        );
    }

    #[test]
    fn spans_count_back_from_now() {
        let now = Local::now().timestamp_millis();
        let week = ago("7d").unwrap();
        assert!((now - week - 7 * 86_400_000).abs() < 5_000);
        assert!(ago("2w").unwrap() < week);
        assert!(ago("1m").is_ok() && ago("1y").is_ok());
        assert!(ago("7").is_err() && ago("xd").is_err());
    }

    #[test]
    fn list_names_have_aliases() {
        assert_eq!(List::parse("Archive"), Some(List::Done));
        assert_eq!(List::parse("set aside"), Some(List::SetAside));
        assert_eq!(List::parse("Junk Email"), Some(List::Spam));
        assert_eq!(List::parse("new_senders"), Some(List::NewSenders));
        assert_eq!(List::parse("Projects"), None);
    }
}
