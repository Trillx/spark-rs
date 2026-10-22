//! API keys in the OS credential store (Windows Credential Manager; the
//! kernel keyring on Linux), one per preset so switching providers keeps
//! them. Keys never reach the UI, the settings file or a log.

use std::sync::Arc;

use keyring_core::{CredentialStore, Entry, Error};

use super::settings::Preset;

const SERVICE: &str = "spark-rs";

pub struct Keys(Result<Arc<CredentialStore>, String>);

impl Keys {
    /// The platform's store, also made keyring-core's default. Call once at startup.
    pub fn platform() -> Self {
        let store = platform_store();
        if let Ok(store) = &store {
            keyring_core::set_default_store(store.clone());
        }
        Self(store)
    }

    #[cfg(test)]
    pub fn with_store(store: Arc<CredentialStore>) -> Self {
        Self(Ok(store))
    }

    fn entry(&self, preset: Preset) -> Result<Entry, String> {
        let store = self.0.as_ref().map_err(Clone::clone)?;
        let user = format!("ai-key:{}", preset.id());
        store.build(SERVICE, &user, None).map_err(|e| describe(&e))
    }

    pub fn get(&self, preset: Preset) -> Result<Option<String>, String> {
        match self.entry(preset)?.get_password() {
            Ok(key) => Ok(Some(key)),
            Err(Error::NoEntry) => Ok(None),
            Err(e) => Err(describe(&e)),
        }
    }

    pub fn has(&self, preset: Preset) -> bool {
        matches!(self.get(preset), Ok(Some(_)))
    }

    /// Stores `key` (trimmed, since pasted keys often end in a newline), or
    /// deletes the stored key when it's empty.
    pub fn set(&self, preset: Preset, key: &str) -> Result<(), String> {
        let entry = self.entry(preset)?;
        let key = key.trim();
        let result = if key.is_empty() {
            entry.delete_credential()
        } else {
            entry.set_password(key)
        };
        match result {
            Ok(()) | Err(Error::NoEntry) => Ok(()),
            Err(e) => Err(describe(&e)),
        }
    }
}

pub(crate) fn describe(error: &Error) -> String {
    match error {
        Error::NoStorageAccess(e) => format!("Couldn't open the credential store: {e}"),
        Error::PlatformFailure(e) => format!("The credential store failed: {e}"),
        // These carry the stored bytes, which must not end up in a message.
        Error::BadEncoding(_) | Error::BadDataFormat(..) => {
            "The saved API key can't be read. Remove it and add it again.".into()
        }
        other => format!("Couldn't use the credential store: {other}"),
    }
}

#[cfg(target_os = "windows")]
pub(crate) fn platform_store() -> Result<Arc<CredentialStore>, String> {
    windows_native_keyring_store::Store::new()
        .map(|store| store as Arc<CredentialStore>)
        .map_err(|e| describe(&e))
}

#[cfg(target_os = "linux")]
pub(crate) fn platform_store() -> Result<Arc<CredentialStore>, String> {
    linux_keyutils_keyring_store::Store::new()
        .map(|store| store as Arc<CredentialStore>)
        .map_err(|e| describe(&e))
}

#[cfg(not(any(target_os = "windows", target_os = "linux")))]
pub(crate) fn platform_store() -> Result<Arc<CredentialStore>, String> {
    Err("spark-rs can't store API keys on this system yet.".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use keyring_core::mock;

    // Each test gets its own mock store; the real one is never touched.
    fn keys() -> Keys {
        Keys::with_store(mock::Store::new().unwrap())
    }

    #[test]
    fn stores_one_key_per_preset() {
        let keys = keys();
        assert_eq!(keys.get(Preset::Anthropic), Ok(None));
        keys.set(Preset::Anthropic, "sk-ant-test\n").unwrap();
        keys.set(Preset::OpenAi, "sk-openai-test").unwrap();
        assert_eq!(
            keys.get(Preset::Anthropic).unwrap().as_deref(),
            Some("sk-ant-test")
        );
        assert_eq!(
            keys.get(Preset::OpenAi).unwrap().as_deref(),
            Some("sk-openai-test")
        );
        assert!(!keys.has(Preset::Custom));

        let entry = keys.entry(Preset::OpenAi).unwrap();
        assert_eq!(
            entry.get_specifiers(),
            Some(("spark-rs".to_owned(), "ai-key:openai".to_owned()))
        );
    }

    #[test]
    fn an_empty_key_deletes_and_deleting_twice_is_fine() {
        let keys = keys();
        keys.set(Preset::Custom, "local-key").unwrap();
        assert!(keys.has(Preset::Custom));
        keys.set(Preset::Custom, "").unwrap();
        assert!(!keys.has(Preset::Custom));
        keys.set(Preset::Custom, "  ").unwrap();
        assert_eq!(keys.get(Preset::Custom), Ok(None));
    }

    #[test]
    fn errors_never_contain_the_key() {
        let keys = keys();
        keys.set(Preset::Anthropic, "sk-ant-secret").unwrap();
        let entry = keys.entry(Preset::Anthropic).unwrap();
        let cred: &mock::Cred = entry.as_any().downcast_ref().unwrap();
        cred.set_error(Error::BadEncoding(b"sk-ant-secret".to_vec()));
        let error = keys.get(Preset::Anthropic).unwrap_err();
        assert!(!error.contains("sk-ant-secret"), "{error}");
        // The mock's error is one-shot; the stored key is untouched.
        assert!(keys.has(Preset::Anthropic));
    }

    #[test]
    fn a_missing_store_is_reported_on_use() {
        let keys = Keys(Err("no store here".into()));
        assert_eq!(keys.get(Preset::Anthropic), Err("no store here".into()));
        assert!(!keys.has(Preset::Anthropic));
    }
}
