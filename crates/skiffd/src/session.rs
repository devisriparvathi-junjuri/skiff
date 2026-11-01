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
    pty::{self, Pty},
    screen::{title_state, Screen, Signals, TitleState},
    workspace::{SavedSession, Workspace},
};

mod away;
mod coordinator;
mod foreground;
mod groups;
mod persistence;
mod launch;
pub mod reload;
use groups::prune;
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

/// Unix time in microseconds, for the latency trace.
fn now_us() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_micros() as u64)
}

/// No output for this long turns `working` into `idle`.
pub const IDLE_AFTER: Duration = Duration::from_secs(3);

/// A stretch of work shorter than this is no turn: a status line that
/// redraws, a notice. Its end leaves nothing to read.
const MIN_TURN_MS: u64 = 1500;

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

struct ScreenState {
    screen: Screen,
    pending: Vec<u8>,
    /// When the first byte of `pending` came. Stale while it is empty.
    held_since: Instant,
    /// When the last byte of `pending` came. Stale while it is empty.
    last_push: Instant,
    last_scan: Instant,
    /// Output came since the last scan.
    unscanned: bool,
    /// A chunk since the last scan printed text.
    printed: bool,
    /// A chunk since the last scan printed text that was not an echo.
    printed_work: bool,
    /// `pending` holds the first output after input: when it was written and read.
    echo: Option<EchoTimes>,
}

/// What a scan of the screen found.
#[derive(Default)]
struct Scan {
    /// The scan ran. The fields below say nothing when it did not.
    scanned: bool,
    /// New text on screen that is not an echo.
    work: bool,
    /// The agent in front shows its approval prompt.
    approval: bool,
    /// The agent in front shows its busy line.
    busy: bool,
}

impl ScreenState {
    fn flush(&mut self, output: &broadcast::Sender<Chunk>) {
        if !self.pending.is_empty() {
            let _ = output.send(Arc::new(Piece {
                data: std::mem::take(&mut self.pending),
                read: self.held_since,
                echo: self.echo.take(),
            }));
        }
    }

    /// When the held output goes out: after a quiet moment, or `HOLD` from
    /// its first byte. `None` with nothing held.
    fn flush_due(&self) -> Option<Instant> {
        (!self.pending.is_empty()).then(|| (self.last_push + QUIET).min(self.held_since + HOLD))
    }

    /// When output that came after the last scan is scanned. `None` with
    /// nothing to scan.
    fn scan_due(&self) -> Option<Instant> {
        self.unscanned.then(|| self.last_scan + SCAN)
    }

    fn scan(&mut self, front: Front) -> Scan {
        let changed = self.printed && self.screen.text_changed();
        let (approval, busy) = match front {
            Front::Agent(agent) => (self.screen.approval_prompt(agent), self.screen.busy_line(agent)),
            _ => (false, false),
        };
        let work = changed && self.printed_work;
        self.last_scan = Instant::now();
        self.unscanned = false;
        self.printed = false;
        self.printed_work = false;
        Scan { scanned: true, work, approval, busy }
    }
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

    /// Queues input for the writer thread, in order.
    fn send(&self, id: &str, items: Vec<Input>) -> Result<()> {
        // Stored before the send: the reader can see the echo before this
        // thread runs again, and it must count as an echo, not as work.
        self.last_input.store(now_ms(), Ordering::Relaxed);
        self.key_pending.store(true, Ordering::Relaxed);
        for item in items {
            self.queued.fetch_add(1, Ordering::AcqRel);
            if self.input.send(item).is_err() {
                self.queued.fetch_sub(1, Ordering::AcqRel);
                bail!("session {id} no longer takes input");
            }
        }
        Ok(())
    }

    /// A snapshot of the screen, and a receiver for every byte after it.
    pub fn attach(&self) -> (Vec<u8>, broadcast::Receiver<Chunk>) {
        let mut st = self.screen.lock().unwrap();
        st.flush(&self.output);
        (st.screen.snapshot(), self.output.subscribe())
    }

    /// True shortly after input: output now is the program answering a key.
    fn echoing(&self) -> bool {
        now_ms().saturating_sub(self.last_input.load(Ordering::Relaxed)) < ECHO_MS
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

    /// Feeds the emulator and queues the bytes. Scans the screen when the last
    /// scan is older than `SCAN`; the reader scans what is left at its
    /// deadline. The last value is true when the chunk answers a key.
    fn push_output(&self, chunk: &[u8]) -> (Signals, Scan, bool) {
        let echoing = self.echoing();
        let front = self.front();
        let mut st = self.screen.lock().unwrap();
        let signals = st.screen.feed(chunk);
        st.unscanned = true;
        if prints(chunk) {
            st.printed = true;
            st.printed_work |= !echoing;
        }
        let scan = if st.last_scan.elapsed() >= SCAN {
            st.scan(front)
        } else {
            Scan::default()
        };
        let now = Instant::now();
        if st.pending.is_empty() {
            st.held_since = now;
        }
        st.last_push = now;
        st.pending.extend_from_slice(chunk);
        // The first read after a write to the PTY: the program's answer.
        let wrote = self.wrote.swap(0, Ordering::Relaxed);
        if wrote != 0 {
            st.echo = Some(EchoTimes { wrote, read: now_us() });
        }
        // The first answer to a keystroke goes out now, even while the
        // program streams output. The reads after that answer merge again,
        // so a redraw does not go out as a stream of small chunks. The rest
        // waits for the reader's deadline, up to `HOLD` from its first byte;
        // a program that streams without a pause never lets the reader wait,
        // so the hold is checked here as well.
        let answers_key = self.key_pending.swap(false, Ordering::Relaxed);
        if answers_key || wrote != 0 || st.pending.len() >= MAX_CHUNK || now >= st.held_since + HOLD {
            st.flush(&self.output);
        }
        (signals, scan, echoing)
    }

    /// Sends the output still held when the reader ends. It waits for the
    /// screen lock: no reader deadline comes after this, so a skipped flush
    /// would lose the child's last bytes.
    fn flush_output(&self) {
        let mut st = self.screen.lock().unwrap();
        st.flush(&self.output);
    }

    /// When the reader must wake with no output: to send the held output,
    /// or to scan output that came after the last scan, so the end of a
    /// burst counts. `None` while nothing is due, so an idle session never
    /// wakes.
    fn next_due(&self) -> Option<Instant> {
        let st = self.screen.lock().unwrap();
        match (st.flush_due(), st.scan_due()) {
            (Some(flush), Some(scan)) => Some(flush.min(scan)),
            (flush, scan) => flush.or(scan),
        }
    }

    /// Does what is due at the reader's deadline: sends the held output, and
    /// scans the screen. Work not due yet waits for the next deadline.
    fn on_due(&self) -> Scan {
        let front = self.front();
        let now = Instant::now();
        let mut st = self.screen.lock().unwrap();
        if st.flush_due().is_some_and(|due| due <= now) {
            st.flush(&self.output);
        }
        if st.scan_due().is_some_and(|due| due <= now) {
            st.scan(front)
        } else {
            Scan::default()
        }
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
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Permission to change sessions or groups, or to start a process.
    /// Refused while a reload runs: the change would miss the handover, and
    /// a new process could inherit fds the reload passes on.
    pub(crate) fn change(&self) -> Result<RwLockReadGuard<'_, ()>> {
        let guard = self.changes.read().unwrap();
        if self.reloading.load(Ordering::Acquire) {
            bail!("skiffd is reloading; try again");
        }
        Ok(guard)
    }

    /// For the watchers: `None` while a reload runs.
    fn quiet(&self) -> Option<RwLockReadGuard<'_, ()>> {
        self.change().ok()
    }

    /// The listening socket, for a reload to hand over.
    pub fn set_listener(&self, fd: i32) {
        self.listener.store(fd, Ordering::Relaxed);
    }

    pub fn list(&self) -> Vec<SessionInfo> {
        let mut v: Vec<SessionInfo> = self
            .sessions
            .read()
            .unwrap()
            .values()
            .map(|s| s.info())
            .collect();
        v.sort_by(|a, b| a.id.cmp(&b.id));
        v
    }

    pub fn metrics(&self) -> skiff_core::protocol::DaemonMetrics {
        let pid = std::process::id();
        let memory_bytes = std::fs::read_to_string(format!("/proc/{pid}/statm"))
            .ok()
            .and_then(|s| s.split_whitespace().nth(1)?.parse::<u64>().ok())
            .map(|pages| pages * 4096)
            .unwrap_or(0);
        skiff_core::protocol::DaemonMetrics {
            version: skiff_core::VERSION.to_string(), pid, started_at: self.started_at.load(Ordering::Relaxed),
            memory_bytes, session_count: self.sessions.read().unwrap().len() as u64,
        }
    }

    pub fn get(&self, id: &str) -> Option<Arc<Session>> {
        self.sessions.read().unwrap().get(id).cloned()
    }

    /// Registers connection `conn` as a watcher of `session`, or replaces its
    /// registration for that session. A replacement keeps `since`. One
    /// connection may watch several sessions: a Skipper's watcher registers
    /// on the grand Skipper and on each project Skipper.
    pub fn watching(
        &self,
        conn: u64,
        pid: Option<u32>,
        session: &str,
        failure: Option<WatchFailure>,
        follows: Option<Vec<SessionId>>,
    ) -> Result<()> {
        if self.get(session).is_none() {
            bail!("no such session: {session}");
        }
        {
            let mut w = self.watchers.lock().unwrap();
            let key = (conn, session.to_string());
            let since = w.get(&key).map_or_else(now_ms, |o| o.since);
            w.insert(key, Watcher { session: session.to_string(), pid, since, failure, follows });
        }
        self.publish_watch(session);
        Ok(())
    }

    /// Connection `conn` closed: its registrations, if any, end.
    pub fn unwatch(&self, conn: u64) {
        let gone: Vec<Watcher> = {
            let mut w = self.watchers.lock().unwrap();
            let keys: Vec<(u64, SessionId)> = w.keys().filter(|k| k.0 == conn).cloned().collect();
            keys.into_iter().filter_map(|k| w.remove(&k)).collect()
        };
        for old in gone {
            self.publish_watch(&old.session);
        }
    }

    /// Writes the registry's view of `id` into its info, and sends
    /// `SessionUpdated` when it changed.
    fn publish_watch(&self, id: &str) {
        let Some(session) = self.get(id) else { return };
        let watch = {
            let w = self.watchers.lock().unwrap();
            let mine: Vec<&Watcher> = w.values().filter(|x| x.session == id).collect();
            (!mine.is_empty()).then(|| WatchInfo {
                since: mine.iter().map(|x| x.since).min().unwrap_or(0),
                pids: {
                    let mut p: Vec<u32> = mine.iter().filter_map(|x| x.pid).collect();
                    p.sort_unstable();
                    p
                },
                failure: mine.iter().filter_map(|x| x.failure.clone()).max_by_key(|f| f.at),
                // The union of what the watchers report. Unknown when none does.
                follows: mine.iter().any(|x| x.follows.is_some()).then(|| {
                    let mut f: Vec<SessionId> = mine.iter().filter_map(|x| x.follows.as_ref()).flatten().cloned().collect();
                    f.sort_unstable();
                    f.dedup();
                    f
                }),
            })
        };
        let changed = {
            let mut info = session.info.lock().unwrap();
            let changed = info.watch != watch;
            info.watch = watch;
            changed
        };
        if changed {
            let _ = self.events.send(Event::SessionUpdated { session: session.info() });
        }
    }

    pub fn create(self: &Arc<Self>, spec: SessionSpec) -> Result<SessionInfo> {
        let _change = self.change()?;
        self.spawn(spec, None)
    }

    /// Starts a session. `id` is the old one when a restore brings it back.
    fn spawn(self: &Arc<Self>, spec: SessionSpec, id: Option<SessionId>) -> Result<SessionInfo> {
        let command = spec
            .command
            .clone()
            .filter(|c| !c.is_empty())
            .unwrap_or_else(default_shell);
        let cwd = spec
            .cwd
            .clone()
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| PathBuf::from("/"));
        // Match the realpaths git reports for worktrees.
        let cwd = cwd.canonicalize().unwrap_or(cwd);
        let label = if spec.label.is_empty() {
            PathBuf::from(&command)
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| command.clone())
        } else {
            spec.label.clone()
        };

        let pty = native_pty_system();
        let pair = pty
            .openpty(PtySize {
                rows: spec.rows,
                cols: spec.cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| anyhow!("openpty: {e}"))?;

        let id: SessionId =
            id.unwrap_or_else(|| uuid::Uuid::new_v4().simple().to_string()[..12].to_string());
        let (cmd, is_shell) = launch::launch_command(&spec, &command, &cwd, &id);
        let child = pair
            .slave
            .spawn_command(cmd)
            .with_context(|| format!("spawn {command} in {}", cwd.display()))?;
        drop(pair.slave);
        // From here on the session is its master fd and the child's pid. The
        // reader reaps the pid; dropping `child` neither waits nor kills.
        let pid = child.process_id();
        drop(child);
        let pty = Pty::from_master(&*pair.master)?;
        drop(pair.master);

        let info = SessionInfo {
            id: id.clone(),
            label,
            role: spec.role,
            cwd,
            command,
            foreground_program: None,
            args: spec.args,
            state: SessionState::Working,
            cols: spec.cols,
            rows: spec.rows,
            pid,
            exit_code: None,
            theme: None,
            name: spec.name.filter(|name| !name.trim().is_empty()),
            title: None,
            started_at: now_ms(),
            last_output_at: now_ms(),
            output_bytes: 0,
            away: Vec::new(),
            was: None,
            resume: None,
            unread: false,
            agent_exit: None,
            peek: spec.peek,
            interactive: false,
            input_gen: 0,
            coordinator: spec.coordinator,
            skipper: spec.skipper.filter(|p| !p.trim().is_empty()),
            retiring: false,
            watch: None,
        };

        let (input, queued, wrote) = start_writer(&id, &pty)?;
        let (output, _) = broadcast::channel(4096);
        let session = Arc::new(Session {
            info: Mutex::new(info.clone()),
            pty,
            input,
            queued,
            wrote,
            waker: Poller::new().context("poller")?,
            park: Park::default(),
            output,
            last_output: AtomicU64::new(info.last_output_at),
            last_input: AtomicU64::new(0),
            key_pending: AtomicBool::new(false),
            last_work: AtomicU64::new(info.last_output_at),
            work_started: AtomicU64::new(info.last_output_at),
            fresh: AtomicBool::new(true),
            busy_seen: AtomicBool::new(false),
            title_state: Mutex::new(None),
            shell: AtomicBool::new(is_shell),
            // The foreground watcher names the agent within a second.
            front: Mutex::new(if is_shell { Front::Shell } else { Front::Program }),
            group: AtomicU32::new(0),
            belled: AtomicBool::new(false),
            screen: Mutex::new(ScreenState {
                screen: Screen::new(spec.cols, spec.rows),
                pending: Vec::new(),
                held_since: Instant::now(),
                last_push: Instant::now(),
                last_scan: Instant::now() - SCAN,
                unscanned: false,
                printed: false,
                printed_work: false,
                echo: None,
            }),
        });
        self.sessions
            .write()
            .unwrap()
            .insert(id.clone(), session.clone());
        let _ = self.events.send(Event::SessionCreated {
            session: info.clone(),
        });
        self.dirty.notify_one();
        if info.peek {
            self.watch_peek(&id);
        }
        self.start_reader(session)?;
        Ok(info)
    }

    /// The thread that reads the PTY until the child is gone, feeds the
    /// screen, and reaps the child by its pid. It waits with a deadline only
    /// while output is held or unscanned, so an idle session never wakes.
    /// A reload can pause it between reads; see [`Session::park_point`].
    fn start_reader(self: &Arc<Self>, session: Arc<Session>) -> Result<()> {
        let mut reader = session.pty.reader().context("clone reader")?;
        // SAFETY: the reader thread owns `reader` and deletes it from the
        // poller before it closes.
        unsafe { session.waker.add(&reader, polling::Event::readable(0)) }.context("watch PTY")?;
        let pool = self.clone();
        let sid = session.info.lock().unwrap().id.clone();
        std::thread::Builder::new()
            .name(format!("pty-{sid}"))
            .spawn(move || {
                let mut buf = vec![0u8; 16 * 1024];
                let mut events = polling::Events::new();
                loop {
                    session.park_point();
                    let timeout = session
                        .next_due()
                        .map(|due| due.saturating_duration_since(Instant::now()));
                    events.clear();
                    match session.waker.wait(&mut events, timeout) {
                        Ok(_) => {}
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(_) => break,
                    }
                    // The deadline, or a wake from `pause_reader` or `set_front`.
                    if events.is_empty() {
                        pool.apply_scan(&session, session.on_due());
                        continue;
                    }
                    let n = match reader.read(&mut buf) {
                        // EIO: the child closed the terminal.
                        Ok(0) | Err(_) => break,
                        Ok(n) => n,
                    };
                    pool.on_output(&session, &sid, &buf[..n]);
                    if session.waker.modify(&reader, polling::Event::readable(0)).is_err() {
                        break;
                    }
                }
                let _ = session.waker.delete(&reader);
                drop(reader);
                session.flush_output();
                pool.reap(&session, &sid);
            })
            .context("spawn reader thread")?;
        Ok(())
    }

    /// One read's worth of output: the screen, the bell, the state, the title.
    fn on_output(&self, session: &Arc<Session>, sid: &str, chunk: &[u8]) {
        session.last_output.store(now_ms(), Ordering::Relaxed);
        {
            let mut info = session.info.lock().unwrap();
            info.output_bytes = info.output_bytes.saturating_add(chunk.len() as u64);
        }
        let (signals, scan, echo) = session.push_output(chunk);
        // A bell that answers a key is no call: the user is at the
        // keys. The BEL that ends a title sequence is not a bell.
        if signals.bell && !echo {
            self.bell(session);
        }
        // A program that starts or ends prints. A new group in front wakes
        // the foreground watcher, which reads /proc on its own thread.
        if scan.scanned {
            if let Some(group) = session.foreground_group() {
                if group != session.group.load(Ordering::Relaxed) {
                    foreground::wake();
                }
            }
        }
        self.apply_scan(session, scan);
        if let Some(title) = signals.title {
            let title = title.map(|t| t.trim().to_string()).filter(|t| !t.is_empty());
            let handoff = title.as_deref().and_then(|t| t.strip_prefix(SHELL_HANDOFF_TITLE));
            if let Some(code) = handoff {
                // The agent that owned this pane just exited and the
                // wrapper is about to exec the fallback shell in its
                // place. Forget the agent's label and command so the
                // icon falls back to a plain shell instead of the
                // agent that is no longer running. Its flags go
                // too: a restore must not pass them to the shell.
                let shell_name = PathBuf::from(default_shell())
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(default_shell);
                session.shell.store(true, Ordering::Relaxed);
                // The group stays. Forget it, so the watcher reads the
                // shell in front now instead of at its next recheck.
                session.group.store(0, Ordering::Relaxed);
                foreground::wake();
                let info = {
                    let mut info = session.info.lock().unwrap();
                    info.title = None;
                    info.label = shell_name.clone();
                    info.command = shell_name;
                    info.args.clear();
                    info.agent_exit = code.strip_prefix(':').and_then(|c| c.parse().ok());
                    info.clone()
                };
                let _ = self.events.send(Event::SessionUpdated { session: info });
                self.dirty.notify_one();
            } else {
                self.apply_title(session, title.as_deref());
                // An agent's spinner changes the glyph, not the topic. A
                // frame of it is no new title. The raw title is kept, so
                // the state rules above and the restore offer read it as
                // before.
                let agent = matches!(session.front(), Front::Agent(_));
                let changed = {
                    let mut info = session.info.lock().unwrap();
                    let changed = if agent {
                        info.title.as_deref().map(topic) != title.as_deref().map(topic)
                    } else {
                        info.title != title
                    };
                    info.title = title.clone();
                    changed
                };
                if changed {
                    let _ = self.events.send(Event::Title {
                        session: sid.to_string(),
                        title,
                    });
                }
            }
        }
    }

    /// Waits for the child to exit, then records its exit. Only this pid is
    /// reaped, never another child of the daemon. A reload can pause the
    /// wait; the exit is reaped and recorded in one step, before or after.
    fn reap(&self, session: &Arc<Session>, sid: &str) {
        let pid = session.info.lock().unwrap().pid;
        let code = loop {
            session.park_point();
            let Some(pid) = pid else { break None };
            match pty::try_reap(pid) {
                pty::Reap::Exited(code) => break Some(code),
                pty::Reap::Gone => break None,
                pty::Reap::Running => std::thread::sleep(Duration::from_millis(20)),
            }
        };
        {
            let mut info = session.info.lock().unwrap();
            info.exit_code = code;
            info.state = SessionState::Done;
        }
        session.set_reader_done();
        self.dying.lock().unwrap().retain(|s| !Arc::ptr_eq(s, session));
        let _ = self.events.send(Event::State {
            session: sid.to_string(),
            state: SessionState::Done,
        });
        let _ = self.events.send(Event::Exit { session: sid.to_string(), code });
    }

    /// A bell outside the echo of a key. A shell rings for its own reasons.
    /// A supported agent says what it wants on the screen, and its bell does
    /// not say why it rang. For every other program the bell is the call.
    fn bell(&self, session: &Session) {
        if matches!(session.front(), Front::Command | Front::Program) {
            session.belled.store(true, Ordering::Relaxed);
            self.set_state(session, SessionState::Waiting);
        }
    }

    /// Work is new text on screen. An echo of typing, a redraw after a resize,
    /// mode codes an idle program re-sends, or a logo that only changes color
    /// are not. Text at a shell prompt is not work either.
    fn apply_scan(&self, session: &Session, scan: Scan) {
        if !scan.scanned {
            return;
        }
        let front = session.front();
        if front == Front::Shell {
            return;
        }
        if scan.work {
            session.last_work.store(now_ms(), Ordering::Relaxed);
        }
        if scan.busy {
            session.busy_seen.store(true, Ordering::Relaxed);
        }
        let waiting = session.info.lock().unwrap().state == SessionState::Waiting;
        let asks = *session.title_state.lock().unwrap() == Some(TitleState::Action);
        if scan.approval || asks {
            session.belled.store(false, Ordering::Relaxed);
            self.set_state(session, SessionState::Waiting);
        } else if waiting && matches!(front, Front::Agent(_)) && !session.belled.load(Ordering::Relaxed) {
            // The prompt left the screen: the user answered it.
            session.last_work.store(now_ms(), Ordering::Relaxed);
            self.set_state(session, SessionState::Working);
        } else if scan.work && !waiting {
            self.set_state(session, SessionState::Working);
        }
    }

    /// The window title of a supported agent: busy keeps it working and
    /// proves a turn, an action-required title sets the bell.
    fn apply_title(&self, session: &Session, title: Option<&str>) {
        let Front::Agent(agent) = session.front() else { return };
        let state = title.and_then(|t| title_state(agent, t));
        let old = std::mem::replace(&mut *session.title_state.lock().unwrap(), state);
        if old == state {
            return;
        }
        match state {
            Some(TitleState::Busy) => {
                session.busy_seen.store(true, Ordering::Relaxed);
                session.last_work.store(now_ms(), Ordering::Relaxed);
                if session.info.lock().unwrap().state != SessionState::Waiting {
                    self.set_state(session, SessionState::Working);
                }
            }
            Some(TitleState::Action) => {
                session.belled.store(false, Ordering::Relaxed);
                self.set_state(session, SessionState::Waiting);
            }
            Some(TitleState::Ready) | None => {
                // The busy title kept the session working. Its end starts the
                // quiet time now, so the turn ends IDLE_AFTER from here.
                if old == Some(TitleState::Busy) {
                    session.last_work.store(now_ms(), Ordering::Relaxed);
                }
            }
        }
    }

    fn set_state(&self, session: &Session, state: SessionState) {
        let id = {
            let mut info = session.info.lock().unwrap();
            if info.state == state || info.state == SessionState::Done {
                return;
            }
            if state == SessionState::Working && info.state == SessionState::Idle {
                session.work_started.store(now_ms(), Ordering::Relaxed);
            }
            info.state = state;
            info.id.clone()
        };
        let _ = self.events.send(Event::State { session: id, state });
    }

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
            let changed = info.unread || info.agent_exit.is_some();
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

    pub fn write(&self, id: &str, data: &[u8]) -> Result<()> {
        let _change = self.change()?;
        let session = self.get(id).ok_or_else(|| anyhow!("no such session: {id}"))?;
        {
            // The info lock orders this write against a submit's check.
            let mut info = session.info.lock().unwrap();
            info.input_gen += 1;
            session.send(id, vec![Input::Bytes(data.to_vec())])?;
        }
        // Input answers a bell. An agent's prompt goes when the screen says so.
        if session.belled.swap(false, Ordering::Relaxed)
            && session.info.lock().unwrap().state == SessionState::Waiting
        {
            self.set_state(&session, SessionState::Working);
        }
        Ok(())
    }

    /// Types `data` and, with `enter`, presses Enter [`ENTER_DELAY`] later,
    /// only if the session took no input since the client read
    /// `input_gen == expect`. Input that comes during the delay waits behind
    /// the Enter. `Ok(false)`: refused, and nothing typed.
    pub fn submit(&self, id: &str, data: &[u8], paste: bool, enter: bool, expect: u64) -> Result<bool> {
        let _change = self.change()?;
        let session = self.get(id).ok_or_else(|| anyhow!("no such session: {id}"))?;
        let data = if paste && session.screen.lock().unwrap().screen.bracketed_paste() {
            bracketed(data)
        } else {
            data.to_vec()
        };
        let mut info = session.info.lock().unwrap();
        if info.input_gen != expect || info.state == SessionState::Done {
            return Ok(false);
        }
        info.input_gen += 1;
        let mut items = vec![Input::Bytes(data)];
        if enter {
            items.extend([Input::Pause(ENTER_DELAY), Input::Bytes(b"\r".to_vec())]);
        }
        session.send(id, items)?;
        Ok(true)
    }

    /// Writes `data` as a paste: inside bracketed paste when the program
    /// asked for it, else as typed text.
    pub fn paste(&self, id: &str, data: &[u8]) -> Result<()> {
        let session = self.get(id).ok_or_else(|| anyhow!("no such session: {id}"))?;
        if !session.screen.lock().unwrap().screen.bracketed_paste() {
            return self.write(id, data);
        }
        self.write(id, &bracketed(data))
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

    /// The emulator reflows 2000 lines of scrollback, so the whole resize
    /// runs on a blocking thread and the executor serves other sessions
    /// meanwhile. The change guard holds a reload until the pty, the screen
    /// and the info agree. The screen lock covers the ioctl and the reflow,
    /// so the redraw the program sends for the new size lands on a screen
    /// of that size. It also covers the info and the event: two clients
    /// resizing at once apply and announce in the same order, so the last
    /// event every client hears is the size the pty has.
    pub async fn resize(self: &Arc<Self>, id: &str, cols: u16, rows: u16) -> Result<()> {
        let pool = self.clone();
        let id = id.to_string();
        tokio::task::spawn_blocking(move || {
            let _change = pool.change()?;
            let session = pool.get(&id).ok_or_else(|| anyhow!("no such session: {id}"))?;
            // The program redraws for the new size. That is not work either.
            // Stored before the ioctl, as a write does.
            session.last_input.store(now_ms(), Ordering::Relaxed);
            let mut st = session.screen.lock().unwrap();
            session.pty.resize(cols, rows)?;
            st.screen.resize(cols, rows);
            {
                let mut info = session.info.lock().unwrap();
                info.cols = cols;
                info.rows = rows;
            }
            let _ = pool.events.send(Event::SessionUpdated { session: session.info() });
            drop(st);
            Ok::<_, anyhow::Error>(())
        })
        .await
        .map_err(|e| anyhow!("resize failed: {e}"))??;
        Ok(())
    }

    /// Terminates the process and forgets the session. Drops it from every
    /// group; a group left with no session keeps an empty slot.
    pub fn kill(&self, id: &str) -> Result<()> {
        let _change = self.change()?;
        let mut groups = self.groups.lock().unwrap();
        let session = self
            .sessions
            .write()
            .unwrap()
            .remove(id)
            .ok_or_else(|| anyhow!("no such session: {id}"))?;
        // A reaped pid can belong to another process by now.
        let (pid, cwd) = {
            let info = session.info.lock().unwrap();
            (info.pid.filter(|_| info.state != SessionState::Done), info.cwd.clone())
        };
        if let Some(pid) = pid {
            pty::hang_up(pid);
        }
        if !session.reader_done() {
            self.dying.lock().unwrap().push(session.clone());
        }
        let pruned = prune(&mut groups, id, &cwd);
        drop(groups);
        let _ = self.events.send(Event::SessionRemoved {
            session: id.to_string(),
        });
        if pruned {
            let _ = self.events.send(Event::GroupsChanged {});
        }
        self.dirty.notify_one();
        Ok(())
    }

    /// A peek tool that takes the terminal is a terminal tool: the app shows
    /// its peek then. One that never does is an app with a window of its own,
    /// or a command that prints and exits.
    fn watch_peek(self: &Arc<Self>, id: &str) {
        let pool = Arc::downgrade(self);
        let sid = id.to_string();
        let _ = std::thread::Builder::new().name(format!("peek-{sid}")).spawn(move || loop {
            std::thread::sleep(Duration::from_millis(40));
            let Some(pool) = pool.upgrade() else { break };
            let Some(s) = pool.get(&sid) else { break };
            if s.info.lock().unwrap().state == SessionState::Done {
                break;
            }
            if s.takes_terminal() {
                let session = {
                    let mut info = s.info.lock().unwrap();
                    info.interactive = true;
                    info.clone()
                };
                let _ = pool.events.send(Event::SessionUpdated { session });
                break;
            }
        });
    }

    /// Flips quiet `working` sessions to `idle`. For an agent, the end of a
    /// stretch of work is the end of its turn: a result to read.
    pub fn spawn_idle_watcher(self: &Arc<Self>) {
        let pool = self.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(1));
            loop {
                tick.tick().await;
                {
                    let Some(_quiet) = pool.quiet() else { continue };
                    let sessions: Vec<Arc<Session>> =
                        pool.sessions.read().unwrap().values().cloned().collect();
                    for s in sessions {
                        pool.idle_if_quiet(&s);
                    }
                }
                // Out of the guard: a kill takes it again.
                pool.end_retired();
            }
        });
    }

    fn idle_if_quiet(&self, s: &Session) {
        let front = s.front();
        // The title says the agent works: a quiet tool call is still work.
        if *s.title_state.lock().unwrap() == Some(TitleState::Busy) {
            return;
        }
        // Check and flip under one lock, so a bell or output the reader
        // handles meanwhile is not overwritten with `idle`.
        let (id, updated, turn) = {
            let mut info = s.info.lock().unwrap();
            let last = s.last_work.load(Ordering::Relaxed);
            let quiet = now_ms().saturating_sub(last) > IDLE_AFTER.as_millis() as u64;
            if !quiet || info.state != SessionState::Working {
                return;
            }
            info.state = SessionState::Idle;
            let stretch = last.saturating_sub(s.work_started.load(Ordering::Relaxed));
            let fresh = s.fresh.swap(false, Ordering::Relaxed);
            let busy = s.busy_seen.swap(false, Ordering::Relaxed);
            let turn = match front {
                // The busy line proves a turn, even a fast one, or the first
                // after a resume. Without it, text is a recap or a redraw.
                Front::Agent(agent) if crate::screen::has_busy_line(agent) => busy,
                _ => !fresh && match front {
                    // An agent with no known busy line redraws its status
                    // line while it does nothing.
                    Front::Agent(_) => stretch >= MIN_TURN_MS,
                    Front::Program => true,
                    // A quiet command in a shell has not finished. Its end
                    // shows when the shell is in front again.
                    Front::Shell | Front::Command => false,
                },
            };
            let updated = (turn && !info.unread).then(|| {
                info.unread = true;
                info.clone()
            });
            (info.id.clone(), updated, turn)
        };
        let _ = self.events.send(Event::State { session: id.clone(), state: SessionState::Idle });
        if turn {
            let _ = self.events.send(Event::TurnEnded { session: id });
        }
        if let Some(session) = updated {
            let _ = self.events.send(Event::SessionUpdated { session });
        }
    }
}

/// One item for the writer thread. A pause holds the input behind it, so a
/// submit's Enter comes after its text with nothing typed between.
pub(crate) enum Input {
    Bytes(Vec<u8>),
    Pause(Duration),
}

/// The thread that writes input to the PTY, its count of writes queued, and
/// when it first wrote since the last output. A program that stops reading its input blocks this
/// thread alone. Input after the PTY is gone is dropped and still counted
/// down, so a reload never waits on a dead session.
fn start_writer(id: &str, pty: &Pty) -> Result<(std::sync::mpsc::Sender<Input>, Arc<AtomicUsize>, Arc<AtomicU64>)> {
    let mut writer = pty.writer().context("clone writer")?;
    let queued = Arc::new(AtomicUsize::new(0));
    let wrote = Arc::new(AtomicU64::new(0));
    let wrote_at = wrote.clone();
    let (input, input_rx) = std::sync::mpsc::channel::<Input>();
    let count = queued.clone();
    std::thread::Builder::new()
        .name(format!("pty-in-{id}"))
        .spawn(move || {
            let mut open = true;
            // Ends when the session is dropped.
            for item in input_rx {
                match item {
                    Input::Bytes(data) => {
                        // Before the write: a terminal in canonical mode echoes
                        // inside it, so the reader can see the answer first.
                        // The first write since the last read: keys typed while
                        // the program is silent wait on the program, and the
                        // trace must not count that wait as skiffd's.
                        let _ = wrote_at.compare_exchange(0, now_us(), Ordering::Relaxed, Ordering::Relaxed);
                        if open && writer.write_all(&data).and_then(|_| writer.flush()).is_err() {
                            open = false;
                        }
                    }
                    Input::Pause(d) => std::thread::sleep(d),
                }
                count.fetch_sub(1, Ordering::AcqRel);
            }
        })
        .context("spawn writer thread")?;
    Ok((input, queued, wrote))
}

/// True when the bytes put text on the screen. Escape sequences and control
/// characters alone do not. A sequence split across reads may misjudge one chunk.
fn prints(bytes: &[u8]) -> bool {
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            0x1b => {
                i += 1;
                match bytes.get(i) {
                    // CSI: parameters, then one final byte in 0x40..=0x7e.
                    Some(b'[') => {
                        i += 1;
                        while i < bytes.len() && !(0x40..=0x7e).contains(&bytes[i]) {
                            i += 1;
                        }
                    }
                    // OSC, DCS, APC, PM, SOS: up to BEL or ST.
                    Some(b']' | b'P' | b'_' | b'^' | b'X') => {
                        while i < bytes.len() && bytes[i] != 0x07 && !(bytes[i] == b'\\' && bytes[i - 1] == 0x1b) {
                            i += 1;
                        }
                    }
                    // Charset designation: one more byte.
                    Some(b'(' | b')' | b'*' | b'+' | b'#' | b'%') => i += 1,
                    _ => {}
                }
            }
            b if b < 0x20 || b == 0x7f => {}
            _ => return true,
        }
        i += 1;
    }
    false
}

/// `data` between bracketed paste marks. An end mark inside the data would
/// end the paste early and let the rest run as keys, so it is taken out.
fn bracketed(data: &[u8]) -> Vec<u8> {
    const END: &[u8] = b"\x1b[201~";
    let mut out = b"\x1b[200~".to_vec();
    let mut i = 0;
    while i < data.len() {
        if data[i..].starts_with(END) {
            i += END.len();
        } else {
            out.push(data[i]);
            i += 1;
        }
    }
    out.extend_from_slice(END);
    out
}

/// The title without the status glyph an agent leads it with: Claude's ◐
/// and ◑, Codex's braille spinner. See `screen::title_state`.
fn topic(title: &str) -> &str {
    title.trim_start_matches(|c: char| !c.is_alphanumeric())
}

fn default_shell() -> String {
    std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string())
}

#[cfg(test)]
mod tests {
    use super::{prints, topic, Front, Screen, ScreenState, HOLD, QUIET, SCAN};
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
