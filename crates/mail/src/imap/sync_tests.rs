//! Syncs against a scripted IMAP server that counts what they ask for:
//! commands, records (FETCH responses and SEARCH hits) and bytes sent back.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use spark_store::{AccountKind, Message, NewAccount};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

use super::*;

#[derive(Clone, Copy)]
struct Stored {
    /// Stays with the message when the mailbox is renumbered.
    id: u32,
    seen: bool,
    flagged: bool,
    modseq: u64,
}

struct Remote {
    validity: u32,
    next_uid: u32,
    modseq: u64,
    by_uid: BTreeMap<u32, Stored>,
}

impl Remote {
    /// `count` messages, every `spacing`th UID, as if the rest left it.
    fn new(count: u32, spacing: u32) -> Self {
        let by_uid: BTreeMap<u32, Stored> = (1..=count)
            .map(|id| {
                let stored = Stored {
                    id,
                    seen: true,
                    flagged: false,
                    modseq: 1,
                };
                (id * spacing, stored)
            })
            .collect();
        Self {
            validity: 1,
            next_uid: count * spacing + 1,
            modseq: 1,
            by_uid,
        }
    }

    fn uid_of(&self, id: u32) -> u32 {
        self.by_uid
            .iter()
            .find(|(_, m)| m.id == id)
            .map(|(uid, _)| *uid)
            .unwrap()
    }

    fn arrive(&mut self, id: u32) -> u32 {
        let uid = self.next_uid;
        self.next_uid += 1;
        self.modseq += 1;
        let stored = Stored {
            id,
            seen: false,
            flagged: false,
            modseq: self.modseq,
        };
        self.by_uid.insert(uid, stored);
        uid
    }

    fn set_flags(&mut self, id: u32, seen: bool, flagged: bool) {
        let uid = self.uid_of(id);
        self.modseq += 1;
        let m = self.by_uid.get_mut(&uid).unwrap();
        (m.seen, m.flagged, m.modseq) = (seen, flagged, self.modseq);
    }

    fn expunge(&mut self, id: u32) {
        let uid = self.uid_of(id);
        self.by_uid.remove(&uid);
        self.modseq += 1;
    }

    /// A new UIDVALIDITY, its messages numbered from 1 again.
    fn renumber(&mut self) {
        let messages: Vec<Stored> = self.by_uid.values().copied().collect();
        self.by_uid = (1..).zip(messages).collect();
        self.next_uid = self.by_uid.len() as u32 + 1;
        self.validity += 1;
    }
}

fn raw(id: u32) -> String {
    format!(
        "Message-ID: <m{id}@x.test>\r\nReply-To: Desk <desk{id}@x.test>\r\n\
         From: Sender <s{id}@x.test>\r\nTo: me@x.test\r\nSubject: Message {id}\r\n\
         Date: Mon, 26 Oct 2026 10:00:00 +0000\r\n\r\nBody of message {id}.\r\n"
    )
}

/// What one sync asked the server for.
#[derive(Clone, Debug, Default)]
struct Tally {
    commands: Vec<String>,
    records: usize,
    bytes: usize,
}

impl Tally {
    fn asked(&self, what: &str) -> bool {
        self.commands.iter().any(|c| c.contains(what))
    }
}

struct Shared {
    mailboxes: HashMap<String, Remote>,
    condstore: bool,
    /// Commands containing the first get the first half of their answer,
    /// then the second (NO or BAD) as their completion.
    refuse: Option<(&'static str, &'static str)>,
    tally: Tally,
}

struct StandIn {
    server: Server,
    state: Arc<Mutex<Shared>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for StandIn {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl StandIn {
    async fn start(mailboxes: HashMap<String, Remote>, condstore: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let server = Server {
            host: "127.0.0.1".into(),
            port: listener.local_addr().unwrap().port(),
            security: Security::Plain,
            trusted_cert: None,
        };
        let state = Arc::new(Mutex::new(Shared {
            mailboxes,
            condstore,
            refuse: None,
            tally: Tally::default(),
        }));
        let shared = state.clone();
        let task = tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                tokio::spawn(serve(stream, shared.clone()));
            }
        });
        Self {
            server,
            state,
            task,
        }
    }

    fn with<T>(&self, mailbox: &str, change: impl FnOnce(&mut Remote) -> T) -> T {
        change(
            self.state
                .lock()
                .unwrap()
                .mailboxes
                .get_mut(mailbox)
                .unwrap(),
        )
    }

    fn take_tally(&self) -> Tally {
        std::mem::take(&mut self.state.lock().unwrap().tally)
    }

    /// Refuses commands containing `command` with `status` from now on.
    fn refuse(&self, command: &'static str, status: &'static str) {
        self.state.lock().unwrap().refuse = Some((command, status));
    }

    fn heal(&self) {
        self.state.lock().unwrap().refuse = None;
    }
}

async fn serve(stream: TcpStream, state: Arc<Mutex<Shared>>) {
    let (read, mut write) = stream.into_split();
    write.write_all(b"* OK stand-in\r\n").await.unwrap();
    let mut lines = BufReader::new(read).lines();
    let mut selected = None;
    while let Ok(Some(line)) = lines.next_line().await {
        let (tag, command) = line.split_once(' ').unwrap();
        let reply = {
            let mut state = state.lock().unwrap();
            let (data, records, status) = respond(&mut state, &mut selected, command);
            let reply = format!("{data}{tag} {status} done\r\n");
            let tally = &mut state.tally;
            tally.commands.push(command.to_owned());
            tally.records += records;
            tally.bytes += reply.len();
            reply
        };
        if write.write_all(reply.as_bytes()).await.is_err() {
            return;
        }
    }
}

/// Untagged data, how many records it holds, and the tagged status.
fn respond(
    state: &mut Shared,
    selected: &mut Option<String>,
    command: &str,
) -> (String, usize, &'static str) {
    let upper = command.to_ascii_uppercase();
    if upper.starts_with("LOGIN ") || upper == "NOOP" {
        return (String::new(), 0, "OK");
    }
    if upper == "LOGOUT" {
        return ("* BYE\r\n".into(), 0, "OK");
    }
    if upper == "CAPABILITY" {
        let condstore = if state.condstore { " CONDSTORE" } else { "" };
        return (format!("* CAPABILITY IMAP4rev1{condstore}\r\n"), 0, "OK");
    }
    if let Some(name) = ["SELECT ", "EXAMINE "]
        .iter()
        .find_map(|verb| upper.starts_with(verb).then(|| &command[verb.len()..]))
    {
        let condstore = name.ends_with(" (CONDSTORE)");
        if condstore && !state.condstore {
            return (String::new(), 0, "BAD");
        }
        let name = name.trim_end_matches(" (CONDSTORE)").trim_matches('"');
        let Some(mailbox) = state.mailboxes.get(name) else {
            return (String::new(), 0, "NO");
        };
        *selected = Some(name.to_owned());
        let mut data = format!(
            "* {} EXISTS\r\n* OK [UIDVALIDITY {}] .\r\n* OK [UIDNEXT {}] .\r\n",
            mailbox.by_uid.len(),
            mailbox.validity,
            mailbox.next_uid
        );
        // Only once SELECT turns CONDSTORE on, as RFC 7162 lets a server.
        if condstore {
            data.push_str(&format!("* OK [HIGHESTMODSEQ {}] .\r\n", mailbox.modseq));
        }
        return (data, 0, "OK");
    }
    let refused = state
        .refuse
        .filter(|(refused, _)| command.contains(refused))
        .map(|(_, status)| status);
    let status = refused.unwrap_or("OK");
    let mailbox = &state.mailboxes[selected.as_deref().expect("a mailbox is selected")];
    let max_uid = mailbox.by_uid.keys().last().copied().unwrap_or(0);
    let exists = mailbox.by_uid.len() as u32;
    if let Some(query) = upper.strip_prefix("UID SEARCH ") {
        let mut hits: Vec<u32> = match query.strip_prefix("UID ") {
            _ if query == "ALL" => mailbox.by_uid.keys().copied().collect(),
            Some(set) => mailbox
                .by_uid
                .keys()
                .copied()
                .filter(|uid| in_set(set, *uid, max_uid))
                .collect(),
            None => panic!("unexpected search {command}"),
        };
        if refused.is_some() {
            hits.truncate(hits.len() / 2);
        }
        let listed: String = hits.iter().map(|uid| format!(" {uid}")).collect();
        return (format!("* SEARCH{listed}\r\n"), hits.len(), status);
    }
    let (by_uid, rest) = match upper.strip_prefix("UID FETCH ") {
        Some(rest) => (true, rest),
        None => match upper.strip_prefix("FETCH ") {
            Some(rest) => (false, rest),
            None => panic!("unexpected command {command}"),
        },
    };
    let (set, items) = rest.split_once(' ').unwrap();
    let changed_since: Option<u64> = items
        .split_once("(CHANGEDSINCE ")
        .map(|(_, n)| n.trim_end_matches(')').parse().unwrap());
    if changed_since.is_some() && !state.condstore {
        return (String::new(), 0, "BAD");
    }
    let mut records = Vec::new();
    for (seq, (uid, m)) in (1..).zip(&mailbox.by_uid) {
        let wanted = if by_uid {
            in_set(set, *uid, max_uid)
        } else {
            in_set(set, seq, exists)
        };
        if !wanted || changed_since.is_some_and(|since| m.modseq <= since) {
            continue;
        }
        let raw = raw(m.id);
        let mut parts = vec![format!("UID {uid}")];
        if items.contains("FLAGS") {
            let flags: Vec<&str> = [(m.seen, "\\Seen"), (m.flagged, "\\Flagged")]
                .into_iter()
                .filter_map(|(on, flag)| on.then_some(flag))
                .collect();
            parts.push(format!("FLAGS ({})", flags.join(" ")));
        }
        if changed_since.is_some() {
            parts.push(format!("MODSEQ ({})", m.modseq));
        }
        if items.contains("RFC822.SIZE") {
            parts.push(format!("RFC822.SIZE {}", raw.len()));
        }
        if items.contains("INTERNALDATE") {
            parts.push("INTERNALDATE \"26-Oct-2026 10:00:00 +0000\"".into());
        }
        if items.contains("BODY.PEEK[]") {
            parts.push(format!("BODY[] {{{}}}\r\n{raw}", raw.len()));
        } else if items.contains("BODY.PEEK[HEADER]") {
            let header = &raw[..raw.find("\r\n\r\n").unwrap() + 4];
            parts.push(format!("BODY[HEADER] {{{}}}\r\n{header}", header.len()));
        } else if let Some((_, fields)) = items.split_once("HEADER.FIELDS (") {
            let fields = &fields[..fields.find(')').unwrap()];
            let mut header: String = raw
                .split("\r\n")
                .filter(|line| {
                    let name = line.split(':').next().unwrap().to_ascii_uppercase();
                    fields.split(' ').any(|f| f == name)
                })
                .map(|line| format!("{line}\r\n"))
                .collect();
            header.push_str("\r\n");
            parts.push(format!(
                "BODY[HEADER.FIELDS ({fields})] {{{}}}\r\n{header}",
                header.len()
            ));
        }
        records.push(format!("* {seq} FETCH ({})\r\n", parts.join(" ")));
    }
    if refused.is_some() {
        records.truncate(records.len() / 2);
    }
    (records.concat(), records.len(), status)
}

/// Whether `n` is in an IMAP set like "1:3,7,9:*", where `*` is `max`
/// (and so "20:*" holds `max` even below 20).
fn in_set(set: &str, n: u32, max: u32) -> bool {
    let value = |s: &str| if s == "*" { max } else { s.parse().unwrap() };
    set.split(',').any(|part| match part.split_once(':') {
        Some((a, b)) => {
            let (a, b) = (value(a), value(b));
            (a.min(b)..=a.max(b)).contains(&n)
        }
        None => value(part) == n,
    })
}

/// A store with an IMAP account whose inbox has the messages `ids` at
/// their UIDs on the stand-in, stored as before Reply-To was kept.
fn cached(stand_in: &StandIn, ids: impl Iterator<Item = u32>) -> (Mutex<Store>, i64) {
    let store = Store::open_in_memory().unwrap();
    let account = store
        .add_account(&NewAccount {
            kind: AccountKind::Imap,
            email: "me@x.test",
            name: "Me",
            label: "Test",
            config: "{}",
        })
        .unwrap();
    let store = Mutex::new(store);
    cache(&store, account.id, stand_in, ids, None);
    (store, account.id)
}

/// Stores inbox messages as the stand-in has them, read, with `reply_to`.
fn cache(
    store: &Mutex<Store>,
    account_id: i64,
    stand_in: &StandIn,
    ids: impl Iterator<Item = u32>,
    reply_to: Option<Vec<String>>,
) {
    let messages: Vec<Incoming> = ids
        .map(|id| {
            let mut m = parse::parse_message(raw(id).as_bytes()).unwrap();
            m.account_id = account_id;
            m.mailbox = Some(INBOX.into());
            m.uid = Some(stand_in.with(INBOX, |b| b.uid_of(id)));
            m.reply_to = reply_to.clone();
            m
        })
        .collect();
    lock(store)
        .batch(|b| {
            for m in &messages {
                b.add_incoming(m, false)?;
            }
            Ok(())
        })
        .unwrap();
}

fn message(store: &Mutex<Store>, id: u32) -> Message {
    let subject = format!("Message {id}");
    lock(store)
        .messages()
        .unwrap()
        .into_iter()
        .find(|m| m.subject == subject)
        .unwrap()
}

/// Server messages, the newest of them tracked, and how many.
const ON_SERVER: u32 = 3000;
const TRACKED: u32 = 1000;
/// Inbox UIDs are every third: the rest were archived.
const SPACING: u32 = 3;

/// An account with a session on the stand-in, whose inbox has `ON_SERVER`
/// messages, and the newest `TRACKED` of them cached.
struct Cached {
    stand_in: StandIn,
    store: Mutex<Store>,
    account_id: i64,
    state: SyncState,
    session: Session,
}

impl Cached {
    /// Cached before Reply-To was kept: its state's JSON has none of the
    /// fields added since.
    async fn old(condstore: bool) -> Self {
        let stand_in = StandIn::start(
            HashMap::from([(INBOX.to_owned(), Remote::new(ON_SERVER, SPACING))]),
            condstore,
        )
        .await;
        let oldest_tracked = ON_SERVER - TRACKED + 1;
        let (store, account_id) = cached(&stand_in, oldest_tracked..=ON_SERVER);
        let state: SyncState = serde_json::from_value(serde_json::json!({
            "uidValidity": 1,
            "floor": oldest_tracked * SPACING,
            "archive": "Archive",
            "trash": "Trash",
        }))
        .unwrap();
        let session = login(&stand_in.server, "me", "pw").await.unwrap();
        stand_in.take_tally();
        Self {
            stand_in,
            store,
            account_id,
            state,
            session,
        }
    }

    /// `old`, then synced: Reply-To fetched, and with CONDSTORE a modseq kept.
    async fn synced(condstore: bool) -> Self {
        let mut cached = Self::old(condstore).await;
        cached.sync().await.unwrap();
        assert!(cached.state.reply_to_fetched);
        assert_eq!(cached.state.modseq.is_some(), condstore);
        cached.stand_in.take_tally();
        cached
    }

    async fn sync(&mut self) -> Result<SyncReport> {
        sync_inbox(
            &mut self.session,
            &self.store,
            self.account_id,
            &mut self.state,
        )
        .await
    }

    async fn older(&mut self) -> Result<usize> {
        older_inbox(
            &mut self.session,
            &self.store,
            self.account_id,
            &mut self.state,
        )
        .await
    }

    /// How many messages are in the inbox.
    fn in_inbox(&self) -> usize {
        lock(&self.store)
            .messages()
            .unwrap()
            .iter()
            .filter(|m| m.state == spark_store::MessageState::Inbox)
            .count()
    }

    /// Syncs (or with `older`, reaches back) while the stand-in refuses
    /// commands containing `command` with `status`, and checks that it
    /// failed and left the state and the inbox as they were.
    async fn refused(&mut self, command: &'static str, status: &'static str, older: bool) {
        let before = self.state.clone();
        let inbox = self.in_inbox();
        self.stand_in.refuse(command, status);
        let failed = if older {
            self.older().await.map(drop)
        } else {
            self.sync().await.map(drop)
        };
        self.stand_in.heal();
        assert!(
            failed.is_err(),
            "{command} was refused, yet it went through"
        );
        assert_eq!(self.state, before, "{command}");
        assert_eq!(self.in_inbox(), inbox, "{command}");
    }
}

async fn inbox_scenarios(condstore: bool) -> Vec<(&'static str, Tally)> {
    let oldest_tracked = ON_SERVER - TRACKED + 1;
    let Cached {
        stand_in,
        store,
        account_id,
        mut state,
        mut session,
    } = Cached::old(condstore).await;
    let mut tallies = Vec::new();

    let report = sync_inbox(&mut session, &store, account_id, &mut state)
        .await
        .unwrap();
    assert!(report.new.is_empty() && report.gone == 0, "{report:?}");
    assert_eq!(report.headers, TRACKED as usize, "Reply-To is filled in");
    assert_eq!(
        message(&store, ON_SERVER).reply_to,
        [format!("Desk <desk{ON_SERVER}@x.test>")]
    );
    tallies.push(("old cache, first poll", stand_in.take_tally()));

    assert!(state.reply_to_fetched);
    assert_eq!(state.modseq.is_some(), condstore);

    let report = sync_inbox(&mut session, &store, account_id, &mut state)
        .await
        .unwrap();
    assert_eq!(report, SyncReport::default());
    tallies.push(("quiet poll", stand_in.take_tally()));

    let flagged = ON_SERVER - 10;
    stand_in.with(INBOX, |b| b.set_flags(flagged, false, true));
    let report = sync_inbox(&mut session, &store, account_id, &mut state)
        .await
        .unwrap();
    assert_eq!(report, SyncReport::default());
    let m = message(&store, flagged);
    assert!(m.unread && m.pinned, "the flag change is followed");
    tallies.push(("one flag change", stand_in.take_tally()));

    let expunged = ON_SERVER - 20;
    stand_in.with(INBOX, |b| b.expunge(expunged));
    let report = sync_inbox(&mut session, &store, account_id, &mut state)
        .await
        .unwrap();
    assert_eq!(report.gone, 1, "{report:?}");
    assert_eq!(
        message(&store, expunged).state,
        spark_store::MessageState::Done
    );
    tallies.push(("one expunge", stand_in.take_tally()));

    stand_in.with(INBOX, |b| b.arrive(ON_SERVER + 1));
    let report = sync_inbox(&mut session, &store, account_id, &mut state)
        .await
        .unwrap();
    assert_eq!(report.new.len(), 1, "{report:?}");
    assert!(message(&store, ON_SERVER + 1).unread);
    tallies.push(("one arrival", stand_in.take_tally()));

    let floor = state.floor;
    let older = older_inbox(&mut session, &store, account_id, &mut state)
        .await
        .unwrap();
    assert_eq!(older, OLDER_STEP);
    assert_eq!(
        state.floor,
        Some((oldest_tracked - OLDER_STEP as u32) * SPACING)
    );
    assert!(state.floor < floor && !state.complete);
    tallies.push(("one older page", stand_in.take_tally()));

    // Mail the reach-back brought in is tracked like the rest.
    let reached = oldest_tracked - 1;
    stand_in.with(INBOX, |b| b.set_flags(reached, false, false));
    sync_inbox(&mut session, &store, account_id, &mut state)
        .await
        .unwrap();
    assert!(message(&store, reached).unread);
    stand_in.take_tally();

    stand_in.with(INBOX, Remote::renumber);
    let report = sync_inbox(&mut session, &store, account_id, &mut state)
        .await
        .unwrap();
    assert!(report.first && report.new.is_empty(), "{report:?}");
    assert_eq!(state.uid_validity, Some(2));
    let newest = stand_in.with(INBOX, |b| b.next_uid - 1);
    let first = newest + 1 - FIRST_SYNC_LIMIT as u32;
    assert_eq!(state.floor, Some(first));
    assert!(!state.complete);
    tallies.push(("UIDVALIDITY change", stand_in.take_tally()));

    // Renumbered mail is tracked again: a flag change below the newest is seen.
    stand_in.with(INBOX, |b| b.set_flags(ON_SERVER - 1, true, true));
    sync_inbox(&mut session, &store, account_id, &mut state)
        .await
        .unwrap();
    assert!(message(&store, ON_SERVER - 1).pinned);

    tallies
}

/// Prints the tallies (`--nocapture` shows them) and checks the commands
/// and records of each against `expected`.
fn check(name: &str, tallies: &[(&str, Tally)], expected: &[(&str, usize, usize)]) {
    eprintln!("{name}:");
    for (scenario, t) in tallies {
        eprintln!(
            "  {scenario:<24} {:>2} commands {:>5} records {:>7} bytes",
            t.commands.len(),
            t.records,
            t.bytes,
        );
    }
    let counted: Vec<(&str, usize, usize)> = tallies
        .iter()
        .map(|(scenario, t)| (*scenario, t.commands.len(), t.records))
        .collect();
    assert_eq!(counted, expected, "{name}: {tallies:#?}");
}

/// Stored headers are asked for once, by the first poll of an old cache.
fn headers_asked_once(tallies: &[(&str, Tally)]) {
    for (scenario, t) in tallies {
        let headers = t
            .commands
            .iter()
            .filter(|c| c.contains("HEADER.FIELDS"))
            .count();
        let expected = match *scenario {
            "old cache, first poll" => (TRACKED as usize).div_ceil(OBSERVE_BATCH),
            _ => 0,
        };
        assert_eq!(headers, expected, "{scenario}: {:?}", t.commands);
    }
}

#[tokio::test]
async fn inbox_polls_without_condstore() {
    let tallies = inbox_scenarios(false).await;
    headers_asked_once(&tallies);
    let quiet = &tallies[1].1.commands;
    assert_eq!(
        quiet,
        &[
            "CAPABILITY",
            "SELECT \"INBOX\"",
            "FETCH 1 (UID)",
            "UID SEARCH UID 6003:*",
            "UID FETCH 6003:* (UID FLAGS)"
        ]
    );
    let k = TRACKED as usize;
    check(
        "without CONDSTORE",
        &tallies,
        &[
            // CAPABILITY (for CONDSTORE, every poll), the lowest UID, the
            // tracked UIDs, their headers, their flags.
            ("old cache, first poll", 7, 1 + k + k + k),
            ("quiet poll", 5, 1 + k + k),
            ("one flag change", 5, 1 + k + k),
            ("one expunge", 5, 1 + (k - 1) + (k - 1)),
            // The new one's size and body, and its flags with the rest.
            ("one arrival", 7, 1 + k + 2 + k),
            // Two searches below the floor (200 UIDs and 400, a third of
            // them in the inbox) and 100 sizes and bodies, 25 at a time.
            ("one older page", 12, 1 + 200 + 2 * OLDER_STEP),
            // The newest 300 again, refetched to be found by Message-ID.
            ("UIDVALIDITY change", 27, 3 * FIRST_SYNC_LIMIT),
        ],
    );
}

#[tokio::test]
async fn inbox_polls_with_condstore() {
    let tallies = inbox_scenarios(true).await;
    headers_asked_once(&tallies);
    // The stand-in sends HIGHESTMODSEQ only to a SELECT that turns
    // CONDSTORE on, which CAPABILITY first says it can.
    let first = &tallies[0].1.commands;
    assert_eq!(first[..2], ["CAPABILITY", "SELECT \"INBOX\" (CONDSTORE)"]);
    let quiet = &tallies[1].1.commands;
    assert_eq!(quiet[0], "SELECT \"INBOX\" (CONDSTORE)");
    assert!(quiet[3].starts_with("UID FETCH 6003:* (UID FLAGS) (CHANGEDSINCE "));
    let k = TRACKED as usize;
    check(
        "with CONDSTORE",
        &tallies,
        &[
            // As without; then, with a modseq kept, no more CAPABILITY.
            ("old cache, first poll", 7, 1 + k + k + k),
            // Flags only of what changed.
            ("quiet poll", 4, 1 + k),
            ("one flag change", 4, 1 + k + 1),
            ("one expunge", 4, 1 + (k - 1)),
            ("one arrival", 6, 1 + k + 2 + 1),
            ("one older page", 12, 1 + 200 + 2 * OLDER_STEP),
            ("UIDVALIDITY change", 26, 3 * FIRST_SYNC_LIMIT),
        ],
    );
}

/// With CONDSTORE, mail a search stored below the floor, which nothing
/// tracked, has its flags checked once reaching back passes it.
#[tokio::test]
async fn reaching_back_past_searched_mail_checks_its_flags() {
    let stand_in = StandIn::start(
        HashMap::from([(INBOX.to_owned(), Remote::new(ON_SERVER, SPACING))]),
        true,
    )
    .await;
    let oldest_tracked = ON_SERVER - TRACKED + 1;
    let (store, account_id) = cached(&stand_in, oldest_tracked..=ON_SERVER);
    let mut state = SyncState {
        uid_validity: Some(1),
        floor: Some(oldest_tracked * SPACING),
        ..Default::default()
    };
    let mut session = login(&stand_in.server, "me", "pw").await.unwrap();
    sync_inbox(&mut session, &store, account_id, &mut state)
        .await
        .unwrap();
    assert!(state.modseq.is_some());

    // Stored read by a search; marked unread elsewhere since.
    let searched = oldest_tracked - 5;
    cache(
        &store,
        account_id,
        &stand_in,
        [searched].into_iter(),
        Some(Vec::new()),
    );
    stand_in.with(INBOX, |b| b.set_flags(searched, false, false));
    sync_inbox(&mut session, &store, account_id, &mut state)
        .await
        .unwrap();
    assert!(
        !message(&store, searched).unread,
        "below the floor: not tracked"
    );

    let older = older_inbox(&mut session, &store, account_id, &mut state)
        .await
        .unwrap();
    assert_eq!(older, OLDER_STEP);
    // Passed, not fetched again.
    assert_eq!(state.floor, Some((oldest_tracked - 101) * SPACING));
    assert_eq!(state.modseq, None);
    stand_in.take_tally();
    sync_inbox(&mut session, &store, account_id, &mut state)
        .await
        .unwrap();
    assert!(message(&store, searched).unread);
    let tally = stand_in.take_tally();
    assert!(tally.asked("(UID FLAGS)") && !tally.asked("CHANGEDSINCE"));
    assert!(state.modseq.is_some());
}

#[tokio::test]
async fn folder_polls() {
    const PROJECTS: &str = "Projects";
    let mut projects = Remote::new(FOLDER_LIMIT + 100, 1);
    // Their own ids, apart from the inbox's.
    projects.by_uid = projects
        .by_uid
        .into_iter()
        .map(|(uid, m)| {
            (
                uid,
                Stored {
                    id: m.id + 100_000,
                    ..m
                },
            )
        })
        .collect();
    let stand_in = StandIn::start(
        HashMap::from([
            (INBOX.to_owned(), Remote::new(0, 1)),
            (PROJECTS.to_owned(), projects),
        ]),
        false,
    )
    .await;
    let (store, account_id) = cached(&stand_in, std::iter::empty());
    lock(&store)
        .replace_folders(
            account_id,
            &[RemoteFolder {
                remote_id: PROJECTS.into(),
                parent: None,
                name: PROJECTS.into(),
                role: None,
                selectable: true,
            }],
        )
        .unwrap();
    let folder = lock(&store).folders().unwrap().remove(0);
    let members = || {
        lock(&store)
            .messages()
            .unwrap()
            .into_iter()
            .filter(|m| m.folders.contains(&folder.id))
            .count()
    };
    let mut session = login(&stand_in.server, "me", "pw").await.unwrap();
    let mut state = FolderState::default();
    stand_in.take_tally();
    let mut tallies = Vec::new();

    let report = sync_folder(&mut session, &store, &folder, &mut state)
        .await
        .unwrap();
    assert_eq!(report.changed, FOLDER_LIMIT as usize);
    assert_eq!(members(), FOLDER_LIMIT as usize);
    tallies.push(("first sync", stand_in.take_tally()));

    // A cache from before this change.
    let mut state: FolderState =
        serde_json::from_value(serde_json::json!({ "uidValidity": 1 })).unwrap();
    let report = sync_folder(&mut session, &store, &folder, &mut state)
        .await
        .unwrap();
    assert_eq!(report, FolderReport::default());
    assert!(state.reply_to_fetched);
    tallies.push(("old cache, first poll", stand_in.take_tally()));

    let report = sync_folder(&mut session, &store, &folder, &mut state)
        .await
        .unwrap();
    assert_eq!(report, FolderReport::default());
    tallies.push(("quiet poll", stand_in.take_tally()));

    let newest = 100_000 + FOLDER_LIMIT + 100;
    stand_in.with(PROJECTS, |b| b.set_flags(newest, false, true));
    sync_folder(&mut session, &store, &folder, &mut state)
        .await
        .unwrap();
    let m = message(&store, newest);
    assert!(m.unread && m.pinned);
    tallies.push(("one flag change", stand_in.take_tally()));

    stand_in.with(PROJECTS, |b| b.expunge(newest));
    let report = sync_folder(&mut session, &store, &folder, &mut state)
        .await
        .unwrap();
    // It leaves the list, and the next older one joins it.
    assert_eq!(report.changed, 2, "{report:?}");
    assert_eq!(members(), FOLDER_LIMIT as usize);
    assert!(!message(&store, newest).folders.contains(&folder.id));
    tallies.push(("one expunge", stand_in.take_tally()));

    let quiet = &tallies[2].1.commands;
    assert_eq!(quiet, &["EXAMINE \"Projects\"", "FETCH 101:* (UID FLAGS)"]);
    let headers = |scenario: usize| tallies[scenario].1.asked("HEADER.FIELDS");
    assert!(headers(1) && !headers(2) && !headers(3));
    let window = FOLDER_LIMIT as usize;
    check(
        "folder",
        &tallies,
        &[
            // The window, then headers, sizes and bodies of each 100 in it.
            ("first sync", 14, window * 4),
            // The window, and the headers of what it lists, once.
            ("old cache, first poll", 3, window * 2),
            ("quiet poll", 2, window),
            ("one flag change", 2, window),
            // The one that joins it is fetched as on a first sync.
            ("one expunge", 5, window + 3),
        ],
    );
}

/// A server that turns CONDSTORE away now (it moved to software without
/// it, say) is selected plainly and asked for every flag.
#[tokio::test]
async fn a_server_without_condstore_any_more_is_synced_plainly() {
    let mut cached = Cached::synced(true).await;
    cached.stand_in.state.lock().unwrap().condstore = false;
    let flagged = ON_SERVER - 10;
    cached
        .stand_in
        .with(INBOX, |b| b.set_flags(flagged, false, true));
    cached.sync().await.unwrap();
    assert!(message(&cached.store, flagged).pinned);
    assert_eq!(cached.state.modseq, None);
    let tally = cached.stand_in.take_tally();
    assert_eq!(
        tally.commands[..2],
        ["SELECT \"INBOX\" (CONDSTORE)", "SELECT \"INBOX\""]
    );
    assert!(tally.asked("(UID FLAGS)") && !tally.asked("CHANGEDSINCE"));

    cached.sync().await.unwrap();
    let tally = cached.stand_in.take_tally();
    assert_eq!(tally.commands[..2], ["CAPABILITY", "SELECT \"INBOX\""]);
}

/// A first sync whose FETCH is refused keeps no position: the next, with
/// the server well again, is a first sync too, and takes the whole inbox.
#[tokio::test]
async fn a_refused_first_sync_keeps_no_position() {
    let stand_in = StandIn::start(
        HashMap::from([(INBOX.to_owned(), Remote::new(100, 1))]),
        false,
    )
    .await;
    let (store, account_id) = cached(&stand_in, std::iter::empty());
    let mut state = SyncState::default();
    let mut session = login(&stand_in.server, "me", "pw").await.unwrap();

    stand_in.refuse("FETCH 1:* (UID)", "NO");
    let refused = sync_inbox(&mut session, &store, account_id, &mut state).await;
    assert!(refused.is_err(), "{refused:?}");
    let renumbered = SyncState {
        uid_validity: Some(1),
        ..Default::default()
    };
    assert_eq!(state, renumbered);
    assert!(lock(&store).messages().unwrap().is_empty());

    stand_in.heal();
    let report = sync_inbox(&mut session, &store, account_id, &mut state)
        .await
        .unwrap();
    assert!(report.first && report.new.len() == 100, "{report:?}");
    assert_eq!((state.floor, state.complete), (Some(1), true));
}

/// A refused check for the lowest UID doesn't take the inbox for complete.
#[tokio::test]
async fn a_refused_lowest_uid_check_leaves_the_inbox_incomplete() {
    let mut cached = Cached::synced(false).await;
    let before = cached.state.clone();
    assert!(!before.complete);
    cached.refused("FETCH 1 (UID)", "NO", false).await;
    cached.sync().await.unwrap();
    assert_eq!(cached.state, before);
}

/// Mail a refused search didn't list isn't taken for gone.
#[tokio::test]
async fn a_refused_search_takes_no_mail_for_gone() {
    let mut cached = Cached::synced(false).await;
    cached.refused("UID SEARCH UID", "NO", false).await;
    let report = cached.sync().await.unwrap();
    assert_eq!(report.gone, 0);
    assert_eq!(cached.in_inbox(), TRACKED as usize);
}

/// The modseq stays put when CHANGEDSINCE is refused, so the next sync
/// asks for the same changes again.
#[tokio::test]
async fn a_refused_changedsince_keeps_the_modseq() {
    let mut cached = Cached::synced(true).await;
    let changed = [ON_SERVER - 10, ON_SERVER - 20];
    for id in changed {
        cached
            .stand_in
            .with(INBOX, |b| b.set_flags(id, false, true));
    }
    cached.refused("CHANGEDSINCE", "BAD", false).await;
    cached.stand_in.take_tally();
    cached.sync().await.unwrap();
    assert!(cached.stand_in.take_tally().asked("CHANGEDSINCE"));
    for id in changed {
        let m = message(&cached.store, id);
        assert!(m.unread && m.pinned, "{id}");
    }
}

/// A refused fetch of stored mail's headers leaves Reply-To to fetch.
#[tokio::test]
async fn a_refused_header_fetch_leaves_reply_to_to_fetch() {
    let mut cached = Cached::old(false).await;
    cached.refused("HEADER.FIELDS", "NO", false).await;
    let report = cached.sync().await.unwrap();
    assert_eq!(report.headers, TRACKED as usize);
    assert!(cached.state.reply_to_fetched);
    assert_eq!(
        message(&cached.store, ON_SERVER).reply_to,
        [format!("Desk <desk{ON_SERVER}@x.test>")]
    );
}

/// Reaching back past UIDs a refused search didn't list neither passes
/// them nor takes the inbox for complete.
#[tokio::test]
async fn a_refused_search_below_the_floor_reaches_back_no_further() {
    let mut cached = Cached::synced(false).await;
    cached.refused("UID SEARCH UID", "NO", true).await;
    assert_eq!(cached.older().await.unwrap(), OLDER_STEP);
    let oldest_tracked = ON_SERVER - TRACKED + 1;
    assert_eq!(
        cached.state.floor,
        Some((oldest_tracked - OLDER_STEP as u32) * SPACING)
    );
    assert!(!cached.state.complete);
}

/// Mail whose body fetch is refused isn't passed by reaching back.
#[tokio::test]
async fn a_refused_body_fetch_drops_no_mail() {
    let mut cached = Cached::synced(false).await;
    cached.refused("BODY.PEEK[]", "NO", true).await;
    assert_eq!(cached.older().await.unwrap(), OLDER_STEP);
    assert_eq!(cached.in_inbox(), TRACKED as usize + OLDER_STEP);
    let oldest_tracked = ON_SERVER - TRACKED + 1;
    assert_eq!(
        cached.state.floor,
        Some((oldest_tracked - OLDER_STEP as u32) * SPACING)
    );
}
