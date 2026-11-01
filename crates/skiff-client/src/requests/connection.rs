use crate::Client;
use anyhow::{bail, Result};
use skiff_core::protocol::{Request, Response};
use std::path::PathBuf;

/// A reload the daemon refused. The old daemon answered, so it did not exec
/// and carries on as before.
#[derive(Debug)]
pub struct Refused(pub String);

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Refused {}

/// What a daemon says about itself in its `pong`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonInfo {
    pub version: String,
    pub protocol: u32,
    /// The handover version it writes on reload. `None`: it cannot reload.
    pub reload_state: Option<u32>,
    /// Its process id, which a reload keeps. `None` from older daemons.
    pub pid: Option<u32>,
    /// A debug build. `None` from older daemons.
    pub debug: Option<bool>,
    /// The Host it serves and this run of it. `None` from older daemons.
    pub identity: Option<skiff_core::protocol::Identity>,
}

impl Client {
    pub async fn ping(&self) -> Result<String> {
        Ok(self.hello().await?.0)
    }

    /// Daemon version and protocol number.
    pub async fn hello(&self) -> Result<(String, u32)> {
        let info = self.daemon_info().await?;
        Ok((info.version, info.protocol))
    }

    /// Version, protocol, reload support and pid.
    pub async fn daemon_info(&self) -> Result<DaemonInfo> {
        match self.request(Request::Ping).await? {
            Response::Pong { version, protocol, reload_state, pid, debug, identity } => {
                Ok(DaemonInfo { version, protocol, reload_state, pid, debug, identity })
            }
            other => bail!("unexpected reply: {other:?}"),
        }
    }

    /// Asks the daemon to move onto `binary` (default: its own executable)
    /// and keep its sessions. Send it on the connection that carried the
    /// writes it must keep. An error is the old daemon's: it did not reload
    /// and carries on: it is a [`Refused`]. Any other error leaves the outcome
    /// open. `Ok` means only that the connection closed, as exec closes it.
    /// Confirm on a new connection: the same `pid`, and each session with the
    /// same id and pid. It waits as long as the daemon's reload takes, with
    /// no reply timeout: a timeout would leave the old daemon mid-reload and
    /// free to exec later. Bound it at the caller.
    pub async fn reload(&self, binary: Option<PathBuf>) -> Result<()> {
        match self.request_within(Request::Reload { binary }, None).await {
            Ok(Response::Error { message }) => Err(Refused(message).into()),
            Ok(other) => bail!("unexpected reply: {other:?}"),
            Err(_) if self.is_closed() => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// Asks the daemon to stop. It ends every session. `Ok` once it agreed,
    /// or closed the connection as it exited.
    pub async fn shutdown(&self) -> Result<()> {
        match self.request(Request::Shutdown).await {
            Ok(Response::Ok) => Ok(()),
            Ok(Response::Error { message }) => bail!("{message}"),
            Ok(other) => bail!("unexpected reply: {other:?}"),
            Err(_) if self.is_closed() => Ok(()),
            Err(e) => Err(e),
        }
    }
}
