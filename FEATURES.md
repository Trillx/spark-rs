# spark-rs and Spark, feature by feature

spark-rs rebuilds [Spark](https://sparkmailapp.com), Readdle's email app, as
a free, open-source Windows app with a Rust core. This page takes every
feature on [Spark's pricing page](https://sparkmailapp.com/pricing), from
all of its plans together, and shows how much of each spark-rs has. spark-rs has no
plans: everything in it is free, and +AI runs on your own AI provider's key.

| Mark | Meaning |
|---|---|
| ✅ | Works as it does in Spark |
| 🟡 | Partly there; the note says what's missing |
| ❌ | Not built yet |
| ➖ | Not planned; the note says why |

Spark's support and enterprise rows (priority support, a success manager,
coaching, central billing, security controls) are services rather than app
features, so they're left out.

Last full check: 2026-10-26.

## Essentials

| Feature | spark-rs | Notes |
|---|:-:|---|
| Unlimited Email Accounts | ✅ | Microsoft 365 through Microsoft Graph, Gmail, and any IMAP server |
| Cross-Device Sync | ➖ | There's no sync server. Mail, read, Done, pins and folders go through your mail servers, so every device sees them; snooze, Set Aside, Priority and Gatekeeper decisions stay on the computer you set them on |
| Smart Inbox | ✅ | Priority, Pins, Invitations, People, Responses, Notifications and Newsletters as cards (each can be turned off or show more), or as one focused list; a sender can be moved to another category |
| Unified Inbox | ✅ | Every account in one inbox, with a row per account to see one alone |
| Smart Search | 🟡 | Full-text search on your computer and on each mail server, in every account or just the one you're in, with `from:`, `has:attachment`, `is:unread` and `date:today`, search by sender and recent searches; no plain-language searches such as "PDFs from Anna last week" |
| Smart Notifications | ✅ | Only mail from people notifies, unless you change it to all mail or only Priority mail |
| Scheduled Sends | ✅ | Send Later with Spark's presets or a day and time you pick; the Scheduled list sends now, moves, edits or cancels what's waiting |
| Snooze | 🟡 | Spark's presets, a custom date and time, and Discard Snooze; Snoozed lists mail by when it comes back, and it returns to the top of the Inbox with a notification. No Someday |
| Set Aside | ✅ | |
| Reminders | 🟡 | Remind me when you send, if nobody replies or either way: the email comes back to the top of the Inbox with a notification, and the Reminders list moves or removes them. Not yet on mail you receive |
| Mark as Done | ✅ | Done archives on the server, with six seconds to undo |
| Swipe Actions | ➖ | An iOS feature in Spark; spark-rs is a desktop app |
| Command Center | ✅ | `Ctrl+K` lists every action with its shortcut |
| Quick Replies | ➖ | An iOS feature in Spark; the +AI quick replies below work |
| Print Emails | ✅ | An email or a whole conversation, through Windows' print dialog, which also saves as PDF; Save as .eml too |
| Dark Mode | ✅ | Light, dark, or the same as Windows |

## Enhanced Productivity

| Feature | spark-rs | Notes |
|---|:-:|---|
| Priority | ✅ | Priority conversations and Priority senders, whose mail is all Priority; the Priority card's size is in Settings → Priority |
| Gatekeeper | ✅ | Spark's three modes (before the Inbox, inside it, off); accept or block a new sender or a whole domain once for every account; colleagues, people you've written to and big companies get through without asking; blocked mail moves to a Blocked folder on the server |
| Group by Sender & Domain | ✅ | A sender's or custom domain's mail folds into one row in the Inbox, opening as a list of its own |
| Smart Folders | ✅ | Save any search as a smart folder; its search can't be edited afterwards |
| Mute Threads | ✅ | New mail in a muted conversation doesn't notify |
| Templates | ✅ | Your own templates with a name, subject and rich text, a Recent section, and placeholders for the recipient's name, yours and custom fill-ins; Send asks if any are left empty. Make one from a draft |
| Productivity Integrations | ❌ | |
| Home Screen | ✅ | Opens at launch and after 15 minutes in the background (adjustable), with what came in while you were away, a Priority, People or All preview, and Email Focus times with reminders. Wallpapers are gradients, not photos |
| CRM Integrations | ❌ | |
| Link Sharing | ➖ | Needs a server to host the shared email |
| Spark CLI (Agent access) | ✅ | `spark-cli` is an MCP server for Claude Code, Codex and other agents, allowed to Read, Triage or Send per account |

## Calendars and Meetings

| Feature | spark-rs | Notes |
|---|:-:|---|
| Unlimited Calendar Connections | ✅ | Every calendar of every Microsoft 365 account you add |
| Multi-Provider Support | 🟡 | Microsoft 365 only; Gmail calendars need Google sign-in, which spark-rs doesn't have yet |
| Create Events | ❌ | Events can be read and answered, not created or edited |
| Zoom Integration | 🟡 | Join from the calendar and the meeting pop-up; adding a call to an event waits on Create Events |
| Google Meet Integration | 🟡 | Same as Zoom |
| Microsoft Teams Integration | 🟡 | Same as Zoom |

## Spark +AI

| Feature | spark-rs | Notes |
|---|:-:|---|
| Monthly Credits | ✅ | No credits to run out of: +AI uses your own Anthropic, OpenAI or OpenAI-compatible key, or a local model through Ollama |
| Writing Assistant | 🟡 | Writes an email from a few words, and rephrases, shortens, expands, proofreads or changes the tone of a draft; no translation |
| Quick Replies | ✅ | Interested, Not interested and Thanks, written by AI |
| Email Summaries | 🟡 | Short, detailed or action-point summaries of one email, not of a whole conversation |
| Translations | ❌ | |
| AI Assistant | ❌ | Meanwhile, agents such as Claude Code can search and read your mail through `spark-cli` |
| Meeting Notes | ❌ | |
| Autopilot | ❌ | |
| Auto-Drafts | 🟡 | New mail that needs an answer gets a reply waiting in Drafts and in the conversation, written with your own AI provider: up to three options to pick from, your custom instructions, senders to leave out, and cleanup of drafts you didn't use. Auto-Labels sort new mail into To respond, FYI and Spark's other labels, kept on Gmail and in Outlook too. Drafts don't learn your writing style yet |

## Team Collaboration

Spark's team features run on Readdle's servers. spark-rs has no server, so
they aren't planned.

| Feature | spark-rs | Notes |
|---|:-:|---|
| Create Team | ➖ | Needs a shared server |
| Private Comments | ➖ | Needs a shared server |
| Shared Threads | ➖ | Needs a shared server |
| Shared Drafts | ➖ | Needs a shared server |
| Assignments | ➖ | Needs a shared server |
| Team File Storage | ➖ | Needs a shared server |
| Shared Inboxes | ➖ | Needs a shared server |
| Shared Templates | ➖ | Needs a shared server |
| Team Administration | ➖ | Needs a shared server |
| Read Statuses | ➖ | Needs a server to track when an email is opened |
