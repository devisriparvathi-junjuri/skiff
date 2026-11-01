//! Connects to `skiffd`, sends requests, and fans events out to subscribers.

use std::{
    collections::HashMap,
    path::Path,
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        Arc, Mutex,
    },
};

use anyhow::{anyhow, bail, Context, Result};
use skiff_core::{
    protocol::{Envelope, Event, Request, Response, ServerMessage},
    session::SessionId,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
    sync::{broadcast, mpsc, oneshot},
};

pub mod agent_folder;
pub mod cache;
pub mod disk;
pub mod fleet;
pub mod host;
pub mod ledger;
mod requests;
pub mod skipper;
pub mod transport;
pub mod update;
pub use requests::{Coordinated, Coordinator, DaemonInfo, Refused};

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Response, ReplyDecodeError>>>>>;
type Outputs = Arc<Mutex<HashMap<SessionId, OutputTx>>>;

/// Removes the reply slot when a caller drops a request before it completes.
struct PendingRequest<'a> {
    pending: &'a Pending,
    id: u64,
}

impl Drop for PendingRequest<'_> {
    fn drop(&mut self) {
        self.pending.lock().unwrap().remove(&self.id);
    }
}

/// Output chunks a session's receiver may hold before it counts as lagged.
const OUTPUT_QUEUE: usize = 4096;

struct OutputTx {
    tx: mpsc::Sender<Event>,
    lagged: Arc<AtomicBool>,
}

/// Output and snapshots for one session, from [`Client::output`].
pub struct Output {
    rx: mpsc::Receiver<Event>,
    lagged: Arc<AtomicBool>,
}

impl Output {
    /// The next [`Event::Output`] or [`Event::Snapshot`]. `Lagged` means
    /// chunks were dropped: subscribe again for a fresh snapshot. `Closed`
    /// means the connection is gone or a newer receiver replaced this one.
    pub async fn recv(&mut self) -> Result<Event, broadcast::error::RecvError> {
        if self.lagged.swap(false, Ordering::Relaxed) {
            return Err(broadcast::error::RecvError::Lagged(0));
        }
        self.rx.recv().await.ok_or(broadcast::error::RecvError::Closed)
    }

    /// Drops everything queued. After a lag, before subscribing again, so the
    /// new snapshot has room.
    pub fn clear(&mut self) {
        while self.rx.try_recv().is_ok() {}
        self.lagged.store(false, Ordering::Relaxed);
    }
}

/// Hands output to its session's receiver alone, without a copy per listener.
/// Never waits: a full queue marks the receiver lagged and drops the chunk.
fn route(outputs: &Outputs, e: Event) {
    let session = match &e {
        Event::Output { session, .. } | Event::Snapshot { session, .. } => session.clone(),
        _ => return,
    };
    let mut map = outputs.lock().unwrap();
    let Some(o) = map.get(&session) else { return };
    match o.tx.try_send(e) {
        Ok(()) => {}
        Err(mpsc::error::TrySendError::Full(_)) => o.lagged.store(true, Ordering::Relaxed),
        Err(mpsc::error::TrySendError::Closed(_)) => {
            map.remove(&session);
        }
    }
}

/// A request with no reply after this long fails instead of hanging.
const REPLY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

/// Preserve timeout identity for the Host adapter without parsing legacy copy.
#[derive(Debug)]
struct ReplyTimeout(std::time::Duration);
impl std::fmt::Display for ReplyTimeout {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "skiffd did not answer within {}s", self.0.as_secs())
    }
}
impl std::error::Error for ReplyTimeout {}

/// A correlated frame rejected by the reader, not a daemon Error response.
/// Keep one Display message so the legacy Tauri diagnostic remains identical.
#[derive(Debug)]
struct ReplyDecodeError(String);
impl std::fmt::Display for ReplyDecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for ReplyDecodeError {}

/// Input refused because the connection's bounded queue is full. Nothing of
/// it was sent, and nothing queues it for later.
#[derive(Debug)]
pub struct InputBusy(pub usize);
impl std::fmt::Display for InputBusy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "input refused: more than {} bytes wait to reach skiffd", self.0)
    }
}
impl std::error::Error for InputBusy {}

pub struct Client {
    /// The daemon's pid, from the socket's peer credentials.
    pub daemon_pid: Option<i32>,
    /// Unbounded, so a keystroke is queued without a wait, in order.
    tx: mpsc::UnboundedSender<Vec<u8>>,
    next_id: AtomicU64,
    pending: Pending,
    /// Every event except output and snapshots, which go to [`Self::output`].
    events: broadcast::Sender<Event>,
    outputs: Outputs,
    /// Set when the daemon closes the connection.
    closed: Arc<AtomicBool>,
    /// Bytes queued for the socket and not yet written.
    queued: Arc<AtomicUsize>,
    /// Most bytes input may queue; 0 means no limit, as a local socket has.
    input_limit: AtomicUsize,
}

impl Client {
    /// Must be called from inside a tokio runtime.
    pub async fn connect(path: &Path) -> Result<Self> {
        let stream = UnixStream::connect(path)
            .await
            .with_context(|| format!("connect {}", path.display()))?;
        let daemon_pid = stream.peer_cred().ok().and_then(|c| c.pid());
        let (rd, mut wr) = transport::Transport::split(stream);

        let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let queued = Arc::new(AtomicUsize::new(0));
        {
            let queued = queued.clone();
            tokio::spawn(async move {
                while let Some(line) = rx.recv().await {
                    if wr.write_all(&line).await.is_err() {
                        break;
                    }
                    queued.fetch_sub(line.len(), Ordering::Relaxed);
                }
            });
        }

        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let (events, _) = broadcast::channel(4096);
        let closed = Arc::new(AtomicBool::new(false));
        let outputs: Outputs = Arc::new(Mutex::new(HashMap::new()));
        {
            let pending = pending.clone();
            let outputs = outputs.clone();
            let events = events.clone();
            let closed = closed.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(rd).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    match serde_json::from_str::<ServerMessage>(&line) {
                        Ok(ServerMessage::Reply(r)) => {
                            if let Some(s) = pending.lock().unwrap().remove(&r.id) {
                                let _ = s.send(Ok(r.response));
                            }
                        }
                        Ok(ServerMessage::Event(e @ (Event::Output { .. } | Event::Snapshot { .. }))) => {
                            route(&outputs, e);
                        }
                        Ok(ServerMessage::Event(e)) => {
                            let _ = events.send(e);
                        }
                        // A newer daemon can send a reply or event this client does not know.
                        // Skip it, but fail the request it answers instead of waiting out the timeout.
                        Err(e) => {
                            tracing::warn!("bad message from skiffd: {e}");
                            let id = serde_json::from_str::<serde_json::Value>(&line)
                                .ok()
                                .and_then(|v| v.get("id").and_then(|i| i.as_u64()));
                            if let Some(s) = id.and_then(|id| pending.lock().unwrap().remove(&id)) {
                                let _ = s.send(Err(ReplyDecodeError(format!("unreadable reply from skiffd: {e}"))));
                            }
                        }
                    }
                }
                closed.store(true, Ordering::Relaxed);
                // Dropping the senders wakes every waiting request with an error.
                pending.lock().unwrap().clear();
                outputs.lock().unwrap().clear();
            });
        }

        Ok(Self {
            daemon_pid,
            tx,
            next_id: AtomicU64::new(1),
            pending,
            events,
            outputs,
            closed,
            queued,
            input_limit: AtomicUsize::new(0),
        })
    }

    /// Refuse input once `bytes` wait to be written. For a slow link, such as
    /// an SSH forward: input is refused there, never held for a retry.
    pub fn limit_input(&self, bytes: usize) {
        self.input_limit.store(bytes, Ordering::Relaxed);
    }

    /// Queues one frame, and counts it until the writer has written it.
    fn send_line(&self, line: Vec<u8>) -> bool {
        let n = line.len();
        self.queued.fetch_add(n, Ordering::Relaxed);
        if self.tx.send(line).is_err() {
            self.queued.fetch_sub(n, Ordering::Relaxed);
            return false;
        }
        true
    }

    /// True once the daemon has closed the connection. No round trip.
    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Relaxed) || self.tx.is_closed()
    }

    pub fn events(&self) -> broadcast::Receiver<Event> {
        self.events.subscribe()
    }

    pub async fn request(&self, request: Request) -> Result<Response> {
        self.request_within(request, Some(REPLY_TIMEOUT)).await
    }

    /// [`Self::request`] with its own reply timeout. `None` waits until the
    /// daemon answers or closes the connection.
    pub(crate) async fn request_within(&self, request: Request, timeout: Option<std::time::Duration>) -> Result<Response> {
        // Preserve the existing public legacy result shape for every caller.
        // Only the Host adapter opts into decode-failure provenance below.
        match self.request_decoded_within(request, timeout).await {
            Err(e) if e.downcast_ref::<ReplyDecodeError>().is_some() => {
                Ok(Response::Error { message: e.to_string() })
            }
            result => result,
        }
    }

    pub(crate) async fn request_decoded(&self, request: Request) -> Result<Response> {
        self.request_decoded_within(request, Some(REPLY_TIMEOUT)).await
    }

    async fn request_decoded_within(&self, request: Request, timeout: Option<std::time::Duration>) -> Result<Response> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (s, r) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, s);
        let _pending = PendingRequest { pending: &self.pending, id };
        // After the reader cleared `pending`, nothing would answer. Checked
        // after the insert: the reader sets `closed` before it clears.
        if self.closed.load(Ordering::Relaxed) {
            bail!("skiffd connection closed");
        }
        let mut line = serde_json::to_vec(&Envelope { id, request })?;
        line.push(b'\n');
        if !self.send_line(line) {
            bail!("skiffd connection closed");
        }
        let decoded = match timeout {
            None => r.await.map_err(|_| anyhow!("skiffd connection closed"))?,
            Some(limit) => match tokio::time::timeout(limit, r).await {
                Ok(reply) => reply.map_err(|_| anyhow!("skiffd connection closed"))?,
                Err(_) => return Err(ReplyTimeout(limit).into()),
            },
        };
        decoded.map_err(Into::into)
    }

    async fn expect_ok(&self, request: Request) -> Result<()> {
        match self.request(request).await? {
            Response::Ok => Ok(()),
            Response::Error { message } => bail!(message),
            other => bail!("unexpected reply: {other:?}"),
        }
    }

    /// The receiver for one session's output. Take it before [`Self::subscribe`]:
    /// the snapshot can arrive before the reply. Replaces an earlier receiver.
    pub fn output(&self, session: &str) -> Output {
        let (tx, rx) = mpsc::channel(OUTPUT_QUEUE);
        let lagged = Arc::new(AtomicBool::new(false));
        let o = OutputTx { tx, lagged: lagged.clone() };
        self.outputs.lock().unwrap().insert(session.to_string(), o);
        Output { rx, lagged }
    }

}

#[cfg(test)]
mod tests {
    use super::*;

    fn client() -> (Client, mpsc::UnboundedReceiver<Vec<u8>>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let (events, _) = broadcast::channel(1);
        (Client {
            daemon_pid: None,
            tx,
            next_id: AtomicU64::new(1),
            pending: Arc::new(Mutex::new(HashMap::new())),
            events,
            outputs: Arc::new(Mutex::new(HashMap::new())),
            closed: Arc::new(AtomicBool::new(false)),
            queued: Arc::new(AtomicUsize::new(0)),
            input_limit: AtomicUsize::new(0),
        }, rx)
    }

    #[tokio::test]
    async fn cancelled_requests_release_their_reply_slots() {
        let (client, mut sent) = client();
        for timeout in [Some(REPLY_TIMEOUT), None] {
            let mut request = Box::pin(client.request_within(Request::Ping, timeout));
            tokio::select! {
                result = &mut request => panic!("request finished before its reply: {result:?}"),
                line = sent.recv() => assert!(line.is_some()),
            }
            assert_eq!(client.pending.lock().unwrap().len(), 1);
            drop(request);
            assert!(client.pending.lock().unwrap().is_empty());
        }
        // A late reply has no waiter; another request still gets its own reply.
        let mut request = Box::pin(client.request(Request::Ping));
        let line = tokio::select! {
            result = &mut request => panic!("request finished before its reply: {result:?}"),
            line = sent.recv() => line.unwrap(),
        };
        let envelope: Envelope = serde_json::from_slice(&line).unwrap();
        assert!(client.pending.lock().unwrap().remove(&(envelope.id - 1)).is_none());
        client.pending.lock().unwrap().remove(&envelope.id).unwrap().send(Ok(Response::Ok)).unwrap();
        assert!(matches!(request.await.unwrap(), Response::Ok));
        assert!(client.pending.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn correlated_decode_errors_preserve_internal_provenance_and_public_legacy_shape() {
        let (client, mut sent) = client();
        for typed in [false, true] {
            for timeout in [Some(REPLY_TIMEOUT), None] {
                let mut request = Box::pin(async {
                    if typed { client.request_decoded_within(Request::Ping, timeout).await }
                    else { client.request_within(Request::Ping, timeout).await }
                });
                let line = tokio::select! {
                    result = &mut request => panic!("finished before reply: {result:?}"),
                    line = sent.recv() => line.unwrap(),
                };
                let envelope: Envelope = serde_json::from_slice(&line).unwrap();
                client.pending.lock().unwrap().remove(&envelope.id).unwrap()
                    .send(Err(ReplyDecodeError("unreadable reply from skiffd: fixture".into()))).unwrap();
                let result = request.await;
                if typed {
                    assert!(result.unwrap_err().downcast_ref::<ReplyDecodeError>().is_some());
                } else {
                    assert!(matches!(result.unwrap(), Response::Error { message } if message == "unreadable reply from skiffd: fixture"));
                }
                assert!(client.pending.lock().unwrap().is_empty());
            }
        }
    }

    #[tokio::test]
    async fn timed_out_and_closed_requests_release_their_reply_slots() {
        let (client, sent) = client();
        let timeout = client.request_within(Request::Ping, Some(std::time::Duration::ZERO)).await.unwrap_err();
        assert!(timeout.downcast_ref::<ReplyTimeout>().is_some());
        assert_eq!(format!("{timeout:#}"), "skiffd did not answer within 0s");
        assert!(client.pending.lock().unwrap().is_empty());
        drop(sent);
        assert!(client.request(Request::Ping).await.is_err());
        assert!(client.pending.lock().unwrap().is_empty());
        client.closed.store(true, Ordering::Relaxed);
        assert!(client.request(Request::Ping).await.is_err());
        assert!(client.pending.lock().unwrap().is_empty());
    }
}
