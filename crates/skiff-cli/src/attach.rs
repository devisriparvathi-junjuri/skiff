//! `skiff attach`: run a session in this terminal until Ctrl-\.

use std::{
    io::{Read, Write},
    sync::Mutex,
    time::{Duration, Instant},
};

use anyhow::{bail, Context, Result};
use skiff_client::Client;
use skiff_core::protocol::Event;
use tokio::{
    signal::unix::{signal, SignalKind},
    sync::{broadcast::error::RecvError, mpsc},
    time::sleep_until,
};

use crate::{
    drops::{Drops, Piece, Remote},
    print, resolve, sessions,
};

/// Ctrl-\ detaches.
const DETACH: u8 = 0x1c;
/// Ctrl-\ under the kitty keyboard protocol, if the session turned it on.
const DETACH_KITTY: &[u8] = b"\x1b[92;5u";
/// Ctrl-C, plain and under the kitty keyboard protocol.
const CTRL_C: u8 = 0x03;
const CTRL_C_KITTY: &[u8] = b"\x1b[99;5u";
/// A second Ctrl-C this soon after the first is held back.
const CTRL_C_WINDOW: Duration = Duration::from_secs(2);
/// How long a notice stays on the bottom row before a redraw wipes it.
const NOTICE: Duration = Duration::from_secs(3);

/// Undoes the modes a session can leave on, then leaves the alternate screen.
const RESET: &str = "\x1b[?2026l\x1b[0m\x1b[r\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1005l\x1b[?1006l\
\x1b[?1004l\x1b[?2004l\x1b[?1l\x1b>\x1b[?7h\x1b[?25h\x1b[<u\x1b[?1049l";

/// The terminal settings from before raw mode. Taken back on restore.
static SAVED: Mutex<Option<libc::termios>> = Mutex::new(None);

/// Puts the terminal back as it was. Safe to call more than once.
fn restore() {
    let Some(t) = SAVED.lock().unwrap_or_else(|e| e.into_inner()).take() else { return };
    let mut out = std::io::stdout();
    let _ = out.write_all(RESET.as_bytes());
    let _ = out.flush();
    unsafe { libc::tcsetattr(0, libc::TCSAFLUSH, &t) };
}

/// Restores the terminal when dropped, so every return path does it.
struct Raw;

impl Raw {
    fn enter() -> Result<Self> {
        if unsafe { libc::isatty(0) } != 1 || unsafe { libc::isatty(1) } != 1 {
            bail!("attach needs a terminal on stdin and stdout");
        }
        let mut t: libc::termios = unsafe { std::mem::zeroed() };
        if unsafe { libc::tcgetattr(0, &mut t) } != 0 {
            return Err(std::io::Error::last_os_error()).context("read terminal settings");
        }
        *SAVED.lock().unwrap() = Some(t);
        // A panic aborts in release builds, so Drop alone is not enough.
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            restore();
            prev(info);
        }));
        let mut raw = t;
        unsafe { libc::cfmakeraw(&mut raw) };
        if unsafe { libc::tcsetattr(0, libc::TCSAFLUSH, &raw) } != 0 {
            SAVED.lock().unwrap().take();
            return Err(std::io::Error::last_os_error()).context("set raw mode");
        }
        // Built first, so a failed write below still restores the terminal.
        let r = Raw;
        let mut out = std::io::stdout();
        out.write_all(b"\x1b[?1049h")?;
        out.flush()?;
        Ok(r)
    }
}

impl Drop for Raw {
    fn drop(&mut self) {
        restore();
    }
}

/// Columns and rows of this terminal.
fn size() -> Option<(u16, u16)> {
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    let ok = unsafe { libc::ioctl(1, libc::TIOCGWINSZ, &mut ws) } == 0;
    (ok && ws.ws_col > 0 && ws.ws_row > 0).then_some((ws.ws_col, ws.ws_row))
}

/// A snapshot starts with a full reset (RIS). That would drop this terminal
/// out of the alternate screen and wipe its history, so clear the screen and
/// the modes instead.
fn snapshot_bytes(data: &[u8]) -> Vec<u8> {
    let body = data.strip_prefix(b"\x1bc").unwrap_or(data);
    let mut v = Vec::with_capacity(body.len() + 64);
    v.extend_from_slice(b"\x1b[0m\x1b[r\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1005l\x1b[?1006l\x1b[?1004l\x1b[?2004l\x1b[?1l\x1b>\x1b[?7h\x1b[?25h\x1b[H\x1b[2J");
    v.extend_from_slice(body);
    v
}

/// A one-line notice on the bottom row, over the session's screen. Saves
/// and restores the cursor and its attributes, so the session's own output
/// goes on where it was. A fresh snapshot wipes it later.
fn notice_bytes(text: &str, cols: u16, rows: u16) -> Vec<u8> {
    // One column short of the edge, so autowrap cannot scroll the screen.
    let width = usize::from(cols.saturating_sub(1));
    let line: String = format!(" {text}").chars().take(width).collect();
    format!("\x1b7\x1b[{rows};1H\x1b[0;7m{line:<width$}\x1b[0m\x1b8").into_bytes()
}

/// Holds back a second Ctrl-C that comes soon after the first, so a habit
/// double-tap does not quit the agent in the pane. The Ctrl-C after a
/// held one passes through.
#[derive(Default)]
struct CtrlCGuard {
    /// When the last Ctrl-C passed through.
    last: Option<Instant>,
    /// A Ctrl-C was held back; the next one passes.
    held: bool,
}

impl CtrlCGuard {
    /// The keys to send, and whether a Ctrl-C was held back.
    fn filter(&mut self, keys: &[u8], now: Instant) -> (Vec<u8>, bool) {
        let mut send = Vec::with_capacity(keys.len());
        let mut held_one = false;
        let mut i = 0;
        while i < keys.len() {
            let len = if keys[i] == CTRL_C {
                1
            } else if keys[i..].starts_with(CTRL_C_KITTY) {
                CTRL_C_KITTY.len()
            } else {
                send.push(keys[i]);
                i += 1;
                continue;
            };
            let soon = self.last.is_some_and(|t| now.duration_since(t) < CTRL_C_WINDOW);
            if soon && !self.held {
                self.held = true;
                held_one = true;
            } else {
                send.extend_from_slice(&keys[i..i + len]);
                // After a held one, the next passes and the count starts over.
                self.last = if self.held { None } else { Some(now) };
                self.held = false;
            }
            i += len;
        }
        (send, held_one)
    }
}

/// How the attach ended.
enum End {
    Detached,
    Exited(Option<i32>),
    Removed,
    DaemonGone,
    Signal,
}

/// `skiff attach --host`: the session on a remote Host from Settings › Hosts,
/// through the same ssh forward the app uses.
pub async fn attach_remote(host: &str, arg: &str) -> Result<()> {
    let hosts = skiff_client::host::known::load(&skiff_client::host::known::path())?;
    let Some(h) = hosts.named(host) else { bail!("no Host named {host} in Settings › Hosts") };
    // The identity the person accepted in the app. The CLI accepts none.
    if h.host_id.is_none() {
        bail!("open {} in the app first: Skiff has not accepted its identity yet", h.name);
    }
    let selection = h.selection(None).map_err(anyhow::Error::msg)?;
    let mut pending = selection.connect().await?;
    pending.daemon_info().await?;
    let selected = pending.activate().await?;
    let remote = Remote { name: h.name.clone(), selection, session: selected.connection() };
    attach(&selected.client(), arg, Some(Drops::new(remote))).await
}

pub async fn attach(c: &Client, arg: &str, mut drops: Option<Drops>) -> Result<()> {
    let all = sessions::sorted(c).await?;
    let s = resolve::session(&all, arg)?;
    let id = s.id.clone();
    let name = resolve::display_name(s).to_string();
    if s.exit_code.is_some() {
        bail!("{name} has exited");
    }

    let mut events = c.events();
    let raw = Raw::enter()?;
    // The pane's size before this client resized it, given back on detach.
    let before = (s.cols, s.rows);
    // Every size this client set. A reported size not among them means
    // another client (the app) sized the pane, and it keeps that size.
    let mut sent: Vec<(u16, u16)> = Vec::new();
    let mut foreign = false;
    if let Some((cols, rows)) = size() {
        c.resize(&id, cols, rows).await?;
        sent.push((cols, rows));
    }
    let mut out = c.output(&id);
    c.subscribe(&id).await?;

    let hint = format!("attached to {name} ({}). Ctrl-\\ detaches.", print::short_id(&id));
    // Drawn over the first snapshot, then wiped.
    let mut hint_pending = true;
    let mut wipe: Option<Instant> = None;
    let mut guard = CtrlCGuard::default();

    // A plain thread: a blocking stdin read must not hold up runtime shutdown.
    let (keys_tx, mut keys) = mpsc::unbounded_channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut stdin = std::io::stdin().lock();
        let mut buf = [0u8; 4096];
        loop {
            match stdin.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if keys_tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
    });
    let mut winch = signal(SignalKind::window_change())?;
    // Handled, so the terminal is restored before the process ends.
    let mut term = signal(SignalKind::terminate())?;
    let mut hup = signal(SignalKind::hangup())?;
    let mut int = signal(SignalKind::interrupt())?;
    let mut quit = signal(SignalKind::quit())?;
    let mut stdout = std::io::stdout();

    let end: Result<End> = async {
        Ok(loop {
            let wipe_at = wipe;
            tokio::select! {
                o = out.recv() => match o {
                    Ok(Event::Snapshot { data, .. }) => {
                        stdout.write_all(&snapshot_bytes(&data))?;
                        if hint_pending {
                            hint_pending = false;
                            if let Some((cols, rows)) = size() {
                                stdout.write_all(&notice_bytes(&hint, cols, rows))?;
                                wipe = Some(Instant::now() + NOTICE);
                            }
                        }
                        stdout.flush()?;
                    }
                    Ok(Event::Output { data, .. }) => {
                        stdout.write_all(&data)?;
                        stdout.flush()?;
                    }
                    Ok(_) => {}
                    Err(RecvError::Lagged(_)) => {
                        // Dropped chunks: redraw from a fresh snapshot.
                        out.clear();
                        c.subscribe(&id).await?;
                    }
                    Err(RecvError::Closed) => break End::DaemonGone,
                },
                k = keys.recv() => {
                    let Some(k) = k else { break End::Detached };
                    // On a remote Host, a paste of local files gets their Host paths.
                    let pieces = match drops.as_mut() {
                        Some(d) => d.pastes.feed(&k),
                        None => vec![Piece::Keys(k)],
                    };
                    let mut detach = false;
                    for piece in pieces {
                        let k = match piece {
                            Piece::Keys(k) => k,
                            Piece::Paste(text) => {
                                match drops.as_mut().expect("only drops find pastes").paste(&text).await {
                                    Ok(send) => c.write_now(&id, send)?,
                                    Err(why) => {
                                        if let Some((cols, rows)) = size() {
                                            let mut err = std::io::stderr();
                                            err.write_all(&notice_bytes(&why, cols, rows))?;
                                            err.flush()?;
                                            wipe = Some(Instant::now() + NOTICE);
                                        }
                                    }
                                }
                                continue;
                            }
                        };
                        let cut = k.iter().position(|&b| b == DETACH).or_else(|| {
                            k.windows(DETACH_KITTY.len()).position(|w| w == DETACH_KITTY)
                        });
                        let (send, held) = guard.filter(&k[..cut.unwrap_or(k.len())], Instant::now());
                        if !send.is_empty() {
                            c.write_now(&id, send)?;
                        }
                        if cut.is_some() {
                            detach = true;
                            break;
                        }
                        if held {
                            if let Some((cols, rows)) = size() {
                                let text = "Ctrl-C held back. Ctrl-\\ detaches. Press Ctrl-C again to send it.";
                                stdout.write_all(&notice_bytes(text, cols, rows))?;
                                stdout.flush()?;
                                wipe = Some(Instant::now() + NOTICE);
                            }
                        }
                    }
                    if detach {
                        break End::Detached;
                    }
                }
                _ = async { sleep_until(wipe_at.unwrap().into()).await }, if wipe_at.is_some() => {
                    // A fresh snapshot repaints the row under the notice.
                    wipe = None;
                    c.subscribe(&id).await?;
                }
                _ = winch.recv() => {
                    if let Some((cols, rows)) = size() {
                        c.resize(&id, cols, rows).await?;
                        sent.push((cols, rows));
                    }
                }
                _ = term.recv() => break End::Signal,
                _ = hup.recv() => break End::Signal,
                _ = int.recv() => break End::Signal,
                _ = quit.recv() => break End::Signal,
                e = events.recv() => match e {
                    Ok(Event::Exit { session, code }) if session == id => break End::Exited(code),
                    Ok(Event::SessionRemoved { session }) if session == id => break End::Removed,
                    Ok(Event::SessionUpdated { session }) if session.id == id => {
                        foreign = !sent.contains(&(session.cols, session.rows));
                    }
                    Ok(_) | Err(RecvError::Lagged(_)) => {}
                    Err(RecvError::Closed) => break End::DaemonGone,
                },
            }
        })
    }
    .await;

    // Give the pane its size back, unless another client (the app) has
    // sized it since. A failed write or resize above ends here too.
    let gone = matches!(end, Ok(End::Exited(_) | End::Removed | End::DaemonGone));
    if !gone && !foreign && sent.last().is_some_and(|&s| s != before) {
        let _ = c.resize(&id, before.0, before.1).await;
    }
    let end = end?;
    if !matches!(end, End::DaemonGone) {
        let _ = c.unsubscribe(&id).await;
    }
    drop(raw);
    match end {
        End::Detached | End::Signal => eprintln!("detached from {name} ({})", print::short_id(&id)),
        End::Exited(Some(code)) => eprintln!("{name} exited with code {code}"),
        End::Exited(None) => eprintln!("{name} exited"),
        End::Removed => eprintln!("{name} was closed"),
        End::DaemonGone => bail!("skiffd closed the connection"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_ctrl_c_is_held_then_third_passes() {
        let mut g = CtrlCGuard::default();
        let t = Instant::now();
        assert_eq!(g.filter(b"a\x03", t), (b"a\x03".to_vec(), false));
        assert_eq!(g.filter(CTRL_C_KITTY, t + Duration::from_millis(500)), (vec![], true));
        assert_eq!(g.filter(b"\x03", t + Duration::from_secs(1)), (b"\x03".to_vec(), false));
        // A slow second Ctrl-C passes.
        let t = t + Duration::from_secs(10);
        assert_eq!(g.filter(b"\x03", t), (b"\x03".to_vec(), false));
        assert_eq!(g.filter(b"\x03", t + Duration::from_secs(3)), (b"\x03".to_vec(), false));
        // Two in one read: the second is held.
        let t = t + Duration::from_secs(10);
        assert_eq!(g.filter(b"\x03x\x03", t), (b"\x03x".to_vec(), true));
    }
}
