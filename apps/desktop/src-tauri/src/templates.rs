//! Templates for Settings and the composer, kept in the store like
//! signatures, so the demo mailbox has them too.

use spark_mail::signature;
use spark_store::{NewTemplate, Template, now_ms};
use tauri::State;

use crate::mailbox::{Mailbox, text};

/// In the user's order.
#[tauri::command]
pub fn list_templates(mailbox: State<'_, Mailbox>) -> Result<Vec<Template>, String> {
    mailbox.live.engine.store().templates().map_err(text)
}

/// Saves template `id`, or a new one without. The HTML is cleaned first,
/// keeping its placeholders; returns the template as kept.
#[tauri::command]
pub fn save_template(
    mailbox: State<'_, Mailbox>,
    id: Option<i64>,
    name: String,
    subject: String,
    html: String,
) -> Result<Template, String> {
    let (name, html, body) = clean(&name, &html)?;
    let new = NewTemplate {
        name: &name,
        subject: subject.trim(),
        html: &html,
        text: &body,
    };
    let store = mailbox.live.engine.store();
    match id {
        Some(id) => {
            if !store.update_template(id, new).map_err(text)? {
                return Err("That template was deleted.".into());
            }
            store
                .template(id)
                .map_err(text)?
                .ok_or_else(|| "That template was deleted.".into())
        }
        None => store.add_template(new).map_err(text),
    }
}

#[tauri::command]
pub fn delete_template(mailbox: State<'_, Mailbox>, id: i64) -> Result<(), String> {
    mailbox
        .live
        .engine
        .store()
        .remove_template(id)
        .map_err(text)
}

/// Puts the templates in the order of `ids`.
#[tauri::command]
pub fn order_templates(mailbox: State<'_, Mailbox>, ids: Vec<i64>) -> Result<(), String> {
    mailbox
        .live
        .engine
        .store()
        .order_templates(&ids)
        .map_err(text)
}

/// A template was applied: it goes first under Recent.
#[tauri::command]
pub fn template_used(mailbox: State<'_, Mailbox>, id: i64) -> Result<(), String> {
    mailbox
        .live
        .engine
        .store()
        .template_used(id, now_ms())
        .map_err(text)
}

/// The name, the cleaned HTML and its text; an error says what's missing.
fn clean(name: &str, html: &str) -> Result<(String, String, String), String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("Give the template a name.".into());
    }
    let html = signature::sanitize_template(html);
    let body = signature::body_text(&html);
    if body.is_empty() && !html.contains("<img") {
        return Err("Write the template first.".into());
    }
    Ok((name.to_owned(), html, body))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn templates_need_a_name_and_something_in_them() {
        assert_eq!(
            clean(" ", "<div>Hi</div>").unwrap_err(),
            "Give the template a name."
        );
        assert_eq!(
            clean("Thanks", "<div><br></div><script>x</script>").unwrap_err(),
            "Write the template first."
        );
        let (name, html, body) = clean(
            " Thanks ",
            r#"<div onclick="x()">Hi <span data-placeholder="recipientFirstName" data-label="Recipient's first name">Recipient's first name</span></div>"#,
        )
        .unwrap();
        assert_eq!(name, "Thanks");
        assert_eq!(
            html,
            r#"<div>Hi <span data-placeholder="recipientFirstName" data-label="Recipient's first name">Recipient's first name</span></div>"#
        );
        assert_eq!(body, "Hi Recipient's first name");
    }
}
