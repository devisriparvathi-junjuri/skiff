//! Skipper watcher ownership, locks, restart policy and detached processes.

use super::*;

/// The watcher that holds the lock, as it writes itself into the lock file.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct WatchHolder {
    pub pid: u32,
    /// The daemon socket it watches.
    pub socket: String,
    /// Unix time in milliseconds.
    pub started_at: u64,
    /// The CLI version it runs. `None` from a watcher older than this field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Its executable, with links resolved. `None` from an older watcher.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exe: Option<String>,
    /// The build of its executable when it started ([`build_of`]). `None`
    /// from an older watcher: that counts as another build.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build: Option<String>,
}

/// The CLI version this crate builds with: the app's and the CLI's.
pub const CLI_VERSION: &str = env!("CARGO_PKG_VERSION");

/// The watch lock beside the socket, as [`watch_status`] reads it.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct WatchLock {
    /// A process holds the lock: a watcher runs for this daemon, registered or not.
    pub held: bool,
    /// What the holder wrote. `None` when the lock is free, or when a watcher
    /// from before this record holds it.
    pub holder: Option<WatchHolder>,
}

/// How long [`restart_watch`] waits for a signalled watcher to let go of its lock.
pub const RESTART_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

/// What [`ensure_watch`] did.
#[derive(Debug, Clone, PartialEq)]
pub enum WatchOutcome {
    /// A watcher of this build, or of a newer version, holds the lock.
    Current,
    /// None ran. It started one.
    Started,
    /// It stopped an older or other-build watcher with SIGTERM and started one.
    Replaced { pid: u32 },
    /// An older watcher holds the lock and keeps running: it is not proven to
    /// be a `skiff watch` for this socket, or it did not stop. Why, for logs.
    Kept(String),
}


/// Starts `skiff watch` for the agent's session, unless one runs for this
/// daemon. `cli` is the `skiff` binary to run. The watcher is its own
/// process, out of the caller's process group and session: it outlives the
/// app or the terminal that started it, connects again after a daemon
/// reload, and stops when the session ends. Its lock beside the socket keeps
/// it to one copy. True when it started one.
pub fn start_watch(socket: &Path, cli: &Path, session: &str) -> Result<bool> {
    let lock = watch_lock(socket);
    if let Ok(f) = std::fs::File::options().create(true).truncate(false).write(true).open(&lock) {
        if f.try_lock().is_err() {
            return Ok(false);
        }
    }
    let log = fresh_log(&socket.with_extension("watch.log"))?;
    let mut cmd = outliving(cli, &unit_for("skiff-watch", socket));
    cmd.args(["watch", session])
        .env("SKIFF_SOCKET", socket)
        .stdin(std::process::Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    detach(&mut cmd);
    let mut child = cmd.spawn().with_context(|| format!("start {}", cli.display()))?;
    // Reap it when it ends while the caller runs.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(true)
}

/// The build id of the program file at `path` (`skiff`, `skiff-host`): 16
/// hex digits of a 64-bit FNV-1a hash over its bytes. Two builds of one
/// version differ by it, as a dev build and the next one do. Read again
/// only when the file's size or time changes. `None` when the file cannot
/// be read.
pub fn build_of(path: &Path) -> Option<String> {
    use std::io::Read;
    type Seen = (PathBuf, u64, Option<std::time::SystemTime>, String);
    static SEEN: std::sync::Mutex<Vec<Seen>> = std::sync::Mutex::new(Vec::new());
    let m = std::fs::metadata(path).ok()?;
    let (len, time) = (m.len(), m.modified().ok());
    if let Some((_, _, _, id)) = SEEN.lock().unwrap().iter().find(|(p, l, t, _)| p == path && *l == len && *t == time) {
        return Some(id.clone());
    }
    let mut f = std::fs::File::open(path).ok()?;
    let mut buf = vec![0u8; 1 << 20];
    let mut h = 0xcbf29ce484222325u64;
    loop {
        let n = f.read(&mut buf).ok()?;
        if n == 0 {
            break;
        }
        h = buf[..n].iter().fold(h, |h, &b| (h ^ b as u64).wrapping_mul(0x100000001b3));
    }
    let id = format!("{h:016x}");
    let mut seen = SEEN.lock().unwrap();
    seen.retain(|(p, ..)| p != path);
    seen.push((path.to_path_buf(), len, time, id.clone()));
    Some(id)
}

/// The lock file of the watcher for `socket`.
pub fn watch_lock(socket: &Path) -> PathBuf {
    socket.with_extension("watch.lock")
}

/// Writes this process into the lock file that `f` holds, for the app's
/// stale-watcher check and Restart. The lock stays on the file.
pub fn write_holder(f: &mut std::fs::File, socket: &Path) -> Result<()> {
    use std::io::{Seek, Write};
    let exe = std::env::current_exe().ok().map(|e| e.canonicalize().unwrap_or(e));
    let me = WatchHolder {
        pid: std::process::id(),
        socket: socket.display().to_string(),
        started_at: skiff_core::session::now_ms(),
        version: Some(CLI_VERSION.to_string()),
        build: exe.as_deref().and_then(build_of),
        exe: exe.map(|e| e.display().to_string()),
    };
    f.set_len(0)?;
    f.seek(std::io::SeekFrom::Start(0))?;
    f.write_all(serde_json::to_string(&me)?.as_bytes())?;
    f.flush()?;
    Ok(())
}

/// Whether a watcher holds the lock for `socket`, and which one. Reads only:
/// a free lock is let go at once.
pub fn watch_status(socket: &Path) -> Result<WatchLock> {
    let path = watch_lock(socket);
    let Ok(f) = std::fs::File::options().read(true).write(true).open(&path) else {
        return Ok(WatchLock { held: false, holder: None });
    };
    match f.try_lock() {
        Ok(()) => Ok(WatchLock { held: false, holder: None }),
        Err(std::fs::TryLockError::WouldBlock) => {
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            Ok(WatchLock { held: true, holder: serde_json::from_str(text.trim()).ok() })
        }
        Err(std::fs::TryLockError::Error(e)) => Err(e).with_context(|| format!("lock {}", path.display())),
    }
}

/// True for the command line of `skiff watch`: a program named `skiff`
/// with `watch` as its first argument. The program is the first word, or
/// the first word after `--` (systemd-run on Linux).
pub fn is_watch_command(cmd: &str) -> bool {
    let words: Vec<&str> = cmd.split_whitespace().collect();
    let at = words.iter().position(|w| *w == "--").map_or(0, |i| i + 1);
    words.get(at).is_some_and(|w| Path::new(w).file_name().is_some_and(|n| n == "skiff")) && words.get(at + 1) == Some(&"watch")
}

/// The command line of a live process. `None` when it does not run.
#[cfg(unix)]
fn command_of(pid: u32) -> Option<String> {
    // SAFETY: signal 0 only checks that the process exists.
    if pid == 0 || unsafe { libc::kill(pid as libc::pid_t, 0) } != 0 {
        return None;
    }
    let out = std::process::Command::new("ps").args(["-o", "command=", "-p", &pid.to_string()]).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string()).filter(|c| !c.is_empty())
}

/// Why the lock holder is not ours to stop. `None`: it is a live `skiff
/// watch` for `socket`.
#[cfg(unix)]
fn not_stoppable(h: &WatchHolder, socket: &Path) -> Option<String> {
    if Path::new(&h.socket) != socket {
        return Some(format!("The watch lock names another socket ({}). Nothing was stopped.", h.socket));
    }
    match command_of(h.pid) {
        None => Some(format!("Process {} holds no watcher. Nothing was stopped.", h.pid)),
        Some(c) if !is_watch_command(&c) => Some(format!("Process {} is not skiff watch. Nothing was stopped.", h.pid)),
        Some(_) => None,
    }
}

/// Stops the watcher that holds the lock for `socket`, then starts one for
/// `session` as [`start_watch`] does. It signals only a live `skiff watch`
/// that wrote itself into the lock for this socket, with SIGTERM, and waits
/// up to [`RESTART_WAIT`] for the lock. It never sends SIGKILL, and it never
/// ends, types into or changes a session. True when it started a watcher.
#[cfg(unix)]
pub fn restart_watch(socket: &Path, cli: &Path, session: &str) -> Result<bool> {
    let st = watch_status(socket)?;
    if st.held {
        let Some(h) = st.holder else {
            bail!("A watcher holds the lock, but its pid is unknown. Stop it from a terminal, then restart.");
        };
        if let Some(why) = not_stoppable(&h, socket) {
            bail!(why);
        }
        // SAFETY: a plain syscall to a checked pid.
        if unsafe { libc::kill(h.pid as libc::pid_t, libc::SIGTERM) } != 0 {
            bail!("Cannot signal the watcher (pid {}): {}", h.pid, std::io::Error::last_os_error());
        }
        let until = std::time::Instant::now() + RESTART_WAIT;
        while watch_status(socket)?.held {
            if std::time::Instant::now() >= until {
                bail!("The watcher (pid {}) did not stop within 5 seconds. Nothing else changed.", h.pid);
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }
    start_watch(socket, cli, session)
}

/// The watcher in `h` is not the build of `cli` at `version`: an older
/// version, a record from before the version, or the same version from
/// another build ([`build_of`]), a record from before the build included.
/// One computer runs one watcher per socket, and the app's build replaces
/// it. A newer version is never replaced. A `cli` that cannot be read
/// replaces nothing of its own version.
pub fn stale_holder(h: &WatchHolder, cli: &Path, version: &str) -> bool {
    let Some(v) = h.version.as_deref() else { return true };
    match skiff_core::compare_versions(v, version) {
        std::cmp::Ordering::Less => true,
        std::cmp::Ordering::Greater => false,
        std::cmp::Ordering::Equal => build_of(cli).is_some_and(|mine| h.build.as_deref() != Some(mine.as_str())),
    }
}

/// Makes one current watcher run for `session`, without the user. `cli` is
/// the bundled `skiff`, at `version`. A free lock gets a watcher. A stale
/// holder ([`stale_holder`]) is stopped and replaced when it is a live
/// `skiff watch` for `socket`: the pid its record names, or for a record
/// from before the pid, the one pid in `known`. `known` holds the watcher
/// pids skiffd registered for `socket`, from its peer credentials.
///
/// `tried` allows one attempt: it is set before the signal, and a set flag
/// stops any later signal, from this call or one that runs beside it. `None`
/// never signals. It sends SIGTERM only and waits up to [`RESTART_WAIT`]
/// for the lock; it never takes a held lock. A watcher it cannot prove, or
/// that does not stop, keeps running, and the outcome says why.
#[cfg(unix)]
pub fn ensure_watch(socket: &Path, cli: &Path, version: &str, session: &str, known: &[u32], tried: Option<&std::sync::atomic::AtomicBool>) -> Result<WatchOutcome> {
    let st = watch_status(socket)?;
    if !st.held {
        return Ok(if start_watch(socket, cli, session)? { WatchOutcome::Started } else { WatchOutcome::Current });
    }
    let pid = match &st.holder {
        Some(h) if !stale_holder(h, cli, version) => return Ok(WatchOutcome::Current),
        Some(h) => {
            if let Some(why) = not_stoppable(h, socket) {
                return Ok(WatchOutcome::Kept(why));
            }
            h.pid
        }
        None => {
            let mut live: Vec<u32> = known.iter().copied().filter(|p| command_of(*p).is_some_and(|c| is_watch_command(&c))).collect();
            live.sort();
            live.dedup();
            match live[..] {
                [pid] => pid,
                _ => return Ok(WatchOutcome::Kept("A watcher from an older Skiff holds the lock, and its process is not known. It keeps running.".into())),
            }
        }
    };
    // The attempt is taken before the signal, so a timeout keeps it taken.
    if tried.is_none_or(|t| t.swap(true, std::sync::atomic::Ordering::SeqCst)) {
        return Ok(WatchOutcome::Kept(format!("The watcher (pid {pid}) is older. A replacement was tried once this run, so it keeps running.")));
    }
    // SAFETY: a plain syscall to a checked pid.
    if unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) } != 0 {
        return Ok(WatchOutcome::Kept(format!("Cannot signal the watcher (pid {pid}): {}", std::io::Error::last_os_error())));
    }
    let until = std::time::Instant::now() + RESTART_WAIT;
    while watch_status(socket)?.held {
        if std::time::Instant::now() >= until {
            return Ok(WatchOutcome::Kept(format!("The watcher (pid {pid}) did not stop within 5 seconds. It keeps running.")));
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    start_watch(socket, cli, session)?;
    Ok(WatchOutcome::Replaced { pid })
}

/// A new, empty watch log. The last one moves to `<log>.1`, so the log of a
/// watcher that died is still there when the next one starts.
fn fresh_log(path: &Path) -> Result<std::fs::File> {
    if path.exists() {
        let mut old = path.as_os_str().to_owned();
        old.push(".1");
        std::fs::rename(path, &old).context("keep the last watch log")?;
    }
    std::fs::File::create(path).context("create the watch log")
}

/// A command for `program` that outlives the login that starts it, an SSH
/// login included. On Linux with a systemd user manager, it runs through
/// `systemd-run --user --scope --unit=<unit>`: the process leaves the
/// login's cgroup, so logind does not kill it at logout
/// (KillUserProcesses=yes). It gets DISPLAY, WAYLAND_DISPLAY,
/// DBUS_SESSION_BUS_ADDRESS and XDG_RUNTIME_DIR from the user manager, so
/// the sessions it starts see the desktop. A scope, not a service: a service
/// unit ends and kills the child when systemd-run returns. Else `program`
/// as is. Add the arguments to the result, then call [`detach`].
pub fn outliving(program: &Path, unit: &str) -> std::process::Command {
    #[cfg(target_os = "linux")]
    if let Some(env) = user_manager_env() {
        let mut cmd = std::process::Command::new("systemd-run");
        cmd.args(["--user", "--scope", "--quiet", "--collect"])
            .arg(format!("--unit={}", unit_name(unit)))
            .arg("--")
            .arg(program)
            .envs(env);
        return cmd;
    }
    let _ = unit;
    std::process::Command::new(program)
}

/// `unit-<socket name>-<8 hex digits of the socket path>`, in the
/// characters a unit name takes. The hash keeps two sockets with one file
/// name apart, such as a test's or a dev checkout's beside the live one:
/// a second `systemd-run` with a unit name in use fails to start.
pub fn unit_for(unit: &str, socket: &Path) -> String {
    let stem = socket.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let hash = socket.as_os_str().as_encoded_bytes().iter().fold(0xcbf29ce484222325u64, |h, &b| (h ^ b as u64).wrapping_mul(0x100000001b3));
    format!("{unit}-{stem}-{:08x}", hash as u32)
}

#[cfg(target_os = "linux")]
fn unit_name(unit: &str) -> String {
    unit.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' }).collect()
}

/// The desktop's variables from `systemctl --user show-environment`. `None`
/// when there is no user manager to ask.
#[cfg(target_os = "linux")]
fn user_manager_env() -> Option<Vec<(String, String)>> {
    const KEEP: [&str; 4] = ["DISPLAY", "WAYLAND_DISPLAY", "DBUS_SESSION_BUS_ADDRESS", "XDG_RUNTIME_DIR"];
    let mut cmd = std::process::Command::new("systemctl");
    cmd.args(["--user", "show-environment"]).stdin(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    // An SSH login can lack XDG_RUNTIME_DIR, and systemctl --user needs it.
    if std::env::var_os("XDG_RUNTIME_DIR").is_none() {
        let run = skiff_core::socket::runtime_dir();
        cmd.env("XDG_RUNTIME_DIR", run);
    }
    let out = cmd.output().ok().filter(|o| o.status.success())?;
    let text = String::from_utf8_lossy(&out.stdout);
    let env: Vec<(String, String)> = text
        .lines()
        .filter_map(|l| l.split_once('='))
        .filter(|(k, _)| KEEP.contains(k))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    // systemd-run reaches the manager through XDG_RUNTIME_DIR too.
    env.iter().any(|(k, _)| k == "XDG_RUNTIME_DIR").then_some(env)
}

/// Runs `cmd` in a session of its own, so a hangup of the terminal that
/// started it (an SSH logout) does not reach it. On Linux it also gets a
/// session keyring of its own, linked to the user keyring as pam_keyinit
/// does: the login's keyring is revoked at logout, and every program the
/// process starts would inherit the dead one.
pub fn detach(cmd: &mut std::process::Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: setsid and keyctl are plain syscalls and touch no memory.
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                #[cfg(target_os = "linux")]
                {
                    const JOIN_SESSION_KEYRING: libc::c_long = 1;
                    const LINK: libc::c_long = 8;
                    const USER_KEYRING: libc::c_long = -4;
                    const SESSION_KEYRING: libc::c_long = -3;
                    // A kernel without keys refuses both. That is no reason not to start.
                    if libc::syscall(libc::SYS_keyctl, JOIN_SESSION_KEYRING, 0 as libc::c_long) >= 0 {
                        libc::syscall(libc::SYS_keyctl, LINK, USER_KEYRING, SESSION_KEYRING);
                    }
                }
                Ok(())
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    /// The lock tests run one at a time: a process another test forks holds
    /// a copy of each open file until it execs, and with it a lock.
    static LOCKS: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn knows_the_watch_command() {
        assert!(is_watch_command("/Applications/Skiff.app/Contents/MacOS/skiff watch 4ac9e7a1"));
        assert!(is_watch_command("skiff watch abc"));
        assert!(is_watch_command("systemd-run --user --scope -- /usr/bin/skiff watch abc"));
        assert!(!is_watch_command("skiff ledger ls"));
        assert!(!is_watch_command("/usr/bin/skiffd"));
        assert!(!is_watch_command("vim skiff watch"));
        assert!(!is_watch_command(""));
    }

    /// A watcher that holds the lock reads as held, with what it wrote. A free
    /// lock reads as free, and the read does not keep it.
    #[test]
    fn reads_the_watch_lock_and_its_holder() {
        let _one = LOCKS.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("skiff-watch-lock-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("s.sock");
        assert_eq!(watch_status(&socket).unwrap(), WatchLock { held: false, holder: None });
        let mut f = std::fs::File::options().create(true).truncate(false).write(true).open(watch_lock(&socket)).unwrap();
        f.lock().unwrap();
        write_holder(&mut f, &socket).unwrap();
        let st = watch_status(&socket).unwrap();
        assert!(st.held);
        let h = st.holder.unwrap();
        assert_eq!((h.pid, h.socket.as_str()), (std::process::id(), socket.to_str().unwrap()));
        // A watcher from before the record: held, pid unknown.
        f.set_len(0).unwrap();
        assert_eq!(watch_status(&socket).unwrap(), WatchLock { held: true, holder: None });
        drop(f);
        assert!(!watch_status(&socket).unwrap().held);
        assert!(!watch_status(&socket).unwrap().held);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Restart signals nothing that is not a live `skiff watch` for this
    /// socket: here the holder is the test itself, and it keeps running.
    #[cfg(unix)]
    #[test]
    fn restart_stops_only_a_skiff_watch() {
        let _one = LOCKS.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("skiff-watch-restart-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("s.sock");
        let cli = dir.join("no-skiff");
        let mut f = std::fs::File::options().create(true).truncate(false).write(true).open(watch_lock(&socket)).unwrap();
        f.lock().unwrap();
        let err = restart_watch(&socket, &cli, "abc").unwrap_err().to_string();
        assert!(err.contains("pid is unknown"), "{err}");
        write_holder(&mut f, &socket).unwrap();
        let err = restart_watch(&socket, &cli, "abc").unwrap_err().to_string();
        assert!(err.contains("is not skiff watch"), "{err}");
        // Another socket's record is refused before any process check.
        let other = WatchHolder { pid: std::process::id(), socket: "/elsewhere.sock".into(), started_at: 0, version: None, exe: None, build: None };
        assert!(not_stoppable(&other, &socket).unwrap().contains("another socket"));
        assert!(watch_status(&socket).unwrap().held);
        drop(f);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A watcher that holds the lock and never registers is stopped with
    /// SIGTERM, and a new one starts. The stand-in shows as `skiff watch` in
    /// `ps`: python takes the lock, then execs `cat` named `skiff` on a FIFO
    /// named `watch`, which blocks. Skipped without python3 or mkfifo.
    #[cfg(unix)]
    #[test]
    fn restart_stops_a_stale_watcher_and_starts_one() {
        let _one = LOCKS.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("skiff-watch-stale-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("s.sock");
        if !std::process::Command::new("mkfifo").arg(dir.join("watch")).status().is_ok_and(|s| s.success()) {
            return;
        }
        let script = r#"
import fcntl, json, os, sys, time
lock, sock = sys.argv[1], sys.argv[2]
fd = os.open(lock, os.O_WRONLY | os.O_CREAT)
fcntl.flock(fd, fcntl.LOCK_EX)
os.ftruncate(fd, 0)
os.write(fd, json.dumps({"pid": os.getpid(), "socket": sock, "started_at": int(time.time() * 1000)}).encode())
os.set_inheritable(fd, True)
os.execv("/bin/cat", ["skiff", "watch"])
"#;
        let Ok(mut child) = std::process::Command::new("python3")
            .args(["-c", script])
            .arg(watch_lock(&socket))
            .arg(&socket)
            .current_dir(&dir)
            .spawn()
        else {
            return;
        };
        let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !watch_status(&socket).unwrap().held || !command_of(child.id()).is_some_and(|c| is_watch_command(&c)) {
            assert!(std::time::Instant::now() < until, "the stand-in watcher did not start");
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let st = watch_status(&socket).unwrap();
        assert_eq!(st.holder.map(|h| h.pid), Some(child.id()));
        // `true` stands in for the skiff that starts the new watcher.
        let started = restart_watch(&socket, Path::new("/usr/bin/true"), "abc").unwrap();
        assert!(started);
        let code = child.wait().unwrap();
        assert_eq!(std::os::unix::process::ExitStatusExt::signal(&code), Some(libc::SIGTERM));
        assert!(!watch_status(&socket).unwrap().held);
        assert!(socket.with_extension("watch.log").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Which holder is stale: no version (a watcher from before the field),
    /// an older one, or the same version from another build, or with no
    /// build. A newer version, or this build, is current, whatever
    /// executable path runs it. Old records still read.
    #[test]
    fn tells_a_stale_watcher_by_its_record() {
        let dir = std::env::temp_dir().join(format!("skiff-watch-build-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cli = dir.join("skiff");
        std::fs::write(&cli, "this build").unwrap();
        let mine = build_of(&cli).unwrap();
        let h = |version: Option<&str>, build: Option<&str>| WatchHolder {
            pid: 1,
            socket: "/s.sock".into(),
            started_at: 0,
            version: version.map(Into::into),
            exe: Some("/src/skiff/target/debug/skiff".into()),
            build: build.map(Into::into),
        };
        assert!(stale_holder(&h(None, None), &cli, "1.6.5"));
        assert!(stale_holder(&h(Some("1.6.4"), Some(&mine)), &cli, "1.6.5"));
        assert!(stale_holder(&h(Some("1.6.5"), Some("0123456789abcdef")), &cli, "1.6.5"));
        assert!(stale_holder(&h(Some("1.6.5"), None), &cli, "1.6.5"));
        assert!(!stale_holder(&h(Some("1.6.5"), Some(&mine)), &cli, "1.6.5"));
        assert!(!stale_holder(&h(Some("1.7.0"), Some("0123456789abcdef")), &cli, "1.6.5"));
        assert!(!stale_holder(&h(Some("1.6.5"), None), &dir.join("missing"), "1.6.5"));
        // A new build at the path is another build.
        std::fs::write(&cli, "the next build").unwrap();
        assert!(stale_holder(&h(Some("1.6.5"), Some(&mine)), &cli, "1.6.5"));
        let legacy: WatchHolder = serde_json::from_str(r#"{"pid":7,"socket":"/s.sock","started_at":3}"#).unwrap();
        assert_eq!((legacy.version, legacy.exe, legacy.build), (None, None, None));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A live `skiff watch` of this version from another build is stopped
    /// and replaced. One of this build is kept. Skipped without python3,
    /// which takes the lock.
    #[test]
    #[cfg(unix)]
    fn replaces_a_watcher_of_another_build_and_keeps_this_one() {
        let _one = LOCKS.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("skiff-watch-other-build-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let dir = dir.canonicalize().unwrap();
        let socket = dir.join("s.sock");
        // `skiff watch` to ps: a shell named skiff that reads the script `watch`.
        std::os::unix::fs::symlink("/bin/sh", dir.join("skiff")).unwrap();
        std::fs::write(dir.join("watch"), "while :; do sleep 0.1; done\n").unwrap();
        // `true` stands in for the app's skiff that starts the new watcher.
        let cli = Path::new("/usr/bin/true");
        let script = r#"
import fcntl, json, os, sys, time
lock, sock, exe, version, build = sys.argv[1:6]
fd = os.open(lock, os.O_WRONLY | os.O_CREAT)
fcntl.flock(fd, fcntl.LOCK_EX)
os.ftruncate(fd, 0)
os.write(fd, json.dumps({"pid": os.getpid(), "socket": sock, "started_at": int(time.time() * 1000), "version": version, "build": build}).encode())
os.set_inheritable(fd, True)
os.execv(exe, ["skiff", "watch"])
"#;
        let stand_in = |build: &str| {
            let child = std::process::Command::new("python3")
                .args(["-c", script])
                .arg(watch_lock(&socket))
                .arg(&socket)
                .arg(dir.join("skiff"))
                .args([CLI_VERSION, build])
                .current_dir(&dir)
                .spawn()
                .ok()?;
            let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while !watch_status(&socket).unwrap().held || !command_of(child.id()).is_some_and(|c| is_watch_command(&c)) {
                assert!(std::time::Instant::now() < until, "the stand-in watcher did not start");
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Some(child)
        };
        // This build: kept, and nothing is signalled.
        let Some(mut same) = stand_in(&build_of(cli).unwrap()) else { return };
        assert_eq!(ensure_watch(&socket, cli, CLI_VERSION, "abc", &[], Some(&AtomicBool::new(false))).unwrap(), WatchOutcome::Current);
        assert!(same.try_wait().unwrap().is_none());
        unsafe { libc::kill(same.id() as libc::pid_t, libc::SIGKILL) };
        let _ = same.wait();
        // Another build of the same version: stopped and replaced.
        let mut old = stand_in("0123456789abcdef").unwrap();
        let out = ensure_watch(&socket, cli, CLI_VERSION, "abc", &[], Some(&AtomicBool::new(false))).unwrap();
        assert_eq!(out, WatchOutcome::Replaced { pid: old.id() });
        let code = old.wait().unwrap();
        assert_eq!(std::os::unix::process::ExitStatusExt::signal(&code), Some(libc::SIGTERM));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A held lock with no record, and no registered `skiff watch` to name
    /// its process, stays as it is: nothing is signalled, nothing starts. The
    /// same with a known pid that is not a watcher, and with an older record
    /// whose pid is not `skiff watch`.
    #[test]
    #[cfg(unix)]
    fn keeps_a_watcher_it_cannot_prove() {
        let _one = LOCKS.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("skiff-watch-ensure-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("s.sock");
        let mut f = std::fs::File::options().create(true).truncate(false).write(true).open(watch_lock(&socket)).unwrap();
        f.lock().unwrap();
        let cli = Path::new("/usr/bin/true");
        for known in [&[][..], &[std::process::id()][..]] {
            let out = ensure_watch(&socket, cli, CLI_VERSION, "abc", known, Some(&AtomicBool::new(false))).unwrap();
            assert!(matches!(out, WatchOutcome::Kept(_)), "{out:?}");
        }
        // This process, as a holder from an older CLI: not a watcher, so it stays.
        write_holder(&mut f, &socket).unwrap();
        let old = WatchHolder { version: Some("0.0.1".into()), ..watch_status(&socket).unwrap().holder.unwrap() };
        f.set_len(0).unwrap();
        std::io::Seek::seek(&mut f, std::io::SeekFrom::Start(0)).unwrap();
        std::io::Write::write_all(&mut f, serde_json::to_string(&old).unwrap().as_bytes()).unwrap();
        let out = ensure_watch(&socket, cli, CLI_VERSION, "abc", &[], Some(&AtomicBool::new(false))).unwrap();
        assert!(matches!(&out, WatchOutcome::Kept(w) if w.contains("is not skiff watch")), "{out:?}");
        // This CLI's own record: current, and nothing changes.
        write_holder(&mut f, &socket).unwrap();
        let me = std::env::current_exe().unwrap();
        assert_eq!(ensure_watch(&socket, &me, CLI_VERSION, "abc", &[], Some(&AtomicBool::new(false))).unwrap(), WatchOutcome::Current);
        assert!(watch_status(&socket).unwrap().held);
        assert!(!socket.with_extension("watch.log").exists());
        drop(f);
        // A free lock gets a watcher.
        assert_eq!(ensure_watch(&socket, cli, CLI_VERSION, "abc", &[], Some(&AtomicBool::new(false))).unwrap(), WatchOutcome::Started);
        std::fs::remove_dir_all(&dir).unwrap();
    }


    /// Each socket gets a scope of its own, so a test daemon named
    /// skiffd.sock beside the live one does not fail to start its watcher.
    #[test]
    fn each_socket_gets_its_own_scope_unit() {
        let a = unit_for("skiff-watch", Path::new("/tmp/a/skiffd.sock"));
        let b = unit_for("skiff-watch", Path::new("/tmp/b/skiffd.sock"));
        assert!(a.starts_with("skiff-watch-skiffd-") && a.len() == "skiff-watch-skiffd-".len() + 8, "{a}");
        assert_ne!(a, b);
        assert_eq!(a, unit_for("skiff-watch", Path::new("/tmp/a/skiffd.sock")));
    }

    /// One attempt per flag. A stale watcher that ignores SIGTERM holds the
    /// lock past the wait: the outcome is Kept, and it keeps running. Two
    /// calls at once, as from a start and a restore, and every call after
    /// the timeout, share the flag: the watcher gets one SIGTERM in all, and
    /// no second watcher starts. Skipped without python3, which takes the lock.
    #[test]
    #[cfg(unix)]
    fn a_watcher_that_does_not_stop_gets_one_signal() {
        let _one = LOCKS.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("skiff-watch-once-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let dir = dir.canonicalize().unwrap();
        let socket = dir.join("s.sock");
        // `skiff watch` to ps: a shell named skiff that reads the script `watch`.
        std::os::unix::fs::symlink("/bin/sh", dir.join("skiff")).unwrap();
        std::fs::write(dir.join("watch"), "trap 'echo term >> count' TERM\nwhile :; do sleep 0.1; done\n").unwrap();
        let script = r#"
import fcntl, json, os, sys, time
lock, sock, exe = sys.argv[1], sys.argv[2], sys.argv[3]
fd = os.open(lock, os.O_WRONLY | os.O_CREAT)
fcntl.flock(fd, fcntl.LOCK_EX)
os.ftruncate(fd, 0)
os.write(fd, json.dumps({"pid": os.getpid(), "socket": sock, "started_at": int(time.time() * 1000)}).encode())
os.set_inheritable(fd, True)
os.execv(exe, ["skiff", "watch"])
"#;
        let Ok(mut child) = std::process::Command::new("python3")
            .args(["-c", script])
            .arg(watch_lock(&socket))
            .arg(&socket)
            .arg(dir.join("skiff"))
            .current_dir(&dir)
            .spawn()
        else {
            return;
        };
        let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !watch_status(&socket).unwrap().held || !command_of(child.id()).is_some_and(|c| is_watch_command(&c)) {
            assert!(std::time::Instant::now() < until, "the stand-in watcher did not start");
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let tried = AtomicBool::new(false);
        let cli = Path::new("/usr/bin/true");
        let both = std::thread::scope(|s| {
            let calls: Vec<_> = (0..2).map(|_| s.spawn(|| ensure_watch(&socket, cli, CLI_VERSION, "abc", &[], Some(&tried)).unwrap())).collect();
            calls.into_iter().map(|c| c.join().unwrap()).collect::<Vec<_>>()
        });
        let later = ensure_watch(&socket, cli, CLI_VERSION, "abc", &[], Some(&tried)).unwrap();
        // A trap runs between sleeps: give a late one time to show.
        std::thread::sleep(std::time::Duration::from_millis(300));
        let signals = std::fs::read_to_string(dir.join("count")).unwrap_or_default().lines().count();
        let running = child.try_wait().unwrap().is_none();
        let held = watch_status(&socket).unwrap().held;
        unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGKILL) };
        let _ = child.wait();
        let kept = |o: &WatchOutcome, what: &str| matches!(o, WatchOutcome::Kept(w) if w.contains(what));
        assert_eq!(both.iter().filter(|o| kept(o, "did not stop")).count(), 1, "{both:?}");
        assert_eq!(both.iter().filter(|o| kept(o, "tried once")).count(), 1, "{both:?}");
        assert!(kept(&later, "tried once"), "{later:?}");
        assert_eq!(signals, 1);
        assert!(running && held, "the watcher keeps running and holds the lock");
        assert!(!socket.with_extension("watch.log").exists(), "a second watcher started");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_new_watch_log_keeps_the_last_one() {
        let dir = std::env::temp_dir().join(format!("skiff-watch-log-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("s.watch.log");
        std::io::Write::write_all(&mut fresh_log(&log).unwrap(), b"first").unwrap();
        fresh_log(&log).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("s.watch.log.1")).unwrap(), "first");
        assert_eq!(std::fs::read_to_string(&log).unwrap(), "");
        std::fs::remove_dir_all(&dir).unwrap();
    }


}
