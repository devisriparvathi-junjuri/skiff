use std::path::PathBuf;
use skiff_core::{group::Group, project::AgentInfo, session::{SessionInfo, SessionSpec}};
use tauri::{AppHandle, Manager, State};
use skiff_client::host::local::{legacy_agents, legacy_create, legacy_delete_group, legacy_groups, legacy_order_groups, legacy_kill, legacy_rename, legacy_save_group, legacy_seen, legacy_set_theme};
use crate::{connection::{blocking, App, ensure_client, err}, input::Sent};

#[tauri::command]
pub(crate) async fn list_sessions(app: State<'_, App>) -> Result<Vec<SessionInfo>, String> {
    crate::connection::inventory_now(&app).await
}

#[tauri::command]
pub(crate) async fn daemon_metrics(app: State<'_, App>) -> Result<skiff_core::protocol::DaemonMetrics, String> {
    let (c, _) = ensure_client(&app).await?;
    c.daemon_metrics().await.map_err(err)
}

#[tauri::command]
pub(crate) async fn create_session(app: State<'_, App>, mut spec: SessionSpec) -> Result<SessionInfo, String> {
    let c = crate::connection::host_now(&app).await?;
    // The + menu's agent gets the image cache's read flag at the end of its line.
    // This computer's cache only: a remote Host's cache is its own.
    if spec.role == skiff_core::session::Role::Agent && !app.remote() {
        if let Ok(agents) = legacy_agents(&c).await {
            let cfg = skiff_core::config::load().unwrap_or_default();
            skiff_client::cache::add_flags(&cfg, &agents, &mut spec);
        }
    }
    legacy_create(&c, spec).await.map_err(err)
}

/// Not async: it runs on the main thread, one call after another, so
/// keystrokes reach the PTY in order. It waits for no lock and no reply.
/// While a reload runs, or before a connection is up, the input waits in
/// the bridge, in order.
#[tauri::command]
pub(crate) fn pty_write(handle: AppHandle, app: State<'_, App>, session: String, data: String) -> Result<(), String> {
    let live = app.live.lock().unwrap().clone().filter(|c| !c.is_closed());
    if app.remote() {
        return remote_input(live.as_deref(), &session, data.into_bytes());
    }
    match app.input.write(live.as_deref(), &session, data.into_bytes()).map_err(err)? {
        Sent::Done | Sent::Held => {}
        // No connection yet. Rare: connect, then send what waits.
        Sent::Connect => {
            tauri::async_runtime::spawn(async move {
                let app = handle.state::<App>();
                let c = crate::connection::host_now(&app).await.ok();
                app.input.connected(c.as_deref());
            });
        }
    }
    Ok(())
}

/// Input to a remote Host is never held: while the link is down it is off,
/// and what is refused is gone. It never arrives late or twice.
fn remote_input(live: Option<&skiff_client::host::HostConnection>, session: &str, data: Vec<u8>) -> Result<(), String> {
    let Some(c) = live else { return Err(INPUT_OFF.into()) };
    match c.terminal_input(session, data, false) {
        Ok(()) => Ok(()),
        Err(skiff_host_api::Error::HostBusy) => Err(format!(
            "Input refused: the link is slow, and {} KiB wait to be sent. Type again when the terminal catches up.",
            skiff_client::host::local::REMOTE_INPUT_LIMIT / 1024
        )),
        Err(_) => Err(INPUT_OFF.into()),
    }
}

/// Why typing does nothing while a remote Host is disconnected.
pub(crate) const INPUT_OFF: &str = "Input is off: the remote Host is disconnected. Nothing you type is sent.";

#[tauri::command]
pub(crate) async fn pty_resize(
    app: State<'_, App>,
    session: String,
    cols: u16,
    rows: u16,
) -> Result<(), String> {
    let c = crate::connection::host_now(&app).await?;
    skiff_client::host::local::legacy_resize(&c, &session, cols, rows).await.map_err(err)
}

#[tauri::command]
pub(crate) async fn set_session_theme(
    app: State<'_, App>,
    session: String,
    theme: Option<String>,
) -> Result<SessionInfo, String> {
    let c = crate::connection::host_now(&app).await?;
    legacy_set_theme(&c, &session, theme).await.map_err(err)
}

#[tauri::command]
pub(crate) async fn rename_session(
    app: State<'_, App>,
    session: String,
    name: String,
) -> Result<SessionInfo, String> {
    let c = crate::connection::host_now(&app).await?;
    legacy_rename(&c, &session, &name).await.map_err(err)
}

/// The session's pane has the keys: its unread flag and agent exit go.
#[tauri::command]
pub(crate) async fn session_seen(app: State<'_, App>, session: String) -> Result<(), String> {
    let c = crate::connection::host_now(&app).await?;
    legacy_seen(&c, &session).await.map_err(err)
}

/// Mission control's text for one session (#88).
#[tauri::command]
pub(crate) async fn session_screen(app: State<'_, App>, session: String, rows: u16) -> Result<Vec<String>, String> {
    crate::connection::screen_now(&app, &session, rows).await
}

#[tauri::command]
pub(crate) async fn kill_session(app: State<'_, App>, session: String) -> Result<(), String> {
    let c = crate::connection::host_now(&app).await?;
    legacy_kill(&c, &session).await.map_err(err)
}

#[tauri::command]
pub(crate) fn config_path() -> String {
    skiff_core::config::config_path().display().to_string()
}

/// Opens projects.toml in the default editor.
#[tauri::command]
pub(crate) async fn open_config() -> Result<(), String> {
    blocking(|| skiff_core::open::open_default(&skiff_core::config::config_path())).await
}

/// Opens a file or folder in the OS default app.
#[tauri::command]
pub(crate) async fn open_path(path: PathBuf) -> Result<(), String> {
    blocking(move || skiff_core::open::open_default(&path)).await
}

/// Opens a web address in the default browser.
#[tauri::command]
pub(crate) async fn open_url(url: String) -> Result<(), String> {
    blocking(move || skiff_core::open::open_url(&url)).await
}

#[tauri::command]
pub(crate) async fn list_themes() -> Result<Vec<skiff_core::theme::TerminalTheme>, String> {
    // The desktop ships its theme catalogue. A running daemon can be older.
    tokio::task::spawn_blocking(skiff_core::theme::list)
        .await.map_err(|e| e.to_string())
}

#[tauri::command]
pub(crate) async fn import_themes(paths: Vec<PathBuf>) -> Result<Vec<String>, String> {
    tokio::task::spawn_blocking(move || skiff_core::theme::import_files(&paths))
        .await.map_err(|e| e.to_string())
}

#[tauri::command]
pub(crate) async fn open_theme_folder() -> Result<(), String> {
    let path = skiff_core::theme::theme_dir().ok_or("Cannot find the theme folder")?;
    std::fs::create_dir_all(&path).map_err(|e| e.to_string())?;
    blocking(move || skiff_core::open::open_default(&path)).await
}

/// A project's background image from the selected Host (`project_assets@1`):
/// bytes the Host checked, never a path read here. The page makes a blob URL.
#[tauri::command]
pub(crate) async fn project_background(app: State<'_, App>, project: String) -> Result<tauri::ipc::Response, String> {
    let image = crate::connection::data_read(&app, |c| async move { skiff_client::host::data::project_background(&c, &project).await }).await?;
    Ok(tauri::ipc::Response::new(image.bytes))
}

/// One folder of a worktree for the sidebar's file tree, from the selected
/// Host (`files@1`). `worktree` is the Host's; `dir` is relative to it.
/// On This Mac links are followed, so a linked folder expands; on a remote
/// Host they are not.
#[tauri::command]
pub(crate) async fn list_files(app: State<'_, App>, worktree: String, dir: String) -> Result<skiff_host_api::HostListing, String> {
    let follow = !app.remote();
    crate::connection::data_read(&app, |c| async move { skiff_client::host::data::file_list(&c, &worktree, &dir, follow).await }).await
}

/// A file's preview from the selected Host: bounded text, an image's bytes,
/// or what it is. Nothing is read or opened on this Mac.
#[tauri::command]
pub(crate) async fn read_file(app: State<'_, App>, worktree: String, file: String) -> Result<skiff_host_api::HostFile, String> {
    use skiff_host_api::ReadPurpose;
    crate::connection::data_read(&app, |c| async move { skiff_client::host::data::file_read(&c, &worktree, &file, ReadPurpose::Preview).await }).await
}

/// Open with for a Host's file: its bytes (5 MiB at most, checked by the
/// Host) become a read-only copy that this app manages. The copy opens as
/// Settings › Open with says (a configured command, or a peek tool run by
/// the page), else in this Mac's default app. Only the copy's path reaches
/// an opener; the Host's path never does.
#[tauri::command]
pub(crate) async fn open_file_copy(app: State<'_, App>, worktree: String, file: String) -> Result<Opened, String> {
    use skiff_host_api::ReadPurpose;
    // Refused before anything is fetched.
    crate::copies::copy_name(&file).map_err(err)?;
    let name = file.clone();
    let read = crate::connection::data_read(&app, |c| async move { skiff_client::host::data::file_read(&c, &worktree, &file, ReadPurpose::Copy).await }).await?;
    let bytes = read.bytes.ok_or("The Host sent no bytes for this file.")?;
    let copy = blocking(move || crate::copies::save(&name, &bytes)).await?;
    let open = skiff_core::config::client::load().map(|c| c.open).unwrap_or_default();
    let cwd = copy.parent().map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"));
    let plan = skiff_core::open::plan_file(&copy, &open);
    run_plan(plan, cwd, copy).await
}

/// The changed files of one worktree, for the sidebar's Changes section:
/// on a branch, since it left the default branch; else the uncommitted ones.
#[tauri::command]
pub(crate) async fn git_changes(app: State<'_, App>, path: String) -> Result<skiff_host_api::HostChanges, String> {
    crate::connection::data_read(&app, |c| async move { skiff_client::host::data::git_changes(&c, &path).await }).await
}

/// One changed file's diff text, or the whole worktree's, from the selected
/// Host's git (`git_diff@1`), for the page's own viewer. `path` and `file`
/// are the Host's: nothing here reads or opens them on this Mac.
#[tauri::command]
pub(crate) async fn git_diff(app: State<'_, App>, path: String, file: Option<String>) -> Result<skiff_host_api::HostDiff, String> {
    crate::connection::data_read(&app, |c| async move { skiff_client::host::data::git_diff(&c, &path, file.as_deref()).await }).await
}

/// What happened to an open request.
#[derive(serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum Opened {
    /// The page runs `script` in the peek, in `cwd`. It shows only if the
    /// script turns out to be a terminal tool.
    Run { script: String, cwd: PathBuf },
    /// No command is set, and the default app took the file.
    Default,
    /// No command is set, and the OS has no app for the file.
    NoApp { message: String },
}

pub(crate) async fn run_plan(plan: skiff_core::open::Plan, cwd: PathBuf, file: PathBuf) -> Result<Opened, String> {
    use skiff_core::open::Plan;
    tokio::task::spawn_blocking(move || match plan {
        Plan::Run(script) => Ok(Opened::Run { script, cwd }),
        Plan::Default => Ok(match skiff_core::open::open_default(&file) {
            Ok(()) => Opened::Default,
            Err(e) => Opened::NoApp { message: format!("{e:#}") },
        }),
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Shows a changed file's diff, or the whole worktree's, with `[open] diff`.
#[tauri::command]
pub(crate) async fn open_diff(path: PathBuf, file: Option<String>) -> Result<Opened, String> {
    let open = skiff_core::config::client::load().map(|c| c.open).unwrap_or_default();
    let dir = path.clone();
    let plan = tokio::task::spawn_blocking(move || skiff_core::open::plan_diff(&dir, file.as_deref(), &open))
        .await
        .map_err(|e| e.to_string())?;
    run_plan(plan, path.clone(), path).await
}

/// The `[open]` commands, empty for a default, and the default diff command.
#[derive(serde::Serialize)]
pub(crate) struct OpenSettings {
    peek: Vec<String>,
    diff: String,
    text: String,
    markdown: String,
    html: String,
    image: String,
    default_diff: &'static str,
}

fn open_settings_now() -> Result<OpenSettings, String> {
    let o = skiff_core::config::client::load().map_err(err)?.open;
    Ok(OpenSettings {
        peek: o.peek,
        diff: o.diff.unwrap_or_default(),
        text: o.text.unwrap_or_default(),
        markdown: o.markdown.unwrap_or_default(),
        html: o.html.unwrap_or_default(),
        image: o.image.unwrap_or_default(),
        default_diff: skiff_core::git::DEFAULT_DIFF,
    })
}

#[tauri::command]
pub(crate) fn open_settings() -> Result<OpenSettings, String> {
    open_settings_now()
}

/// Sets one `[open]` command. Empty: the default.
#[tauri::command]
pub(crate) fn set_open(key: String, command: String) -> Result<OpenSettings, String> {
    let c = command.trim();
    skiff_core::config::set_open(&key, (!c.is_empty()).then_some(c)).map_err(err)?;
    open_settings_now()
}

#[tauri::command]
pub(crate) fn set_open_peek(key: String, peek: bool) -> Result<OpenSettings, String> {
    skiff_core::config::set_open_peek(&key, peek).map_err(err)?;
    open_settings_now()
}

#[derive(serde::Serialize)]
pub(crate) struct FilePreview {
    script: Option<String>,
    cwd: PathBuf,
    app: String,
    peek: bool,
}

/// Plans a selection preview without starting an external app.
#[tauri::command]
pub(crate) async fn preview_file(path: PathBuf) -> Result<FilePreview, String> {
    tokio::task::spawn_blocking(move || {
        let open = skiff_core::config::client::load().map_err(err)?.open;
        let command = skiff_core::open::command_for(&path, &open);
        let app = command.as_deref().and_then(|c| c.split_whitespace().next())
            .and_then(|c| std::path::Path::new(c).file_name()).and_then(|c| c.to_str())
            .unwrap_or("default app").to_string();
        let script = match skiff_core::open::plan_file(&path, &open) {
            skiff_core::open::Plan::Run(script) => Some(script),
            skiff_core::open::Plan::Default => None,
        };
        let peek = skiff_core::open::previews_file(&path, &open);
        let cwd = path.parent().unwrap_or(std::path::Path::new("/")).to_path_buf();
        Ok(FilePreview { script, cwd, app, peek })
    }).await.map_err(|e| e.to_string())?
}

/// Opens a file with its `[open]` command, else in its default app.
#[tauri::command]
pub(crate) async fn open_file(path: PathBuf) -> Result<Opened, String> {
    let open = skiff_core::config::client::load().map(|c| c.open).unwrap_or_default();
    let file = path.clone();
    let plan = tokio::task::spawn_blocking(move || skiff_core::open::plan_file(&file, &open))
        .await
        .map_err(|e| e.to_string())?;
    let cwd = path.parent().map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"));
    run_plan(plan, cwd, path).await
}

/// Opens a file as Settings › Open with › Text files says, whatever its
/// extension. A peek command goes back to the page; an app starts here.
/// `app_only`: the copy of a remote Host's file, which the peek on that
/// Host cannot open, so a peek command gives the default app.
pub(crate) async fn open_text(path: PathBuf, app_only: bool) -> Result<Opened, String> {
    use skiff_core::open::{self, Plan};
    let o = skiff_core::config::load().map(|c| c.open).unwrap_or_default();
    let cwd = path.parent().map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"));
    let plan = match open::plan_text(&path, &o) {
        Plan::Run(_) if app_only && open::previews_text(&o) => Plan::Default,
        plan => plan,
    };
    match plan {
        Plan::Run(script) if !open::previews_text(&o) => tokio::task::spawn_blocking(move || open::start(&script, &cwd))
            .await
            .map_err(|e| e.to_string())?
            .map(|_| Opened::Default)
            .map_err(|e| format!("{e:#}")),
        plan => run_plan(plan, cwd, path).await,
    }
}

#[tauri::command]
pub(crate) async fn reveal_file(path: PathBuf) -> Result<(), String> {
    blocking(move || reveal(&std::fs::canonicalize(&path)?)).await
}

/// Shows the file selected in its folder.
#[cfg(target_os = "macos")]
pub(crate) fn reveal(path: &std::path::Path) -> anyhow::Result<()> {
    skiff_core::open::launch("open -R", path)
}

/// Asks the file manager to select the file over D-Bus. A file manager
/// without FileManager1 opens the folder instead.
#[cfg(target_os = "linux")]
pub(crate) fn reveal(path: &std::path::Path) -> anyhow::Result<()> {
    use gtk::{gio::{self, prelude::FileExt}, glib::ToVariant};
    let shown = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE).and_then(|bus| {
        let uri = gio::File::for_path(path).uri().to_string();
        bus.call_sync(
            Some("org.freedesktop.FileManager1"),
            "/org/freedesktop/FileManager1",
            "org.freedesktop.FileManager1",
            "ShowItems",
            Some(&(vec![uri], "").to_variant()),
            None,
            gio::DBusCallFlags::NONE,
            5000,
            gio::Cancellable::NONE,
        )
    });
    match shown {
        Ok(_) => Ok(()),
        Err(_) => skiff_core::open::open_default(path.parent().unwrap_or(path)),
    }
}

/// The selected Host's agents, from its own account: its PATH and settings,
/// never this Mac's for a remote Host (`session_agents@1`). This Mac's are
/// read from the config this app writes.
#[tauri::command]
pub(crate) async fn list_agents(app: State<'_, App>) -> Result<Vec<AgentInfo>, String> {
    if !app.remote() {
        return crate::config::local_agents(&app).await;
    }
    let c = crate::connection::host_now(&app).await?;
    legacy_agents(&c).await.map_err(err)
}

#[tauri::command]
pub(crate) async fn list_agent_sessions(app: State<'_, App>, command: Vec<String>, cwd: String) -> Result<String, String> {
    let (c, _) = ensure_client(&app).await?;
    c.list_agent_sessions(command, cwd.into()).await.map_err(err)
}

#[tauri::command]
pub(crate) async fn list_groups(app: State<'_, App>) -> Result<Vec<Group>, String> {
    let c = crate::connection::host_now(&app).await?;
    legacy_groups(&c).await.map_err(err)
}

#[tauri::command]
pub(crate) async fn save_group(app: State<'_, App>, group: Group) -> Result<Group, String> {
    let c = crate::connection::host_now(&app).await?;
    legacy_save_group(&c, group).await.map_err(err)
}

#[tauri::command]
pub(crate) async fn delete_group(app: State<'_, App>, group: String) -> Result<(), String> {
    let c = crate::connection::host_now(&app).await?;
    legacy_delete_group(&c, &group).await.map_err(err)
}

#[cfg(test)]
mod tests {
    #[test]
    fn remote_input_is_off_while_disconnected_and_never_held() {
        assert_eq!(super::remote_input(None, "s", b"ls\n".to_vec()), Err(super::INPUT_OFF.to_string()));
    }
}

#[tauri::command]
pub(crate) async fn order_groups(app: State<'_, App>, groups: Vec<String>) -> Result<(), String> {
    let c = crate::connection::host_now(&app).await?;
    legacy_order_groups(&c, groups).await.map_err(err)
}
