//! M1b fake contract and real legacy-client parity. Private sockets only.
use skiff_client::host::{
    local::{legacy_screen, HostSelection},
    HostConnection, ScreenSource, StatusSource,
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
};

mod common;

type Reply = Result<Response, Error>;
struct Fake {
    calls: Mutex<Vec<(String, u16)>>,
    reply: Reply,
    gate: Option<tokio::sync::Semaphore>,
}
impl StatusSource for Fake {
    fn status(&self) -> Pin<Box<dyn Future<Output = Reply> + Send + '_>> {
        Box::pin(async { panic!("screen must not dispatch status or fall back") })
    }
}
impl ScreenSource for Fake {
    fn screen(
        &self,
        session: String,
        rows: u16,
    ) -> Pin<Box<dyn Future<Output = Reply> + Send + '_>> {
        Box::pin(async move {
            self.calls.lock().unwrap().push((session, rows));
            if let Some(gate) = &self.gate {
                gate.acquire().await.unwrap().forget();
            }
            self.reply.clone()
        })
    }
}
fn source(reply: Reply) -> Arc<Fake> {
    Arc::new(Fake {
        calls: Mutex::new(vec![]),
        reply,
        gate: None,
    })
}
fn hello() -> Hello {
    common::hello("screen-host", "screen-generation", vec![Capability::session_screen()])
}
fn connect(source: &Arc<Fake>, hello: Hello) -> Result<HostConnection, Error> {
    HostConnection::local_screen(source.clone(), source.clone(), hello, None)
}
fn request() -> Request {
    Request::SessionScreen {
        session: "s".into(),
        rows: 12,
    }
}

#[tokio::test]
async fn screen_capability_is_independent_and_failures_never_dispatch_or_fall_back() {
    let source = source(Ok(Response::SessionScreen { lines: vec![] }));
    let mut h = hello();
    h.api_major += 1;
    assert!(matches!(
        connect(&source, h),
        Err(Error::IncompatibleVersion { .. })
    ));
    for capabilities in [
        vec![],
        vec![Capability {
            name: SESSION_SCREEN.into(),
            version: 2,
        }],
    ] {
        let mut h = hello();
        h.capabilities = capabilities;
        assert_eq!(
            connect(&source, h).unwrap().request(request()).await,
            Err(Error::UnsupportedCapability {
                capability: Capability::session_screen()
            })
        );
    }
    let connection = connect(&source, hello()).unwrap();
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
            connection.request(request).await,
            Err(Error::UnsupportedCapability { .. })
        ));
    }
    assert!(source.calls.lock().unwrap().is_empty());
    connection.request(request()).await.unwrap();
    assert_eq!(*source.calls.lock().unwrap(), vec![("s".into(), 12)]);
}

#[tokio::test]
async fn screen_values_and_error_text_have_no_projection_or_guesswork() {
    let lines = vec!["".into(), "λ\0  ".into(), "trailing  ".into()];
    let fake = source(Ok(Response::SessionScreen {
        lines: lines.clone(),
    }));
    let connection = connect(&fake, hello()).unwrap();
    assert_eq!(
        legacy_screen(&connection, "session λ", 0).await.unwrap(),
        lines
    );
    assert_eq!(
        legacy_screen(&connection, "s", u16::MAX).await.unwrap(),
        lines
    );
    assert_eq!(
        *fake.calls.lock().unwrap(),
        vec![("session λ".into(), 0), ("s".into(), u16::MAX)]
    );
    let fake = source(Err(Error::OperationFailed {
        message: "unknown variant `screen`".into(),
    }));
    let connection = connect(&fake, hello()).unwrap();
    assert_eq!(
        connection.request(request()).await,
        Err(Error::OperationFailed {
            message: "unknown variant `screen`".into()
        })
    );
    assert_eq!(
        legacy_screen(&connection, "s", 12)
            .await
            .unwrap_err()
            .to_string(),
        "unknown variant `screen`"
    );
    let fake = source(Ok(Response::Ok));
    assert!(matches!(
        connect(&fake, hello()).unwrap().request(request()).await,
        Err(Error::MalformedContract { .. })
    ));
}

#[tokio::test]
async fn screen_fanout_above_the_host_limit_queues_without_new_busy_errors() {
    let source = Arc::new(Fake {
        calls: Mutex::new(vec![]),
        reply: Ok(Response::SessionScreen { lines: vec![] }),
        gate: Some(tokio::sync::Semaphore::new(0)),
    });
    struct Status;
    impl StatusSource for Status {
        fn status(&self) -> Pin<Box<dyn Future<Output = Reply> + Send + '_>> {
            Box::pin(async {
                Ok(Response::ConnectionStatus {
                    version: "status".into(),
                    protocol: 27,
                })
            })
        }
    }
    let mut h = hello();
    h.capabilities.push(Capability::connection_status());
    let connection =
        Arc::new(HostConnection::local_screen(Arc::new(Status), source.clone(), h, None).unwrap());
    let mut tasks = vec![];
    for _ in 0..65 {
        let connection = connection.clone();
        tasks.push(tokio::spawn(
            async move { connection.request(request()).await },
        ));
    }
    tokio::time::timeout(Duration::from_secs(2), async {
        while source.calls.lock().unwrap().len() != 63 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(
        tasks.iter().all(|t| !t.is_finished()),
        "65th read must wait, not become HostBusy"
    );
    // A full board must not consume the last slot needed by the existing
    // status refresh, otherwise its GUI version fields could disappear.
    assert_eq!(
        connection.request(Request::ConnectionStatus).await,
        Ok(Response::ConnectionStatus {
            version: "status".into(),
            protocol: 27
        })
    );
    source.gate.as_ref().unwrap().add_permits(65);
    for task in tasks {
        assert_eq!(
            task.await.unwrap(),
            Ok(Response::SessionScreen { lines: vec![] })
        );
    }
    assert_eq!(source.calls.lock().unwrap().len(), 65);
}

#[tokio::test(start_paused = true)]
async fn cancellation_disconnect_and_deadline_apply_to_waiting_and_active_reads() {
    let source = Arc::new(Fake {
        calls: Mutex::new(vec![]),
        reply: Ok(Response::SessionScreen { lines: vec![] }),
        gate: Some(tokio::sync::Semaphore::new(0)),
    });
    let mut h = hello();
    h.limits.max_in_flight = 1;
    let connection = connect(&source, h).unwrap();
    let mut first = Box::pin(connection.request(request()));
    tokio::select! { biased; _ = &mut first => panic!("completed"), _ = tokio::task::yield_now() => {} }
    let mut queued = Box::pin(connection.request(request()));
    tokio::select! { biased; _ = &mut queued => panic!("completed"), _ = tokio::task::yield_now() => {} }
    assert_eq!(source.calls.lock().unwrap().len(), 1);
    drop(first);
    tokio::select! { biased; _ = &mut queued => panic!("completed"), _ = tokio::task::yield_now() => {} }
    assert_eq!(source.calls.lock().unwrap().len(), 2);
    let mut waiting = Box::pin(connection.request(request()));
    tokio::select! { biased; _ = &mut waiting => panic!("completed"), _ = tokio::task::yield_now() => {} }
    assert_eq!(source.calls.lock().unwrap().len(), 2);
    connection.disconnect();
    assert_eq!(queued.await, Err(Error::TransportLost));
    assert_eq!(waiting.await, Err(Error::TransportLost));
    assert_eq!(
        connection.request(request()).await,
        Err(Error::TransportLost)
    );
    let connection = connect(&source, hello()).unwrap();
    assert_eq!(connection.request(request()).await, Err(Error::TimedOut));
    assert_eq!(
        legacy_screen(&connection, "s", 12)
            .await
            .unwrap_err()
            .to_string(),
        "skiffd did not answer within 20s"
    );
}

struct Dir(PathBuf);
impl Dir {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("s1b-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&dir).unwrap();
        Self(dir)
    }
}
impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[tokio::test]
async fn selected_screen_matches_legacy_requests_outputs_and_errors_on_the_same_socket() {
    let dir = Dir::new();
    let socket = dir.0.join("a.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let unused = UnixListener::bind(dir.0.join("b.sock")).unwrap();
    let selection = HostSelection::new(socket, dir.0.join("local-host-id"));
    let (pending, peer) = tokio::join!(selection.connect(), listener.accept());
    let host = pending.unwrap().activate().await.unwrap();
    let client = host.client();
    let mut peer = BufReader::new(peer.unwrap().0);
    let mut id = 0;
    for (rows, response) in [
        (0, serde_json::json!({"type":"screen","lines":[]})),
        (
            12,
            serde_json::json!({"type":"screen","lines":["", "λ\u{0}  ", "trailing  "]}),
        ),
        (
            u16::MAX,
            serde_json::json!({"type":"error","message":"unknown variant `screen`"}),
        ),
        (
            1,
            serde_json::json!({"type":"error","message":"no such session"}),
        ),
        (2, serde_json::json!({"type":"ok"})),
    ] {
        let mut results = vec![];
        for legacy in [true, false] {
            id += 1;
            let operation = async {
                if legacy {
                    client.screen("session λ", rows).await
                } else {
                    legacy_screen(&host.connection(), "session λ", rows).await
                }
                .map_err(|e| format!("{e:#}"))
            };
            let (result, ()) = tokio::join!(operation, async {
                let mut line = String::new();
                peer.read_line(&mut line).await.unwrap();
                assert_eq!(
                    serde_json::from_str::<serde_json::Value>(&line).unwrap(),
                    serde_json::json!({"id":id,"cmd":"screen","session":"session λ","rows":rows})
                );
                let mut response = response.clone();
                response["id"] = id.into();
                let mut bytes = serde_json::to_vec(&response).unwrap();
                bytes.push(b'\n');
                peer.get_mut().write_all(&bytes).await.unwrap();
            });
            results.push(result);
        }
        assert_eq!(results[0], results[1], "legacy/new parity for {response}");
    }
    assert!(Arc::ptr_eq(&client, &host.client()));
    assert!(
        tokio::time::timeout(Duration::from_millis(20), unused.accept())
            .await
            .is_err()
    );
    // A genuine open-socket stall reaches both legacy and Host deadlines.
    let connection = host.connection();
    let mut request = Box::pin(connection.request(request()));
    let mut line = String::new();
    tokio::select! { biased; result = &mut request => panic!("completed {result:?}"), _ = peer.read_line(&mut line) => {} }
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&line).unwrap()["cmd"],
        "screen"
    );
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(21)).await;
    assert_eq!(request.await, Err(Error::TimedOut));
    assert!(!client.is_closed());
    tokio::time::resume();
    drop(peer);
    let error = legacy_screen(&connection, "s", 12).await.unwrap_err();
    assert_eq!(error.to_string(), "skiffd connection closed");
}

fn malformed_screen_message() -> String {
    let line =
        serde_json::to_string(&serde_json::json!({"id":1,"type":"screen","lines":[42]})).unwrap();
    let error = serde_json::from_str::<skiff_core::protocol::ServerMessage>(&line).unwrap_err();
    format!("unreadable reply from skiffd: {error}")
}

async fn selected_screen_error_paths(response: serde_json::Value) -> (Error, String, String) {
    let dir = Dir::new();
    let socket = dir.0.join("a.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let selection = HostSelection::new(socket, dir.0.join("local-host-id"));
    let (pending, peer) = tokio::join!(selection.connect(), listener.accept());
    let host = pending.unwrap().activate().await.unwrap();
    let connection = host.connection();
    let client = host.client();
    let mut peer = BufReader::new(peer.unwrap().0);
    let mut typed = None;
    let mut messages = vec![];
    for id in 1..=3 {
        let operation = async {
            if id == 1 {
                typed = Some(
                    connection
                        .request(Request::SessionScreen {
                            session: "s".into(),
                            rows: 12,
                        })
                        .await
                        .unwrap_err(),
                );
            } else {
                let error = if id == 2 {
                    legacy_screen(&connection, "s", 12).await.unwrap_err()
                } else {
                    client.screen("s", 12).await.unwrap_err()
                };
                messages.push(format!("{error:#}"));
            }
        };
        tokio::join!(operation, async {
            let mut line = String::new();
            peer.read_line(&mut line).await.unwrap();
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&line).unwrap(),
                serde_json::json!({"id":id,"cmd":"screen","session":"s","rows":12})
            );
            let mut response = response.clone();
            response["id"] = id.into();
            let mut bytes = serde_json::to_vec(&response).unwrap();
            bytes.push(b'\n');
            peer.get_mut().write_all(&bytes).await.unwrap();
        });
    }
    (typed.unwrap(), messages.remove(0), messages.remove(0))
}

#[tokio::test]
async fn malformed_correlated_screen_data_retains_decode_provenance_and_legacy_text() {
    let (typed, projected, legacy) =
        selected_screen_error_paths(serde_json::json!({"type":"screen","lines":[42]})).await;
    let message = malformed_screen_message();
    assert_eq!(
        typed,
        Error::MalformedContract {
            message: message.clone()
        }
    );
    assert_eq!(projected, message);
    assert_eq!(legacy, message);
}

#[tokio::test]
async fn genuine_screen_error_with_identical_decode_diagnostic_text_stays_operation_failed() {
    // A daemon is allowed to send this exact text. Classification must use the
    // decoded reply/error provenance, never a string-prefix heuristic.
    let message = malformed_screen_message();
    let (typed, projected, legacy) =
        selected_screen_error_paths(serde_json::json!({"type":"error","message":message})).await;
    assert_eq!(
        typed,
        Error::OperationFailed {
            message: message.clone()
        }
    );
    assert_eq!(projected, message);
    assert_eq!(legacy, message);
}

#[tokio::test]
async fn correlated_decode_failure_is_not_overwritten_by_a_subsequent_disconnect() {
    let dir = Dir::new();
    let socket = dir.0.join("a.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let selection = HostSelection::new(socket, dir.0.join("local-host-id"));
    let (pending, peer) = tokio::join!(selection.connect(), listener.accept());
    let host = pending.unwrap().activate().await.unwrap();
    let connection = host.connection();
    let mut request = Box::pin(connection.request(Request::SessionScreen {
        session: "s".into(),
        rows: 12,
    }));
    let mut peer = BufReader::new(peer.unwrap().0);
    let mut line = String::new();
    tokio::select! { biased; result = &mut request => panic!("completed before reply: {result:?}"), _ = peer.read_line(&mut line) => {} }
    peer.get_mut()
        .write_all(b"{\"id\":1,\"type\":\"screen\",\"lines\":[42]}\n")
        .await
        .unwrap();
    drop(peer);
    // Let the reader correlate the decode error and observe EOF before the
    // Host source resumes: its completed result takes precedence over EOF.
    tokio::time::timeout(Duration::from_secs(2), async {
        while !host.client().is_closed() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        request.await,
        Err(Error::MalformedContract {
            message: malformed_screen_message()
        })
    );
}
