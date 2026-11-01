use std::{future::Future, pin::Pin, sync::{Arc, atomic::{AtomicUsize, Ordering}}};
use skiff_client::host::{local::{legacy_attach, legacy_detach, legacy_output}, OutputGap, TerminalFrame};
use skiff_core::protocol::Event;
use tauri::{ipc::{Channel, InvokeResponseBody}, AppHandle, Emitter, State};
use tokio::sync::Notify;
use crate::connection::{App, ensure_host, err, host_now};

/// One write to the page holds at most this much, so xterm can yield to the
/// page between pieces instead of parsing one long chunk.
const PIECE: usize = 64 * 1024;
/// Unparsed live bytes past this, and live output is dropped instead of
/// queued. A keystroke echo or Ctrl+C waits behind everything queued, and
/// xterm parses about 10 MB/s on WebKitGTK, so this is about 25 ms of parse:
/// under two frames. The last snapshot does not count: it can be larger than
/// this on its own, and dropping output behind it would only ask for another.
const BEHIND_HIGH: usize = 256 * 1024;
/// Unparsed bytes under this, and a pane that dropped output redraws from a
/// fresh snapshot. A quarter of the high mark, so the queue is near empty
/// when the snapshot lands and the redraw itself is not behind.
const BEHIND_LOW: usize = BEHIND_HIGH / 4;

/// A subscribe in flight for a fresh snapshot. Boxed, so the select loop can
/// hold it across turns.
type Resubscribe = Pin<Box<dyn Future<Output = bool> + Send>>;

/// Bytes one page stream's xterm has parsed, for the stream task to pace by.
pub(crate) struct Flow {
    /// The page's stream number, so an ack from an older stream is ignored.
    stream: u32,
    /// Bytes the page's xterm parsed so far.
    parsed: AtomicUsize,
    /// Signalled on each ack.
    acked: Notify,
}

impl Flow {
    /// Sends `data` to the page in pieces. Returns the bytes sent.
    fn send(&self, on_output: &Channel<InvokeResponseBody>, data: Vec<u8>) -> tauri::Result<usize> {
        let len = data.len();
        if len <= PIECE {
            on_output.send(InvokeResponseBody::Raw(data))?;
            return Ok(len);
        }
        for piece in data.chunks(PIECE) {
            on_output.send(InvokeResponseBody::Raw(piece.to_vec()))?;
        }
        Ok(len)
    }
}

/// Streams raw PTY bytes for one session, starting with a screen snapshot,
/// from the selected Host's `session_terminal@1` plane. Replaces any earlier
/// subscription. Output the page cannot keep up with is dropped; once it
/// catches up, a snapshot redraws the screen. Nothing is replayed.
#[tauri::command]
pub(crate) async fn subscribe_output(
    app: State<'_, App>,
    session: String,
    stream: u32,
    on_output: Channel<InvokeResponseBody>,
) -> Result<(), String> {
    let c = host_now(&app).await?;
    // Listen first: the snapshot can arrive before the attach reply.
    let mut rx = legacy_output(&c, &session).map_err(err)?;
    legacy_attach(&c, &session).await.map_err(err)?;
    let flow = Arc::new(Flow {
        stream,
        parsed: AtomicUsize::new(0),
        acked: Notify::new(),
    });
    app.flows.lock().unwrap().insert(session.clone(), flow.clone());
    let sid = session.clone();
    // Weak, so a replaced Host connection still drops and closes `rx`.
    let client = Arc::downgrade(&c);
    let task = tauri::async_runtime::spawn(async move {
        // Output from an earlier subscription predates the snapshot. Skip it.
        let mut live = false;
        // Output was dropped because the page fell behind.
        let mut behind = false;
        // Bytes sent to the page, and `sent` as of the last snapshot.
        let mut sent = 0usize;
        let mut snapshot_end = 0usize;
        // The loop keeps draining `rx` while this runs, so the queue cannot
        // fill behind the round trip and lag again. Starting a new one drops
        // the old: its snapshot still lands, and the later one is newer.
        let mut resubscribe: Option<Resubscribe> = None;
        // Asks the daemon for a fresh snapshot without blocking the loop.
        let start_resubscribe = || -> Option<Resubscribe> {
            let c = client.upgrade()?;
            let sid = sid.clone();
            Some(Box::pin(async move { legacy_attach(&c, &sid).await.is_ok() }))
        };
        loop {
            let event = tokio::select! {
                event = rx.recv() => event,
                ok = async { resubscribe.as_mut().unwrap().await }, if resubscribe.is_some() => {
                    resubscribe = None;
                    if !ok {
                        break;
                    }
                    continue;
                }
                _ = flow.acked.notified(), if behind => {
                    if sent.saturating_sub(flow.parsed.load(Ordering::Relaxed)) > BEHIND_LOW {
                        continue;
                    }
                    behind = false;
                    live = false;
                    rx.clear();
                    let Some(r) = start_resubscribe() else { break };
                    resubscribe = Some(r);
                    continue;
                }
            };
            match event {
                Ok(TerminalFrame::Snapshot { data, .. }) => {
                    live = true;
                    let Ok(n) = flow.send(&on_output, data) else { break };
                    sent += n;
                    snapshot_end = sent;
                }
                Ok(TerminalFrame::Output { data, echo }) if live && !behind => {
                    // Live bytes past the last snapshot that xterm has not parsed.
                    let parsed = flow.parsed.load(Ordering::Relaxed).max(snapshot_end);
                    if sent.saturating_sub(parsed) >= BEHIND_HIGH {
                        behind = true;
                        continue;
                    }
                    // The latency trace reads it before the bytes it times.
                    if let Some(echo) = echo.filter(|_| crate::diagnostics::tracing_on()).and_then(|e| serde_json::to_string(&e).ok()) {
                        if on_output.send(InvokeResponseBody::Json(echo)).is_err() {
                            break;
                        }
                    }
                    let Ok(n) = flow.send(&on_output, data) else { break };
                    sent += n;
                }
                Ok(_) => {}
                Err(OutputGap::Lagged) => {
                    // Output was lost. Ask for a fresh snapshot and skip
                    // everything before it. A pane that is behind asks
                    // once it catches up.
                    live = false;
                    rx.clear();
                    if behind {
                        continue;
                    }
                    let Some(r) = start_resubscribe() else { break };
                    resubscribe = Some(r);
                }
                Err(OutputGap::Closed) => break,
            }
        }
    });
    if let Some(old) = app.subs.lock().await.insert(session, task) {
        old.abort();
    }
    Ok(())
}

/// The page's xterm parsed `bytes` more of stream `stream`.
#[tauri::command]
pub(crate) fn ack_output(app: State<'_, App>, session: String, stream: u32, bytes: usize) {
    let Some(flow) = app.flows.lock().unwrap().get(&session).cloned() else { return };
    if flow.stream != stream {
        return;
    }
    flow.parsed.fetch_add(bytes, Ordering::Relaxed);
    flow.acked.notify_one();
}

#[tauri::command]
pub(crate) async fn unsubscribe_output(app: State<'_, App>, session: String) -> Result<(), String> {
    app.flows.lock().unwrap().remove(&session);
    if let Some(task) = app.subs.lock().await.remove(&session) {
        task.abort();
    }
    let c = host_now(&app).await?;
    legacy_detach(&c, &session).await.map_err(err)
}

/// The selected remote Host's connection ended.
pub(crate) const HOST_LOST: &str = "skiff:host-lost";

/// Streams every non-output event: state changes, exits, created, removed.
/// The client routes output elsewhere, so a busy terminal cannot make this lag.
#[tauri::command]
pub(crate) async fn subscribe_events(handle: AppHandle, app: State<'_, App>, on_event: Channel<Event>) -> Result<(), String> {
    // Through the selected Host; a lag is skipped there, as it was here.
    // Both from the one connection, so a Host switch between the two
    // cannot pair one Host's updates with another's link.
    let (host, _) = ensure_host(&app).await?;
    let connection = host.connection();
    let mut updates = skiff_client::host::local::legacy_updates(&connection).map_err(err)?;
    let remote = app.remote();
    let link = connection.clone();
    let task = tauri::async_runtime::spawn(async move {
        // The stream does not end when the link drops: look each second.
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(1));
        loop {
            tokio::select! {
                e = updates.recv() => match e {
                    Some(e) => if on_event.send(e).is_err() { return },
                    None => break,
                },
                _ = tick.tick() => if link.is_closed() { break },
            }
        }
        // A remote Host's page reconnects on this: handshake, identity,
        // then fresh subscriptions and snapshots. This Mac keeps its policy.
        if remote {
            let _ = handle.emit(HOST_LOST, ());
        }
    });
    // A switch while this ran replaced the selection: this task would keep
    // the old Host's link open, so it goes, and the reloaded page subscribes.
    let same = app.live.lock().unwrap().as_ref().is_some_and(|live| Arc::ptr_eq(live, &connection));
    if !same {
        task.abort();
        return Err("The Host changed. The page subscribes again.".into());
    }
    if let Some(old) = app.events.lock().await.replace(task) {
        old.abort();
    }
    Ok(())
}
