//! M1a–M2 connection ownership: status, screen, inventory, session updates,
//! the session terminal, session lifecycle and groups, and the Host identity
//! skiffd reports in `pong`, over this Mac's socket or an SSH forward to a
//! remote skiffd. No Host service and no new skiffd requests: the adapter
//! speaks skiffd's own protocol over either transport.
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use skiff_host_api::{
    Capability, Endpoint, Error, Hello, Host, HostId, InstanceId, Limits, Request, Response,
    API_MAJOR,
};
use uuid::Uuid;

use super::ssh::{self, SshError, SshForward, SshOptions, SshTarget};
use super::{
    AgentSource, GroupSource, HostConnection, InventorySource, LifecycleSource, LocalSources, OutputGap,
    OutputStream, ScreenSource, SessionUpdates, StatusSource, TerminalOutput, TerminalSource,
    UpdateGap, UpdateSource, UpdateStream,
};
use crate::{Client, DaemonInfo};

struct Selection {
    /// The local daemon's socket, or for SSH the private directory that
    /// holds each forward's own directory.
    socket: PathBuf,
    /// The app's bootstrap id, for an older local skiffd only.
    identity_file: PathBuf,
    previous: Mutex<Option<Hello>>,
    /// The Host id this endpoint must report: the client's stored pin. A
    /// different one is IdentityChanged, on every connection of this
    /// selection and every one built with the same pin, until the person
    /// accepts the new identity. Activation never sets it: the client does,
    /// once the first identity is stored where every later selection reads it.
    pin: Mutex<Option<HostId>>,
    remote: Option<Remote>,
    /// M4: this Host's data-plane service, a second endpoint of it.
    data: tokio::sync::Mutex<super::data::DataState>,
}

/// An SSH endpoint: the login, the socket the probe found there, and the
/// forward this selection reuses.
struct Remote {
    /// The login. Its `socket` is the one given at construction, which may
    /// be empty: `socket` below is the one in use.
    target: SshTarget,
    /// The remote skiffd socket, `<runtime dir>/skiff/skiffd.sock` on that
    /// Host, from the probe. None until a probe found it; the first connect
    /// runs one.
    socket: Mutex<Option<String>>,
    options: SshOptions,
    dir: PathBuf,
    forward: tokio::sync::Mutex<Option<SshForward>>,
    /// Failed opens in a row, and when the next one may start.
    retry: Mutex<(u32, Option<Instant>)>,
}

/// Input that waits to cross an SSH link: more is refused (host_busy), and
/// nothing is kept to send later.
pub const REMOTE_INPUT_LIMIT: usize = 1024 * 1024;

/// The one endpoint this app runtime reaches: this Mac's socket, or a remote
/// Host over SSH. It never starts, reloads or kills a daemon: only a local
/// endpoint yields [`ProcessControl`], and the desktop policy uses it.
#[derive(Clone)]
pub struct HostSelection(Arc<Selection>);
impl HostSelection {
    /// The local endpoint at `socket`.
    pub fn new(socket: PathBuf, identity_file: PathBuf) -> Self {
        Self(Arc::new(Selection {
            socket,
            identity_file,
            previous: Mutex::new(None),
            pin: Mutex::new(None),
            remote: None,
            data: Default::default(),
        }))
    }

    /// A remote Host through `ssh`. Each forward gets its own private
    /// directory under `dir`. `pin`: the Host id stored for this endpoint;
    /// `None` only before its first accepted connection. `target.socket`
    /// may be empty: the first connect probes the Host for its one socket.
    pub fn ssh(
        target: SshTarget,
        options: SshOptions,
        dir: PathBuf,
        pin: Option<HostId>,
    ) -> Result<Self, String> {
        target.validate_login()?;
        if !target.socket.is_empty() {
            target.validate()?;
        }
        let socket = Mutex::new(Some(target.socket.clone()).filter(|s| !s.is_empty()));
        Ok(Self(Arc::new(Selection {
            socket: dir.clone(),
            identity_file: PathBuf::new(),
            previous: Mutex::new(None),
            pin: Mutex::new(pin),
            remote: Some(Remote {
                target,
                socket,
                options,
                dir,
                forward: tokio::sync::Mutex::new(None),
                retry: Mutex::new((0, None)),
            }),
            data: Default::default(),
        })))
    }

    /// The socket this app connects to. For SSH, the private directory the
    /// forwards' own directories live in: each forward has its own socket.
    pub fn socket(&self) -> &Path {
        &self.0.socket
    }
    pub fn is_local(&self) -> bool {
        self.0.remote.is_none()
    }
    pub fn ssh_target(&self) -> Option<&SshTarget> {
        self.0.remote.as_ref().map(|r| &r.target)
    }
    /// The remote skiffd socket in use, once a probe found it.
    pub fn remote_socket(&self) -> Option<String> {
        self.0.remote.as_ref().and_then(|r| r.socket.lock().unwrap().clone())
    }
    /// The socket a probe of this Host found: the forward goes there from
    /// now on. Only an absolute path with no colon, as `-L` takes it.
    pub fn set_remote_socket(&self, socket: String) {
        if let Some(r) = &self.0.remote {
            if (SshTarget { socket: socket.clone(), ..r.target.clone() }).validate().is_ok() {
                *r.socket.lock().unwrap() = Some(socket);
            }
        }
    }
    /// The login with the socket in use. None before a probe found one.
    fn remote_target(&self) -> Option<SshTarget> {
        let r = self.0.remote.as_ref()?;
        let socket = r.socket.lock().unwrap().clone()?;
        Some(SshTarget { socket, ..r.target.clone() })
    }
    /// The Host id this endpoint must report, once known.
    pub fn pinned(&self) -> Option<HostId> {
        self.0.pin.lock().unwrap().clone()
    }
    /// Makes `host_id` the one this endpoint must report, as the client's
    /// store now holds it: the first identity once it is stored, or the one
    /// another window or run stored first.
    pub fn repin(&self, host_id: HostId) {
        *self.0.pin.lock().unwrap() = Some(host_id);
    }
    /// A descriptor. For SSH, `account` and `port` stay empty and 0: ssh
    /// config decides them for the destination.
    pub fn endpoint(&self) -> Endpoint {
        match &self.0.remote {
            None => Endpoint::Local {
                socket: self.0.socket.to_string_lossy().into_owned(),
            },
            Some(r) => Endpoint::Ssh {
                hostname: r.target.destination.clone(),
                account: r.target.user.clone().unwrap_or_default(),
                port: r.target.port.unwrap_or(0),
                socket: r.socket.lock().unwrap().clone().unwrap_or_default(),
            },
        }
    }

    /// Permission to start, signal or reload this endpoint's daemon, from
    /// the endpoint itself. See [`ProcessControl`].
    pub fn process_control(&self) -> Option<ProcessControl> {
        ProcessControl::for_endpoint(&self.endpoint())
    }

    /// No identity write or new handshake before existing compatibility policy
    /// accepts the legacy client. Even probes have this selected endpoint.
    /// For SSH it reuses a live forward or opens one, with a growing wait
    /// after failures, and bounds the input queue.
    pub async fn connect(&self) -> anyhow::Result<PendingConnection> {
        let Some(remote) = &self.0.remote else {
            return Ok(PendingConnection {
                selection: self.clone(),
                client: Arc::new(Client::connect(self.socket()).await?),
                info: None,
            });
        };
        let mut forward = remote.forward.lock().await;
        if !forward.as_mut().is_some_and(|f| f.is_alive()) {
            *forward = None;
            {
                let retry = remote.retry.lock().unwrap();
                if let Some(at) = retry.1.filter(|at| *at > Instant::now()) {
                    let seconds = (at - Instant::now()).as_secs() + 1;
                    return Err(SshError::Waiting {
                        destination: remote.target.destination.clone(),
                        seconds,
                    }
                    .into());
                }
            }
            // The socket, from the probe this connect or an earlier one ran.
            let target = match self.remote_target() {
                Some(t) => t,
                None => match self.probe_socket(remote).await {
                    Ok(t) => t,
                    Err(e) => {
                        let mut retry = remote.retry.lock().unwrap();
                        retry.0 += 1;
                        retry.1 = Some(Instant::now() + ssh::backoff(retry.0));
                        return Err(e);
                    }
                },
            };
            match SshForward::open(&target, &remote.options, &remote.dir).await {
                Ok(f) => {
                    *remote.retry.lock().unwrap() = (0, None);
                    *forward = Some(f);
                }
                Err(e) => {
                    let mut retry = remote.retry.lock().unwrap();
                    retry.0 += 1;
                    retry.1 = Some(Instant::now() + ssh::backoff(retry.0));
                    return Err(e.into());
                }
            }
        }
        let open = forward.as_ref().expect("forward is open");
        let client = match Client::connect(open.socket()).await {
            Ok(client) => client,
            Err(e) => return Err(open.diagnose().map(Into::into).unwrap_or(e)),
        };
        client.limit_input(REMOTE_INPUT_LIMIT);
        Ok(PendingConnection {
            selection: self.clone(),
            client: Arc::new(client),
            info: None,
        })
    }

    /// Asks the Host where its skiffd listens, over one strict ssh run, and
    /// keeps the answer. No skiffd there is [`SshError::NoSkiffd`].
    async fn probe_socket(&self, remote: &Remote) -> anyhow::Result<SshTarget> {
        use super::check::{run_probe, ProbeRun};
        let probe = run_probe(&remote.target, &remote.options).await.map_err(anyhow::Error::msg)?;
        let p = match probe {
            ProbeRun::Probed(p, _) => p,
            ProbeRun::Ssh(stderr) => anyhow::bail!("ssh to {} stopped before Skiff could look for skiffd: {}", remote.target.destination, stderr.lines().last().unwrap_or_default()),
            ProbeRun::TimedOut => anyhow::bail!("{} did not answer in time", remote.target.destination),
            ProbeRun::Garbled => anyhow::bail!("Skiff cannot read {}: its login shell prints extra text", remote.target.destination),
        };
        let target = SshTarget { socket: p.socket.clone(), ..remote.target.clone() };
        target.validate().map_err(anyhow::Error::msg)?;
        if !p.socket_live {
            return Err(SshError::NoSkiffd { destination: remote.target.destination.clone(), socket: p.socket }.into());
        }
        *remote.socket.lock().unwrap() = Some(p.socket);
        Ok(target)
    }

    /// M4: where this Host's data-plane service listens, beside its skiffd
    /// socket. For SSH, the remote path, named by path arithmetic only.
    /// None for a remote Host whose socket no probe has found yet: the
    /// session plane connects first, and its probe finds it.
    pub(super) fn data_route(&self) -> Option<super::data::DataRoute> {
        use super::data::DataRoute;
        Some(match &self.0.remote {
            None => DataRoute::Local(skiff_core::socket::data_socket(self.socket())),
            Some(r) => {
                let t = self.remote_target()?;
                DataRoute::Ssh {
                    target: SshTarget {
                        socket: skiff_core::socket::data_socket(Path::new(&t.socket)).to_string_lossy().into_owned(),
                        ..t
                    },
                    options: r.options.clone(),
                    dir: r.dir.clone(),
                }
            }
        })
    }
    pub(super) fn data_state(&self) -> &tokio::sync::Mutex<super::data::DataState> {
        &self.0.data
    }

    /// The SSH cause of a failure on this endpoint, if ssh reported one.
    async fn diagnose(&self) -> Option<SshError> {
        let remote = self.0.remote.as_ref()?;
        let forward = remote.forward.lock().await;
        forward.as_ref()?.diagnose()
    }
}

/// Only HostSelection can construct a pending connection; activation cannot
/// accidentally pair a selected Host with a client from another endpoint.
pub struct PendingConnection {
    selection: HostSelection,
    client: Arc<Client>,
    info: Option<DaemonInfo>,
}
impl PendingConnection {
    pub fn client(&self) -> &Arc<Client> {
        &self.client
    }

    /// The daemon's `pong` on this connection. Activation uses the last one,
    /// so checking the daemon first costs no second Ping.
    pub async fn daemon_info(&mut self) -> anyhow::Result<DaemonInfo> {
        let info = match self.client.daemon_info().await {
            Ok(info) => info,
            // Over SSH, a closed channel usually has its cause in ssh's output.
            Err(e) => {
                // The channel's failure reaches ssh's stderr just after EOF.
                for _ in 0..20 {
                    if let Some(cause) = self.selection.diagnose().await {
                        return Err(cause.into());
                    }
                    if self.selection.is_local() {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
                return Err(e);
            }
        };
        self.info = Some(info.clone());
        Ok(info)
    }

    /// Takes the Host's identity from the `pong` that [`Self::daemon_info`]
    /// recorded; activation itself sends nothing. With no identity there (an
    /// older skiffd, or no recorded pong) it uses the app's bootstrap id and
    /// an adapter generation for the instance, so every reconnect is
    /// NewInstance. Never a pid or peer credentials.
    pub async fn activate(self) -> anyhow::Result<SelectedHost> {
        let (version, protocol, identity) = match self.info {
            Some(info) => (Some(info.version), Some(info.protocol), info.identity),
            None => (None, None, None),
        };
        let remote = self.selection.ssh_target().cloned();
        // A remote Host must say who it is. The app's bootstrap id describes
        // this Mac, and the forward's peer credentials name `ssh`.
        if let (Some(target), None) = (&remote, &identity) {
            return Err(Error::MalformedContract {
                message: format!(
                    "skiffd on {} reports no Host identity. Update Skiff on that Host.",
                    target.destination
                ),
            }
            .into());
        }
        let malformed = |message: &str| Error::MalformedContract {
            message: message.into(),
        };
        let mut service_versions = BTreeMap::from([(
            "local-status-adapter".to_string(),
            env!("CARGO_PKG_VERSION").to_string(),
        )]);
        service_versions.extend(version.map(|v| ("skiffd".to_string(), v)));
        let (host, instance_id, source) = match identity {
            Some(identity) => (
                Host {
                    host_id: HostId::try_from(identity.host_id)
                        .map_err(|_| malformed("blank host id from skiffd"))?,
                    os: identity.os,
                    account: identity.account,
                },
                InstanceId::try_from(identity.instance_id)
                    .map_err(|_| malformed("blank instance id from skiffd"))?,
                IdentitySource::Host,
            ),
            None => {
                let path = self.selection.0.identity_file.clone();
                let host_id =
                    tokio::task::spawn_blocking(move || skiff_core::host_identity::load(&path))
                        .await??;
                (
                    Host {
                        host_id: HostId::try_from(host_id).map_err(anyhow::Error::msg)?,
                        os: std::env::consts::OS.into(),
                        account: skiff_core::host_identity::account(),
                    },
                    // The in-process adapter generation, NOT skiffd's PID or
                    // an assertion that a legacy daemon supplies identity.
                    InstanceId::try_from(Uuid::new_v4().to_string()).unwrap(),
                    IdentitySource::LocalBootstrap,
                )
            }
        };
        // The pin outlives any one connection: a rebuilt selection or a new
        // app run that carries it refuses another Host just as a reconnect does.
        if let Some(pin) = self.selection.0.pin.lock().unwrap().as_ref() {
            if *pin != host.host_id {
                return Err(Error::IdentityChanged {
                    expected: pin.clone(),
                    received: host.host_id.clone(),
                }
                .into());
            }
        }
        // What the endpoint can actually do. Locally the app replaces or
        // reloads a skiffd that speaks another protocol, so every family is
        // served. A remote skiffd is never changed: its mutations and agent
        // list are offered only when it speaks this app's legacy frames.
        let mutable = remote.is_none() || protocol == Some(skiff_core::PROTOCOL);
        let hello = Hello {
            host,
            instance_id,
            service_versions,
            api_major: API_MAJOR,
            capabilities: [
                Capability::connection_status(),
                Capability::session_screen(),
                Capability::session_inventory(),
                Capability::session_updates(),
                Capability::session_terminal(),
            ]
            .into_iter()
            .chain(
                mutable
                    .then(|| {
                        [
                            Capability::session_lifecycle(),
                            Capability::session_groups(),
                            Capability::session_agents(),
                        ]
                    })
                    .into_iter()
                    .flatten(),
            )
            .collect(),
            limits: Limits {
                max_frame_bytes: 8 * 1024 * 1024,
                max_in_flight: 64,
            },
        };
        let connection = {
            let mut previous = self.selection.0.previous.lock().unwrap();
            let source = Arc::new(LegacyReadOnly(self.client.clone()));
            let sessions = Arc::new(LegacySessions {
                client: self.client.clone(),
                remote: remote.is_some(),
            });
            let connection = HostConnection::local(
                LocalSources {
                    status: source.clone(),
                    screen: Some(source.clone()),
                    inventory: Some(source.clone()),
                    updates: Some(source),
                    terminal: Some(Arc::new(LegacyTerminal(self.client.clone()))),
                    lifecycle: mutable.then(|| sessions.clone() as _),
                    groups: mutable.then(|| sessions.clone() as _),
                    agents: mutable.then(|| sessions.clone() as _),
                },
                hello,
                previous.as_ref(),
            )?;
            *previous = Some(connection.hello().clone());
            // No pin is taken here: the client pins a first identity only
            // after it stored it, so a failed store leaves this selection
            // unpinned and the next try stores it again.
            Arc::new(connection)
        };
        Ok(SelectedHost {
            selection: self.selection,
            client: self.client,
            connection,
            source,
        })
    }
}

/// Desktop runtime owner. Existing terminal/data paths borrow the very same
/// Client until their own slices migrate; they never obtain a second one here.
pub struct SelectedHost {
    selection: HostSelection,
    client: Arc<Client>,
    connection: Arc<HostConnection>,
    source: IdentitySource,
}
impl SelectedHost {
    pub fn client(&self) -> Arc<Client> {
        self.client.clone()
    }
    pub fn connection(&self) -> Arc<HostConnection> {
        self.connection.clone()
    }
    pub fn endpoint(&self) -> Endpoint {
        self.selection.endpoint()
    }
    pub fn process_control(&self) -> Option<ProcessControl> {
        self.selection.process_control()
    }
    pub fn identity_source(&self) -> IdentitySource {
        self.source
    }
}

/// Where the Hello's identity came from. A remote Host must be [`Self::Host`]:
/// only the serving Host can say who it is and which run answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentitySource {
    /// skiffd's `pong`: its Host id, and an instance id new on every start
    /// and reload, so a reconnect can be SameInstance.
    Host,
    /// An older skiffd: the app's bootstrap id and an adapter generation.
    /// Every reconnect is NewInstance.
    LocalBootstrap,
}
impl Drop for SelectedHost {
    fn drop(&mut self) {
        self.connection.disconnect();
    }
}

/// Proof that the selected endpoint is local, so the app may start, signal,
/// reload or replace its daemon, link its CLI or manage its watcher. Only
/// [`Self::for_endpoint`] makes one, and it refuses every non-local endpoint:
/// a remote Host is reached, never managed. This is endpoint policy, not
/// daemon identity: peer credentials still only name the local socket's peer.
#[derive(Debug, Clone)]
pub struct ProcessControl(());
impl ProcessControl {
    pub fn for_endpoint(endpoint: &Endpoint) -> Option<Self> {
        endpoint.is_local().then_some(Self(()))
    }
}

struct LegacyReadOnly(Arc<Client>);
impl StatusSource for LegacyReadOnly {
    fn is_lost(&self) -> bool {
        self.0.is_closed()
    }

    fn status(
        &self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Response, Error>> + Send + '_>>
    {
        Box::pin(async {
            match self.0.hello().await {
                Ok((version, protocol)) => Ok(Response::ConnectionStatus { version, protocol }),
                Err(e) if e.downcast_ref::<crate::ReplyTimeout>().is_some() => Err(Error::TimedOut),
                Err(_) if self.0.is_closed() => Err(Error::TransportLost),
                Err(e) => Err(Error::MalformedContract {
                    message: format!("{e:#}"),
                }),
            }
        })
    }
}

impl ScreenSource for LegacyReadOnly {
    fn screen(
        &self,
        session: String,
        rows: u16,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Response, Error>> + Send + '_>>
    {
        Box::pin(async move {
            // Preserve the legacy request/reply mapping exactly; no new Host
            // envelope is sent to skiffd and no terminal subscription is taken.
            match self
                .0
                .request_decoded(skiff_core::protocol::Request::Screen { session, rows })
                .await
            {
                Ok(skiff_core::protocol::Response::Screen { lines }) => {
                    Ok(Response::SessionScreen { lines })
                }
                Ok(skiff_core::protocol::Response::Error { message }) => {
                    Err(Error::OperationFailed { message })
                }
                Ok(other) => Err(Error::MalformedContract {
                    message: format!("unexpected reply: {other:?}"),
                }),
                Err(e) if e.downcast_ref::<crate::ReplyTimeout>().is_some() => Err(Error::TimedOut),
                Err(e) if e.downcast_ref::<crate::ReplyDecodeError>().is_some() => {
                    Err(Error::MalformedContract {
                        message: e.to_string(),
                    })
                }
                Err(_) if self.0.is_closed() => Err(Error::TransportLost),
                Err(e) => Err(Error::MalformedContract {
                    message: format!("{e:#}"),
                }),
            }
        })
    }
}

impl InventorySource for LegacyReadOnly {
    fn inventory(
        &self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Response, Error>> + Send + '_>>
    {
        Box::pin(async {
            match self
                .0
                .request_decoded(skiff_core::protocol::Request::ListSessions)
                .await
            {
                Ok(skiff_core::protocol::Response::Sessions { sessions }) => {
                    let sessions = sessions
                        .into_iter()
                        .map(super::inventory::project_session)
                        .collect::<Result<_, _>>()?;
                    Ok(Response::SessionInventory { sessions })
                }
                Ok(skiff_core::protocol::Response::Error { message }) => {
                    Err(Error::OperationFailed { message })
                }
                Ok(other) => Err(Error::MalformedContract {
                    message: format!("unexpected reply: {other:?}"),
                }),
                Err(e) if e.downcast_ref::<crate::ReplyTimeout>().is_some() => Err(Error::TimedOut),
                Err(e) if e.downcast_ref::<crate::ReplyDecodeError>().is_some() => {
                    Err(Error::MalformedContract {
                        message: e.to_string(),
                    })
                }
                Err(_) if self.0.is_closed() => Err(Error::TransportLost),
                Err(e) => Err(Error::MalformedContract {
                    message: format!("{e:#}"),
                }),
            }
        })
    }
}

impl UpdateSource for LegacyReadOnly {
    fn updates(&self) -> Box<dyn UpdateStream> {
        // The same broadcast the desktop read directly before M1d: the
        // Client already routes output elsewhere and skips unreadable frames.
        Box::new(LegacyUpdateStream(self.0.events()))
    }
}

struct LegacyUpdateStream(tokio::sync::broadcast::Receiver<skiff_core::protocol::Event>);
impl UpdateStream for LegacyUpdateStream {
    fn recv(
        &mut self,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<skiff_host_api::SessionUpdate, UpdateGap>>
                + Send
                + '_,
        >,
    > {
        Box::pin(async {
            use tokio::sync::broadcast::error::RecvError;
            match self.0.recv().await {
                Ok(event) => super::updates::project_update(event).map_err(UpdateGap::Malformed),
                Err(RecvError::Lagged(n)) => Err(UpdateGap::Lagged(n)),
                Err(RecvError::Closed) => Err(UpdateGap::Closed),
            }
        })
    }
}

/// Temporary desktop compatibility projection of [`SessionUpdates`]: the
/// existing page events, in order. As before M1d, missed updates are skipped
/// without replay, so the page's own refresh policy is unchanged.
pub struct LegacyUpdates(SessionUpdates);
impl LegacyUpdates {
    /// The next event, or None once the connection is gone.
    pub async fn recv(&mut self) -> Option<skiff_core::protocol::Event> {
        loop {
            match self.0.recv().await {
                Ok(update) => return Some(super::updates::restore_update(update)),
                Err(UpdateGap::Lagged(_)) => {}
                Err(UpdateGap::Malformed(e)) => tracing::warn!("skipped session update: {e}"),
                Err(UpdateGap::Closed) => return None,
            }
        }
    }
}

/// Capability/version failures are explicit, never a raw Client fallback.
pub fn legacy_updates(connection: &HostConnection) -> anyhow::Result<LegacyUpdates> {
    Ok(LegacyUpdates(connection.session_updates()?))
}

/// Preserve the existing Tauri list schema until a deliberate GUI type migration.
pub async fn legacy_inventory(
    connection: &HostConnection,
) -> anyhow::Result<Vec<skiff_core::session::SessionInfo>> {
    match connection
        .request(Request::SessionInventory)
        .await
        .map_err(legacy_read_error)?
    {
        Response::SessionInventory { sessions } => Ok(sessions
            .into_iter()
            .map(super::inventory::restore_session)
            .collect()),
        _ => Err(Error::MalformedContract {
            message: "expected session inventory".into(),
        }
        .into()),
    }
}

fn legacy_read_error(error: Error) -> anyhow::Error {
    match error {
        Error::OperationFailed { message } | Error::MalformedContract { message } => {
            anyhow::Error::msg(message)
        }
        Error::TimedOut => crate::ReplyTimeout(std::time::Duration::from_secs(20)).into(),
        Error::TransportLost => anyhow::Error::msg("skiffd connection closed"),
        other => other.into(),
    }
}

/// Temporary desktop compatibility projection. The Host request remains typed;
/// only its existing Tauri caller receives the unchanged legacy error text.
/// Capability/version failures propagate explicitly, never via legacy fallback.
pub async fn legacy_screen(
    connection: &HostConnection,
    session: &str,
    rows: u16,
) -> anyhow::Result<Vec<String>> {
    let reply = connection
        .request(Request::SessionScreen {
            session: session.into(),
            rows,
        })
        .await
        .map_err(legacy_read_error)?;
    match reply {
        Response::SessionScreen { lines } => Ok(lines),
        _ => Err(Error::MalformedContract {
            message: "expected session screen".into(),
        }
        .into()),
    }
}

/// The M1e terminal plane: the very same skiffd frames the desktop sent
/// before (subscribe, unsubscribe, resize, write), on the selected Client,
/// and its per-session output queue with lag marking. No Host envelope.
struct LegacyTerminal(Arc<Client>);
impl LegacyTerminal {
    async fn expect_ok(&self, request: skiff_core::protocol::Request) -> Result<Response, Error> {
        match self.0.request_decoded(request).await {
            Ok(skiff_core::protocol::Response::Ok) => Ok(Response::Ok),
            Ok(skiff_core::protocol::Response::Error { message }) => {
                Err(Error::OperationFailed { message })
            }
            Ok(other) => Err(Error::MalformedContract {
                message: format!("unexpected reply: {other:?}"),
            }),
            Err(e) => Err(legacy_transport_error(&self.0, e)),
        }
    }
}
type Op<'a> =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<Response, Error>> + Send + 'a>>;
impl TerminalSource for LegacyTerminal {
    fn output(&self, session: &str) -> Box<dyn OutputStream> {
        Box::new(LegacyOutput(self.0.output(session)))
    }
    fn attach(&self, session: String) -> Op<'_> {
        Box::pin(self.expect_ok(skiff_core::protocol::Request::Subscribe { session }))
    }
    fn detach(&self, session: String) -> Op<'_> {
        // As Client::unsubscribe: the receiver closes before the request.
        self.0.outputs.lock().unwrap().remove(&session);
        Box::pin(self.expect_ok(skiff_core::protocol::Request::Unsubscribe { session }))
    }
    fn resize(&self, session: String, cols: u16, rows: u16) -> Op<'_> {
        Box::pin(self.expect_ok(skiff_core::protocol::Request::Resize {
            session,
            cols,
            rows,
        }))
    }
    fn input(&self, session: &str, data: Vec<u8>, paste: bool) -> Result<(), Error> {
        // The legacy queue: a keystroke never waits, in call order. A remote
        // connection bounds it; full is HostBusy, and nothing is kept to retry.
        self.0.input_now(session, data, paste).map_err(|e| {
            match e.downcast_ref::<crate::InputBusy>() {
                Some(_) => Error::HostBusy,
                None => Error::TransportLost,
            }
        })
    }
}

struct LegacyOutput(crate::Output);
impl OutputStream for LegacyOutput {
    fn recv(
        &mut self,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<skiff_host_api::TerminalFrame, OutputGap>>
                + Send
                + '_,
        >,
    > {
        Box::pin(async {
            use skiff_core::protocol::Event;
            use skiff_host_api::{EchoTimes, TerminalFrame};
            use tokio::sync::broadcast::error::RecvError;
            loop {
                match self.0.recv().await {
                    Ok(Event::Snapshot {
                        data, cols, rows, ..
                    }) => return Ok(TerminalFrame::Snapshot { data, cols, rows }),
                    Ok(Event::Output { data, echo, .. }) => {
                        return Ok(TerminalFrame::Output {
                            data,
                            echo: echo.map(|e| EchoTimes {
                                wrote: e.wrote,
                                read: e.read,
                            }),
                        })
                    }
                    // The Client routes only output and snapshots here.
                    Ok(_) => {}
                    Err(RecvError::Lagged(_)) => return Err(OutputGap::Lagged),
                    Err(RecvError::Closed) => return Err(OutputGap::Closed),
                }
            }
        })
    }
    fn clear(&mut self) {
        self.0.clear();
    }
}

fn legacy_transport_error(client: &Client, e: anyhow::Error) -> Error {
    if e.downcast_ref::<crate::ReplyTimeout>().is_some() {
        Error::TimedOut
    } else if e.downcast_ref::<crate::ReplyDecodeError>().is_some() {
        Error::MalformedContract {
            message: e.to_string(),
        }
    } else if client.is_closed() {
        Error::TransportLost
    } else {
        Error::MalformedContract {
            message: format!("{e:#}"),
        }
    }
}

/// Temporary desktop compatibility projection of the terminal family: typed
/// Host requests, with the existing Tauri error text. Capability/version
/// failures propagate explicitly, never via a raw Client fallback.
pub async fn legacy_attach(connection: &HostConnection, session: &str) -> anyhow::Result<()> {
    legacy_terminal_request(
        connection,
        Request::TerminalAttach {
            session: session.into(),
        },
    )
    .await
}
pub async fn legacy_detach(connection: &HostConnection, session: &str) -> anyhow::Result<()> {
    legacy_terminal_request(
        connection,
        Request::TerminalDetach {
            session: session.into(),
        },
    )
    .await
}
pub async fn legacy_resize(
    connection: &HostConnection,
    session: &str,
    cols: u16,
    rows: u16,
) -> anyhow::Result<()> {
    legacy_terminal_request(
        connection,
        Request::TerminalResize {
            session: session.into(),
            cols,
            rows,
        },
    )
    .await
}
async fn legacy_terminal_request(
    connection: &HostConnection,
    request: Request,
) -> anyhow::Result<()> {
    match connection
        .request(request)
        .await
        .map_err(legacy_read_error)?
    {
        Response::Ok => Ok(()),
        other => Err(anyhow::anyhow!("unexpected reply: {other:?}")),
    }
}
/// Keystrokes and pastes, queued without a wait; the old write_now text on loss.
pub fn legacy_input(
    connection: &HostConnection,
    session: &str,
    data: Vec<u8>,
) -> anyhow::Result<()> {
    connection
        .terminal_input(session, data, false)
        .map_err(legacy_read_error)
}
/// The session's terminal receiver; take it before [`legacy_attach`].
pub fn legacy_output(connection: &HostConnection, session: &str) -> anyhow::Result<TerminalOutput> {
    Ok(connection.terminal_output(session)?)
}

/// Match the legacy desktop status tuple without exposing core types in the
/// Host contract. A capability/version error never falls back to Client::hello.
pub async fn connection_status(connection: &HostConnection) -> Result<(String, u32), Error> {
    match connection.request(Request::ConnectionStatus).await? {
        Response::ConnectionStatus { version, protocol } => Ok((version, protocol)),
        _ => Err(Error::MalformedContract {
            message: "expected connection status".into(),
        }),
    }
}

/// M1g: session lifecycle and groups. The very same skiffd requests the
/// desktop sent before (`create_session`, `kill`, `rename_session`,
/// `set_session_theme`, `seen`, `list_groups`, `save_group`, `delete_group`)
/// on the selected Client, and M3's `list_agents`. No Host envelope, no
/// retry: each request is written once, and a lost reply is an error.
struct LegacySessions {
    client: Arc<Client>,
    /// A remote Host: an operation that acts on this Mac is refused.
    remote: bool,
}
impl LegacySessions {
    async fn call(
        &self,
        request: skiff_core::protocol::Request,
    ) -> Result<skiff_core::protocol::Response, Error> {
        match self.client.request_decoded(request).await {
            Ok(skiff_core::protocol::Response::Error { message }) => {
                Err(Error::OperationFailed { message })
            }
            Ok(reply) => Ok(reply),
            Err(e) => Err(legacy_transport_error(&self.client, e)),
        }
    }

    async fn row(&self, request: skiff_core::protocol::Request) -> Result<Response, Error> {
        match self.call(request).await? {
            skiff_core::protocol::Response::Session { session } => Ok(Response::Session {
                session: Box::new(super::inventory::project_session(session)?),
            }),
            other => Err(unexpected(other)),
        }
    }

    async fn ok(&self, request: skiff_core::protocol::Request) -> Result<Response, Error> {
        match self.call(request).await? {
            skiff_core::protocol::Response::Ok => Ok(Response::Ok),
            other => Err(unexpected(other)),
        }
    }
}

fn unexpected(reply: skiff_core::protocol::Response) -> Error {
    Error::MalformedContract {
        message: format!("unexpected reply: {reply:?}"),
    }
}

impl LifecycleSource for LegacySessions {
    fn lifecycle(&self, request: Request) -> Op<'_> {
        use skiff_core::protocol::Request as Legacy;
        Box::pin(async move {
            match request {
                // A peek tool shows a file of this Mac over its terminals: it
                // never starts on a remote Host.
                Request::SessionCreate { spec } if self.remote && spec.peek => {
                    Err(Error::Permission {
                        message: "A peek tool runs on this computer only, not on a remote Host.".into(),
                    })
                }
                Request::SessionCreate { spec } => {
                    self.row(Legacy::CreateSession {
                        spec: super::lifecycle::restore_spec(spec),
                    })
                    .await
                }
                Request::SessionKill { session } => self.ok(Legacy::Kill { session }).await,
                Request::SessionRename { session, name } => {
                    self.row(Legacy::RenameSession { session, name }).await
                }
                Request::SessionSetTheme { session, theme } => {
                    self.row(Legacy::SetSessionTheme { session, theme }).await
                }
                Request::SessionSeen { session } => self.ok(Legacy::Seen { session }).await,
                _ => Err(Error::MalformedContract {
                    message: "not a session lifecycle request".into(),
                }),
            }
        })
    }
}

impl GroupSource for LegacySessions {
    fn groups(&self, request: Request) -> Op<'_> {
        use skiff_core::protocol::{Request as Legacy, Response as LegacyReply};
        Box::pin(async move {
            match request {
                Request::GroupList => match self.call(Legacy::ListGroups).await? {
                    LegacyReply::Groups { groups } => Ok(Response::Groups {
                        groups: groups
                            .into_iter()
                            .map(super::lifecycle::project_group)
                            .collect(),
                    }),
                    other => Err(unexpected(other)),
                },
                Request::GroupSave { group } => match self
                    .call(Legacy::SaveGroup {
                        group: super::lifecycle::restore_group(group),
                    })
                    .await?
                {
                    LegacyReply::Group { group } => Ok(Response::Group {
                        group: super::lifecycle::project_group(group),
                    }),
                    other => Err(unexpected(other)),
                },
                Request::GroupDelete { group } => self.ok(Legacy::DeleteGroup { group }).await,
                Request::GroupOrder { groups } => self.ok(Legacy::OrderGroups { groups }).await,
                _ => Err(Error::MalformedContract {
                    message: "not a session group request".into(),
                }),
            }
        })
    }
}

impl AgentSource for LegacySessions {
    fn agents(&self) -> Op<'_> {
        use skiff_core::protocol::{Request as Legacy, Response as LegacyReply};
        Box::pin(async move {
            match self.call(Legacy::ListAgents).await? {
                LegacyReply::Agents { agents } => Ok(Response::Agents {
                    agents: agents
                        .into_iter()
                        .map(super::lifecycle::project_agent)
                        .collect(),
                }),
                other => Err(unexpected(other)),
            }
        })
    }
}

/// Temporary desktop compatibility projections of the lifecycle and group
/// families: typed Host requests, the existing Tauri results and error text.
/// Capability/version failures propagate explicitly, never via a raw Client.
async fn legacy_request(connection: &HostConnection, request: Request) -> anyhow::Result<Response> {
    connection.request(request).await.map_err(legacy_read_error)
}

fn legacy_row(reply: Response) -> anyhow::Result<skiff_core::session::SessionInfo> {
    match reply {
        Response::Session { session } => Ok(super::inventory::restore_session(*session)),
        _ => Err(Error::MalformedContract {
            message: "expected a session".into(),
        }
        .into()),
    }
}

pub async fn legacy_create(
    connection: &HostConnection,
    spec: skiff_core::session::SessionSpec,
) -> anyhow::Result<skiff_core::session::SessionInfo> {
    let spec = super::lifecycle::project_spec(spec).map_err(legacy_read_error)?;
    legacy_row(legacy_request(connection, Request::SessionCreate { spec }).await?)
}

pub async fn legacy_kill(connection: &HostConnection, session: &str) -> anyhow::Result<()> {
    legacy_request(
        connection,
        Request::SessionKill {
            session: session.into(),
        },
    )
    .await
    .map(drop)
}

pub async fn legacy_rename(
    connection: &HostConnection,
    session: &str,
    name: &str,
) -> anyhow::Result<skiff_core::session::SessionInfo> {
    legacy_row(
        legacy_request(
            connection,
            Request::SessionRename {
                session: session.into(),
                name: name.into(),
            },
        )
        .await?,
    )
}

pub async fn legacy_set_theme(
    connection: &HostConnection,
    session: &str,
    theme: Option<String>,
) -> anyhow::Result<skiff_core::session::SessionInfo> {
    legacy_row(
        legacy_request(
            connection,
            Request::SessionSetTheme {
                session: session.into(),
                theme,
            },
        )
        .await?,
    )
}

pub async fn legacy_seen(connection: &HostConnection, session: &str) -> anyhow::Result<()> {
    legacy_request(
        connection,
        Request::SessionSeen {
            session: session.into(),
        },
    )
    .await
    .map(drop)
}

pub async fn legacy_groups(
    connection: &HostConnection,
) -> anyhow::Result<Vec<skiff_core::group::Group>> {
    match legacy_request(connection, Request::GroupList).await? {
        Response::Groups { groups } => Ok(groups
            .into_iter()
            .map(super::lifecycle::restore_group)
            .collect()),
        _ => Err(Error::MalformedContract {
            message: "expected groups".into(),
        }
        .into()),
    }
}

pub async fn legacy_save_group(
    connection: &HostConnection,
    group: skiff_core::group::Group,
) -> anyhow::Result<skiff_core::group::Group> {
    let group = super::lifecycle::project_group(group);
    match legacy_request(connection, Request::GroupSave { group }).await? {
        Response::Group { group } => Ok(super::lifecycle::restore_group(group)),
        _ => Err(Error::MalformedContract {
            message: "expected a group".into(),
        }
        .into()),
    }
}

pub async fn legacy_delete_group(connection: &HostConnection, group: &str) -> anyhow::Result<()> {
    legacy_request(
        connection,
        Request::GroupDelete {
            group: group.into(),
        },
    )
    .await
    .map(drop)
}

pub async fn legacy_order_groups(connection: &HostConnection, groups: Vec<String>) -> anyhow::Result<()> {
    legacy_request(connection, Request::GroupOrder { groups }).await.map(drop)
}

/// M3: the Host account's agent list, in the existing Tauri result.
pub async fn legacy_agents(
    connection: &HostConnection,
) -> anyhow::Result<Vec<skiff_core::project::AgentInfo>> {
    match legacy_request(connection, Request::AgentList).await? {
        Response::Agents { agents } => Ok(agents
            .into_iter()
            .map(super::lifecycle::restore_agent)
            .collect()),
        _ => Err(Error::MalformedContract {
            message: "expected agents".into(),
        }
        .into()),
    }
}
