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
