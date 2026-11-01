//! M1e session terminal: explicit fakes and selected private socket parity.
use serde_json::Value;
use skiff_client::host::{
    local::{
        legacy_attach, legacy_detach, legacy_input, legacy_output, legacy_resize, HostSelection,
    },
    HostConnection, LocalSources, OutputGap, OutputStream, StatusSource, TerminalFrame,
    TerminalSource,
};
use skiff_host_api::*;
use std::{
    future::Future,
    path::PathBuf,
    pin::Pin,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::UnixListener,
    sync::mpsc,
};

mod common;

type Reply = Result<Response, Error>;
type Item = Result<TerminalFrame, OutputGap>;
fn fixture() -> Value {
    serde_json::from_str(include_str!("../../skiff-host-api/tests/fixtures/m1e.json")).unwrap()
}

/// Records every call in order; each `output` takes the next queued stream.
struct Fake {
    calls: Mutex<Vec<String>>,
    streams: Mutex<Vec<mpsc::UnboundedReceiver<Item>>>,
    lost: std::sync::atomic::AtomicBool,
}
struct FakeStream(mpsc::UnboundedReceiver<Item>);
impl OutputStream for FakeStream {
    fn recv(&mut self) -> Pin<Box<dyn Future<Output = Item> + Send + '_>> {
        Box::pin(async { self.0.recv().await.unwrap_or(Err(OutputGap::Closed)) })
    }
    fn clear(&mut self) {
        while self.0.try_recv().is_ok() {}
    }
}
impl Fake {
    fn call(&self, what: String) -> Pin<Box<dyn Future<Output = Reply> + Send + '_>> {
        self.calls.lock().unwrap().push(what);
        Box::pin(async { Ok(Response::Ok) })
    }
}
impl TerminalSource for Fake {
    fn output(&self, session: &str) -> Box<dyn OutputStream> {
        self.calls.lock().unwrap().push(format!("output {session}"));
        Box::new(FakeStream(self.streams.lock().unwrap().remove(0)))
    }
    fn attach(&self, session: String) -> Pin<Box<dyn Future<Output = Reply> + Send + '_>> {
        self.call(format!("attach {session}"))
    }
    fn detach(&self, session: String) -> Pin<Box<dyn Future<Output = Reply> + Send + '_>> {
        self.call(format!("detach {session}"))
    }
    fn resize(
        &self,
        session: String,
        cols: u16,
        rows: u16,
    ) -> Pin<Box<dyn Future<Output = Reply> + Send + '_>> {
        self.call(format!("resize {session} {cols}x{rows}"))
    }
    fn input(&self, session: &str, data: Vec<u8>, paste: bool) -> Result<(), Error> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("input {session} {data:?} {paste}"));
        Ok(())
    }
}
impl StatusSource for Fake {
    fn status(&self) -> Pin<Box<dyn Future<Output = Reply> + Send + '_>> {
        Box::pin(async { panic!("not terminal") })
    }
    fn is_lost(&self) -> bool {
        self.lost.load(std::sync::atomic::Ordering::Relaxed)
    }
}
fn fake(n: usize) -> (Arc<Fake>, Vec<mpsc::UnboundedSender<Item>>) {
    let (txs, rxs) = (0..n).map(|_| mpsc::unbounded_channel()).unzip();
    let fake = Arc::new(Fake {
        calls: Mutex::new(Vec::new()),
        streams: Mutex::new(rxs),
        lost: Default::default(),
    });
    (fake, txs)
}
fn hello(capabilities: Vec<Capability>) -> Hello {
    common::hello("host", "generation", capabilities)
}
fn connect(source: &Arc<Fake>, hello: Hello, terminal: bool) -> Result<HostConnection, Error> {
    HostConnection::local(
        LocalSources {
            status: source.clone(),
            screen: None,
            inventory: None,
            updates: None,
            terminal: terminal.then(|| source.clone() as Arc<dyn TerminalSource>),
            lifecycle: None,
            groups: None,
            agents: None,
        },
        hello,
        None,
    )
}
fn attach(session: &str) -> Request {
    Request::TerminalAttach {
        session: session.into(),
    }
}

#[tokio::test]
async fn the_terminal_requires_its_capability_and_a_source_without_fallback() {
    let (source, _txs) = fake(1);
    for caps in [
        vec![Capability::connection_status()],
        vec![Capability {
            name: SESSION_TERMINAL.into(),
            version: 2,
        }],
    ] {
        let connection = connect(&source, hello(caps), true).unwrap();
        let unsupported = |e: Error| matches!(e, Error::UnsupportedCapability { capability } if capability == Capability::session_terminal());
        assert!(unsupported(connection.terminal_output("s").err().unwrap()));
        assert!(unsupported(
            connection
                .terminal_input("s", b"x".to_vec(), false)
                .unwrap_err()
        ));
        assert!(unsupported(
            connection.request(attach("s")).await.unwrap_err()
        ));
        // The desktop projection never retries through a raw Client.
        assert!(legacy_input(&connection, "s", b"x".to_vec()).is_err());
    }
    assert!(source.calls.lock().unwrap().is_empty());
    // An adapter without a terminal source cannot advertise it, and no
    // adapter claims a family it does not know.
    let unknown = || Capability { name: "sessions".into(), version: 1 };
    for caps in [vec![Capability::session_terminal()], vec![unknown()]] {
        assert!(matches!(
            connect(&source, hello(caps), false),
            Err(Error::MalformedContract { .. })
        ));
    }
    assert!(matches!(
        connect(&source, hello(vec![unknown()]), true),
        Err(Error::MalformedContract { .. })
    ));
}

#[tokio::test]
async fn output_input_and_control_keep_order_report_lag_and_never_replay() {
    let (source, txs) = fake(2);
    let connection = connect(&source, hello(vec![Capability::session_terminal()]), true).unwrap();
    // Receiver first, then attach: a snapshot queued before the reply is kept.
    let mut output = legacy_output(&connection, "s").unwrap();
    txs[0]
        .send(Ok(TerminalFrame::Snapshot {
            data: vec![0, 255],
            cols: 80,
            rows: 24,
        }))
        .unwrap();
    legacy_attach(&connection, "s").await.unwrap();
    for i in 0..3u8 {
        legacy_input(&connection, "s", vec![i]).unwrap();
    }
    connection
        .terminal_input("s", b"paste".to_vec(), true)
        .unwrap();
    legacy_resize(&connection, "s", 100, 30).await.unwrap();
    assert_eq!(
        output.recv().await,
        Ok(TerminalFrame::Snapshot {
            data: vec![0, 255],
            cols: 80,
            rows: 24
        })
    );
    let out = |d: &[u8]| {
        Ok(TerminalFrame::Output {
            data: d.to_vec(),
            echo: None,
        })
    };
    txs[0].send(out(b"a")).unwrap();
    txs[0].send(Err(OutputGap::Lagged)).unwrap();
    txs[0].send(out(b"stale")).unwrap();
    assert_eq!(output.recv().await, out(b"a"));
    // Lag is reported, never repaired by replay: the caller clears and re-attaches.
    assert_eq!(output.recv().await, Err(OutputGap::Lagged));
    output.clear();
    legacy_attach(&connection, "s").await.unwrap();
    txs[0]
        .send(Ok(TerminalFrame::Snapshot {
            data: b"new".to_vec(),
            cols: 1,
            rows: 1,
        }))
        .unwrap();
    assert!(
        matches!(output.recv().await, Ok(TerminalFrame::Snapshot { data, .. }) if data == b"new")
    );
    legacy_detach(&connection, "s").await.unwrap();
    assert_eq!(
        *source.calls.lock().unwrap(),
        [
            "output s",
            "attach s",
            "input s [0] false",
            "input s [1] false",
            "input s [2] false",
            "input s [112, 97, 115, 116, 101] true",
            "resize s 100x30",
            "attach s",
            "detach s",
        ]
    );
    // Disconnect ends a receiver even with a frame queued, and refuses later
    // input instead of holding it for a replay.
    let mut second = legacy_output(&connection, "t").unwrap();
    txs[1].send(out(b"queued")).unwrap();
    assert!(!connection.is_closed());
    connection.disconnect();
    assert!(connection.is_closed());
    assert_eq!(output.recv().await, Err(OutputGap::Closed));
    assert_eq!(second.recv().await, Err(OutputGap::Closed));
    assert_eq!(
        connection.terminal_input("s", b"late".to_vec(), false),
        Err(Error::TransportLost)
    );
    assert_eq!(
        legacy_input(&connection, "s", b"late".to_vec())
            .unwrap_err()
            .to_string(),
        "skiffd connection closed"
    );
    assert!(matches!(
        connection.terminal_output("s"),
        Err(Error::TransportLost)
    ));
    assert!(matches!(
        connection.request(attach("s")).await,
        Err(Error::TransportLost)
    ));
    assert_eq!(source.calls.lock().unwrap().len(), 10);
}

#[tokio::test]
async fn a_dropped_connection_or_a_lost_source_reads_as_closed() {
    let (source, _txs) = fake(1);
    let connection = connect(&source, hello(vec![Capability::session_terminal()]), true).unwrap();
    let mut output = connection.terminal_output("s").unwrap();
    let mut pending = Box::pin(output.recv());
    assert!(
        tokio::time::timeout(Duration::from_millis(10), &mut pending)
            .await
            .is_err()
    );
    source
        .lost
        .store(true, std::sync::atomic::Ordering::Relaxed);
    // The source's own liveness, with no round trip: the input bridge holds.
    assert!(connection.is_closed());
    drop(connection);
    assert_eq!(pending.await, Err(OutputGap::Closed));
}

struct Dir(PathBuf);
impl Dir {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!("t1e-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&p).unwrap();
        Self(p)
    }
}
impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

async fn next(rd: &mut BufReader<tokio::net::unix::OwnedReadHalf>) -> Value {
    let mut line = String::new();
    rd.read_line(&mut line).await.unwrap();
    serde_json::from_str::<Value>(&line).unwrap()
}

/// The very same skiffd frames as the pre-M1e Client calls, interleaved on
/// one selected Client, with the legacy per-session output queue.
#[tokio::test]
async fn selected_terminal_sends_exactly_the_legacy_frames_on_one_client() {
    let dir = Dir::new();
    let socket = dir.0.join("a.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let selection = HostSelection::new(socket, dir.0.join("local-host-id"));
    let (pending, peer) = tokio::join!(selection.connect(), listener.accept());
    let host = pending.unwrap().activate().await.unwrap();
    let connection = host.connection();
    let client = host.client();
    assert!(connection
        .hello()
        .capabilities
        .contains(&Capability::session_terminal()));
    let (rd, mut wr) = peer.unwrap().0.into_split();
    let mut rd = BufReader::new(rd);
    let legacy = fixture()["legacy"].clone();
    let with_id = |mut v: Value, id: u64| {
        v["id"] = id.into();
        v
    };
    let mut output = legacy_output(&connection, "s1").unwrap();
    // Legacy attach, then the Host attach, on the same id sequence.
    let (old, frame) = tokio::join!(client.subscribe("s1"), async {
        let f = next(&mut rd).await;
        wr.write_all(b"{\"id\":1,\"type\":\"ok\"}\n").await.unwrap();
        f
    });
    old.unwrap();
    assert_eq!(frame, with_id(legacy["terminal_attach"].clone(), 1));
    let (new, frame) = tokio::join!(legacy_attach(&connection, "s1"), async {
        let f = next(&mut rd).await;
        // The snapshot can precede the reply; it lands on the receiver taken first.
        wr.write_all(b"{\"event\":\"snapshot\",\"session\":\"s1\",\"data\":\"G1tIAP8=\",\"cols\":80,\"rows\":24}\n{\"id\":2,\"type\":\"ok\"}\n").await.unwrap();
        f
    });
    new.unwrap();
    assert_eq!(frame, with_id(legacy["terminal_attach"].clone(), 2));
    assert_eq!(
        output.recv().await,
        Ok(TerminalFrame::Snapshot {
            data: vec![27, 91, 72, 0, 255],
            cols: 80,
            rows: 24
        })
    );
    // Input: legacy write_now and Host input interleave in call order.
    let bytes = vec![0, 255, 10, 13, 27];
    client.write_now("s1", bytes.clone()).unwrap();
    legacy_input(&connection, "s1", bytes.clone()).unwrap();
    connection
        .terminal_input("s1", bytes.clone(), true)
        .unwrap();
    assert_eq!(
        next(&mut rd).await,
        with_id(legacy["terminal_input"].clone(), 3)
    );
    assert_eq!(
        next(&mut rd).await,
        with_id(legacy["terminal_input"].clone(), 4)
    );
    let mut paste = with_id(legacy["terminal_input"].clone(), 5);
    paste["paste"] = true.into();
    assert_eq!(next(&mut rd).await, paste);
    // Resize, with a service error passed through as the legacy text.
    let (r, frame) = tokio::join!(legacy_resize(&connection, "s1", 65535, 0), async {
        let f = next(&mut rd).await;
        wr.write_all(b"{\"id\":6,\"type\":\"error\",\"message\":\"no session s1\"}\n")
            .await
            .unwrap();
        f
    });
    assert_eq!(frame, with_id(legacy["terminal_resize"].clone(), 6));
    // Client::resize gave exactly the service message.
    assert_eq!(r.unwrap_err().to_string(), "no session s1");
    // Lag: the legacy per-session queue overflows; the gap surfaces once,
    // clear drops the backlog, and nothing is replayed.
    let mut flood = Vec::new();
    for _ in 0..5000 {
        flood.extend(b"{\"event\":\"output\",\"session\":\"s1\",\"data\":\"eA==\"}\n");
    }
    flood.extend(b"{\"event\":\"output\",\"session\":\"s1\",\"data\":\"bHMK\",\"echo\":{\"wrote\":1,\"read\":2}}\n");
    wr.write_all(&flood).await.unwrap();
    assert_eq!(output.recv().await, Err(OutputGap::Lagged));
    // Let the reader route the tail, then drop it all.
    tokio::time::sleep(Duration::from_millis(50)).await;
    output.clear();
    wr.write_all(b"{\"event\":\"output\",\"session\":\"s1\",\"data\":\"bHMK\",\"echo\":{\"wrote\":1,\"read\":2}}\n").await.unwrap();
    assert_eq!(
        output.recv().await,
        Ok(TerminalFrame::Output {
            data: b"ls\n".to_vec(),
            echo: Some(EchoTimes { wrote: 1, read: 2 })
        })
    );
    // Detach closes the receiver before its request, as unsubscribe did.
    let (d, frame) = tokio::join!(legacy_detach(&connection, "s1"), async {
        let f = next(&mut rd).await;
        wr.write_all(b"{\"id\":7,\"type\":\"ok\"}\n").await.unwrap();
        f
    });
    d.unwrap();
    assert_eq!(frame, with_id(legacy["terminal_detach"].clone(), 7));
    assert_eq!(output.recv().await, Err(OutputGap::Closed));
    // A daemon EOF closes a live receiver and marks the connection lost, so
    // the desktop bridge holds input and reconnects instead of sending it.
    let mut again = legacy_output(&connection, "s1").unwrap();
    drop(wr);
    assert_eq!(again.recv().await, Err(OutputGap::Closed));
    let until = std::time::Instant::now() + Duration::from_secs(2);
    while !connection.is_closed() {
        assert!(std::time::Instant::now() < until);
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}
