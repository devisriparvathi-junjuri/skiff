//! The session pool: one PTY per session, one reader thread per PTY.

use std::{
    collections::HashMap,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicU64, AtomicUsize, Ordering},
        Arc, Condvar, Mutex, RwLock, RwLockReadGuard,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{anyhow, bail, Context, Result};
use polling::Poller;
use portable_pty::{native_pty_system, PtySize};
use skiff_core::{
    group::Group,
    protocol::{EchoTimes, Event},
    session::{now_ms, SessionId, SessionInfo, SessionSpec, SessionState, WatchFailure, WatchInfo, ENTER_DELAY},
};
use tokio::sync::{broadcast, Notify};

use crate::{
    program_status::{self, Records},
    pty::{self, Pty},
    screen::{title_state, Screen, Signals, StatusSignal, TitleState},
    workspace::{SavedSession, Workspace},
};

mod away;
mod attention;
mod coordinator;
mod foreground;
mod groups;
mod input;
mod output;
mod persistence;
mod registry;
mod status;
mod launch;
mod lifecycle;
pub mod reload;
use groups::prune;
use attention::{end_reports, live, topic};
use input::{start_writer, Input};
use lifecycle::default_shell;
use output::{Scan, ScreenState};
#[cfg(test)]
use output::prints;
pub use coordinator::{Ask, Coordinated};

/// A process's command line, for logs. Empty when it is gone.
pub(crate) fn proc_argv(pid: u32) -> Vec<String> {
    away::proc::argv(pid)
}

/// Output for subscribers, as one piece.
pub type Chunk = Arc<Piece>;

pub struct Piece {
    pub data: Vec<u8>,
    /// When the reader read its first byte.
    pub read: Instant,
    /// Set when it holds the first output read after input.
    pub echo: Option<EchoTimes>,
}

impl std::ops::Deref for Piece {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        &self.data
    }
}

/// No output for this long turns `working` into `idle`.
pub const IDLE_AFTER: Duration = Duration::from_secs(3);

/// A stretch of work shorter than this is no turn: a status line that
/// redraws, a notice. Its end leaves nothing to read.
pub(crate) const MIN_TURN_MS: u64 = 1500;

/// A terminal title only the wrapper shell sets, right before it execs the
/// fallback shell in place of an agent that just exited. Seeing it means the
/// session is a plain shell now, not the agent it was launched as. The exit
/// code of the agent follows a colon: `skiff:shell-handoff:1`.
const SHELL_HANDOFF_TITLE: &str = "skiff:shell-handoff";

/// What runs in front of a session. It selects the rules for the bell and
/// the unread flag, so a shell and an agent each get rules that are true.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Front {
    /// The shell at its prompt. It is never working and never rings.
    Shell,
    /// A program the shell runs. Its end is a result to read.
    Command,
    /// A supported agent, by the name the foreground watcher gives it. Its
    /// approval prompt is read from the screen.
    Agent(&'static str),
    /// A program the launch wrapper runs that is no supported agent. The bell
    /// is its only way to call the user.
    Program,
}

/// Output waits this long for more, so a burst of reads goes out as one
/// chunk. A read with nothing after it goes out when this passes.
const QUIET: Duration = Duration::from_micros(500);

/// Output waits at most this long from its first byte, however fast the
/// program streams.
const HOLD: Duration = Duration::from_millis(8);

/// A chunk goes out once it holds this much, before the hold ends. A client
/// parses a chunk in one go, so a large one stalls it.
const MAX_CHUNK: usize = 64 * 1024;

/// Output this soon after input or a resize answers it: an echo or a
/// redraw. It does not mark the session working.
const ECHO_MS: u64 = 250;

/// A client's answer to a query counts as one this long after the query.
/// Without a client nothing answers, and keys typed later are keys.
const REPLY_WINDOW_MS: u64 = 5000;

/// Queries kept unanswered at most.
const MAX_QUERIES: u32 = 64;

/// A program that says it works yields to its own approval prompt once the
/// prompt has stayed on screen this long after its last report: it may ask
/// without saying so.
const GRACE_MS: u64 = 2000;

/// A program that says it works and prints nothing for this long is marked
/// stalled: a hint that it may hang.
const STALL_MS: u64 = 5 * 60 * 1000;

/// A report that changes only its message, title or progress, or a child's,
/// is sent at most this often. Grok changes its message at every phase.
const STATUS_EVERY_MS: u64 = 1000;

/// The screen text is checked at most this often. Each check walks every cell,
/// and what it feeds (idle detection, the approval prompt) works in seconds.
const SCAN: Duration = Duration::from_millis(100);

pub struct Session {
    info: Mutex<SessionInfo>,
    pty: Pty,
    /// Input for the writer thread. A program that stops reading its input
    /// blocks that thread, never the daemon's executor or other sessions.
    input: std::sync::mpsc::Sender<Input>,
    /// Writes accepted and not yet in the PTY. A reload waits for 0.
    queued: Arc<AtomicUsize>,
    /// Unix µs when the writer thread first wrote input to the PTY since the
    /// last output. The reader takes it with the first output after it; 0 once taken.
    wrote: Arc<AtomicU64>,
    /// Wakes the reader thread out of its wait, so it sees a pause request.
    waker: Poller,
    /// Pauses the reader thread for a reload.
    park: Park,
    /// Raw PTY output. Subscribers that lag lose chunks, never block the reader.
    pub output: broadcast::Sender<Chunk>,
    /// Unix ms of the last PTY read. Atomic so the reader takes no lock for it.
    last_output: AtomicU64,
    /// Unix ms of the last output that was work: visible text, not an echo.
    last_work: AtomicU64,
    /// Unix ms of the last input or resize from a client.
    last_input: AtomicU64,
    /// A key went in and no output has answered it yet. The first read
    /// after it goes out at once; the reads after that merge again.
    key_pending: AtomicBool,
    /// Unix ms when the current stretch of work began.
    work_started: AtomicU64,
    /// No stretch of work has ended yet under the program in front. The first
    /// one is the program drawing itself, not a turn.
    fresh: AtomicBool,
    /// The agent's busy line showed in the current stretch of work: it is a
    /// turn, however short.
    busy_seen: AtomicBool,
    /// What the agent in front last said in its window title.
    title_state: Mutex<Option<TitleState>>,
    /// The PTY's root process is a shell: launched as one, or handed off to.
    shell: AtomicBool,
    front: Mutex<Front>,
    /// The foreground process group `front` was read from. 0 before the first read.
    group: AtomicU32,
    /// `waiting` came from a bell, so input answers it.
    belled: AtomicBool,
    /// What the program in front reported with OSC 7501. Locked before `info`
    /// when both are.
    records: Mutex<Records>,
    /// Queries the program sent and no client answered yet, and when the
    /// last one came (Unix ms). See [`replies`].
    queries: AtomicU32,
    queried_at: AtomicU64,
    /// Unix ms since the agent's approval prompt shows under a working
    /// report; 0 while it does not. See [`GRACE_MS`].
    approval_since: AtomicU64,
    /// That prompt made the session wait, over the report.
    raised: AtomicBool,
    /// The report is marked stalled, so a read clears it without a lock.
    stalled: AtomicBool,
    /// Unix ms of the last update sent for a report, and a minor change
    /// since then that waits to be sent. See [`STATUS_EVERY_MS`].
    status_sent_at: AtomicU64,
    status_dirty: AtomicBool,
    /// Emulator plus unsent bytes. One lock, so a snapshot and the live
    /// stream never overlap or leave a gap.
    screen: Mutex<ScreenState>,
}

/// The handshake that pauses a reader thread. A reader checks it between
/// reads, never with bytes read and not yet fed to the screen.
#[derive(Default)]
struct Park {
    state: Mutex<ParkState>,
    changed: Condvar,
}

#[derive(Default)]
struct ParkState {
    /// A reload wants the reader paused.
    asked: bool,
    /// The reader is paused.
    parked: bool,
    /// The reader has ended: the child is reaped and its exit recorded.
    done: bool,
}

impl Session {
    /// The program took the terminal: raw input, as editors, pagers and
    /// pickers set, or the alternate screen.
    fn takes_terminal(&self) -> bool {
        self.screen.lock().unwrap().screen.alt() || self.pty.raw_input()
    }

    pub fn info(&self) -> SessionInfo {
        let mut info = self.info.lock().unwrap().clone();
        info.last_output_at = self.last_output.load(Ordering::Relaxed);
        info
    }

    /// A snapshot of the screen, and a receiver for every byte after it.
    pub fn attach(&self) -> (Vec<u8>, broadcast::Receiver<Chunk>) {
        let mut st = self.screen.lock().unwrap();
        st.flush(&self.output);
        (st.screen.snapshot(), self.output.subscribe())
    }

    /// The program in front reports its own state with OSC 7501. Its report
    /// decides the state over the screen rules and the idle timer. A result
    /// kept after the program left decides nothing.
    fn claimed(&self) -> bool {
        live(&self.info.lock().unwrap())
    }

    pub(crate) fn front(&self) -> Front {
        *self.front.lock().unwrap()
    }

    /// The process group in front of the PTY. One ioctl.
    pub(crate) fn foreground_group(&self) -> Option<u32> {
        self.pty.foreground_group()
    }

    /// Called by the reader thread between reads. Waits while a reload has
    /// it paused.
    fn park_point(&self) {
        let mut st = self.park.state.lock().unwrap();
        if !st.asked {
            return;
        }
        st.parked = true;
        self.park.changed.notify_all();
        while st.asked {
            st = self.park.changed.wait(st).unwrap();
        }
        st.parked = false;
    }

    /// Asks the reader to pause and waits until it has, or has ended.
    fn pause_reader(&self, until: Instant) -> Result<()> {
        let mut st = self.park.state.lock().unwrap();
        st.asked = true;
        let _ = self.waker.notify();
        while !st.parked && !st.done {
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                bail!("the reader of session {} did not pause", self.info.lock().unwrap().id);
            }
            st = self.park.changed.wait_timeout(st, left).unwrap().0;
        }
        Ok(())
    }

    fn resume_reader(&self) {
        self.park.state.lock().unwrap().asked = false;
        self.park.changed.notify_all();
    }

    /// The reader ended: the child is reaped, or the session was adopted done.
    fn reader_done(&self) -> bool {
        self.park.state.lock().unwrap().done
    }

    fn set_reader_done(&self) {
        self.park.state.lock().unwrap().done = true;
        self.park.changed.notify_all();
    }

}

pub struct SessionPool {
    sessions: RwLock<HashMap<SessionId, Arc<Session>>>,
    /// Killed sessions whose reader has not reaped the child yet.
    dying: Mutex<Vec<Arc<Session>>>,
    /// Children of sessions lost to a reload, not reaped yet. A reload
    /// hands them on with the dying sessions' children.
    orphans: Mutex<Vec<u32>>,
    /// Named layouts, in creation order. Locked before `sessions` when both are.
    groups: Mutex<Vec<Group>>,
    /// State changes, exits, creations, removals. Not output.
    pub events: broadcast::Sender<Event>,
    /// Groups or sessions changed since the last save.
    dirty: Notify,
    /// Set while a reload hands the sessions over. Changes are refused and
    /// the watchers skip their turn.
    reloading: AtomicBool,
    /// Held shared by each change to sessions or groups and each process
    /// spawn. A reload takes it alone once, to wait for those in flight.
    changes: RwLock<()>,
    /// One reload at a time.
    reload: Mutex<()>,
    /// The listening socket's fd, which a reload hands over. -1 when unset.
    listener: AtomicI32,
    /// Held while a coordinator request finds or starts one. Holds the
    /// last session a start or resume line went into, and when.
    coordinator: Mutex<Option<(SessionId, Instant)>>,
    /// The daemon process start, retained through reload handover by its pid.
    started_at: AtomicU64,
    /// `skiff watch` registrations by connection number. Not saved and not
    /// handed over: a watcher registers again when it connects again.
    /// By connection and the session watched: one watcher process may
    /// register for several Skippers, each on its own session.
    watchers: Mutex<HashMap<(u64, SessionId), Watcher>>,
}

/// One connection's [`Request::Watching`] registration.
///
/// [`Request::Watching`]: skiff_core::protocol::Request::Watching
struct Watcher {
    session: SessionId,
    pid: Option<u32>,
    since: u64,
    failure: Option<WatchFailure>,
    follows: Option<Vec<SessionId>>,
}

impl Default for SessionPool {
    fn default() -> Self {
        let (events, _) = broadcast::channel(4096);
        Self {
            sessions: RwLock::new(HashMap::new()),
            dying: Mutex::new(Vec::new()),
            orphans: Mutex::new(Vec::new()),
            groups: Mutex::new(Vec::new()),
            events,
            dirty: Notify::new(),
            reloading: AtomicBool::new(false),
            changes: RwLock::new(()),
            reload: Mutex::new(()),
            listener: AtomicI32::new(-1),
            coordinator: Mutex::new(None),
            started_at: AtomicU64::new(now_ms()),
            watchers: Mutex::new(HashMap::new()),
        }
    }
}

/// Changes this close together are saved once.
const SAVE_DELAY: Duration = Duration::from_millis(500);

impl SessionPool {
    /// Records the program in front. A program that ends and leaves the shell
    /// in front is a result to read.
    pub(crate) fn set_front(&self, session: &Session, front: Front) {
        let old = std::mem::replace(&mut *session.front.lock().unwrap(), front);
        if old == front {
            return;
        }
        session.belled.store(false, Ordering::Relaxed);
        *session.title_state.lock().unwrap() = None;
        let agent = matches!(front, Front::Agent(_));
        if agent {
            session.fresh.store(true, Ordering::Relaxed);
            // The screen was last read without this agent's prompt rule.
            // The reader is woken, so it sets its scan deadline.
            if let Ok(mut st) = session.screen.try_lock() {
                st.unscanned = true;
            }
            let _ = session.waker.notify();
        }
        let (state, updated, ended) = {
            let mut records = session.records.lock().unwrap();
            let mut info = session.info.lock().unwrap();
            if info.state == SessionState::Done {
                return;
            }
            let mut state = None;
            let mut changed = false;
            let mut ended = None;
            if front == Front::Shell {
                if info.state != SessionState::Idle {
                    info.state = SessionState::Idle;
                    state = Some(info.id.clone());
                }
                changed = !info.unread;
                info.unread = true;
                ended = Some(info.id.clone());
                // The shell's prompt is back: the program's reports end with it.
                changed |= end_reports(&mut records, &mut info);
            } else if agent && info.agent_exit.take().is_some() {
                changed = true;
                // The exit code is saved with the workspace.
                self.dirty.notify_one();
            }
            (state, changed.then(|| info.clone()), ended)
        };
        if let Some(session) = state {
            let _ = self.events.send(Event::State { session, state: SessionState::Idle });
        }
        if let Some(session) = ended {
            let _ = self.events.send(Event::TurnEnded { session });
        }
        if let Some(session) = updated {
            let _ = self.events.send(Event::SessionUpdated { session });
        }
    }

    /// The session's last rows of text, without the agent's input box.
    pub fn tail_text(&self, id: &str, rows: usize) -> Result<Vec<String>> {
        let session = self.get(id).ok_or_else(|| anyhow!("no such session: {id}"))?;
        let agent = match session.front() {
            Front::Agent(agent) => Some(agent),
            _ => None,
        };
        let st = session.screen.lock().unwrap();
        Ok(st.screen.tail_text(agent, rows))
    }

    /// A client's pane for the session has the keys: the user has seen it.
    pub fn seen(&self, id: &str) -> Result<()> {
        let _change = self.change()?;
        let session = self.get(id).ok_or_else(|| anyhow!("no such session: {id}"))?;
        let updated = {
            let mut info = session.info.lock().unwrap();
            // A result kept after its program left is read now.
            let kept = info.program_status.as_ref().is_some_and(|p| p.ended);
            if kept {
                info.program_status = None;
            }
            let changed = info.unread || info.agent_exit.is_some() || kept;
            info.unread = false;
            if info.agent_exit.take().is_some() {
                self.dirty.notify_one();
            }
            changed.then(|| info.clone())
        };
        if let Some(session) = updated {
            let _ = self.events.send(Event::SessionUpdated { session });
        }
        Ok(())
    }

    pub fn rename(&self, id: &str, name: &str) -> Result<SessionInfo> {
        let _change = self.change()?;
        let session = self.get(id).ok_or_else(|| anyhow!("no such session: {id}"))?;
        let name = name.trim();
        session.info.lock().unwrap().name = (!name.is_empty()).then(|| name.to_string());
        let info = session.info();
        let _ = self.events.send(Event::SessionUpdated {
            session: info.clone(),
        });
        self.dirty.notify_one();
        Ok(info)
    }

    /// Takes a project Skipper over: its mark goes now, and the session ends
    /// at its next idle (see [`Self::retired`]). An idle one ends now.
    pub fn retire(&self, id: &str) -> Result<()> {
        let info = {
            let _change = self.change()?;
            let session = self.get(id).ok_or_else(|| anyhow!("no such session: {id}"))?;
            let mut info = session.info.lock().unwrap();
            if info.skipper.is_none() || info.coordinator {
                bail!("session {id} is not a project Skipper");
            }
            info.skipper = None;
            info.retiring = true;
            info.clone()
        };
        tracing::info!("session {id} retires: a fleet Skipper took it over");
        let _ = self.events.send(Event::SessionUpdated { session: info });
        self.dirty.notify_one();
        self.end_retired();
        Ok(())
    }

    /// Moves a Skipper to a new scope and name: a fleet's rename.
    pub fn rescope(&self, id: &str, scope: &str, name: &str) -> Result<SessionInfo> {
        let _change = self.change()?;
        let session = self.get(id).ok_or_else(|| anyhow!("no such session: {id}"))?;
        {
            let mut info = session.info.lock().unwrap();
            if info.skipper.is_none() || info.coordinator || scope.trim().is_empty() {
                bail!("session {id} is not a project or fleet Skipper");
            }
            info.skipper = Some(scope.to_string());
            info.name = (!name.trim().is_empty()).then(|| name.trim().to_string());
        }
        let info = session.info();
        let _ = self.events.send(Event::SessionUpdated { session: info.clone() });
        self.dirty.notify_one();
        Ok(info)
    }

    /// The retiring sessions that are idle or done. A waiting one keeps its
    /// prompt for the person; a working one finishes its turn.
    fn retired(&self) -> Vec<String> {
        let sessions = self.sessions.read().unwrap();
        sessions
            .values()
            .filter_map(|s| {
                let info = s.info.lock().unwrap();
                (info.retiring && matches!(info.state, SessionState::Idle | SessionState::Done)).then(|| info.id.clone())
            })
            .collect()
    }

    fn end_retired(&self) {
        for id in self.retired() {
            match self.kill(&id) {
                Ok(()) => tracing::info!("session {id} retired: it was idle"),
                Err(e) => tracing::warn!("cannot end retiring session {id}: {e:#}"),
            }
        }
    }

    pub fn set_theme(&self, id: &str, theme: Option<String>) -> Result<SessionInfo> {
        let _change = self.change()?;
        let session = self.get(id).ok_or_else(|| anyhow!("no such session: {id}"))?;
        session.info.lock().unwrap().theme = theme.filter(|t| !t.is_empty());
        let info = session.info();
        let _ = self.events.send(Event::SessionUpdated {
            session: info.clone(),
        });
        self.dirty.notify_one();
        Ok(info)
    }

}

#[cfg(test)]
mod tests {
    use super::{
        now_ms, prints, topic, Front, Screen, ScreenState, GRACE_MS, HOLD, MIN_TURN_MS,
        QUIET, SCAN, STALL_MS,
    };
    use std::time::Instant;

    fn state() -> ScreenState {
        ScreenState {
            screen: Screen::new(40, 5),
            pending: Vec::new(),
            held_since: Instant::now(),
            last_push: Instant::now(),
            last_scan: Instant::now() - SCAN,
            unscanned: false,
            printed: false,
            printed_work: false,
            echo: None,
        }
    }

    fn print(st: &mut ScreenState, bytes: &[u8], echo: bool) {
        st.screen.feed(bytes);
        st.unscanned = true;
        st.printed = true;
        st.printed_work |= !echo;
    }

    #[test]
    fn scan_counts_new_text_once() {
        let mut st = state();
        print(&mut st, b"hello", false);
        assert!(st.scan(Front::Program).work);
        assert!(!st.unscanned);
        print(&mut st, b"", false);
        assert!(!st.scan(Front::Program).work);
    }

    #[test]
    fn echo_updates_the_baseline_without_work() {
        let mut st = state();
        print(&mut st, b"typed", true);
        assert!(!st.scan(Front::Program).work);
        print(&mut st, b"", false);
        assert!(!st.scan(Front::Program).work);
    }

    #[test]
    fn held_output_is_due_after_a_quiet_moment_or_the_hold() {
        let mut st = state();
        assert!(st.flush_due().is_none());
        let first = Instant::now();
        st.held_since = first;
        st.last_push = first;
        st.pending.extend_from_slice(b"a");
        assert_eq!(st.flush_due(), Some(first + QUIET));
        st.last_push = first + HOLD;
        assert_eq!(st.flush_due(), Some(first + HOLD));
        st.unscanned = true;
        assert_eq!(st.scan_due(), Some(st.last_scan + SCAN));
    }

    #[test]
    fn topic_drops_the_status_glyph() {
        assert_eq!(topic("◐ Fix the flusher"), "Fix the flusher");
        assert_eq!(topic("◑ Fix the flusher"), "Fix the flusher");
        assert_eq!(topic("⠋ Codex"), "Codex");
        assert_eq!(topic("[ ! ] Action Required"), "Action Required");
        assert_eq!(topic("plain"), "plain");
    }

    #[test]
    fn only_text_counts_as_printing() {
        assert!(!prints(b"\x1b(B\x0f\x1b[?2004h\x1b[?1000h\x1b[?1006h"));
        assert!(!prints(b"\x1b]0;\xe2\x9c\xb3 Claude Code\x07\r\n"));
        assert!(!prints(b"\x1b[?2026h\x1b[?25l\x1b[43;16H\x1b[?25h\x1b[?2026l"));
        assert!(prints(b"\x1b[38;2;215;119;87m*\x1b[39m"));
        assert!(prints("\x1b[3G✻".as_bytes()));
    }

    // SPLIT-SCAFFOLD
    #[test]
    fn a_status_result_orders_state_turn_and_update_events() {
        use skiff_core::{
            protocol::Event,
            session::{SessionSpec, SessionState},
        };
        let pool = super::SessionPool::new();
        let info = pool
            .create(SessionSpec {
                command: Some("/bin/sleep".into()),
                args: vec!["30".into()],
                ..Default::default()
            })
            .unwrap();
        let session = pool.get(&info.id).unwrap();
        pool.set_front(&session, Front::Program);
        pool.apply_status(
            &session,
            vec![super::StatusSignal::Report(b"state=working".to_vec())],
        );
        let mut events = pool.events.subscribe();
        pool.apply_status(
            &session,
            vec![super::StatusSignal::Report(b"state=done".to_vec())],
        );
        let events: Vec<_> = std::iter::from_fn(|| events.try_recv().ok()).collect();
        assert!(matches!(events.as_slice(), [
            Event::State { state: SessionState::Idle, .. },
            Event::TurnEnded { .. },
            Event::SessionUpdated { .. },
        ]));
        pool.kill(&info.id).unwrap();
    }

    // SPLIT-SCAFFOLD
    #[test]
    fn shell_front_ignores_status_reports() {
        use skiff_core::session::SessionSpec;
        let pool = super::SessionPool::new();
        let info = pool
            .create(SessionSpec {
                command: Some("/bin/sh".into()),
                args: vec!["-c".into(), "sleep 30".into()],
                ..Default::default()
            })
            .unwrap();
        let session = pool.get(&info.id).unwrap();
        pool.set_front(&session, Front::Shell);
        pool.apply_status(
            &session,
            vec![super::StatusSignal::Report(b"state=working:app=codex".to_vec())],
        );
        assert!(session.info().program_status.is_none());
        pool.kill(&info.id).unwrap();
    }

    /// While an agent reports with OSC 7501, its reports decide the state:
    /// not its approval prompt, its title, its bell or the quiet time. With
    /// no report left, those rules decide again.
    #[test]
    fn an_agents_reports_win_over_the_screen_and_the_timer() {
        use skiff_core::{
            protocol::Event,
            session::{SessionSpec, SessionState},
        };
        use std::time::Duration;
        let pool = super::SessionPool::new();
        let info = pool
            .create(SessionSpec {
                command: Some("/bin/sleep".into()),
                args: vec!["30".into()],
                cols: 80,
                rows: 24,
                ..Default::default()
            })
            .unwrap();
        let session = pool.get(&info.id).unwrap();
        pool.set_front(&session, Front::Agent("codex"));
        let mut events = pool.events.subscribe();
        // Each feed is scanned, as a read after a quiet moment is.
        let feed = |bytes: &[u8]| {
            std::thread::sleep(SCAN);
            pool.on_output(&session, &info.id, bytes);
            session.info()
        };
        let prompt = b"\r\nWould you like to run the following command?\r\nPress enter to confirm or esc to cancel\r\n";

        let now = feed(b"\x1b]7501;state=working:app=codex\x1b\\");
        assert_eq!(now.state, SessionState::Working);
        assert_eq!(now.program_status.unwrap().app.as_deref(), Some("codex"));
        assert_eq!(
            feed(prompt).state,
            SessionState::Working,
            "the screen rules overrode the report"
        );
        assert_eq!(
            feed(b"\x1b]0;[ ! ] Action Required\x07\x07").state,
            SessionState::Working,
            "the title or bell overrode it"
        );
        session
            .last_work
            .store(0, std::sync::atomic::Ordering::Relaxed);
        pool.idle_if_quiet(&session);
        assert_eq!(
            session.info().state,
            SessionState::Working,
            "the idle timer overrode it"
        );

        let asked =
            feed(b"\x1b]7501;state=blocked:app=codex:kind=question:msg=UGljayBhIGJyYW5jaA==\x1b\\");
        assert_eq!(asked.state, SessionState::Waiting);
        let status = asked.program_status.unwrap();
        assert_eq!(
            (status.kind.as_deref(), status.msg.as_deref()),
            (Some("question"), Some("Pick a branch"))
        );
        pool.write(&info.id, b"x").unwrap();
        assert_eq!(
            session.info().state,
            SessionState::Waiting,
            "input answered it as if it were a bell"
        );

        let done = feed(b"\x1b]7501;state=done:app=codex\x1b\\");
        assert_eq!((done.state, done.unread), (SessionState::Idle, true));
        assert_eq!(
            done.program_status.unwrap().state,
            "done",
            "done is told apart from idle"
        );
        feed(b"\x1b]7501;state=done:app=codex\x1b\\");
        let ended = std::iter::from_fn(|| events.try_recv().ok())
            .filter(|e| matches!(e, Event::TurnEnded { .. }))
            .count();
        assert_eq!(ended, 1, "a repeated done ended a second turn");

        // Cleared, the prompt still on screen counts again.
        let cleared = feed(b"\x1b]7501;state=clear\x1b\\");
        assert!(cleared.program_status.is_none());
        assert_eq!(feed(prompt).state, SessionState::Waiting);

        // A full reset drops the reports, and so does the shell's prompt.
        assert!(feed(b"\x1b]7501;state=working\x07")
            .program_status
            .is_some());
        assert!(feed(b"\x1bc").program_status.is_none());
        feed(b"\x1b]7501;state=error\x07");
        pool.set_front(&session, Front::Shell);
        // The failure stays to be read, and decides nothing more.
        let kept = session.info();
        let status = kept.program_status.unwrap();
        assert_eq!((status.state.as_str(), status.ended), ("error", true));
        assert!(kept.unread && !session.claimed());
        pool.seen(&info.id).unwrap();
        assert!(session.info().program_status.is_none());
        std::thread::sleep(Duration::from_millis(10));
        pool.kill(&info.id).unwrap();
    }

    /// A program that never says done (OpenCode) ends a turn the user
    /// started with idle: the pane is unread. Its startup is no turn.
    #[test]
    fn idle_after_a_typed_turn_ends_it_for_a_program_that_never_says_done() {
        use skiff_core::{
            protocol::Event,
            session::{SessionSpec, SessionState},
        };
        let pool = super::SessionPool::new();
        let info = pool
            .create(SessionSpec {
                command: Some("/bin/sleep".into()),
                args: vec!["30".into()],
                cols: 80,
                rows: 24,
                ..Default::default()
            })
            .unwrap();
        let session = pool.get(&info.id).unwrap();
        pool.set_front(&session, Front::Agent("opencode"));
        let feed = |bytes: &[u8]| {
            std::thread::sleep(SCAN);
            pool.on_output(&session, &info.id, bytes);
            session.info()
        };
        let mut events = pool.events.subscribe();
        // Its startup: idle, then work nobody asked for.
        feed(b"\x1b]7501;state=idle:app=opencode\x07");
        feed(b"\x1b]7501;state=working:app=opencode\x07starting");
        std::thread::sleep(std::time::Duration::from_millis(MIN_TURN_MS));
        assert!(
            !feed(b"\x1b]7501;state=idle:app=opencode\x07").unread,
            "its startup was a turn"
        );
        // A turn the user typed.
        pool.write(&info.id, b"say hi\r").unwrap();
        feed(b"\x1b]7501;state=working:app=opencode\x07");
        std::thread::sleep(std::time::Duration::from_millis(MIN_TURN_MS));
        feed(b"Hi! How can I help?\r\n");
        let done = feed(b"\x1b]7501;state=idle:app=opencode\x07");
        assert_eq!((done.state, done.unread), (SessionState::Idle, true));
        let ended = std::iter::from_fn(|| events.try_recv().ok())
            .filter(|e| matches!(e, Event::TurnEnded { .. }))
            .count();
        assert_eq!(ended, 1);
        pool.kill(&info.id).unwrap();
    }

    /// The refinements over a report: another program in front ends it, the
    /// agent's own prompt outlasts a report that it works, a long silence is
    /// a hint, and a message-only change waits a moment.
    #[test]
    fn reports_bind_to_their_program_and_yield_to_its_prompt() {
        use skiff_core::{
            protocol::Event,
            session::{SessionSpec, SessionState},
        };
        use std::sync::atomic::Ordering::Relaxed;
        let pool = super::SessionPool::new();
        let info = pool
            .create(SessionSpec {
                command: Some("/bin/sleep".into()),
                args: vec!["30".into()],
                cols: 80,
                rows: 24,
                ..Default::default()
            })
            .unwrap();
        let session = pool.get(&info.id).unwrap();
        pool.set_front(&session, Front::Agent("codex"));
        let feed = |bytes: &[u8]| {
            std::thread::sleep(SCAN);
            pool.on_output(&session, &info.id, bytes);
            session.info()
        };
        let prompt = b"\r\nWould you like to run the following command?\r\nPress enter to confirm or esc to cancel\r\n";

        // Another group in front: the reports were the old program's.
        feed(b"\x1b]7501;state=done:app=codex\x07");
        let group = session.records.lock().unwrap().group;
        assert_ne!(group, 0, "the report took no group");
        pool.status_group(&session, group);
        assert!(
            session.info().program_status.is_some(),
            "the same group ended it"
        );
        pool.status_group(&session, group + 1);
        let left = session.info().program_status.unwrap();
        assert!(left.ended && !session.claimed(), "{left:?}");

        // The prompt under a report that it works, past the grace.
        assert_eq!(
            feed(b"\x1b]7501;state=working\x07").state,
            SessionState::Working
        );
        assert!(!session.info().program_status.unwrap().ended);
        assert_eq!(feed(prompt).state, SessionState::Working);
        pool.status_tick(&session);
        assert_eq!(session.info().state, SessionState::Working, "no grace");
        session.approval_since.store(now_ms() - GRACE_MS, Relaxed);
        pool.status_tick(&session);
        assert_eq!(session.info().state, SessionState::Waiting);
        assert_eq!(
            feed(b"\x1b[2J\x1b[Hdone deciding\r\n").state,
            SessionState::Working
        );

        // Silence while it works.
        session.last_output.store(now_ms() - STALL_MS, Relaxed);
        pool.status_tick(&session);
        assert!(session.info().program_status.unwrap().stalled);
        assert!(!feed(b"x").program_status.unwrap().stalled);

        // A message-only change waits for the tick; a state change does not.
        let mut events = pool.events.subscribe();
        let updates = |events: &mut tokio::sync::broadcast::Receiver<Event>| {
            std::iter::from_fn(|| events.try_recv().ok())
                .filter(|e| matches!(e, Event::SessionUpdated { .. }))
                .count()
        };
        feed(b"\x1b]7501;state=working:msg=VGhpbmtpbmc=\x07");
        feed(b"\x1b]7501;state=working:msg=UmVzcG9uZGluZw==\x07");
        assert_eq!(
            updates(&mut events),
            0,
            "a message-only change went out at once"
        );
        assert_eq!(
            session.info().program_status.unwrap().msg.as_deref(),
            Some("Responding")
        );
        pool.status_tick(&session);
        assert_eq!(updates(&mut events), 1, "the tick did not send it");
        feed(b"\x1b]7501;state=blocked:kind=permission\x07");
        assert_eq!(updates(&mut events), 1);
        pool.kill(&info.id).unwrap();
    }

    /// An update sent from the info under its lock (here the shell back in
    /// front) carries the time of the last output, not the session's start.
    #[test]
    fn an_update_carries_the_last_output_time() {
        use skiff_core::{protocol::Event, session::SessionSpec};
        let pool = super::SessionPool::new();
        let info = pool
            .create(SessionSpec {
                command: Some("/bin/sleep".into()),
                args: vec!["30".into()],
                ..Default::default()
            })
            .unwrap();
        let session = pool.get(&info.id).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        pool.on_output(&session, &info.id, b"x");
        let mut events = pool.events.subscribe();
        pool.set_front(&session, Front::Shell);
        let sent = std::iter::from_fn(|| events.try_recv().ok())
            .find_map(|e| match e {
                Event::SessionUpdated { session } => Some(session),
                _ => None,
            })
            .expect("no update");
        assert!(
            sent.last_output_at > info.started_at,
            "{} <= {}",
            sent.last_output_at,
            info.started_at
        );
        assert_eq!(sent.last_output_at, session.info().last_output_at);
        pool.kill(&info.id).unwrap();
    }

    /// The pty's size, read back from the kernel.
    fn pty_size(session: &super::Session) -> (u16, u16) {
        let mut size = libc::winsize { ws_row: 0, ws_col: 0, ws_xpixel: 0, ws_ypixel: 0 };
        // SAFETY: TIOCGWINSZ writes only `size`.
        assert_eq!(unsafe { libc::ioctl(session.pty.raw_fd(), libc::TIOCGWINSZ as _, &mut size as *mut _) }, 0);
        (size.ws_col, size.ws_row)
    }

    /// Two clients resize at once. The first has sized the pty and not yet
    /// recorded it (the info lock is held here to keep it there). The second
    /// must wait, or it sizes the pty and the first then announces its older
    /// size last: the inventory and every client would disagree with the pty.
    #[tokio::test(flavor = "multi_thread")]
    async fn concurrent_resizes_announce_the_size_the_pty_has() {
        use skiff_core::{protocol::Event, session::SessionSpec};
        use std::time::Duration;
        let pool = super::SessionPool::new();
        let info = pool
            .create(SessionSpec { command: Some("/bin/cat".into()), cols: 80, rows: 24, ..Default::default() })
            .unwrap();
        let session = pool.get(&info.id).unwrap();
        let mut events = pool.events.subscribe();
        let resize = |cols, rows| {
            let (pool, id) = (pool.clone(), info.id.clone());
            tokio::spawn(async move { pool.resize(&id, cols, rows).await })
        };
        let held = session.info.lock().unwrap();
        let first = resize(100, 30);
        let end = std::time::Instant::now() + Duration::from_secs(5);
        while pty_size(&session) != (100, 30) {
            assert!(std::time::Instant::now() < end, "the first resize never reached the pty");
            std::thread::sleep(Duration::from_millis(10));
        }
        let second = resize(120, 40);
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(pty_size(&session), (100, 30), "the second resize ran ahead of the first's announcement");
        drop(held);
        first.await.unwrap().unwrap();
        second.await.unwrap().unwrap();
        let mut last = None;
        while let Ok(e) = events.try_recv() {
            if let Event::SessionUpdated { session: s } = e {
                if s.id == info.id {
                    last = Some((s.cols, s.rows));
                }
            }
        }
        let now = pty_size(&session);
        let inventory = pool.get(&info.id).unwrap().info();
        assert_eq!(last, Some(now), "the last event");
        assert_eq!((inventory.cols, inventory.rows), now, "the inventory");
        assert_eq!(now, (120, 40));
        pool.kill(&info.id).unwrap();
    }
}

