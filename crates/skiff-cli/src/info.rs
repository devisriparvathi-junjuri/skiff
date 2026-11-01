//! status, agents, projects.

use std::path::Path;

use anyhow::Result;
use skiff_client::Client;
use skiff_core::{project::ProjectKind, session::SessionInfo};

use crate::{print, resolve};

pub async fn status(socket: &Path) -> Result<()> {
    let exe = std::env::current_exe().ok();
    let shown = exe.as_deref().map_or("?".to_string(), |p| p.display().to_string());
    println!("skiff     {} (skiffd {}) {shown}", env!("CARGO_PKG_VERSION"), skiff_core::VERSION);
    if let Some(w) = exe.as_deref().and_then(|e| path_warning(e, &std::env::var_os("PATH").unwrap_or_default())) {
        eprintln!("skiff: warning: {w}");
    }
    let c = crate::connect(socket).await?;
    let info = c.daemon_info().await?;
    let (version, protocol) = (info.version.clone(), info.protocol);
    if version != skiff_core::VERSION {
        eprintln!("skiff: warning: {}", crate::daemon::advice(&c, &info).await);
    }
    let pid = info.pid.map(|p| p as i32).or(c.daemon_pid).map_or("?".to_string(), |p| p.to_string());
    println!("skiffd    {version}");
    let note = if protocol == skiff_core::PROTOCOL {
        String::new()
    } else {
        format!(" (this skiff speaks {})", skiff_core::PROTOCOL)
    };
    println!("protocol  {protocol}{note}");
    println!("pid       {pid}");
    println!("reload    {}", info.reload_state.map_or("no".to_string(), |v| format!("yes (handover {v})")));
    println!("socket    {}", socket.display());
    let line = if protocol < 27 {
        format!("unknown (skiffd speaks {protocol})")
    } else {
        let all = crate::sessions::sorted(&c).await?;
        let agent = all.iter().find(|s| s.coordinator && s.state != skiff_core::session::SessionState::Done);
        watcher_line(agent, || watch_lock_held(socket), &socket.with_extension("watch.log"))
    };
    println!("watcher   {line}");
    Ok(())
}

/// `skiff status --json`: for a program, such as the app's check of a Host
/// over ssh. No warnings and no starts: exit 1 when no skiffd answers.
pub async fn status_json(socket: &Path) -> i32 {
    let daemon = match crate::connect(socket).await {
        Ok(c) => c.daemon_info().await.ok(),
        Err(_) => None,
    };
    let skiffd = daemon.as_ref().map(|i| {
        serde_json::json!({
            "version": i.version,
            "protocol": i.protocol,
            "pid": i.pid,
            "host_id": i.identity.as_ref().map(|id| id.host_id.clone()),
        })
    });
    let out = serde_json::json!({
        "skiff": env!("CARGO_PKG_VERSION"),
        "protocol": skiff_core::PROTOCOL,
        "socket": socket.display().to_string(),
        "skiffd": skiffd,
    });
    println!("{out}");
    if daemon.is_some() { 0 } else { 1 }
}

/// The `watcher` line of `skiff status`, for the Skiff agent's session.
/// `lock_held` is asked only when no watcher registered: then a held lock
/// means a watcher that does not register (an older `skiff watch`) runs.
fn watcher_line(agent: Option<&SessionInfo>, lock_held: impl FnOnce() -> bool, log: &Path) -> String {
    let Some(s) = agent else { return "no Skiff agent session".into() };
    let who = format!("{} ({})", resolve::display_name(s), print::short_id(&s.id));
    let Some(w) = &s.watch else {
        return if lock_held() {
            format!("none for {who}. The watch lock is held: an older skiff watch runs. See {}", log.display())
        } else {
            format!("none for {who}")
        };
    };
    let pids = w.pids.iter().map(u32::to_string).collect::<Vec<_>>().join(" ");
    let mut line = match w.pids.len() {
        0 | 1 => format!("runs for {who} since {}, pid {}", clock(w.since), if pids.is_empty() { "?" } else { &pids }),
        n => format!("{n} run for {who}, pids {pids}"),
    };
    if let Some(f) = &w.failure {
        line.push_str(&format!("  last failure {} {}: {}", clock(f.at), f.kind, f.message));
    }
    line
}

/// Another process holds the watch lock beside the socket.
fn watch_lock_held(socket: &Path) -> bool {
    std::fs::File::open(socket.with_extension("watch.lock"))
        .is_ok_and(|f| matches!(f.try_lock(), Err(std::fs::TryLockError::WouldBlock)))
}

/// Unix ms as local `HH:MM`.
fn clock(ms: u64) -> String {
    let t = (ms / 1000) as libc::time_t;
    // SAFETY: localtime_r writes only into `tm`.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&t, &mut tm) }.is_null() {
        return "?".into();
    }
    format!("{:02}:{:02}", tm.tm_hour, tm.tm_min)
}

/// Why the `skiff` a shell runs is not `exe`: another one comes first on
/// `path`, or none is on it. An old copy first gives old commands.
fn path_warning(exe: &Path, path: &std::ffi::OsStr) -> Option<String> {
    use std::os::unix::fs::PermissionsExt;
    let first = std::env::split_paths(path)
        .map(|d| d.join("skiff"))
        .find(|f| f.metadata().is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0));
    let Some(first) = first else {
        return Some(format!("no skiff is on PATH. A shell runs none, and this one is {}", exe.display()));
    };
    let same = |a: &Path, b: &Path| matches!((a.canonicalize(), b.canonicalize()), (Ok(a), Ok(b)) if a == b);
    (!same(&first, exe)).then(|| format!("the first skiff on PATH is {}, not this one ({}). A shell runs that one", first.display(), exe.display()))
}

/// The agents, then the task types with the command each one starts. JSON:
/// `{"agents": [...], "tasks": [...]}`, so Skipper and the skill read one source.
pub async fn agents(c: &Client, json: bool) -> Result<()> {
    let agents = c.list_agents().await?;
    let cfg = skiff_core::config::load()?;
    let tasks = skiff_core::tasks::resolve(&cfg.skiff_agent.tasks, &agents, &skiff_client::skipper::configured(&cfg)?);
    if json {
        return print::json(&serde_json::json!({ "agents": agents, "tasks": tasks }));
    }
    let yes = |b: bool| if b { "yes" } else { "no" }.to_string();
    let rows: Vec<Vec<String>> = agents
        .iter()
        .map(|a| vec![a.id.clone(), yes(a.installed), yes(a.enabled), a.command.clone()])
        .collect();
    print::table(&["ID", "INSTALLED", "ENABLED", "COMMAND"], &rows);
    if !tasks.is_empty() {
        println!();
        print::table(&["TYPE", "AGENT", "MODEL", "INSTRUCTIONS", "COMMAND"], &task_rows(&tasks));
    }
    Ok(())
}

/// A task type per row. A row that starts nothing shows why in COMMAND.
fn task_rows(tasks: &[skiff_core::tasks::TaskType]) -> Vec<Vec<String>> {
    tasks
        .iter()
        .map(|t| {
            let cmd = match (&t.command, &t.error) {
                (Some(c), None) => c.clone(),
                (Some(c), Some(e)) => format!("{c}  ({e})"),
                (None, e) => format!("error: {}", e.as_deref().unwrap_or("none")),
            };
            let file = t.instructions.map_or_else(|| "none".into(), |n| format!("{n} B"));
            vec![t.name.clone(), t.agent.clone(), t.model.clone().unwrap_or_else(|| "-".into()), file, cmd]
        })
        .collect()
}

/// From projects.toml, read here as the app reads it. Closed projects too.
pub fn projects(json: bool) -> Result<()> {
    let list = skiff_core::project::list()?;
    if json {
        return print::json(&list);
    }
    let rows: Vec<Vec<String>> = list
        .iter()
        .map(|p| {
            let state = project_state(p);
            let kind = match p.kind {
                ProjectKind::Git => "git",
                ProjectKind::Folder => "folder",
            };
            vec![p.name.clone(), p.short.clone(), kind.into(), print::tilde(&p.path), p.agents.join(","), state]
        })
        .collect();
    print::table(&["NAME", "SHORT", "KIND", "PATH", "AGENTS", "STATE"], &rows);
    Ok(())
}

/// The STATE cell. A plain folder has no worktrees to count: `-`.
fn project_state(p: &skiff_core::project::Project) -> String {
    if p.closed {
        "closed".to_string()
    } else if let Some(e) = &p.error {
        format!("error: {e}")
    } else if p.kind == ProjectKind::Folder {
        "-".to_string()
    } else {
        format!("{} worktrees", p.worktrees.len())
    }
}

#[cfg(test)]
mod tests {
    use std::{os::unix::fs::PermissionsExt, path::PathBuf};

    use super::{path_warning, project_state};

    #[test]
    fn a_plain_folder_counts_no_worktrees() {
        let project = |kind: &str| -> skiff_core::project::Project {
            serde_json::from_value(serde_json::json!({
                "name": "docs", "short": "doc", "path": "/docs", "color": null, "agents": [], "error": null, "kind": kind,
                "worktrees": [{ "path": "/docs", "branch": null, "head": "", "is_main": true, "locked": false, "prunable": false }],
            }))
            .unwrap()
        };
        assert_eq!(project_state(&project("folder")), "-");
        assert_eq!(project_state(&project("git")), "1 worktrees");
    }

    fn bin(dir: &PathBuf) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let f = dir.join("skiff");
        std::fs::write(&f, "").unwrap();
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o755)).unwrap();
        f
    }

    #[test]
    fn warns_when_another_skiff_comes_first_on_path() {
        let root = std::env::temp_dir().join(format!("skiff-status-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (old, new, empty) = (root.join("old"), root.join("new"), root.join("empty"));
        let (old_bin, new_bin) = (bin(&old), bin(&new));
        std::fs::create_dir_all(&empty).unwrap();
        let path = |dirs: &[&PathBuf]| std::env::join_paths(dirs).unwrap();
        assert_eq!(path_warning(&new_bin, &path(&[&empty, &new, &old])), None);
        assert!(path_warning(&new_bin, &path(&[&old, &new])).unwrap().contains(&old_bin.display().to_string()));
        assert!(path_warning(&new_bin, &path(&[&empty])).unwrap().starts_with("no skiff"));
        // A file that cannot run is skipped, as a shell skips it.
        std::fs::set_permissions(&old_bin, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(path_warning(&new_bin, &path(&[&old, &new])), None);
        std::fs::remove_dir_all(&root).unwrap();
    }
}
