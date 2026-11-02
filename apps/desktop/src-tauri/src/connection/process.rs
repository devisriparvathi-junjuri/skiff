//! Local daemon and data-service process helpers.

use super::*;

/// A program this app ships: `env` names it when set, else the one beside
/// this app's executable, else `name` on PATH.
pub(crate) fn bundled(name: &str, env: Option<&str>) -> PathBuf {
    if let Some(p) = env.and_then(std::env::var_os) {
        return PathBuf::from(p);
    }
    if let Ok(exe) = std::env::current_exe() {
        let beside = exe.with_file_name(name);
        if beside.exists() {
            // The AppImage's mount goes when its app quits, while skiffd and
            // skiff-host run on. Their binary paths then name nothing, and a
            // later reload has no target. They run from a copy outside the mount.
            if matches!(name, "skiffd" | "skiff-host") && std::env::var_os("APPIMAGE").is_some() {
                if let Some(dir) = dirs::data_dir() {
                    match copy_out(&beside, &dir.join("skiff/bin")) {
                        Ok(copy) => return copy,
                        Err(e) => eprintln!("{name} not copied out of the AppImage: {e:#}"),
                    }
                }
            }
            return beside;
        }
    }
    PathBuf::from(name)
}

pub(crate) fn daemon_binary() -> PathBuf {
    bundled("skiffd", Some("SKIFF_DAEMON"))
}

/// Copies `src` to `dir` under its own name when that differs. Writes beside
/// it and renames, so a running copy keeps its own file.
pub(super) fn copy_out(src: &std::path::Path, dir: &std::path::Path) -> anyhow::Result<PathBuf> {
    use anyhow::Context;
    use std::os::unix::fs::PermissionsExt;
    let name = src.file_name().context("no file name")?.to_string_lossy();
    let dest = dir.join(&*name);
    let new = std::fs::read(src).with_context(|| format!("read {}", src.display()))?;
    if std::fs::read(&dest).is_ok_and(|old| old == new) {
        return Ok(dest);
    }
    std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    let tmp = dir.join(format!(".{name}.new"));
    std::fs::write(&tmp, &new).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
    std::fs::rename(&tmp, &dest).with_context(|| format!("replace {}", dest.display()))?;
    Ok(dest)
}

/// Starts `cmd` apart from this app: its own process group, no stdin, and
/// its errors in `log`. The log before it is kept as `.1`: it saw what the
/// new one restores, or why the last one stopped.
pub(super) fn spawn_detached(mut cmd: std::process::Command, log: PathBuf) -> anyhow::Result<std::process::Child> {
    if let Some(dir) = log.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let _ = std::fs::rename(&log, log.with_extension("log.1"));
    let log = std::fs::File::create(&log)?;
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::from(log));
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    Ok(cmd.spawn()?)
}

/// Only for a local endpoint: `_local` proves it.
pub(super) fn spawn_daemon(_local: &ProcessControl) -> anyhow::Result<()> {
    let mut cmd = std::process::Command::new(daemon_binary());
    // The daemon passes its environment to every session it starts.
    if crate::SET_WEBKIT_DMABUF.load(std::sync::atomic::Ordering::Relaxed) {
        cmd.env_remove(crate::WEBKIT_DMABUF_VAR);
    }
    spawn_detached(cmd, socket_path().with_extension("log"))?;
    Ok(())
}

pub(crate) fn data_binary() -> PathBuf {
    bundled("skiff-host", Some("SKIFF_HOST_BIN"))
}

/// How long after one start of `skiff-host` the next may come.
pub(super) const DATA_START_WAIT: Duration = Duration::from_secs(10);

/// Only for a local endpoint: `_local` proves it. Starts this Mac's
/// data-plane service for the skiffd at `session`, on that socket's state.
/// A second start finds the first one listening and exits.
pub(super) fn spawn_data_service(_local: &ProcessControl, session: &std::path::Path) -> anyhow::Result<()> {
    let mut cmd = std::process::Command::new(data_binary());
    cmd.arg("--session-socket").arg(session).arg("--this-mac");
    reap(spawn_detached(cmd, skiff_core::socket::data_socket(session).with_extension("log"))?);
    Ok(())
}

/// Waits for the data service on a thread of its own, so one that exits or
/// is stopped leaves no zombie under the app. Waiting changes nothing else:
/// the service keeps its own process group, outlives the app and is never
/// signalled from here.
pub(super) fn reap(mut child: std::process::Child) {
    let _ = std::thread::Builder::new().name("skiff-host reaper".into()).spawn(move || {
        let _ = child.wait();
    });
}
