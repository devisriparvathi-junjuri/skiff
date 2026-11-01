//! Only paired in-memory streams/private UnixStream pairs. No daemon or user data.
use skiff_client::{
    host::{Continuity, HostConnection},
    transport::Transport,
};
use skiff_host_api::*;
use std::{sync::Arc, time::Duration};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, ReadHalf, WriteHalf};

struct Fake(DuplexStream);
impl Transport for Fake {
    type Reader = ReadHalf<DuplexStream>;
    type Writer = WriteHalf<DuplexStream>;
    fn split(self) -> (Self::Reader, Self::Writer) {
        tokio::io::split(self.0)
    }
}
struct Peer {
    rd: BufReader<ReadHalf<DuplexStream>>,
    wr: WriteHalf<DuplexStream>,
}
impl Peer {
    async fn request(&mut self) -> Envelope {
        let mut line = String::new();
        assert!(self.rd.read_line(&mut line).await.unwrap() > 0);
        serde_json::from_str(&line).unwrap()
    }
    async fn send(&mut self, msg: Message) {
        let mut line = serde_json::to_vec(&msg).unwrap();
        line.push(b'\n');
        self.wr.write_all(&line).await.unwrap();
    }
    async fn reply(&mut self, id: u64, result: Result<Response, Error>) {
        self.send(Message::Reply { id, result }).await;
    }
}
fn hello() -> Hello {
    serde_json::from_value(serde_json::json!({
        "host_id":"host-a","instance_id":"instance-a","os":"linux","account":"me",
        "service_versions":{"skiff-host":"2.0.0"},"api_major":1,
        "capabilities":[{"name":"session_agents","version":1},{"name":"session_terminal","version":1}],
        "limits":{"max_frame_bytes":8388608,"max_in_flight":64}
    }))
    .unwrap()
}
fn pair() -> (Fake, Peer) {
    let (client, peer) = tokio::io::duplex(32 * 1024);
    let (rd, wr) = tokio::io::split(peer);
    (
        Fake(client),
        Peer {
            rd: BufReader::new(rd),
            wr,
        },
    )
}
async fn connect(h: Hello, previous: Option<&Hello>) -> (Result<HostConnection, Error>, Peer) {
    let (transport, mut peer) = pair();
    let (result, ()) = tokio::join!(
        HostConnection::connect_with_timeout(transport, previous, Duration::from_secs(1)),
        async {
            let ask = peer.request().await;
            assert_eq!(ask.request, Request::Hello);
            peer.reply(ask.id, Ok(Response::Hello { hello: h })).await;
        }
    );
    (result, peer)
}

#[tokio::test(start_paused = true)]
async fn unknown_api_or_missing_capability_never_dispatches_an_operation() {
    let mut h = hello();
    h.api_major = 99;
    let (result, _) = connect(h, None).await;
    assert!(matches!(result, Err(Error::IncompatibleVersion { .. })));
    for caps in [
        vec![],
        vec![Capability {
            name: "session_agents".into(),
            version: 2,
        }],
    ] {
        let mut h = hello();
        h.capabilities = caps;
        let (connection, mut peer) = connect(h, None).await;
        let connection = connection.unwrap();
        assert!(matches!(
            connection.request(Request::AgentList).await,
            Err(Error::UnsupportedCapability { .. })
        ));
        assert!(
            tokio::time::timeout(Duration::from_millis(10), peer.request())
                .await
                .is_err()
        );
    }
}

#[tokio::test]
async fn replies_are_correlated_out_of_order_and_duplicate_reply_cannot_complete_twice() {
    let (connection, mut peer) = connect(hello(), None).await;
    let connection = connection.unwrap();
    let (a, b, ()) = tokio::join!(
        connection.request(Request::AgentList),
        connection.request(Request::TerminalResize {
            session: "s".into(),
            cols: 80,
            rows: 24
        }),
        async {
            let a = peer.request().await;
            let b = peer.request().await;
            peer.reply(b.id, Ok(Response::Ok)).await;
            peer.reply(
                b.id,
                Err(Error::Permission {
                    message: "duplicate".into(),
                }),
            )
            .await;
            peer.reply(a.id, Ok(Response::Agents { agents: vec![] }))
                .await;
        }
    );
    assert_eq!(a, Ok(Response::Agents { agents: vec![] }));
    assert_eq!(b, Ok(Response::Ok));
}

#[tokio::test]
async fn snapshot_can_precede_subscribe_reply_and_bytes_and_writes_stay_ordered() {
    let (connection, mut peer) = connect(hello(), None).await;
    let connection = connection.unwrap();
    let mut events = connection.events().unwrap();
    let data = vec![0, 255, 13, 10];
    let (result, ()) = tokio::join!(
        connection.request(Request::TerminalAttach {
            session: "s".into()
        }),
        async {
            let ask = peer.request().await;
            peer.send(Message::Event {
                event: Event::TerminalSnapshot {
                    session: "s".into(),
                    data: data.clone(),
                    cols: 80,
                    rows: 24,
                },
            })
            .await;
            peer.send(Message::Event {
                event: Event::TerminalOutput {
                    session: "s".into(),
                    data: vec![27, 91],
                    echo: None,
                },
            })
            .await;
            peer.reply(ask.id, Ok(Response::Ok)).await;
        }
    );
    assert_eq!(result, Ok(Response::Ok));
    assert_eq!(
        events.recv().await.unwrap(),
        Event::TerminalSnapshot {
            session: "s".into(),
            data: data.clone(),
            cols: 80,
            rows: 24,
        }
    );
    assert_eq!(
        events.recv().await.unwrap(),
        Event::TerminalOutput {
            session: "s".into(),
            data: vec![27, 91],
            echo: None,
        }
    );
    for data in [data, vec![1, 2]] {
        let request = Request::TerminalInput {
            session: "s".into(),
            data,
            paste: false,
        };
        let (result, ()) = tokio::join!(connection.request(request.clone()), async {
            let ask = peer.request().await;
            assert_eq!(ask.request, request);
            peer.reply(ask.id, Ok(Response::Ok)).await;
        });
        result.unwrap();
    }
}

#[tokio::test(start_paused = true)]
async fn cancellation_stall_and_timeout_remove_pending_slots() {
    let mut h = hello();
    h.limits.max_in_flight = 1;
    let (connection, mut peer) = connect(h, None).await;
    let connection = Arc::new(connection.unwrap());
    let c = connection.clone();
    let task = tokio::spawn(async move { c.request(Request::AgentList).await });
    let cancelled = peer.request().await;
    assert_eq!(
        connection.request(Request::AgentList).await,
        Err(Error::HostBusy)
    );
    task.abort();
    let _ = task.await;
    // A late reply to the cancelled waiter is harmless, and its slot is reusable.
    peer.reply(cancelled.id, Ok(Response::Agents { agents: vec![] }))
        .await;
    let c = connection.clone();
    let stalled = tokio::spawn(async move { c.request(Request::AgentList).await });
    let timed_out = peer.request().await;
    tokio::time::advance(Duration::from_secs(2)).await;
    assert_eq!(stalled.await.unwrap(), Err(Error::TimedOut));
    peer.reply(timed_out.id, Ok(Response::Agents { agents: vec![] }))
        .await;
    let (result, ()) = tokio::join!(connection.request(Request::AgentList), async {
        let ask = peer.request().await;
        peer.reply(ask.id, Ok(Response::Agents { agents: vec![] }))
            .await;
    });
    assert!(result.is_ok());
}

#[tokio::test]
async fn disconnect_and_peer_drop_complete_waiters_and_close_events() {
    for drop_peer in [false, true] {
        let (connection, mut peer) = connect(hello(), None).await;
        let connection = connection.unwrap();
        let mut events = connection.events().unwrap();
        let (result, ()) = tokio::join!(connection.request(Request::AgentList), async {
            peer.request().await;
            if drop_peer {
                drop(peer);
            } else {
                connection.disconnect();
                connection.disconnect();
            }
        });
        assert_eq!(result, Err(Error::TransportLost));
        assert!(events.recv().await.is_err());
        assert_eq!(
            connection.request(Request::AgentList).await,
            Err(Error::TransportLost)
        );
    }
}

#[tokio::test]
async fn reconnect_validates_identity_and_instance_without_replay_or_resubscription() {
    let old = hello();
    let mut h = old.clone();
    h.host.host_id = "host-b".to_string().try_into().unwrap();
    assert!(matches!(
        connect(h, Some(&old)).await.0,
        Err(Error::IdentityChanged { .. })
    ));
    for (h, expected) in [
        (old.clone(), Continuity::SameInstance),
        (
            {
                let mut h = old.clone();
                h.instance_id = "instance-b".to_string().try_into().unwrap();
                h
            },
            Continuity::NewInstance,
        ),
    ] {
        let (connection, mut peer) = connect(h, Some(&old)).await;
        assert_eq!(connection.unwrap().continuity(), expected);
        // Dropping this connection closes it; only Hello was written.
        let mut extra = String::new();
        assert_eq!(peer.rd.read_line(&mut extra).await.unwrap(), 0);
    }
}

#[tokio::test]
async fn malformed_or_mismatched_replies_fail_explicitly() {
    for malformed in [true, false] {
        let (connection, mut peer) = connect(hello(), None).await;
        let connection = connection.unwrap();
        let (result, ()) = tokio::join!(connection.request(Request::AgentList), async {
            let ask = peer.request().await;
            if malformed {
                peer.wr.write_all(b"not json\n").await.unwrap();
            } else {
                peer.reply(ask.id, Ok(Response::Ok)).await;
            }
        });
        assert!(matches!(result, Err(Error::MalformedContract { .. })));
    }
}

#[tokio::test]
async fn bounded_events_report_lag_without_starving_control_replies() {
    let (connection, mut peer) = connect(hello(), None).await;
    let connection = connection.unwrap();
    let mut events = connection.events().unwrap();
    let (reply, ()) = tokio::join!(connection.request(Request::AgentList), async {
        let ask = peer.request().await;
        for _ in 0..300 {
            peer.send(Message::Event {
                event: Event::TerminalOutput {
                    session: "s".into(),
                    data: vec![1],
                    echo: None,
                },
            })
            .await;
        }
        peer.reply(ask.id, Ok(Response::Agents { agents: vec![] }))
            .await;
    });
    assert!(reply.is_ok());
    assert!(matches!(
        events.recv().await,
        Err(tokio::sync::broadcast::error::RecvError::Lagged(_))
    ));
}

#[tokio::test]
async fn local_unix_adapter_implements_the_same_host_contract() {
    let (local, peer) = tokio::net::UnixStream::pair().unwrap();
    let (result, ()) = tokio::join!(HostConnection::connect(local, None), async {
        let mut peer = BufReader::new(peer);
        let mut line = String::new();
        peer.read_line(&mut line).await.unwrap();
        let ask: Envelope = serde_json::from_str(&line).unwrap();
        assert_eq!(ask.request, Request::Hello);
        let mut frame = serde_json::to_vec(&Message::Reply {
            id: ask.id,
            result: Ok(Response::Hello { hello: hello() }),
        })
        .unwrap();
        frame.push(b'\n');
        peer.get_mut().write_all(&frame).await.unwrap();
    });
    assert_eq!(result.unwrap().continuity(), Continuity::FirstConnection);
}

#[tokio::test(start_paused = true)]
async fn oversized_requests_are_rejected_before_dispatch_and_transport_stalls_are_bounded() {
    let mut h = hello();
    h.limits.max_frame_bytes = 128;
    let (connection, mut peer) = connect(h, None).await;
    let connection = connection.unwrap();
    assert!(matches!(
        connection
            .request(Request::TerminalInput {
                session: "s".into(),
                data: vec![0; 256],
                paste: false
            })
            .await,
        Err(Error::MalformedContract { .. })
    ));
    assert!(
        tokio::time::timeout(Duration::from_millis(10), peer.request())
            .await
            .is_err()
    );
    // No Hello reply: connecting itself has a bounded budget and no usable client.
    let (transport, _peer) = pair();
    assert!(matches!(
        HostConnection::connect_with_timeout(transport, None, Duration::from_secs(1)).await,
        Err(Error::TimedOut)
    ));
}

#[tokio::test]
async fn service_errors_are_typed_without_fallback_or_connection_replacement() {
    let (connection, mut peer) = connect(hello(), None).await;
    let connection = connection.unwrap();
    for error in [
        Error::NotFound {
            message: "missing".into(),
        },
        Error::Permission {
            message: "denied".into(),
        },
        Error::Conflict {
            message: "changed".into(),
        },
        Error::HostBusy,
    ] {
        let (result, ()) = tokio::join!(connection.request(Request::AgentList), async {
            let ask = peer.request().await;
            assert_eq!(ask.request, Request::AgentList);
            peer.reply(ask.id, Err(error.clone())).await;
        });
        assert_eq!(result, Err(error));
    }
    assert_eq!(connection.continuity(), Continuity::FirstConnection);
}

#[tokio::test]
async fn m1a_connection_status_also_works_over_the_negotiated_transport() {
    let mut h = hello();
    h.capabilities = vec![Capability::connection_status()];
    let (connection, mut peer) = connect(h, None).await;
    let connection = connection.unwrap();
    let (response, ()) = tokio::join!(connection.request(Request::ConnectionStatus), async {
        let request = peer.request().await;
        assert_eq!(request.request, Request::ConnectionStatus);
        peer.reply(
            request.id,
            Ok(Response::ConnectionStatus {
                version: "fixture".into(),
                protocol: 27,
            }),
        )
        .await;
    });
    assert_eq!(
        response,
        Ok(Response::ConnectionStatus {
            version: "fixture".into(),
            protocol: 27
        })
    );
    assert!(matches!(
        connection.request(Request::AgentList).await,
        Err(Error::UnsupportedCapability { .. })
    ));
}

#[tokio::test]
async fn m1b_screen_works_over_transport_without_granting_sessions() {
    let mut h = hello();
    h.capabilities = vec![Capability::session_screen()];
    let (connection, mut peer) = connect(h, None).await;
    let connection = connection.unwrap();
    let request = Request::SessionScreen {
        session: "s".into(),
        rows: 0,
    };
    let (reply, ()) = tokio::join!(connection.request(request.clone()), async {
        let envelope = peer.request().await;
        assert_eq!(envelope.request, request);
        peer.reply(
            envelope.id,
            Ok(Response::SessionScreen {
                lines: vec!["λ  ".into()],
            }),
        )
        .await;
    });
    assert_eq!(
        reply,
        Ok(Response::SessionScreen {
            lines: vec!["λ  ".into()]
        })
    );
    assert!(matches!(
        connection.request(Request::AgentList).await,
        Err(Error::UnsupportedCapability { .. })
    ));
}

#[tokio::test]
async fn m1c_inventory_is_typed_and_gated_on_the_negotiated_transport() {
    let mut h = hello();
    h.capabilities = vec![Capability::session_inventory()];
    let (connection, mut peer) = connect(h, None).await;
    let connection = connection.unwrap();
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../../skiff-host-api/tests/fixtures/m1c.json")).unwrap();
    let response: Response = serde_json::from_value(fixture["reply"].clone()).unwrap();
    let (reply, ()) = tokio::join!(connection.request(Request::SessionInventory), async {
        let envelope = peer.request().await;
        assert_eq!(envelope.request, Request::SessionInventory);
        peer.reply(envelope.id, Ok(response.clone())).await;
    });
    assert_eq!(reply, Ok(response));
    assert!(matches!(
        connection.request(Request::AgentList).await,
        Err(Error::UnsupportedCapability { .. })
    ));
}

#[tokio::test]
async fn m1d_updates_are_gated_ordered_and_closed_on_the_negotiated_transport() {
    let (connection, _peer) = connect(hello(), None).await;
    assert!(matches!(
        connection.unwrap().session_updates(),
        Err(Error::UnsupportedCapability { .. })
    ));
    let mut h = hello();
    h.capabilities = vec![Capability::session_updates()];
    let (connection, mut peer) = connect(h, None).await;
    let connection = connection.unwrap();
    let mut updates = connection.session_updates().unwrap();
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../../skiff-host-api/tests/fixtures/m1d.json")).unwrap();
    let messages: Vec<Message> = serde_json::from_value(fixture["messages"].clone()).unwrap();
    // Output belongs to another receiver and is not a session update.
    peer.send(Message::Event {
        event: Event::TerminalOutput {
            session: "a".into(),
            data: vec![0xff],
            echo: None,
        },
    })
    .await;
    for message in &messages {
        peer.send(message.clone()).await;
    }
    for message in messages {
        let Message::Event {
            event: Event::SessionUpdate { update },
        } = message
        else {
            unreachable!()
        };
        assert_eq!(updates.recv().await, Ok(*update));
    }
    // A malformed frame fails the connection explicitly; the stream ends.
    peer.wr.write_all(b"{\"type\":\"event\",\"event\":{\"event\":\"session_update\",\"update\":{\"kind\":\"future\"}}}\n").await.unwrap();
    assert_eq!(
        updates.recv().await,
        Err(skiff_client::host::UpdateGap::Closed)
    );
    assert!(matches!(
        connection.session_updates(),
        Err(Error::MalformedContract { .. })
    ));
}

#[tokio::test]
async fn m1e_terminal_is_gated_ordered_and_lag_reporting_on_the_negotiated_transport() {
    use skiff_client::host::{OutputGap, TerminalFrame};
    // Another family alone does not grant the terminal family; nothing is sent.
    let mut h = hello();
    h.capabilities = vec![Capability::session_agents()];
    let (connection, mut peer) = connect(h, None).await;
    let connection = connection.unwrap();
    assert!(matches!(
        connection.terminal_output("s1"),
        Err(Error::UnsupportedCapability { .. })
    ));
    assert!(matches!(
        connection.terminal_input("s1", b"x".to_vec(), false),
        Err(Error::UnsupportedCapability { .. })
    ));
    assert!(matches!(
        connection
            .request(Request::TerminalAttach {
                session: "s1".into()
            })
            .await,
        Err(Error::UnsupportedCapability { .. })
    ));
    let mut line = String::new();
    assert!(
        tokio::time::timeout(Duration::from_millis(20), peer.rd.read_line(&mut line))
            .await
            .is_err()
    );

    let mut h = hello();
    h.capabilities = vec![Capability::session_terminal()];
    let (connection, mut peer) = connect(h, None).await;
    let connection = connection.unwrap();
    let mut output = connection.terminal_output("s1").unwrap();
    let (attached, ()) = tokio::join!(
        connection.request(Request::TerminalAttach {
            session: "s1".into()
        }),
        async {
            let ask = peer.request().await;
            assert_eq!(
                ask.request,
                Request::TerminalAttach {
                    session: "s1".into()
                }
            );
            // Snapshot before the reply; other sessions are skipped.
            for event in [
                Event::TerminalSnapshot {
                    session: "s1".into(),
                    data: vec![0, 255],
                    cols: 80,
                    rows: 24,
                },
                Event::TerminalOutput {
                    session: "s2".into(),
                    data: b"other".to_vec(),
                    echo: None,
                },
                Event::TerminalOutput {
                    session: "s1".into(),
                    data: vec![10, 13],
                    echo: Some(EchoTimes { wrote: 1, read: 2 }),
                },
            ] {
                peer.send(Message::Event { event }).await;
            }
            peer.reply(ask.id, Ok(Response::Ok)).await;
        }
    );
    assert_eq!(attached, Ok(Response::Ok));
    assert_eq!(
        output.recv().await,
        Ok(TerminalFrame::Snapshot {
            data: vec![0, 255],
            cols: 80,
            rows: 24
        })
    );
    assert_eq!(
        output.recv().await,
        Ok(TerminalFrame::Output {
            data: vec![10, 13],
            echo: Some(EchoTimes { wrote: 1, read: 2 })
        })
    );
    // Input is queued without waiting for replies, in call order, on the same id sequence.
    for i in 0..3u8 {
        connection.terminal_input("s1", vec![i], i == 2).unwrap();
    }
    let mut ids = Vec::new();
    for i in 0..3u8 {
        let ask = peer.request().await;
        assert_eq!(
            ask.request,
            Request::TerminalInput {
                session: "s1".into(),
                data: vec![i],
                paste: i == 2
            }
        );
        ids.push(ask.id);
    }
    assert!(ids.windows(2).all(|w| w[1] == w[0] + 1));
    // A late or failed input reply has no waiter and changes nothing.
    peer.reply(
        ids[0],
        Err(Error::OperationFailed {
            message: "gone".into(),
        }),
    )
    .await;
    let (resized, ()) = tokio::join!(
        connection.request(Request::TerminalResize {
            session: "s1".into(),
            cols: 100,
            rows: 30
        }),
        async {
            let ask = peer.request().await;
            assert!(ask.id > ids[2]);
            peer.reply(ask.id, Ok(Response::Ok)).await;
        }
    );
    assert_eq!(resized, Ok(Response::Ok));
    // A bounded receiver that falls behind reports lag; clear drops the backlog.
    for _ in 0..300 {
        peer.send(Message::Event {
            event: Event::TerminalOutput {
                session: "s1".into(),
                data: b"x".to_vec(),
                echo: None,
            },
        })
        .await;
    }
    let ping = connection.request(Request::TerminalDetach {
        session: "s9".into(),
    });
    let (_, ()) = tokio::join!(ping, async {
        let ask = peer.request().await;
        peer.reply(ask.id, Ok(Response::Ok)).await;
    });
    assert_eq!(output.recv().await, Err(OutputGap::Lagged));
    output.clear();
    // Disconnect ends the receiver and refuses input; nothing is replayed.
    connection.disconnect();
    assert_eq!(output.recv().await, Err(OutputGap::Closed));
    assert!(connection.is_closed());
    assert_eq!(
        connection.terminal_input("s1", b"late".to_vec(), false),
        Err(Error::TransportLost)
    );
}

#[tokio::test(start_paused = true)]
async fn m1g_lifecycle_and_groups_are_typed_and_separately_gated_on_the_negotiated_transport() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../../skiff-host-api/tests/fixtures/m1g.json")).unwrap();
    let mut h = hello();
    h.capabilities = vec![Capability::session_lifecycle()];
    let (connection, mut peer) = connect(h, None).await;
    let connection = connection.unwrap();
    let row: Response =
        serde_json::from_value(fixture["replies"][0]["result"]["Ok"].clone()).unwrap();
    for value in fixture["lifecycle_requests"].as_array().unwrap() {
        let request: Request = serde_json::from_value(value.clone()).unwrap();
        let answer = match request {
            Request::SessionKill { .. } | Request::SessionSeen { .. } => Response::Ok,
            _ => row.clone(),
        };
        let (reply, ()) = tokio::join!(connection.request(request.clone()), async {
            let envelope = peer.request().await;
            assert_eq!(envelope.request, request);
            peer.reply(envelope.id, Ok(answer.clone())).await;
        });
        assert_eq!(reply, Ok(answer));
    }
    // A service rejection stays typed; nothing is retried.
    let kill = Request::SessionKill {
        session: "s1".into(),
    };
    let (reply, ()) = tokio::join!(connection.request(kill.clone()), async {
        let envelope = peer.request().await;
        peer.reply(
            envelope.id,
            Err(Error::OperationFailed {
                message: "no such session".into(),
            }),
        )
        .await;
    });
    assert_eq!(
        reply,
        Err(Error::OperationFailed {
            message: "no such session".into()
        })
    );
    // A row where Ok belongs, or Ok where a row belongs, is malformed.
    let (reply, ()) = tokio::join!(connection.request(kill), async {
        let envelope = peer.request().await;
        peer.reply(envelope.id, Ok(row.clone())).await;
    });
    assert!(matches!(reply, Err(Error::MalformedContract { .. })));
    // Lifecycle grants no groups, and nothing is sent for them.
    for request in [
        Request::GroupList,
        Request::GroupDelete { group: "g".into() },
    ] {
        assert_eq!(
            connection.request(request).await,
            Err(Error::UnsupportedCapability {
                capability: Capability::session_groups()
            })
        );
    }

    let mut h = hello();
    h.capabilities = vec![Capability::session_groups()];
    let (connection, mut peer) = connect(h, None).await;
    let connection = connection.unwrap();
    let save: Request = serde_json::from_value(fixture["group_requests"][1].clone()).unwrap();
    let stored: Response =
        serde_json::from_value(fixture["replies"][2]["result"]["Ok"].clone()).unwrap();
    let (reply, ()) = tokio::join!(connection.request(save.clone()), async {
        let envelope = peer.request().await;
        assert_eq!(envelope.request, save);
        peer.reply(envelope.id, Ok(stored.clone())).await;
    });
    assert_eq!(reply, Ok(stored));
    assert_eq!(
        connection
            .request(Request::SessionKill {
                session: "s1".into()
            })
            .await,
        Err(Error::UnsupportedCapability {
            capability: Capability::session_lifecycle()
        })
    );
}
