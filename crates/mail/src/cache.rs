//! Attachments kept on this computer once they're downloaded, so opening or
//! saving one again doesn't fetch it again (for IMAP, the whole email), and
//! Spark's Auto-download can fetch them ahead. Files sit under the account
//! and email they're from, the email named by its id and its `files_key`
//! (a stored id can be another email's later; the key can't). The oldest go
//! once there's more than a limit, an account's go with it, and an email's
//! once it's gone from the store (`sweep`).

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::SystemTime;

use crate::attachment::{AttachmentData, mime_for};

/// How much the cache keeps before the oldest files go.
pub const LIMIT_BYTES: u64 = 1024 * 1024 * 1024;

#[derive(Debug)]
pub struct AttachmentCache {
    dir: PathBuf,
    limit: u64,
    /// Held while files are written, and while an account's or email's are
    /// taken away, so a download that finishes as its account goes can't
    /// put them back.
    busy: Mutex<()>,
}

impl AttachmentCache {
    pub fn new(dir: PathBuf) -> Self {
        Self::with_limit(dir, LIMIT_BYTES)
    }

    pub fn with_limit(dir: PathBuf, limit: u64) -> Self {
        Self {
            dir,
            limit,
            busy: Mutex::new(()),
        }
    }

    /// Hold it to check an account is still there and keep its files.
    pub fn lock(&self) -> MutexGuard<'_, ()> {
        self.busy
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn path(&self, account: i64, message: &str, index: usize) -> PathBuf {
        self.dir
            .join(account.to_string())
            .join(message)
            .join(index.to_string())
    }

    /// Attachment `index` of email `message`, named `name`, of type `mime`
    /// (as the email lists it; empty goes by the name), if it's here.
    pub fn get(
        &self,
        account: i64,
        message: &str,
        index: usize,
        name: &str,
        mime: &str,
    ) -> Option<AttachmentData> {
        let path = self.path(account, message, index);
        let bytes = fs::read(&path).ok()?;
        // Read again, it's the newest: pruning goes by when files were used.
        let _ = touch(&path);
        Some(AttachmentData {
            name: name.to_owned(),
            mime: if mime.is_empty() {
                mime_for(name).into()
            } else {
                mime.to_owned()
            },
            bytes,
        })
    }

    pub fn has(&self, account: i64, message: &str, index: usize) -> bool {
        self.path(account, message, index).is_file()
    }

    /// Keeps a downloaded attachment, with the lock held (`lock`). Failing
    /// to keep it isn't an error: it's downloaded again next time.
    pub fn put(&self, account: i64, message: &str, index: usize, bytes: &[u8]) {
        let path = self.path(account, message, index);
        let Some(parent) = path.parent() else {
            return;
        };
        if fs::create_dir_all(parent).is_err() {
            return;
        }
        // Whole or not at all, even with two downloads of it at once: each
        // writes a file of its own, moved into place when it's complete.
        let mut random = [0u8; 8];
        if getrandom::fill(&mut random).is_err() {
            return;
        }
        let suffix: String = random.iter().map(|b| format!("{b:02x}")).collect();
        let partial = parent.join(format!("{index}.{suffix}.part"));
        if fs::write(&partial, bytes).is_err() || fs::rename(&partial, &path).is_err() {
            let _ = fs::remove_file(&partial);
        }
    }

    /// An account's attachments, as it's removed.
    pub fn forget_account(&self, account: i64) {
        let _held = self.lock();
        let _ = fs::remove_dir_all(self.dir.join(account.to_string()));
    }

    /// Takes away what belongs to no email the store still has: `keep` is
    /// each account's emails, as `<id>-<files_key>`; an account not in it is
    /// gone.
    pub fn sweep(&self, keep: &HashMap<i64, HashSet<String>>) {
        let _held = self.lock();
        let Ok(accounts) = fs::read_dir(&self.dir) else {
            return;
        };
        for account in accounts.flatten() {
            let path = account.path();
            let kept = account
                .file_name()
                .to_str()
                .and_then(|name| name.parse::<i64>().ok())
                .and_then(|id| keep.get(&id));
            let Some(kept) = kept else {
                let _ = fs::remove_dir_all(&path);
                continue;
            };
            let Ok(messages) = fs::read_dir(&path) else {
                continue;
            };
            for message in messages.flatten() {
                let known = message
                    .file_name()
                    .to_str()
                    .is_some_and(|n| kept.contains(n));
                if !known {
                    let _ = fs::remove_dir_all(message.path());
                }
            }
        }
    }

    /// Takes the least recently used files out until the cache is within
    /// its limit.
    pub fn prune(&self) {
        let _held = self.lock();
        let mut files = Vec::new();
        collect(&self.dir, &mut files);
        let mut total: u64 = files.iter().map(|(_, size, _)| size).sum();
        if total <= self.limit {
            return;
        }
        files.sort_by_key(|(_, _, used)| *used);
        for (path, size, _) in files {
            if total <= self.limit {
                break;
            }
            if fs::remove_file(&path).is_ok() {
                total -= size;
                // A message with nothing left goes too.
                if let Some(dir) = path.parent() {
                    let _ = fs::remove_dir(dir);
                }
            }
        }
    }
}

/// Every file under `dir`, with its size and when it was last used.
fn collect(dir: &Path, out: &mut Vec<(PathBuf, u64, SystemTime)>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if meta.is_dir() {
            collect(&entry.path(), out);
        } else {
            let used = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            out.push((entry.path(), meta.len(), used));
        }
    }
}

/// Marks a file as just used.
fn touch(path: &Path) -> std::io::Result<()> {
    fs::File::options()
        .append(true)
        .open(path)?
        .set_modified(SystemTime::now())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("spark-cache-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn kept_attachments_come_back_by_email_and_index() {
        let cache = AttachmentCache::new(dir("keep"));
        assert!(cache.get(1, "7-ab", 0, "plan.pdf", "").is_none());
        cache.put(1, "7-ab", 0, b"%PDF");
        let got = cache.get(1, "7-ab", 0, "plan.pdf", "").unwrap();
        assert_eq!(
            (got.name.as_str(), got.mime.as_str(), got.bytes.as_slice()),
            ("plan.pdf", "application/pdf", &b"%PDF"[..])
        );
        // The type the email lists, not the name's guess.
        cache.put(1, "7-ab", 1, b"BEGIN:VCALENDAR");
        assert_eq!(
            cache
                .get(1, "7-ab", 1, "invite", "text/calendar")
                .unwrap()
                .mime,
            "text/calendar"
        );
        assert!(
            cache.has(1, "7-ab", 0)
                && !cache.has(1, "7-ab", 2)
                && !cache.has(1, "7-cd", 0)
                && !cache.has(2, "7-ab", 0)
        );
        cache.forget_account(1);
        assert!(!cache.has(1, "7-ab", 0));
    }

    #[test]
    fn what_the_store_no_longer_has_is_swept() {
        let cache = AttachmentCache::new(dir("sweep"));
        cache.put(1, "7-ab", 0, b"kept");
        cache.put(1, "8-cd", 0, b"deleted");
        cache.put(2, "9-ef", 0, b"removed account");
        let keep = HashMap::from([(1, HashSet::from(["7-ab".to_owned()]))]);
        cache.sweep(&keep);
        assert!(cache.has(1, "7-ab", 0) && !cache.has(1, "8-cd", 0) && !cache.has(2, "9-ef", 0));
    }

    #[test]
    fn the_least_recently_used_go_first_over_the_limit() {
        let cache = AttachmentCache::with_limit(dir("prune"), 25);
        cache.put(1, "1", 0, &[0; 10]);
        std::thread::sleep(std::time::Duration::from_millis(20));
        cache.put(1, "2", 0, &[0; 10]);
        std::thread::sleep(std::time::Duration::from_millis(20));
        // Using the first makes the second the oldest.
        assert!(cache.get(1, "1", 0, "a", "").is_some());
        std::thread::sleep(std::time::Duration::from_millis(20));
        cache.put(1, "3", 0, &[0; 10]);
        cache.prune();
        assert!(cache.has(1, "1", 0) && !cache.has(1, "2", 0) && cache.has(1, "3", 0));
    }
}
