//! Mail passwords and Microsoft refresh tokens in the OS credential store.
//!
//! Windows caps one credential at 2,560 bytes, and refresh tokens can be
//! longer, so a value is stored as raw bytes in numbered parts
//! (`name#0`, `name#1`, …) with the part count under `name` itself.

use std::sync::Arc;

use keyring_core::{CredentialStore, Entry, Error};

use crate::ai::keys::{describe, platform_store};

const SERVICE: &str = "spark-rs";
const PART_BYTES: usize = 2_000;

pub struct MailSecrets(Result<Arc<CredentialStore>, String>);

impl MailSecrets {
    pub fn platform() -> Self {
        Self(platform_store())
    }

    #[cfg(test)]
    pub fn with_store(store: Arc<CredentialStore>) -> Self {
        Self(Ok(store))
    }

    fn entry(&self, user: &str) -> Result<Entry, String> {
        let store = self.0.as_ref().map_err(Clone::clone)?;
        store.build(SERVICE, user, None).map_err(|e| describe(&e))
    }

    fn read(&self, user: &str) -> Result<Option<Vec<u8>>, String> {
        match self.entry(user)?.get_secret() {
            Ok(bytes) => Ok(Some(bytes)),
            Err(Error::NoEntry) => Ok(None),
            Err(e) => Err(describe(&e)),
        }
    }

    fn delete(&self, user: &str) -> Result<(), String> {
        match self.entry(user)?.delete_credential() {
            Ok(()) | Err(Error::NoEntry) => Ok(()),
            Err(e) => Err(describe(&e)),
        }
    }

    fn part_count(&self, name: &str) -> Result<usize, String> {
        Ok(self
            .read(name)?
            .and_then(|bytes| String::from_utf8(bytes).ok())
            .and_then(|count| count.parse().ok())
            .unwrap_or(0))
    }
}

impl spark_mail::Secrets for MailSecrets {
    fn get(&self, name: &str) -> Result<Option<String>, String> {
        let count = self.part_count(name)?;
        if count == 0 {
            return Ok(None);
        }
        let mut bytes = Vec::new();
        for i in 0..count {
            match self.read(&format!("{name}#{i}"))? {
                Some(part) => bytes.extend(part),
                None => {
                    return Err(
                        "A saved sign-in is incomplete. Remove the account and add it again."
                            .into(),
                    );
                }
            }
        }
        String::from_utf8(bytes).map(Some).map_err(|_| {
            "A saved sign-in can't be read. Remove the account and add it again.".into()
        })
    }

    fn set(&self, name: &str, value: &str) -> Result<(), String> {
        let old = self.part_count(name)?;
        let parts: Vec<&[u8]> = value.as_bytes().chunks(PART_BYTES).collect();
        for (i, part) in parts.iter().enumerate() {
            self.entry(&format!("{name}#{i}"))?
                .set_secret(part)
                .map_err(|e| describe(&e))?;
        }
        if parts.is_empty() {
            self.delete(name)?;
        } else {
            self.entry(name)?
                .set_secret(parts.len().to_string().as_bytes())
                .map_err(|e| describe(&e))?;
        }
        for i in parts.len()..old {
            self.delete(&format!("{name}#{i}"))?;
        }
        Ok(())
    }
}

/// Deletes every password, token and API key spark-rs saved. The uninstaller
/// runs `spark-desktop.exe --forget-saved-passwords` for this when the user
/// ticks "Also delete my mail data and saved passwords".
#[cfg(target_os = "windows")]
pub fn forget_all() -> Result<(), String> {
    use windows::Win32::Security::Credentials::{
        CRED_TYPE_GENERIC, CREDENTIALW, CredDeleteW, CredEnumerateW, CredFree,
    };
    use windows::core::PCWSTR;

    // The Windows store names a credential `<user>.<service>`.
    let mut count = 0;
    let mut found: *mut *mut CREDENTIALW = std::ptr::null_mut();
    // Fails with "not found" when there are none.
    // Windows only supports a wildcard at the end of the filter, so enumerate
    // names and select this service here. Credential values are never read.
    if let Err(e) = unsafe { CredEnumerateW(PCWSTR::null(), None, &mut count, &mut found) } {
        return if e.code() == windows::Win32::Foundation::ERROR_NOT_FOUND.to_hresult() {
            Ok(())
        } else {
            Err(e.to_string())
        };
    }
    let mut result = Ok(());
    let found_slice = unsafe { std::slice::from_raw_parts(found, count as usize) };
    for &credential in found_slice {
        let credential = unsafe { &*credential };
        let target = unsafe { credential.TargetName.to_string() }.unwrap_or_default();
        if credential.Type == CRED_TYPE_GENERIC && target.ends_with(&format!(".{SERVICE}")) {
            if let Err(e) =
                unsafe { CredDeleteW(PCWSTR(credential.TargetName.0), CRED_TYPE_GENERIC, None) }
            {
                result = Err(e.to_string());
            }
        }
    }
    unsafe { CredFree(found as *const _) };
    result
}

#[cfg(not(target_os = "windows"))]
pub fn forget_all() -> Result<(), String> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use keyring_core::mock;
    use spark_mail::Secrets;

    #[test]
    fn long_values_are_split_and_put_back_together() {
        let secrets = MailSecrets::with_store(mock::Store::new().unwrap());
        assert_eq!(secrets.get("account:1:refresh-token"), Ok(None));

        let token = "x".repeat(4_500);
        secrets.set("account:1:refresh-token", &token).unwrap();
        assert_eq!(secrets.part_count("account:1:refresh-token"), Ok(3));
        assert_eq!(
            secrets.get("account:1:refresh-token").unwrap().as_deref(),
            Some(token.as_str())
        );

        // A shorter value leaves no stale parts behind.
        secrets.set("account:1:refresh-token", "short").unwrap();
        assert_eq!(
            secrets.get("account:1:refresh-token").unwrap().as_deref(),
            Some("short")
        );
        assert_eq!(secrets.read("account:1:refresh-token#1"), Ok(None));

        secrets.set("account:1:refresh-token", "").unwrap();
        assert_eq!(secrets.get("account:1:refresh-token"), Ok(None));
        assert_eq!(secrets.read("account:1:refresh-token#0"), Ok(None));
    }
}
