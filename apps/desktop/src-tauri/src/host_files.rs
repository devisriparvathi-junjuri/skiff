//! Settings › Skippers › Edit on a remote Host (`skipper_file@1`). Edit
//! fetches the Host's file into `hosts/<host>/skiff-agent/<same path>`
//! beside `projects.toml`, and the page opens that copy. A watcher sends
//! each save back with the revision it read. A Host whose file changed
//! since then refuses the write: the copy stays, nothing more is sent, and
//! the page shows one line under the row the next time it reads the files
//! ([`take_errors`]). The next Edit replaces the copy and stops the old
//! watcher.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Mutex,
    time::{Duration, SystemTime},
};

use serde::Serialize;
use skiff_client::host::data;
use skiff_host_api::{Error as E, HostFileScope, HostId};
use tauri::{AppHandle, Manager};

use crate::connection::{data_now, data_read, App};

/// How often the watcher looks at the copy.
const EVERY: Duration = Duration::from_secs(1);

/// A save that did not go back, for the line under its row.
#[derive(Clone, Serialize)]
pub(crate) struct FileError {
    /// The file's path in the Host's Skipper folder, as its row shows it.
    file: String,
    /// The Host's file changed after Edit fetched it.
    conflict: bool,
    message: String,
}

/// The saves that did not go back since the page last read them.
static ERRORS: Mutex<Vec<FileError>> = Mutex::new(Vec::new());

/// The saves that did not go back since the last call, oldest first.
pub(crate) fn take_errors() -> Vec<FileError> {
    std::mem::take(&mut *ERRORS.lock().unwrap())
}

/// The watcher each copy has now. A new Edit of the same copy stops the old one.
static WATCHING: Mutex<Option<HashMap<PathBuf, u64>>> = Mutex::new(None);

/// Fetches the file of `scope` from the selected Host into its local copy,
/// starts the watcher, and returns the copy's path.
pub(crate) async fn fetch(app: &App, handle: AppHandle, scope: HostFileScope) -> Result<PathBuf, String> {
    let s = scope.clone();
    let (file, host) = data_read(app, |c| async move {
        let host = c.hello().host.host_id.clone();
        data::skipper_file_read(&c, s).await.map(|f| (f, host))
    })
    .await?;
    let rel = skiff_client::skipper::host_file(&scope).map_err(|e| format!("{e:#}"))?;
    let copy = copy_path(&host, &rel);
    if let Some(dir) = copy.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    }
    std::fs::write(&copy, &file.text).map_err(|e| format!("write {}: {e}", copy.display()))?;
    let gen = {
        let mut w = WATCHING.lock().unwrap();
        let map = w.get_or_insert_with(HashMap::new);
        let n = map.get(&copy).map_or(1, |n| n + 1);
        map.insert(copy.clone(), n);
        n
    };
    tauri::async_runtime::spawn(watch(handle, host, scope, rel, copy.clone(), file.revision, gen));
    Ok(copy)
}

/// `hosts/<host>/skiff-agent/<rel>` beside `projects.toml`.
fn copy_path(host: &HostId, rel: &str) -> PathBuf {
    let id: String = host.as_str().chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect();
    skiff_core::config::config_path().with_file_name("hosts").join(id).join("skiff-agent").join(rel)
}

fn stamp(p: &Path) -> Option<SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

/// Sends each save of `copy` back to `host` with the revision it read.
/// Stops on a conflict, when the copy goes, or when a new Edit takes it.
async fn watch(handle: AppHandle, host: HostId, scope: HostFileScope, rel: String, copy: PathBuf, mut revision: String, gen: u64) {
    let mut seen = stamp(&copy);
    let error = |conflict: bool, message: String| ERRORS.lock().unwrap().push(FileError { file: rel.clone(), conflict, message });
    loop {
        tokio::time::sleep(EVERY).await;
        if WATCHING.lock().unwrap().as_ref().and_then(|m| m.get(&copy)) != Some(&gen) {
            return;
        }
        let now = stamp(&copy);
        if now.is_none() {
            return;
        }
        if now == seen {
            continue;
        }
        seen = now;
        let Ok(text) = std::fs::read_to_string(&copy) else { continue };
        let app = handle.state::<App>();
        let c = match data_now(&app).await {
            Ok(c) => c,
            Err(e) => {
                error(false, e);
                continue;
            }
        };
        // Only the Host the copy came from takes it.
        if c.hello().host.host_id != host {
            return error(false, "Another Host is on screen. Your copy was not sent.".into());
        }
        match data::skipper_file_write(&c, scope.clone(), text, revision.clone()).await {
            Ok(file) => revision = file.revision,
            Err(E::Conflict { .. }) => return error(true, String::new()),
            Err(e) => error(false, data::message(e)),
        }
    }
}
