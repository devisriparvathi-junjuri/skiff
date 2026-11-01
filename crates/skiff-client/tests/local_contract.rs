//! Exercise the unchanged production Client against an isolated socket fixture.
use skiff_client::Client;
use skiff_core::protocol::{Envelope, Event, Reply, Request, Response, ServerMessage};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::UnixListener,
};

struct Socket(PathBuf);
impl Drop for Socket {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[tokio::test]
async fn production_client_keeps_ping_wire_input_order_and_snapshot_bytes() {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let dir = Socket(std::env::temp_dir().join(format!(
        "skiff-m0-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )));
    std::fs::create_dir(&dir.0).unwrap();
    let path = dir.0.join("fixture.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let (connection, accepted) = tokio::join!(Client::connect(&path), listener.accept());
    let client = connection.unwrap();
    let mut peer = BufReader::new(accepted.unwrap().0);
    let (ping, ()) = tokio::join!(client.hello(), async {
        let mut line = String::new();
        peer.read_line(&mut line).await.unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&line).unwrap(),
            serde_json::json!({"id":1,"cmd":"ping"})
        );
        let reply = ServerMessage::Reply(Reply {
            id: 1,
            response: Response::Pong {
                version: "fixture".into(),
                protocol: skiff_core::PROTOCOL,
                reload_state: None,
                pid: None,
                debug: None,
                identity: None,
            },
        });
        let mut bytes = serde_json::to_vec(&reply).unwrap();
        bytes.push(b'\n');
        peer.get_mut().write_all(&bytes).await.unwrap();
    });
    assert_eq!(ping.unwrap(), ("fixture".into(), 27));
    let mut output = client.output("s1");
    let (subscribe, ()) = tokio::join!(client.subscribe("s1"), async {
        let mut line = String::new();
        peer.read_line(&mut line).await.unwrap();
        let ask: Envelope = serde_json::from_str(&line).unwrap();
        assert!(matches!(ask.request, Request::Subscribe { .. }));
        // The snapshot intentionally precedes the subscribe acknowledgement.
        for message in [
            serde_json::json!({"event":"snapshot","session":"s1","data":"AP8KDQ==","cols":80,"rows":24}),
            serde_json::json!({"id":ask.id,"type":"ok"}),
            serde_json::json!({"event":"output","session":"s1","data":"G1s="}),
        ] {
            let mut bytes = serde_json::to_vec(&message).unwrap();
            bytes.push(b'\n');
            peer.get_mut().write_all(&bytes).await.unwrap();
        }
    });
    subscribe.unwrap();
    assert!(
        matches!(tokio::time::timeout(std::time::Duration::from_secs(1), output.recv()).await.unwrap().unwrap(), Event::Snapshot { data, .. } if data == [0,255,10,13])
    );
    assert!(
        matches!(tokio::time::timeout(std::time::Duration::from_secs(1), output.recv()).await.unwrap().unwrap(), Event::Output { data, .. } if data == [27,91])
    );
    for bytes in [vec![0, 255, 10, 13], vec![27, 91]] {
        client.write_now("s1", bytes).unwrap();
    }
    for (expected_id, bytes) in [(3, vec![0, 255, 10, 13]), (4, vec![27, 91])] {
        let mut line = String::new();
        peer.read_line(&mut line).await.unwrap();
        let ask: Envelope = serde_json::from_str(&line).unwrap();
        assert_eq!(ask.id, expected_id);
        assert!(matches!(ask.request, Request::Write { data, .. } if data == bytes));
    }
}
