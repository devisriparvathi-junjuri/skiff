use std::{os::fd::{AsRawFd, FromRawFd}, path::PathBuf};

use anyhow::{bail, Context, Result};
use clap::Parser;
use skiff_core::socket::socket_path;
use skiffd::{paths, pty, server, session::{reload, SessionPool}, shellenv, stop};
use tokio::net::{UnixListener, UnixStream};
use tracing_subscriber::filter::LevelFilter;

/// The command line. Parsed before anything else: `--version`, `--help` and
/// a wrong argument print and exit without starting a daemon.
#[derive(Parser)]
#[command(name = "skiffd", version, about = "The Skiff daemon: owns every PTY and serves clients over a Unix socket")]
struct Args {
    /// Asked by a reload of a binary it may exec. With a handover file, it
    /// must also read that file.
    #[arg(long, hide = true, value_name = "FILE", num_args = 0..=1, conflicts_with = "adopt")]
    reload_info: Option<Option<PathBuf>>,
    /// Exec'd by a reload, with the handover file.
    #[arg(long, hide = true, value_name = "FILE")]
    adopt: Option<PathBuf>,
}

/// How this process started.
enum Start {
    Fresh,
    /// Exec'd by a reload, with the handover file.
    Adopt(PathBuf),
}

fn main() -> Result<()> {
    let args = Args::parse();
    if let Some(file) = args.reload_info {
        println!("{}", serde_json::to_string(&reload::reload_info())?);
        if let Some(file) = file {
            reload::load(&file)?;
        }
        return Ok(());
    }
    let start = match args.adopt {
        Some(file) => Start::Adopt(file),
        None => Start::Fresh,
    };
    // Before the runtime starts any thread: it sets environment variables.
    // A reload kept the environment it set before.
    server::take_test_version();
    if let Start::Fresh = start {
        shellenv::import();
    }
    tokio::runtime::Runtime::new()?.block_on(run(start))
}

async fn run(start: Start) -> Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stderr()))
        // RUST_LOG takes one level, such as `debug`. Anything else means info.
        .with_max_level(
            std::env::var("RUST_LOG").ok().and_then(|l| l.parse().ok()).unwrap_or(LevelFilter::INFO),
        )
        .init();

    let pool = SessionPool::new();
    let listener = match start {
        Start::Fresh => {
            let path = socket_path();
            let listener = bind(&path).await?;
            paths::set_socket(bound(&listener).unwrap_or(path));
            pool.restore();
            listener
        }
        Start::Adopt(file) => match adopt(&pool, &file) {
            Ok(listener) => listener,
            Err(e) => {
                // The sessions end with this process. The workspace file
                // still has their layout for the next start.
                tracing::error!("reload failed after exec; sessions are lost: {e:#}");
                let _ = std::fs::remove_file(&file);
                // Only the socket the old image listened on. A computed
                // path may belong to another daemon.
                if let Some(path) = paths::listening() {
                    let _ = std::fs::remove_file(path);
                }
                return Err(e);
            }
        },
    };
    // After the socket: it selects private or real state.
    skiffd::identity::init();
    pool.set_listener(listener.as_raw_fd());
    pool.spawn_saver();
    pool.spawn_idle_watcher();
    pool.spawn_away_watcher();
    pool.spawn_foreground_watcher();
    let path = paths::socket();
    tracing::info!("skiffd {} listening on {}", skiff_core::VERSION, path.display());
    match skiff_core::state::private_dir() {
        Some(d) => tracing::info!("private state in {}: not the live socket. {}=1 uses the real state", d.display(), skiff_core::state::REAL_ENV),
        None => tracing::info!("real state: {}", skiff_core::config::config_path().display()),
    }

    // SIGTERM and SIGINT from inside a session are ignored; see `stop`.
    let mut stops = stop::install()?;

    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (stream, _) = accepted?;
                let pool = pool.clone();
                tokio::spawn(async move {
                    if let Err(e) = server::handle(stream, pool).await {
                        tracing::debug!("client closed: {e:#}");
                    }
                });
            }
            _ = stops.recv() => break,
        }
    }

    tracing::info!("skiffd shutting down");
    pool.save_now();
    let _ = std::fs::remove_file(&path);
    Ok(())
}

async fn bind(path: &std::path::Path) -> Result<UnixListener> {
    // Only a directory we create gets locked down. Never chmod a shared one like /tmp.
    if let Some(dir) = path.parent().filter(|d| !d.exists()) {
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
    }
    if path.exists() {
        // A live socket means another daemon. A dead one is leftover from a crash.
        if UnixStream::connect(path).await.is_ok() {
            bail!("skiffd is already running at {}", path.display());
        }
        std::fs::remove_file(path)?;
    }

    let listener =
        UnixListener::bind(path).with_context(|| format!("bind {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(listener)
}

/// The path the listener is bound to.
fn bound(listener: &UnixListener) -> Option<PathBuf> {
    listener.local_addr().ok()?.as_pathname().map(PathBuf::from)
}

/// Takes over the sessions and the listening socket from the image this
/// process replaced. Skips the shell probe, the bind and the restore.
fn adopt(pool: &std::sync::Arc<SessionPool>, file: &std::path::Path) -> Result<UnixListener> {
    let handover = reload::load(file).with_context(|| format!("read {}", file.display()))?;
    let _ = std::fs::remove_file(file);
    let fd = handover.listener();
    if fd < 0 {
        bail!("the handover names no listening socket");
    }
    // SAFETY: the old image passed this fd on as its listener, and nothing
    // else here owns it.
    let listener = unsafe { std::os::unix::net::UnixListener::from_raw_fd(fd) };
    // The path the old image bound, which this build may not compute itself.
    let socket = listener.local_addr().ok().and_then(|a| a.as_pathname().map(PathBuf::from));
    match &socket {
        Some(socket) => paths::set_socket(socket.clone()),
        None => tracing::warn!("the handed-over listener has no path; panes get {}", paths::socket().display()),
    }
    if let Some(state) = handover.state().map(PathBuf::from) {
        paths::set_state(state);
    }
    pty::set_inherit(fd, false)?;
    listener.set_nonblocking(true)?;
    let listener = UnixListener::from_std(listener)?;
    pool.adopt(handover);
    Ok(listener)
}

#[cfg(test)]
mod tests {
    use super::Args;
    use clap::{error::ErrorKind, Parser};
    use std::path::PathBuf;

    fn parse(args: &[&str]) -> Result<Args, ErrorKind> {
        Args::try_parse_from(std::iter::once("skiffd").chain(args.iter().copied())).map_err(|e| e.kind())
    }

    #[test]
    fn version_and_help_stop_before_the_daemon() {
        for a in ["--version", "-V"] {
            assert_eq!(parse(&[a]).err(), Some(ErrorKind::DisplayVersion), "{a}");
        }
        for a in ["--help", "-h"] {
            assert_eq!(parse(&[a]).err(), Some(ErrorKind::DisplayHelp), "{a}");
        }
    }

    #[test]
    fn unknown_flags_and_extra_arguments_are_errors() {
        assert_eq!(parse(&["--bogus"]).err(), Some(ErrorKind::UnknownArgument));
        assert_eq!(parse(&["extra"]).err(), Some(ErrorKind::UnknownArgument));
    }

    #[test]
    fn reload_forms_still_parse() {
        let a = parse(&[]).unwrap();
        assert!(a.reload_info.is_none() && a.adopt.is_none());
        assert_eq!(parse(&["--reload-info"]).unwrap().reload_info, Some(None));
        assert_eq!(parse(&["--reload-info", "/tmp/h"]).unwrap().reload_info, Some(Some(PathBuf::from("/tmp/h"))));
        assert_eq!(parse(&["--adopt", "/tmp/h"]).unwrap().adopt, Some(PathBuf::from("/tmp/h")));
        assert_eq!(parse(&["--adopt"]).err(), Some(ErrorKind::InvalidValue));
    }
}
