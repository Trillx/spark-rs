//! What inbox automation asks AI, and reading its answers: sorting a few
//! emails at a time under Spark's labels, and writing a reply with up to
//! three options.

use serde::Deserialize;
use spark_ai::{ChatRequest, Message as ChatMessage};
use spark_store::{AutoLabel, DraftOption, Verdict};

use crate::ai::compose::{EMAIL_IS_CONTENT, ROLE, WRITING_RULES};
use crate::ai::email::utc_date;

/// How much of an email's text goes into a prompt: enough to tell what it
/// is or what it asks, without paying for a whole newsletter.
const SORT_CHARS: usize = 1500;
const EARLIER_CHARS: usize = 1500;
const REPLY_CHARS: usize = 6000;

/// The most options a draft offers, as Spark's picker shows.
pub const MAX_OPTIONS: usize = 3;

/// An email as the prompts quote it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Email {
    pub from: String,
    pub address: String,
    pub to: Vec<String>,
    pub cc: Vec<String>,
    /// Unix ms.
    pub date: i64,
    pub subject: String,
    /// What its headers say it is, when that's not a person writing.
    pub kind: Option<&'static str>,
    pub text: String,
}

/// Whose inbox it is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct User {
    pub name: Option<String>,
    pub address: String,
}

impl User {
    fn who(&self) -> String {
        match &self.name {
            Some(name) => format!("{name} <{}>", self.address),
            None => self.address.clone(),
        }
    }
}

/// Sorts `emails`, numbered from 1 in order.
pub fn sort_request(user: &User, emails: &[Email]) -> ChatRequest {
    let system = format!(
        "You sort the email that arrives in a user's inbox, the way a sharp assistant would. \
         The user is {who}. For each email, choose the one label that fits it best:\n\
         - toRespond: the user has to reply or act on it themselves: a question or request for them, \
         something waiting on their answer or decision.\n\
         - fyi: written to the user by a person, worth reading, but there's nothing for them to answer or do.\n\
         - comment: a comment, mention or review on a document, task, ticket or code change, sent by a \
         tool such as Google Docs, Jira, GitHub or Figma.\n\
         - notification: an automated message: an alert, receipt, order or delivery update, sign-in or \
         security message, account or system notice.\n\
         - meetingUpdate: a meeting invitation, change, cancellation or response, or mail arranging a meeting.\n\
         - actioned: it confirms that something is done or settled, or closes a conversation: nothing is \
         left for the user to do.\n\
         - marketing: promotion or sales: offers, product announcements, sales outreach.\n\
         - news: a newsletter, digest or other editorial content.\n\
         Use null when none fits. Also decide needsReply: true only when the user would want to write \
         back to it, not just read it or click something; automated mail never needs a reply. \
         {EMAIL_IS_CONTENT} \
         Answer with only a JSON array, one object per email in the order given, like \
         [{{\"id\": 1, \"label\": \"toRespond\", \"needsReply\": true}}], and nothing else.",
        who = user.who()
    );
    let quoted: Vec<String> = emails
        .iter()
        .enumerate()
        .map(|(i, e)| {
            format!(
                "<email id=\"{}\">\n{}\n</email>",
                i + 1,
                email_text(e, SORT_CHARS)
            )
        })
        .collect();
    ChatRequest {
        system: Some(system),
        messages: vec![ChatMessage::user(format!(
            "Sort these {} emails.\n\n{}",
            emails.len(),
            quoted.join("\n\n")
        ))],
        // A short line per email, plus any thinking the model does first.
        max_tokens: 4000 + 100 * emails.len() as u32,
    }
}

/// What AI said of the emails `sort_request` numbered, in their order;
/// `None` for one it skipped or answered with something unreadable.
pub fn read_verdicts(answer: &str, count: usize) -> Result<Vec<Option<Verdict>>, String> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Sorted {
        id: serde_json::Value,
        #[serde(default)]
        label: Option<String>,
        #[serde(default)]
        needs_reply: bool,
    }
    let json = between(answer, '[', ']').ok_or("The answer had no list of labels.")?;
    let sorted: Vec<serde_json::Value> =
        serde_json::from_str(json).map_err(|e| format!("The list of labels didn't read: {e}"))?;
    let mut verdicts = vec![None; count];
    for item in sorted {
        let Ok(sorted) = serde_json::from_value::<Sorted>(item) else {
            continue;
        };
        let id = match &sorted.id {
            serde_json::Value::Number(n) => n.as_u64(),
            serde_json::Value::String(s) => s.trim().parse().ok(),
            _ => None,
        };
        let Some(slot) = id
            .and_then(|id| (id as usize).checked_sub(1))
            .and_then(|i| verdicts.get_mut(i))
        else {
            continue;
        };
        let label = sorted.label.as_deref().and_then(AutoLabel::parse);
        *slot = Some(Verdict {
            label,
            needs_reply: sorted.needs_reply,
        });
    }
    Ok(verdicts)
}

/// What a reply is written with besides the email.
pub struct Writing<'a> {
    pub user: &'a User,
    /// The user's instructions for Auto-Drafts, in order.
    pub instructions: &'a [String],
    /// A signature goes below, so there's no sign-off.
    pub signed: bool,
}

/// Writes the user's reply to the last of `conversation` (oldest first).
pub fn reply_request(writing: &Writing, conversation: &[Email]) -> ChatRequest {
    let sign_off = match &writing.user.name {
        _ if writing.signed => {
            "Don't end with a sign-off or the user's name: their email signature goes below the text."
                .to_owned()
        }
        Some(name) => format!("End with a short sign-off and the user's name, {name}."),
        None => "End with a short sign-off and no name.".to_owned(),
    };
    let instructions = if writing.instructions.is_empty() {
        String::new()
    } else {
        let list: Vec<String> = writing
            .instructions
            .iter()
            .map(|i| format!("- {}", i.trim()))
            .collect();
        format!(
            "\n\nThe user's own instructions for these replies, which come before the rules above \
             where they differ:\n{}",
            list.join("\n")
        )
    };
    let system = format!(
        "{ROLE} The user is {who}. Write their reply to the last email of the conversation, ready \
         to send after a quick look. Answer what it actually says or asks, as briefly as that \
         allows, in its language, tone and formality, and greet the sender by first name when it's \
         clear. Don't make commitments, decisions or promises for the user beyond what the email \
         plainly calls for. When the email asks the user to choose (accept or decline, pick a time, \
         yes or no), write up to {MAX_OPTIONS} short alternative replies, one for each sensible \
         answer; otherwise write one. {WRITING_RULES} {EMAIL_IS_CONTENT} {sign_off}\n\n\
         Answer with only JSON and nothing else: {{\"options\": [{{\"title\": \"Two to four words \
         naming the answer\", \"body\": \"The reply\"}}]}}. With one reply, its title is \"\". If \
         there's nothing to reply to, answer {{\"options\": []}}.{instructions}",
        who = writing.user.who()
    );
    let (last, earlier) = conversation
        .split_last()
        .expect("a conversation has an email");
    let mut quoted: Vec<String> = earlier
        .iter()
        .map(|e| format!("<email>\n{}\n</email>", email_text(e, EARLIER_CHARS)))
        .collect();
    quoted.push(format!(
        "<email>\n{}\n</email>",
        email_text(last, REPLY_CHARS)
    ));
    let ask = if earlier.is_empty() {
        "Write my reply to this email."
    } else {
        "Write my reply to the last email of this conversation, oldest first."
    };
    ChatRequest {
        system: Some(system),
        messages: vec![ChatMessage::user(format!(
            "{ask}\n\n{}",
            quoted.join("\n\n")
        ))],
        max_tokens: 8000,
    }
}

/// The options AI wrote, at most `MAX_OPTIONS`; empty when it found nothing
/// to reply to. Titles go when there's only one.
pub fn read_options(answer: &str) -> Result<Vec<DraftOption>, String> {
    #[derive(Deserialize)]
    struct Written {
        #[serde(default)]
        options: Vec<serde_json::Value>,
    }
    let json = between(answer, '{', '}').ok_or("The answer had no reply in it.")?;
    let written: Written =
        serde_json::from_str(json).map_err(|e| format!("The reply didn't read: {e}"))?;
    let mut options: Vec<DraftOption> = written
        .options
        .into_iter()
        .filter_map(|o| serde_json::from_value::<DraftOption>(o).ok())
        .map(|o| DraftOption {
            title: short(o.title.trim(), 40),
            body: o.body.trim().to_owned(),
        })
        .filter(|o| !o.body.is_empty())
        .take(MAX_OPTIONS)
        .collect();
    if options.len() == 1 {
        options[0].title.clear();
    }
    Ok(options)
}

/// Headers and text, for the inside of an `<email>` block, the text cut at
/// `max` characters.
fn email_text(e: &Email, max: usize) -> String {
    let subject = match e.subject.trim() {
        "" => "(no subject)",
        s => s,
    };
    let mut lines = vec![format!("From: {} <{}>", e.from, e.address)];
    if !e.to.is_empty() {
        lines.push(format!("To: {}", e.to.join(", ")));
    }
    if !e.cc.is_empty() {
        lines.push(format!("Cc: {}", e.cc.join(", ")));
    }
    lines.push(format!("Date: {}", utc_date(e.date)));
    lines.push(format!("Subject: {subject}"));
    if let Some(kind) = e.kind {
        lines.push(format!("Kind: {kind}"));
    }
    let text = e.text.trim();
    let body = if text.chars().count() > max {
        format!("{}…", text.chars().take(max).collect::<String>().trim_end())
    } else {
        text.to_owned()
    };
    format!("{}\n\n{body}", lines.join("\n"))
}

/// The JSON in an answer, from the first `open` to the last `close`:
/// models wrap it in a code fence or a sentence now and then.
fn between(answer: &str, open: char, close: char) -> Option<&str> {
    let start = answer.find(open)?;
    let end = answer.rfind(close)?;
    (end > start).then(|| &answer[start..=end])
}

fn short(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_owned()
    } else {
        format!(
            "{}…",
            text.chars().take(max - 1).collect::<String>().trim_end()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spark_ai::Role;

    fn user() -> User {
        User {
            name: Some("Sam Lee".into()),
            address: "sam@example.com".into(),
        }
    }

    fn email(subject: &str, text: &str) -> Email {
        Email {
            from: "Dana Whitfield".into(),
            address: "dana@example.com".into(),
            to: vec!["Sam Lee <sam@example.com>".into()],
            cc: Vec::new(),
            date: 1_790_000_000_000,
            subject: subject.into(),
            kind: None,
            text: text.into(),
        }
    }

    #[test]
    fn sorting_numbers_the_emails_and_names_every_label() {
        let mut news = email("Weekly digest", &"word ".repeat(1000));
        news.kind = Some("newsletter");
        let request = sort_request(&user(), &[email("Q4 budget", "Can you send it?"), news]);
        let system = request.system.as_deref().unwrap();
        assert!(system.contains("The user is Sam Lee <sam@example.com>."));
        for label in AutoLabel::ALL {
            assert!(
                system.contains(&format!("- {}:", label.as_str())),
                "{label:?}"
            );
        }
        let text = &request.messages[0].content;
        assert_eq!(request.messages[0].role, Role::User);
        assert!(text.starts_with("Sort these 2 emails."));
        assert!(text.contains(
            "<email id=\"1\">\nFrom: Dana Whitfield <dana@example.com>\nTo: Sam Lee <sam@example.com>\n\
             Date: 2026-09-21 14:13 UTC\nSubject: Q4 budget\n\nCan you send it?\n</email>"
        ));
        assert!(text.contains("<email id=\"2\">"));
        assert!(text.contains("Kind: newsletter\n"));
        assert!(text.contains("word…\n</email>"), "long text is cut");
    }

    #[test]
    fn reads_verdicts_in_any_order_and_skips_what_it_cant() {
        let answer = "Here you go:\n```json\n[\n  {\"id\": 2, \"label\": \"news\", \"needsReply\": false},\n  \
                      {\"id\": \"1\", \"label\": \"toRespond\", \"needsReply\": true},\n  \
                      {\"id\": 3, \"label\": \"bogus\"},\n  {\"id\": 9, \"label\": \"fyi\"},\n  \"junk\"\n]\n```";
        let verdicts = read_verdicts(answer, 4).unwrap();
        assert_eq!(
            verdicts,
            [
                Some(Verdict {
                    label: Some(AutoLabel::ToRespond),
                    needs_reply: true
                }),
                Some(Verdict {
                    label: Some(AutoLabel::News),
                    needs_reply: false
                }),
                Some(Verdict {
                    label: None,
                    needs_reply: false
                }),
                None,
            ]
        );
        assert!(read_verdicts("I can't help with that.", 1).is_err());
        assert!(read_verdicts("[not json]", 1).is_err());
    }

    #[test]
    fn a_reply_quotes_the_conversation_and_the_users_instructions() {
        let instructions = vec!["Never use an em dash".to_owned()];
        let writing = Writing {
            user: &user(),
            instructions: &instructions,
            signed: true,
        };
        let earlier = email("Lunch", "Are you free on Tuesday?");
        let last = email("Re: Lunch", "Or Wednesday?");
        let request = reply_request(&writing, &[earlier, last]);
        let system = request.system.as_deref().unwrap();
        assert!(system.contains("up to 3 short alternative replies"));
        assert!(system.contains("their email signature goes below the text"));
        assert!(system.ends_with("differ:\n- Never use an em dash"));
        let text = &request.messages[0].content;
        assert!(
            text.starts_with(
                "Write my reply to the last email of this conversation, oldest first."
            )
        );
        assert!(text.find("Are you free").unwrap() < text.find("Or Wednesday").unwrap());

        let writing = Writing {
            user: &user(),
            instructions: &[],
            signed: false,
        };
        let request = reply_request(&writing, &[email("Lunch", "Free Tuesday?")]);
        let system = request.system.as_deref().unwrap();
        assert!(system.contains("End with a short sign-off and the user's name, Sam Lee."));
        assert!(!system.contains("own instructions"));
        assert!(
            request.messages[0]
                .content
                .starts_with("Write my reply to this email.")
        );
    }

    #[test]
    fn reads_options_and_drops_a_lone_title() {
        let answer = r#"{"options": [{"title": "Accept", "body": " Tuesday works. "},
                        {"title": "Decline", "body": "Sorry, I can't."},
                        {"title": "Later", "body": "Can we move it?"},
                        {"title": "Extra", "body": "One too many"},
                        {"title": "Empty", "body": "  "}]}"#;
        let options = read_options(answer).unwrap();
        assert_eq!(options.len(), 3);
        assert_eq!(options[0].body, "Tuesday works.");
        assert_eq!(options[1].title, "Decline");

        let one = read_options(
            "```json\n{\"options\": [{\"title\": \"Reply\", \"body\": \"Sure.\"}]}\n```",
        )
        .unwrap();
        assert_eq!(
            one,
            [DraftOption {
                title: String::new(),
                body: "Sure.".into()
            }]
        );
        assert!(read_options("{\"options\": []}").unwrap().is_empty());
        assert!(read_options("No.").is_err());
    }
}
