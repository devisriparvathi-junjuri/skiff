//! M2: reach a remote Host's skiffd through the system `ssh`. OpenSSH forwards
//! a Unix socket in a private `0700` directory on this machine to the remote
//! skiffd socket (`-L local.sock:remote.sock`). Each forward makes its own
//! directory, so two windows or app runs never share or remove a socket. `~/.ssh/config`, ProxyJump,
//! agents, hardware keys and `known_hosts` work as in a terminal.
//!
//! Trust is the user's own: `StrictHostKeyChecking=yes`, so an unknown or
//! changed host key fails and nothing is learned or written (`UpdateHostKeys=no`).
//! `BatchMode=yes`: there is no prompt. The link adds no TOFU, no auth of its
//! own, no TCP listener: the person trusts a key in Settings › Hosts, after a
//! confirm, through `keys`, never here. The `ssh` process is this app's transport, not the Host's:
//! stopping it ends only the forward, and the remote sessions run on.
//! Identity comes from skiffd's `pong`, never from the forwarded socket's peer
//! credentials, which name `ssh`.

use std::{
    ffi::OsString,
    fmt,
    os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, Command},
};

/// How long `ssh` may take to log in and listen.
pub const OPEN_TIMEOUT: Duration = Duration::from_secs(20);
/// ssh's own connect timeout, in seconds, on every run.
pub const CONNECT_TIMEOUT: u64 = 15;
/// The most stderr kept for diagnosis.
const STDERR_KEEP: usize = 16 * 1024;

/// One remote Host as `ssh` reaches it. `destination` is what the user types
/// after `ssh`: an alias from ssh config, `host`, or `user@host`. Keys and
/// jumps come from ssh config, and so do user and port unless set here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshTarget {
    pub destination: String,
    /// Sent as `-l` only when the person set it.
    pub user: Option<String>,
    /// Sent as `-p` only when the person set it.
    pub port: Option<u16>,
    /// The remote skiffd socket, as `skiff status` prints it there.
    pub socket: String,
}

impl SshTarget {
    /// A target with the user and port from ssh config.
    pub fn new(destination: impl Into<String>, socket: impl Into<String>) -> Self {
        Self {
            destination: destination.into(),
            user: None,
            port: None,
            socket: socket.into(),
        }
    }

    /// `-l user -p port`, only for what the person set. Before `--`.
    pub fn login_args(&self) -> Vec<OsString> {
        let mut args: Vec<OsString> = Vec::new();
        if let Some(u) = &self.user {
            args.push("-l".into());
            args.push(u.into());
        }
        if let Some(p) = self.port {
            args.push("-p".into());
            args.push(p.to_string().into());
        }
        args
    }

    /// Refuses a destination, user or port that `ssh` could misread. The
    /// socket is not checked: [`Self::validate`] does that for a forward.
    pub fn validate_login(&self) -> Result<(), String> {
        let word = |s: &str| {
            !s.is_empty() && !s.starts_with('-') && !s.chars().any(|c| c.is_whitespace() || c.is_control())
        };
        if !word(&self.destination) {
            return Err("Type one name, such as devbox or 10.0.0.5.".into());
        }
        if self.user.as_deref().is_some_and(|u| !word(u) || u.contains('@')) {
            return Err("Type one user name, such as jose.".into());
        }
        if self.port == Some(0) {
            return Err("Type a port from 1 to 65535.".into());
        }
        Ok(())
    }

    /// Refuses what `ssh` could read as an option, and paths `-L` cannot carry.
    pub fn validate(&self) -> Result<(), String> {
        self.validate_login()?;
        let s = &self.socket;
        if !s.starts_with('/') || s.contains(':') || s.chars().any(|c| c.is_control()) {
            return Err("The remote socket must be an absolute path with no colon.".into());
        }
        Ok(())
    }
}

/// The `ssh` program, and for test fixtures only, a private ssh config (`-F`).
#[derive(Debug, Clone)]
pub struct SshOptions {
    pub program: PathBuf,
    pub config: Option<PathBuf>,
}

impl Default for SshOptions {
    fn default() -> Self {
        // An app started from the Dock has a short PATH.
        let system = Path::new("/usr/bin/ssh");
        let program = if system.is_file() {
            system.into()
        } else {
            PathBuf::from("ssh")
        };
        Self {
            program,
            config: None,
        }
    }
}

/// The arguments for one forward. Each `-o` here wins over ssh config.
pub fn forward_args(target: &SshTarget, local: &Path, options: &SshOptions) -> Vec<OsString> {
    let mut args: Vec<OsString> = Vec::new();
    if let Some(config) = &options.config {
        args.push("-F".into());
        args.push(config.into());
    }
    for (flag, value) in [
        ("-o", "BatchMode=yes"),
        ("-o", "StrictHostKeyChecking=yes"),
        ("-o", "UpdateHostKeys=no"),
        ("-o", "ExitOnForwardFailure=yes"),
        ("-o", "ServerAliveInterval=15"),
        ("-o", "ServerAliveCountMax=3"),
        ("-o", "ConnectTimeout=15"), // CONNECT_TIMEOUT
        ("-o", "StreamLocalBindMask=0177"),
        ("-o", "StreamLocalBindUnlink=yes"),
        // Our own connection: not a shared master, and no extra channels.
        ("-o", "ControlMaster=no"),
        ("-o", "ControlPath=none"),
        ("-o", "ForwardAgent=no"),
        ("-o", "ForwardX11=no"),
        ("-o", "PermitLocalCommand=no"),
        ("-o", "RequestTTY=no"),
        // A failed channel is logged at INFO, which DEBUG1 keeps, for the
        // cause. DEBUG1 adds the lines a connect shows (`shown_line`).
        ("-o", "LogLevel=DEBUG1"),
    ] {
        args.push(flag.into());
        args.push(value.into());
    }
    args.extend(target.login_args());
    args.push("-N".into());
    let mut spec = OsString::from(local);
    spec.push(":");
    spec.push(&target.socket);
    args.push("-L".into());
    args.push(spec);
    args.push("--".into());
    args.push(target.destination.clone().into());
    args
}

/// One strict, batch ssh run of a fixed remote `command`: no prompt, no
/// shared master, no extra channels. Each `-o` wins over ssh config.
pub fn batch_args(target: &SshTarget, options: &SshOptions, command: String) -> Vec<OsString> {
    let mut args: Vec<OsString> = Vec::new();
    if let Some(config) = &options.config {
        args.push("-F".into());
        args.push(config.into());
    }
    for value in [
        "BatchMode=yes",
        "StrictHostKeyChecking=yes",
        "UpdateHostKeys=no",
        &format!("ConnectTimeout={CONNECT_TIMEOUT}"),
        "ControlMaster=no",
        "ControlPath=none",
        "ForwardAgent=no",
        "ForwardX11=no",
        "PermitLocalCommand=no",
        "RequestTTY=no",
        "LogLevel=INFO",
    ] {
        args.push("-o".into());
        args.push(value.into());
    }
    args.push("-T".into());
    args.extend(target.login_args());
    args.push("--".into());
    args.push(target.destination.clone().into());
    args.push(command.into());
    args
}

/// One shell word in single quotes, for sh, bash, zsh and fish alike.
pub fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// What one batch run gave.
pub enum BatchRun {
    /// The command ran: its stdout.
    Done(String),
    /// ssh stopped before the command ran (exit 255): its stderr, for the cause.
    Ssh(String),
    TimedOut,
}

/// Runs `command` over one strict, batch ssh run, with `stdin`, within
/// `timeout`. Err: ssh itself could not be run.
pub async fn run_batch(target: &SshTarget, options: &SshOptions, command: String, stdin: &[u8], timeout: Duration) -> Result<BatchRun, String> {
    run_batch_steps(target, options, command, stdin, timeout, None).await
}

/// What a server step prints on stderr, one line per step, for a person
/// who waits: `skiff-step: <words>`.
const STEP: &str = "skiff-step: ";

/// What a batch run shows while it runs.
pub enum BatchEvent<'a> {
    /// A line the server printed as `skiff-step: <words>`.
    Step(&'a str),
    /// One of ssh's own lines that [`shown_line`] lets through.
    Ssh(String),
    /// Bytes of stdin sent so far, and the whole.
    Sent(u64, u64),
}

/// How much stdin goes per write, so the bytes sent can be shown.
const CHUNK: usize = 64 * 1024;

/// [`run_batch`]. With `show`, ssh logs at DEBUG1, and the server's step
/// lines, ssh's allowlisted lines and the bytes sent go to `show` as they
/// happen. The rest of stderr stays for the cause, as without it.
pub async fn run_batch_steps(
    target: &SshTarget,
    options: &SshOptions,
    command: String,
    stdin: &[u8],
    timeout: Duration,
    show: Option<&(dyn Fn(BatchEvent) + Send + Sync)>,
) -> Result<BatchRun, String> {
    use tokio::io::AsyncReadExt;
    let mut args = batch_args(target, options, command);
    if show.is_some() {
        for a in args.iter_mut().filter(|a| *a == "LogLevel=INFO") {
            *a = "LogLevel=DEBUG1".into();
        }
    }
    let mut child = Command::new(&options.program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("Skiff cannot run ssh: {e}"))?;
    let mut input = child.stdin.take().expect("piped");
    let (mut stdout, stderr) = (child.stdout.take().expect("piped"), child.stderr.take().expect("piped"));
    let emit = |e: BatchEvent| {
        if let Some(f) = show {
            f(e)
        }
    };
    let run = async {
        // A failed write shows as ssh's own failure below.
        let write = async {
            let total = stdin.len() as u64;
            let mut last = 0u64;
            for chunk in stdin.chunks(CHUNK) {
                if input.write_all(chunk).await.is_err() {
                    break;
                }
                last += chunk.len() as u64;
                if total > 4 * CHUNK as u64 && (last.is_multiple_of(4 * CHUNK as u64) || last == total) {
                    emit(BatchEvent::Sent(last, total));
                }
            }
            drop(input);
        };
        let mut out = Vec::new();
        let read_out = stdout.read_to_end(&mut out);
        let read_err = async {
            let (mut lines, mut rest) = (BufReader::new(stderr).lines(), String::new());
            while let Ok(Some(line)) = lines.next_line().await {
                match line.strip_prefix(STEP) {
                    Some(words) => emit(BatchEvent::Step(words.trim())),
                    None => {
                        if let Some(shown) = shown_line(&line) {
                            emit(BatchEvent::Ssh(shown));
                        }
                        if !line.starts_with("debug") {
                            rest.push_str(&line);
                            rest.push('\n');
                        }
                    }
                }
            }
            rest
        };
        let (_, _, err) = tokio::join!(write, read_out, read_err);
        (child.wait().await, out, err)
    };
    let (status, out, err) = match tokio::time::timeout(timeout, run).await {
        Ok(done) => done,
        Err(_) => return Ok(BatchRun::TimedOut),
    };
    let status = status.map_err(|e| format!("Skiff cannot run ssh: {e}"))?;
    if status.code() == Some(255) {
        return Ok(BatchRun::Ssh(err));
    }
    Ok(BatchRun::Done(String::from_utf8_lossy(&out).into_owned()))
}

/// One of ssh's own stderr lines as a person may see it, or `None`. Only a
/// short allowlist passes: connecting, the connection, a known host key,
/// authenticated, and forwarding, without the `debug1: ` prefix, and ssh's
/// connect failures. Never a key path, a key, an agent or a password line.
pub fn shown_line(line: &str) -> Option<String> {
    let l = line.trim();
    let debug = l.strip_prefix("debug1: ");
    let keep = match debug {
        Some(d) => {
            (d.starts_with("Connecting to ") && d.contains(" port "))
                || d.starts_with("Connection established")
                || (d.starts_with("Host '") && d.contains("is known and matches"))
                || d.starts_with("Authenticated to ")
                || d.starts_with("Local connections to ")
                || d.starts_with("Local forwarding listening on ")
        }
        None => {
            // VERBOSE lines, which DEBUG1 also prints, come without a prefix.
            l.starts_with("Authenticated to ")
                || l.starts_with("ssh: connect to host ")
                || l.starts_with("ssh: Could not resolve hostname ")
                || l.starts_with("Host key verification failed")
                || (l.contains("Permission denied (") && !l.contains('/'))
                || l.starts_with("Connection closed by ")
                || l.starts_with("Connection timed out")
        }
    };
    let text = debug.unwrap_or(l);
    // A key or identity file never shows, whatever the line.
    (keep && !text.contains("/.ssh/") && !text.contains("identity")).then(|| text.to_string())
}

/// Lines a connect shows as it goes: ssh's allowlisted lines from each
/// forward, and the replies the app notes. One stream for this app run.
static CONNECT_LOG: std::sync::OnceLock<tokio::sync::broadcast::Sender<String>> = std::sync::OnceLock::new();

fn connect_log_tx() -> &'static tokio::sync::broadcast::Sender<String> {
    CONNECT_LOG.get_or_init(|| tokio::sync::broadcast::channel(64).0)
}

/// Every connect line from now on.
pub fn connect_log() -> tokio::sync::broadcast::Receiver<String> {
    connect_log_tx().subscribe()
}

/// Adds one line to the connect log, such as a skiffd reply.
pub fn connect_note(line: impl Into<String>) {
    let _ = connect_log_tx().send(line.into());
}

/// The longest wait between two tries to open a forward.
const RETRY_MAX: Duration = Duration::from_secs(30);

/// How long to wait before the next try to open a forward, after
/// `failures` in a row: 1, 2, 4 … up to [`RETRY_MAX`].
pub fn backoff(failures: u32) -> Duration {
    Duration::from_secs(1 << failures.saturating_sub(1).min(5)).min(RETRY_MAX)
}

/// A new private folder of this app run in the temp dir, for one step.
/// The caller removes it.
pub fn temp_dir(what: &str) -> std::io::Result<PathBuf> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!("skiff-{what}-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::DirBuilder::new().mode(0o700).create(&dir)?;
    Ok(dir)
}

/// Why a forward could not reach the remote skiffd. The text follows the
/// design rules; it never repeats ssh's own output, which can name key paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SshError {
    HostKeyUnknown(String),
    HostKeyChanged(String),
    AuthFailed(String),
    /// The Host's sshd does not allow socket forwarding.
    ForwardingRefused(String),
    /// The Host refused the channel to the socket, and as far as Skiff knows
    /// no skiffd listens there. OpenSSH reports a missing socket and
    /// disabled forwarding with the same line: a probe that sees a live
    /// socket makes this [`Self::ForwardingRefused`] (see [`Self::refine`]).
    NoSkiffd {
        destination: String,
        socket: String,
    },
    Unreachable(String),
    Exited {
        destination: String,
        code: Option<i32>,
    },
    TimedOut(String),
    Waiting {
        destination: String,
        seconds: u64,
    },
    Setup(String),
}

impl fmt::Display for SshError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::HostKeyUnknown(d) => write!(f, "This computer does not know the host key of {d} yet. Skiff does not connect. Settings › Hosts checks {d}."),
            Self::HostKeyChanged(d) => write!(f, "The host key of {d} changed since the last connection. Skiff does not connect."),
            Self::AuthFailed(d) => write!(f, "{d} does not accept a key from this computer without a prompt. Settings › Hosts checks {d}."),
            Self::ForwardingRefused(d) => write!(f, "{d} does not allow Skiff's connection to its sessions service. Its admin must allow socket forwarding for ssh (AllowStreamLocalForwarding)."),
            Self::NoSkiffd { destination, .. } => write!(f, "Skiff's sessions service is not running on {destination}."),
            Self::Unreachable(d) => write!(f, "Cannot reach {d}. Check that this computer is online and that the server is on."),
            Self::Exited { destination, .. } => write!(f, "ssh to {destination} stopped for an unknown reason. Settings › Hosts checks {destination} and finds the cause."),
            Self::TimedOut(d) => write!(f, "ssh to {d} did not connect within {}s.", OPEN_TIMEOUT.as_secs()),
            Self::Waiting { destination, seconds } => write!(f, "Not connected to {destination}. Skiff tries again in {seconds}s."),
            Self::Setup(why) => write!(f, "Cannot prepare the SSH forward: {why}"),
        }
    }
}
impl std::error::Error for SshError {}

impl SshError {
    /// A refused channel, with what a probe saw: a live socket there means
    /// the sshd blocks the forward, not that skiffd is missing.
    pub fn refine(self, socket_live: bool) -> Self {
        match self {
            Self::NoSkiffd { destination, .. } if socket_live => Self::ForwardingRefused(destination),
            e => e,
        }
    }
}

/// What ssh's stderr says, as one of the known causes. `None`: no known cause.
pub fn classify(stderr: &str, target: &SshTarget) -> Option<SshError> {
    let d = target.destination.clone();
    if stderr.contains("REMOTE HOST IDENTIFICATION HAS CHANGED")
        || (stderr.contains("Host key for") && stderr.contains("has changed"))
    {
        Some(SshError::HostKeyChanged(d))
    } else if stderr.contains("Host key verification failed")
        || stderr.contains("No ED25519 host key is known")
        || stderr.contains("host key is known")
    {
        Some(SshError::HostKeyUnknown(d))
    } else if stderr.contains("Permission denied")
        || stderr.contains("Too many authentication failures")
    {
        Some(SshError::AuthFailed(d))
    } else if stderr.contains("administratively prohibited") {
        Some(SshError::ForwardingRefused(d))
    } else if stderr.contains("open failed: connect failed") {
        Some(SshError::NoSkiffd {
            destination: d,
            socket: target.socket.clone(),
        })
    } else if [
        "Could not resolve hostname",
        "Connection refused",
        "Connection timed out",
        "No route to host",
        "Network is unreachable",
        "Operation timed out",
    ]
    .iter()
    .any(|m| stderr.contains(m))
    {
        Some(SshError::Unreachable(d))
    } else {
        None
    }
}

/// A running forward. Dropping it stops this app's `ssh` and removes its own
/// directory; it never signals anything on the Host.
pub struct SshForward {
    target: SshTarget,
    child: Child,
    dir: PathBuf,
    socket: PathBuf,
    stderr: Arc<Mutex<String>>,
}

impl SshForward {
    /// Starts `ssh` and waits until it listens on the local socket, which
    /// lives in a new directory of its own inside the private `parent`.
    pub async fn open(
        target: &SshTarget,
        options: &SshOptions,
        parent: &Path,
    ) -> Result<Self, SshError> {
        target.validate().map_err(SshError::Setup)?;
        private_dir(parent).map_err(|e| SshError::Setup(format!("{}: {e}", parent.display())))?;
        let dir =
            own_dir(parent).map_err(|e| SshError::Setup(format!("{}: {e}", parent.display())))?;
        let socket = dir.join("skiffd.sock");
        connect_note(format!("ssh -v {}", target.destination));
        let child = Command::new(&options.program)
            .args(forward_args(target, &socket, options))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn();
        let mut child = match child {
            Ok(child) => child,
            Err(e) => {
                let _ = std::fs::remove_dir(&dir);
                return Err(SshError::Setup(format!(
                    "run {}: {e}",
                    options.program.display()
                )));
            }
        };
        let stderr = Arc::new(Mutex::new(String::new()));
        if let Some(err) = child.stderr.take() {
            let keep = stderr.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(err).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    if let Some(shown) = shown_line(&line) {
                        connect_note(shown);
                    }
                    let mut kept = keep.lock().unwrap();
                    kept.push_str(&line);
                    kept.push('\n');
                    if kept.len() > STDERR_KEEP {
                        let cut = kept.len() - STDERR_KEEP;
                        let cut = (cut..kept.len())
                            .find(|&i| kept.is_char_boundary(i))
                            .unwrap_or(0);
                        kept.drain(..cut);
                    }
                }
            });
        }
        let mut forward = Self {
            target: target.clone(),
            child,
            dir,
            socket,
            stderr,
        };
        let until = Instant::now() + OPEN_TIMEOUT;
        loop {
            if let Ok(Some(status)) = forward.child.try_wait() {
                // Let the stderr reader catch the last lines.
                tokio::time::sleep(Duration::from_millis(50)).await;
                return Err(forward.diagnose().unwrap_or(SshError::Exited {
                    destination: target.destination.clone(),
                    code: status.code(),
                }));
            }
            if forward.socket.exists() {
                return Ok(forward);
            }
            if Instant::now() >= until {
                return Err(SshError::TimedOut(target.destination.clone()));
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    /// The local end; a connection here is a channel to the remote socket.
    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// `ssh` still runs.
    pub fn is_alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// The known cause in ssh's output so far, if any.
    pub fn diagnose(&self) -> Option<SshError> {
        classify(&self.stderr.lock().unwrap(), &self.target)
    }
}

impl Drop for SshForward {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
        let _ = std::fs::remove_file(&self.socket);
        let _ = std::fs::remove_dir(&self.dir);
    }
}

/// A new directory inside `parent` that no other forward, window or app run
/// has: created exclusively, never reused. The name stays short for `-L`.
fn own_dir(parent: &Path) -> std::io::Result<PathBuf> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    loop {
        let dir = parent.join(format!(
            "{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        match std::fs::DirBuilder::new().mode(0o700).create(&dir) {
            Ok(()) => return Ok(dir),
            // Left by an earlier run with this pid: never adopt it.
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
}

/// A directory only this account can enter, made if missing. An existing one
/// that is not ours or not private is refused, never repaired.
fn private_dir(dir: &Path) -> std::io::Result<()> {
    match std::fs::DirBuilder::new().mode(0o700).create(dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e),
    }
    let meta = std::fs::symlink_metadata(dir)?;
    // SAFETY: geteuid takes no arguments and cannot fail.
    if !meta.is_dir()
        || meta.uid() != unsafe { libc::geteuid() }
        || meta.permissions().mode() & 0o077 != 0
    {
        return Err(std::io::Error::other(
            "not a private directory of this account",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target() -> SshTarget {
        SshTarget::new("devbox", "/run/user/501/skiff/skiffd.sock")
    }

    #[test]
    fn the_forward_keeps_strict_trust_and_never_takes_the_destination_as_an_option() {
        let args = forward_args(
            &target(),
            Path::new("/tmp/skiff-x/skiffd.sock"),
            &SshOptions {
                program: "ssh".into(),
                config: None,
            },
        );
        let args: Vec<String> = args.into_iter().map(|a| a.into_string().unwrap()).collect();
        for want in [
            "BatchMode=yes",
            "StrictHostKeyChecking=yes",
            "UpdateHostKeys=no",
            "ExitOnForwardFailure=yes",
            "ControlPath=none",
            "ForwardAgent=no",
        ] {
            let at = args
                .iter()
                .position(|a| a == want)
                .unwrap_or_else(|| panic!("no {want}: {args:?}"));
            assert_eq!(args[at - 1], "-o");
        }
        // Nothing turns host key checking off or accepts a new key.
        assert!(!args.iter().any(|a| a.contains("StrictHostKeyChecking=no")
            || a.contains("accept-new")
            || a.contains("UserKnownHostsFile")));
        assert!(!args.contains(&"-F".to_string()));
        let n = args.len();
        assert_eq!(
            &args[n - 5..],
            [
                "-N",
                "-L",
                "/tmp/skiff-x/skiffd.sock:/run/user/501/skiff/skiffd.sock",
                "--",
                "devbox"
            ]
        );
        let with = forward_args(
            &target(),
            Path::new("/x"),
            &SshOptions {
                program: "ssh".into(),
                config: Some("/fixture/ssh_config".into()),
            },
        );
        assert_eq!(
            with[..2],
            [OsString::from("-F"), OsString::from("/fixture/ssh_config")]
        );
        // User and port go before `--`, and only when set.
        let set = SshTarget {
            user: Some("jose".into()),
            port: Some(2222),
            ..target()
        };
        let args: Vec<String> = forward_args(&set, Path::new("/x"), &SshOptions::default())
            .into_iter()
            .map(|a| a.into_string().unwrap())
            .collect();
        let n = args.len();
        assert_eq!(&args[n - 9..n - 5], ["-l", "jose", "-p", "2222"]);
        assert!(SshTarget { user: Some("-oX".into()), ..target() }.validate().is_err());
        assert!(SshTarget { port: Some(0), ..target() }.validate().is_err());
    }

    #[test]
    fn only_allowlisted_ssh_lines_show_and_never_a_key_path() {
        let shown = |l: &str| shown_line(l);
        assert_eq!(shown("debug1: Connecting to p0p [192.168.1.40] port 22.").as_deref(), Some("Connecting to p0p [192.168.1.40] port 22."));
        assert_eq!(shown("debug1: Connection established.").as_deref(), Some("Connection established."));
        assert!(shown("debug1: Host 'p0p' is known and matches the ED25519 host key.").is_some());
        assert!(shown("debug1: Authenticated to p0p ([192.168.1.40]:22) using \"publickey\".").is_some());
        assert!(shown("ssh: connect to host p0p port 22: Connection timed out").is_some());
        for hidden in [
            "debug1: Will attempt key: /home/me/.ssh/id_ed25519 ED25519 SHA256:abc",
            "debug1: Offering public key: /home/me/.ssh/id_ed25519",
            "debug1: identity file /home/me/.ssh/id_rsa type -1",
            "debug1: Reading configuration data /home/me/.ssh/config",
            "debug1: client_input_global_request: rtype keepalive",
            "Load key \"/home/me/.ssh/id\": bad permissions",
            "debug1: Server host key: ssh-ed25519 SHA256:abc",
        ] {
            assert_eq!(shown(hidden), None, "{hidden}");
        }
    }

    #[test]
    fn targets_that_ssh_could_misread_are_refused() {
        assert!(target().validate().is_ok());
        assert!(SshTarget {
            destination: "me@devbox".into(),
            ..target()
        }
        .validate()
        .is_ok());
        for d in ["", "-oProxyCommand=x", "dev box", "dev\nbox"] {
            assert!(
                SshTarget {
                    destination: d.into(),
                    ..target()
                }
                .validate()
                .is_err(),
                "{d:?}"
            );
        }
        for s in ["relative.sock", "/a:b.sock", "/a\n.sock", ""] {
            assert!(
                SshTarget {
                    socket: s.into(),
                    ..target()
                }
                .validate()
                .is_err(),
                "{s:?}"
            );
        }
        // Spaces are fine: a macOS socket lives under Application Support.
        assert!(SshTarget {
            socket: "/Users/me/Library/Application Support/skiff/skiffd.sock".into(),
            ..target()
        }
        .validate()
        .is_ok());
    }

    #[test]
    fn ssh_output_maps_to_one_cause_without_repeating_it() {
        let t = target();
        let cases = [
            ("@@@@@\n@    WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED!     @\nHost key verification failed.\n", SshError::HostKeyChanged("devbox".into())),
            ("No ED25519 host key is known for [127.0.0.1]:42022 and you have requested strict checking.\nHost key verification failed.\n", SshError::HostKeyUnknown("devbox".into())),
            ("me@devbox: Permission denied (publickey).\n", SshError::AuthFailed("devbox".into())),
            ("channel 2: open failed: administratively prohibited: open failed\n", SshError::ForwardingRefused("devbox".into())),
            // OpenSSH 9.7 prints this for a missing socket and for disabled forwarding alike.
            ("channel 1: open failed: connect failed: open failed\n", SshError::NoSkiffd { destination: "devbox".into(), socket: t.socket.clone() }),
            ("ssh: Could not resolve hostname devbox: nodename nor servname provided\n", SshError::Unreachable("devbox".into())),
        ];
        for (stderr, want) in cases {
            assert_eq!(classify(stderr, &t), Some(want), "{stderr}");
        }
        assert_eq!(
            classify("Load key \"/Users/me/.ssh/id\": bad permissions\n", &t),
            None
        );
        // The probe splits the one line in two.
        let refused = SshError::NoSkiffd { destination: "devbox".into(), socket: t.socket.clone() };
        assert_eq!(refused.clone().refine(false), refused);
        assert_eq!(refused.refine(true), SshError::ForwardingRefused("devbox".into()));
        // The messages name the destination and never ssh's own lines.
        let text = SshError::AuthFailed("devbox".into()).to_string();
        assert!(text.contains("devbox") && !text.contains(".ssh"));
    }

    #[test]
    fn a_shared_or_foreign_directory_is_refused_not_repaired() {
        let dir = std::env::temp_dir().join(format!("skiff-ssh-dir-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        private_dir(&dir).unwrap();
        assert_eq!(
            std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(private_dir(&dir).is_err());
        assert_eq!(
            std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
            0o755
        );
        std::fs::remove_dir(&dir).unwrap();
    }
}
