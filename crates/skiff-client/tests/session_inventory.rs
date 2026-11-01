//! M1c lossless inventory: explicit fakes and selected private socket parity.
use serde_json::{json, Value};
use skiff_client::host::{
    local::{legacy_inventory, HostSelection},
    HostConnection, InventorySource, ScreenSource, StatusSource,
};
use skiff_host_api::*;
use std::{
    future::Future,
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

mod common;

type Reply = Result<Response, Error>;
fn fixture() -> Value {
    serde_json::from_str(include_str!("../../skiff-host-api/tests/fixtures/m1c.json")).unwrap()
}
struct Fake {
    calls: AtomicUsize,
    response: Reply,
    stall: bool,
}
impl StatusSource for Fake {
    fn status(&self) -> Pin<Box<dyn Future<Output = Reply> + Send + '_>> {
        Box::pin(async { panic!("not inventory") })
    }
}
impl ScreenSource for Fake {
    fn screen(&self, _: String, _: u16) -> Pin<Box<dyn Future<Output = Reply> + Send + '_>> {
        Box::pin(async { panic!("not inventory") })
    }
}
impl InventorySource for Fake {
    fn inventory(&self) -> Pin<Box<dyn Future<Output = Reply> + Send + '_>> {
        Box::pin(async {
            self.calls.fetch_add(1, Ordering::Relaxed);
            if self.stall {
                std::future::pending::<()>().await;
            }
            self.response.clone()
        })
    }
}
fn hello() -> Hello {
    common::hello("host", "generation", vec![Capability::session_inventory()])
}
fn connect(source: &Arc<Fake>, hello: Hello) -> Result<HostConnection, Error> {
    HostConnection::local_inventory(source.clone(), source.clone(), source.clone(), hello, None)
}

#[tokio::test]
async fn inventory_has_no_broad_sessions_permission_or_fallback_on_capability_version_errors() {
    let source = Arc::new(Fake {
        calls: AtomicUsize::new(0),
        response: Ok(Response::SessionInventory { sessions: vec![] }),
        stall: false,
    });
    let mut h = hello();
    h.api_major += 1;
    assert!(matches!(
        connect(&source, h),
        Err(Error::IncompatibleVersion { .. })
    ));
    for capabilities in [
        vec![],
        vec![Capability {
            name: SESSION_INVENTORY.into(),
            version: 2,
        }],
    ] {
        let mut h = hello();
        h.capabilities = capabilities;
        assert_eq!(
            connect(&source, h)
                .unwrap()
                .request(Request::SessionInventory)
                .await,
            Err(Error::UnsupportedCapability {
                capability: Capability::session_inventory()
            })
        );
    }
    let host = connect(&source, hello()).unwrap();
    for request in [
        Request::AgentList,
        Request::TerminalInput {
            session: "s".into(),
            data: vec![],
            paste: false,
        },
        Request::TerminalResize {
            session: "s".into(),
            cols: 1,
            rows: 1,
        },
        Request::TerminalAttach {
            session: "s".into(),
        },
        Request::TerminalDetach {
            session: "s".into(),
        },
    ] {
        assert!(matches!(
            host.request(request).await,
            Err(Error::UnsupportedCapability { .. })
        ));
    }
    assert_eq!(source.calls.load(Ordering::Relaxed), 0);
    assert!(legacy_inventory(&host).await.unwrap().is_empty());
    assert_eq!(source.calls.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn fake_inventory_projection_keeps_every_field_and_all_role_state_variants() {
    let data = fixture();
    let mut sessions = vec![];
    for role in ["agent", "task", "server", "shell"] {
        for state in ["working", "waiting", "idle", "done"] {
            let mut session = data["reply"]["sessions"][0].clone();
            session["role"] = role.into();
            session["state"] = state.into();
            sessions.push(session);
        }
    }
    // Preserve watcher unknown-vs-empty, absent optional fields, and defaults.
    sessions.push(data["legacy_minimal"].clone());
    let mut watch = data["reply"]["sessions"][0].clone();
    watch["watch"] = json!({"since":1,"pids":[],"follows":[]});
    sessions.push(watch);
    let mut watch = data["reply"]["sessions"][0].clone();
    watch["watch"] = json!({"since":1,"pids":[]});
    sessions.push(watch);
    let expected: Vec<skiff_core::session::SessionInfo> =
        serde_json::from_value(json!(sessions)).unwrap();
    let projected: Vec<InventorySession> = serde_json::from_value(json!(sessions)).unwrap();
    let source = Arc::new(Fake {
        calls: AtomicUsize::new(0),
        response: Ok(Response::SessionInventory {
            sessions: projected,
        }),
        stall: false,
    });
    assert_eq!(
        serde_json::to_value(
            legacy_inventory(&connect(&source, hello()).unwrap())
                .await
                .unwrap()
        )
        .unwrap(),
        serde_json::to_value(expected).unwrap()
    );
    let bad = Arc::new(Fake {
        calls: AtomicUsize::new(0),
        response: Ok(Response::Ok),
        stall: false,
    });
    assert!(matches!(
        connect(&bad, hello())
            .unwrap()
            .request(Request::SessionInventory)
            .await,
        Err(Error::MalformedContract { .. })
    ));
}

#[tokio::test(start_paused = true)]
async fn inventory_deadline_cancellation_and_disconnect_cover_active_and_queued_reads() {
    let source = Arc::new(Fake {
        calls: AtomicUsize::new(0),
        response: Ok(Response::Ok),
        stall: true,
    });
    let mut h = hello();
    h.limits.max_in_flight = 1;
    let connection = connect(&source, h).unwrap();
    let mut active = Box::pin(connection.request(Request::SessionInventory));
    tokio::select! { biased; _ = &mut active => panic!("completed"), _ = tokio::task::yield_now() => {} }
    let mut queued = Box::pin(connection.request(Request::SessionInventory));
    tokio::select! { biased; _ = &mut queued => panic!("completed"), _ = tokio::task::yield_now() => {} }
    assert_eq!(source.calls.load(Ordering::Relaxed), 1);
    drop(active);
    tokio::select! { biased; _ = &mut queued => panic!("completed"), _ = tokio::task::yield_now() => {} }
    assert_eq!(source.calls.load(Ordering::Relaxed), 2);
    connection.disconnect();
    assert_eq!(queued.await, Err(Error::TransportLost));
    let connection = connect(&source, hello()).unwrap();
    assert_eq!(
        connection.request(Request::SessionInventory).await,
        Err(Error::TimedOut)
    );
    assert_eq!(
        legacy_inventory(&connection).await.unwrap_err().to_string(),
        "skiffd did not answer within 20s"
    );
}

struct Dir(PathBuf);
impl Dir {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!("i1c-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&p).unwrap();
        Self(p)
    }
}
impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn success_cases() -> Vec<Value> {
    let data = fixture();
    let rich = data["reply"]["sessions"][0].clone();
    let minimal = data["legacy_minimal"].clone();
    let mut unknown = minimal.clone();
    unknown["future_optional"] = json!({"x":42});
    let mut empty_watch = rich.clone();
    empty_watch["watch"] = json!({"since":0,"pids":[],"follows":[]});
    let mut unknown_watch = rich.clone();
    unknown_watch["watch"] = json!({"since":0,"pids":[]});
    vec![
        json!([]),
        json!([
            rich.clone(),
            minimal,
            rich,
            unknown,
            empty_watch,
            unknown_watch
        ]),
    ]
}

#[tokio::test]
async fn selected_inventory_wire_fields_order_defaults_and_json_match_legacy_on_one_client() {
    let dir = Dir::new();
    let socket = dir.0.join("a.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let other = UnixListener::bind(dir.0.join("b.sock")).unwrap();
    let selection = HostSelection::new(socket, dir.0.join("local-host-id"));
    let (pending, peer) = tokio::join!(selection.connect(), listener.accept());
    let host = pending.unwrap().activate().await.unwrap();
    let client = host.client();
    let mut peer = BufReader::new(peer.unwrap().0);
    let mut id = 0;
    for sessions in success_cases() {
        let expected: Vec<skiff_core::session::SessionInfo> =
            serde_json::from_value(sessions.clone()).unwrap();
        let expected_json = serde_json::to_value(expected).unwrap();
        for path in 0..3 {
            id += 1;
            let operation = async {
                match path {
                    0 => serde_json::to_value(client.list_sessions().await.unwrap()).unwrap(),
                    1 => serde_json::to_value(legacy_inventory(&host.connection()).await.unwrap())
                        .unwrap(),
                    _ => match host
                        .connection()
                        .request(Request::SessionInventory)
                        .await
                        .unwrap()
                    {
                        Response::SessionInventory { sessions } => {
                            serde_json::to_value(sessions).unwrap()
                        }
                        _ => panic!("wrong response"),
                    },
                }
            };
            let (result, ()) = tokio::join!(operation, async {
                let mut line = String::new();
                peer.read_line(&mut line).await.unwrap();
                assert_eq!(
                    serde_json::from_str::<Value>(&line).unwrap(),
                    json!({"id":id,"cmd":"list_sessions"})
                );
                let mut bytes =
                    serde_json::to_vec(&json!({"id":id,"type":"sessions","sessions":sessions}))
                        .unwrap();
                bytes.push(b'\n');
                peer.get_mut().write_all(&bytes).await.unwrap();
            });
            assert_eq!(result, expected_json);
        }
    }
    assert!(Arc::ptr_eq(&client, &host.client()));
    assert!(
        tokio::time::timeout(Duration::from_millis(20), other.accept())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn selected_inventory_malformed_service_errors_and_legacy_error_text_are_distinct() {
    let dir = Dir::new();
    let socket = dir.0.join("a.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let selection = HostSelection::new(socket, dir.0.join("local-host-id"));
    let (pending, peer) = tokio::join!(selection.connect(), listener.accept());
    let host = pending.unwrap().activate().await.unwrap();
    let client = host.client();
    let connection = host.connection();
    let mut peer = BufReader::new(peer.unwrap().0);
    let mut id = 0;
    let mut malformed = fixture()["legacy_minimal"].clone();
    malformed["state"] = 42.into();
    let line =
        serde_json::to_string(&json!({"id":1,"type":"sessions","sessions":[malformed.clone()]}))
            .unwrap();
    let decode = serde_json::from_str::<skiff_core::protocol::ServerMessage>(&line).unwrap_err();
    let diagnostic = format!("unreadable reply from skiffd: {decode}");
    for (reply, malformed, expected_text) in [
        (
            json!({"type":"sessions","sessions":[malformed]}),
            true,
            diagnostic.clone(),
        ),
        (
            json!({"type":"error","message":diagnostic}),
            false,
            diagnostic.clone(),
        ),
        (
            json!({"type":"error","message":"unknown variant `list_sessions`"}),
            false,
            "unknown variant `list_sessions`".into(),
        ),
        (json!({"type":"ok"}), true, "unexpected reply: Ok".into()),
    ] {
        for path in 0..3 {
            id += 1;
            let operation = async {
                if path == 0 {
                    let error = connection
                        .request(Request::SessionInventory)
                        .await
                        .unwrap_err();
                    if malformed {
                        assert_eq!(
                            error,
                            Error::MalformedContract {
                                message: expected_text.clone()
                            }
                        );
                    } else {
                        assert_eq!(
                            error,
                            Error::OperationFailed {
                                message: expected_text.clone()
                            }
                        );
                    }
                } else {
                    let error = if path == 1 {
                        legacy_inventory(&connection).await.unwrap_err()
                    } else {
                        client.list_sessions().await.unwrap_err()
                    };
                    assert_eq!(format!("{error:#}"), expected_text);
                }
            };
            tokio::join!(operation, async {
                let mut line = String::new();
                peer.read_line(&mut line).await.unwrap();
                assert_eq!(
                    serde_json::from_str::<Value>(&line).unwrap(),
                    json!({"id":id,"cmd":"list_sessions"})
                );
                let mut reply = reply.clone();
                reply["id"] = id.into();
                let mut bytes = serde_json::to_vec(&reply).unwrap();
                bytes.push(b'\n');
                peer.get_mut().write_all(&bytes).await.unwrap();
            });
        }
    }
}

#[tokio::test]
async fn selected_inventory_stall_is_timeout_and_disconnect_keeps_legacy_text() {
    let dir = Dir::new();
    let socket = dir.0.join("a.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let selection = HostSelection::new(socket, dir.0.join("local-host-id"));
    let (pending, peer) = tokio::join!(selection.connect(), listener.accept());
    let host = pending.unwrap().activate().await.unwrap();
    let connection = host.connection();
    let client = host.client();
    let mut peer = BufReader::new(peer.unwrap().0);
    let mut request = Box::pin(connection.request(Request::SessionInventory));
    let mut line = String::new();
    tokio::select! { biased; r=&mut request=>panic!("completed {r:?}"), _=peer.read_line(&mut line)=>{} }
    assert_eq!(
        serde_json::from_str::<Value>(&line).unwrap(),
        json!({"id":1,"cmd":"list_sessions"})
    );
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(21)).await;
    assert_eq!(request.await, Err(Error::TimedOut));
    assert!(!client.is_closed());
    tokio::time::resume();
    drop(peer);
    assert_eq!(
        legacy_inventory(&connection).await.unwrap_err().to_string(),
        "skiffd connection closed"
    );
}

#[tokio::test]
async fn correlated_inventory_decode_error_survives_subsequent_eof() {
    let dir = Dir::new();
    let socket = dir.0.join("a.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let selection = HostSelection::new(socket, dir.0.join("local-host-id"));
    let (pending, peer) = tokio::join!(selection.connect(), listener.accept());
    let host = pending.unwrap().activate().await.unwrap();
    let connection = host.connection();
    let mut peer = BufReader::new(peer.unwrap().0);
    let mut line = String::new();
    let mut request = Box::pin(connection.request(Request::SessionInventory));
    tokio::select! { biased; r=&mut request=>panic!("completed {r:?}"), _=peer.read_line(&mut line)=>{} }
    peer.get_mut()
        .write_all(b"{\"id\":1,\"type\":\"sessions\",\"sessions\":[42]}\n")
        .await
        .unwrap();
    drop(peer);
    tokio::time::timeout(Duration::from_secs(2), async {
        while !host.client().is_closed() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(matches!(
        request.await,
        Err(Error::MalformedContract { .. })
    ));
}
