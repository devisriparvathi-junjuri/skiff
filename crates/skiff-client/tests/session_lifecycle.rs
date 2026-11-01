//! M1g session lifecycle and groups: selected private socket parity with the
//! legacy Client (exact frames, results and error text), and explicit fakes
//! for gating and bounds. No daemon or user data.
use serde_json::{json, Value};
use skiff_client::host::{
    local::{
        legacy_agents, legacy_create, legacy_delete_group, legacy_groups, legacy_kill,
        legacy_rename,
        legacy_save_group, legacy_seen, legacy_set_theme, HostSelection,
    },
    GroupSource, HostConnection, LifecycleSource, LocalSources, StatusSource,
};
use skiff_core::{group::Group, session::SessionSpec};
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
    net::{UnixListener, UnixStream},
};

mod common;

struct Dir(PathBuf);
impl Dir {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("skiff-m1g-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&dir).unwrap();
        Self(dir)
    }
}
impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn row(id: &str) -> Value {
    json!({"id":id,"label":"sh","role":"shell","cwd":"/srv/λ","command":"/bin/sh","args":[],
        "state":"idle","cols":80,"rows":24,"pid":7,"exit_code":null,"theme":"sea-dog","name":"n",
        "unread":true,"input_gen":3})
}

fn spec() -> SessionSpec {
    serde_json::from_value(
        json!({"label":"claude","name":"Fix it","role":"agent","cwd":"/srv/λ repo",
        "command":"claude","args":["--resume",""],"cols":100,"rows":30}),
    )
    .unwrap()
}

fn group() -> Group {
    serde_json::from_value(json!({"name":"Review","layout":{"type":"split","dir":"col","ratio":0.25,
        "a":{"type":"pane","session":"s1"},"b":{"type":"pane","session":"slot:1"}},"focus":"s1","cwd":"/srv"}))
    .unwrap()
}

type Peer = BufReader<UnixStream>;
type Op<'a> = Pin<Box<dyn Future<Output = Result<Value, String>> + 'a>>;

/// Runs `op` once on the legacy Client and once through the selected Host,
/// answering each with `reply`. Both must send `frame` (with their own id)
/// and come back with the same result or the same error text.
async fn same(
    peer: &mut Peer,
    id: &mut u64,
    frame: &Value,
    reply: &Value,
    op: impl Fn(bool) -> Op<'static>,
) -> Result<Value, String> {
    let mut results = vec![];
    for legacy in [true, false] {
        *id += 1;
        let (result, ()) = tokio::join!(op(legacy), async {
            let mut line = String::new();
            peer.read_line(&mut line).await.unwrap();
            let mut sent: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(sent["id"], json!(*id), "{line}");
            sent.as_object_mut().unwrap().remove("id");
            assert_eq!(&sent, frame, "legacy={legacy}");
            let mut answer = reply.clone();
            answer["id"] = json!(*id);
            let mut bytes = serde_json::to_vec(&answer).unwrap();
            bytes.push(b'\n');
            peer.get_mut().write_all(&bytes).await.unwrap();
        });
        results.push(result);
    }
    assert_eq!(results[0], results[1], "{frame}");
    results.pop().unwrap()
}

macro_rules! op {
    ($client:ident, $conn:ident, |$c:ident| $legacy:expr, |$h:ident| $host:expr $(, clone $extra:ident)*) => {{
        let $client = $client.clone();
        let $conn = $conn.clone();
        move |legacy: bool| -> Op<'static> {
            let $c = $client.clone();
            let $h = $conn.clone();
            $(let $extra = $extra.clone();)*
            Box::pin(async move {
                let value = if legacy {
                    $legacy.await.map(|v| serde_json::to_value(v).unwrap())
                } else {
                    $host.await.map(|v| serde_json::to_value(v).unwrap())
                };
                value.map_err(|e| format!("{e:#}"))
            })
        }
    }};
}

#[tokio::test]
async fn selected_lifecycle_and_groups_match_legacy_frames_results_and_errors_on_one_client() {
    let dir = Dir::new();
    let socket = dir.0.join("a.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let other = UnixListener::bind(dir.0.join("b.sock")).unwrap();
    let selection = HostSelection::new(socket, dir.0.join("local-host-id"));
    let (pending, peer) = tokio::join!(selection.connect(), listener.accept());
    let host = pending.unwrap().activate().await.unwrap();
    let client = host.client();
    let conn = host.connection();
    assert!(conn
        .hello()
        .capabilities
        .contains(&Capability::session_lifecycle()));
    assert!(conn
        .hello()
        .capabilities
        .contains(&Capability::session_groups()));
    let mut peer = BufReader::new(peer.unwrap().0);
    let mut id = 0;
    let error = json!({"type":"error","message":"no such session"});
    let ok = json!({"type":"ok"});
    let session = json!({"type":"session","session":row("s1")});
    let unreadable = json!({"type":"session","session":{"id":"s1"}});

    let create = op!(client, conn, |c| c.create_session(spec()), |h| {
        legacy_create(&h, spec())
    });
    let frame = json!({"cmd":"create_session","spec":{"label":"claude","name":"Fix it","role":"agent",
        "cwd":"/srv/λ repo","command":"claude","args":["--resume",""],"peek":false,"cols":100,"rows":30}});
    let created = same(&mut peer, &mut id, &frame, &session, &create)
        .await
        .unwrap();
    assert_eq!(created["id"], "s1");
    assert_eq!(created["unread"], true);
    assert_eq!(
        same(&mut peer, &mut id, &frame, &error, &create).await,
        Err("no such session".into())
    );
    // Wrong reply kind and an unreadable row keep their legacy text.
    assert_eq!(
        same(&mut peer, &mut id, &frame, &ok, &create).await,
        Err("unexpected reply: Ok".into())
    );
    assert!(same(&mut peer, &mut id, &frame, &unreadable, &create)
        .await
        .is_err());

    let rename = op!(client, conn, |c| c.rename_session("s1", ""), |h| {
        legacy_rename(&h, "s1", "")
    });
    let frame = json!({"cmd":"rename_session","session":"s1","name":""});
    assert_eq!(
        same(&mut peer, &mut id, &frame, &session, &rename)
            .await
            .unwrap()["name"],
        "n"
    );
    assert_eq!(
        same(&mut peer, &mut id, &frame, &error, &rename).await,
        Err("no such session".into())
    );

    for theme in [Some("sea-dog".to_string()), None] {
        let t = theme.clone();
        let set = op!(client, conn, |c| c.set_session_theme("s1", t.clone()), |h| legacy_set_theme(&h, "s1", t.clone()), clone t);
        let frame = json!({"cmd":"set_session_theme","session":"s1","theme":theme});
        assert!(same(&mut peer, &mut id, &frame, &session, &set)
            .await
            .is_ok());
    }

    let kill = op!(
        client,
        conn,
        |c| c.kill(&"s1".to_string()),
        |h| legacy_kill(&h, "s1")
    );
    let frame = json!({"cmd":"kill","session":"s1"});
    assert_eq!(
        same(&mut peer, &mut id, &frame, &ok, &kill).await,
        Ok(Value::Null)
    );
    assert_eq!(
        same(&mut peer, &mut id, &frame, &error, &kill).await,
        Err("no such session".into())
    );
    assert!(same(&mut peer, &mut id, &frame, &session, &kill)
        .await
        .unwrap_err()
        .starts_with("unexpected reply: Session"));

    let seen = op!(client, conn, |c| c.seen("s1"), |h| legacy_seen(&h, "s1"));
    let frame = json!({"cmd":"seen","session":"s1"});
    assert_eq!(
        same(&mut peer, &mut id, &frame, &ok, &seen).await,
        Ok(Value::Null)
    );

    let list = op!(client, conn, |c| c.list_groups(), |h| legacy_groups(&h));
    let stored = {
        let mut g = serde_json::to_value(group()).unwrap();
        g["id"] = json!("g1");
        g
    };
    let groups = same(
        &mut peer,
        &mut id,
        &json!({"cmd":"list_groups"}),
        &json!({"type":"groups","groups":[stored.clone(), stored.clone()]}),
        &list,
    )
    .await;
    assert_eq!(groups, Ok(json!([stored.clone(), stored.clone()])));
    assert_eq!(
        same(
            &mut peer,
            &mut id,
            &json!({"cmd":"list_groups"}),
            &error,
            &list
        )
        .await,
        Err("no such session".into())
    );

    let save = op!(client, conn, |c| c.save_group(group()), |h| {
        legacy_save_group(&h, group())
    });
    let frame = json!({"cmd":"save_group","group":serde_json::to_value(group()).unwrap()});
    assert_eq!(
        frame["group"]["id"], "",
        "a new group goes out with an empty id"
    );
    assert_eq!(
        same(
            &mut peer,
            &mut id,
            &frame,
            &json!({"type":"group","group":stored}),
            &save
        )
        .await,
        Ok(stored)
    );

    let delete = op!(client, conn, |c| c.delete_group("g1"), |h| {
        legacy_delete_group(&h, "g1")
    });
    assert_eq!(
        same(
            &mut peer,
            &mut id,
            &json!({"cmd":"delete_group","group":"g1"}),
            &ok,
            &delete
        )
        .await,
        Ok(Value::Null)
    );

    // Only the selected endpoint, on the very same Client.
    assert!(Arc::ptr_eq(&client, &host.client()));
    assert!(
        tokio::time::timeout(Duration::from_millis(20), other.accept())
            .await
            .is_err()
    );

    // A lost connection: the same text, and the Host never resends.
    drop(peer);
    tokio::time::timeout(Duration::from_secs(5), async {
        while !client.is_closed() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let legacy = client.kill(&"s1".to_string()).await.unwrap_err();
    let hosted = legacy_kill(&conn, "s1").await.unwrap_err();
    assert_eq!(format!("{legacy:#}"), format!("{hosted:#}"));
}

/// M3: the agent list through the selected Host sends the very same
/// `list_agents` frame and returns the same rows and error text as the legacy
/// Client. It has its own capability.
#[tokio::test]
async fn selected_agent_list_matches_the_legacy_frame_result_and_errors() {
    let dir = Dir::new();
    let socket = dir.0.join("a.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let selection = HostSelection::new(socket, dir.0.join("local-host-id"));
    let (pending, peer) = tokio::join!(selection.connect(), listener.accept());
    let host = pending.unwrap().activate().await.unwrap();
    let client = host.client();
    let conn = host.connection();
    assert!(conn
        .hello()
        .capabilities
        .contains(&Capability::session_agents()));
    let mut peer = BufReader::new(peer.unwrap().0);
    let mut id = 0;
    let rows = json!([
        {"id":"claude","command":"claude --model λ","default_command":"claude","custom":false,"installed":true,"enabled":true},
        {"id":"mine","command":"/opt/λ/mine","default_command":"","custom":true,"installed":false,"enabled":false}
    ]);
    let list = op!(client, conn, |c| c.list_agents(), |h| legacy_agents(&h));
    let frame = json!({"cmd":"list_agents"});
    assert_eq!(
        same(&mut peer, &mut id, &frame, &json!({"type":"agents","agents":rows}), &list).await,
        Ok(rows)
    );
    assert_eq!(
        same(
            &mut peer,
            &mut id,
            &frame,
            &json!({"type":"error","message":"daemon reloading"}),
            &list
        )
        .await,
        Err("daemon reloading".into())
    );
    assert!(same(&mut peer, &mut id, &frame, &json!({"type":"ok"}), &list)
        .await
        .unwrap_err()
        .starts_with("unexpected reply: Ok"));
}

#[tokio::test]
async fn a_coordinator_spec_is_refused_rather_than_silently_changed() {
    let dir = Dir::new();
    let socket = dir.0.join("a.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let selection = HostSelection::new(socket, dir.0.join("local-host-id"));
    let (pending, peer) = tokio::join!(selection.connect(), listener.accept());
    let host = pending.unwrap().activate().await.unwrap();
    let mut spec = spec();
    spec.coordinator = true;
    assert!(legacy_create(&host.connection(), spec).await.is_err());
    let mut peer = BufReader::new(peer.unwrap().0);
    let mut line = String::new();
    assert!(
        tokio::time::timeout(Duration::from_millis(20), peer.read_line(&mut line))
            .await
            .is_err()
    );
}

struct Status;
impl StatusSource for Status {
    fn status(&self) -> Pin<Box<dyn Future<Output = Result<Response, Error>> + Send + '_>> {
        Box::pin(async { panic!("not status") })
    }
}

/// Holds every call until `open`, so a fan-out fills the lane.
struct Fake {
    calls: AtomicUsize,
    open: tokio::sync::Semaphore,
}
impl LifecycleSource for Fake {
    fn lifecycle(
        &self,
        request: Request,
    ) -> Pin<Box<dyn Future<Output = Result<Response, Error>> + Send + '_>> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::Relaxed);
            let _go = self.open.acquire().await.unwrap();
            match request {
                Request::SessionKill { .. } => Ok(Response::Ok),
                _ => Ok(Response::Groups { groups: vec![] }),
            }
        })
    }
}
impl GroupSource for Fake {
    fn groups(
        &self,
        _: Request,
    ) -> Pin<Box<dyn Future<Output = Result<Response, Error>> + Send + '_>> {
        Box::pin(async { panic!("not groups") })
    }
}

fn hello(capabilities: Vec<Capability>) -> Hello {
    common::hello("host", "i", capabilities)
}

fn local(
    fake: &Arc<Fake>,
    lifecycle: bool,
    groups: bool,
    caps: Vec<Capability>,
) -> Result<HostConnection, Error> {
    HostConnection::local(
        LocalSources {
            status: Arc::new(Status),
            screen: None,
            inventory: None,
            updates: None,
            terminal: None,
            lifecycle: lifecycle.then(|| fake.clone() as Arc<dyn LifecycleSource>),
            groups: groups.then(|| fake.clone() as Arc<dyn GroupSource>),
            agents: None,
        },
        hello(caps),
        None,
    )
}

#[tokio::test]
async fn lifecycle_is_gated_by_its_own_source_and_capability_and_never_falls_back() {
    let fake = Arc::new(Fake {
        calls: AtomicUsize::new(0),
        open: tokio::sync::Semaphore::new(1000),
    });
    // An adapter cannot claim a capability it has no source for.
    for (lifecycle, groups, cap) in [
        (false, true, Capability::session_lifecycle()),
        (true, false, Capability::session_groups()),
    ] {
        assert!(matches!(
            local(&fake, lifecycle, groups, vec![cap]),
            Err(Error::MalformedContract { .. })
        ));
    }
    // A source without the capability in Hello is never called.
    let host = local(&fake, true, true, vec![Capability::connection_status()]).unwrap();
    let error = legacy_kill(&host, "s1").await.unwrap_err();
    assert_eq!(
        error.downcast_ref::<Error>(),
        Some(&Error::UnsupportedCapability {
            capability: Capability::session_lifecycle()
        })
    );
    assert!(legacy_groups(&host).await.is_err());
    // Another major is not this capability.
    let host = local(
        &fake,
        true,
        true,
        vec![Capability {
            name: SESSION_LIFECYCLE.into(),
            version: 2,
        }],
    )
    .unwrap();
    assert!(legacy_seen(&host, "s1").await.is_err());
    assert_eq!(fake.calls.load(Ordering::Relaxed), 0);
    // A reply of the wrong kind from the source is malformed, never accepted.
    let host = local(&fake, true, false, vec![Capability::session_lifecycle()]).unwrap();
    assert!(matches!(
        host.request(Request::SessionSeen {
            session: "s".into()
        })
        .await,
        Err(Error::MalformedContract { .. })
    ));
}

#[tokio::test]
async fn closing_a_project_ends_more_sessions_than_the_lane_holds_without_host_busy() {
    let fake = Arc::new(Fake {
        calls: AtomicUsize::new(0),
        open: tokio::sync::Semaphore::new(0),
    });
    let host = Arc::new(local(&fake, true, false, vec![Capability::session_lifecycle()]).unwrap());
    let kills: Vec<_> = (0..80)
        .map(|i| {
            let host = host.clone();
            tokio::spawn(async move {
                legacy_kill(&host, &format!("s{i}"))
                    .await
                    .map_err(|e| e.to_string())
            })
        })
        .collect();
    // The lane admits at most 63 at once (one slot stays for status).
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(fake.calls.load(Ordering::Relaxed), 63);
    fake.open.add_permits(80);
    for kill in kills {
        assert_eq!(kill.await.unwrap(), Ok(()));
    }
    assert_eq!(fake.calls.load(Ordering::Relaxed), 80);
    // Disconnect wakes a waiting call; nothing is replayed.
    let fake = Arc::new(Fake {
        calls: AtomicUsize::new(0),
        open: tokio::sync::Semaphore::new(0),
    });
    let host = Arc::new(local(&fake, true, false, vec![Capability::session_lifecycle()]).unwrap());
    let waiting = {
        let host = host.clone();
        tokio::spawn(async move {
            host.request(Request::SessionKill {
                session: "s".into(),
            })
            .await
        })
    };
    tokio::time::sleep(Duration::from_millis(20)).await;
    host.disconnect();
    assert_eq!(waiting.await.unwrap(), Err(Error::TransportLost));
    assert_eq!(fake.calls.load(Ordering::Relaxed), 1);
}
