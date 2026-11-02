//! Reload: skiffd execs a new binary in place and keeps its sessions.
//!
//! The pid, the children, their PTYs and the listening socket stay. The old
//! image writes everything else to a private handover file, the new image
//! reads it back and carries on. In order:
//!
//! 1. Preflight: the target runs with `--reload-info` and must accept this
//!    handover version and encode a terminal the same way.
//! 2. Quiesce: changes and spawns are refused, the ones in flight finish.
//! 3. Drain: every write accepted so far reaches its PTY.
//! 4. Park: each reader thread pauses between reads, or has reaped its child.
//! 5. Hand over: the workspace is saved and the handover file is written.
//!    The target must read that file back (`--reload-info <file>`). Then
//!    only the PTY masters and the listener lose close-on-exec.
//! 6. Exec. Each step before it undoes itself on failure, and the old daemon
//!    carries on as before. After exec there is no way back.
//!
//! The new image skips the shell probe, the socket bind and the workspace
//! restore. It adopts the fds, terminals and detector state, and reaps each
//! child by its pid.

use std::{
    convert::Infallible,
    fs::File,
    io::{BufWriter, Read, Write},
    os::unix::{fs::OpenOptionsExt, process::CommandExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

use anyhow::{anyhow, bail, Context, Result};
use polling::Poller;
use serde::{Deserialize, Serialize};
use skiff_core::{group::Group, session::SessionInfo};
use tokio::sync::broadcast;

use super::{foreground, groups::prune, start_writer, Front, Park, ScreenState, Session, SessionPool, SCAN};
use crate::{
    pty::{self, Pty},
    screen::{Screen, TitleState},
    shellenv,
};

/// The handover file's format. Raise it when `Meta` or `SessionMeta` change
/// in a way an older or newer daemon would misread. A change to the terminal
/// state shows in [`fingerprint`] on its own.
pub const HANDOVER: u32 = 3;

/// Handover versions this binary reads. The daemon that runs when the user
/// installs a new Skiff writes an older version, so keep each one this code
/// still reads correctly: without it, the update cannot finish on its own
/// and every session must stop (#182). Version 3 only added fields with a
/// default (`Meta::started_at`, `SessionInfo::output_bytes`), so 2 reads as 3.
const READS: [u32; 2] = [2, HANDOVER];

const MAGIC: &[u8; 8] = b"SKIFFHO\n";
const PREFLIGHT_TIMEOUT: Duration = Duration::from_secs(5);
const DRAIN_TIMEOUT: Duration = Duration::from_secs(3);
const PARK_TIMEOUT: Duration = Duration::from_secs(3);

/// What `skiffd --reload-info` prints, as one line of JSON.
#[derive(Serialize, Deserialize, Debug)]
pub struct ReloadInfo {
    pub version: String,
    /// Handover versions this binary can adopt.
    pub handover: Vec<u32>,
    /// See [`fingerprint`].
    pub fingerprint: String,
}

pub fn reload_info() -> ReloadInfo {
    ReloadInfo {
        version: skiff_core::VERSION.to_string(),
        handover: READS.to_vec(),
        fingerprint: fingerprint(),
    }
}

/// A hash of one fixed terminal's saved state. Two binaries with the same
/// fingerprint encode terminals the same way, so one can adopt the other's.
/// The terminal uses both screens, history, a saved cursor, a scroll region,
/// charsets, colors, a title stack, mouse, paste and kitty keyboard modes,
/// and ends inside an escape sequence.
fn fingerprint() -> String {
    let mut screen = Screen::new(12, 4);
    screen.feed(
        b"\x1b]0;t\x07\x1b[22;0t\x1b]2;u\x07one\r\ntwo\r\nthree\r\nfour\r\n\x1b[1;31;48;2;1;2;3mfive\x1b[0m\
          \x1b[2;3r\x1b7\x1b(0q\x1b(B\x1b]4;1;rgb:10/20/30\x1b\\\x1b[?2004h\x1b[?1000h\x1b[?1006h\
          \x1b[>5u\x1b[4 q\x1b]8;id=x;https://e\x1b\\link\x1b]8;;\x1b\\\x1b[?1049h\x1b[>1u\x1b[2;2Halt\x1b[3",
    );
    let bytes = screen.save().unwrap_or_default();
    // FNV-1a: stable across Rust releases, unlike the std hasher.
    let mut h: u64 = 0xcbf29ce484222325;
    for b in bytes {
        h = (h ^ b as u64).wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}")
}

/// The handover file's JSON part.
#[derive(Serialize, Deserialize)]
struct Meta {
    listener: i32,
    /// The workspace file the old image saved to. The new image keeps it,
    /// whatever a debug or release build would pick on its own.
    #[serde(default)]
    state: Option<PathBuf>,
    aliases: String,
    #[serde(default)]
    started_at: u64,
    groups: Vec<Group>,
    sessions: Vec<SessionMeta>,
    /// Killed sessions' children not reaped yet. The new image reaps them.
    orphans: Vec<u32>,
}

/// A session besides its terminal. The fields after `done` are the
/// detector's, so a reload marks nothing waiting or unread on its own.
#[derive(Serialize, Deserialize)]
struct SessionMeta {
    info: SessionInfo,
    /// The PTY master, inherited at this number.
    fd: i32,
    /// The child is reaped and its exit recorded.
    done: bool,
    last_output: u64,
    last_work: u64,
    last_input: u64,
    work_started: u64,
    fresh: bool,
    busy_seen: bool,
    title_state: Option<TitleState>,
    shell: bool,
    front: FrontSave,
    group: u32,
    belled: bool,
    unscanned: bool,
    printed: bool,
    printed_work: bool,
}

#[derive(Serialize, Deserialize)]
enum FrontSave {
    Shell,
    Command,
    Agent(String),
    Program,
}

/// A handover file read back: the JSON part, and where each session's
/// terminal lies in the file's bytes.
pub struct Handover {
    meta: Meta,
    bytes: Vec<u8>,
    screens: Vec<std::ops::Range<usize>>,
}

impl Handover {
    pub fn listener(&self) -> i32 {
        self.meta.listener
    }

    pub fn state(&self) -> Option<&Path> {
        self.meta.state.as_deref()
    }
}

/// The handover file of this process, next to the socket. That folder is
/// the daemon's own.
fn handover_path() -> PathBuf {
    let socket = crate::paths::socket();
    socket.with_file_name(format!("skiffd-handover-{}", std::process::id()))
}

impl SessionPool {
    /// Moves the daemon onto `binary`, or onto its own executable when
    /// `None`. Returns only on failure, with the daemon as it was.
    pub fn reload(self: &Arc<Self>, binary: Option<PathBuf>) -> Result<Infallible> {
        let _one = self.reload.try_lock().map_err(|_| anyhow!("a reload is already running"))?;
        let binary = target(binary)?;
        preflight(&binary, None)?;
        self.reloading.store(true, Ordering::Release);
        // Wait out the changes and spawns that started before the flag.
        drop(self.changes.write().unwrap());
        let err = self.hand_over(&binary);
        self.reloading.store(false, Ordering::Release);
        // A save skipped while the reload ran.
        self.dirty.notify_one();
        tracing::warn!("reload onto {} failed: {err:#}", binary.display());
        Err(err)
    }

    fn hand_over(self: &Arc<Self>, binary: &Path) -> anyhow::Error {
        let mut sessions: Vec<Arc<Session>> = self.sessions.read().unwrap().values().cloned().collect();
        sessions.sort_by_key(|s| s.info.lock().unwrap().id.clone());
        let dying: Vec<Arc<Session>> = self.dying.lock().unwrap().clone();

        let until = Instant::now() + DRAIN_TIMEOUT;
        for s in &sessions {
            while s.queued.load(Ordering::Acquire) > 0 {
                if Instant::now() >= until {
                    return anyhow!("input to session {} did not reach its terminal", s.info.lock().unwrap().id);
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        }

        let until = Instant::now() + PARK_TIMEOUT;
        let all = || sessions.iter().chain(&dying);
        let paused = all().try_for_each(|s| s.pause_reader(until));
        let err = match paused {
            Ok(()) => self.exec(binary, &sessions, &dying),
            Err(e) => e,
        };
        all().for_each(|s| s.resume_reader());
        err
    }

    /// Writes the handover and execs. Returns only on failure, after it
    /// restored close-on-exec and removed the file.
    fn exec(&self, binary: &Path, sessions: &[Arc<Session>], dying: &[Arc<Session>]) -> anyhow::Error {
        // Output held back by coalescing goes to the clients now. It is in
        // the terminal state as well, which a reconnecting client redraws.
        for s in sessions {
            s.screen.lock().unwrap().flush(&s.output);
        }
        self.save_now();
        let path = handover_path();
        let listener = self.listener.load(Ordering::Relaxed);
        let fds = match self.write_handover(&path, listener, sessions, dying) {
            Ok(fds) => fds,
            Err(e) => {
                let _ = std::fs::remove_file(&path);
                return e.context("write handover");
            }
        };
        // The metadata holds skiff-core types that change with the protocol,
        // so the target proves it reads this very file before exec.
        if let Err(e) = preflight(binary, Some(&path)) {
            let _ = std::fs::remove_file(&path);
            return e;
        }
        let mut inherited = Vec::new();
        let err = (|| {
            for fd in fds.iter().copied().chain((listener >= 0).then_some(listener)) {
                pty::set_inherit(fd, true).with_context(|| format!("pass fd {fd} on"))?;
                inherited.push(fd);
            }
            tracing::info!("reloading onto {}", binary.display());
            let e = Command::new(binary).arg("--adopt").arg(&path).exec();
            Err::<(), _>(anyhow!(e).context(format!("exec {}", binary.display())))
        })()
        .unwrap_err();
        for fd in inherited {
            let _ = pty::set_inherit(fd, false);
        }
        let _ = std::fs::remove_file(&path);
        err
    }

    /// Writes the file, private to the user, and returns the PTY fds it names.
    fn write_handover(&self, path: &Path, listener: i32, sessions: &[Arc<Session>], dying: &[Arc<Session>]) -> Result<Vec<i32>> {
        let _ = std::fs::remove_file(path);
        let file = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(path)?;
        // The readers are paused, so the screens stay as the metas describe
        // them while each is saved and written in turn.
        let mut metas = Vec::with_capacity(sessions.len());
        for s in sessions {
            let st = s.screen.lock().unwrap();
            let front = match s.front() {
                Front::Shell => FrontSave::Shell,
                Front::Command => FrontSave::Command,
                Front::Agent(a) => FrontSave::Agent(a.to_string()),
                Front::Program => FrontSave::Program,
            };
            metas.push(SessionMeta {
                // Watchers register again on the new process.
                info: SessionInfo { watch: None, ..s.info() },
                fd: s.pty.raw_fd(),
                done: s.reader_done(),
                last_output: s.last_output.load(Ordering::Relaxed),
                last_work: s.last_work.load(Ordering::Relaxed),
                last_input: s.last_input.load(Ordering::Relaxed),
                work_started: s.work_started.load(Ordering::Relaxed),
                fresh: s.fresh.load(Ordering::Relaxed),
                busy_seen: s.busy_seen.load(Ordering::Relaxed),
                title_state: *s.title_state.lock().unwrap(),
                shell: s.shell.load(Ordering::Relaxed),
                front,
                group: s.group.load(Ordering::Relaxed),
                belled: s.belled.load(Ordering::Relaxed),
                unscanned: st.unscanned,
                printed: st.printed,
                printed_work: st.printed_work,
            });
        }
        // The reaper skips its turn while a reload runs, so no pid listed
        // here is reaped before exec.
        let mut orphans: Vec<u32> = self.orphans.lock().unwrap().clone();
        orphans.extend(
            dying
                .iter()
                .filter(|s| !s.reader_done())
                .filter_map(|s| s.info.lock().unwrap().pid),
        );
        let meta = Meta {
            listener,
            state: Some(crate::paths::state()),
            aliases: shellenv::aliases().to_string(),
            started_at: self.started_at.load(Ordering::Relaxed),
            groups: self.groups.lock().unwrap().clone(),
            sessions: metas,
            orphans,
        };
        let fds = meta.sessions.iter().map(|m| m.fd).collect();
        let json = serde_json::to_vec(&meta)?;
        let mut w = BufWriter::new(file);
        w.write_all(MAGIC)?;
        w.write_all(&HANDOVER.to_le_bytes())?;
        w.write_all(&(json.len() as u64).to_le_bytes())?;
        w.write_all(&json)?;
        for s in sessions {
            let screen = s.screen.lock().unwrap().screen.save()?;
            w.write_all(&(screen.len() as u64).to_le_bytes())?;
            w.write_all(&screen)?;
        }
        w.into_inner().map_err(|e| e.into_error())?.sync_all()?;
        Ok(fds)
    }

    /// Takes the sessions over from a handover. A session that cannot be
    /// adopted is hung up and leaves its groups, as a failed restore does.
    pub fn adopt(self: &Arc<Self>, handover: Handover) {
        let Handover { meta, bytes, screens } = handover;
        if meta.started_at != 0 { self.started_at.store(meta.started_at, Ordering::Relaxed); }
        shellenv::set_aliases(&meta.aliases);
        let mut groups = meta.groups;
        let mut orphans = meta.orphans;
        let total = meta.sessions.len();
        let mut adopted = 0;
        for (m, screen) in meta.sessions.into_iter().zip(screens) {
            let (id, cwd, pid, done) = (m.info.id.clone(), m.info.cwd.clone(), m.info.pid, m.done);
            // SAFETY: the old image passed this fd on for this session alone.
            let result = unsafe { Pty::adopt(m.fd) }
                .context("PTY")
                .and_then(|pty| self.adopt_session(m, pty, &bytes[screen]));
            match result {
                Ok(()) => adopted += 1,
                Err(e) => {
                    // The PTY, if adopted, closed with the error.
                    tracing::error!("reload lost session {id}: {e:#}");
                    if let Some(pid) = pid.filter(|_| !done) {
                        pty::hang_up(pid);
                        orphans.push(pid);
                    }
                    prune(&mut groups, &id, &cwd);
                }
            }
        }
        *self.groups.lock().unwrap() = groups;
        self.reap_orphans(orphans);
        tracing::info!("adopted {adopted} of {total} sessions");
        if adopted < total {
            self.dirty.notify_one();
        }
    }

    /// Reaps children of sessions that were killed before the reload, each
    /// by its pid. A pid stays ours until reaped, so no other child can take
    /// it. The list lives in the pool, so a second reload hands it on.
    fn reap_orphans(self: &Arc<Self>, pids: Vec<u32>) {
        if pids.is_empty() {
            return;
        }
        *self.orphans.lock().unwrap() = pids;
        let pool = Arc::downgrade(self);
        let _ = std::thread::Builder::new().name("orphans".into()).spawn(move || loop {
            let Some(pool) = pool.upgrade() else { return };
            {
                let mut pids = pool.orphans.lock().unwrap();
                // A reload lists these pids for the next image; leave them.
                if !pool.reloading.load(Ordering::Acquire) {
                    pids.retain(|&pid| matches!(pty::try_reap(pid), pty::Reap::Running));
                }
                if pids.is_empty() {
                    return;
                }
            }
            drop(pool);
            std::thread::sleep(Duration::from_millis(100));
        });
    }

    fn adopt_session(self: &Arc<Self>, m: SessionMeta, pty: Pty, screen: &[u8]) -> Result<()> {
        let screen = Screen::restore(screen)?;
        let id = m.info.id.clone();
        let (input, queued, wrote) = start_writer(&id, &pty)?;
        let front = match m.front {
            FrontSave::Shell => Front::Shell,
            FrontSave::Command => Front::Command,
            FrontSave::Agent(a) => foreground::known(&a).map_or(Front::Program, Front::Agent),
            FrontSave::Program => Front::Program,
        };
        let (output, _) = broadcast::channel(4096);
        let info = m.info;
        let session = Arc::new(Session {
            pty,
            input,
            queued,
            wrote,
            waker: Poller::new().context("poller")?,
            park: Park::default(),
            output,
            last_output: AtomicU64::new(m.last_output),
            last_work: AtomicU64::new(m.last_work),
            last_input: AtomicU64::new(m.last_input),
            key_pending: AtomicBool::new(false),
            work_started: AtomicU64::new(m.work_started),
            fresh: AtomicBool::new(m.fresh),
            busy_seen: AtomicBool::new(m.busy_seen),
            title_state: Mutex::new(m.title_state),
            shell: AtomicBool::new(m.shell),
            front: Mutex::new(front),
            group: AtomicU32::new(m.group),
            belled: AtomicBool::new(m.belled),
            // Answers to queries before the reload count as input, as before.
            queries: AtomicU32::new(0),
            queried_at: AtomicU64::new(0),
            screen: Mutex::new(ScreenState {
                screen,
                pending: Vec::new(),
                held_since: Instant::now(),
                last_push: Instant::now(),
                last_scan: Instant::now() - SCAN,
                unscanned: m.unscanned,
                printed: m.printed,
                printed_work: m.printed_work,
                echo: None,
            }),
            info: Mutex::new(info.clone()),
        });
        self.sessions.write().unwrap().insert(id.clone(), session.clone());
        if m.done {
            session.set_reader_done();
            return Ok(());
        }
        if info.peek && !info.interactive {
            self.watch_peek(&id);
        }
        self.start_reader(session)
    }
}

/// Reads a handover file. The caller removes it.
pub fn load(path: &Path) -> Result<Handover> {
    let mut bytes = Vec::new();
    File::open(path)?.read_to_end(&mut bytes)?;
    let mut at = 0;
    let mut take = |n: usize| -> Result<std::ops::Range<usize>> {
        if bytes.len() - at < n {
            bail!("handover file is cut short");
        }
        at += n;
        Ok(at - n..at)
    };
    let word = |r: std::ops::Range<usize>| u64::from_le_bytes(bytes[r].try_into().unwrap_or([0; 8]));
    if bytes[take(MAGIC.len())?] != MAGIC[..] {
        bail!("not a handover file");
    }
    let version = u32::from_le_bytes(bytes[take(4)?].try_into()?);
    if !READS.contains(&version) {
        bail!("handover version {version}; this skiffd reads {READS:?}");
    }
    let len = word(take(8)?) as usize;
    let meta: Meta = serde_json::from_slice(&bytes[take(len)?])?;
    let mut screens = Vec::with_capacity(meta.sessions.len());
    for _ in 0..meta.sessions.len() {
        let len = word(take(8)?) as usize;
        screens.push(take(len)?);
    }
    Ok(Handover { meta, bytes, screens })
}

/// The binary to exec. Without one, this daemon's own executable as it is
/// on disk now, so a rebuild in place is picked up.
fn target(binary: Option<PathBuf>) -> Result<PathBuf> {
    let path = match binary {
        Some(p) if !p.is_absolute() => bail!("the binary must be an absolute path: {}", p.display()),
        Some(p) => p,
        None => {
            let exe = std::env::current_exe().context("find this skiffd")?;
            // Linux names a replaced executable "<path> (deleted)".
            match exe.to_str().and_then(|s| s.strip_suffix(" (deleted)")) {
                Some(s) => PathBuf::from(s),
                None => exe,
            }
        }
    };
    if !path.is_file() {
        bail!("no skiffd at {}", path.display());
    }
    Ok(path)
}

/// Asks the target what it can adopt. Never starts a daemon: `--reload-info`
/// prints and exits before anything else. With `handover`, the target must
/// also read that file as `--adopt` would.
fn preflight(binary: &Path, handover: Option<&Path>) -> Result<()> {
    let mut child = Command::new(binary)
        .arg("--reload-info")
        .args(handover)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("run {}", binary.display()))?;
    let (status, out) = match crate::command::output(&mut child, PREFLIGHT_TIMEOUT) {
        Ok(output) => output,
        Err(e) => {
            let _ = child.kill();
            let _ = child.wait();
            if e.kind() == std::io::ErrorKind::TimedOut {
                bail!("{} did not answer --reload-info within {}s", binary.display(), PREFLIGHT_TIMEOUT.as_secs());
            }
            return Err(e.into());
        }
    };
    let info: ReloadInfo = match serde_json::from_slice(&out) {
        Ok(info) if status.success() => info,
        Ok(info) if handover.is_some() => bail!("skiffd {} cannot read this daemon's handover", info.version),
        _ => bail!("{} cannot adopt sessions: it does not support reload", binary.display()),
    };
    if !info.handover.contains(&HANDOVER) {
        bail!("skiffd {} cannot adopt this daemon's sessions (handover {HANDOVER}, it reads {:?})", info.version, info.handover);
    }
    if info.fingerprint != fingerprint() {
        bail!("skiffd {} saves terminals another way; it cannot adopt these sessions", info.version);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A handover from a skiffd before version 3 (0.6.31 and older) loads,
    /// with the fields that version added at their defaults (#182).
    #[test]
    fn reads_a_version_2_handover() {
        // A 0.6.31 session, with no `output_bytes`.
        let info = serde_json::json!({
            "id": "s1", "label": "t", "role": "shell", "cwd": "/", "command": "/bin/sh", "args": [],
            "state": "idle", "cols": 80, "rows": 24, "pid": 1, "exit_code": null, "started_at": 1, "last_output_at": 1,
        });
        let meta = serde_json::json!({
            "listener": 3, "state": null, "aliases": "", "groups": [], "orphans": [],
            "sessions": [{
                "info": info, "fd": 4, "done": false, "last_output": 0, "last_work": 0, "last_input": 0,
                "work_started": 0, "fresh": false, "busy_seen": false, "title_state": null, "shell": true,
                "front": "Shell", "group": 0, "belled": false, "unscanned": false, "printed": false, "printed_work": false,
            }],
        });
        let json = serde_json::to_vec(&meta).unwrap();
        let mut bytes = MAGIC.to_vec();
        bytes.extend(2u32.to_le_bytes());
        bytes.extend((json.len() as u64).to_le_bytes());
        bytes.extend(&json);
        bytes.extend(3u64.to_le_bytes());
        bytes.extend(b"scr");
        let path = std::env::temp_dir().join(format!("skiffd-handover-v2-{}", std::process::id()));
        std::fs::write(&path, &bytes).unwrap();
        let loaded = load(&path);
        let _ = std::fs::remove_file(&path);
        let h = loaded.unwrap();
        assert_eq!((h.meta.started_at, h.meta.sessions[0].info.output_bytes), (0, 0));
        assert_eq!(&h.bytes[h.screens[0].clone()], b"scr");
        assert!(reload_info().handover.contains(&2));
    }

    #[test]
    fn fingerprint_is_stable() {
        assert_eq!(super::fingerprint(), super::fingerprint());
        assert_ne!(super::fingerprint(), format!("{:016x}", 0xcbf29ce484222325u64));
    }
}
