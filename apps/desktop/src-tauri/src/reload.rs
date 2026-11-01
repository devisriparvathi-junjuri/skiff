//! Moves an older skiffd onto the one this app ships, with its sessions.
//!
//! skiffd's own reload keeps its pid, the PTYs and the terminals. This side
//! holds terminal input meanwhile and proves the result on a new connection
//! before the held input goes on: the same daemon pid, the bundled version,
//! and each session with the same id and child pid. Nothing here stops a
//! session or starts a daemon: a failure is the user's to resolve.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{atomic::Ordering, Arc},
    time::{Duration, Instant},
};
use serde::Serialize;
use skiff_client::{host::local::ProcessControl, update::{self, Handover}, DaemonInfo, Refused};
#[cfg(test)]
use skiff_client::Client;
use skiff_core::session::{SessionInfo, SessionState};
use tauri::State;
use crate::connection::{check_daemon, compare_versions, daemon_binary, restart_warning, status_now, App, DaemonStatus, Warning, HELLO_TIMEOUT};

/// The new image adopts the sessions and answers within this.
const RECONNECT: Duration = Duration::from_secs(15);
/// While the daemon's pid lives, the app waits this long for it to answer.
/// Many sessions or a loaded machine make the adoption slow, and a restart
/// would stop the sessions it adopts.
const ADOPT_LIMIT: Duration = Duration::from_secs(120);
/// skiffd answers the reload request, or execs, within this. Its own steps
/// are bounded, but it first waits for changes in flight.
const RELOAD_WAIT: Duration = Duration::from_secs(120);

/// A reload the app can start: the daemon supports it, and the bundled
/// skiffd reads its handover.
pub(crate) struct Plan {
    binary: PathBuf,
    /// What `binary` reports. The daemon must answer with it after the reload.
    version: String,
    /// The running daemon's version and protocol.
    from: String,
    protocol: u32,
    pid: u32,
}

/// Why there is no reload.
pub(crate) enum NoPlan {
    /// The bundled skiffd reports this version, no newer than the daemon. A
    /// restart starts it again, so the restart does not finish the update.
    /// Only a dev build gets here: `tauri dev` rebuilds the app, not skiffd.
    Stale(String),
    Other(String),
}

impl From<String> for NoPlan {
    fn from(why: String) -> Self {
        NoPlan::Other(why)
    }
}

/// A reload onto the bundled skiffd, or why there is none. The bundled
/// skiffd must be newer than the daemon: the same version stays. The
/// verdict is [`update::handover`], the one `skiff status` uses. It runs
/// the bundled binary, so only a local endpoint may: `_local` proves it.
pub(crate) async fn plan(_local: &ProcessControl, info: &DaemonInfo) -> Result<Plan, NoPlan> {
    plan_with(info, &daemon_binary()).await
}

async fn plan_with(info: &DaemonInfo, binary: &Path) -> Result<Plan, NoPlan> {
    let binary = resolve(binary).ok_or_else(|| NoPlan::Other("the bundled skiffd is missing".into()))?;
    let target = update::reload_info(&binary).await?;
    match update::handover(info, &target) {
        Handover::NotNewer => Err(NoPlan::Stale(target.version)),
        Handover::Blocked(why) => Err(NoPlan::Other(why)),
        Handover::Ready => Ok(Plan {
            binary,
            version: target.version,
            from: info.version.clone(),
            protocol: info.protocol,
            pid: info.pid.unwrap_or_default(),
        }),
    }
}

/// An absolute path for `p`, from `PATH` for a bare name. A reload needs one.
fn resolve(p: &Path) -> Option<PathBuf> {
    if p.is_absolute() {
        return p.is_file().then(|| p.to_path_buf());
    }
    if p.components().count() > 1 {
        return std::path::absolute(p).ok().filter(|p| p.is_file());
    }
    std::env::split_paths(&std::env::var_os("PATH")?).map(|d| d.join(p)).find(|p| p.is_file())
}

#[derive(Serialize)]
pub(crate) struct Reloaded {
    status: DaemonStatus,
    /// Why the reload failed or is not proven. `None`: it worked.
    error: Option<String>,
    /// The app now talks to another connection: the page subscribes again.
    reconnected: bool,
    /// Names of live sessions that did not come through with the same child.
    missing: Vec<String>,
    /// Names of sessions whose input, typed during the reload, was dropped.
    lost: Vec<String>,
}

/// Reloads the daemon `daemon_status` planned for. Without a plan, it only
/// reports the status.
#[tauri::command]
pub(crate) async fn reload_daemon(app: State<'_, App>) -> Result<Reloaded, String> {
    Ok(reload_planned(&app).await)
}

/// Update skiffd, from the page: moves this Mac's older skiffd onto the
/// bundled one now, sessions kept, whatever the launch did. The daemon must
/// be older than this app's skiffd and able to hand over, else nothing
/// changes and the answer says why.
#[tauri::command]
pub(crate) async fn update_daemon(app: State<'_, App>) -> Result<Reloaded, String> {
    let Some(control) = app.selection().process_control() else {
        return Err("Skiff updates only this computer's skiffd. Update a Host in Settings › Hosts.".into());
    };
    let (c, _) = crate::connection::ensure_client(&app).await?;
    let info = tokio::time::timeout(HELLO_TIMEOUT, c.daemon_info()).await.map_err(|_| "skiffd did not answer.".to_string())?.map_err(crate::connection::err)?;
    if !compare_versions(&info.version, skiff_core::VERSION).is_lt() {
        return Err(format!("skiffd {} is not older than this Skiff. Nothing to update.", info.version));
    }
    match plan(&control, &info).await {
        Ok(p) => *app.reload.lock().await = Some(p),
        Err(NoPlan::Stale(bundled)) => return Err(format!("The skiffd this Skiff ships ({bundled}) is not newer than the one that runs ({}). Nothing changed.", info.version)),
        Err(NoPlan::Other(why)) => return Err(format!("skiffd {} cannot move onto this Skiff's with its sessions: {why}. Nothing changed.", info.version)),
    }
    let out = reload_planned(&app).await;
    match &out.error {
        Some(e) => Err(format!("The update did not finish: {e}")),
        None => Ok(out),
    }
}

async fn reload_planned(app: &App) -> Reloaded {
    let plan = app.reload.lock().await.take();
    let mut out = Outcome::default();
    if let Some(plan) = plan {
        app.reload_tried.store(true, Ordering::Relaxed);
        out = run(app, &plan).await;
    }
    Reloaded {
        status: status_now(app).await,
        error: out.error,
        reconnected: out.reconnected,
        missing: out.missing,
        lost: out.lost,
    }
}

#[derive(Default)]
struct Outcome {
    error: Option<String>,
    reconnected: bool,
    missing: Vec<String>,
    lost: Vec<String>,
}

/// Each session by id: its child pid, and whether it still runs.
type Listed = HashMap<String, (Option<u32>, bool)>;

fn listed(list: &[SessionInfo]) -> Listed {
    list.iter().map(|s| (s.id.clone(), (s.pid, s.state != SessionState::Done))).collect()
}

/// Keeps the pids of registered watchers: the reload drops the registrations.
pub(crate) fn note_watchers(app: &App, list: &[SessionInfo]) {
    let mut kept = app.watch_pids.lock().unwrap();
    for pid in list.iter().filter_map(|s| s.watch.as_ref()).flat_map(|w| w.pids.iter()) {
        if !kept.contains(pid) {
            kept.push(*pid);
        }
    }
}

/// What the user calls each session, for the messages. The id when unknown.
fn naming(list: &[SessionInfo]) -> impl Fn(Vec<String>) -> Vec<String> {
    let names: HashMap<String, String> = list
        .iter()
        .map(|s| (s.id.clone(), s.name.clone().or(s.title.clone()).unwrap_or(s.label.clone())))
        .collect();
    move |ids| ids.into_iter().map(|id| names.get(&id).cloned().unwrap_or(id)).collect()
}

/// The same live session, with the same child, before and after. A session
/// id alone does not prove it: a restore reuses the id for a new shell.
fn same(before: &Listed, after: &Listed, id: &str) -> bool {
    match (before.get(id), after.get(id)) {
        (Some((Some(a), true)), Some((Some(b), true))) => a == b,
        _ => false,
    }
}

async fn run(app: &App, plan: &Plan) -> Outcome {
    // No other call connects, or starts a daemon, until the reload ends.
    let mut guard = app.host.lock().await;
    // A reload that ends before the request keeps the restart of #27.
    let keep = |why: &str, live: Option<usize>| {
        *app.no_reload.lock().unwrap() = Some(why.to_string());
        restart_warning(&plan.from, plan.protocol, live, Some(why))
    };
    // Reload is endpoint-local lifecycle: only a local endpoint may run it.
    if guard.as_ref().is_some_and(|host| host.process_control().is_none()) {
        return Outcome { error: Some("skiffd on this Host is not managed by this app".into()), ..Default::default() };
    }
    let Some(old_host) = guard.as_ref().cloned().filter(|host| !host.connection().is_closed()) else {
        let why = "skiffd closed the connection before the reload";
        *app.warning.lock().await = Some(keep(why, None));
        return Outcome { error: Some(why.into()), ..Default::default() };
    };
    let old = old_host.client();
    let old_terminal = old_host.connection();
    // Input from here on waits. Input sent before is on this connection,
    // ahead of the reload request, and skiffd drains it into the PTYs.
    app.input.close_gate();
    let (before, name) = match old.list_sessions().await {
        Ok(l) => {
            note_watchers(app, &l);
            (listed(&l), naming(&l))
        }
        Err(e) => {
            let lost = app.input.open_gate(Some(&old_terminal), |_| true);
            let why = format!("skiffd did not list its sessions: {e:#}");
            *app.warning.lock().await = Some(keep(&why, None));
            return Outcome { error: Some(why), lost, ..Default::default() };
        }
    };
    let live = before.values().filter(|(_, running)| *running).count();

    let Ok(sent) = tokio::time::timeout(RELOAD_WAIT, old.reload(Some(plan.binary.clone()))).await else {
        // Mid-reload, or stuck. A new connection would reach the old image,
        // which may still exec and close it. The user decides.
        let lost = name(app.input.open_gate(None, |_| false));
        let message = format!("skiffd {} did not finish the reload within {}s", plan.from, RELOAD_WAIT.as_secs());
        *app.warning.lock().await = Some(Warning {
            kind: "hung",
            message: format!("{message}. Restarting stops it and its sessions. Panes come back as shells in their folders."),
            sessions: Some(live),
        });
        return Outcome { error: Some(message), lost, ..Default::default() };
    };
    match sent {
        Err(e) if e.downcast_ref::<Refused>().is_some() => {
            // skiffd answered: it did not exec and runs as before. The held
            // input goes to it, for the sessions that still run.
            let why = format!("{e:#}");
            let now = old.list_sessions().await.map(|l| listed(&l)).unwrap_or_default();
            let lost = name(app.input.open_gate(Some(&old_terminal), |id| same(&before, &now, id)));
            *app.warning.lock().await = Some(keep(&why, Some(live)));
            return Outcome { error: Some(format!("skiffd {} did not reload: {why}", plan.from)), lost, ..Default::default() };
        }
        // The connection closed, as exec closes it, or the outcome is open.
        // Only a new connection tells.
        _ => {}
    }
    *guard = None;
    *app.live.lock().unwrap() = None;
    drop(old);
    drop(old_terminal);
    drop(old_host);

    let (c, info) = match reconnect(app, plan.pid).await {
        Ok(found) => found,
        Err(why) => {
            let lost = name(app.input.open_gate(None, |_| false));
            // The page keeps asking: a late answer still brings the sessions.
            let (message, restart) = match alive(plan.pid) {
                true => (format!("{why}. It still runs and may answer later."), "Restarting stops it and its sessions."),
                false => (format!("{why}. Its sessions may have ended."), "Restarting starts a new skiffd."),
            };
            *app.failed.lock().unwrap() = Some(message.clone());
            *app.warning.lock().await = Some(Warning {
                kind: "failed",
                message: format!("{message} {restart} Panes come back as shells in their folders."),
                sessions: None,
            });
            return Outcome { error: Some(message), reconnected: true, lost, ..Default::default() };
        }
    };
    let host = match c.activate().await {
        Ok(host) => Arc::new(host),
        Err(e) => {
            let why = format!("local Host bootstrap after reload: {e:#}");
            *app.failed.lock().unwrap() = Some(why.clone());
            let lost = name(app.input.open_gate(None, |_| false));
            return Outcome { error: Some(why), reconnected: true, lost, ..Default::default() };
        }
    };
    let c = host.client();
    // Streams of the old connection are gone. The page subscribes again.
    for (_, task) in app.subs.lock().await.drain() {
        task.abort();
    }
    app.flows.lock().unwrap().clear();
    if let Some(task) = app.events.lock().await.take() {
        task.abort();
    }
    let terminal = host.connection();
    *guard = Some(host);
    *app.live.lock().unwrap() = Some(terminal.clone());

    let mut error = None;
    let mut proven = true;
    if info.pid != Some(plan.pid) {
        proven = false;
        let now = info.pid.map_or("unknown".into(), |p| p.to_string());
        error = Some(format!("skiffd restarted instead of reloading (pid {}, now {now}). Its sessions did not survive.", plan.pid));
    } else if info.version != plan.version {
        proven = false;
        error = Some(format!("skiffd answered as {} after the reload, not {}", info.version, plan.version));
    } else {
        *app.replaced.lock().await = Some(plan.from.clone());
    }
    let after = match tokio::time::timeout(HELLO_TIMEOUT, c.list_sessions()).await {
        Ok(Ok(l)) => listed(&l),
        _ => {
            proven = false;
            error.get_or_insert_with(|| "skiffd did not list its sessions after the reload".into());
            Listed::new()
        }
    };
    // Gone, or another child under the same id. One that exited is not missing.
    let mut missing: Vec<String> = match proven {
        true => before
            .iter()
            .filter(|(id, (pid, running))| *running && after.get(*id).map(|a| a.0) != Some(*pid))
            .map(|(id, _)| id.clone())
            .collect(),
        false => Vec::new(),
    };
    missing.sort();
    let lost = name(app.input.open_gate(Some(&terminal), |id| proven && same(&before, &after, id)));
    // Sets the warning for the daemon as it is now. No second reload.
    check_daemon(app, &c, false).await;
    if error.is_none() && !missing.is_empty() {
        error = Some(format!("{} of {live} live sessions did not come through the reload", missing.len()));
    }
    Outcome { error, reconnected: true, missing: name(missing), lost }
}

/// Connects again and asks the daemon what it is. Never starts a daemon.
/// Waits up to [`RECONNECT`], and up to [`ADOPT_LIMIT`] while `pid` lives.
async fn reconnect(app: &App, pid: u32) -> Result<(skiff_client::host::local::PendingConnection, DaemonInfo), String> {
    let start = Instant::now();
    loop {
        let last = match app.selection().connect().await {
            // Activation takes the Host's identity from this pong.
            Ok(mut c) => match tokio::time::timeout(HELLO_TIMEOUT, c.daemon_info()).await {
                Ok(Ok(info)) => return Ok((c, info)),
                Ok(Err(e)) => format!("{e:#}"),
                Err(_) => "no answer".into(),
            },
            Err(e) => format!("{e:#}"),
        };
        let waited = start.elapsed();
        if waited >= ADOPT_LIMIT || (waited >= RECONNECT && !alive(pid)) {
            return Err(format!("skiffd did not come back after the reload ({last})"));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// True while `pid` runs. Reaps it first when this app started it: a zombie
/// still answers kill(0).
fn alive(pid: u32) -> bool {
    let pid = pid as i32;
    // SAFETY: kill and waitpid take plain integers and touch no memory of ours.
    unsafe {
        libc::waitpid(pid, std::ptr::null_mut(), libc::WNOHANG) != pid
            && (libc::kill(pid, 0) == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM))
    }
}

#[cfg(all(test, unix))]
pub(crate) mod tests {
    use super::*;
    use crate::connection::ensure_client;
    use skiff_client::skipper;
    use skiff_core::session::SessionSpec;
    use std::process::Stdio;

    /// The tests that set `SKIFF_SOCKET` or `SKIFF_DAEMON` run one at a time.
    pub(crate) static ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// A bundled skiffd no newer than the daemon is stale, before the
    /// handover check: a restart onto it would ask again.
    #[test]
    fn an_older_bundled_skiffd_is_stale() {
        let dir = std::env::temp_dir().join(format!("skiff-app-stale-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let bin = dir.join("skiffd");
        std::fs::write(&bin, "#!/bin/sh\necho '{\"version\":\"0.6.19\",\"handover\":[]}'\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        let info = |version: &str| DaemonInfo { version: version.into(), protocol: skiff_core::PROTOCOL, reload_state: Some(2), pid: Some(1), debug: Some(true), identity: None };
        let rt = tokio::runtime::Runtime::new().unwrap();
        // The same version is stale too: whatever its build, it stays.
        for running in ["0.6.19", "0.6.20"] {
            match rt.block_on(plan_with(&info(running), &bin)) {
                Err(NoPlan::Stale(v)) => assert_eq!(v, "0.6.19"),
                _ => panic!("skiffd {running}: not stale"),
            }
        }
        // Newer, but it cannot read handover 2.
        assert!(matches!(rt.block_on(plan_with(&info("0.6.18"), &bin)), Err(NoPlan::Other(_))));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A private daemon reloads onto its own binary while input arrives.
    /// Each line reaches the shell once, in order, and the pid stays.
    #[test]
    fn held_input_survives_a_reload() {
        let _env = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let bin = std::env::var_os("SKIFF_TEST_DAEMON")
            .map(PathBuf::from)
            .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../target/debug/skiffd"));
        assert!(bin.is_file(), "build skiffd first with cargo build -p skiffd; set SKIFF_TEST_DAEMON for a custom target directory: {}", bin.display());
        let dir = std::env::temp_dir().join(format!("skiff-app-reload-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("skiffd.sock");
        let old_socket = std::env::var_os("SKIFF_SOCKET");
        let old_daemon = std::env::var_os("SKIFF_DAEMON");
        std::env::set_var("SKIFF_SOCKET", &socket);
        std::env::set_var("SKIFF_DAEMON", &bin);
        let mut daemon = std::process::Command::new(&bin)
            .env("SKIFF_SOCKET", &socket)
            .env("SKIFF_STATE", dir.join("workspace.json"))
            .env("SKIFF_CONFIG", dir.join("projects.toml"))
            .env("SKIFF_LEDGER", dir.join("ledger.db"))
            .env("SKIFF_SHELL_ENV", "0")
            .env("SHELL", "/bin/sh")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(dir.join("daemon.log")).unwrap())
            .spawn()
            .unwrap();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| rt.block_on(async {
            wait_for(&socket).await;
            let app = App::default();
            let (c, _) = ensure_client(&app).await.unwrap();
            let pid = c.daemon_info().await.unwrap().pid.unwrap();
            // The Hello is skiffd's own, from the Ping the connect already sent.
            let first = app.host.lock().await.as_ref().unwrap().clone();
            assert_eq!(first.identity_source(), skiff_client::host::local::IdentitySource::Host);
            let hello = first.connection().hello().clone();
            drop(first);
            let s = c
                .create_session(SessionSpec {
                    label: "t".into(),
                    command: Some("/bin/sh".into()),
                    args: vec![],
                    cwd: Some(dir.clone()),
                    cols: 80,
                    rows: 24,
                    ..Default::default()
                })
                .await
                .unwrap();
            let app = Arc::new(app);
            let typing = |from: usize| {
                let app = app.clone();
                let id = s.id.clone();
                tokio::spawn(async move {
                    for i in from..from + 20 {
                        let live = app.live.lock().unwrap().clone();
                        let _ = app.input.write(live.as_deref(), &id, format!("echo L{i:02} >> out\n").into_bytes());
                        tokio::time::sleep(Duration::from_millis(5)).await;
                    }
                })
            };
            // A target that cannot adopt: the old daemon answers and carries on.
            let refused = Plan { binary: "/bin/false".into(), version: "x".into(), from: "old".into(), protocol: skiff_core::PROTOCOL, pid };
            let t = typing(0);
            let out = run(&app, &refused).await;
            t.await.unwrap();
            assert!(out.error.is_some_and(|e| e.contains("did not reload")) && !out.reconnected);
            assert!(out.lost.is_empty(), "lost {:?}", out.lost);
            // The restart warning of #27 stays, with the reason.
            assert_eq!(app.warning.lock().await.as_ref().map(|w| w.kind), Some("outdated"));

            let plan = Plan { binary: bin.clone(), version: skiff_core::VERSION.into(), from: "old".into(), protocol: skiff_core::PROTOCOL, pid };
            let t = typing(20);
            let out = run(&app, &plan).await;
            t.await.unwrap();
            assert_eq!(out.error, None);
            assert!(out.reconnected);
            assert!(out.lost.is_empty(), "lost {:?}", out.lost);
            // Input typed after the reload goes straight on.
            let host = app.host.lock().await.as_ref().unwrap().clone();
            let c = host.client();
            assert_eq!(c.daemon_info().await.unwrap().pid, Some(pid));
            // Same pid and Host, a new run: the page attaches again for snapshots.
            let after = host.connection().hello().clone();
            assert_eq!(after.host.host_id, hello.host.host_id);
            assert_ne!(after.instance_id, hello.instance_id);
            assert_eq!(host.connection().continuity(), skiff_client::host::Continuity::NewInstance);
            drop(host);
            let live = app.live.lock().unwrap().clone().unwrap();
            let _ = app.input.write(Some(&live), &s.id, b"echo DONE >> out\n".to_vec());
            let file = dir.join("out");
            let until = Instant::now() + Duration::from_secs(10);
            let text = loop {
                let text = std::fs::read_to_string(&file).unwrap_or_default();
                if text.ends_with("DONE\n") || Instant::now() > until {
                    break text;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            };
            let want: String = (0..40).map(|i| format!("L{i:02}\n")).chain(["DONE\n".to_string()]).collect();
            assert_eq!(text, want);
        })));
        // Shutdown uses only this test's socket, including after an assertion
        // fails. Never signal a daemon or fall back to the user's socket.
        let shutdown = rt.block_on(async {
            tokio::time::timeout(Duration::from_secs(5), async {
                let c = Client::connect(&socket).await?;
                c.shutdown().await
            }).await
        });
        let until = Instant::now() + Duration::from_secs(5);
        let stopped = loop {
            match daemon.try_wait() {
                Ok(Some(_)) => break true,
                Err(_) => break false,
                Ok(None) if Instant::now() >= until => break false,
                Ok(None) => std::thread::sleep(Duration::from_millis(25)),
            }
        };
        for (key, old) in [("SKIFF_SOCKET", old_socket), ("SKIFF_DAEMON", old_daemon)] {
            match old {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
        if result.is_err() || !stopped {
            eprintln!("{}", std::fs::read_to_string(dir.join("daemon.log")).unwrap_or_default());
        }
        if stopped {
            let _ = std::fs::remove_dir_all(&dir);
        }
        result.unwrap();
        assert!(stopped, "private test daemon did not stop; shutdown: {shutdown:?}; files: {}", dir.display());
    }

    /// Waits until the fixture daemon listens, so `ensure_client` never starts
    /// a second daemon of its own.
    async fn wait_for(socket: &Path) {
        let until = Instant::now() + Duration::from_secs(10);
        while Client::connect(socket).await.is_err() {
            assert!(Instant::now() < until, "the test daemon did not listen on {}", socket.display());
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    fn built(name: &str, var: &str) -> PathBuf {
        let bin = std::env::var_os(var)
            .map(PathBuf::from)
            .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../target/debug").join(name));
        assert!(bin.is_file(), "build {name} first with cargo build -p skiffd -p skiff-cli; set {var} for a custom target directory: {}", bin.display());
        bin.canonicalize().unwrap()
    }

    /// Quit, install, open, against fixtures only: a private socket and
    /// command folder, and the state, workspace and ledger that the socket
    /// selects. The daemon reports an older version until it reloads. The
    /// command folder holds a checkout's link, a live session and a ledger
    /// row exist, and an older watcher holds the lock with a record from
    /// before the version field. The launch goes through the app's own steps:
    /// the status plans the reload, the page's reload runs it, and two
    /// watcher checks at once, as a start and a restore, replace the watcher
    /// once. The `skiff` a shell finds on PATH then reaches the same daemon
    /// and ledger, with no ledger override. A second launch changes nothing,
    /// and the ledger reads with the daemon gone. Skipped without python3 or
    /// mkfifo, which make the stand-in watcher.
    #[test]
    fn one_launch_finishes_the_update() {
        let _env = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let skiffd = built("skiffd", "SKIFF_TEST_DAEMON");
        let skiff = built("skiff", "SKIFF_TEST_CLI");
        let dir = std::env::temp_dir().join(format!("skiff-app-update-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        std::fs::create_dir_all(dir.join("checkout/target/debug")).unwrap();
        if !std::process::Command::new("mkfifo").arg(dir.join("watch")).status().is_ok_and(|s| s.success()) {
            return;
        }
        let dir = dir.canonicalize().unwrap();
        let bin = dir.join("bin");

        // The command folder: a checkout's link where the app's should be.
        let checkout = dir.join("checkout/target/debug/skiff");
        std::fs::write(&checkout, "checkout").unwrap();
        std::os::unix::fs::symlink(&checkout, bin.join("skiff")).unwrap();

        // The app's context: its socket and bundled skiffd. The socket stands
        // in for the default one, which a test cannot move on macOS. Nothing
        // names a ledger, state or config file: the socket selects them.
        let socket = dir.join("skiffd.sock");
        const CONTEXT: [&str; 6] = ["SKIFF_SOCKET", "SKIFF_DAEMON", "SKIFF_LEDGER", "SKIFF_STATE", "SKIFF_CONFIG", "SKIFF_REAL_STATE"];
        let saved = CONTEXT.map(|k| (k, std::env::var_os(k)));
        for k in &CONTEXT[2..] {
            std::env::remove_var(k);
        }
        std::env::set_var("SKIFF_SOCKET", &socket);
        std::env::set_var("SKIFF_DAEMON", &skiffd);
        let mut daemon = std::process::Command::new(&skiffd)
            .env("SKIFF_TEST_REPORT_VERSION", "0.0.1")
            .env("SKIFF_SHELL_ENV", "0")
            .env("SHELL", "/bin/sh")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(dir.join("daemon.log")).unwrap())
            .spawn()
            .unwrap();
        // The `skiff` a shell finds on PATH, as from Termius or a pane.
        let shell = |args: &[&str]| {
            let out = std::process::Command::new("skiff")
                .args(args)
                .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
                .env("HOME", &dir)
                .env_remove("SKIFF_SESSION")
                .output()
                .unwrap();
            assert!(out.status.success(), "skiff {args:?}: {}", String::from_utf8_lossy(&out.stderr));
            String::from_utf8(out.stdout).unwrap()
        };
        let mut standin: Option<std::process::Child> = None;
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| rt.block_on(async {
            wait_for(&socket).await;
            let app = App::default();
            // The launch, as main.ts runs it: the status first.
            let status = serde_json::to_value(status_now(&app).await).unwrap();
            assert_eq!((&status["version"], &status["reload"]), (&"0.0.1".into(), &true.into()), "{status}");
            assert!(status["warning"].is_null(), "{status}");
            let c = app.host.lock().await.as_ref().unwrap().client();
            let pid = c.daemon_info().await.unwrap().pid.unwrap();
            let agent = c
                .coordinator(skiff_client::Coordinator {
                    spec: Some(SessionSpec { label: "Skipper".into(), command: Some("/bin/sh".into()), cwd: Some(dir.clone()), cols: 80, rows: 24, ..Default::default() }),
                    ..Default::default()
                })
                .await
                .unwrap()
                .unwrap()
                .session;
            // The Skiff agent's folder, as the app makes it, and work from
            // before the update, by the CLI that ran then.
            skipper::ensure(&skiff_core::config::load().unwrap()).unwrap();
            let out = std::process::Command::new(&skiff)
                .args(["ledger", "add", "--person", "--pane", &agent.id, "--task", "keep me", "--project", "p"])
                .env("HOME", &dir)
                .env_remove("SKIFF_SESSION")
                .output()
                .unwrap();
            assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
            // The socket's own ledger, not the person's.
            let ledger = skiff_client::ledger::path().unwrap();
            assert!(ledger.starts_with(socket.with_extension("state")) && ledger.is_file(), "{}", ledger.display());
            let rows = || {
                let db = skiff_client::ledger::open_read(&ledger).unwrap();
                serde_json::to_string(&skiff_client::ledger::rows(&db, "", None).unwrap()).unwrap()
            };
            let before_rows = rows();
            let uid = serde_json::from_str::<serde_json::Value>(&before_rows).unwrap()[0]["uid"].as_str().unwrap().to_string();
            let result_path = ledger.parent().unwrap().join("results/kept.md");
            std::fs::create_dir_all(result_path.parent().unwrap()).unwrap();
            std::fs::write(&result_path, "result").unwrap();

            // An older watcher: it holds the lock, wrote a record without a
            // version, shows as `skiff watch` and never registers.
            let script = r#"
import fcntl, json, os, sys, time
lock, sock = sys.argv[1], sys.argv[2]
fd = os.open(lock, os.O_WRONLY | os.O_CREAT)
os.set_inheritable(fd, True)
fcntl.flock(fd, fcntl.LOCK_EX)
os.ftruncate(fd, 0)
os.write(fd, json.dumps({"pid": os.getpid(), "socket": sock, "started_at": int(time.time() * 1000)}).encode())
os.chdir(os.path.dirname(lock))
os.execv("/bin/cat", ["skiff", "watch"])
"#;
            let Ok(child) = std::process::Command::new("python3")
                .args(["-c", script])
                .arg(skipper::watch_lock(&socket))
                .arg(&socket)
                .stdin(Stdio::null())
                .spawn()
            else {
                return None;
            };
            let old_watcher = child.id();
            standin = Some(child);
            let until = Instant::now() + Duration::from_secs(5);
            while skipper::watch_status(&socket).unwrap().holder.is_none_or(|h| h.pid != old_watcher) {
                assert!(Instant::now() < until, "the stand-in watcher did not start: {:?} {:?}", skipper::watch_status(&socket), standin.as_mut().map(|c| c.try_wait()));
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            std::thread::sleep(Duration::from_millis(200));

            // The command, as the app's launch fixes it.
            assert!(matches!(crate::cli_path::reconcile(&[bin.as_path()], &skiff, false).unwrap(), crate::cli_path::Linked::Updated(_)));
            assert_eq!(std::fs::read_link(bin.join("skiff")).unwrap(), skiff);
            assert_eq!(std::fs::read_to_string(&checkout).unwrap(), "checkout");

            // The reload the status planned, as the page starts it.
            let before = c.list_sessions().await.unwrap();
            drop(c);
            let r = reload_planned(&app).await;
            assert_eq!(r.error, None);
            assert!(r.missing.is_empty() && r.lost.is_empty());
            let status = serde_json::to_value(&r.status).unwrap();
            assert_eq!((&status["version"], &status["replaced"], &status["reload"]), (&skiff_core::VERSION.into(), &"0.0.1".into(), &false.into()), "{status}");
            assert!(status["warning"].is_null(), "{status}");
            let c = app.host.lock().await.as_ref().unwrap().client();
            assert_eq!(c.daemon_info().await.unwrap().pid, Some(pid));
            let after = c.list_sessions().await.unwrap();
            let ids = |l: &[SessionInfo]| l.iter().map(|s| (s.id.clone(), s.pid)).collect::<Vec<_>>();
            assert_eq!(ids(&before), ids(&after));

            // The watcher, from a start and a restore at once.
            // Watcher reconciliation is endpoint-local: the local endpoint grants it.
            let local = app.selection().process_control().unwrap();
            let (a, b) = tokio::join!(
                crate::skiff_agent::ensure_watch(&app, &local, skiff.clone(), agent.id.clone()),
                crate::skiff_agent::ensure_watch(&app, &local, skiff.clone(), agent.id.clone()),
            );
            assert!(a.clone().unwrap() || b.clone().unwrap(), "no watcher started: {a:?} {b:?}");
            // The stand-in got SIGTERM and ended, not SIGKILL.
            let status = standin.take().unwrap().wait().unwrap();
            use std::os::unix::process::ExitStatusExt;
            assert_eq!(status.signal(), Some(libc::SIGTERM));
            // One watcher of this CLI writes itself into the lock and registers.
            let until = Instant::now() + Duration::from_secs(10);
            let holder = loop {
                let h = skipper::watch_status(&socket).unwrap().holder.filter(|h| h.version.is_some());
                let registered = c.list_sessions().await.unwrap().into_iter().find(|s| s.id == agent.id).and_then(|s| s.watch).map(|w| w.pids);
                if let (Some(h), Some(pids)) = (&h, &registered) {
                    if pids == &vec![h.pid] {
                        break h.clone();
                    }
                }
                assert!(Instant::now() < until, "no current watcher registered: {h:?} {registered:?}");
                tokio::time::sleep(Duration::from_millis(100)).await;
            };
            assert_eq!(holder.version.as_deref(), Some(skipper::CLI_VERSION));
            assert_eq!(holder.exe.as_deref().map(PathBuf::from), Some(skiff.clone()));
            assert_eq!(holder.build, skipper::build_of(&skiff));

            // The `skiff` a shell finds is this CLI, on the same daemon and ledger.
            assert!(shell(&["ls", "--json"]).contains(&agent.id));
            assert!(shell(&["ledger", "ls", "--json"]).contains(&uid));

            // The next launch, a new app run, changes nothing.
            let next = App::default();
            let status = serde_json::to_value(status_now(&next).await).unwrap();
            assert_eq!((&status["reload"], &status["replaced"]), (&false.into(), &serde_json::Value::Null), "{status}");
            assert!(status["warning"].is_null(), "{status}");
            assert!(matches!(crate::cli_path::reconcile(&[bin.as_path()], &skiff, false).unwrap(), crate::cli_path::Linked::Current(_)));
            assert_eq!(crate::skiff_agent::ensure_watch(&next, &next.selection().process_control().unwrap(), skiff.clone(), agent.id.clone()).await, Ok(false));
            assert_eq!(skipper::watch_status(&socket).unwrap().holder.map(|h| h.pid), Some(holder.pid));
            let same = next.host.lock().await.as_ref().unwrap().client();
            assert_eq!(same.daemon_info().await.unwrap().pid, Some(pid));

            assert_eq!(rows(), before_rows);
            assert_eq!(std::fs::read_to_string(&result_path).unwrap(), "result");
            // The watcher ends with SIGTERM too.
            unsafe { libc::kill(holder.pid as i32, libc::SIGTERM) };
            Some(uid)
        })));
        if let Some(mut child) = standin {
            unsafe { libc::kill(child.id() as i32, libc::SIGTERM) };
            let _ = child.wait();
        }
        let shutdown = rt.block_on(async {
            tokio::time::timeout(Duration::from_secs(5), async { Client::connect(&socket).await?.shutdown().await }).await
        });
        let until = Instant::now() + Duration::from_secs(5);
        let stopped = loop {
            match daemon.try_wait() {
                Ok(Some(_)) => break true,
                Err(_) => break false,
                Ok(None) if Instant::now() >= until => break false,
                Ok(None) => std::thread::sleep(Duration::from_millis(25)),
            }
        };
        // Offline: the same command still reads the ledger, as structured output.
        let offline = match &result {
            Ok(Some(uid)) if stopped => Some(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                assert!(shell(&["ledger", "ls", "--json"]).contains(uid.as_str()));
            }))),
            _ => None,
        };
        for (key, old) in saved {
            match old {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
        if result.is_err() || !stopped {
            eprintln!("{}", std::fs::read_to_string(dir.join("daemon.log")).unwrap_or_default());
            eprintln!("{}", std::fs::read_to_string(socket.with_extension("watch.log")).unwrap_or_default());
        }
        if stopped {
            let _ = std::fs::remove_dir_all(&dir);
        }
        result.unwrap();
        assert!(stopped, "private test daemon did not stop; shutdown: {shutdown:?}; files: {}", dir.display());
        if let Some(offline) = offline {
            offline.unwrap();
        }
    }
}
