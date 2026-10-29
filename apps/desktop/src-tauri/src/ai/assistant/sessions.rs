//! Saved chats: one JSON file each, holding what the model saw (the turns,
//! with tool results and thinking blocks, so a chat can go on where it left
//! off) and what the chat shows (the entries).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use spark_ai::{Part, Role, Turn};

use super::tools::{Card, CardState, EmailRef};

/// Older chats beyond this many are deleted.
const KEEP: usize = 50;
/// Titles are the first question, cut here.
const TITLE_CHARS: usize = 80;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    pub id: String,
    pub title: String,
    pub created: i64,
    pub updated: i64,
    /// Fixed for the chat: a changed system prompt would void the model's
    /// earlier thinking.
    pub system: String,
    pub turns: Vec<Turn>,
    pub entries: Vec<Entry>,
}

/// A line of the chat as the panel shows it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "camelCase")]
pub enum Entry {
    User { text: String },
    Assistant { answer: Answer },
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Answer {
    /// Text and cards, in the order they came.
    pub blocks: Vec<Block>,
    /// What it did along the way, e.g. "Checking your emails".
    pub steps: Vec<String>,
    /// The emails the tools returned, for the citations in the text.
    pub emails: Vec<EmailRef>,
    pub status: Status,
    /// Why it failed or was refused, in words.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Block {
    Text { text: String },
    Card { card: Card },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Status {
    #[default]
    Working,
    Done,
    /// Stopped at the length limit.
    Truncated,
    /// The user stopped it.
    Stopped,
    /// The provider declined.
    Refused,
    Failed,
}

/// Where an answer's text stood, to go back to after a refusal.
pub struct Mark {
    blocks: usize,
    text: usize,
}

impl Answer {
    pub fn push_text(&mut self, text: &str) {
        match self.blocks.last_mut() {
            Some(Block::Text { text: last }) => last.push_str(text),
            _ => self.blocks.push(Block::Text {
                text: text.to_owned(),
            }),
        }
    }

    pub fn push_card(&mut self, card: Card) {
        self.blocks.push(Block::Card { card });
    }

    /// Replaces the card with the same id.
    pub fn update_card(&mut self, card: &Card) {
        for block in &mut self.blocks {
            if let Block::Card { card: old } = block
                && old.id() == card.id()
            {
                *old = card.clone();
            }
        }
    }

    pub fn add_emails(&mut self, emails: &[EmailRef]) {
        for email in emails {
            match self.emails.iter_mut().find(|e| e.id == email.id) {
                Some(known) => *known = email.clone(),
                None => self.emails.push(email.clone()),
            }
        }
    }

    pub fn mark(&self) -> Mark {
        Mark {
            blocks: self.blocks.len(),
            text: match self.blocks.last() {
                Some(Block::Text { text }) => text.len(),
                _ => 0,
            },
        }
    }

    /// Drops the text that came after `mark`.
    pub fn rewind(&mut self, mark: &Mark) {
        self.blocks.truncate(mark.blocks);
        if let Some(Block::Text { text }) = self.blocks.last_mut() {
            text.truncate(mark.text);
        }
        if matches!(self.blocks.last(), Some(Block::Text { text }) if text.is_empty()) {
            self.blocks.pop();
        }
    }

    /// The text of the answer, for tests and titles.
    #[cfg(test)]
    pub fn text(&self) -> String {
        self.blocks
            .iter()
            .filter_map(|b| match b {
                Block::Text { text } => Some(text.as_str()),
                Block::Card { .. } => None,
            })
            .collect()
    }
}

/// A chat in the history list.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfo {
    pub id: String,
    pub title: String,
    pub updated: i64,
}

impl Session {
    pub fn new(id: String, system: String, question: &str, now: i64) -> Self {
        let line = question.split_whitespace().collect::<Vec<_>>().join(" ");
        let title = match line.char_indices().nth(TITLE_CHARS) {
            Some((cut, _)) => format!("{}…", line[..cut].trim_end()),
            None => line,
        };
        Self {
            id,
            title,
            created: now,
            updated: now,
            system,
            turns: Vec::new(),
            entries: Vec::new(),
        }
    }

    /// The answer being written: the last entry.
    pub fn answer(&mut self) -> &mut Answer {
        if !matches!(self.entries.last(), Some(Entry::Assistant { .. })) {
            self.entries.push(Entry::Assistant {
                answer: Answer::default(),
            });
        }
        match self.entries.last_mut() {
            Some(Entry::Assistant { answer }) => answer,
            _ => unreachable!(),
        }
    }

    /// Ends an answer that was cut short (stopped, or the app quit): tool
    /// calls left without results get results saying so, since the model
    /// can't go on without them, and cards still waiting count as skipped.
    /// Results already in hand stay as they are.
    pub fn seal(&mut self) {
        if let Some(i) = self.turns.iter().rposition(|t| t.role == Role::Assistant) {
            let answered: Vec<&str> = match self.turns.get(i + 1) {
                Some(next) => next
                    .parts
                    .iter()
                    .filter_map(|p| match p {
                        Part::ToolResult { id, .. } => Some(id.as_str()),
                        _ => None,
                    })
                    .collect(),
                None => Vec::new(),
            };
            let missing: Vec<Part> = self.turns[i]
                .tool_calls()
                .filter(|(id, _, _)| !answered.contains(id))
                .map(|(id, _, _)| Part::ToolResult {
                    id: id.to_owned(),
                    content: "The user stopped the assistant before this ran.".into(),
                    is_error: true,
                })
                .collect();
            if !missing.is_empty() {
                match self.turns.get_mut(i + 1) {
                    // Results go before any text in a turn.
                    Some(next) => {
                        let at = next
                            .parts
                            .iter()
                            .take_while(|p| matches!(p, Part::ToolResult { .. }))
                            .count();
                        next.parts.splice(at..at, missing);
                    }
                    None => self.turns.push(Turn {
                        role: Role::User,
                        parts: missing,
                    }),
                }
            }
        }
        if let Some(Entry::Assistant { answer }) = self.entries.last_mut()
            && answer.status == Status::Working
        {
            answer.status = Status::Stopped;
            for block in &mut answer.blocks {
                if let Block::Card { card } = block {
                    card.settle(CardState::Skipped);
                }
            }
        }
    }

    fn info(&self) -> SessionInfo {
        SessionInfo {
            id: self.id.clone(),
            title: self.title.clone(),
            updated: self.updated,
        }
    }
}

pub struct Sessions {
    dir: PathBuf,
}

impl Sessions {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    /// Only ids this app made: letters and digits, so they can't name
    /// another file.
    fn path(&self, id: &str) -> Result<PathBuf, String> {
        if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric()) {
            return Err(format!("There's no chat {id:?}."));
        }
        Ok(self.dir.join(format!("{id}.json")))
    }

    pub fn load(&self, id: &str) -> Result<Session, String> {
        let text = std::fs::read_to_string(self.path(id)?)
            .map_err(|_| "This chat isn't saved anymore.".to_owned())?;
        serde_json::from_str(&text).map_err(|e| format!("This chat couldn't be read: {e}"))
    }

    pub fn save(&self, session: &Session) -> Result<(), String> {
        std::fs::create_dir_all(&self.dir).map_err(|e| e.to_string())?;
        let path = self.path(&session.id)?;
        let tmp = path.with_extension("json.tmp");
        let json = serde_json::to_vec(session).map_err(|e| e.to_string())?;
        std::fs::write(&tmp, json)
            .and_then(|()| std::fs::rename(&tmp, &path))
            .map_err(|e| format!("The chat couldn't be saved: {e}"))
    }

    pub fn delete(&self, id: &str) -> Result<(), String> {
        match std::fs::remove_file(self.path(id)?) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.to_string()),
            _ => Ok(()),
        }
    }

    /// Newest first. Chats past the newest [`KEEP`] are deleted.
    pub fn list(&self) -> Vec<SessionInfo> {
        let Ok(dir) = std::fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        let mut sessions: Vec<SessionInfo> = dir
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
            .filter_map(|e| std::fs::read_to_string(e.path()).ok())
            .filter_map(|text| serde_json::from_str::<Session>(&text).ok())
            .map(|s| s.info())
            .collect();
        sessions.sort_by(|a, b| b.updated.cmp(&a.updated).then_with(|| b.id.cmp(&a.id)));
        for old in sessions.split_off(sessions.len().min(KEEP)) {
            let _ = self.delete(&old.id);
        }
        sessions
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::assistant::tools::DraftCard;
    use serde_json::json;

    fn dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("spark-assistant-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn titles_are_the_first_question_on_one_line() {
        let s = Session::new("a1".into(), String::new(), "  What's   new\nfrom Ana? ", 1);
        assert_eq!(s.title, "What's new from Ana?");
        let long = "word ".repeat(40);
        let s = Session::new("a2".into(), String::new(), &long, 1);
        assert!(s.title.ends_with('…'));
        assert!(s.title.chars().count() <= TITLE_CHARS + 1);
    }

    #[test]
    fn rewinding_drops_only_what_came_after_the_mark() {
        let mut answer = Answer::default();
        answer.push_text("Looking. ");
        let mark = answer.mark();
        answer.push_text("Partial");
        answer.push_card(Card::Draft {
            id: "c1".into(),
            draft: DraftCard::default(),
        });
        answer.push_text("more");
        answer.rewind(&mark);
        assert_eq!(answer.text(), "Looking. ");
        assert_eq!(answer.blocks.len(), 1);

        let mut fresh = Answer::default();
        let mark = fresh.mark();
        fresh.push_text("Refused text");
        fresh.rewind(&mark);
        assert!(fresh.blocks.is_empty());
    }

    #[test]
    fn sealing_answers_dangling_tool_calls_and_skips_waiting_cards() {
        let mut s = Session::new("a3".into(), String::new(), "Archive the newsletters", 1);
        s.turns.push(Turn::user("Archive the newsletters"));
        s.turns.push(Turn {
            role: Role::Assistant,
            parts: vec![Part::ToolCall {
                id: "t1".into(),
                name: "update_emails".into(),
                input: json!({ "ids": [1], "action": "done" }),
            }],
        });
        s.entries.push(Entry::User {
            text: "Archive the newsletters".into(),
        });
        s.answer().push_card(Card::Action {
            id: "t1".into(),
            title: "Mark 1 email Done".into(),
            emails: Vec::new(),
            senders: Vec::new(),
            state: CardState::Waiting,
            note: None,
            event: None,
        });
        s.seal();
        assert_eq!(s.turns.len(), 3);
        assert!(
            matches!(&s.turns[2].parts[0], Part::ToolResult { id, is_error: true, .. } if id == "t1")
        );
        let answer = s.answer();
        assert_eq!(answer.status, Status::Stopped);
        assert!(matches!(
            &answer.blocks[0],
            Block::Card {
                card: Card::Action {
                    state: CardState::Skipped,
                    ..
                }
            }
        ));

        // Sealing again changes nothing.
        let turns = s.turns.len();
        s.seal();
        assert_eq!(s.turns.len(), turns);
    }

    #[test]
    fn sealing_keeps_the_results_already_in() {
        let mut s = Session::new("a4".into(), String::new(), "Archive both", 1);
        s.turns.push(Turn::user("Archive both"));
        s.turns.push(Turn {
            role: Role::Assistant,
            parts: vec![
                Part::ToolCall {
                    id: "t1".into(),
                    name: "update_emails".into(),
                    input: json!({ "ids": [1], "action": "done" }),
                },
                Part::ToolCall {
                    id: "t2".into(),
                    name: "update_emails".into(),
                    input: json!({ "ids": [2], "action": "done" }),
                },
            ],
        });
        s.turns.push(Turn {
            role: Role::User,
            parts: vec![Part::ToolResult {
                id: "t1".into(),
                content: "updated".into(),
                is_error: false,
            }],
        });
        s.seal();
        assert_eq!(s.turns.len(), 3);
        assert_eq!(
            s.turns[2].parts,
            vec![
                Part::ToolResult {
                    id: "t1".into(),
                    content: "updated".into(),
                    is_error: false
                },
                Part::ToolResult {
                    id: "t2".into(),
                    content: "The user stopped the assistant before this ran.".into(),
                    is_error: true,
                },
            ]
        );

        // A results turn begun but empty (the app quit mid-way) is filled in.
        s.turns[2].parts.clear();
        s.seal();
        assert_eq!(s.turns[2].parts.len(), 2);
        assert_eq!(s.turns.len(), 3);
    }

    #[test]
    fn saves_lists_and_deletes_chats_newest_first() {
        let sessions = Sessions::new(dir("store"));
        assert!(sessions.list().is_empty());
        let mut older = Session::new("s1".into(), "sys".into(), "First", 10);
        older.turns.push(Turn::user("First"));
        sessions.save(&older).unwrap();
        sessions
            .save(&Session::new("s2".into(), "sys".into(), "Second", 20))
            .unwrap();
        let listed: Vec<_> = sessions.list().into_iter().map(|s| s.title).collect();
        assert_eq!(listed, ["Second", "First"]);
        assert_eq!(sessions.load("s1").unwrap().turns, older.turns);

        assert!(sessions.load("../s1").is_err());
        assert!(sessions.delete("..").is_err());
        sessions.delete("s1").unwrap();
        sessions.delete("s1").unwrap();
        assert_eq!(sessions.list().len(), 1);
        std::fs::remove_dir_all(&sessions.dir).unwrap();
    }
}
