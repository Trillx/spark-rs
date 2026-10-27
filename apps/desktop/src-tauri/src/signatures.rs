//! Signatures for Settings and the composer. They're kept in the store, by
//! sending address, so the demo mailbox's addresses can have them too.

use serde::Serialize;
use spark_mail::signature::{self, MAX_IMAGE_BYTES};
use spark_mail::trackers;
use spark_store::{DefaultSignature, Signature, now_ms};
use tauri::State;

use crate::mailbox::{Mailbox, text};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SignaturesView {
    /// Oldest first.
    pub signatures: Vec<Signature>,
    /// Each sending address's default, in account order.
    pub defaults: Vec<AddressDefault>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AddressDefault {
    pub mailbox: String,
    pub signature: DefaultSignature,
}

#[tauri::command]
pub fn list_signatures(mailbox: State<'_, Mailbox>) -> Result<SignaturesView, String> {
    let addresses = mailbox.addresses();
    let store = mailbox.live.engine.store();
    let defaults = addresses
        .into_iter()
        .map(|mailbox| {
            let signature = store.default_signature(&mailbox).map_err(text)?;
            Ok(AddressDefault { mailbox, signature })
        })
        .collect::<Result<_, String>>()?;
    Ok(SignaturesView {
        signatures: store.signatures().map_err(text)?,
        defaults,
    })
}

/// Saves signature `id`, or a new one without. The HTML is cleaned first;
/// returns the signature as kept.
#[tauri::command]
pub fn save_signature(
    mailbox: State<'_, Mailbox>,
    id: Option<i64>,
    html: String,
) -> Result<Signature, String> {
    let (html, text) = clean(&html)?;
    let store = mailbox.live.engine.store();
    match id {
        Some(id) => {
            if !store
                .update_signature(id, &html, &text)
                .map_err(crate::mailbox::text)?
            {
                return Err("That signature was deleted.".into());
            }
            Ok(Signature { id, html, text })
        }
        None => store
            .add_signature(&html, &text, now_ms())
            .map_err(crate::mailbox::text),
    }
}

/// Addresses that defaulted to it go back to Most Recent.
#[tauri::command]
pub fn delete_signature(mailbox: State<'_, Mailbox>, id: i64) -> Result<(), String> {
    mailbox
        .live
        .engine
        .store()
        .remove_signature(id)
        .map_err(text)
}

#[tauri::command]
pub fn set_default_signature(
    mailbox: State<'_, Mailbox>,
    address: String,
    signature: DefaultSignature,
) -> Result<(), String> {
    mailbox
        .live
        .engine
        .store()
        .set_default_signature(&address, signature)
        .map_err(text)
}

/// The signature the composer starts with for `account`: for a reply to
/// `reply_to`, the one used last in that conversation.
#[tauri::command]
pub fn resolve_signature(
    mailbox: State<'_, Mailbox>,
    account: i64,
    reply_to: Option<i64>,
) -> Result<Option<Signature>, String> {
    let Some(address) = mailbox.account_email(account) else {
        return Ok(None);
    };
    let live = mailbox.live();
    let store = mailbox.live.engine.store();
    match reply_to {
        Some(original) if live => signature::for_reply(&store, &address, original),
        _ => store.signature_for(&address),
    }
    .map_err(text)
}

/// Cleaned HTML, without a tracking pixel copied in from an email, and its
/// text; an error says what's wrong with it.
fn clean(html: &str) -> Result<(String, String), String> {
    let html = signature::sanitize(&trackers::strip(html));
    let text = signature::text(&html);
    if text.is_empty() && !html.contains("<img") {
        return Err("Write a signature first.".into());
    }
    let (_, images) = signature::embed_images(&html);
    if images.iter().any(|i| i.bytes.len() > MAX_IMAGE_BYTES) {
        return Err("An image in the signature is over 1 MB. Use a smaller one.".into());
    }
    Ok((html, text))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signatures_need_something_in_them() {
        assert_eq!(
            clean("<div><br></div><script>x</script>").unwrap_err(),
            "Write a signature first."
        );
        let (html, text) = clean(r#"<div onclick="x()">Ana</div>"#).unwrap();
        assert_eq!((html.as_str(), text.as_str()), ("<div>Ana</div>", "Ana"));
        assert!(
            clean(r#"<img src="data:image/png;base64,iVBORw0KGgo=">"#).is_ok(),
            "an image alone is fine"
        );
    }

    #[test]
    fn images_over_a_megabyte_are_refused() {
        // Zeros, a byte over.
        let big = "A".repeat(4 * (MAX_IMAGE_BYTES + 1).div_ceil(3));
        let err = clean(&format!(r#"<img src="data:image/png;base64,{big}">"#)).unwrap_err();
        assert!(err.contains("1 MB"), "{err}");
    }
}
