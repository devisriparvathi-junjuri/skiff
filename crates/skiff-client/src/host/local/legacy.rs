//! Legacy local Host sources and procedural compatibility adapter.

use super::*;

pub(super) struct LegacyReadOnly(pub(super) Arc<Client>);
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
                        .map(super::super::inventory::project_session)
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
                Ok(event) => super::super::updates::project_update(event).map_err(UpdateGap::Malformed),
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
                Ok(update) => return Some(super::super::updates::restore_update(update)),
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
            .map(super::super::inventory::restore_session)
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
pub(super) struct LegacyTerminal(pub(super) Arc<Client>);
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
pub(super) struct LegacySessions {
    pub(super) client: Arc<Client>,
    /// A remote Host: an operation that acts on this Mac is refused.
    pub(super) remote: bool,
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
                session: Box::new(super::super::inventory::project_session(session)?),
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
                        spec: super::super::lifecycle::restore_spec(spec),
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
                            .map(super::super::lifecycle::project_group)
                            .collect(),
                    }),
                    other => Err(unexpected(other)),
                },
                Request::GroupSave { group } => match self
                    .call(Legacy::SaveGroup {
                        group: super::super::lifecycle::restore_group(group),
                    })
                    .await?
                {
                    LegacyReply::Group { group } => Ok(Response::Group {
                        group: super::super::lifecycle::project_group(group),
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
                        .map(super::super::lifecycle::project_agent)
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
        Response::Session { session } => Ok(super::super::inventory::restore_session(*session)),
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
    let spec = super::super::lifecycle::project_spec(spec).map_err(legacy_read_error)?;
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
            .map(super::super::lifecycle::restore_group)
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
    let group = super::super::lifecycle::project_group(group);
    match legacy_request(connection, Request::GroupSave { group }).await? {
        Response::Group { group } => Ok(super::super::lifecycle::restore_group(group)),
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
            .map(super::super::lifecycle::restore_agent)
            .collect()),
        _ => Err(Error::MalformedContract {
            message: "expected agents".into(),
        }
        .into()),
    }
}
