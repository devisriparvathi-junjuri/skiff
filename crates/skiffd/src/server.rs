//! One task per client connection. Fast requests are answered in order, slow
//! ones in order on a second task; events and subscribed output are
//! interleaved on the same connection.

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use anyhow::Result;
use skiff_core::{
    protocol::{Envelope, Event, Reply, Request, Response, ServerMessage},
    session::SessionId,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
    sync::{broadcast, mpsc},
    task::JoinHandle,
};

use crate::session::{Session, SessionPool};

mod slow;
use slow::answer_slow;

/// A request or a piece of output slower than this is logged at debug level.
const SLOW: Duration = Duration::from_millis(50);

/// The ms since `since`, when that is longer than [`SLOW`].
fn slow_ms(since: Instant) -> Option<u128> {
    let took = since.elapsed();
    (took > SLOW).then_some(took.as_millis())
}

/// Numbers each client connection, for the watcher registry.
static CONNECTIONS: AtomicU64 = AtomicU64::new(1);

/// What `ping` reports in place of the version, from [`take_test_version`].
static TEST_VERSION: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// A debug build started with `SKIFF_TEST_REPORT_VERSION` reports that
/// version to `ping`, so the app's update tests meet an older daemon. It
/// takes the variable out of the environment: the image a reload execs, and
/// every session, see none, and the reloaded daemon reports its own version.
/// Call it before any thread starts. A release build ignores the variable.
pub fn take_test_version() {
    const VAR: &str = "SKIFF_TEST_REPORT_VERSION";
    if cfg!(debug_assertions) {
        if let Some(v) = std::env::var_os(VAR) {
            let _ = TEST_VERSION.set(v.to_string_lossy().into_owned());
            std::env::remove_var(VAR);
        }
    }
}

/// Ends the connection's watcher registration, on every way out of [`handle`].
struct Unwatch {
    pool: Arc<SessionPool>,
    conn: u64,
}

impl Drop for Unwatch {
    fn drop(&mut self) {
        self.pool.unwatch(self.conn);
    }
}

pub async fn handle(stream: UnixStream, pool: Arc<SessionPool>) -> Result<()> {
    let conn = CONNECTIONS.fetch_add(1, Ordering::Relaxed);
    let peer = stream.peer_cred().ok().and_then(|c| c.pid()).and_then(|p| u32::try_from(p).ok());
    let _unwatch = Unwatch { pool: pool.clone(), conn };
    let (rd, mut wr) = stream.into_split();
    // Output and snapshots, with when the PTY read of the output's first byte came.
    let (tx, mut rx) = mpsc::channel::<(ServerMessage, Option<Instant>)>(4096);
    // Replies and state events. Small and few, so unbounded, and written
    // first, so a busy terminal does not delay a reply or the sidebar.
    let (ctl, mut ctl_rx) = mpsc::unbounded_channel::<ServerMessage>();

    let writer = tokio::spawn(async move {
        loop {
            let (msg, read) = tokio::select! {
                biased;
                Some(msg) = ctl_rx.recv() => (msg, None),
                Some(out) = rx.recv() => out,
                else => break,
            };
            let mut line = serde_json::to_vec(&msg)?;
            line.push(b'\n');
            wr.write_all(&line).await?;
            if let (Some(read), ServerMessage::Event(Event::Output { session, .. })) = (read, &msg) {
                if let Some(ms) = slow_ms(read) {
                    tracing::debug!("output of {session} took {ms} ms from PTY read to send");
                }
            }
        }
        anyhow::Ok(())
    });

    // Every client hears every state change.
    let mut pool_events = pool.events.subscribe();
    let ctl_events = ctl.clone();
    let events_task = tokio::spawn(async move {
        loop {
            match pool_events.recv().await {
                Ok(e) => {
                    if ctl_events.send(ServerMessage::Event(e)).is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!("client lagged {n} events");
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });

    // Slow requests answer from here, in the order they came. A reply can
    // pass one from this queue; the client matches replies by id.
    let (slow_tx, mut slow_rx) = mpsc::unbounded_channel::<(u64, Request, Instant)>();
    let slow_task = {
        let ctl = ctl.clone();
        let pool = pool.clone();
        tokio::spawn(async move {
            while let Some((id, request, since)) = slow_rx.recv().await {
                let name = request.name();
                let response = answer_slow(pool.clone(), request).await;
                if let Some(ms) = slow_ms(since) {
                    tracing::debug!("request {name} took {ms} ms");
                }
                if ctl.send(ServerMessage::Reply(Reply { id, response })).is_err() {
                    break;
                }
            }
        })
    };

    let mut subs: HashMap<SessionId, JoinHandle<()>> = HashMap::new();
    let mut lines = BufReader::new(rd).lines();
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let env: Envelope = match serde_json::from_str(&line) {
            Ok(e) => e,
            Err(e) => {
                // Answer with the request's id when it has one, so the
                // client does not wait forever on a command we do not know.
                let id = serde_json::from_str::<serde_json::Value>(&line)
                    .ok()
                    .and_then(|v| v.get("id").and_then(|i| i.as_u64()))
                    .unwrap_or(0);
                let reply = Reply {
                    id,
                    response: Response::Error {
                        message: format!("bad request: {e}"),
                    },
                };
                ctl.send(ServerMessage::Reply(reply))?;
                continue;
            }
        };

        let since = Instant::now();
        let name = env.request.name();
        let response = match env.request {
            Request::Ping => Response::Pong {
                version: TEST_VERSION.get().map_or(skiff_core::VERSION, |v| v.as_str()).to_string(),
                protocol: skiff_core::PROTOCOL,
                reload_state: Some(crate::session::reload::HANDOVER),
                pid: Some(std::process::id()),
                debug: Some(cfg!(debug_assertions)),
                identity: crate::identity::get(),
            },
            // Writes before this one on the connection are queued already;
            // the reload drains them. It answers only when it fails: success
            // execs and closes the connection.
            Request::Reload { binary } => {
                let ctl = ctl.clone();
                let pool = pool.clone();
                let id = env.id;
                tokio::spawn(async move {
                    let message = match tokio::task::spawn_blocking(move || pool.reload(binary)).await {
                        Ok(Err(e)) => format!("{e:#}"),
                        Ok(Ok(never)) => match never {},
                        Err(e) => format!("reload: {e}"),
                    };
                    let _ = ctl.send(ServerMessage::Reply(Reply { id, response: Response::Error { message } }));
                });
                continue;
            }
            Request::Shutdown => {
                tracing::info!("shutdown requested over the socket");
                crate::stop::request();
                Response::Ok
            }
            Request::ListSessions => Response::Sessions {
                sessions: pool.list(),
            },
            Request::DaemonMetrics => Response::DaemonMetrics { metrics: pool.metrics() },
            req @ (Request::CreateSession { .. }
            | Request::ListAgents
            | Request::ListAgentSessions { .. }
            | Request::Coordinator { .. }) => {
                if slow_tx.send((env.id, req, since)).is_err() {
                    break;
                }
                continue;
            }
            Request::Write { session, data, paste } => match if paste { pool.paste(&session, &data) } else { pool.write(&session, &data) } {
                Ok(()) => Response::Ok,
                Err(e) => error(e),
            },
            Request::Submit { session, data, paste, enter, expect } => {
                match pool.submit(&session, &data, paste, enter, expect) {
                    Ok(true) => Response::Ok,
                    Ok(false) => Response::Refused,
                    Err(e) => error(e),
                }
            }
            Request::SetSessionTheme { session, theme } => match pool.set_theme(&session, theme) {
                Ok(session) => Response::Session { session },
                Err(e) => error(e),
            },
            Request::RenameSession { session, name } => match pool.rename(&session, &name) {
                Ok(session) => Response::Session { session },
                Err(e) => error(e),
            },
            Request::Resize {
                session,
                cols,
                rows,
            } => match pool.resize(&session, cols, rows).await {
                Ok(()) => Response::Ok,
                Err(e) => error(e),
            },
            Request::Subscribe { session } => match pool.get(&session) {
                Some(s) => {
                    // Stop the old stream before the snapshot. Otherwise it can
                    // still send chunks the snapshot already holds.
                    if let Some(old) = subs.remove(&session) {
                        old.abort();
                        let _ = old.await;
                    }
                    let (snapshot, mut out) = s.attach();
                    tx.send((snapshot_event(&session, &s, snapshot), None)).await?;
                    let tx = tx.clone();
                    let sid = session.clone();
                    // Weak, so a killed session still drops and closes `out`.
                    let s = Arc::downgrade(&s);
                    let task = tokio::spawn(async move {
                        loop {
                            match out.recv().await {
                                Ok(chunk) => {
                                    let ev = Event::Output {
                                        session: sid.clone(),
                                        data: chunk.data.clone(),
                                        echo: chunk.echo,
                                    };
                                    if tx.send((ServerMessage::Event(ev), Some(chunk.read))).await.is_err() {
                                        break;
                                    }
                                }
                                Err(broadcast::error::RecvError::Lagged(n)) => {
                                    // Lost chunks corrupt the client's screen.
                                    // Redraw it from a fresh snapshot.
                                    tracing::warn!("subscriber to {sid} lagged {n} chunks");
                                    let Some(s) = s.upgrade() else { break };
                                    let (snapshot, rx) = s.attach();
                                    out = rx;
                                    if tx.send((snapshot_event(&sid, &s, snapshot), None)).await.is_err() {
                                        break;
                                    }
                                }
                                Err(broadcast::error::RecvError::Closed) => break,
                            }
                        }
                    });
                    subs.insert(session, task);
                    Response::Ok
                }
                None => Response::Error {
                    message: format!("no such session: {session}"),
                },
            },
            Request::Unsubscribe { session } => {
                if let Some(task) = subs.remove(&session) {
                    task.abort();
                }
                Response::Ok
            }
            Request::Watching { session, failure, follows } => match pool.watching(conn, peer, &session, failure, follows) {
                Ok(()) => Response::Ok,
                Err(e) => error(e),
            },
            Request::Retire { session } => match pool.retire(&session) {
                Ok(()) => Response::Ok,
                Err(e) => error(e),
            },
            Request::Rescope { session, scope, name } => match pool.rescope(&session, &scope, &name) {
                Ok(session) => Response::Session { session },
                Err(e) => error(e),
            },
            Request::Seen { session } => match pool.seen(&session) {
                Ok(()) => Response::Ok,
                Err(e) => error(e),
            },
            Request::Screen { session, rows } => match pool.tail_text(&session, rows as usize) {
                Ok(lines) => Response::Screen { lines },
                Err(e) => error(e),
            },
            Request::Kill { session } => match pool.kill(&session) {
                Ok(()) => Response::Ok,
                Err(e) => error(e),
            },
            Request::ListGroups => Response::Groups {
                groups: pool.list_groups(),
            },
            Request::SaveGroup { group } => match pool.save_group(group) {
                Ok(group) => Response::Group { group },
                Err(e) => error(e),
            },
            Request::DeleteGroup { group } => match pool.delete_group(&group) {
                Ok(()) => Response::Ok,
                Err(e) => error(e),
            },
            Request::OrderGroups { groups } => match pool.order_groups(&groups) {
                Ok(()) => Response::Ok,
                Err(e) => error(e),
            },
        };

        if let Some(ms) = slow_ms(since) {
            tracing::debug!("request {name} took {ms} ms");
        }
        let reply = Reply {
            id: env.id,
            response,
        };
        if ctl.send(ServerMessage::Reply(reply)).is_err() {
            break;
        }
    }

    for (_, task) in subs {
        task.abort();
    }
    events_task.abort();
    // Answer what is still queued before the writer closes.
    drop(slow_tx);
    let _ = slow_task.await;
    drop(tx);
    drop(ctl);
    let _ = writer.await;
    Ok(())
}

fn snapshot_event(session: &str, s: &Session, data: Vec<u8>) -> ServerMessage {
    let info = s.info();
    ServerMessage::Event(Event::Snapshot {
        session: session.to_string(),
        data,
        cols: info.cols,
        rows: info.rows,
    })
}

fn error(e: anyhow::Error) -> Response {
    Response::Error {
        message: format!("{e:#}"),
    }
}
