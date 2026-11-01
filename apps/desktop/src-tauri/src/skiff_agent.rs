//! The Skiff agent's settings, folder and session, for the page. The folder
//! setup and the find-or-start rule live in `skiff_client::skipper`, which
//! `skiff skipper` uses too, so the app and the CLI pick the same session.

use std::path::PathBuf;

use serde::Serialize;
use skiff_client::{
    agent_folder::{self, Mode},
    fleet,
    host::data,
    skipper,
};
use skiff_core::{config, session::SessionInfo};
use skiff_host_api::{HostSkipper, SkipperEdit, SkipperFolderDone, SkipperFolderPreview, TakenSkipper};
use tauri::{AppHandle, Emitter, State};

use crate::{
    config::PROJECTS_CHANGED,
    connection::{blocking, bundled, data_write, ensure_client, err, App},
};
use skiff_client::host::local::ProcessControl;

/// Runs at app start, off the main thread: makes this computer's agent
/// folder when it is missing, with Skipper on or off. A remote Host's
/// `skiff-host` makes its own.
pub(crate) fn make_folder() {
    if let Err(e) = config::load().and_then(|cfg| skipper::make_folder(&cfg)) {
        eprintln!("skiff agent folder: {e:#}");
    }
}

/// Runs off the main thread at app start, and again on a switch to this Mac.
/// Never prompts. Copies the built-in `skiff-cli` skill out when it changed:
/// this Mac's Skipper setup, so only while this Mac is the shown Host, and
/// once per app run. True when it ran now.
pub(crate) fn sync(app: &App) -> bool {
    use std::sync::atomic::{AtomicBool, Ordering};
    static DONE: AtomicBool = AtomicBool::new(false);
    if app.remote() || DONE.swap(true, Ordering::SeqCst) {
        return false;
    }
    if let Err(e) = skipper::sync_skill() {
        eprintln!("skiff-cli skill: {e:#}");
    }
    true
}

/// The `skiff` CLI beside the app binary, else the one on `PATH`.
fn cli() -> PathBuf {
    bundled("skiff", None)
}

/// Starts `skiff watch` for the agent's session, unless one runs for this
/// daemon. The page calls it on every start and every restore. True when it
/// started one. A remote Host starts its own, which its `skiff-host` keeps.
#[tauri::command]
pub async fn start_skiff_watch(app: State<'_, App>, session: String) -> Result<bool, String> {
    let Some(local) = app.selection().process_control() else {
        return data_write(&app, |c| async move { data::skipper_watch(&c, &session).await }).await;
    };
    ensure_watch(&app, &local, cli(), session).await
}

/// One current watcher for the agent's session. A watcher of an older CLI,
/// as after an update, gets one replacement attempt per app run when it is
/// proven to be this daemon's `skiff watch`. One that is not, or that does
/// not stop, keeps running, and only the log says so: the update is not
/// finished, and nothing asks the user. One check runs at a time. It starts
/// and signals processes, so only a local endpoint may: `_local` proves it.
pub(crate) async fn ensure_watch(app: &App, _local: &ProcessControl, cli: PathBuf, session: String) -> Result<bool, String> {
    let _one = app.watch_gate.lock().await;
    let mut known = app.watch_pids.lock().unwrap().clone();
    if let Ok(list) = crate::connection::inventory_now(app).await {
        known.extend(list.iter().filter(|s| s.id == session).filter_map(|s| s.watch.as_ref()).flat_map(|w| w.pids.iter().copied()));
    }
    // Only the bundled CLI replaces a watcher: a bare `skiff` from PATH is
    // not known to be this app's build.
    let tried = cli.is_absolute().then(|| app.watch_tried.clone());
    let socket = app.selection().socket().to_path_buf();
    let out = blocking(move || skipper::ensure_watch(&socket, &cli, skipper::CLI_VERSION, &session, &known, tried.as_deref())).await?;
    match out {
        skipper::WatchOutcome::Started => Ok(true),
        skipper::WatchOutcome::Current => Ok(false),
        skipper::WatchOutcome::Replaced { pid } => {
            eprintln!("skiff watch: replaced the older watcher (pid {pid})");
            Ok(true)
        }
        skipper::WatchOutcome::Kept(why) => {
            eprintln!("skiff watch not updated: {why}");
            Ok(false)
        }
    }
}

/// The watch log beside the socket, for the row's Open watch log.
#[tauri::command]
pub fn skiff_watch_log() -> String {
    skiff_core::socket::socket_path().with_extension("watch.log").display().to_string()
}

/// What Start project Skipper did.
#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProjectStart {
    /// The project Skipper runs: its session id.
    Started { id: String },
    /// This Mac's skiffd is too old to mark a project Skipper. Nothing
    /// started. The page offers Update skiffd (`update_daemon`).
    DaemonOlder { version: String },
}

/// Project menu › Start project Skipper: finds or starts the project's
/// Skipper, sends a new one SKIPPER.md as its first message, and starts the
/// watcher. This Mac runs the bundled `skiff` (local process control), and
/// refuses first when its skiffd is too old to mark one; a remote Host does
/// it there, through its `skiff-host` (`skipper_run@1`).
#[tauri::command]
pub async fn start_project_skipper(app: State<'_, App>, project: String) -> Result<ProjectStart, String> {
    if app.selection().process_control().is_none() {
        let s = data_write(&app, |c| async move { data::skipper_project(&c, &project).await }).await?;
        return Ok(ProjectStart::Started { id: s.id });
    }
    let (c, _) = ensure_client(&app).await?;
    let daemon = c.daemon_info().await.map_err(err)?;
    if skipper::too_old_for_project(&daemon.version) {
        return Ok(ProjectStart::DaemonOlder { version: daemon.version });
    }
    let socket = app.selection().socket().to_path_buf();
    let id = blocking(move || skipper::start_project(&socket, &cli(), &project)).await?;
    Ok(ProjectStart::Started { id })
}

/// The Skiff agent's session, and what the open did to it.
#[derive(Serialize)]
pub struct Opened {
    session: SessionInfo,
    created: bool,
    typed: bool,
    /// Why its watcher did not start, for the row's tooltip.
    watch_error: Option<String>,
}

/// The row and the key: find the Skiff agent's session or start it, through
/// the daemon's guarded request, and start its watcher. With `restore`, the
/// `input_gen` the page read: only continue a restored shell, as Settings
/// says. `null` when there is nothing to open. A remote Host does it all
/// there, through its `skiff-host`.
#[tauri::command]
pub async fn open_skiff_agent(app: State<'_, App>, restore: Option<u64>) -> Result<Option<Opened>, String> {
    if app.remote() {
        let opened = data_write(&app, |c| async move { data::skipper_open(&c, restore).await }).await?;
        return Ok(opened.map(|o| Opened {
            session: skiff_client::host::restore_session(o.session),
            created: o.created,
            typed: o.typed,
            watch_error: o.watch_error,
        }));
    }
    let (c, _) = ensure_client(&app).await?;
    let how = match restore {
        Some(expect) => skipper::Open::Restore { expect },
        None => skipper::Open::Start,
    };
    let Some(found) = skipper::open(&c, how).await.map_err(err)? else { return Ok(None) };
    let watch_error = match app.selection().process_control() {
        Some(local) => ensure_watch(&app, &local, cli(), found.session.id.clone()).await.err(),
        None => None,
    };
    if let Some(e) = &watch_error {
        eprintln!("skiff watch: {e}");
    }
    Ok(Some(Opened { session: found.session, created: found.created, typed: found.typed, watch_error }))
}

/// `[skiff_agent]` and its task types, from the selected Host (`skipper@1`).
/// This Mac's come from the config this app writes ([`set_skiff_agent`]),
/// with the same function skiff-host uses, so a change always reads back:
/// an older local skiff-host does not know every setting.
#[tauri::command]
pub async fn get_skiff_agent(app: State<'_, App>) -> Result<HostSkipper, String> {
    if app.remote() {
        return crate::connection::data_read(&app, |c| async move { data::skipper_settings(&c).await }).await;
    }
    blocking(local).await
}

fn local() -> anyhow::Result<HostSkipper> {
    skipper::settings(&config::load()?)
}

/// Saves one setting on the selected Host: This Mac's config here, a remote
/// Host's through its `skiff-host` (`skipper_config@1`). Answers with all of
/// them: a remote Host's read again through it, This Mac's from the config
/// just written, so an old or stopped skiff-host cannot fail the change.
/// The page reloads its projects on the event, so the row follows at once.
#[tauri::command]
pub async fn set_skiff_agent(app: State<'_, App>, handle: AppHandle, edit: SkipperEdit) -> Result<HostSkipper, String> {
    if app.remote() {
        let settings = data_write(&app, |c| async move { data::skipper_edit(&c, edit).await }).await?;
        let _ = handle.emit(PROJECTS_CHANGED, ());
        return Ok(settings);
    }
    let settings = blocking(move || {
        skipper::apply(&edit)?;
        local()
    })
    .await?;
    let _ = handle.emit(PROJECTS_CHANGED, ());
    Ok(settings)
}

/// Task types › Edit: the type's instruction file, with its `# <type>`
/// line written when it is missing, on the selected Host. It opens as a
/// Skipper's file does.
#[tauri::command]
pub async fn edit_task_type_file(app: State<'_, App>, handle: AppHandle, name: String) -> Result<crate::commands::Opened, String> {
    edit_skipper_file(app, handle, skiff_host_api::HostFileScope::Type(name)).await
}

/// Settings › Skippers › Instructions: each Skipper's file and its
/// status, on the selected Host (`skipper@1` carries them for a remote one).
#[tauri::command]
pub async fn skipper_files(app: State<'_, App>) -> Result<Vec<skiff_host_api::HostInstruction>, String> {
    if app.remote() {
        let s = crate::connection::data_read(&app, |c| async move { data::skipper_settings(&c).await }).await?;
        return Ok(s.instructions);
    }
    blocking(|| skipper::host_instructions(&config::load()?)).await
}

/// Edit: the file opens as Settings › Open with › Text files says. A
/// missing file is written from the template first. On a remote Host, a
/// local copy opens in an app on this computer, and each save goes back
/// (`skipper_file@1`). The page shows a peek command; an app or the
/// default app starts here.
#[tauri::command]
pub async fn edit_skipper_file(app: State<'_, App>, handle: AppHandle, scope: skiff_host_api::HostFileScope) -> Result<crate::commands::Opened, String> {
    let remote = app.remote();
    let path = if remote { crate::host_files::fetch(&app, handle, scope).await? } else { blocking(move || skipper::edit_host_file(&config::load()?, &scope)).await? };
    crate::commands::open_text(path, remote).await
}

/// The saves of a remote Host's copy that did not go back since the last
/// call, for one line under each row.
#[tauri::command]
pub fn skipper_file_errors() -> Vec<crate::host_files::FileError> {
    crate::host_files::take_errors()
}

/// Reset…: the template again, the old file in `<name>.bak`, on the
/// selected Host.
#[tauri::command]
pub async fn reset_skipper_file(app: State<'_, App>, scope: skiff_host_api::HostFileScope) -> Result<(), String> {
    if app.remote() {
        return data_write(&app, |c| async move { data::skipper_file_reset(&c, scope).await }).await;
    }
    blocking(move || skipper::reset_file(&config::load()?, &skipper::scope_of(&scope)?).map(|_| ())).await
}

/// Fleet menu › Start fleet Skipper: finds or starts the fleet's Skipper,
/// takes the fleet's project Skippers over (the page asked first), sends a
/// new one SKIPPER.md as its first message, and starts the watcher. Returns
/// its session id. This Mac runs the bundled `skiff skipper --fleet --yes`,
/// and refuses first when its skiffd is too old to retire a project
/// Skipper; a remote Host does it there, through its `skiff-host`
/// (`skipper_run@2`).
#[tauri::command]
pub async fn start_fleet_skipper(app: State<'_, App>, fleet: String) -> Result<String, String> {
    if app.selection().process_control().is_none() {
        let s = data_write(&app, |c| async move { data::skipper_fleet(&c, &fleet).await }).await?;
        return Ok(s.id);
    }
    let (c, _) = ensure_client(&app).await?;
    let daemon = c.daemon_info().await.map_err(err)?;
    if skipper::too_old_for_fleet(&daemon.version) {
        return Err(format!("skiffd {} is older than this Skiff and cannot start a fleet Skipper. Update skiffd first", daemon.version));
    }
    let socket = app.selection().socket().to_path_buf();
    blocking(move || skipper::start_fleet(&socket, &cli(), &fleet)).await
}

/// The project Skippers a takeover of `fleet` would stop, for the confirm:
/// those of its projects, and of `join` when it joins. Changes nothing.
#[tauri::command]
pub async fn fleet_takeover_preview(app: State<'_, App>, fleet: String, join: Option<String>) -> Result<Vec<TakenSkipper>, String> {
    if app.remote() {
        return crate::connection::data_read(&app, |c| async move { data::fleet_takeover_preview(&c, &fleet, join.as_deref()).await }).await;
    }
    let (c, _) = ensure_client(&app).await?;
    let taken = fleet::preview(&c, &fleet, join.as_deref()).await.map_err(err)?;
    Ok(taken.iter().map(fleet::Taken::to_host).collect())
}

/// Add to fleet…, Move to fleet… or a drop, on the person's confirm: the
/// running fleet Skipper of `fleet` takes `join`'s project Skipper over,
/// then `join` joins the fleet. A remote Host does it there.
#[tauri::command]
pub async fn fleet_takeover(app: State<'_, App>, handle: AppHandle, fleet: String, join: String) -> Result<(), String> {
    let out = if app.remote() {
        data_write(&app, |c| async move { data::fleet_takeover(&c, &fleet, &join).await }).await.map(|_| ())
    } else {
        let (c, _) = ensure_client(&app).await?;
        let socket = app.selection().socket().to_path_buf();
        fleet::takeover(&c, &socket, &fleet, Some(&join), true).await.map(|_| ()).map_err(err)
    };
    let _ = handle.emit(PROJECTS_CHANGED, ());
    out
}

/// Rename fleet…: the fleet, its Skipper's folder, rows, session and mark.
/// A remote Host does it there.
#[tauri::command]
pub async fn rename_fleet(app: State<'_, App>, handle: AppHandle, old: String, new: String) -> Result<(), String> {
    let out = if app.remote() {
        data_write(&app, |c| async move { data::fleet_rename(&c, &old, &new).await }).await
    } else {
        let (c, _) = ensure_client(&app).await?;
        fleet::rename(&c, &old, new.trim()).await.map_err(err)
    };
    let _ = handle.emit(PROJECTS_CHANGED, ());
    out
}

/// The chosen folder, resolved and read on the selected Host. A relative
/// path resolves against the Host's home folder. Changes nothing.
#[tauri::command]
pub async fn preview_skiff_folder(app: State<'_, App>, path: String) -> Result<SkipperFolderPreview, String> {
    if app.remote() {
        return crate::connection::data_read(&app, |c| async move { data::skipper_folder_preview(&c, &path).await }).await;
    }
    let (panes, skipper) = live(&app).await;
    blocking(move || {
        let home = skipper::home()?;
        let to = agent_folder::resolve(&path, &home, &home)?;
        let preview = agent_folder::preview(&config::load()?, &to, &panes)?;
        Ok(preview.to_host(skipper.is_some()))
    })
    .await
}

/// The live panes and Skipper's session. With no daemon, none run.
async fn live(app: &App) -> (agent_folder::Panes, Option<SessionInfo>) {
    match ensure_client(app).await {
        Ok((c, _)) => agent_folder::sessions(&c).await.unwrap_or_default(),
        Err(_) => Default::default(),
    }
}

/// Copy, check and switch, start empty, or switch to a Skiff folder. `path`
/// is the preview's resolved folder. Skipper's session ends first when
/// `end_skipper`, else a running one stops the change. `anyway` lets the
/// change run while tasks still run. A remote Host runs it there.
#[tauri::command]
pub async fn change_skiff_folder(
    app: State<'_, App>,
    handle: AppHandle,
    path: PathBuf,
    mode: Mode,
    end_skipper: bool,
    anyway: bool,
) -> Result<SkipperFolderDone, String> {
    if app.remote() {
        let path = path.to_string_lossy().into_owned();
        let done = data_write(&app, |c| async move { data::skipper_folder_change(&c, &path, mode.into(), end_skipper, anyway).await }).await?;
        let _ = handle.emit(PROJECTS_CHANGED, ());
        return Ok(done);
    }
    let (mut panes, skipper) = live(&app).await;
    if let Some(s) = skipper {
        if !end_skipper {
            return Err("Admiral runs. It ends before the change.".into());
        }
        let c = crate::connection::host_now(&app).await?;
        skiff_client::host::local::legacy_kill(&c, &s.id).await.map_err(err)?;
        panes.remove(&s.id);
    }
    let done = blocking(move || agent_folder::change(&config::load()?, &path, mode, &panes, anyway)).await?;
    let _ = handle.emit(PROJECTS_CHANGED, ());
    Ok(done.to_host())
}

#[cfg(test)]
mod tests {
    use skiff_host_api::SkipperEdit as Edit;

    /// Review P2: a launch with a remote Host selected leaves this Mac's
    /// Skipper skill alone (private config; the review's repro wrote it).
    #[test]
    fn a_remote_launch_does_not_set_up_this_macs_skipper() {
        let _env = crate::reload::tests::ENV.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("skiff-remote-skill-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let old = std::env::var_os("SKIFF_CONFIG");
        std::env::set_var("SKIFF_CONFIG", dir.join("projects.toml"));
        let app = crate::connection::App::default();
        let target = skiff_client::host::ssh::SshTarget::new("devbox", "/s.sock");
        let options = skiff_client::host::ssh::SshOptions { program: "/nonexistent/ssh".into(), config: None };
        app.set_selection(skiff_client::host::local::HostSelection::ssh(target, options, dir.join("fwd"), None).unwrap());
        assert!(!super::sync(&app));
        assert!(!dir.join("skills").exists(), "a remote launch wrote this Mac's skill");
        match old {
            Some(v) => std::env::set_var("SKIFF_CONFIG", v),
            None => std::env::remove_var("SKIFF_CONFIG"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// This Mac's Skipper model reads back what it just wrote, from
    /// projects.toml, with no skiff-host asked.
    #[test]
    fn this_mac_reads_back_the_skipper_model() {
        let _env = crate::reload::tests::ENV.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("skiff-read-back-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let old = std::env::var_os("SKIFF_CONFIG");
        std::env::set_var("SKIFF_CONFIG", dir.join("projects.toml"));
        assert_eq!(super::local().unwrap().model, None);
        super::skipper::apply(&Edit::Model(Some("opus".into()))).unwrap();
        super::skipper::apply(&Edit::ProjectModel(Some("sonnet".into()))).unwrap();
        let s = super::local().unwrap();
        assert_eq!((s.model.as_deref(), s.project_model.as_deref()), (Some("opus"), Some("sonnet")));
        super::skipper::apply(&Edit::Model(None)).unwrap();
        assert_eq!(super::local().unwrap().model, None);
        match old {
            Some(v) => std::env::set_var("SKIFF_CONFIG", v),
            None => std::env::remove_var("SKIFF_CONFIG"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The shapes `SkiffAgentEdit` in apps/desktop/src/appearance/skiffAgentSettings.ts sends.
    #[test]
    fn reads_the_task_type_edits_the_page_sends() {
        let edit = |v: serde_json::Value| serde_json::from_value::<Edit>(v).unwrap();
        let add = edit(serde_json::json!({ "key": "add_task", "value": { "type": "review", "agent": "claude", "model": "opus" } }));
        assert!(matches!(add, Edit::AddTask(t) if t.name == "review" && t.agent == "claude" && t.model.as_deref() == Some("opus")));
        let set = edit(serde_json::json!({ "key": "task", "value": { "type": "review", "agent": "aider", "model": null } }));
        assert!(matches!(set, Edit::Task(t) if t.model.is_none()));
        let rename = edit(serde_json::json!({ "key": "rename_task", "value": { "from": "review", "to": "code-review" } }));
        assert!(matches!(rename, Edit::RenameTask { from, to } if from == "review" && to == "code-review"));
        assert!(matches!(edit(serde_json::json!({ "key": "remove_task", "value": "review" })), Edit::RemoveTask(t) if t == "review"));
        assert!(matches!(edit(serde_json::json!({ "key": "name", "value": null })), Edit::Name(None)));
        assert!(matches!(edit(serde_json::json!({ "key": "model", "value": "opus" })), Edit::Model(Some(m)) if m == "opus"));
        assert!(matches!(edit(serde_json::json!({ "key": "project_model", "value": null })), Edit::ProjectModel(None)));
        // The keys Skippers dropped.
        assert!(serde_json::from_value::<Edit>(serde_json::json!({ "key": "enabled", "value": true })).is_err());
        assert!(serde_json::from_value::<Edit>(serde_json::json!({ "key": "project_skippers", "value": true })).is_err());
    }
}
