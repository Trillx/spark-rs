//! Updates from `main`'s builds: Settings → About, the sidebar dot, and the
//! install itself. CI numbers each build `X.Y.Z+N`, where N counts the
//! app-changing commits on `main`, and publishes `latest.json` with a `build`
//! object next to the updater's own fields (see `.github/workflows/release.yml`).

use std::time::{Duration, SystemTime, UNIX_EPOCH};
use std::{fs, sync::Mutex};

use semver::Version;
use serde::{Deserialize, Serialize};
use tauri::utils::config::BundleType;
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_updater::{Update, UpdaterExt};

use crate::agents;

/// The commit CI built this copy from.
const COMMIT: Option<&str> = option_env!("SPARK_COMMIT");
/// Checks this long after start, then every `EVERY`.
const FIRST_CHECK: Duration = Duration::from_secs(20);
const EVERY: Duration = Duration::from_secs(4 * 60 * 60);
/// How often the loop looks at the clock. Short, because a sleeping laptop
/// doesn't count down a timer, but its clock moves on.
const TICK: Duration = Duration::from_secs(10 * 60);
const TIMEOUT: Duration = Duration::from_secs(30);

/// This copy, as CI built it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Installed {
    /// `X.Y.Z`, without the build number.
    pub version: String,
    /// N, or `None` for a copy CI didn't build, which can't update itself.
    pub build: Option<u32>,
}

impl Installed {
    pub fn from_version(version: &Version) -> Self {
        Self {
            version: format!("{}.{}.{}", version.major, version.minor, version.patch),
            build: version.build.as_str().parse().ok(),
        }
    }
}

/// The newest build, from the `build` object in `latest.json`.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewestBuild {
    /// N: the app-changing commits on `main` up to `commit`.
    pub number: u32,
    pub commit: String,
    /// App-changing commits, newest first, as many as CI lists.
    pub commits: Vec<Commit>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Commit {
    pub sha: String,
    pub subject: String,
}

impl NewestBuild {
    /// `manifest` is the whole `latest.json`, as the updater returns it.
    pub fn from_manifest(manifest: &serde_json::Value) -> Result<Self, String> {
        Self::deserialize(&manifest["build"])
            .map_err(|e| format!("The update list is missing its build: {e}"))
    }
}

/// What the newest build has that this copy doesn't.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Behind {
    /// Commits behind.
    pub count: u32,
    /// This copy's commit isn't in the newest build's list, because it's older
    /// than the list goes back or `main` was rewritten, so `count` comes from
    /// the build numbers.
    pub estimated: bool,
    /// The newest of those commits, newest first, at most `SHOWN`.
    pub commits: Vec<Commit>,
}

/// How many commit subjects Settings lists.
pub const SHOWN: usize = 10;

/// How far this copy, built as `build` from `commit`, is behind `newest`.
pub fn behind(build: u32, commit: &str, newest: &NewestBuild) -> Behind {
    let found = newest.commits.iter().position(|c| c.sha == commit);
    let count = match found {
        Some(i) => i as u32,
        None => newest.number.saturating_sub(build),
    };
    Behind {
        count,
        estimated: found.is_none(),
        commits: newest
            .commits
            .iter()
            .take((count as usize).min(SHOWN))
            .cloned()
            .collect(),
    }
}

/// The last check and the update it found, if any.
#[derive(Default)]
pub struct Updates(Mutex<Check>);

#[derive(Default)]
struct Check {
    checking: bool,
    installing: bool,
    /// Milliseconds since the epoch.
    checked_at: Option<i64>,
    available: Option<Available>,
    /// What Update installs: the same build as `available`.
    pending: Option<Update>,
    /// The last check or install failed; a finished check clears it.
    error: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct SavedCheck {
    checked_at: Option<i64>,
    manifest: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Available {
    build: u32,
    behind: Behind,
}

/// Settings → About and the sidebar dot.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdatesView {
    version: String,
    build: Option<u32>,
    /// Short.
    commit: Option<String>,
    /// Installed by the installer from a CI build, so it can update itself.
    can_update: bool,
    checking: bool,
    installing: bool,
    checked_at: Option<i64>,
    available: Option<Available>,
    error: Option<String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Progress {
    downloaded: u64,
    total: Option<u64>,
}

fn installed(app: &AppHandle) -> Installed {
    Installed::from_version(&app.package_info().version)
}

fn can_update(app: &AppHandle) -> bool {
    installed(app).build.is_some()
        && COMMIT.is_some()
        && tauri::utils::platform::bundle_type() == Some(BundleType::Nsis)
}

fn view(app: &AppHandle, check: &Check) -> UpdatesView {
    let Installed { version, build } = installed(app);
    UpdatesView {
        version,
        build,
        commit: COMMIT.map(|c| c.chars().take(7).collect()),
        can_update: can_update(app),
        checking: check.checking,
        installing: check.installing,
        checked_at: check.checked_at,
        available: check.available.clone(),
        error: check.error.clone(),
    }
}

fn changed(app: &AppHandle) {
    let updates = app.state::<Updates>();
    let view = view(app, &updates.0.lock().unwrap());
    let _ = app.emit("updates-changed", view);
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

/// Checks on start and every few hours, if this copy can update itself.
pub fn start(app: &AppHandle) {
    if !can_update(app) {
        return;
    }
    // Keep the last successful result visible when starting offline. Restored
    // updates still verify the package and its signed version before install.
    if let Ok(dir) = app.path().app_config_dir()
        && let Ok(bytes) = fs::read(dir.join("updates.json"))
        && let Ok(saved) = serde_json::from_slice::<SavedCheck>(&bytes)
    {
        let updates = app.state::<Updates>();
        let mut check = updates.0.lock().unwrap();
        check.checked_at = saved.checked_at;
        if let Some(manifest) = saved.manifest
            && let Ok(updater) = updater(app)
            && let Ok(Some(update)) = updater.restore_update(manifest)
            && let Ok(available) = available(app, &update)
        {
            check.available = Some(available);
            check.pending = Some(update);
        }
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(FIRST_CHECK).await;
        check(&app).await;
        loop {
            tokio::time::sleep(TICK).await;
            let last = app.state::<Updates>().0.lock().unwrap().checked_at;
            if last.is_none_or(|t| now_ms() - t >= EVERY.as_millis() as i64) {
                check(&app).await;
            }
        }
    });
}

async fn check(app: &AppHandle) {
    {
        let updates = app.state::<Updates>();
        let mut check = updates.0.lock().unwrap();
        if check.checking || check.installing {
            return;
        }
        check.checking = true;
    }
    changed(app);
    let found = find(app).await;
    {
        let updates = app.state::<Updates>();
        let mut check = updates.0.lock().unwrap();
        check.checking = false;
        check.checked_at = Some(now_ms());
        match found {
            Ok(found) => {
                check.error = None;
                (check.available, check.pending) = match found {
                    Some((available, update)) => (Some(available), Some(update)),
                    None => (None, None),
                };
            }
            // Keeps what the last good check found.
            Err(e) => check.error = Some(e),
        }
        if let Ok(dir) = app.path().app_config_dir() {
            let saved = SavedCheck {
                checked_at: check.checked_at,
                manifest: check.pending.as_ref().map(|u| u.raw_json.clone()),
            };
            if let Ok(bytes) = serde_json::to_vec(&saved) {
                let temp = dir.join("updates.json.tmp");
                let _ = fs::create_dir_all(&dir)
                    .and_then(|()| fs::write(&temp, bytes))
                    .and_then(|()| fs::rename(temp, dir.join("updates.json")));
            }
        }
    }
    changed(app);
}

async fn find(app: &AppHandle) -> Result<Option<(Available, Update)>, String> {
    let Some(update) = updater(app)?.check().await.map_err(|e| e.to_string())? else {
        return Ok(None);
    };
    Ok(Some((available(app, &update)?, update)))
}

fn updater(app: &AppHandle) -> Result<tauri_plugin_updater::Updater, String> {
    let handle = app.clone();
    app.updater_builder()
        .timeout(TIMEOUT)
        .on_before_exit(move || before_install_exit(&handle))
        .build()
        .map_err(|e| e.to_string())
}

fn available(app: &AppHandle, update: &Update) -> Result<Available, String> {
    let build = installed(app)
        .build
        .ok_or("This copy has no build number.")?;
    let commit = COMMIT.ok_or("This copy has no commit.")?;
    let newest = NewestBuild::from_manifest(&update.raw_json)?;
    let version = Version::parse(&update.version).map_err(|e| e.to_string())?;
    if version.build.as_str().parse::<u32>().ok() != Some(newest.number) {
        return Err("The update version and build number don't match.".into());
    }
    Ok(Available {
        build: newest.number,
        behind: behind(build, commit, &newest),
    })
}

#[tauri::command]
pub fn get_updates(app: AppHandle, updates: State<'_, Updates>) -> UpdatesView {
    view(&app, &updates.0.lock().unwrap())
}

#[tauri::command]
pub async fn check_for_updates(app: AppHandle) -> UpdatesView {
    if can_update(&app) {
        check(&app).await;
    }
    view(&app, &app.state::<Updates>().0.lock().unwrap())
}

/// Downloads the update, then quits: the installer replaces the files and
/// starts spark-rs again. Returns only if that fails.
#[tauri::command]
pub async fn install_update(app: AppHandle) -> Result<(), String> {
    let update = {
        let updates = app.state::<Updates>();
        let mut check = updates.0.lock().unwrap();
        if check.installing || check.checking {
            return Ok(());
        }
        let Some(update) = check.pending.take() else {
            return Err("There's no update to install. Check again.".into());
        };
        check.installing = true;
        check.error = None;
        update
    };
    changed(&app);
    let mut downloaded = 0;
    let progress = app.clone();
    let result = update
        .download_and_install(
            |chunk, total| {
                downloaded += chunk as u64;
                let _ = progress.emit("update-progress", Progress { downloaded, total });
            },
            || {},
        )
        .await;
    {
        let updates = app.state::<Updates>();
        let mut check = updates.0.lock().unwrap();
        check.installing = false;
        check.pending = Some(update);
        if let Err(e) = &result {
            check.error = Some(e.to_string());
        }
    }
    changed(&app);
    result.map_err(|e| e.to_string())
}

/// Runs as the update quits the app for the installer: the same cleanup as
/// Quit, so agents don't find a stale address.
fn before_install_exit(app: &AppHandle) {
    agents::stopped(app);
    app.cleanup_before_exit();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_ci_build_carries_its_build_number_in_the_version() {
        let installed = Installed::from_version(&Version::parse("0.2.0+123").unwrap());
        assert_eq!(
            installed,
            Installed {
                version: "0.2.0".into(),
                build: Some(123)
            }
        );
    }

    #[test]
    fn a_copy_ci_didnt_build_has_no_build_number() {
        let installed = Installed::from_version(&Version::parse("0.2.0").unwrap());
        assert_eq!(installed.build, None);
        assert_eq!(installed.version, "0.2.0");
    }

    #[test]
    fn the_manifest_says_which_build_is_newest() {
        let manifest = serde_json::json!({
            "version": "0.2.0+125",
            "platforms": {},
            "build": {
                "number": 125,
                "commit": "c125",
                "commits": [
                    { "sha": "c125", "subject": "Show commits behind in Settings" },
                    { "sha": "c124", "subject": "Fix the sidebar dot" }
                ]
            }
        });
        assert_eq!(
            NewestBuild::from_manifest(&manifest).unwrap(),
            NewestBuild {
                number: 125,
                commit: "c125".into(),
                commits: vec![
                    Commit {
                        sha: "c125".into(),
                        subject: "Show commits behind in Settings".into()
                    },
                    Commit {
                        sha: "c124".into(),
                        subject: "Fix the sidebar dot".into()
                    },
                ],
            }
        );
    }

    #[test]
    fn a_manifest_without_build_data_is_refused() {
        let manifest = serde_json::json!({ "version": "0.2.0+125", "platforms": {} });
        assert!(NewestBuild::from_manifest(&manifest).is_err());
    }

    fn commit(n: u32) -> Commit {
        Commit {
            sha: format!("c{n}"),
            subject: format!("Change {n}"),
        }
    }

    /// Build `number`, listing the commits from `number` down to `oldest`.
    fn newest(number: u32, oldest: u32) -> NewestBuild {
        NewestBuild {
            number,
            commit: format!("c{number}"),
            commits: (oldest..=number).rev().map(commit).collect(),
        }
    }

    #[test]
    fn counts_the_commits_newer_than_this_copys() {
        assert_eq!(
            behind(122, "c122", &newest(125, 100)),
            Behind {
                count: 3,
                estimated: false,
                commits: vec![commit(125), commit(124), commit(123)],
            }
        );
    }

    #[test]
    fn lists_only_the_newest_subjects_but_counts_them_all() {
        let far = behind(100, "c100", &newest(130, 90));
        assert_eq!(far.count, 30);
        assert!(!far.estimated);
        assert_eq!(
            far.commits,
            (121..=130).rev().map(commit).collect::<Vec<_>>()
        );
    }

    #[test]
    fn counts_by_build_number_when_this_copys_commit_isnt_listed() {
        assert_eq!(
            behind(40, "c40-before-a-rewrite", &newest(45, 43)),
            Behind {
                count: 5,
                estimated: true,
                commits: vec![commit(45), commit(44), commit(43)],
            }
        );
    }
}
