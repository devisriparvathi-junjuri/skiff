use std::{collections::HashMap, path::PathBuf, process::Stdio, sync::{atomic::{AtomicBool, Ordering}, Arc}, time::Duration};
use serde::Serialize;
use skiff_client::{Client, host::{HostConnection, data::{DataError, DataPlane}, local::{HostSelection, ProcessControl, SelectedHost, connection_status}}};
use skiff_core::{socket::socket_path, session::SessionState};
use tauri::{async_runtime::JoinHandle, State};
use tokio::sync::Mutex;
use crate::{input::Input, reload::{self, NoPlan, Plan}, streaming::Flow};

#[derive(Default)]
pub(crate) struct App {
    pub(crate) host: Mutex<Option<Arc<SelectedHost>>>,
    /// M4: the selected Host's data-plane service (`skiff-host`), checked
    /// against the session plane's Host id on every connection.
    pub(crate) data: Mutex<Option<DataPlane>>,
    /// This run replaced a local `skiff-host` of another build already:
    /// once per run.
    pub(crate) data_replaced: AtomicBool,
    /// A reply from the local `skiff-host` was unreadable: the next
    /// connection replaces it, whatever its build.
    pub(crate) data_unreadable: AtomicBool,
    /// When this app run last started `skiff-host`: one that stops right
    /// after it starts is not started again every read.
    pub(crate) data_started: std::sync::Mutex<Option<std::time::Instant>>,
    /// Counts the data-plane connections made: a new one may bring other
    /// data than the page shows (`data_plane_status`).
    pub(crate) data_gen: std::sync::atomic::AtomicU64,
    /// The Host id and the `[[project]]` revision of the last project read or
    /// change: what the next project change is made from.
    pub(crate) revision: std::sync::Mutex<Option<(skiff_host_api::HostId, String)>>,
    /// The Host id and the `[agents]` revision of the last agent read or
    /// change of a remote Host: what the next agent change is made from.
    pub(crate) agent_revision: std::sync::Mutex<Option<(skiff_host_api::HostId, String)>>,
    /// The one endpoint this window reaches: this Mac, or the remote Host
    /// `hosts.toml` selects. Only `select_host` changes it, never a reconnect.
    /// Legacy paths only borrow its accepted Client.
    selection: std::sync::Mutex<Option<HostSelection>>,
    /// The same selected Host connection, for keystrokes, which must not
    /// wait on a lock.
    pub(crate) live: std::sync::Mutex<Option<Arc<HostConnection>>>,
    /// The shown remote Host reported another identity than its pin: the
    /// destination and the id it reported, for `accept_host_identity`.
    pub(crate) changed: std::sync::Mutex<Option<(String, skiff_host_api::HostId)>>,
    pub(crate) subs: Mutex<HashMap<String, JoinHandle<()>>>,
    /// The task that streams events to the page.
    pub(crate) events: Mutex<Option<JoinHandle<()>>>,
    /// Terminal input on its way to the daemon.
    pub(crate) input: Input,
    /// Per session: how far the page's xterm is behind its stream.
    pub(crate) flows: std::sync::Mutex<HashMap<String, Arc<Flow>>>,
    /// Set when the daemon differs from this app and was not replaced.
    pub(crate) warning: Mutex<Option<Warning>>,
    /// The version of a daemon this app replaced at launch.
    pub(crate) replaced: Mutex<Option<String>>,
    /// An older daemon that can reload onto the bundled one. The page starts
    /// the reload with `reload_daemon`.
    pub(crate) reload: Mutex<Option<Plan>>,
    /// A reload ran. The app does not start another one on its own.
    pub(crate) reload_tried: AtomicBool,
    /// Why the daemon cannot move onto the bundled skiffd with its sessions:
    /// no handover, or a reload it refused. The warning says it on each
    /// check, including after a reconnect.
    pub(crate) no_reload: std::sync::Mutex<Option<String>>,
    /// Why the daemon is gone after a reload. No daemon starts on its own
    /// while it is set: only an explicit restart starts one.
    pub(crate) failed: std::sync::Mutex<Option<String>>,
    /// Watcher pids skiffd registered, from its peer credentials, kept from
    /// before a reload, which drops the registrations. They identify a
    /// watcher from before the lock record.
    pub(crate) watch_pids: std::sync::Mutex<Vec<u32>>,
    /// The app tried to replace the watcher this run. Set before the
    /// signal, so a watcher that did not stop is not signalled again.
    pub(crate) watch_tried: Arc<AtomicBool>,
    /// Held across each watcher check, so a start and a restore at once do
    /// not both act on the same lock.
    pub(crate) watch_gate: Mutex<()>,
}

impl App {
    /// The selected Host's session connection, while it is open.
    pub(crate) async fn live_session(&self) -> Option<Arc<HostConnection>> {
        self.host.lock().await.as_ref().map(|h| h.connection()).filter(|c| !c.is_closed())
    }

    /// The selected endpoint, from `hosts.toml` on first use. A file that
    /// cannot be read selects this Mac.
    pub(crate) fn selection(&self) -> HostSelection {
        let mut selection = self.selection.lock().unwrap();
        let selection = selection.get_or_insert_with(|| {
            crate::hosts::load(&crate::hosts::path())
                .map(|hosts| crate::hosts::selected(&hosts, local_selection))
                .unwrap_or_else(|e| {
                    eprintln!("hosts.toml: {e:#}; showing this computer");
                    local_selection()
                })
        });
        selection.clone()
    }

    /// The window shows a remote Host. Commands that read this Mac's data
    /// are refused then (see `lib.rs`).
    pub(crate) fn remote(&self) -> bool {
        !self.selection().is_local()
    }

    /// The Host id this window's live connection accepted, if it has one.
    pub(crate) fn live_identity(&self) -> Option<skiff_host_api::HostId> {
        self.live.lock().unwrap().as_ref().map(|c| c.hello().host.host_id.clone())
    }

    #[cfg(test)]
    pub(crate) fn set_selection(&self, selection: HostSelection) {
        *self.selection.lock().unwrap() = Some(selection);
    }
}

/// This Mac: the local skiffd socket, and the bootstrap id for an older one.
pub(crate) fn local_selection() -> HostSelection {
    HostSelection::new(socket_path(), skiff_core::config::config_path().with_file_name("local-host-id"))
}

/// Shows another endpoint. The old connection, its streams and any held
/// input go first: nothing typed for one Host reaches another.
pub(crate) async fn replace_selection(app: &App, selection: HostSelection) {
    let mut guard = app.host.lock().await;
    *guard = None;
    *app.data.lock().await = None;
    *app.live.lock().unwrap() = None;
    app.input.discard();
    for (_, task) in app.subs.lock().await.drain() {
        task.abort();
    }
    app.flows.lock().unwrap().clear();
    if let Some(task) = app.events.lock().await.take() {
        task.abort();
    }
    *app.warning.lock().await = None;
    *app.replaced.lock().await = None;
    *app.reload.lock().await = None;
    *app.failed.lock().unwrap() = None;
    *app.changed.lock().unwrap() = None;
    *app.selection.lock().unwrap() = Some(selection);
}

/// How long a remote skiffd may take to answer the first Ping.
const REMOTE_HELLO_TIMEOUT: Duration = Duration::from_secs(15);

/// A remote Host: connect through its forward, check the identity it reports
/// against its pin, and never start, reload, stop or replace anything. A
/// version mismatch is reported, not acted on. Every failure leaves its cause
/// in the warning, so the status and the picker say why, in words that never
/// repeat ssh's own output.
async fn connect_remote(app: &App, selection: &HostSelection, guard: &mut Option<Arc<SelectedHost>>) -> Result<(Arc<SelectedHost>, bool), String> {
    let destination = selection.ssh_target().map(|t| t.destination.clone()).unwrap_or_default();
    let arrival = match selection.ssh_target() {
        Some(t) => {
            let arrival = crate::hosts::update_on_connect(t).await;
            // The probe that ran for it found the Host's one socket: the
            // forward goes there. Without it, the connect probes itself.
            if let Some(socket) = crate::hosts::found_socket(t) {
                selection.set_remote_socket(socket);
            }
            arrival
        }
        None => None,
    };
    match try_remote(app, selection, &destination, arrival.as_ref(), crate::hosts::path()).await {
        Ok(host) => {
            // The passive note: "skiffd 2.1 (updated from 2.0)".
            if let Some(from) = arrival.and_then(|a| a.updated_from) {
                *app.replaced.lock().await = Some(from);
            }
            if let Some(t) = selection.ssh_target().cloned() {
                tauri::async_runtime::spawn_blocking(move || crate::hosts::note_login(&t, true));
            }
            *app.changed.lock().unwrap() = None;
            *app.live.lock().unwrap() = Some(host.connection());
            *guard = Some(host.clone());
            Ok((host, false))
        }
        Err(e) => {
            let e = refine_refused(e, selection).await;
            let (warning, message) = remote_failure(&e, &destination);
            if let (Some(_), Some(t)) = (&warning, selection.ssh_target().cloned()) {
                tauri::async_runtime::spawn_blocking(move || crate::hosts::note_login(&t, false));
            }
            if let Some(skiff_host_api::Error::IdentityChanged { received, .. }) = e.downcast_ref() {
                *app.changed.lock().unwrap() = Some((destination.clone(), received.clone()));
            }
            // The wait between tries is not a cause: the last cause stays.
            if let Some(w) = warning {
                *app.warning.lock().await = Some(w);
            }
            Err(message)
        }
    }
}

async fn try_remote(app: &App, selection: &HostSelection, destination: &str, arrival: Option<&crate::hosts::Arrival>, file: PathBuf) -> anyhow::Result<Arc<SelectedHost>> {
    let mut c = selection.connect().await?;
    let info = match tokio::time::timeout(REMOTE_HELLO_TIMEOUT, c.daemon_info()).await {
        Ok(info) => info?,
        Err(_) => anyhow::bail!(RemoteSilent),
    };
    skiff_client::host::ssh::connect_note(format!("skiffd {}, protocol {}", info.version, info.protocol));
    let name = arrival.map_or(destination, |a| a.name.as_str());
    let managed = arrival.is_some_and(|a| a.managed);
    *app.warning.lock().await =
        crate::hosts::version_warning(name, arrival.and_then(|a| a.version.as_deref()), info.protocol, managed).map(|(kind, message)| Warning { kind, message, sessions: None });
    let first = selection.pinned().is_none();
    let host = Arc::new(c.activate().await?);
    if first {
        // The first identity becomes the Host's pin before anything uses it:
        // stored in hosts.toml first, then pinned here. Until the store
        // succeeds the selection stays unpinned, so every try stores again
        // and none connects on an identity only this window holds.
        let id = host.connection().hello().host.host_id.clone();
        let target = selection.ssh_target().cloned().ok_or_else(|| anyhow::anyhow!("{destination} is not a remote Host"))?;
        let pinned = id.clone();
        let stored = tauri::async_runtime::spawn_blocking(move || crate::hosts::pin_first(&file, &target, &id)).await?;
        match stored {
            Ok(()) => selection.repin(pinned),
            Err(e) => {
                // Another window or run stored another identity first: that
                // one is the pin from now on, here too.
                if let Some(skiff_host_api::Error::IdentityChanged { expected, .. }) = e.downcast_ref() {
                    selection.repin(expected.clone());
                }
                return Err(e);
            }
        }
    }
    Ok(host)
}

/// A refused channel names two causes. The probe looks at the socket: when
/// one is there, the sshd blocks the forward. Any other error stays.
async fn refine_refused(e: anyhow::Error, selection: &HostSelection) -> anyhow::Error {
    use skiff_client::host::{check, ssh::SshError};
    let (Some(SshError::NoSkiffd { .. }), Some(t)) = (e.downcast_ref::<SshError>(), selection.ssh_target()) else { return e };
    let options = skiff_client::host::ssh::SshOptions::default();
    match check::run_probe(t, &options).await {
        Ok(check::ProbeRun::Probed(p, _)) => match e.downcast::<SshError>() {
            Ok(ssh) => ssh.refine(p.socket_live).into(),
            Err(e) => e,
        },
        _ => e,
    }
}

/// The remote skiffd did not answer the first Ping in time.
#[derive(Debug)]
struct RemoteSilent;
impl std::fmt::Display for RemoteSilent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "no answer within {}s", REMOTE_HELLO_TIMEOUT.as_secs())
    }
}
impl std::error::Error for RemoteSilent {}

/// The warning and the message for a failed remote connection. Only known
/// causes are named; anything else gets one plain line with what to check.
/// `None` warning: a wait between tries, which keeps the last cause.
pub(crate) fn remote_failure(e: &anyhow::Error, destination: &str) -> (Option<Warning>, String) {
    use skiff_client::host::ssh::SshError;
    let warn = |kind: &'static str, message: String| (Some(Warning { kind, message: message.clone(), sessions: None }), message);
    if let Some(e) = e.downcast_ref::<SshError>() {
        return match e {
            SshError::Waiting { .. } => (None, e.to_string()),
            _ => warn("remote", e.to_string()),
        };
    }
    match e.downcast_ref::<skiff_host_api::Error>() {
        Some(skiff_host_api::Error::IdentityChanged { .. }) => warn(
            "identity",
            format!(
                "{destination} answers as another Skiff Host than before. Skiff stopped and sends nothing to it. \
                 If you expect this, as after Skiff was reinstalled there, accept the new identity."
            ),
        ),
        Some(skiff_host_api::Error::MalformedContract { .. }) => warn(
            "remote",
            format!("Skiff cannot read the replies from the sessions service on {destination}. Settings › Hosts checks {destination}."),
        ),
        _ if e.downcast_ref::<RemoteSilent>().is_some() => warn(
            "remote",
            format!("skiffd on {destination} did not answer within {} s. Settings › Hosts checks {destination}.", REMOTE_HELLO_TIMEOUT.as_secs()),
        ),
        _ => warn(
            "remote",
            format!("Cannot reach skiffd on {destination} over ssh. Settings › Hosts checks {destination} and finds the cause."),
        ),
    }
}

#[derive(Serialize, Clone)]
pub(crate) struct Warning {
    /// `outdated`, `protocol`, `newer`, `hung`, `failed` for a reload that
    /// did not come back, `remote` for a remote Host's problem,
    /// `identity` for a remote Host that reported another identity, or
    /// `update` for a Skiff that Skiff installed there and can update.
    pub(crate) kind: &'static str,
    pub(crate) message: String,
    /// Live sessions a restart would stop. `None` when the daemon did not say.
    pub(crate) sessions: Option<usize>,
}

#[derive(Serialize, Clone)]
pub(crate) struct DaemonStatus {
    connected: bool,
    version: Option<String>,
    /// The daemon's protocol. `None` when the app could not ask.
    protocol: Option<u32>,
    socket: String,
    /// The daemon's pid, from the socket's peer credentials.
    pid: Option<i32>,
    spawned: bool,
    warning: Option<Warning>,
    replaced: Option<String>,
    /// The daemon is older and can reload onto this app's skiffd. The page
    /// calls `reload_daemon` next.
    reload: bool,
}

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
fn copy_out(src: &std::path::Path, dir: &std::path::Path) -> anyhow::Result<PathBuf> {
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
fn spawn_detached(mut cmd: std::process::Command, log: PathBuf) -> anyhow::Result<std::process::Child> {
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
fn spawn_daemon(_local: &ProcessControl) -> anyhow::Result<()> {
    let mut cmd = std::process::Command::new(daemon_binary());
    // The daemon passes its environment to every session it starts.
    if crate::SET_WEBKIT_DMABUF.load(std::sync::atomic::Ordering::Relaxed) {
        cmd.env_remove(crate::WEBKIT_DMABUF_VAR);
    }
    spawn_detached(cmd, socket_path().with_extension("log"))?;
    Ok(())
}

/// A daemon that takes longer than this to answer `hello` is hung.
pub(crate) const HELLO_TIMEOUT: Duration = Duration::from_secs(2);

/// Reuses a live connection, else connects, else starts the daemon and connects.
/// A live connection costs no round trip: every keystroke comes through here.
pub(crate) async fn ensure_client(app: &App) -> Result<(Arc<Client>, bool), String> {
    let (host, spawned) = ensure_host(app).await?;
    Ok((host.client(), spawned))
}

/// The one M1b read path: same owner and legacy result, through HostConnection.
pub(crate) async fn screen_now(app: &App, session: &str, rows: u16) -> Result<Vec<String>, String> {
    let (host, _) = ensure_host(app).await?;
    skiff_client::host::local::legacy_screen(&host.connection(), session, rows).await.map_err(err)
}

/// The one M1c read path; lifecycle compatibility checks still use Client.
pub(crate) async fn inventory_now(app: &App) -> Result<Vec<skiff_core::session::SessionInfo>, String> {
    let (host, _) = ensure_host(app).await?;
    skiff_client::host::local::legacy_inventory(&host.connection()).await.map_err(err)
}

/// The selected owner's Host connection: the M1e terminal plane and the M1g
/// session lifecycle and groups. The caller keeps no Client of its own.
pub(crate) async fn host_now(app: &App) -> Result<Arc<HostConnection>, String> {
    let (host, _) = ensure_host(app).await?;
    Ok(host.connection())
}

/// The selected Host's capabilities as `name@major`, for the page: it offers
/// a session or terminal surface only when the Host has its capability. The
/// Host connection checks them again on every request.
///
/// One Host, one list: the session plane's capabilities and, when its
/// data-plane service answers, that service's. Without the service the
/// list simply lacks `projects` and `git_status`; nothing falls back to
/// this Mac's data.
///
/// This Mac's agent and Skiff agent changes are the exception: this app's
/// local adapter writes them (`config.rs`, `skiff_agent.rs`), so they are
/// offered whatever skiff-host answers, or whether one answers at all.
#[tauri::command]
pub(crate) async fn host_capabilities(app: State<'_, App>) -> Result<Vec<String>, String> {
    let c = host_now(&app).await?;
    let mut names = capability_names(&c);
    match data_now(&app).await {
        Ok(d) => names.extend(capability_names(&d).into_iter().filter(|n| !names.contains(n)).collect::<Vec<_>>()),
        Err(e) => eprintln!("data plane: {e}"),
    }
    if !app.remote() {
        add_local_adapter(&mut names);
    }
    Ok(names)
}

/// The capabilities this app serves itself for This Mac: the settings it
/// writes, and their reads, so a change reads back from the same config, and
/// Skipper's sessions, which it starts with the bundled `skiff`, fleet
/// Skippers included, and the Skippers' instruction files.
const LOCAL_ADAPTER: [&str; 8] = ["agent_config@1", "skipper_config@1", "skipper@1", "skipper_file@1", "skipper_run@1", "skipper_run@2", "session_agents@1", "daemon_metrics@1"];

fn add_local_adapter(names: &mut Vec<String>) {
    for cap in LOCAL_ADAPTER {
        if !names.iter().any(|n| n == cap) {
            names.push(cap.to_string());
        }
    }
}

/// M4: the selected Host's data-plane connection. Its Host id must be the
/// one the session plane accepted, so data from another Host never shows
/// under this one. This Mac's service is started when none answers (a
/// local endpoint's lifecycle); a remote Host's never is.
pub(crate) async fn data_now(app: &App) -> Result<Arc<HostConnection>, String> {
    let session = host_now(app).await?;
    let host = session.hello().host.host_id.clone();
    let mut guard = app.data.lock().await;
    if let Some(d) = guard.as_ref() {
        let c = d.connection();
        if !c.is_closed() && c.hello().host.host_id == host {
            return Ok(c);
        }
    }
    *guard = None;
    let selection = app.selection();
    let not_running = |e: &anyhow::Error| matches!(e.downcast_ref(), Some(DataError::NotRunning));
    let plane = match selection.connect_data(&host).await {
        Ok(plane) => plane,
        Err(e) if not_running(&e) && selection.process_control().is_some() => {
            let control = selection.process_control().expect("checked");
            // One that stops right after it starts is tried again later,
            // not on every read: its log stays readable meanwhile.
            let too_soon = {
                let mut started = app.data_started.lock().unwrap();
                let soon = started.is_some_and(|at| at.elapsed() < DATA_START_WAIT);
                if !soon {
                    *started = Some(std::time::Instant::now());
                }
                soon
            };
            if too_soon {
                return Err(data_failure(&e, &selection));
            }
            spawn_data_service(&control, selection.socket()).map_err(start_failure)?;
            let mut last = e;
            let mut plane = None;
            for _ in 0..50 {
                tokio::time::sleep(Duration::from_millis(100)).await;
                match selection.connect_data(&host).await {
                    Ok(p) => {
                        plane = Some(p);
                        break;
                    }
                    Err(e) if not_running(&e) => last = e,
                    Err(e) => return Err(data_failure(&e, &selection)),
                }
            }
            plane.ok_or_else(|| data_failure(&last, &selection))?
        }
        // This computer's service from a build this app cannot read: it is
        // replaced as in `replace_older_data_service`, by its socket's peer.
        Err(e) if unreadable(&e) && selection.process_control().is_some() => {
            let control = selection.process_control().expect("checked");
            match data_service_pid(&selection).await {
                Some(pid) if !app.data_replaced.swap(true, Ordering::SeqCst) => {
                    eprintln!("skiff-host (pid {pid}) is from a build this app cannot read: replacing it");
                    restart_data_service(app, &control, &selection, &host, pid).await?
                }
                _ => return Err(data_failure(&e, &selection)),
            }
        }
        Err(e) => return Err(data_failure(&e, &selection)),
    };
    let plane = match selection.process_control() {
        Some(control) => replace_older_data_service(app, &control, &selection, &host, plane).await?,
        None => plane,
    };
    let c = plane.connection();
    *guard = Some(plane);
    app.data_gen.fetch_add(1, Ordering::SeqCst);
    Ok(c)
}

/// One skiff-host per computer, from this app's build: a local service of an
/// older version, or of this version from another build, is stopped with
/// SIGTERM and the bundled one started, once per app run, as the `skiff
/// watch` watcher is (`skipper::stale_holder`). So is one whose reply was
/// unreadable. It holds no session, so nothing is lost. A newer version
/// stays. One that does not stop is used as it is. One that does not come
/// back within 5 s is an error for this read: the next read starts it as for
/// any missing service. Only for a local endpoint (`_local` proves it).
async fn replace_older_data_service(app: &App, _local: &ProcessControl, selection: &HostSelection, host: &skiff_host_api::HostId, plane: DataPlane) -> Result<DataPlane, String> {
    let theirs = plane.connection().hello().service_versions.get("skiff-host").cloned().unwrap_or_default();
    let Some(pid) = plane.pid() else { return Ok(plane) };
    let unreadable = app.data_unreadable.load(Ordering::SeqCst);
    if !unreadable && !stale_service(&theirs).await {
        return Ok(plane);
    }
    if app.data_replaced.swap(true, Ordering::SeqCst) {
        return Ok(plane);
    }
    app.data_unreadable.store(false, Ordering::SeqCst);
    eprintln!("skiff-host {theirs} is not this app's build ({}): replacing it", skiff_core::VERSION);
    drop(plane);
    restart_data_service(app, _local, selection, host, pid).await
}

/// Stops this computer's `skiff-host` at `pid` and starts the bundled one.
async fn restart_data_service(app: &App, _local: &ProcessControl, selection: &HostSelection, host: &skiff_host_api::HostId, pid: u32) -> Result<DataPlane, String> {
    if !stop_daemon(_local, pid as i32, false).await {
        eprintln!("skiff-host (pid {pid}) did not stop; it keeps running");
        return selection.connect_data(host).await.map_err(|e| data_failure(&e, selection));
    }
    spawn_data_service(_local, selection.socket()).map_err(start_failure)?;
    *app.data_started.lock().unwrap() = Some(std::time::Instant::now());
    let mut last = None;
    for _ in 0..50 {
        match selection.connect_data(host).await {
            Ok(p) => return Ok(p),
            Err(e) => last = Some(e),
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Err(last.map(|e| data_failure(&e, selection)).unwrap_or_else(|| "The new projects service on this computer did not answer. Skiff checks again on its own.".into()))
}

/// The local `skiff-host` that reports `theirs` is not this app's: an older
/// version, or this version from another build or from a build that did not
/// name its build. A newer version is never replaced, nor is any when this
/// app's `skiff-host` cannot be read.
async fn stale_service(theirs: &str) -> bool {
    let (theirs, bin) = (theirs.to_string(), data_binary());
    tauri::async_runtime::spawn_blocking(move || skiff_client::host::data::stale_service(&theirs, skiff_core::VERSION, &bin)).await.unwrap_or(false)
}

/// The Hello of a data service this app cannot use: another Host API, or
/// one it cannot read.
fn unreadable(e: &anyhow::Error) -> bool {
    use skiff_host_api::Error as E;
    matches!(e.downcast_ref(), Some(E::IncompatibleVersion { .. } | E::MalformedContract { .. }))
}

/// The pid of the service at this computer's data socket, by its peer
/// credentials.
async fn data_service_pid(selection: &HostSelection) -> Option<u32> {
    let s = tokio::net::UnixStream::connect(skiff_core::socket::data_socket(selection.socket())).await.ok()?;
    s.peer_cred().ok()?.pid().and_then(|p| u32::try_from(p).ok())
}

/// Why this computer's `skiff-host` did not start.
fn start_failure(e: anyhow::Error) -> String {
    format!("Skiff cannot start the projects service on this computer ({e:#}). It tries again on its own.")
}

/// Why a data reply is not shown: the connection it came on was replaced.
pub(crate) const DATA_REPLACED: &str = "The connection to the projects service changed during the read. Skiff reads again.";

/// One data-plane read. Its reply is used only if the connection it came on
/// is still the selected Host's data connection: a reply from a connection
/// that a Host switch or a reconnect replaced is never shown.
pub(crate) async fn data_read<T, F, Fut>(app: &App, read: F) -> Result<T, String>
where
    F: FnOnce(Arc<HostConnection>) -> Fut,
    Fut: std::future::Future<Output = Result<T, skiff_host_api::Error>>,
{
    let c = data_now(app).await?;
    let result = read(c.clone()).await;
    let current = app.data.lock().await.as_ref().map(|d| d.connection());
    if !current.is_some_and(|d| Arc::ptr_eq(&d, &c)) {
        return Err(DATA_REPLACED.into());
    }
    result.map_err(|e| read_err(app, e))
}

/// A data-plane read error as the page shows it. An unreadable reply from
/// this computer's `skiff-host` marks it for a replace at the next
/// connection, once per run.
fn read_err(app: &App, e: skiff_host_api::Error) -> String {
    let skiff_host_api::Error::MalformedContract { .. } = e else { return data_err(e) };
    match app.selection().ssh_target().map(|t| t.destination.clone()) {
        Some(d) => format!("Skiff cannot read the replies from the projects service on {d}. Update Skiff… in Settings › Hosts updates it."),
        None if app.data_replaced.load(Ordering::SeqCst) => {
            "Skiff cannot read the replies from the projects service on this computer, also after a restart. Skiff checks again on its own.".into()
        }
        None => {
            app.data_unreadable.store(true, Ordering::SeqCst);
            "Skiff cannot read the replies from the projects service on this computer. It starts the service again.".into()
        }
    }
}

/// Why a change was not sent: no project read of this Host to make it from.
pub(crate) const DATA_UNREAD: &str = "Skiff has not read this Host's projects yet. It reads them again: try again.";
/// Why a change's outcome is unknown: its reply was lost or late.
pub(crate) const DATA_UNCERTAIN: &str =
    "The Host did not answer in time, so the change may or may not have been made. Skiff reads the projects again: check them before you try again.";

/// Keeps the revision of a project read or change from the Host `c` names.
pub(crate) fn keep_revision(app: &App, c: &HostConnection, revision: Option<String>) {
    *app.revision.lock().unwrap() = revision.map(|r| (c.hello().host.host_id.clone(), r));
}

/// One data-plane change, sent once and never again: not after a lost
/// connection or a late reply, whose outcome is then unknown.
pub(crate) async fn data_write<T, F, Fut>(app: &App, write: F) -> Result<T, String>
where
    F: FnOnce(Arc<HostConnection>) -> Fut,
    Fut: std::future::Future<Output = Result<T, skiff_host_api::Error>>,
{
    use skiff_host_api::Error as E;
    let c = data_now(app).await?;
    match write(c).await {
        Ok(out) => Ok(out),
        Err(E::TransportLost | E::TimedOut) => Err(DATA_UNCERTAIN.into()),
        Err(e) => Err(data_err(e)),
    }
}

/// A project change, made from this Host's last project read or change:
/// without one nothing is sent. Its answer's revision is kept for the next
/// change. A conflict forgets it, so the next change waits for the page to
/// read the projects again; the change is not sent again.
pub(crate) async fn project_write<T, F, Fut>(app: &App, write: F) -> Result<T, String>
where
    F: FnOnce(Arc<HostConnection>, String) -> Fut,
    Fut: std::future::Future<Output = Result<(T, String), skiff_host_api::Error>>,
{
    data_write(app, |c| async move {
        let host = c.hello().host.host_id.clone();
        let revision = app.revision.lock().unwrap().clone().filter(|(h, _)| *h == host).map(|(_, r)| r);
        let Some(revision) = revision else {
            return Err(skiff_host_api::Error::Invalid { message: DATA_UNREAD.into() });
        };
        match write(c.clone(), revision).await {
            Ok((out, revision)) => {
                keep_revision(app, &c, Some(revision));
                Ok(out)
            }
            Err(skiff_host_api::Error::Conflict { message }) => {
                keep_revision(app, &c, None);
                Err(skiff_host_api::Error::Conflict { message: format!("{message} Skiff reads them again: try again if it is still needed.") })
            }
            Err(e) => Err(e),
        }
    })
    .await
}

/// The data plane as the page shows it: reachable or why not, and which
/// connection, so the page reads projects and Git again after a new one.
#[derive(Serialize, Debug, PartialEq)]
pub(crate) struct DataPlaneStatus {
    ready: bool,
    /// No live session plane: the data plane is not tried.
    waiting: bool,
    host_id: Option<String>,
    instance_id: Option<String>,
    /// Changes with every new data connection.
    connection: u64,
    capabilities: Vec<String>,
    message: Option<String>,
}

/// Checks the data plane over the live session plane only: it never connects
/// or starts skiffd, and never touches a terminal stream. On This Mac a
/// missing `skiff-host` starts, as for any data read.
#[tauri::command]
pub(crate) async fn data_plane_status(app: State<'_, App>) -> Result<DataPlaneStatus, String> {
    Ok(data_status(&app).await)
}

pub(crate) async fn data_status(app: &App) -> DataPlaneStatus {
    let live = app.live_session().await;
    let mut status = DataPlaneStatus {
        ready: false,
        waiting: live.is_none(),
        host_id: None,
        instance_id: None,
        connection: app.data_gen.load(Ordering::SeqCst),
        capabilities: Vec::new(),
        message: None,
    };
    if live.is_none() {
        return status;
    }
    match data_now(app).await {
        Ok(c) => {
            status.ready = true;
            status.host_id = Some(c.hello().host.host_id.as_str().to_string());
            status.instance_id = Some(c.hello().instance_id.as_str().to_string());
            status.capabilities = capability_names(&c);
        }
        Err(e) => status.message = Some(e),
    }
    status.connection = app.data_gen.load(Ordering::SeqCst);
    status
}

/// Why the data plane is out of reach, in words that name no local path.
pub(crate) fn data_failure(e: &anyhow::Error, selection: &HostSelection) -> String {
    let host = selection.ssh_target().map(|t| t.destination.clone());
    let on = host.as_deref().map(|d| format!(" on {d}")).unwrap_or_default();
    if let Some(e) = e.downcast_ref::<skiff_client::host::ssh::SshError>() {
        return e.to_string();
    }
    match (e.downcast_ref::<DataError>(), e.downcast_ref::<skiff_host_api::Error>()) {
        (Some(DataError::NotRunning), _) if host.is_some() => format!(
            "The projects service does not run{on}, so Skiff cannot show its projects. Settings › Hosts starts it."
        ),
        (Some(DataError::NotRunning), _) => {
            "The projects service did not start on this computer, so Skiff cannot show the projects. It starts the service again on its own.".into()
        }
        (Some(DataError::Waiting { seconds }), _) => format!("Skiff cannot reach the projects service{on}. It tries again in {seconds} s."),
        (_, Some(skiff_host_api::Error::IdentityChanged { .. })) => format!(
            "The projects service{on} answers as another Skiff Host than the sessions service. Skiff sends it nothing and checks again on its own."
        ),
        (_, Some(skiff_host_api::Error::IncompatibleVersion { .. })) if host.is_some() => {
            format!("The projects service{on} is from another Skiff version. Update Skiff… in Settings › Hosts updates it.")
        }
        (_, Some(skiff_host_api::Error::IncompatibleVersion { .. } | skiff_host_api::Error::MalformedContract { .. })) if host.is_none() => {
            "The projects service on this computer is from another Skiff build, and Skiff could not replace it. It checks again on its own.".into()
        }
        _ => format!("Skiff cannot reach the projects service{}. It checks again on its own.", if on.is_empty() { " on this computer" } else { &on }),
    }
}

/// A data-plane reply error as the page shows it.
pub(crate) fn data_err(e: skiff_host_api::Error) -> String {
    skiff_client::host::data::message(e)
}

pub(crate) fn data_binary() -> PathBuf {
    bundled("skiff-host", Some("SKIFF_HOST_BIN"))
}

/// How long after one start of `skiff-host` the next may come.
const DATA_START_WAIT: Duration = Duration::from_secs(10);

/// Only for a local endpoint: `_local` proves it. Starts this Mac's
/// data-plane service for the skiffd at `session`, on that socket's state.
/// A second start finds the first one listening and exits.
fn spawn_data_service(_local: &ProcessControl, session: &std::path::Path) -> anyhow::Result<()> {
    let mut cmd = std::process::Command::new(data_binary());
    cmd.arg("--session-socket").arg(session).arg("--this-mac");
    reap(spawn_detached(cmd, skiff_core::socket::data_socket(session).with_extension("log"))?);
    Ok(())
}

/// Waits for the data service on a thread of its own, so one that exits or
/// is stopped leaves no zombie under the app. Waiting changes nothing else:
/// the service keeps its own process group, outlives the app and is never
/// signalled from here.
fn reap(mut child: std::process::Child) {
    let _ = std::thread::Builder::new().name("skiff-host reaper".into()).spawn(move || {
        let _ = child.wait();
    });
}

fn capability_names(c: &HostConnection) -> Vec<String> {
    c.hello().capabilities.iter().map(|c| format!("{}@{}", c.name, c.version)).collect()
}


pub(crate) async fn ensure_host(app: &App) -> Result<(Arc<SelectedHost>, bool), String> {
    let mut guard = app.host.lock().await;
    if let Some(host) = guard.as_ref() {
        if !host.connection().is_closed() {
            return Ok((host.clone(), false));
        }
    }
    *guard = None;
    let selection = app.selection();
    if !selection.is_local() {
        return connect_remote(app, &selection, &mut guard).await;
    }
    let path = selection.socket();
    // Starting a daemon is endpoint-local lifecycle. Without it this only connects.
    let control = selection.process_control();
    let mut spawned = false;
    let mut replaced = false;
    let mut last = String::new();
    for _ in 0..40 {
        let mut c = match selection.connect().await {
            Ok(c) => c,
            Err(e) => {
                last = format!("{e:#}");
                // A reload failed: starting a new daemon is the user's call.
                if let Some(why) = app.failed.lock().unwrap().clone() {
                    return Err(why);
                }
                if !spawned {
                    if let Some(control) = &control {
                        spawn_daemon(control).map_err(|e| format!("start skiffd: {e:#}"))?;
                        spawned = true;
                    }
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
        };
        // Recorded for activation: the Host's identity comes from this pong.
        let info = tokio::time::timeout(HELLO_TIMEOUT, c.daemon_info()).await.ok().and_then(|r| r.ok());
        match check_info(app, c.client(), info, !replaced).await {
            Check::Use => {
                *app.failed.lock().unwrap() = None;
                let host = Arc::new(c.activate().await.map_err(err)?);
                *app.live.lock().unwrap() = Some(host.connection());
                *guard = Some(host.clone());
                return Ok((host, spawned));
            }
            Check::Replaced => {
                replaced = true;
                spawned = false;
            }
            Check::Hung => return Err("skiffd is not responding".into()),
        }
    }
    Err(format!("cannot reach skiffd at {}: {last}", path.display()))
}

pub(crate) enum Check {
    Use,
    /// The old daemon was stopped: connect again, to a fresh one.
    Replaced,
    Hung,
}

/// Compares the daemon with this app. Version order decides, whatever the
/// build: one computer runs one skiffd, and the newer build updates it. An
/// older daemon that can reload is planned for a reload onto the bundled
/// skiffd, which keeps its sessions. The same version stays as it is. Else
/// an older daemon, or an incompatible one that is not newer, with no live
/// session is stopped with SIGTERM and replaced without a word. A newer
/// daemon is never replaced. One that does not stop, or has live sessions,
/// keeps running: this records a warning that says the update is not
/// finished.
pub(crate) async fn check_daemon(app: &App, c: &Client, may_replace: bool) -> Check {
    let info = tokio::time::timeout(HELLO_TIMEOUT, c.daemon_info()).await.ok().and_then(|r| r.ok());
    check_info(app, c, info, may_replace).await
}

/// [`check_daemon`] with the daemon's `pong` already read; `None`: no answer.
async fn check_info(app: &App, c: &Client, info: Option<skiff_client::DaemonInfo>, may_replace: bool) -> Check {
    let Some(info) = info else {
        *app.warning.lock().await = Some(Warning {
            kind: "hung",
            message: format!(
                "skiffd accepts connections but did not answer within {}s. \
                 This app cannot reach its sessions. Restarting stops it and starts a new one.",
                HELLO_TIMEOUT.as_secs()
            ),
            sessions: None,
        });
        return Check::Hung;
    };
    let version = info.version.clone();
    // The skiffd this app ships with, not the app version.
    let order = compare_versions(&version, skiff_core::VERSION);
    let same_protocol = info.protocol == skiff_core::PROTOCOL;
    if same_protocol && order.is_eq() {
        *app.warning.lock().await = None;
        return Check::Use;
    }
    // Never a downgrade, and one try per app run. Planning runs the bundled
    // skiffd and replacing signals the daemon: endpoint-local only.
    let control = app.selection().process_control();
    let mut old_build = None;
    let planned = order.is_lt() && !app.reload_tried.load(Ordering::Relaxed);
    if let (true, Some(local)) = (planned, &control) {
        match reload::plan(local, &info).await {
            Ok(plan) => {
                *app.reload.lock().await = Some(plan);
                *app.warning.lock().await = None;
                return Check::Use;
            }
            Err(NoPlan::Stale(bundled)) => old_build = Some(bundled),
            Err(NoPlan::Other(why)) => {
                eprintln!("skiffd {version} does not reload: {why}");
                *app.no_reload.lock().unwrap() = Some(why);
            }
        }
    }
    let why = app.no_reload.lock().unwrap().clone().filter(|_| order.is_lt());
    let live = tokio::time::timeout(HELLO_TIMEOUT, c.list_sessions())
        .await
        .ok()
        .and_then(|r| r.ok())
        .map(|v| v.iter().filter(|s| s.state != SessionState::Done).count());
    // The skiffd this app starts is no newer than the one that runs. A
    // restart starts the same build again and asks again: no prompt, and no
    // silent replace. Only a dev build gets here.
    if let Some(bundled) = old_build {
        let why = format!(
            "the skiffd this app starts ({}) is {bundled}, older than the app's {}. Rebuild it: cargo build -p skiffd",
            daemon_binary().display(),
            skiff_core::VERSION
        );
        eprintln!("skiffd {version} does not reload: {why}");
        *app.warning.lock().await = match same_protocol {
            true => None,
            false => Some(restart_warning(&version, info.protocol, live, Some(&why))),
        };
        return Check::Use;
    }
    // Never a downgrade: a newer daemon keeps running, whatever its protocol.
    let stale = !order.is_gt() && (!same_protocol || order.is_lt());
    // Never SIGKILL here: a daemon that does not stop keeps running as it is.
    if let (true, true, Some(0), Some(pid), Some(control)) = (may_replace, stale, live, c.daemon_pid, control) {
        if stop_daemon(&control, pid, false).await {
            *app.replaced.lock().await = Some(version);
            return Check::Replaced;
        }
    }
    *app.warning.lock().await = Some(restart_warning(&version, info.protocol, live, why.as_deref()));
    Check::Use
}

/// The warning for a daemon that differs from this app and keeps running.
/// `why`: the reload that failed, and the daemon carries on as before.
pub(crate) fn restart_warning(version: &str, protocol: u32, live: Option<usize>, why: Option<&str>) -> Warning {
    let ours = skiff_core::VERSION;
    let n = live.unwrap_or(0);
    let running = match n {
        1 => "1 session is running".to_string(),
        n => format!("{n} sessions are running"),
    };
    let ends = "They keep running. A restart stops them, and panes come back as shells in their folders.";
    let failed = why.map(|w| format!("It did not reload: {w}. ")).unwrap_or_default();
    let order = compare_versions(version, ours);
    if protocol != skiff_core::PROTOCOL {
        Warning {
            kind: "protocol",
            message: format!(
                "skiffd {version} speaks another protocol than this app ({ours}). {failed}\
                 Parts of the app fail while it runs. {running}. {ends}"
            ),
            sessions: live,
        }
    } else if !order.is_gt() {
        // The words `skiff status` uses too.
        let why = why.unwrap_or("it did not hand over its sessions");
        Warning {
            kind: "outdated",
            message: skiff_client::update::unfinished(version, ours, why, live, "Restart skiffd"),
            sessions: live,
        }
    } else {
        Warning {
            kind: "newer",
            message: format!(
                "skiffd {version} is newer than this app ({ours}). \
                 They speak the same protocol, so the app keeps using it."
            ),
            sessions: live,
        }
    }
}

pub(crate) use skiff_core::compare_versions;

/// SIGTERM, up to 3 s to exit. With `escalate`, which only a restart the
/// user asked for passes, then SIGKILL and up to 2 s more: a hung daemon may
/// ignore SIGTERM. True once the process is gone: a new daemon refuses to
/// start while the old one still holds the socket.
async fn stop_daemon(_local: &ProcessControl, pid: i32, escalate: bool) -> bool {
    // SAFETY: kill and waitpid take plain integers and touch no memory of ours.
    let gone = || unsafe {
        // The app may have started it: reap the zombie, which kill(0) still sees.
        libc::waitpid(pid, std::ptr::null_mut(), libc::WNOHANG) == pid
            || (libc::kill(pid, 0) != 0
                && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH))
    };
    let steps: &[(i32, u32)] = if escalate { &[(libc::SIGTERM, 30), (libc::SIGKILL, 20)] } else { &[(libc::SIGTERM, 30)] };
    for &(signal, ticks) in steps {
        if unsafe { libc::kill(pid, signal) } != 0 {
            return gone();
        }
        for _ in 0..ticks {
            if gone() {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    false
}

/// Stops the running daemon, which ends its sessions, and starts a new one.
#[tauri::command]
pub(crate) async fn restart_daemon(app: State<'_, App>) -> Result<DaemonStatus, String> {
    {
        let mut guard = app.host.lock().await;
        // A peer pid is the local socket's peer: signal it only for a local endpoint.
        let selection = app.selection();
        let control = selection.process_control().ok_or("skiffd on this Host is not managed by this app")?;
        let pid = match guard.as_ref() {
            Some(host) => host.client().daemon_pid,
            None => selection.connect().await.ok().and_then(|c| c.client().daemon_pid),
        };
        *guard = None;
        *app.live.lock().unwrap() = None;
        if let Some(pid) = pid {
            if !stop_daemon(&control, pid, true).await {
                return Err(format!("skiffd (pid {pid}) did not stop"));
            }
        }
        *app.warning.lock().await = None;
        *app.failed.lock().unwrap() = None;
        *app.no_reload.lock().unwrap() = None;
        *app.reload.lock().await = None;
    }
    for (_, task) in app.subs.lock().await.drain() {
        task.abort();
    }
    daemon_status(app).await
}

pub(crate) fn err(e: anyhow::Error) -> String {
    format!("{e:#}")
}

/// Runs a call that may wait, such as a file lock, off the async workers.
pub(crate) async fn blocking<T: Send + 'static>(f: impl FnOnce() -> anyhow::Result<T> + Send + 'static) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(f).await.map_err(|e| e.to_string())?.map_err(err)
}

#[tauri::command]
pub(crate) async fn daemon_status(app: State<'_, App>) -> Result<DaemonStatus, String> {
    Ok(status_now(&app).await)
}

pub(crate) async fn status_now(app: &App) -> DaemonStatus {
    let selection = app.selection();
    // A remote Host shows as its destination and socket. Its forward's peer
    // pid is this Mac's `ssh`, never the daemon: it is not shown.
    let socket = match selection.ssh_target() {
        Some(t) => format!("{}:{}", t.destination, selection.remote_socket().unwrap_or_default()),
        None => selection.socket().display().to_string(),
    };
    let local = selection.is_local();
    match ensure_host(app).await {
        Ok((host, spawned)) => {
            let c = host.client();
            let hello = tokio::time::timeout(HELLO_TIMEOUT, connection_status(&host.connection())).await.ok().and_then(|r| r.ok());
            DaemonStatus {
                connected: true,
                version: hello.as_ref().map(|(v, _)| v.clone()),
                protocol: hello.map(|(_, p)| p),
                socket,
                pid: c.daemon_pid.filter(|_| local),
                spawned,
                warning: app.warning.lock().await.clone(),
                replaced: app.replaced.lock().await.clone(),
                reload: app.reload.lock().await.is_some(),
            }
        }
        Err(_) => DaemonStatus {
            connected: false,
            version: None,
            protocol: None,
            socket,
            pid: None,
            spawned: false,
            warning: app.warning.lock().await.clone(),
            replaced: None,
            reload: false,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::{compare_versions, copy_out, spawn_data_service, stop_daemon, App, ProcessControl};
    use std::{path::PathBuf, time::Duration};
    use std::cmp::Ordering::*;

    /// This Mac's settings writes do not wait on skiff-host: an old one that
    /// lacks them, or none, still leaves them offered, each once.
    #[test]
    fn this_mac_offers_its_own_settings_writes() {
        let mut none = vec!["session_inventory@1".to_string()];
        super::add_local_adapter(&mut none);
        assert_eq!(none, ["session_inventory@1", "agent_config@1", "skipper_config@1", "skipper@1", "skipper_file@1", "skipper_run@1", "skipper_run@2", "session_agents@1", "daemon_metrics@1"]);
        let mut current = ["skipper_config@1", "agent_config@1", "skipper@1", "skipper_file@1", "skipper_run@1", "skipper_run@2", "session_agents@1", "daemon_metrics@1"].map(String::from).to_vec();
        super::add_local_adapter(&mut current);
        assert_eq!(current.len(), 8);
    }

    #[tokio::test]
    async fn status_and_legacy_commands_share_the_selected_local_connection() {
        use super::*;
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        let dir = std::env::temp_dir().join(format!("skiff-m1a-desktop-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir(&dir).unwrap();
        let socket = dir.join("selected.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        // Existing data remains byte-for-byte unchanged by the new bootstrap.
        let config = dir.join("projects.toml");
        let ledger = dir.join("ledger.db");
        std::fs::write(&config, "# existing config\n").unwrap();
        std::fs::write(&ledger, b"existing ledger fixture").unwrap();
        let app = App::default();
        app.set_selection(HostSelection::new(socket.clone(), dir.join("local-host-id")));
        let inventory_fixture: serde_json::Value = serde_json::from_str(include_str!("../../../../crates/skiff-host-api/tests/fixtures/m1c.json")).unwrap();
        let inventory = inventory_fixture["reply"]["sessions"].clone();
        let expected_inventory: Vec<skiff_core::session::SessionInfo> = serde_json::from_value(inventory.clone()).unwrap();
        let updates_fixture: serde_json::Value = serde_json::from_str(include_str!("../../../../crates/skiff-host-api/tests/fixtures/m1d.json")).unwrap();
        let page_events = updates_fixture["legacy"].as_array().unwrap().clone();
        let wire_events = page_events.clone();
        let peer = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut stream = BufReader::new(stream);
            for id in 1..=3 {
                let mut line = String::new();
                stream.read_line(&mut line).await.unwrap();
                assert_eq!(serde_json::from_str::<serde_json::Value>(&line).unwrap(), serde_json::json!({"id":id,"cmd":"ping"}));
                let reply = serde_json::json!({"id":id,"type":"pong","version":skiff_core::VERSION,"protocol":skiff_core::PROTOCOL});
                let mut bytes = serde_json::to_vec(&reply).unwrap(); bytes.push(b'\n');
                stream.get_mut().write_all(&bytes).await.unwrap();
            }
            for (id, response) in [
                (4, serde_json::json!({"type":"screen","lines":["", "λ  "]})),
                (5, serde_json::json!({"type":"error","message":"unknown variant `screen`"})),
            ] {
                let mut line = String::new();
                stream.read_line(&mut line).await.unwrap();
                assert_eq!(serde_json::from_str::<serde_json::Value>(&line).unwrap(), serde_json::json!({"id":id,"cmd":"screen","session":"snapshot","rows":2}));
                let mut response = response; response["id"] = id.into();
                let mut bytes = serde_json::to_vec(&response).unwrap(); bytes.push(b'\n');
                stream.get_mut().write_all(&bytes).await.unwrap();
            }
            for (id, response) in [
                (6, serde_json::json!({"type":"sessions","sessions":inventory})),
                (7, serde_json::json!({"type":"error","message":"inventory unavailable"})),
            ] {
                let mut line = String::new();
                stream.read_line(&mut line).await.unwrap();
                assert_eq!(serde_json::from_str::<serde_json::Value>(&line).unwrap(), serde_json::json!({"id":id,"cmd":"list_sessions"}));
                let mut response = response; response["id"] = id.into();
                let mut bytes = serde_json::to_vec(&response).unwrap(); bytes.push(b'\n');
                stream.get_mut().write_all(&bytes).await.unwrap();
            }
            // Pushed events, with no subscribe request from the page stream.
            for event in wire_events {
                let mut bytes = serde_json::to_vec(&event).unwrap(); bytes.push(b'\n');
                stream.get_mut().write_all(&bytes).await.unwrap();
            }
            // M1e: the terminal plane sends exactly the legacy frames.
            let legacy = |id: u64, request: skiff_core::protocol::Request| serde_json::to_value(skiff_core::protocol::Envelope { id, request }).unwrap();
            let mut line = String::new();
            stream.read_line(&mut line).await.unwrap();
            assert_eq!(serde_json::from_str::<serde_json::Value>(&line).unwrap(), legacy(8, skiff_core::protocol::Request::Subscribe { session: "t1".into() }));
            // The snapshot and output precede the reply, as skiffd may send them.
            for frame in [
                serde_json::json!({"event":"snapshot","session":"t1","data":"AP8KDQ==","cols":80,"rows":24}),
                serde_json::json!({"event":"output","session":"other","data":"eA=="}),
                serde_json::json!({"event":"output","session":"t1","data":"bHMK","echo":{"wrote":1,"read":2}}),
                serde_json::json!({"id":8,"type":"ok"}),
            ] {
                let mut bytes = serde_json::to_vec(&frame).unwrap(); bytes.push(b'\n');
                stream.get_mut().write_all(&bytes).await.unwrap();
            }
            line.clear();
            stream.read_line(&mut line).await.unwrap();
            assert_eq!(serde_json::from_str::<serde_json::Value>(&line).unwrap(), legacy(9, skiff_core::protocol::Request::Write { session: "t1".into(), data: vec![0, 255, b'\n'], paste: false }));
            line.clear();
            stream.read_line(&mut line).await.unwrap();
            assert_eq!(serde_json::from_str::<serde_json::Value>(&line).unwrap(), legacy(10, skiff_core::protocol::Request::Write { session: "t1".into(), data: b"ls\n".to_vec(), paste: false }));
            for (id, request, reply) in [
                (11, skiff_core::protocol::Request::Resize { session: "t1".into(), cols: 100, rows: 30 }, serde_json::json!({"type":"ok"})),
                (12, skiff_core::protocol::Request::Resize { session: "gone".into(), cols: 1, rows: 1 }, serde_json::json!({"type":"error","message":"no such session"})),
                (13, skiff_core::protocol::Request::Unsubscribe { session: "t1".into() }, serde_json::json!({"type":"ok"})),
            ] {
                line.clear();
                stream.read_line(&mut line).await.unwrap();
                assert_eq!(serde_json::from_str::<serde_json::Value>(&line).unwrap(), legacy(id, request));
                let mut reply = reply; reply["id"] = id.into();
                let mut bytes = serde_json::to_vec(&reply).unwrap(); bytes.push(b'\n');
                stream.get_mut().write_all(&bytes).await.unwrap();
            }
            // M1g: lifecycle and groups send exactly the legacy frames.
            let spec: skiff_core::session::SessionSpec = serde_json::from_value(serde_json::json!({"label":"sh","cwd":"/srv/λ","cols":90,"rows":20})).unwrap();
            let group: skiff_core::group::Group = serde_json::from_value(serde_json::json!({"name":"G","layout":{"type":"pane","session":"t1"}})).unwrap();
            let mut row = inventory[0].clone();
            row["id"] = "made".into();
            for (id, request, reply) in [
                (14, skiff_core::protocol::Request::CreateSession { spec }, serde_json::json!({"type":"session","session":row})),
                (15, skiff_core::protocol::Request::Kill { session: "gone".into() }, serde_json::json!({"type":"error","message":"no such session"})),
                (16, skiff_core::protocol::Request::ListGroups, serde_json::json!({"type":"groups","groups":[]})),
                (17, skiff_core::protocol::Request::SaveGroup { group }, serde_json::json!({"type":"group","group":{"id":"g1","name":"G","layout":{"type":"pane","session":"t1"},"focus":null}})),
            ] {
                line.clear();
                stream.read_line(&mut line).await.unwrap();
                assert_eq!(serde_json::from_str::<serde_json::Value>(&line).unwrap(), legacy(id, request));
                let mut reply = reply; reply["id"] = id.into();
                let mut bytes = serde_json::to_vec(&reply).unwrap(); bytes.push(b'\n');
                stream.get_mut().write_all(&bytes).await.unwrap();
            }
            // Keep the one connection alive through the reuse assertions.
            line.clear();
            stream.read_line(&mut line).await.ok();
        });
        let first = status_now(&app).await;
        assert!(first.connected && !first.spawned);
        assert_eq!(first.version.as_deref(), Some(skiff_core::VERSION));
        assert_eq!(first.protocol, Some(skiff_core::PROTOCOL));
        assert_eq!(first.socket, socket.display().to_string());
        assert!(first.warning.is_none() && !first.reload);
        let owner = app.host.lock().await.as_ref().unwrap().clone();
        let mut updates = skiff_client::host::local::legacy_updates(&owner.connection()).unwrap();
        let (client, spawned) = ensure_client(&app).await.unwrap();
        assert!(!spawned && Arc::ptr_eq(&owner.client(), &client));
        let live = app.live.lock().unwrap().clone().unwrap();
        assert!(Arc::ptr_eq(&live, &owner.connection()));
        let second = status_now(&app).await;
        assert_eq!(second.version, first.version);
        assert_eq!(screen_now(&app, "snapshot", 2).await.unwrap(), vec!["", "λ  "]);
        assert_eq!(screen_now(&app, "snapshot", 2).await.unwrap_err(), "unknown variant `screen`");
        let listed = inventory_now(&app).await.unwrap();
        assert_eq!(listed, expected_inventory);
        assert_eq!(serde_json::to_value(&listed).unwrap(), serde_json::to_value(&expected_inventory).unwrap());
        assert_eq!(inventory_now(&app).await.unwrap_err(), "inventory unavailable");
        assert!(Arc::ptr_eq(&owner, app.host.lock().await.as_ref().unwrap()));
        for want in &page_events {
            assert_eq!(serde_json::to_value(updates.recv().await.unwrap()).unwrap(), *want);
        }
        // M1e: attach/output/input/resize/detach on the same selected owner.
        let terminal = host_now(&app).await.unwrap();
        assert!(Arc::ptr_eq(&terminal, &live));
        let mut output = skiff_client::host::local::legacy_output(&terminal, "t1").unwrap();
        skiff_client::host::local::legacy_attach(&terminal, "t1").await.unwrap();
        use skiff_client::host::TerminalFrame;
        assert_eq!(output.recv().await, Ok(TerminalFrame::Snapshot { data: vec![0, 255, 10, 13], cols: 80, rows: 24 }));
        let Ok(TerminalFrame::Output { data, echo: Some(echo) }) = output.recv().await else { panic!("no output") };
        assert_eq!(data, b"ls\n");
        // The latency trace reads the same JSON as before.
        assert_eq!(serde_json::to_value(echo).unwrap(), serde_json::json!({"wrote":1,"read":2}));
        // Input through the bridge, in order, with arbitrary bytes.
        assert!(matches!(app.input.write(Some(&live), "t1", vec![0, 255, b'\n']).unwrap(), crate::input::Sent::Done));
        assert!(matches!(app.input.write(Some(&live), "t1", b"ls\n".to_vec()).unwrap(), crate::input::Sent::Done));
        skiff_client::host::local::legacy_resize(&terminal, "t1", 100, 30).await.unwrap();
        assert_eq!(skiff_client::host::local::legacy_resize(&terminal, "gone", 1, 1).await.unwrap_err().to_string(), "no such session");
        skiff_client::host::local::legacy_detach(&terminal, "t1").await.unwrap();
        // Detach closes the receiver first, as unsubscribe did.
        assert_eq!(output.recv().await, Err(skiff_client::host::OutputGap::Closed));
        let mut reattached = skiff_client::host::local::legacy_output(&terminal, "t1").unwrap();
        // M1g: lifecycle and groups on the same selected owner, and the
        // capabilities the page reads.
        assert_eq!(capability_names(&terminal), ["connection_status@1", "session_screen@1", "session_inventory@1", "session_updates@1", "session_terminal@1", "session_lifecycle@1", "session_groups@1", "session_agents@1"]);
        let made = skiff_client::host::local::legacy_create(&terminal, serde_json::from_value(serde_json::json!({"label":"sh","cwd":"/srv/λ","cols":90,"rows":20})).unwrap()).await.unwrap();
        assert_eq!(made.id, "made");
        assert_eq!(skiff_client::host::local::legacy_kill(&terminal, "gone").await.unwrap_err().to_string(), "no such session");
        assert!(skiff_client::host::local::legacy_groups(&terminal).await.unwrap().is_empty());
        let saved = skiff_client::host::local::legacy_save_group(&terminal, serde_json::from_value(serde_json::json!({"name":"G","layout":{"type":"pane","session":"t1"}})).unwrap()).await.unwrap();
        assert_eq!(saved.id, "g1");
        assert!(Arc::ptr_eq(&owner, app.host.lock().await.as_ref().unwrap()));
        drop(terminal);
        drop(live);
        // Dropping the selected owner, as reconnect/restart/reload do, ends it.
        drop(owner);
        drop(client);
        *app.host.lock().await = None;
        assert!(updates.recv().await.is_none());
        assert_eq!(reattached.recv().await, Err(skiff_client::host::OutputGap::Closed));
        *app.live.lock().unwrap() = None;
        assert_eq!(std::fs::read_to_string(config).unwrap(), "# existing config\n");
        assert_eq!(std::fs::read(ledger).unwrap(), b"existing ledger fixture");
        peer.abort();
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A remote Host is reached, never managed: a failing SSH endpoint gets
    /// an explanation, and the app starts, reloads and signals nothing here.
    #[test]
    fn a_remote_endpoint_never_runs_local_lifecycle_and_reports_why() {
        use super::*;
        let _env = crate::reload::tests::ENV.lock().unwrap_or_else(|e| e.into_inner());
        tokio::runtime::Runtime::new().unwrap().block_on(async {
        let dir = std::env::temp_dir().join(format!("skiff-remote-life-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir(&dir).unwrap();
        // A local "skiffd" and "ssh" that only leave a mark.
        let mark = |name: &str| {
            let script = dir.join(name);
            std::fs::write(&script, format!("#!/bin/sh\necho \"$@\" >> {}/{name}.ran\nexit 255\n", dir.display())).unwrap();
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
            script
        };
        let old_daemon = std::env::var_os("SKIFF_DAEMON");
        std::env::set_var("SKIFF_DAEMON", mark("skiffd"));
        let old_data = std::env::var_os("SKIFF_HOST_BIN");
        std::env::set_var("SKIFF_HOST_BIN", mark("skiff-host"));
        let app = App::default();
        let target = skiff_client::host::ssh::SshTarget::new("devbox", "/run/user/1/skiff/skiffd.sock");
        // Review P2: an unknown host key from a fresh state reaches the status.
        let ssh = mark("ssh");
        let text = std::fs::read_to_string(&ssh).unwrap().replace("exit 255", "echo 'Host key verification failed.' >&2\nexit 255");
        std::fs::write(&ssh, text).unwrap();
        let options = skiff_client::host::ssh::SshOptions { program: ssh, config: None };
        app.set_selection(HostSelection::ssh(target, options, dir.join("fwd"), None).unwrap());
        assert!(app.remote());
        assert!(app.selection().process_control().is_none());
        let started = std::time::Instant::now();
        let status = status_now(&app).await;
        assert!(!status.connected && !status.spawned && status.pid.is_none());
        assert_eq!(status.socket, "devbox:/run/user/1/skiff/skiffd.sock");
        let cause = status.warning.as_ref().expect("the cause reaches the status");
        assert_eq!(cause.kind, "remote");
        assert!(cause.message.contains("does not know the host key of devbox") && !cause.message.contains("verification failed"), "{}", cause.message);
        // One ssh run, no retry loop, and a second try waits.
        assert!(started.elapsed() < Duration::from_secs(5));
        let e = ensure_host(&app).await.err().unwrap();
        assert!(e.contains("tries again"), "{e}");
        // The wait keeps the cause: the status still says what to fix.
        let again = status_now(&app).await;
        assert!(again.warning.is_some_and(|w| w.message.contains("host key")));
        assert_eq!(std::fs::read_to_string(dir.join("ssh.ran")).unwrap().lines().count(), 1);
        assert!(!dir.join("skiffd.ran").exists(), "the local daemon binary ran for a remote Host");
        // M4: no data-plane service is started for a remote Host, here or there.
        assert!(data_now(&app).await.is_err());
        assert!(!dir.join("skiff-host.ran").exists(), "the data-plane binary ran for a remote Host");
        // Switching back to this Mac drops the remote selection and its forward.
        replace_selection(&app, local_selection()).await;
        assert!(!app.remote());
        match old_daemon {
            Some(v) => std::env::set_var("SKIFF_DAEMON", v),
            None => std::env::remove_var("SKIFF_DAEMON"),
        }
        match old_data {
            Some(v) => std::env::set_var("SKIFF_HOST_BIN", v),
            None => std::env::remove_var("SKIFF_HOST_BIN"),
        }
        let _ = std::fs::remove_dir_all(&dir);
        });
    }

    /// Review P2: each remote failure has a cause the status can show, in
    /// words that never repeat ssh's output; a wait keeps the last cause.
    #[test]
    fn remote_failures_name_a_cause_and_a_wait_keeps_the_last_one() {
        use skiff_client::host::ssh::SshError;
        let d = "devbox";
        let cause = |e: anyhow::Error| super::remote_failure(&e, d);
        for e in [
            SshError::HostKeyUnknown(d.into()),
            SshError::HostKeyChanged(d.into()),
            SshError::AuthFailed(d.into()),
            SshError::NoSkiffd { destination: d.into(), socket: "/s.sock".into() },
            SshError::ForwardingRefused(d.into()),
        ] {
            let text = e.to_string();
            let (w, message) = cause(e.into());
            assert_eq!(w.map(|w| (w.kind, w.message)), Some(("remote", text.clone())));
            assert_eq!(message, text);
        }
        let (w, _) = cause(SshError::Waiting { destination: d.into(), seconds: 4 }.into());
        assert!(w.is_none());
        let id = |s: &str| skiff_host_api::HostId::try_from(s.to_string()).unwrap();
        let (w, _) = cause(skiff_host_api::Error::IdentityChanged { expected: id("A"), received: id("B") }.into());
        assert_eq!(w.unwrap().kind, "identity");
        // An unknown failure names no path or ssh line: one line, what to check.
        let (w, _) = cause(anyhow::anyhow!("connect /var/folders/x/T/skiff-501-1/9-0/skiffd.sock: No such file"));
        let w = w.unwrap();
        assert!(w.message.contains("Settings › Hosts") && !w.message.contains("/var/folders"), "{}", w.message);
    }

    fn local_control() -> ProcessControl {
        App::default().selection().process_control().unwrap()
    }

    #[test]
    fn copies_skiffd_out_of_the_appimage_once_per_change() {
        let dir = std::env::temp_dir().join(format!("skiff-copy-out-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("mount")).unwrap();
        let src = dir.join("mount/skiffd");
        std::fs::write(&src, b"one").unwrap();
        let out = dir.join("bin");
        let dest = copy_out(&src, &out).unwrap();
        assert_eq!(dest, out.join("skiffd"));
        assert_eq!(std::fs::read(&dest).unwrap(), b"one");
        let first = std::fs::metadata(&dest).unwrap();
        assert_eq!(copy_out(&src, &out).unwrap(), dest);
        // The same bytes: the file stays, so a running skiffd's path still names it.
        use std::os::unix::fs::MetadataExt;
        assert_eq!(std::fs::metadata(&dest).unwrap().ino(), first.ino());
        std::fs::write(&src, b"two").unwrap();
        copy_out(&src, &out).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"two");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn orders_versions_by_number() {
        assert_eq!(compare_versions("0.3.2", "0.5.0"), Less);
        assert_eq!(compare_versions("0.10.0", "0.9.9"), Greater);
        assert_eq!(compare_versions("0.5.0", "0.5.0"), Equal);
    }

    #[test]
    #[allow(clippy::zombie_processes, reason = "stop_daemon reaps the pid with waitpid")]
    fn kills_a_process_that_ignores_sigterm() {
        let child = std::process::Command::new("sh")
            // exec keeps the ignored TERM disposition and leaves no child
            // sleep behind when the SIGKILL fallback stops this process.
            .args(["-c", "trap '' TERM; exec sleep 30"])
            .spawn()
            .unwrap();
        // Let the shell set its trap first.
        std::thread::sleep(std::time::Duration::from_millis(200));
        let rt = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
        assert!(rt.block_on(stop_daemon(&local_control(), child.id() as i32, true)));
    }

    /// A data service that exits, or is stopped to restart, is reaped: no
    /// zombie stays under the app. The service runs from a private fake
    /// binary; no live service is started or signalled.
    #[test]
    fn a_data_service_that_ends_leaves_no_zombie() {
        let _env = crate::reload::tests::ENV.lock().unwrap_or_else(|e| e.into_inner());
        let dir = PathBuf::from("/tmp").join(format!("reap-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir(&dir).unwrap();
        let old = std::env::var_os("SKIFF_HOST_BIN");
        // Gone: no such process, and not a zombie, which kill(0) still finds.
        let gone = |pid: i32| unsafe { libc::kill(pid, 0) != 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) };
        let until_gone = |pid: i32| (0..50).any(|_| gone(pid) || (std::thread::sleep(Duration::from_millis(100)), false).1);
        for (name, body) in [("exits", "exit 0"), ("stopped", "exec sleep 30")] {
            let bin = dir.join(name);
            let pidfile = dir.join(format!("{name}.pid"));
            std::fs::write(&bin, format!("#!/bin/sh\necho $$ > {}\n{body}\n", pidfile.display())).unwrap();
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
            std::env::set_var("SKIFF_HOST_BIN", &bin);
            spawn_data_service(&local_control(), &dir.join(format!("{name}.sock"))).unwrap();
            let pid = (0..50)
                .find_map(|_| std::fs::read_to_string(&pidfile).ok().and_then(|p| p.trim().parse::<i32>().ok()).or_else(|| (std::thread::sleep(Duration::from_millis(100)), None).1))
                .expect("the fake service started");
            if name == "stopped" {
                assert!(!gone(pid), "it runs until stopped");
                unsafe { libc::kill(pid, libc::SIGTERM) };
            }
            assert!(until_gone(pid), "{name}: the service stays a zombie");
        }
        match old {
            Some(v) => std::env::set_var("SKIFF_HOST_BIN", v),
            None => std::env::remove_var("SKIFF_HOST_BIN"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The automatic stop sends SIGTERM only. A process that ignores it
    /// keeps running, and the stop says so.
    #[test]
    fn the_automatic_stop_never_kills() {
        let mut child = std::process::Command::new("sh")
            .args(["-c", "trap '' TERM; exec sleep 30"])
            .spawn()
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(200));
        let rt = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
        let stopped = rt.block_on(stop_daemon(&local_control(), child.id() as i32, false));
        let running = child.try_wait().unwrap().is_none();
        let _ = child.kill();
        let _ = child.wait();
        assert!(!stopped);
        assert!(running, "the process was stopped");
    }

    /// A stand-in daemon on a private socket: it answers `ping` as
    /// `version` on `protocol` and lists no session. The connection names
    /// `pid` as the daemon, a process the test owns, so a stop shows on it.
    /// With `reloads`, the pong says it can hand over (state 2, its pid).
    async fn fake_daemon(dir: &std::path::Path, version: &'static str, protocol: u32, pid: u32, reloads: bool) -> skiff_client::Client {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
        let socket = dir.join(format!("{version}.sock"));
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (rd, mut wr) = stream.into_split();
            let mut lines = tokio::io::BufReader::new(rd).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let req: serde_json::Value = serde_json::from_str(&line).unwrap();
                let id = req["id"].clone();
                let reply = match req["cmd"].as_str() {
                    Some("ping") if reloads => serde_json::json!({"id": id, "type": "pong", "version": version, "protocol": protocol, "reload_state": 2, "pid": pid}),
                    Some("ping") => serde_json::json!({"id": id, "type": "pong", "version": version, "protocol": protocol}),
                    Some("list_sessions") => serde_json::json!({"id": id, "type": "sessions", "sessions": []}),
                    other => serde_json::json!({"id": id, "type": "error", "message": format!("{other:?}")}),
                };
                let mut out = serde_json::to_vec(&reply).unwrap();
                out.push(b'\n');
                wr.write_all(&out).await.unwrap();
            }
        });
        let mut c = skiff_client::Client::connect(&socket).await.unwrap();
        c.daemon_pid = Some(pid as i32);
        c
    }

    /// Never a downgrade: a newer daemon on another protocol with no live
    /// session keeps running, and the app does not replace it. An older one
    /// in the same place is stopped and replaced, which shows the stop works.
    #[test]
    fn a_newer_daemon_is_never_replaced() {
        let dir = std::env::temp_dir().join(format!("skiff-app-newer-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let other = skiff_core::PROTOCOL + 1;
        for (version, replaced) in [("99.0.0", false), ("0.0.1", true)] {
            let mut stand_in = std::process::Command::new("sleep").arg("30").spawn().unwrap();
            let app = super::App::default();
            let check = rt.block_on(async {
                let c = fake_daemon(&dir, version, other, stand_in.id(), false).await;
                super::check_daemon(&app, &c, true).await
            });
            // The stop reaps what it ends, so a gone child reads as ECHILD.
            let running = stand_in.try_wait().is_ok_and(|s| s.is_none());
            let _ = stand_in.kill();
            let _ = stand_in.wait();
            assert_eq!(matches!(check, super::Check::Replaced), replaced, "skiffd {version}");
            assert_eq!(running, !replaced, "skiffd {version}: signalled");
            assert_eq!(rt.block_on(app.replaced.lock()).is_some(), replaced);
            if !replaced {
                assert_eq!(rt.block_on(app.warning.lock()).as_ref().map(|w| w.kind), Some("protocol"));
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Residual P1, failure injection: the first identity's store fails (a
    /// directory sits where the new hosts.toml is written). The connection is
    /// refused and the selection stays unpinned, on a retry too; once the
    /// store works, the retry stores the pin before it connects. A Host that
    /// then answers as B is refused here and by a selection rebuilt from the
    /// file. A fake `ssh` only (a Python Unix-socket server), never OpenSSH.
    #[test]
    fn a_first_identity_connects_only_once_it_is_stored_even_on_a_retry() {
        use super::*;
        if !std::path::Path::new("/usr/bin/python3").is_file() {
            eprintln!("skipped: no /usr/bin/python3");
            return;
        }
        // Short: the forward's socket path must fit in sun_path.
        let dir = PathBuf::from("/tmp").join(format!("sd-store-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let id_file = dir.join("remote-id");
        let script = format!(
            "#!/usr/bin/python3\n\
             import socket,sys,json,threading\n\
             spec=sys.argv[sys.argv.index('-L')+1]\n\
             s=socket.socket(socket.AF_UNIX); s.bind(spec.split(':')[0]); s.listen()\n\
             def serve(c):\n\
             \x20with c, c.makefile('rb') as f:\n\
             \x20 for line in f:\n\
             \x20  req=json.loads(line)\n\
             \x20  if req.get('cmd')=='ping':\n\
             \x20   hid=open('{id}').read()\n\
             \x20   data={{'id':req['id'],'type':'pong','version':'{v}','protocol':{p},'identity':{{'host_id':hid,'instance_id':'run','os':'fixture','account':'fixture'}}}}\n\
             \x20   c.sendall((json.dumps(data)+'\\n').encode())\n\
             while True:\n\
             \x20c,_=s.accept();threading.Thread(target=serve,args=(c,),daemon=True).start()\n",
            id = id_file.display(),
            v = env!("CARGO_PKG_VERSION"),
            p = skiff_core::PROTOCOL,
        );
        let ssh = dir.join("fake-ssh");
        std::fs::write(&ssh, script).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(&id_file, "A").unwrap();
        let file = dir.join("hosts.toml");
        std::fs::write(&file, "selected = \"h_fixture\"\n[[host]]\nid = \"h_fixture\"\nname = \"fixture\"\ndestination = \"fixture\"\nsocket = \"/fixture.sock\"\n").unwrap();
        let target = skiff_client::host::ssh::SshTarget::new("fixture", "/fixture.sock");
        let options = skiff_client::host::ssh::SshOptions { program: ssh, config: None };
        let selection = |pin: Option<&str>| {
            let pin = pin.map(|p| skiff_host_api::HostId::try_from(p.to_string()).unwrap());
            HostSelection::ssh(target.clone(), options.clone(), dir.join("fwd"), pin).unwrap()
        };
        let disk_pin = || crate::hosts::load(&file).unwrap().hosts[0].host_id.clone();
        let changed = |r: &anyhow::Result<Arc<SelectedHost>>| matches!(r.as_ref().err().and_then(|e| e.downcast_ref()), Some(skiff_host_api::Error::IdentityChanged { .. }));
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let app = App::default();
            let first = selection(None);
            // Only the store fails: the new file cannot be written beside.
            let obstruction = file.with_extension(format!("toml.{}.tmp", std::process::id()));
            std::fs::create_dir(&obstruction).unwrap();
            for attempt in 0..2 {
                let r = try_remote(&app, &first, "fixture", None, file.clone()).await;
                assert!(r.is_err(), "attempt {attempt} connected with no stored pin");
                assert_eq!(first.pinned(), None, "attempt {attempt} pinned an unstored identity");
                assert_eq!(disk_pin(), None);
            }
            std::fs::remove_dir(&obstruction).unwrap();
            let retry = try_remote(&app, &first, "fixture", None, file.clone()).await;
            assert!(retry.is_ok(), "{:?}", retry.err());
            assert_eq!(disk_pin().as_deref(), Some("A"), "the retry connected without storing its pin");
            assert_eq!(first.pinned().map(|p| p.as_str().to_string()).as_deref(), Some("A"));
            drop(retry);
            // Another Host answers there now.
            std::fs::write(&id_file, "B").unwrap();
            assert!(changed(&try_remote(&app, &first, "fixture", None, file.clone()).await), "the same selection accepted B");
            drop(first);
            let rebuilt = selection(disk_pin().as_deref());
            assert!(changed(&try_remote(&app, &rebuilt, "fixture", None, file.clone()).await), "a rebuilt selection accepted B");
            assert_eq!(disk_pin().as_deref(), Some("A"));
        });
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// M4: this Mac's data plane is started for the selected socket when none
    /// answers, paired with the session plane's Host id, and refused when it
    /// names another Host. Private config only; nothing live is read.
    #[test]
    fn the_data_plane_starts_here_and_pairs_with_the_session_host() {
        use super::*;
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        let _env = crate::reload::tests::ENV.lock().unwrap_or_else(|e| e.into_inner());
        let dir = PathBuf::from("/tmp").join(format!("sd-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir(&dir).unwrap();
        let old = (std::env::var_os("SKIFF_HOST_BIN"), std::env::var_os("SKIFF_CONFIG"));
        let launcher = dir.join("skiff-host");
        std::fs::write(&launcher, format!("#!/bin/sh\necho \"$@\" >> {}/started\n", dir.display())).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::env::set_var("SKIFF_HOST_BIN", &launcher);
        std::env::set_var("SKIFF_CONFIG", dir.join("projects.toml"));
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let session = dir.join("s.sock");
            let listener = tokio::net::UnixListener::bind(&session).unwrap();
            // A skiffd that reports Host A on every connection.
            tokio::spawn(async move {
                loop {
                    let (stream, _) = listener.accept().await.unwrap();
                    tokio::spawn(async move {
                        let mut stream = BufReader::new(stream);
                        let mut line = String::new();
                        while stream.read_line(&mut line).await.unwrap_or(0) > 0 {
                            let id = serde_json::from_str::<serde_json::Value>(&line).unwrap()["id"].clone();
                            let reply = serde_json::json!({"id":id,"type":"pong","version":skiff_core::VERSION,"protocol":skiff_core::PROTOCOL,
                                "identity":{"host_id":"A","instance_id":"run","os":"test","account":"uid:1"}});
                            let mut bytes = serde_json::to_vec(&reply).unwrap(); bytes.push(b'\n');
                            stream.get_mut().write_all(&bytes).await.unwrap();
                            line.clear();
                        }
                    });
                }
            });
            let app = App::default();
            app.set_selection(HostSelection::new(session.clone(), dir.join("bootstrap-id")));
            // The launcher's build is the bundled service's: the service is current.
            let version = format!("{}+{}", skiff_core::VERSION, skiff_client::host::data::build_of(&launcher).unwrap());
            let host = |id: &str| {
                let mut h = skiff_host::hello().unwrap();
                h.host.host_id = skiff_host_api::HostId::try_from(id.to_string()).unwrap();
                h.service_versions.insert("skiff-host".into(), version.clone());
                h
            };
            // The "started" service binds once the launcher has run.
            let data_socket = skiff_core::socket::data_socket(&session);
            let (marks, socket, hello_a) = (dir.join("started"), data_socket.clone(), host("A"));
            let service = tokio::spawn(async move {
                while !marks.exists() {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                let listener = tokio::net::UnixListener::bind(&socket).unwrap();
                let _ = skiff_host::serve(listener, Arc::new(hello_a)).await;
            });
            let c = data_now(&app).await.unwrap();
            assert_eq!(c.hello().host.host_id.as_str(), "A");
            let started = std::fs::read_to_string(dir.join("started")).unwrap();
            assert_eq!(started.trim(), format!("--session-socket {} --this-mac", session.display()));
            assert!(skiff_client::host::data::project_list(&c).await.unwrap().is_empty());
            // Reused while open: no second start.
            assert!(Arc::ptr_eq(&c, &data_now(&app).await.unwrap()));
            assert_eq!(std::fs::read_to_string(dir.join("started")).unwrap().lines().count(), 1);

            // The page's check: ready, with the data plane's own families.
            let first = data_status(&app).await;
            assert!(first.ready && !first.waiting && first.host_id.as_deref() == Some("A"), "{first:?}");
            assert!(first.capabilities.contains(&"git_diff@1".to_string()), "{first:?}");
            // A reply that comes on a connection replaced meanwhile is never shown.
            let app = &app;
            let stale = data_read(app, |c| async move {
                *app.data.lock().await = None;
                skiff_client::host::data::project_list(&c).await
            })
            .await;
            assert_eq!(stale, Err(DATA_REPLACED.to_string()));
            let again = data_status(app).await;
            assert!(again.ready && again.connection > first.connection && again.instance_id == first.instance_id, "{again:?}");

            // The service restarts: the next check finds the new instance on
            // a new connection, with no page reload and nothing else redone.
            service.abort();
            let _ = service.await;
            let _ = std::fs::remove_file(&data_socket);
            let listener = tokio::net::UnixListener::bind(&data_socket).unwrap();
            let restarted = tokio::spawn(skiff_host::serve(listener, Arc::new(host("A"))));
            let old_c = app.data.lock().await.as_ref().unwrap().connection();
            for _ in 0..100 {
                if old_c.is_closed() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            let back = data_status(app).await;
            assert!(back.ready && back.connection > again.connection && back.instance_id != again.instance_id, "{back:?}");
            assert_eq!(std::fs::read_to_string(dir.join("started")).unwrap().lines().count(), 1, "a running service was started again");
            let service = restarted;

            // Another Host's service at this socket: refused, nothing sent.
            service.abort();
            let _ = service.await;
            *app.data.lock().await = None;
            let _ = std::fs::remove_file(&data_socket);
            let listener = tokio::net::UnixListener::bind(&data_socket).unwrap();
            let other = tokio::spawn(skiff_host::serve(listener, Arc::new(host("B"))));
            let e = data_now(app).await.err().unwrap();
            assert!(e.contains("answers as another Skiff Host"), "{e}");
            other.abort();
        });
        for (key, value) in [("SKIFF_HOST_BIN", old.0), ("SKIFF_CONFIG", old.1)] {
            match value {
                Some(v) => std::env::set_var(key, v),
                None => std::env::remove_var(key),
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Version order decides, whatever the build. An older daemon that can
    /// hand over is planned for a reload onto the bundled skiffd, with no
    /// warning. A newer one is left alone, with the "newer" notice. The
    /// same version is used as it is. Nothing is signalled in any case.
    #[test]
    fn an_older_daemon_reloads_and_a_newer_one_is_left_alone() {
        let _env = crate::reload::tests::ENV.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("skiff-app-order-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // The bundled skiffd: it answers --reload-info as this build's version.
        let bin = dir.join("skiffd");
        std::fs::write(&bin, format!("#!/bin/sh\necho '{{\"version\":\"{}\",\"handover\":[2],\"fingerprint\":\"x\"}}'\n", skiff_core::VERSION)).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        let old_daemon = std::env::var_os("SKIFF_DAEMON");
        std::env::set_var("SKIFF_DAEMON", &bin);
        let rt = tokio::runtime::Runtime::new().unwrap();
        let mut outcome = Vec::new();
        for (version, want) in [("0.0.1", "reload"), ("99.0.0", "newer"), (skiff_core::VERSION, "same")] {
            let mut stand_in = std::process::Command::new("sleep").arg("30").spawn().unwrap();
            let app = super::App::default();
            let check = rt.block_on(async {
                let c = fake_daemon(&dir, version, skiff_core::PROTOCOL, stand_in.id(), true).await;
                super::check_daemon(&app, &c, true).await
            });
            let running = stand_in.try_wait().is_ok_and(|s| s.is_none());
            let _ = stand_in.kill();
            let _ = stand_in.wait();
            let planned = rt.block_on(app.reload.lock()).is_some();
            let warning = rt.block_on(app.warning.lock()).as_ref().map(|w| w.kind);
            outcome.push((want, matches!(check, super::Check::Use), running, planned, warning));
        }
        match old_daemon {
            Some(v) => std::env::set_var("SKIFF_DAEMON", v),
            None => std::env::remove_var("SKIFF_DAEMON"),
        }
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(outcome, [("reload", true, true, true, None), ("newer", true, true, false, Some("newer")), ("same", true, true, false, None)]);
    }

    /// #182: the bundled skiffd cannot read the daemon's handover. No reload
    /// is planned, and the warning says why and what a restart does, in the
    /// words `skiff status` uses.
    #[test]
    fn a_blocked_handover_says_why() {
        let _env = crate::reload::tests::ENV.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("skiff-app-blocked-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let bin = dir.join("skiffd");
        std::fs::write(&bin, format!("#!/bin/sh\necho '{{\"version\":\"{}\",\"handover\":[3],\"fingerprint\":\"x\"}}'\n", skiff_core::VERSION)).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        let old_daemon = std::env::var_os("SKIFF_DAEMON");
        std::env::set_var("SKIFF_DAEMON", &bin);
        let rt = tokio::runtime::Runtime::new().unwrap();
        let app = super::App::default();
        rt.block_on(async {
            let c = fake_daemon(&dir, "0.0.1", skiff_core::PROTOCOL, std::process::id(), true).await;
            super::check_daemon(&app, &c, false).await
        });
        match old_daemon {
            Some(v) => std::env::set_var("SKIFF_DAEMON", v),
            None => std::env::remove_var("SKIFF_DAEMON"),
        }
        let _ = std::fs::remove_dir_all(&dir);
        assert!(rt.block_on(app.reload.lock()).is_none());
        let w = rt.block_on(app.warning.lock()).clone().unwrap();
        assert_eq!(w.kind, "outdated");
        assert!(w.message.contains("handover 2") && w.message.contains("Restart skiffd stops them"), "{}", w.message);
    }
}
