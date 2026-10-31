//! Settings › Disk use. The tree, the rule and the flush live in
//! `skiff_client::disk`; this reads the folder, the ledger and the daemon's
//! sessions for them. This computer only, also while the window shows a
//! remote Host: a Host's own disk use needs a Host op (v2 follow-up).

use serde::Serialize;
use skiff_client::{
    disk::{self, Expected, Flushed},
    ledger, skipper,
};
use skiff_core::{config, session::SessionInfo};
use tauri::{AppHandle, Emitter, State};

use crate::connection::App;

/// The tree, with the auto flush setting for its row.
#[derive(Serialize)]
pub struct Usage {
    #[serde(flatten)]
    usage: disk::Usage,
    /// The folder with `~` for home.
    shown: String,
    cache_days: u32,
}

/// This computer's sessions, on the open connection. `None` when the daemon
/// does not answer, or while a remote Host is shown: its sessions are not
/// this computer's. Never starts a daemon.
async fn sessions(app: &App) -> Option<Vec<SessionInfo>> {
    if app.remote() {
        return None;
    }
    let c = app.live_session().await?;
    skiff_client::host::local::legacy_inventory(&c).await.ok()
}

/// The folder, its cache and the ledger. Makes the folder when it is missing.
fn places() -> anyhow::Result<(std::path::PathBuf, std::path::PathBuf, std::path::PathBuf, u32)> {
    let cfg = config::load()?;
    let folder = skipper::make_folder(&cfg)?;
    Ok((folder.clone(), skipper::cache_dir(&cfg)?, ledger::path()?, cfg.skiff_agent.cache_days()))
}

fn count(sessions: Option<Vec<SessionInfo>>) -> anyhow::Result<Usage> {
    let (folder, cache, ledger, cache_days) = places()?;
    let usage = disk::usage(&folder, &cache, &ledger, sessions.as_deref(), &skiff_core::state::socket());
    let home = skipper::home().ok();
    let shown = match home.as_deref().and_then(|h| usage.folder.strip_prefix(h).ok()) {
        Some(rel) if rel.as_os_str().is_empty() => "~".to_string(),
        Some(rel) => format!("~/{}", rel.display()),
        None => usage.folder.display().to_string(),
    };
    Ok(Usage { usage, shown, cache_days })
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> anyhow::Result<T> + Send + 'static) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| format!("{e:#}"))
}

#[tauri::command]
pub async fn disk_usage(app: State<'_, App>) -> Result<Usage, String> {
    let s = sessions(&app).await;
    blocking(move || count(s)).await
}

/// Auto flush's age in days, in this computer's projects.toml. `0`: off.
#[tauri::command]
pub async fn set_cache_days(days: u32) -> Result<(), String> {
    blocking(move || config::set_skiff_agent(config::SkiffAgentKey::CacheDays(days))).await
}

/// Flushes the items the dialog showed. The flush counts again and removes
/// only what is still stale and unchanged. Refused when the daemon does not
/// answer: the rule needs the session list.
#[tauri::command]
pub async fn flush_disk(handle: AppHandle, app: State<'_, App>, items: Vec<Expected>) -> Result<Flushed, String> {
    let Some(s) = sessions(&app).await else {
        return Err("Skiff cannot reach skiffd. A flush needs the session list.".into());
    };
    let out = blocking(move || {
        let (folder, cache, ledger, _) = places()?;
        Ok(disk::flush(&folder, &cache, &ledger, Some(&s), &skiff_core::state::socket(), &items))
    })
    .await;
    let _ = handle.emit(crate::drops::IMAGES_CHANGED, ());
    out
}

/// Runs at app start on its own thread: removes what the rule finds stale
/// and older than `[skiff_agent] cache_days`. It asks skiffd for the
/// sessions and does nothing when skiffd does not answer. One log line, no UI.
pub fn auto_flush() {
    let Ok((folder, cache, ledger, days)) = places() else { return };
    if days == 0 {
        return;
    }
    let socket = skiff_core::state::socket();
    let sessions = tauri::async_runtime::block_on(async {
        let c = skiff_client::Client::connect(&socket).await.ok()?;
        c.list_sessions().await.ok()
    });
    let Some(s) = sessions else {
        eprintln!("auto flush: skiffd did not answer; nothing removed");
        return;
    };
    let out = disk::flush_old(&folder, &cache, &ledger, Some(&s), &socket, days);
    eprintln!("auto flush: removed {} files, {} from {}", out.files, skiff_client::agent_folder::size(out.bytes), folder.display());
}
