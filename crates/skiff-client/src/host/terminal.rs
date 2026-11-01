//! M1e session terminal seam: one output receiver per attached session, plus
//! the control requests and ordered input of `session_terminal@1`.
use std::{future::Future, pin::Pin};

use skiff_host_api::{Error, Event, Response, TerminalFrame};
use tokio::sync::{broadcast, watch};

type Reply = Result<Response, Error>;
type Op<'a> = Pin<Box<dyn Future<Output = Reply> + Send + 'a>>;

/// An in-process terminal plane, independent of socket/core session types.
/// Take [`Self::output`] before [`Self::attach`]: the snapshot can precede
/// the attach reply.
pub trait TerminalSource: Send + Sync {
    /// The session's receiver from now on. Replaces an earlier receiver for
    /// the same session, which then closes.
    fn output(&self, session: &str) -> Box<dyn OutputStream>;
    fn attach(&self, session: String) -> Op<'_>;
    fn detach(&self, session: String) -> Op<'_>;
    fn resize(&self, session: String, cols: u16, rows: u16) -> Op<'_>;
    /// Queues input without waiting. Calls from one thread reach the session
    /// in call order. An error means this input was not queued.
    fn input(&self, session: &str, data: Vec<u8>, paste: bool) -> Result<(), Error>;
}

/// One session's frames, in the source's order.
pub trait OutputStream: Send {
    fn recv(
        &mut self,
    ) -> Pin<Box<dyn Future<Output = Result<TerminalFrame, OutputGap>> + Send + '_>>;
    /// Drops everything queued and any pending lag, before attaching again.
    fn clear(&mut self);
}

/// Why a terminal receiver returned no frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutputGap {
    /// Frames were dropped. Nothing is replayed: clear, attach again and
    /// skip output until the new snapshot.
    Lagged,
    /// The connection ended, was disconnected, or a newer receiver replaced
    /// this one. Nothing more arrives.
    Closed,
}

pub(super) enum OutputInner {
    Wire {
        session: String,
        events: broadcast::Receiver<Event>,
    },
    Local {
        stream: Box<dyn OutputStream>,
        closed: watch::Receiver<bool>,
    },
}

/// One session's terminal frames, from [`super::HostConnection::terminal_output`].
pub struct TerminalOutput(pub(super) OutputInner);
impl TerminalOutput {
    pub async fn recv(&mut self) -> Result<TerminalFrame, OutputGap> {
        match &mut self.0 {
            OutputInner::Wire { session, events } => loop {
                match events.recv().await {
                    Ok(event) => match event.into_terminal() {
                        Some((s, frame)) if s == *session => return Ok(frame),
                        // Other sessions and families have their own receivers.
                        _ => {}
                    },
                    Err(broadcast::error::RecvError::Lagged(_)) => return Err(OutputGap::Lagged),
                    Err(broadcast::error::RecvError::Closed) => return Err(OutputGap::Closed),
                }
            },
            OutputInner::Local { stream, closed } => {
                if *closed.borrow_and_update() {
                    return Err(OutputGap::Closed);
                }
                tokio::select! {
                    biased;
                    // A dropped connection ends the stream like a disconnect.
                    _ = closed.changed() => Err(OutputGap::Closed),
                    frame = stream.recv() => frame,
                }
            }
        }
    }

    /// Drops everything queued. After a lag, before attaching again, so the
    /// new snapshot has room.
    pub fn clear(&mut self) {
        match &mut self.0 {
            OutputInner::Wire { events, .. } => {
                while let Ok(_) | Err(broadcast::error::TryRecvError::Lagged(_)) = events.try_recv()
                {
                }
            }
            OutputInner::Local { stream, .. } => stream.clear(),
        }
    }
}
