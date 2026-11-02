//! Local and SSH endpoint selection and connection lifetime.

use super::*;
use super::legacy::{LegacyReadOnly, LegacySessions, LegacyTerminal};

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
    data: tokio::sync::Mutex<super::super::data::DataState>,
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
        use super::super::check::{run_probe, ProbeRun};
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
    pub(in crate::host) fn data_route(&self) -> Option<super::super::data::DataRoute> {
        use super::super::data::DataRoute;
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
    pub(in crate::host) fn data_state(&self) -> &tokio::sync::Mutex<super::super::data::DataState> {
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
        let (version, protocol, identity, features) = match self.info {
            Some(info) => (Some(info.version), Some(info.protocol), info.identity, info.features),
            None => (None, None, None, Vec::new()),
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
            // Only a skiffd that says it reads OSC 7501 fills program_status.
            .chain(
                features
                    .iter()
                    .any(|f| f == skiff_core::protocol::FEATURE_PROGRAM_STATUS)
                    .then(Capability::program_status),
            )
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
