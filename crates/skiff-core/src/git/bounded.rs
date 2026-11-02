//! `git` run with a deadline and an output limit, in the folder the root holds open.

use super::Root;

use std::{
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

use anyhow::{anyhow, bail, Context, Result};

/// `git`, started in the folder `root` holds open rather than at its path:
/// a path swapped for another folder, even for a moment, is never read.
/// `dir` names `root`, for a system that cannot start a process in an open folder.
pub(super) fn git_in(root: &Root, dir: &Path) -> Command {
    let mut cmd = Command::new("git");
    #[cfg(unix)]
    {
        use std::os::{fd::AsRawFd, unix::process::CommandExt};
        let _ = dir;
        let fd = root.dir.as_raw_fd();
        // SAFETY: fchdir(2) is async-signal-safe. The fd is `root`'s, open
        // while the caller borrows it, and the child inherits it until exec.
        unsafe {
            cmd.pre_exec(move || if libc::fchdir(fd) == 0 { Ok(()) } else { Err(std::io::Error::last_os_error()) });
        }
    }
    #[cfg(not(unix))]
    {
        let _ = root;
        cmd.arg("-C").arg(dir);
    }
    cmd
}

/// When a run, or several that share it, must be done.
#[derive(Clone, Copy)]
pub(crate) struct Deadline {
    at: Instant,
    after: Duration,
}

impl Deadline {
    pub(crate) fn after(after: Duration) -> Deadline {
        Deadline { at: Instant::now() + after, after }
    }
}

/// Runs `git <args>` in `root` with [`bounded`].
pub(super) fn git_bounded(root: &Root, dir: &Path, args: &[&str], limit: usize, deadline: Deadline) -> Result<(Vec<u8>, bool)> {
    git_run(git_in(root, dir), args, limit, deadline)
}

/// Runs the git command `cmd` with `args` under [`bounded`], as a read:
/// git takes no optional lock and writes no index. A failure carries git's
/// own words.
pub(super) fn git_run(mut cmd: Command, args: &[&str], limit: usize, deadline: Deadline) -> Result<(Vec<u8>, bool)> {
    cmd.args(args).env("GIT_OPTIONAL_LOCKS", "0");
    bounded(cmd, limit, deadline).map_err(|e| match e.downcast_ref::<Failed>() {
        Some(Failed(err)) if err.is_empty() => anyhow!("git {} failed", args.join(" ")),
        Some(Failed(err)) => anyhow!("{err}"),
        None => e,
    })
}

/// A command that exited with an error, and the start of what it printed there.
#[derive(Debug)]
struct Failed(String);
impl std::fmt::Display for Failed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for Failed {}

/// How long a command's pipes are still read after it exits: a helper it
/// started may hold them open, and never close them.
const DRAIN_GRACE: Duration = Duration::from_millis(200);
/// The start of a command's errors that is kept.
const ERROR_LIMIT: usize = 4096;

/// Runs `cmd` and keeps at most `limit` bytes of its output, and the start
/// of its errors. More output, or a run past `deadline`, kills it. True: the
/// output was cut. Its pipes are read here, never past the deadline, nor
/// past [`DRAIN_GRACE`] after it exits; then the process group it leads,
/// any helper it started included, is killed.
#[cfg(unix)]
pub(super) fn bounded(mut cmd: Command, limit: usize, deadline: Deadline) -> Result<(Vec<u8>, bool)> {
    use std::{
        io::Read,
        os::{fd::AsRawFd, fd::OwnedFd, unix::process::CommandExt},
    };
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .context("start the command")?;
    let pid = child.id() as libc::pid_t;
    let mut pipes: [Option<std::fs::File>; 2] = [
        child.stdout.take().map(|p| OwnedFd::from(p).into()),
        child.stderr.take().map(|p| OwnedFd::from(p).into()),
    ];
    for p in pipes.iter().flatten() {
        // SAFETY: fcntl(2) on an fd this function owns.
        unsafe {
            let flags = libc::fcntl(p.as_raw_fd(), libc::F_GETFL);
            libc::fcntl(p.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK);
        }
    }
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let (mut truncated, mut timed_out) = (false, false);
    let mut exited: Option<Instant> = None;
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        if exited.is_none() && has_exited(pid) {
            exited = Some(Instant::now());
        }
        let open: Vec<usize> = (0..2).filter(|&i| pipes[i].is_some()).collect();
        if open.is_empty() && exited.is_some() {
            break;
        }
        let end = exited.map_or(deadline.at, |t| deadline.at.min(t + DRAIN_GRACE));
        let now = Instant::now();
        if now >= end {
            timed_out = exited.is_none();
            break;
        }
        // Woken now and then to see whether the command exited.
        let wait = (end - now).min(Duration::from_millis(20));
        let mut fds: Vec<libc::pollfd> = open
            .iter()
            .map(|&i| libc::pollfd { fd: pipes[i].as_ref().unwrap().as_raw_fd(), events: libc::POLLIN, revents: 0 })
            .collect();
        // SAFETY: `fds` holds `fds.len()` entries for fds open here.
        if unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, wait.as_millis().max(1) as libc::c_int) } < 0 {
            let e = std::io::Error::last_os_error();
            if e.kind() != std::io::ErrorKind::Interrupted {
                return Err(e).context("wait for the command's output");
            }
            continue;
        }
        for (fd, &i) in fds.iter().zip(&open) {
            if fd.revents == 0 {
                continue;
            }
            match pipes[i].as_mut().unwrap().read(&mut buf) {
                Ok(0) => pipes[i] = None,
                Ok(n) => {
                    // Read to the end, so the command never waits on a full pipe; the start is kept.
                    let (kept, most) = if i == 0 { (&mut out, limit + 1) } else { (&mut err, ERROR_LIMIT) };
                    let room = most.saturating_sub(kept.len());
                    kept.extend_from_slice(&buf[..n.min(room)]);
                }
                Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted) => {}
                Err(_) => pipes[i] = None,
            }
        }
        if out.len() > limit {
            truncated = true;
            out.truncate(limit);
            break;
        }
    }
    drop(pipes);
    // The leader is not reaped yet, so the group id is still its own and the
    // kill reaches no other process. A helper that left the group is cut off
    // by the closed pipes instead.
    // SAFETY: kill(2) of this command's own process group.
    unsafe { libc::kill(-pid, libc::SIGKILL) };
    let status = child.wait().context("wait for the command")?;
    let err = String::from_utf8_lossy(&err).trim().to_string();
    if timed_out {
        bail!("git took longer than {}s", deadline.after.as_secs_f32());
    }
    if !truncated && !status.success() {
        return Err(Failed(err).into());
    }
    Ok((out, truncated))
}

/// The child `pid` exited. It is left unreaped.
#[cfg(unix)]
fn has_exited(pid: libc::pid_t) -> bool {
    // SAFETY: waitid(2) with WNOWAIT only reads the child's state.
    unsafe {
        let mut info: libc::siginfo_t = std::mem::zeroed();
        let r = libc::waitid(libc::P_PID, pid as libc::id_t, &mut info, libc::WEXITED | libc::WNOHANG | libc::WNOWAIT);
        // With WNOHANG and no exit yet, the pid stays zero.
        r == 0 && info.si_pid() != 0
    }
}

/// Without process groups: the output is cut at `limit`, and the time is not bounded.
#[cfg(not(unix))]
pub(super) fn bounded(mut cmd: Command, limit: usize, _deadline: Deadline) -> Result<(Vec<u8>, bool)> {
    let o = cmd.stdin(Stdio::null()).output().context("start the command")?;
    if !o.status.success() {
        return Err(Failed(String::from_utf8_lossy(&o.stderr).trim().to_string()).into());
    }
    let mut out = o.stdout;
    let truncated = out.len() > limit;
    out.truncate(limit);
    Ok((out, truncated))
}
