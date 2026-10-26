//! Settings → Notifications: which new mail shows a notification, its sound,
//! the meeting pop-up, and running in the background. Saved as `prefs.json`
//! in the app's config directory.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::{fs, io};

use serde::{Deserialize, Serialize};
use tauri::State;

use crate::agents::Access;
use crate::background;
use crate::meetings::Meetings;

const FILE_NAME: &str = "prefs.json";

/// Which new mail shows a notification.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum NotifyFor {
    /// Spark's smart notifications: mail from people, not notifications or
    /// newsletters.
    #[default]
    People,
    All,
    Off,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Prefs {
    pub notify: NotifyFor,
    /// Windows' New Mail sound with each notification.
    pub sound: bool,
    /// Closing the window leaves spark-rs in the notification area, still
    /// syncing; Quit is in the icon's menu.
    pub keep_running: bool,
    /// Starts in the background when the user signs in to Windows.
    pub open_at_login: bool,
    /// The meeting pop-up (`meetings.rs`) a minute before each meeting.
    pub meeting_popup: bool,
    /// Leaves events with no one else and no call out of the pop-up.
    pub meeting_popup_guests_only: bool,
    /// A chime as a meeting appears in the pop-up.
    pub meeting_popup_sound: bool,
    /// Suggest people from the user's mail while they type in To, Cc and Bcc.
    pub suggest_recipients: bool,
    /// The "still running" notice has been shown, on the first close.
    pub told_keep_running: bool,
    /// What agents may do in each account, by account id; absent is none.
    /// Settings → Agents changes it, not `save_prefs`.
    pub agent_access: BTreeMap<i64, Access>,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            notify: NotifyFor::People,
            sound: true,
            keep_running: true,
            open_at_login: false,
            meeting_popup: true,
            meeting_popup_guests_only: true,
            meeting_popup_sound: true,
            suggest_recipients: true,
            told_keep_running: false,
            agent_access: BTreeMap::new(),
        }
    }
}

impl Prefs {
    fn path(dir: &Path) -> PathBuf {
        dir.join(FILE_NAME)
    }

    /// The defaults until the user changes something. A damaged file also
    /// gives the defaults; the next change replaces it.
    fn load(dir: &Path) -> Self {
        let path = Self::path(dir);
        match fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
                eprintln!(
                    "spark-rs: {} is damaged ({e}); using defaults",
                    path.display()
                );
                Self::default()
            }),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Self::default(),
            Err(e) => {
                eprintln!("spark-rs: couldn't read {}: {e}", path.display());
                Self::default()
            }
        }
    }

    /// Writes a temporary file and renames it over `prefs.json`, so a crash
    /// never leaves half a file behind.
    fn save(&self, dir: &Path) -> Result<(), String> {
        let path = Self::path(dir);
        let temp = path.with_extension("json.tmp");
        let json = serde_json::to_string_pretty(self).expect("prefs serialize");
        fs::create_dir_all(dir)
            .and_then(|()| fs::write(&temp, json))
            .and_then(|()| fs::rename(&temp, &path))
            .map_err(|e| format!("Couldn't save {}: {e}", path.display()))
    }
}

pub struct PrefsState {
    dir: PathBuf,
    /// Also serializes writes to `prefs.json`.
    current: Mutex<Prefs>,
}

impl PrefsState {
    pub fn load(dir: PathBuf) -> Self {
        Self {
            current: Mutex::new(Prefs::load(&dir)),
            dir,
        }
    }

    pub fn get(&self) -> Prefs {
        self.current.lock().unwrap().clone()
    }

    /// Changes and saves the prefs. In memory they change even if saving
    /// fails, so the session behaves as the user asked.
    pub fn update(&self, change: impl FnOnce(&mut Prefs)) -> Result<Prefs, String> {
        let mut current = self.current.lock().unwrap();
        change(&mut current);
        current.save(&self.dir)?;
        Ok(current.clone())
    }
}

#[tauri::command]
pub fn get_prefs(state: State<'_, PrefsState>) -> Prefs {
    state.get()
}

#[tauri::command]
pub fn save_prefs(
    state: State<'_, PrefsState>,
    meetings: State<'_, Meetings>,
    prefs: Prefs,
) -> Result<Prefs, String> {
    if prefs.open_at_login != state.get().open_at_login {
        background::set_open_at_login(prefs.open_at_login)?;
    }
    let saved = state.update(|current| {
        *current = Prefs {
            told_keep_running: current.told_keep_running,
            agent_access: std::mem::take(&mut current.agent_access),
            ..prefs
        }
    });
    meetings.changed();
    saved
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("spark-rs-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn missing_fields_take_their_defaults() {
        let prefs: Prefs = serde_json::from_value(json!({ "notify": "all" })).unwrap();
        assert_eq!(
            prefs,
            Prefs {
                notify: NotifyFor::All,
                ..Prefs::default()
            }
        );
        assert!(prefs.sound && prefs.keep_running && !prefs.open_at_login);
        assert!(
            prefs.meeting_popup && prefs.meeting_popup_guests_only && prefs.meeting_popup_sound
        );
    }

    #[test]
    fn saves_and_loads_round_trip() {
        let dir = temp_dir("prefs-roundtrip");
        assert_eq!(Prefs::load(&dir), Prefs::default());

        let state = PrefsState::load(dir.clone());
        let saved = state
            .update(|p| {
                p.notify = NotifyFor::Off;
                p.sound = false;
            })
            .unwrap();
        assert_eq!(Prefs::load(&dir), saved);
        assert!(!dir.join("prefs.json.tmp").exists());

        fs::write(dir.join(FILE_NAME), "{ not json").unwrap();
        assert_eq!(Prefs::load(&dir), Prefs::default());
        fs::remove_dir_all(&dir).unwrap();
    }
}
