//! Service-provided Host identity: the selected local adapter adopts the
//! identity skiffd reports in `pong`, validates it on every reconnect, and
//! never infers it from the socket, peer credentials or pid. Private sockets
//! and identity paths only.
use skiff_client::host::{
    local::{legacy_attach, legacy_output, HostSelection, IdentitySource, SelectedHost},
    Continuity, OutputGap, TerminalFrame,
};
use skiff_host_api::Error;
use std::{path::PathBuf, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{UnixListener, UnixStream},
};

struct Dir(PathBuf);
impl Dir {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("skiff-id-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&dir).unwrap();
        Self(dir)
    }
}
impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

type Peer = BufReader<UnixStream>;

async fn line(peer: &mut Peer) -> serde_json::Value {
    let mut line = String::new();
    peer.read_line(&mut line).await.unwrap();
    serde_json::from_str(&line).unwrap()
}

async fn reply(peer: &mut Peer, value: serde_json::Value) {
    let mut bytes = serde_json::to_vec(&value).unwrap();
    bytes.push(b'\n');
    peer.get_mut().write_all(&bytes).await.unwrap();
}

/// Connects, answers the recorded Ping with `identity` (or none), activates.
async fn activate(
    selection: &HostSelection,
    listener: &UnixListener,
    identity: Option<serde_json::Value>,
) -> (anyhow::Result<SelectedHost>, Peer) {
    let (pending, peer) = tokio::join!(selection.connect(), listener.accept());
    let mut pending = pending.unwrap();
    let mut peer = BufReader::new(peer.unwrap().0);
    let (info, ()) = tokio::join!(pending.daemon_info(), async {
        assert_eq!(
            line(&mut peer).await,
            serde_json::json!({"id":1,"cmd":"ping"})
        );
        let mut pong =
            serde_json::json!({"id":1,"type":"pong","version":"0.6.24","protocol":27,"pid":4242});
        if let Some(identity) = identity {
            pong["identity"] = identity;
        }
        reply(&mut peer, pong).await;
    });
    assert_eq!(info.unwrap().pid, Some(4242));
    let host = pending.activate().await;
    // Activation sends nothing more: the recorded pong is the handshake.
    let mut extra = String::new();
    // (A refused activation drops its client: EOF, still no frame.)
    if let Ok(read) =
        tokio::time::timeout(Duration::from_millis(20), peer.read_line(&mut extra)).await
    {
        assert_eq!((read.unwrap(), host.is_err()), (0, true), "{extra}");
    }
    (host, peer)
}

fn identity(host: &str, instance: &str) -> serde_json::Value {
    // Not this process's account: it must come from the pong, not from here.
    serde_json::json!({"host_id":host,"instance_id":instance,"os":"fixture-os","account":"uid:fixture"})
}

#[tokio::test]
async fn reported_identity_is_adopted_and_every_reconnect_validates_it() {
    let dir = Dir::new();
    let socket = dir.0.join("a.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let bootstrap = dir.0.join("config/local-host-id");
    let selection = HostSelection::new(socket, bootstrap.clone());

    let (first, _p1) = activate(&selection, &listener, Some(identity("host-a", "run-1"))).await;
    let first = first.unwrap();
    let hello = first.connection().hello().clone();
    assert_eq!(first.identity_source(), IdentitySource::Host);
    assert_eq!(first.connection().continuity(), Continuity::FirstConnection);
    assert_eq!(
        (hello.host.host_id.as_str(), hello.instance_id.as_str()),
        ("host-a", "run-1")
    );
    assert_eq!(
        (hello.host.os.as_str(), hello.host.account.as_str()),
        ("fixture-os", "uid:fixture")
    );
    assert_eq!(
        hello.service_versions.get("skiffd").map(String::as_str),
        Some("0.6.24")
    );
    // The serving Host owns identity: the app's bootstrap store is untouched,
    // and nothing came from the peer's pid.
    assert!(!bootstrap.exists());
    assert!(!hello.instance_id.as_str().contains("4242"));
    drop(first);

    let (same, _p2) = activate(&selection, &listener, Some(identity("host-a", "run-1"))).await;
    assert_eq!(
        same.unwrap().connection().continuity(),
        Continuity::SameInstance
    );
    let (next, _p3) = activate(&selection, &listener, Some(identity("host-a", "run-2"))).await;
    assert_eq!(
        next.unwrap().connection().continuity(),
        Continuity::NewInstance
    );

    // Another Host behind the same endpoint is refused, and stays refused.
    let (other, _p4) = activate(&selection, &listener, Some(identity("host-b", "run-3"))).await;
    assert!(matches!(
        other.err().unwrap().downcast_ref::<Error>(),
        Some(Error::IdentityChanged { .. })
    ));
    let (blank, _p5) = activate(&selection, &listener, Some(identity("", "run-4"))).await;
    assert!(matches!(
        blank.err().unwrap().downcast_ref::<Error>(),
        Some(Error::MalformedContract { .. })
    ));
    let (back, _p6) = activate(&selection, &listener, Some(identity("host-a", "run-2"))).await;
    assert_eq!(
        back.unwrap().connection().continuity(),
        Continuity::SameInstance
    );
}

#[tokio::test]
async fn an_older_daemon_gets_the_bootstrap_identity_and_always_a_new_instance() {
    let dir = Dir::new();
    let socket = dir.0.join("a.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let bootstrap = dir.0.join("config/local-host-id");
    let selection = HostSelection::new(socket, bootstrap.clone());
    let (old, _p1) = activate(&selection, &listener, None).await;
    let old = old.unwrap();
    assert_eq!(old.identity_source(), IdentitySource::LocalBootstrap);
    let id = std::fs::read_to_string(&bootstrap).unwrap();
    assert_eq!(old.connection().hello().host.host_id.as_str(), id.trim());
    let (again, _p2) = activate(&selection, &listener, None).await;
    assert_eq!(
        again.unwrap().connection().continuity(),
        Continuity::NewInstance
    );
    // A reload onto a skiffd that reports the same store's id carries on;
    // the instance is the service's from now on.
    let (reloaded, _p3) = activate(&selection, &listener, Some(identity(id.trim(), "run-1"))).await;
    let reloaded = reloaded.unwrap();
    assert_eq!(reloaded.identity_source(), IdentitySource::Host);
    assert_eq!(reloaded.connection().continuity(), Continuity::NewInstance);
}

#[tokio::test]
async fn same_instance_still_takes_a_fresh_subscription_and_snapshot() {
    let dir = Dir::new();
    let socket = dir.0.join("a.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let selection = HostSelection::new(socket, dir.0.join("local-host-id"));
    let (first, _p1) = activate(&selection, &listener, Some(identity("host-a", "run-1"))).await;
    let first = first.unwrap();
    let mut old = legacy_output(&first.connection(), "s").unwrap();
    // The link drops: the old owner goes, and its receiver closes.
    drop(first);
    assert_eq!(old.recv().await, Err(OutputGap::Closed));

    let (next, mut peer) = activate(&selection, &listener, Some(identity("host-a", "run-1"))).await;
    let next = next.unwrap();
    assert_eq!(next.connection().continuity(), Continuity::SameInstance);
    let connection = next.connection();
    let mut output = legacy_output(&connection, "s").unwrap();
    let (attached, ()) = tokio::join!(legacy_attach(&connection, "s"), async {
        // A new subscribe on the new connection, never a replay of the old one.
        assert_eq!(
            line(&mut peer).await,
            serde_json::json!({"id":2,"cmd":"subscribe","session":"s"})
        );
        reply(
            &mut peer,
            serde_json::json!({"event":"snapshot","session":"s","data":"aGk=","cols":80,"rows":24}),
        )
        .await;
        reply(&mut peer, serde_json::json!({"id":2,"type":"ok"})).await;
    });
    attached.unwrap();
    assert_eq!(
        output.recv().await,
        Ok(TerminalFrame::Snapshot {
            data: b"hi".to_vec(),
            cols: 80,
            rows: 24
        })
    );
}
