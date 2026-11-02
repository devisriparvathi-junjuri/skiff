//! Selected Host data-plane lifecycle and status helpers.

use super::*;

/// One skiff-host per computer, from this app's build: a local service of an
/// older version, or of this version from another build, is stopped with
/// SIGTERM and the bundled one started, once per app run, as the `skiff
/// watch` watcher is (`skipper::stale_holder`). So is one whose reply was
/// unreadable. It holds no session, so nothing is lost. A newer version
/// stays. One that does not stop is used as it is. One that does not come
/// back within 5 s is an error for this read: the next read starts it as for
/// any missing service. Only for a local endpoint (`_local` proves it).
pub(super) async fn replace_older_data_service(app: &App, _local: &ProcessControl, selection: &HostSelection, host: &skiff_host_api::HostId, plane: DataPlane) -> Result<DataPlane, String> {
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
pub(super) async fn restart_data_service(app: &App, _local: &ProcessControl, selection: &HostSelection, host: &skiff_host_api::HostId, pid: u32) -> Result<DataPlane, String> {
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
pub(super) fn unreadable(e: &anyhow::Error) -> bool {
    use skiff_host_api::Error as E;
    matches!(e.downcast_ref(), Some(E::IncompatibleVersion { .. } | E::MalformedContract { .. }))
}

/// The pid of the service at this computer's data socket, by its peer
/// credentials.
pub(super) async fn data_service_pid(selection: &HostSelection) -> Option<u32> {
    let s = tokio::net::UnixStream::connect(skiff_core::socket::data_socket(selection.socket())).await.ok()?;
    s.peer_cred().ok()?.pid().and_then(|p| u32::try_from(p).ok())
}

/// Why this computer's `skiff-host` did not start.
pub(super) fn start_failure(e: anyhow::Error) -> String {
    format!("Skiff cannot start the projects service on this computer ({e:#}). It tries again on its own.")
}

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
    pub(super) ready: bool,
    /// No live session plane: the data plane is not tried.
    pub(super) waiting: bool,
    pub(super) host_id: Option<String>,
    pub(super) instance_id: Option<String>,
    /// Changes with every new data connection.
    pub(super) connection: u64,
    pub(super) capabilities: Vec<String>,
    pub(super) message: Option<String>,
}

pub(super) async fn data_status(app: &App) -> DataPlaneStatus {
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
pub(super) fn data_failure(e: &anyhow::Error, selection: &HostSelection) -> String {
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
pub(super) fn data_err(e: skiff_host_api::Error) -> String {
    skiff_client::host::data::message(e)
}

pub(super) fn capability_names(c: &HostConnection) -> Vec<String> {
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
