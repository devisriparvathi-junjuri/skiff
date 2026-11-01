//! M1a: private socket fixtures and fake status sources, never live user data.
use skiff_client::host::{
    local::{connection_status, HostSelection},
    Continuity, HostConnection, StatusSource,
};
use skiff_host_api::*;
use std::{
    collections::BTreeMap,
    future::{pending, Future},
    path::PathBuf,
    pin::Pin,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::UnixListener,
};

struct Dir(PathBuf);
impl Dir {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("skiff-m1a-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&dir).unwrap();
        Self(dir)
    }
}
impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn hello() -> Hello {
    Hello {
        host: Host {
            host_id: HostId::try_from("fixture-host".to_owned()).unwrap(),
            os: "fixture".into(),
            account: "fixture".into(),
        },
        instance_id: InstanceId::try_from("adapter-1".to_owned()).unwrap(),
        service_versions: BTreeMap::new(),
        api_major: API_MAJOR,
        capabilities: vec![Capability::connection_status()],
        limits: Limits {
            max_frame_bytes: 8388608,
            max_in_flight: 1,
        },
    }
}
struct Fake {
    calls: AtomicUsize,
    stall: bool,
}
impl StatusSource for Fake {
    fn status(&self) -> Pin<Box<dyn Future<Output = Result<Response, Error>> + Send + '_>> {
        Box::pin(async {
            self.calls.fetch_add(1, Ordering::Relaxed);
            if self.stall {
                pending::<()>().await;
            }
            Ok(Response::ConnectionStatus {
                version: "fake".into(),
                protocol: 27,
            })
        })
    }
}

#[tokio::test]
async fn version_capability_and_identity_errors_never_call_a_fallback_source() {
    let source = Arc::new(Fake {
        calls: AtomicUsize::new(0),
        stall: false,
    });
    let mut h = hello();
    h.api_major += 1;
    assert!(matches!(
        HostConnection::local_status(source.clone(), h, None),
        Err(Error::IncompatibleVersion { .. })
    ));
    let mut h = hello();
    h.capabilities[0].version = 2;
    let host = HostConnection::local_status(source.clone(), h, None).unwrap();
    assert!(matches!(
        connection_status(&host).await,
        Err(Error::UnsupportedCapability { .. })
    ));
    let mut h = hello();
    h.capabilities.clear();
    let host = HostConnection::local_status(source.clone(), h, None).unwrap();
    assert!(matches!(
        connection_status(&host).await,
        Err(Error::UnsupportedCapability { .. })
    ));
    let host = HostConnection::local_status(source.clone(), hello(), None).unwrap();
    assert!(matches!(
        host.request(Request::AgentList).await,
        Err(Error::UnsupportedCapability { .. })
    ));
    let mut previous = hello();
    previous.host.host_id = HostId::try_from("another-host".to_owned()).unwrap();
    assert!(matches!(
        HostConnection::local_status(source.clone(), hello(), Some(&previous)),
        Err(Error::IdentityChanged { .. })
    ));
    assert_eq!(source.calls.load(Ordering::Relaxed), 0);
    assert_eq!(connection_status(&host).await.unwrap(), ("fake".into(), 27));
    assert_eq!(source.calls.load(Ordering::Relaxed), 1);
}

#[tokio::test(start_paused = true)]
async fn local_status_is_bounded_cancellable_and_disconnect_wakes_pending_without_replay() {
    let source = Arc::new(Fake {
        calls: AtomicUsize::new(0),
        stall: true,
    });
    let host = HostConnection::local_status(source.clone(), hello(), None).unwrap();
    let mut first = Box::pin(connection_status(&host));
    tokio::select! { biased; _ = &mut first => panic!("unexpected completion"), _ = tokio::task::yield_now() => {} }
    assert_eq!(connection_status(&host).await, Err(Error::HostBusy));
    drop(first);
    let mut next = Box::pin(connection_status(&host));
    tokio::select! { biased; _ = &mut next => panic!("unexpected completion"), _ = tokio::task::yield_now() => {} }
    host.disconnect();
    assert_eq!(next.await, Err(Error::TransportLost));
    assert_eq!(connection_status(&host).await, Err(Error::TransportLost));
    assert_eq!(source.calls.load(Ordering::Relaxed), 2);
    let host = HostConnection::local_status(source.clone(), hello(), None).unwrap();
    assert_eq!(connection_status(&host).await, Err(Error::TimedOut));
}

#[tokio::test]
async fn selected_endpoint_is_the_only_status_source_and_keeps_legacy_wire_and_client() {
    let dir = Dir::new();
    let socket = dir.0.join("a.sock");
    let a = UnixListener::bind(&socket).unwrap();
    let b = UnixListener::bind(dir.0.join("b.sock")).unwrap();
    let identity = dir.0.join("config/local-host-id");
    let selection = HostSelection::new(socket.clone(), identity.clone());
    let (connection, peer) = tokio::join!(selection.connect(), a.accept());
    let pending = connection.unwrap();
    let client = pending.client().clone();
    let host = pending.activate().await.unwrap();
    assert!(Arc::ptr_eq(&client, &host.client()));
    assert_eq!(
        host.endpoint(),
        Endpoint::Local {
            socket: socket.to_str().unwrap().into()
        }
    );
    assert_eq!(host.connection().continuity(), Continuity::FirstConnection);
    assert_eq!(
        host.connection().hello().capabilities,
        vec![
            Capability::connection_status(),
            Capability::session_screen(),
            Capability::session_inventory(),
            Capability::session_updates(),
            Capability::session_terminal(),
            Capability::session_lifecycle(),
            Capability::session_groups(),
            Capability::session_agents()
        ]
    );
    // Endpoint-local lifecycle permission comes from the selected endpoint.
    assert!(host.process_control().is_some());
    let ssh = Endpoint::Ssh {
        hostname: "h".into(),
        account: "a".into(),
        port: 22,
        socket: "/s".into(),
    };
    assert!(skiff_client::host::local::ProcessControl::for_endpoint(&ssh).is_none());
    let mut peer = BufReader::new(peer.unwrap().0);
    // Activation performs no wire Hello or extra Ping.
    let mut line = String::new();
    assert!(
        tokio::time::timeout(Duration::from_millis(20), peer.read_line(&mut line))
            .await
            .is_err()
    );
    for id in 1..=2 {
        let status = async {
            if id == 1 {
                connection_status(&host.connection()).await.unwrap()
            } else {
                host.client().hello().await.unwrap()
            }
        };
        let (status, ()) = tokio::join!(status, async {
            line.clear();
            peer.read_line(&mut line).await.unwrap();
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&line).unwrap(),
                serde_json::json!({"id":id,"cmd":"ping"})
            );
            let reply = format!(
                "{{\"id\":{id},\"type\":\"pong\",\"version\":\"fixture\",\"protocol\":27}}\n"
            );
            peer.get_mut().write_all(reply.as_bytes()).await.unwrap();
        });
        assert_eq!(status, ("fixture".into(), 27));
    }
    assert!(tokio::time::timeout(Duration::from_millis(20), b.accept())
        .await
        .is_err());
    let id = host.connection().hello().host.host_id.clone();
    let instance = host.connection().hello().instance_id.clone();
    let (again, peer2) = tokio::join!(selection.connect(), a.accept());
    let next = again.unwrap().activate().await.unwrap();
    assert_eq!(next.connection().hello().host.host_id, id);
    assert_ne!(next.connection().hello().instance_id, instance);
    assert_eq!(next.connection().continuity(), Continuity::NewInstance);
    drop(peer2);
    // Reopening the app and even using another endpoint does not derive identity
    // from the locator: the same explicit identity store is authoritative.
    let reopened = HostSelection::new(dir.0.join("b.sock"), identity.clone());
    let (again, _peer) = tokio::join!(reopened.connect(), b.accept());
    assert_eq!(
        again
            .unwrap()
            .activate()
            .await
            .unwrap()
            .connection()
            .hello()
            .host
            .host_id,
        id
    );
    std::fs::write(&identity, uuid::Uuid::new_v4().to_string()).unwrap();
    let (changed, _peer) = tokio::join!(selection.connect(), a.accept());
    let error = changed.unwrap().activate().await.err().unwrap();
    assert!(matches!(
        error.downcast_ref::<Error>(),
        Some(Error::IdentityChanged { .. })
    ));
}

#[tokio::test]
async fn bootstrap_concurrency_permissions_and_invalid_identity_are_safe() {
    use std::os::unix::fs::PermissionsExt;
    let dir = Dir::new();
    let listener = UnixListener::bind(dir.0.join("a.sock")).unwrap();
    let identity = dir.0.join("config/local-host-id");
    let selection = HostSelection::new(dir.0.join("a.sock"), identity.clone());
    let mut pending = Vec::new();
    let mut peers = Vec::new();
    for _ in 0..8 {
        let (connection, peer) = tokio::join!(selection.connect(), listener.accept());
        pending.push(tokio::spawn(connection.unwrap().activate()));
        peers.push(peer.unwrap());
    }
    let mut ids = Vec::new();
    for task in pending {
        ids.push(
            task.await
                .unwrap()
                .unwrap()
                .connection()
                .hello()
                .host
                .host_id
                .clone(),
        );
    }
    assert!(ids.iter().all(|id| id == &ids[0]));
    assert_eq!(
        std::fs::metadata(&identity).unwrap().permissions().mode() & 0o777,
        0o600
    );
    std::fs::write(&identity, "invalid").unwrap();
    let (connection, _peer) = tokio::join!(selection.connect(), listener.accept());
    assert!(connection.unwrap().activate().await.is_err());
    assert_eq!(std::fs::read_to_string(&identity).unwrap(), "invalid");
    std::fs::set_permissions(&identity, std::fs::Permissions::from_mode(0o644)).unwrap();
    let (connection, _peer) = tokio::join!(selection.connect(), listener.accept());
    assert!(connection.unwrap().activate().await.is_err());
    std::fs::remove_file(&identity).unwrap();
    let target = dir.0.join("unchanged-target");
    std::fs::write(&target, "not an identity").unwrap();
    std::os::unix::fs::symlink(&target, &identity).unwrap();
    let (connection, _peer) = tokio::join!(selection.connect(), listener.accept());
    assert!(connection.unwrap().activate().await.is_err());
    assert!(std::fs::symlink_metadata(&identity)
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(std::fs::read_to_string(target).unwrap(), "not an identity");
    // No fallback connection or daemon start: an absent selected endpoint fails.
    let missing = HostSelection::new(dir.0.join("missing.sock"), dir.0.join("unused-id"));
    assert!(missing.connect().await.is_err());
    assert!(!dir.0.join("unused-id").exists());
}

#[test]
fn bare_relative_config_bootstraps_on_first_activation() {
    const CHILD: &str = "SKIFF_TEST_RELATIVE_HOST_CHILD";
    if std::env::var_os(CHILD).is_none() {
        // Isolate cwd and config environment from other parallel test cases.
        let dir = Dir::new();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "bare_relative_config_bootstraps_on_first_activation",
                "--nocapture",
            ])
            .current_dir(&dir.0)
            .env(CHILD, "1")
            .env("SKIFF_CONFIG", "projects.toml")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let config = skiff_core::config::config_path();
        assert_eq!(config, PathBuf::from("projects.toml"));
        std::fs::write(&config, "# existing config\n").unwrap();
        let identity = config.with_file_name("local-host-id");
        assert_eq!(identity.parent().unwrap(), std::path::Path::new(""));
        assert!(!identity.exists(), "must test the very first publication");
        let socket = PathBuf::from("s.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let selection = HostSelection::new(socket, identity.clone());
        let (pending, _peer) = tokio::join!(selection.connect(), listener.accept());
        let host = pending
            .unwrap()
            .activate()
            .await
            .expect("first activation must succeed, without retry");
        assert_eq!(
            std::fs::read_to_string(identity).unwrap().trim(),
            host.connection().hello().host.host_id.as_str()
        );
        assert_eq!(
            std::fs::read_to_string(config).unwrap(),
            "# existing config\n"
        );
        assert!(!std::fs::read_dir(".").unwrap().any(|entry| entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".tmp")));
    });
}

#[tokio::test]
async fn stalled_selected_legacy_ping_is_a_typed_host_timeout() {
    let dir = Dir::new();
    let socket = dir.0.join("s.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let selection = HostSelection::new(socket, dir.0.join("local-host-id"));
    let (pending, peer) = tokio::join!(selection.connect(), listener.accept());
    let host = pending.unwrap().activate().await.unwrap();
    let connection = host.connection();
    let mut request = Box::pin(connection_status(&connection));
    let mut peer = BufReader::new(peer.unwrap().0);
    let mut line = String::new();
    tokio::select! {
        biased;
        result = &mut request => panic!("status completed before Ping: {result:?}"),
        result = peer.read_line(&mut line) => { assert!(result.unwrap() > 0); }
    }
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&line).unwrap(),
        serde_json::json!({"id":1,"cmd":"ping"})
    );
    // Leave the real Unix connection open, withhold Pong, and don't poll the
    // request until BOTH its 20s deadlines have expired. Tokio polls the inner
    // source first: a Fake::status() without Client's timeout cannot catch this.
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(21)).await;
    assert_eq!(request.await, Err(Error::TimedOut));
    assert!(
        !host.client().is_closed(),
        "this is a stall, not transport loss"
    );
}
