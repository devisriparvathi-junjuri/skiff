//! M1d read-only session updates: explicit fakes and selected private socket parity.
use serde_json::Value;
use skiff_client::host::{
    local::{legacy_updates, HostSelection},
    HostConnection, InventorySource, ScreenSource, StatusSource, UpdateGap, UpdateSource,
    UpdateStream,
};
use skiff_host_api::*;
use std::{
    future::Future,
    path::PathBuf,
    pin::Pin,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixListener,
    sync::mpsc,
};

mod common;

type Reply = Result<Response, Error>;
type Item = Result<SessionUpdate, UpdateGap>;
fn fixture() -> Value {
    serde_json::from_str(include_str!("../../skiff-host-api/tests/fixtures/m1d.json")).unwrap()
}
fn updates() -> Vec<SessionUpdate> {
    fixture()["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| serde_json::from_value(m["event"]["update"].clone()).unwrap())
        .collect()
}

/// Each `updates()` call takes the next queued receiver: no shared backlog.
struct Fake {
    calls: AtomicUsize,
    streams: Mutex<Vec<mpsc::UnboundedReceiver<Item>>>,
}
struct FakeStream(mpsc::UnboundedReceiver<Item>);
impl UpdateStream for FakeStream {
    fn recv(&mut self) -> Pin<Box<dyn Future<Output = Item> + Send + '_>> {
        Box::pin(async { self.0.recv().await.unwrap_or(Err(UpdateGap::Closed)) })
    }
}
impl UpdateSource for Fake {
    fn updates(&self) -> Box<dyn UpdateStream> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        Box::new(FakeStream(self.streams.lock().unwrap().remove(0)))
    }
}
impl StatusSource for Fake {
    fn status(&self) -> Pin<Box<dyn Future<Output = Reply> + Send + '_>> {
        Box::pin(async { panic!("not updates") })
    }
}
impl ScreenSource for Fake {
    fn screen(&self, _: String, _: u16) -> Pin<Box<dyn Future<Output = Reply> + Send + '_>> {
        Box::pin(async { panic!("not updates") })
    }
}
impl InventorySource for Fake {
    fn inventory(&self) -> Pin<Box<dyn Future<Output = Reply> + Send + '_>> {
        Box::pin(async { panic!("not updates") })
    }
}
fn fake(n: usize) -> (Arc<Fake>, Vec<mpsc::UnboundedSender<Item>>) {
    let (txs, rxs) = (0..n).map(|_| mpsc::unbounded_channel()).unzip();
    let fake = Arc::new(Fake {
        calls: AtomicUsize::new(0),
        streams: Mutex::new(rxs),
    });
    (fake, txs)
}
fn hello(capabilities: Vec<Capability>) -> Hello {
    common::hello("host", "generation", capabilities)
}
fn connect(source: &Arc<Fake>, hello: Hello) -> Result<HostConnection, Error> {
    HostConnection::local_updates(
        source.clone(),
        source.clone(),
        source.clone(),
        source.clone(),
        hello,
        None,
    )
}

#[tokio::test]
async fn updates_require_their_capability_and_a_source_without_fallback() {
    let (source, _txs) = fake(1);
    for caps in [
        vec![Capability::session_inventory()],
        vec![Capability { name: SESSION_UPDATES.into(), version: 2 }],
        vec![Capability { name: "sessions".into(), version: 1 }],
    ] {
        let Ok(connection) = connect(&source, hello(caps.clone())) else {
            // A family the local adapter does not know is never claimable.
            assert_eq!(caps[0].name, "sessions");
            continue;
        };
        assert!(matches!(
            connection.session_updates(),
            Err(Error::UnsupportedCapability { capability }) if capability == Capability::session_updates()
        ));
    }
    assert_eq!(source.calls.load(Ordering::Relaxed), 0);
    // An adapter without an update source cannot advertise the capability.
    assert!(matches!(
        HostConnection::local_inventory(
            source.clone(),
            source.clone(),
            source.clone(),
            hello(vec![Capability::session_updates()]),
            None
        ),
        Err(Error::MalformedContract { .. })
    ));
}

#[tokio::test]
async fn updates_keep_order_report_gaps_without_replay_and_end_on_disconnect() {
    let (source, txs) = fake(2);
    let connection = connect(&source, hello(vec![Capability::session_updates()])).unwrap();
    let mut first = connection.session_updates().unwrap();
    let mut second = legacy_updates(&connection).unwrap();
    let sent = updates();
    let malformed = Error::MalformedContract { message: "fixture".into() };
    for (i, update) in sent.iter().enumerate() {
        if i == 3 {
            txs[0].send(Err(UpdateGap::Lagged(2))).unwrap();
            txs[1].send(Err(UpdateGap::Lagged(2))).unwrap();
        }
        if i == 5 {
            txs[0].send(Err(UpdateGap::Malformed(malformed.clone()))).unwrap();
            txs[1].send(Err(UpdateGap::Malformed(malformed.clone()))).unwrap();
        }
        txs[0].send(Ok(update.clone())).unwrap();
        txs[1].send(Ok(update.clone())).unwrap();
    }
    let mut got = Vec::new();
    for _ in 0..sent.len() + 2 {
        got.push(first.recv().await);
    }
    let mut expected: Vec<Item> = sent.iter().cloned().map(Ok).collect();
    expected.insert(5, Err(UpdateGap::Malformed(malformed)));
    expected.insert(3, Err(UpdateGap::Lagged(2)));
    assert_eq!(got, expected);
    // The desktop projection skips gaps exactly like the legacy loop did and
    // restores the existing page event JSON in order.
    let legacy = fixture()["legacy"].as_array().unwrap().clone();
    for want in &legacy {
        let event = second.recv().await.unwrap();
        assert_eq!(serde_json::to_value(event).unwrap(), *want);
    }
    // Disconnect ends both receivers even with an update still queued.
    txs[0].send(Ok(sent[0].clone())).unwrap();
    txs[1].send(Ok(sent[0].clone())).unwrap();
    connection.disconnect();
    assert_eq!(first.recv().await, Err(UpdateGap::Closed));
    assert!(second.recv().await.is_none());
    assert!(matches!(connection.session_updates(), Err(Error::TransportLost)));
    assert_eq!(source.calls.load(Ordering::Relaxed), 2);
}

#[tokio::test]
async fn dropping_the_connection_ends_an_idle_receiver() {
    let (source, _txs) = fake(1);
    let connection = connect(&source, hello(vec![Capability::session_updates()])).unwrap();
    let mut updates = connection.session_updates().unwrap();
    let mut pending = Box::pin(updates.recv());
    assert!(tokio::time::timeout(Duration::from_millis(10), &mut pending).await.is_err());
    drop(connection);
    assert_eq!(pending.await, Err(UpdateGap::Closed));
}

struct Dir(PathBuf);
impl Dir {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!("u1d-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&p).unwrap();
        Self(p)
    }
}
impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[tokio::test]
async fn selected_updates_match_the_legacy_stream_on_one_client_without_requests() {
    let dir = Dir::new();
    let socket = dir.0.join("a.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let selection = HostSelection::new(socket, dir.0.join("local-host-id"));
    let (pending, peer) = tokio::join!(selection.connect(), listener.accept());
    let host = pending.unwrap().activate().await.unwrap();
    assert!(host.connection().hello().capabilities.contains(&Capability::session_updates()));
    let mut peer = peer.unwrap().0;
    // Both receivers start before any event: the pre-M1d receiver and the Host one.
    let mut legacy = host.client().events();
    let mut routed = legacy_updates(&host.connection()).unwrap();
    let mut late_output = host.client().output("a");
    let events = fixture()["legacy"].as_array().unwrap().clone();
    let mut wire = Vec::new();
    for (i, event) in events.iter().enumerate() {
        if i == 2 {
            // Output goes to its own receiver; unreadable frames are skipped by
            // the Client as before, neither reaching nor ending the stream.
            wire.extend(b"{\"event\":\"output\",\"session\":\"a\",\"data\":\"aGk=\"}\n");
            wire.extend(b"not json\n{\"event\":\"future_event\",\"session\":\"a\"}\n");
        }
        wire.extend(serde_json::to_vec(event).unwrap());
        wire.push(b'\n');
    }
    peer.write_all(&wire).await.unwrap();
    for want in &events {
        let old = legacy.recv().await.unwrap();
        let new = routed.recv().await.unwrap();
        assert_eq!(serde_json::to_value(&old).unwrap(), *want);
        assert_eq!(serde_json::to_value(&new).unwrap(), *want);
    }
    assert!(late_output.recv().await.is_ok());
    // No subscribe, ack or replay request reaches skiffd for this stream.
    let mut sent = [0u8; 1];
    assert!(tokio::time::timeout(Duration::from_millis(50), peer.read(&mut sent)).await.is_err());
    // A daemon EOF leaves the legacy Client alive and both receivers idle; only
    // dropping the selected owner (reconnect/restart/reload) ends the Host stream.
    drop(peer);
    assert!(tokio::time::timeout(Duration::from_millis(50), routed.recv()).await.is_err());
    drop(host);
    assert!(routed.recv().await.is_none());
}
