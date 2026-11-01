//! The desktop app is a client of `skiffd`. It owns no PTY.

mod cli_path;
mod clipboard;
mod connection;
mod commands;
mod config;
mod copies;
mod diagnostics;
mod disk;
mod drops;
mod host_files;
mod hosts;
mod input;
mod ledger;
mod quit;
mod reload;
mod skiff_agent;
mod streaming;
mod updater;
mod workspace_file;
mod native_menu;
#[cfg(target_os = "linux")]
mod header_bar;

use connection::App;
use tauri::{Emitter, Manager};

/// The renderer switch Skiff sets for its own webview. Sessions must not
/// inherit it: it slows every WebKitGTK app started from a Skiff shell.
pub(crate) const WEBKIT_DMABUF_VAR: &str = "WEBKIT_DISABLE_DMABUF_RENDERER";

/// True when Skiff set `WEBKIT_DMABUF_VAR` itself, not the user.
pub(crate) static SET_WEBKIT_DMABUF: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// WebKitGTK paints a blank window on NVIDIA under Wayland when it renders
/// through DMA-BUF. Turn that path off there, unless the user chose
/// otherwise. X11, Intel and AMD keep it: it is the faster path.
#[cfg(target_os = "linux")]
fn disable_dmabuf_where_broken() {
    let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some()
        && std::env::var("GDK_BACKEND").map_or(true, |b| !b.starts_with("x11"));
    if std::env::var_os(WEBKIT_DMABUF_VAR).is_none()
        && wayland
        && std::path::Path::new("/proc/driver/nvidia").exists()
    {
        std::env::set_var(WEBKIT_DMABUF_VAR, "1");
        SET_WEBKIT_DMABUF.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Commands that read or change this Mac's host-owned data or processes:
/// local icon and background paths, `git init`, local openers, the watch
/// log, the resume list, and daemon lifecycle. While a remote Host is
/// selected they are refused here, in one place, so a page bug cannot show
/// this Mac's data under a remote Host's name. M4–M6 move them behind Host APIs.
const LOCAL_ONLY: &[&str] = &[
    "skiff_watch_log",
    "init_repository", "set_project_icon", "set_project_background", "skipper_avatars", "set_skipper_avatar",
    "open_diff", "preview_file", "open_file", "reveal_file", "open_path", "disk_open", "disk_reveal",
    "list_agent_sessions", "config_path", "open_config", "install_cli", "restart_daemon", "reload_daemon", "update_daemon",
];

/// Why a [`LOCAL_ONLY`] command was refused.
pub(crate) const LOCAL_ONLY_REFUSED: &str =
    "Not available on a remote Host yet. It reads projects, files, the ledger or Skipper on this computer.";

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    #[cfg(target_os = "linux")]
    disable_dmabuf_where_broken();
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(App::default())
        .manage(quit::Guard::default())
        // The window starts hidden: GTK takes a header bar only before it shows.
        .setup(|app| {
            // Copies of Host files left by an app that did not quit cleanly.
            std::thread::spawn(copies::sweep);
            // Endpoint-local lifecycle: a remote Host's CLI is its own.
            if let Some(local) = app.state::<App>().selection().process_control() {
                std::thread::spawn(move || cli_path::sync(&local));
            }
            // This computer's agent folder and its image cache, Skipper on
            // or off, then the cache's age from Disk use.
            std::thread::spawn(|| {
                skiff_agent::make_folder();
                disk::auto_flush();
            });
            // This Mac's Skipper skill: not for a remote Host. The ledger
            // watcher asks whichever Host is selected.
            let handle = app.handle().clone();
            std::thread::spawn(move || skiff_agent::sync(&handle.state::<App>()));
            let handle = app.handle().clone();
            ledger::watch(handle);
            hosts::watch(app.handle().clone());
            // A remote connect's live lines, for the connecting panel.
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                use tauri::Emitter;
                let mut lines = skiff_client::host::ssh::connect_log();
                loop {
                    match lines.recv().await {
                        Ok(line) => drop(handle.emit("skiff:connect-line", line)),
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(_) => break,
                    }
                }
            });
            let Some(window) = app.get_webview_window("main") else { return Ok(()) };
            #[cfg(target_os = "linux")]
            if header_bar::wanted() {
                header_bar::install(app.handle(), &window)?;
            }
            window.show()?;
            Ok(())
        })
        .on_menu_event(|app, event| {
            let _ = app.emit("skiff:menu", event.id().as_ref());
        })
        .invoke_handler(local_only(tauri::generate_handler![
            native_menu::set_menu,
            quit::quit_app,
            quit::guard_quit,
            updater::update_off,
            updater::relaunch_app,
            native_menu::menu_layout,
            native_menu::set_menu_layout,
            native_menu::set_window_dark,
            clipboard::clipboard_write,
            clipboard::clipboard_read,
            drops::drop_files,
            drops::clipboard_image,
            drops::images,
            drops::image_thumb,
            drops::image_insert,
            drops::image_pin,
            drops::image_delete,
            drops::open_image,
            drops::agent_cache_flags,
            connection::daemon_status,
            connection::host_capabilities,
            hosts::list_hosts,
            hosts::add_host,
            hosts::edit_host,
            hosts::rename_host,
            hosts::remove_host,
            hosts::uninstall_preview,
            hosts::ssh_aliases,
            hosts::resolve_ssh,
            hosts::check_host,
            hosts::host_key_scan,
            hosts::host_key_trust,
            hosts::make_key,
            hosts::unlock_key,
            hosts::public_key,
            hosts::install_key,
            hosts::fix_permissions,
            hosts::install_skiff,
            hosts::start_skiff,
            hosts::turn_on_linger,
            hosts::update_skiff,
            hosts::cancel_install,
            hosts::set_host_auto_update,
            hosts::select_host,
            hosts::set_host_clipboard,
            hosts::host_clipboard,
            hosts::accept_host_identity,
            cli_path::install_cli,
            skiff_agent::get_skiff_agent,
            skiff_agent::set_skiff_agent,
            skiff_agent::edit_task_type_file,
            skiff_agent::start_skiff_watch,
            skiff_agent::start_project_skipper,
            skiff_agent::skipper_files,
            skiff_agent::skipper_file_errors,
            skiff_agent::edit_skipper_file,
            skiff_agent::reset_skipper_file,
            skiff_agent::start_fleet_skipper,
            skiff_agent::fleet_takeover_preview,
            skiff_agent::fleet_takeover,
            skiff_agent::rename_fleet,
            skiff_agent::skiff_watch_log,
            skiff_agent::open_skiff_agent,
            skiff_agent::preview_skiff_folder,
            skiff_agent::change_skiff_folder,
            ledger::ledger_rows,
            ledger::ledger_result,
            disk::disk_usage,
            disk::flush_disk,
            disk::set_cache_days,
            disk::disk_open,
            disk::disk_reveal,
            commands::list_sessions,
            commands::daemon_metrics,
            commands::create_session,
            commands::pty_write,
            commands::pty_resize,
            commands::session_seen,
            commands::kill_session,
            commands::rename_session,
            commands::set_session_theme,
            config::list_projects,
            config::get_project,
            config::add_worktree,
            config::list_branches,
            config::remove_worktree,
            config::add_project,
            connection::restart_daemon,
            reload::reload_daemon,
            reload::update_daemon,
            config::inspect_folder,
            config::init_repository,
            config::init_host_repository,
            config::folder_home,
            config::folder_list,
            config::folder_create,
            config::folder_places,
            config::set_project_icon,
            config::skipper_avatars,
            config::set_skipper_avatar,
            config::set_project_short,
            config::set_project_fleet,
            config::set_project_color,
            config::set_project_closed,
            config::remove_project,
            config::set_project_background,
            config::set_project_background_opacity,
            commands::project_background,
            commands::list_files,
            commands::read_file,
            commands::open_file_copy,
            commands::git_changes,
            commands::git_diff,
            connection::data_plane_status,
            commands::open_diff,
            commands::open_settings,
            commands::set_open,
            commands::set_open_peek,
            commands::preview_file,
            commands::open_file,
            commands::reveal_file,
            commands::open_path,
            commands::open_url,
            config::set_project_changes,
            config::set_project_files,
            config::set_project_theme,
            config::reorder_projects,
            commands::list_themes,
            commands::import_themes,
            commands::open_theme_folder,
            config::get_appearance,
            diagnostics::memory_usage,
            config::get_keys,
            commands::config_path,
            diagnostics::trace_info,
            diagnostics::trace_write,
            diagnostics::trace_set,
            diagnostics::system_load,
            commands::open_config,
            config::set_appearance,
            config::set_font_size,
            config::set_mission_filter,
            commands::list_agents,
            commands::list_agent_sessions,
            config::agent_config,
            config::set_agents,
            config::set_agent_command,
            config::rename_agent,
            config::remove_agent,
            config::restore_agents,
            commands::list_groups,
            commands::session_screen,
            commands::save_group,
            commands::delete_group,
            commands::order_groups,
            streaming::subscribe_output,
            streaming::ack_output,
            streaming::unsubscribe_output,
            streaming::subscribe_events,
            workspace_file::workspace_read,
            workspace_file::workspace_revision,
            workspace_file::workspace_write,
            workspace_file::workspace_launch_file,
            workspace_file::workspace_took,
        ]))
        .build(tauri::generate_context!())
        .expect("error while running tauri application")
        .run(|_app, _event| {
            if let tauri::RunEvent::ExitRequested { api, .. } = &_event {
                quit::requested(_app, api);
            }
            if let tauri::RunEvent::Exit = &_event {
                copies::cleanup();
            }
            // A workspace file opened from Finder while Skiff runs, or to start it.
            #[cfg(target_os = "macos")]
            if let tauri::RunEvent::Opened { urls } = &_event {
                workspace_file::os_open(_app, urls.iter().map(|u| u.to_string()));
            }
        });
}

/// Wraps the command handler: [`LOCAL_ONLY`] commands are refused while a
/// remote Host is selected, before they run.
fn local_only<R: tauri::Runtime>(
    handler: impl Fn(tauri::ipc::Invoke<R>) -> bool + Send + Sync + 'static,
) -> impl Fn(tauri::ipc::Invoke<R>) -> bool + Send + Sync + 'static {
    move |invoke| {
        if LOCAL_ONLY.contains(&invoke.message.command()) && invoke.message.webview().state::<App>().remote() {
            invoke.resolver.reject(LOCAL_ONLY_REFUSED);
            return true;
        }
        handler(invoke)
    }
}

#[cfg(test)]
mod tests {
    /// The registered commands, as `generate_handler!` lists them.
    fn registered() -> Vec<String> {
        let source = include_str!("lib.rs");
        let start = source.find("generate_handler![").unwrap();
        let end = source[start..].find("])").unwrap() + start;
        source[start + "generate_handler![".len()..end]
            .split(',')
            .filter_map(|c| c.trim().rsplit("::").next().map(str::to_string))
            .filter(|c| !c.is_empty())
            .collect()
    }

    #[test]
    fn every_local_only_command_exists_and_none_reaches_a_host_session_path() {
        let all = registered();
        for name in super::LOCAL_ONLY {
            assert!(all.iter().any(|c| c == name), "{name} is not a registered command");
        }
        // Session and terminal commands go through the selected Host, and are
        // gated by its capabilities instead.
        for host in ["list_sessions", "daemon_metrics", "subscribe_events", "subscribe_output", "pty_write", "pty_resize", "session_screen", "create_session", "kill_session", "rename_session", "list_groups", "save_group", "delete_group", "order_groups", "list_agents", "daemon_status", "host_capabilities", "list_hosts", "select_host", "list_projects", "get_project", "git_changes", "git_diff", "data_plane_status", "list_files", "read_file", "open_file_copy", "project_background",
            "ledger_rows", "ledger_result", "get_skiff_agent", "add_worktree", "list_branches", "remove_worktree", "add_project", "inspect_folder", "init_host_repository", "folder_home", "folder_list", "folder_create", "folder_places", "set_project_short", "set_project_fleet", "set_project_color", "set_project_closed", "remove_project", "set_project_background_opacity", "set_project_changes", "set_project_files", "set_project_theme", "reorder_projects",
            "set_skiff_agent", "preview_skiff_folder", "change_skiff_folder", "open_skiff_agent", "start_skiff_watch", "start_project_skipper", "start_fleet_skipper", "skipper_files", "skipper_file_errors", "edit_skipper_file", "edit_task_type_file", "reset_skipper_file", "fleet_takeover_preview", "fleet_takeover", "rename_fleet",
            "agent_config", "set_agents", "set_agent_command", "rename_agent", "remove_agent", "restore_agents"] {
            assert!(!super::LOCAL_ONLY.contains(&host), "{host}");
        }
        // M4: project reads and changes and Git status go to the selected
        // Host's data plane (`skiff-host`), never to this Mac's config or git.
        // M5: files and backgrounds come from the Host's data plane too.
        // M6: the ledger, results and Skipper's settings come from it too,
        // and Skipper's settings writes, launch and watcher (#166 R7) and the
        // agent settings (R8) go to it. The watch log stays here.
        // Icon and background paths, `git init` and every opener of this Mac stay listed.
        for local in ["open_path", "open_file", "reveal_file", "disk_open", "disk_reveal", "open_diff", "preview_file", "set_project_icon", "set_project_background", "init_repository", "skiff_watch_log", "restart_daemon", "reload_daemon", "update_daemon", "install_cli", "list_agent_sessions"] {
            assert!(super::LOCAL_ONLY.contains(&local), "{local}");
        }
        // A command added later must be placed on one side or the other.
        let allowed = ["set_menu", "quit_app", "guard_quit", "menu_layout", "set_menu_layout", "set_window_dark", "clipboard_write", "clipboard_read",
            "daemon_status", "host_capabilities", "list_hosts", "add_host", "edit_host", "rename_host", "remove_host", "uninstall_preview", "ssh_aliases", "resolve_ssh", "check_host", "host_key_scan", "host_key_trust", "make_key", "unlock_key", "public_key", "install_key", "fix_permissions", "install_skiff", "cancel_install", "start_skiff", "turn_on_linger", "update_skiff", "set_host_auto_update", "select_host", "set_host_clipboard", "host_clipboard", "accept_host_identity",
            "list_sessions", "daemon_metrics", "create_session", "pty_write", "pty_resize", "session_seen", "kill_session", "rename_session", "set_session_theme", "list_agents", "list_projects", "get_project", "git_changes", "git_diff", "data_plane_status", "list_files", "read_file", "open_file_copy", "project_background",
            "ledger_rows", "ledger_result", "get_skiff_agent", "add_worktree", "list_branches", "remove_worktree", "add_project", "inspect_folder", "init_host_repository", "folder_home", "folder_list", "folder_create", "folder_places", "set_project_short", "set_project_fleet", "set_project_color", "set_project_closed", "remove_project", "set_project_background_opacity", "set_project_changes", "set_project_files", "set_project_theme", "reorder_projects",
            "set_skiff_agent", "preview_skiff_folder", "change_skiff_folder", "open_skiff_agent", "start_skiff_watch", "start_project_skipper", "start_fleet_skipper", "skipper_files", "skipper_file_errors", "edit_skipper_file", "edit_task_type_file", "reset_skipper_file", "fleet_takeover_preview", "fleet_takeover", "rename_fleet",
            "agent_config", "set_agents", "set_agent_command", "rename_agent", "remove_agent", "restore_agents",
            "open_settings", "set_open", "set_open_peek", "open_url", "list_themes", "import_themes", "open_theme_folder", "get_appearance",
            "memory_usage", "get_keys", "trace_info", "trace_write", "trace_set", "system_load", "set_appearance", "set_font_size", "set_mission_filter",
            "list_groups", "session_screen", "save_group", "delete_group", "order_groups", "subscribe_output", "ack_output", "unsubscribe_output", "subscribe_events",
            "workspace_read", "workspace_revision", "workspace_write", "workspace_launch_file", "workspace_took",
            // This computer's Disk use and Images, whichever Host is shown. A drop or an Insert on a remote Host's pane goes to that Host.
            "disk_usage", "flush_disk", "set_cache_days",
            "drop_files", "clipboard_image", "images", "image_thumb", "image_insert", "image_pin", "image_delete", "open_image", "agent_cache_flags",
            // This app's own updater and restart, whichever Host is shown.
            "update_off", "relaunch_app"];
        for c in &all {
            assert!(super::LOCAL_ONLY.contains(&c.as_str()) || allowed.contains(&c.as_str()), "{c}: decide whether a remote Host may run it");
        }
    }
}
