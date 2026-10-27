//! Syncs against a scripted IMAP server that counts what they ask for:
//! commands, records (FETCH responses and SEARCH hits) and bytes sent back.

use std::collections::{BTreeMap, HashMap, HashSet};
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

    /// The fetches of stored mail's headers, and of new mail's in a folder:
    /// not those of older mail reaching back fetches without its body.
    fn header_fetches(&self) -> Vec<&str> {
        self.commands
            .iter()
            .map(String::as_str)
            .filter(|c| c.contains("HEADER.FIELDS") && !c.contains("BODYSTRUCTURE"))
            .collect()
    }
}

struct Shared {
    mailboxes: HashMap<String, Remote>,
    condstore: bool,
    /// Commands containing the first get the first half of their answer,
    /// then the second (NO or BAD) as their completion.
    refuse: Option<(&'static str, &'static str)>,
    /// Messages (by id) left out of FETCHes containing their text, which
    /// then end in NO: as Gmail answers for mail it can't fetch.
    broken: HashMap<u32, &'static str>,
    /// Messages (by id) whose body comes as NIL, in a FETCH that ends OK.
    bodiless: HashSet<u32>,
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
            broken: HashMap::new(),
            bodiless: HashSet::new(),
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

    /// Leaves message `id` out of FETCHes containing `fetch` from now on,
    /// even of it alone, and ends them in NO.
    fn break_message(&self, id: u32, fetch: &'static str) {
        self.state.lock().unwrap().broken.insert(id, fetch);
    }

    /// Answers message `id`'s body as NIL from now on, and the fetch OK.
    fn without_body(&self, id: u32) {
        self.state.lock().unwrap().bodiless.insert(id);
    }

    fn heal(&self) {
        let mut state = self.state.lock().unwrap();
        state.refuse = None;
        state.broken.clear();
        state.bodiless.clear();
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
    let mut status = refused.unwrap_or("OK");
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
        if state.broken.get(&m.id).is_some_and(|f| command.contains(f)) {
            status = "NO";
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
        let text = &raw[raw.find("\r\n\r\n").unwrap() + 4..];
        if items.contains("BODYSTRUCTURE") {
            parts.push(format!(
                "BODYSTRUCTURE (\"TEXT\" \"PLAIN\" (\"CHARSET\" \"us-ascii\") NIL NIL \"7BIT\" {} 1 \
                 NIL NIL NIL NIL)",
                text.len()
            ));
        }
        if items.contains("BODY.PEEK[TEXT]<0.") {
            parts.push(format!("BODY[TEXT]<0> {{{}}}\r\n{text}", text.len()));
        }
        if items.contains("BODY.PEEK[]") && state.bodiless.contains(&m.id) {
            parts.push("BODY[] NIL".into());
        } else if items.contains("BODY.PEEK[]") {
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
    stored(store, id).unwrap()
}

fn stored(store: &Mutex<Store>, id: u32) -> Option<Message> {
    let subject = format!("Message {id}");
    lock(store)
        .messages()
        .unwrap()
        .into_iter()
        .find(|m| m.subject == subject)
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
    // It came without its body, and lists as if it were whole.
    let older = message(&store, oldest_tracked - 1);
    assert_eq!(
        older.snippet,
        format!("Body of message {}.", oldest_tracked - 1)
    );
    assert!(
        lock(&store)
            .content(older.id)
            .unwrap()
            .unwrap()
            .body_missing
    );

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
        let headers = t.header_fetches().len();
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
    // Nothing came or went: the tracked UIDs aren't listed again.
    let quiet = &tallies[1].1.commands;
    assert_eq!(
        quiet,
        &[
            "CAPABILITY",
            "SELECT \"INBOX\"",
            "FETCH 1 (UID)",
            "UID FETCH 6003:* (UID FLAGS)"
        ]
    );
    // Mail went: they are. Mail came: only the new ones are.
    assert!(tallies[3].1.asked("UID SEARCH UID 6003:*"));
    assert!(
        tallies[4]
            .1
            .asked(&format!("UID SEARCH UID {}:*", ON_SERVER * SPACING + 1))
    );
    let k = TRACKED as usize;
    check(
        "without CONDSTORE",
        &tallies,
        &[
            // CAPABILITY (for CONDSTORE, every poll), the lowest UID, the
            // tracked UIDs, their headers, their flags.
            ("old cache, first poll", 7, 1 + k + k + k),
            ("quiet poll", 4, 1 + k),
            ("one flag change", 4, 1 + k),
            ("one expunge", 5, 1 + (k - 1) + (k - 1)),
            // The new one's UID, size and body, and its flags with the rest.
            ("one arrival", 7, 1 + 1 + 2 + k),
            // Two searches below the floor (200 UIDs and 400, a third of
            // them in the inbox) and 100 heads, without their bodies.
            ("one older page", 5, 1 + 200 + OLDER_STEP),
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
    assert!(quiet[2].starts_with("UID FETCH 6003:* (UID FLAGS) (CHANGEDSINCE "));
    let k = TRACKED as usize;
    check(
        "with CONDSTORE",
        &tallies,
        &[
            // As without; then, with a modseq kept, no more CAPABILITY.
            ("old cache, first poll", 7, 1 + k + k + k),
            // Flags only of what changed.
            ("quiet poll", 3, 1),
            ("one flag change", 3, 1 + 1),
            ("one expunge", 4, 1 + (k - 1)),
            ("one arrival", 6, 1 + 1 + 2 + 1),
            ("one older page", 5, 1 + 200 + OLDER_STEP),
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

const PROJECTS: &str = "Projects";

/// A stand-in whose inbox is empty and whose Projects folder has
/// `FOLDER_LIMIT + 100` messages, their ids from 100,001; a store that
/// has the folder; and a session.
async fn projects() -> (StandIn, Mutex<Store>, Folder, Session) {
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
    let session = login(&stand_in.server, "me", "pw").await.unwrap();
    stand_in.take_tally();
    (stand_in, store, folder, session)
}

/// How many messages `folder` lists.
fn listed_in(store: &Mutex<Store>, folder: &Folder) -> usize {
    lock(store)
        .messages()
        .unwrap()
        .into_iter()
        .filter(|m| m.folders.contains(&folder.id))
        .count()
}

#[tokio::test]
async fn folder_polls() {
    let (stand_in, store, folder, mut session) = projects().await;
    let members = || listed_in(&store, &folder);
    let mut state = FolderState::default();
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
    // Mail left, so the sync lists what's there.
    cached.stand_in.with(INBOX, |b| b.expunge(ON_SERVER - 1));
    cached.refused("UID SEARCH UID", "NO", false).await;
    let report = cached.sync().await.unwrap();
    assert_eq!(report.gone, 1);
    assert_eq!(cached.in_inbox(), TRACKED as usize - 1);
}

/// Mail that came as other mail went leaves the count as it was, but not
/// UIDNEXT: the sync lists the inbox whole, and sees both.
#[tokio::test]
async fn mail_that_comes_as_other_mail_goes_is_seen() {
    let mut cached = Cached::synced(true).await;
    cached.stand_in.with(INBOX, |b| {
        b.expunge(ON_SERVER - 1);
        b.arrive(ON_SERVER + 1);
    });
    let report = cached.sync().await.unwrap();
    assert_eq!((report.new.len(), report.gone), (1, 1), "{report:?}");
    assert!(cached.stand_in.take_tally().asked("UID SEARCH UID 6003:*"));
    // Then nothing came or went: it isn't listed again.
    let report = cached.sync().await.unwrap();
    assert_eq!(report, SyncReport::default());
    assert!(!cached.stand_in.take_tally().asked("UID SEARCH"));
    // Reaching back lowers the floor: the next sync lists it from there.
    cached.older().await.unwrap();
    cached.stand_in.take_tally();
    cached.sync().await.unwrap();
    let floor = cached.state.floor.unwrap();
    assert!(
        cached
            .stand_in
            .take_tally()
            .asked(&format!("UID SEARCH UID {floor}:*"))
    );
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

/// A refused fetch of stored mail's headers leaves Reply-To to fetch: of
/// the state, only what's still to fetch is kept (all of it).
/// (Refused with BAD: a NO holds back only the messages it left out, as
/// `reply_to_refused_for_a_while_is_fetched_once_the_server_recovers`
/// shows.)
#[tokio::test]
async fn a_refused_header_fetch_leaves_reply_to_to_fetch() {
    let mut cached = Cached::old(false).await;
    let before = cached.state.clone();
    cached.stand_in.refuse("HEADER.FIELDS", "BAD");
    let refused = cached.sync().await;
    cached.stand_in.heal();
    assert!(refused.is_err(), "{refused:?}");
    let pending = std::mem::take(&mut cached.state.reply_to_pending);
    assert_eq!(cached.state, before);
    assert_eq!(pending.len(), TRACKED as usize);
    cached.state.reply_to_pending = pending;

    let report = cached.sync().await.unwrap();
    assert_eq!(report.headers, TRACKED as usize);
    assert!(cached.state.reply_to_fetched);
    assert!(cached.state.reply_to_pending.is_empty());
    assert_eq!(
        message(&cached.store, ON_SERVER).reply_to,
        [format!("Desk <desk{ON_SERVER}@x.test>")]
    );
}

/// A provider that briefly refuses the Reply-To backfill (each batch cut
/// short, then what it held back refused even alone) fails nothing: the
/// round goes on to flags, what came is stored, and the rest stays to
/// fetch. The next round asks for only that, and the backfill is done.
#[tokio::test]
async fn reply_to_refused_for_a_while_is_fetched_once_the_server_recovers() {
    let mut cached = Cached::old(false).await;
    let flagged = ON_SERVER - 10;
    cached
        .stand_in
        .with(INBOX, |b| b.set_flags(flagged, false, true));
    cached.stand_in.refuse("HEADER.FIELDS", "NO");
    let report = cached.sync().await.unwrap();
    cached.stand_in.heal();
    // Each batch of 500 sends its first half.
    let held = TRACKED as usize / 2;
    assert_eq!(report.headers, held);
    assert!(message(&cached.store, flagged).pinned);
    assert!(!cached.state.reply_to_fetched);
    let pending = cached.state.reply_to_pending.clone();
    assert_eq!(pending.len(), held);
    assert!(pending.contains(&(ON_SERVER * SPACING)));
    assert!(message(&cached.store, ON_SERVER).reply_to.is_empty());
    let tally = cached.stand_in.take_tally();
    let headers = tally.header_fetches();
    assert_eq!(headers.len(), 2 + held, "the batches, then each held alone");
    // Kept as JSON between rounds.
    let saved = serde_json::to_string(&cached.state).unwrap();
    cached.state = serde_json::from_str(&saved).unwrap();
    assert_eq!(cached.state.reply_to_pending, pending);

    let report = cached.sync().await.unwrap();
    assert_eq!(report.headers, held);
    assert_eq!(
        message(&cached.store, ON_SERVER).reply_to,
        [format!("Desk <desk{ON_SERVER}@x.test>")]
    );
    assert!(cached.state.reply_to_fetched);
    assert!(cached.state.reply_to_pending.is_empty());
    let asked = format!("UID FETCH {} (UID {REPLY_TO_HEADERS})", uid_set(&pending));
    assert_eq!(
        cached.stand_in.take_tally().header_fetches(),
        [asked.as_str()]
    );

    cached.sync().await.unwrap();
    assert!(!cached.stand_in.take_tally().asked("HEADER.FIELDS"));
}

/// What's still to fetch is in UIDs the server numbered anew: forgotten
/// with them.
#[tokio::test]
async fn a_uidvalidity_change_forgets_the_reply_to_still_to_fetch() {
    let mut cached = Cached::old(false).await;
    cached.stand_in.refuse("HEADER.FIELDS", "NO");
    cached.sync().await.unwrap();
    cached.stand_in.heal();
    assert!(!cached.state.reply_to_pending.is_empty());

    cached.stand_in.with(INBOX, Remote::renumber);
    cached.stand_in.take_tally();
    let report = cached.sync().await.unwrap();
    assert!(report.first, "{report:?}");
    assert!(cached.state.reply_to_fetched);
    assert!(cached.state.reply_to_pending.is_empty());
    assert!(!cached.stand_in.take_tally().asked("HEADER.FIELDS"));
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

/// Mail whose fetch is refused isn't passed by reaching back.
/// (Refused with BAD: for a NO, see
/// `a_broken_message_reaching_back_is_left_to_the_inbox_sync`.)
#[tokio::test]
async fn a_refused_head_fetch_drops_no_mail() {
    let mut cached = Cached::synced(false).await;
    cached.refused("BODYSTRUCTURE", "BAD", true).await;
    assert_eq!(cached.older().await.unwrap(), OLDER_STEP);
    assert_eq!(cached.in_inbox(), TRACKED as usize + OLDER_STEP);
    let oldest_tracked = ON_SERVER - TRACKED + 1;
    assert_eq!(
        cached.state.floor,
        Some((oldest_tracked - OLDER_STEP as u32) * SPACING)
    );
}

/// One new message the server can't fetch holds back only itself: the
/// rest is stored, the round goes on to flags and gone mail, and each
/// round after asks for it again, until it comes.
#[tokio::test]
async fn a_broken_new_message_holds_back_only_itself() {
    let mut cached = Cached::synced(true).await;
    let arrivals = ON_SERVER + 1..=ON_SERVER + 3;
    let broken = ON_SERVER + 2;
    let (flagged, expunged) = (ON_SERVER - 10, ON_SERVER - 20);
    cached.stand_in.with(INBOX, |b| {
        for id in arrivals.clone() {
            b.arrive(id);
        }
        b.set_flags(flagged, false, true);
        b.expunge(expunged);
    });
    cached.stand_in.break_message(broken, "BODY.PEEK[]");
    let floor = cached.state.floor;
    let alone = format!(
        "UID FETCH {} (UID FLAGS INTERNALDATE BODY.PEEK[])",
        cached.stand_in.with(INBOX, |b| b.uid_of(broken))
    );

    let report = cached.sync().await.unwrap();
    assert_eq!(report.new.len(), 2, "{report:?}");
    assert_eq!(report.skipped, 1);
    assert!(stored(&cached.store, ON_SERVER + 1).is_some());
    assert!(stored(&cached.store, ON_SERVER + 3).is_some());
    assert!(stored(&cached.store, broken).is_none());
    assert_eq!(report.gone, 1);
    assert!(message(&cached.store, flagged).pinned);
    let modseq = cached.stand_in.with(INBOX, |b| b.modseq);
    assert_eq!(cached.state.modseq, Some(modseq));
    assert_eq!(cached.state.floor, floor);
    let tally = cached.stand_in.take_tally();
    assert_eq!(
        tally.commands.iter().filter(|c| **c == alone).count(),
        1,
        "{:?}",
        tally.commands
    );

    // Still new, and still broken: asked for once, alone.
    let report = cached.sync().await.unwrap();
    assert!(report.new.is_empty(), "{report:?}");
    assert_eq!(report.skipped, 1);
    let tally = cached.stand_in.take_tally();
    assert_eq!(
        tally.commands.iter().filter(|c| **c == alone).count(),
        1,
        "{:?}",
        tally.commands
    );

    cached.stand_in.heal();
    let report = cached.sync().await.unwrap();
    assert_eq!(report.new.len(), 1, "{report:?}");
    assert_eq!(report.skipped, 0);
    assert!(message(&cached.store, broken).unread);
    assert_eq!(cached.in_inbox(), TRACKED as usize - 1 + 3);
    assert_eq!(cached.state.floor, floor);
}

/// A lone new message (as IDLE reports one) that the server won't give,
/// its size or its body, is counted as skipped, so the app asks again soon.
#[tokio::test]
async fn a_lone_new_message_refused_is_counted_as_skipped() {
    for fetch in ["RFC822.SIZE", "BODY.PEEK[]"] {
        let mut cached = Cached::synced(false).await;
        let id = ON_SERVER + 1;
        cached.stand_in.with(INBOX, |b| b.arrive(id));
        cached.stand_in.break_message(id, fetch);
        let report = cached.sync().await.unwrap();
        assert!(report.new.is_empty(), "{fetch}: {report:?}");
        assert_eq!(report.skipped, 1, "{fetch}");
        assert!(stored(&cached.store, id).is_none(), "{fetch}");

        cached.stand_in.heal();
        let report = cached.sync().await.unwrap();
        assert_eq!((report.new.len(), report.skipped), (1, 0), "{fetch}");
    }
}

/// A new message the server gives without its body, in a fetch that ends
/// OK, isn't stored, and the next sync asks for it again even though
/// nothing came or went since.
#[tokio::test]
async fn mail_that_came_without_its_body_is_asked_for_again() {
    let mut cached = Cached::synced(true).await;
    let id = ON_SERVER + 1;
    cached.stand_in.with(INBOX, |b| b.arrive(id));
    cached.stand_in.without_body(id);
    let report = cached.sync().await.unwrap();
    assert!(report.new.is_empty() && report.skipped == 0, "{report:?}");
    assert!(stored(&cached.store, id).is_none());
    assert_eq!(cached.state.seen, None);

    cached.stand_in.heal();
    cached.stand_in.take_tally();
    let report = cached.sync().await.unwrap();
    assert_eq!(report.new.len(), 1, "{report:?}");
    assert!(stored(&cached.store, id).is_some());
    assert!(cached.stand_in.take_tally().asked("UID SEARCH UID 6003:*"));
    assert!(cached.state.seen.is_some());
}

/// A message reaching back can't fetch is passed like the rest, but it's
/// above the new floor: the inbox sync asks for it from then on, as new
/// mail, until it comes. (As before IM-1, which didn't notice the NO.)
#[tokio::test]
async fn a_broken_message_reaching_back_is_left_to_the_inbox_sync() {
    let mut cached = Cached::synced(false).await;
    let oldest_tracked = ON_SERVER - TRACKED + 1;
    let broken = oldest_tracked - 3;
    // Its head, and its body when the sync asks for it.
    cached.stand_in.break_message(broken, "INTERNALDATE");
    assert_eq!(cached.older().await.unwrap(), OLDER_STEP - 1);
    let floor = (oldest_tracked - OLDER_STEP as u32) * SPACING;
    assert_eq!(cached.state.floor, Some(floor));
    assert!(!cached.state.complete);
    assert!(stored(&cached.store, broken).is_none());
    let head_alone = format!("UID FETCH {} {}", broken * SPACING, Heads::Full.query());
    assert!(cached.stand_in.take_tally().commands.contains(&head_alone));

    let alone = format!(
        "UID FETCH {} (UID FLAGS INTERNALDATE BODY.PEEK[])",
        broken * SPACING
    );
    let report = cached.sync().await.unwrap();
    assert!(report.new.is_empty(), "{report:?}");
    assert!(cached.stand_in.take_tally().commands.contains(&alone));

    cached.stand_in.heal();
    let report = cached.sync().await.unwrap();
    assert_eq!(report.new.len(), 1, "{report:?}");
    assert_eq!(cached.in_inbox(), TRACKED as usize + OLDER_STEP);
    assert_eq!(cached.state.floor, Some(floor));
}

/// A message whose headers the server can't fetch doesn't hold up the
/// Reply-To backfill of the rest, or the round: it stays to fetch, and
/// each round asks for it alone (and nothing else) until it comes.
#[tokio::test]
async fn a_broken_message_in_the_reply_to_backfill_holds_back_only_itself() {
    let mut cached = Cached::old(false).await;
    let broken = ON_SERVER - 5;
    let flagged = ON_SERVER - 10;
    cached
        .stand_in
        .with(INBOX, |b| b.set_flags(flagged, false, true));
    cached.stand_in.break_message(broken, "HEADER.FIELDS");
    let report = cached.sync().await.unwrap();
    assert_eq!(report.headers, TRACKED as usize - 1);
    assert!(!cached.state.reply_to_fetched);
    assert_eq!(cached.state.reply_to_pending, [broken * SPACING]);
    assert!(message(&cached.store, broken).reply_to.is_empty());
    assert_eq!(
        message(&cached.store, ON_SERVER).reply_to,
        [format!("Desk <desk{ON_SERVER}@x.test>")]
    );
    assert!(message(&cached.store, flagged).pinned);
    let alone = format!("UID FETCH {} (UID {REPLY_TO_HEADERS})", broken * SPACING);
    assert!(cached.stand_in.take_tally().commands.contains(&alone));

    cached.sync().await.unwrap();
    assert_eq!(
        cached.stand_in.take_tally().header_fetches(),
        [alone.as_str()]
    );
    assert_eq!(cached.state.reply_to_pending, [broken * SPACING]);

    cached.stand_in.heal();
    let report = cached.sync().await.unwrap();
    assert_eq!(report.headers, 1);
    assert_eq!(
        message(&cached.store, broken).reply_to,
        [format!("Desk <desk{broken}@x.test>")]
    );
    assert!(cached.state.reply_to_fetched);
    assert!(cached.state.reply_to_pending.is_empty());
    assert_eq!(
        cached.stand_in.take_tally().header_fetches(),
        [alone.as_str()]
    );

    cached.sync().await.unwrap();
    assert!(!cached.stand_in.take_tally().asked("HEADER.FIELDS"));
}

/// A message expunged while its Reply-To is still to fetch is no longer
/// asked for: the fetch finds it gone (an OK without it).
#[tokio::test]
async fn expunged_mail_leaves_the_reply_to_still_to_fetch() {
    let mut cached = Cached::old(false).await;
    let (first, second) = (ON_SERVER - 5, ON_SERVER - 6);
    for id in [first, second] {
        cached.stand_in.break_message(id, "HEADER.FIELDS");
    }
    cached.sync().await.unwrap();
    cached.stand_in.heal();
    assert_eq!(
        cached.state.reply_to_pending,
        [second * SPACING, first * SPACING]
    );

    // Gone before the fetch: an OK without it.
    cached.stand_in.with(INBOX, |b| b.expunge(first));
    cached.sync().await.unwrap();
    assert!(cached.state.reply_to_fetched);
    assert!(cached.state.reply_to_pending.is_empty());
    assert_eq!(
        message(&cached.store, second).reply_to,
        [format!("Desk <desk{second}@x.test>")]
    );
    assert_eq!(
        message(&cached.store, first).state,
        spark_store::MessageState::Done
    );
}

/// A flags fetch the server cuts short keeps the flags that came, and
/// reads nothing into the ones that didn't: no mail gone, no flag changed.
#[tokio::test]
async fn a_flags_fetch_cut_short_keeps_the_flags_that_came() {
    let mut cached = Cached::synced(false).await;
    let (came, broken) = (ON_SERVER - 10, ON_SERVER - 20);
    for id in [came, broken] {
        cached
            .stand_in
            .with(INBOX, |b| b.set_flags(id, false, true));
    }
    cached.stand_in.break_message(broken, "(UID FLAGS)");
    let report = cached.sync().await.unwrap();
    assert_eq!(report, SyncReport::default());
    assert!(message(&cached.store, came).pinned);
    let m = message(&cached.store, broken);
    assert!(!m.unread && !m.pinned);
    assert_eq!(cached.in_inbox(), TRACKED as usize);

    cached.stand_in.heal();
    cached.sync().await.unwrap();
    let m = message(&cached.store, broken);
    assert!(m.unread && m.pinned);
}

/// A folder's list the server cuts short keeps the flags that came, and
/// takes nothing out of the folder for what didn't.
#[tokio::test]
async fn a_folder_list_cut_short_takes_nothing_out_of_it() {
    let (stand_in, store, folder, mut session) = projects().await;
    let mut state = FolderState::default();
    sync_folder(&mut session, &store, &folder, &mut state)
        .await
        .unwrap();
    let newest = 100_000 + FOLDER_LIMIT + 100;
    let (flagged, expunged, broken) = (newest, newest - 1, newest - 2);
    stand_in.with(PROJECTS, |b| {
        b.set_flags(flagged, false, true);
        b.expunge(expunged);
    });
    stand_in.break_message(broken, "(UID FLAGS)");
    let report = sync_folder(&mut session, &store, &folder, &mut state)
        .await
        .unwrap();
    // The next older one joins the window; the expunged one stays listed.
    assert_eq!(report.changed, 1, "{report:?}");
    assert_eq!(listed_in(&store, &folder), FOLDER_LIMIT as usize + 1);
    assert!(message(&store, flagged).pinned);
    assert!(message(&store, broken).folders.contains(&folder.id));

    stand_in.heal();
    let report = sync_folder(&mut session, &store, &folder, &mut state)
        .await
        .unwrap();
    assert_eq!(report.changed, 1, "{report:?}");
    assert_eq!(listed_in(&store, &folder), FOLDER_LIMIT as usize);
    assert!(!message(&store, expunged).folders.contains(&folder.id));
}

/// A folder whose list always ends in NO (a damaged message in it) still
/// fetches the Reply-To of the stored mail it lists. Only the damaged one
/// stays to fetch, and it isn't asked for until a list has it.
#[tokio::test]
async fn a_damaged_message_holds_back_only_its_own_reply_to_in_a_folder() {
    let (stand_in, store, folder, mut session) = projects().await;
    // The window, stored before Reply-To was kept.
    let window: Vec<Incoming> = stand_in.with(PROJECTS, |b| {
        b.by_uid
            .iter()
            .rev()
            .take(FOLDER_LIMIT as usize)
            .map(|(uid, m)| {
                let mut stored = parse::parse_message(raw(m.id).as_bytes()).unwrap();
                stored.account_id = folder.account_id;
                stored.mailbox = Some(PROJECTS.into());
                stored.uid = Some(*uid);
                stored.reply_to = None;
                stored
            })
            .collect()
    });
    add_to_folder(&store, folder.id, &window).unwrap();
    let mut state: FolderState =
        serde_json::from_value(serde_json::json!({ "uidValidity": 1 })).unwrap();
    let newest = 100_000 + FOLDER_LIMIT + 100;
    let damaged = newest - 2;
    let damaged_uid = stand_in.with(PROJECTS, |b| b.uid_of(damaged));
    stand_in.break_message(damaged, "(UID FLAGS)");
    let reply_to = |id: u32| message(&store, id).reply_to;
    let desk = |id: u32| [format!("Desk <desk{id}@x.test>")];

    let report = sync_folder(&mut session, &store, &folder, &mut state)
        .await
        .unwrap();
    assert_eq!(report.changed, FOLDER_LIMIT as usize - 1, "{report:?}");
    assert_eq!(reply_to(newest), desk(newest));
    assert!(reply_to(damaged).is_empty());
    assert!(!state.reply_to_fetched);
    assert_eq!(state.reply_to_pending, [damaged_uid]);
    assert_eq!(listed_in(&store, &folder), FOLDER_LIMIT as usize);
    stand_in.take_tally();

    // Still left out of the list: not asked for.
    sync_folder(&mut session, &store, &folder, &mut state)
        .await
        .unwrap();
    assert!(stand_in.take_tally().header_fetches().is_empty());
    assert_eq!(state.reply_to_pending, [damaged_uid]);

    stand_in.heal();
    sync_folder(&mut session, &store, &folder, &mut state)
        .await
        .unwrap();
    assert_eq!(reply_to(damaged), desk(damaged));
    assert!(state.reply_to_fetched);
    assert!(state.reply_to_pending.is_empty());
    let alone = format!("UID FETCH {damaged_uid} (UID {REPLY_TO_HEADERS})");
    assert_eq!(stand_in.take_tally().header_fetches(), [alone.as_str()]);

    sync_folder(&mut session, &store, &folder, &mut state)
        .await
        .unwrap();
    assert!(!stand_in.take_tally().asked("HEADER.FIELDS"));
}

/// A CHANGEDSINCE the server cuts short keeps the flags that came, but not
/// the modseq: the next sync asks from the same one, and gets the rest.
#[tokio::test]
async fn a_changedsince_cut_short_keeps_the_modseq() {
    let mut cached = Cached::synced(true).await;
    let since = cached.state.modseq.unwrap();
    let (came, broken) = (ON_SERVER - 10, ON_SERVER - 20);
    for id in [came, broken] {
        cached
            .stand_in
            .with(INBOX, |b| b.set_flags(id, false, true));
    }
    cached.stand_in.break_message(broken, "CHANGEDSINCE");
    cached.sync().await.unwrap();
    assert!(message(&cached.store, came).pinned);
    assert!(!message(&cached.store, broken).pinned);
    assert_eq!(cached.state.modseq, Some(since));

    cached.stand_in.heal();
    cached.stand_in.take_tally();
    cached.sync().await.unwrap();
    assert!(
        cached
            .stand_in
            .take_tally()
            .asked(&format!("(CHANGEDSINCE {since})"))
    );
    assert!(message(&cached.store, broken).pinned);
    let modseq = cached.stand_in.with(INBOX, |b| b.modseq);
    assert_eq!(cached.state.modseq, Some(modseq));
}

/// A NO for one message still fails the fetches taken for all there is:
/// the first sync's UIDs (`FETCH 1:*`) and the lowest UID.
#[tokio::test]
async fn a_broken_message_still_fails_the_uid_fetches() {
    let stand_in = StandIn::start(
        HashMap::from([(INBOX.to_owned(), Remote::new(100, 1))]),
        false,
    )
    .await;
    let (store, account_id) = cached(&stand_in, std::iter::empty());
    let mut state = SyncState::default();
    let mut session = login(&stand_in.server, "me", "pw").await.unwrap();
    stand_in.break_message(1, "FETCH 1:* (UID)");
    let refused = sync_inbox(&mut session, &store, account_id, &mut state).await;
    assert!(refused.is_err(), "{refused:?}");
    assert_eq!((state.floor, state.complete), (None, false));
    assert!(lock(&store).messages().unwrap().is_empty());
    stand_in.heal();
    let report = sync_inbox(&mut session, &store, account_id, &mut state)
        .await
        .unwrap();
    assert!(report.first && report.new.len() == 100, "{report:?}");

    let mut cached = Cached::synced(false).await;
    let before = cached.state.clone();
    let inbox = cached.in_inbox();
    cached.stand_in.break_message(1, "FETCH 1 (UID)");
    let refused = cached.sync().await;
    assert!(refused.is_err(), "{refused:?}");
    assert_eq!(cached.state, before);
    assert_eq!(cached.in_inbox(), inbox);
}
