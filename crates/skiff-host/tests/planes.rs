//! Two real processes, as on a Host: a private skiffd (session plane) and
//! `skiff-host` (data plane) on the same private state, each on its own
//! socket. Stopping and starting `skiff-host` leaves skiffd, its PTY and the
//! app's session connection untouched; both report one durable Host id, and
//! each has an instance of its own.
//!
//! Needs the skiffd binary of this build (`cargo build -p skiffd`), found
//! beside this package's `skiff-host` or at `SKIFFD_BIN`.
#![cfg(unix)]

use std::{
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

use skiff_client::host::{
    data::{self, DataError},
    local::{legacy_create, legacy_input, legacy_inventory, legacy_kill, legacy_screen, HostSelection, IdentitySource},
    Continuity,
};
use skiff_host_api::Error;

struct Fixture {
    dir: PathBuf,
    children: Vec<Child>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        for c in &mut self.children {
            // SIGTERM: skiff-host exits; skiffd saves and exits.
            term(c);
        }
        for c in &mut self.children {
            let until = Instant::now() + Duration::from_secs(5);
            while c.try_wait().ok().flatten().is_none() && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(50));
            }
            let _ = c.kill();
            let _ = c.wait();
        }
        if std::thread::panicking() {
            for log in ["skiffd.log", "skiff-host.log"] {
                eprintln!("{log}: {}", std::fs::read_to_string(self.dir.join(log)).unwrap_or_default());
            }
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn term(c: &Child) {
    // SAFETY: a signal to a child this test started and has not reaped.
    unsafe { libc::kill(c.id() as i32, libc::SIGTERM) };
}

fn skiffd() -> PathBuf {
    if let Ok(p) = std::env::var("SKIFFD_BIN") {
        return p.into();
    }
    let beside = Path::new(env!("CARGO_BIN_EXE_skiff-host")).with_file_name("skiffd");
    assert!(beside.is_file(), "build skiffd first: cargo build -p skiffd (or set SKIFFD_BIN)");
    beside
}

/// The same private environment for both processes, as the app gives both.
fn command(program: &Path, dir: &Path, log: &str) -> Command {
    let mut c = Command::new(program);
    c.env("SKIFF_SOCKET", dir.join("skiffd.sock"))
        .env("SKIFF_STATE", dir.join("workspace.json"))
        .env("SKIFF_CONFIG", dir.join("config/projects.toml"))
        .env("SKIFF_LEDGER", dir.join("ledger.db"))
        .env("SKIFF_SHELL_ENV", "0")
        .env("SHELL", "/bin/sh")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(std::fs::File::options().create(true).append(true).open(dir.join(log)).unwrap());
    c
}

fn data_service(dir: &Path) -> Child {
    command(Path::new(env!("CARGO_BIN_EXE_skiff-host")), dir, "skiff-host.log")
        .arg("--session-socket")
        .arg(dir.join("skiffd.sock"))
        .spawn()
        .unwrap()
}

async fn wait_for(path: &Path) {
    let until = Instant::now() + Duration::from_secs(10);
    while tokio::net::UnixStream::connect(path).await.is_err() {
        assert!(Instant::now() < until, "nothing listens at {}", path.display());
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn screen_shows(c: &skiff_client::host::HostConnection, session: &str, text: &str) {
    let until = Instant::now() + Duration::from_secs(10);
    loop {
        let lines = legacy_screen(c, session, 24).await.unwrap();
        if lines.iter().any(|l| l.contains(text)) {
            return;
        }
        assert!(Instant::now() < until, "{text} never showed: {lines:?}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn restarting_the_data_plane_keeps_every_pty_and_the_host() {
    let dir = PathBuf::from("/tmp").join(format!("sp-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("config")).unwrap();
    std::fs::write(dir.join("config/projects.toml"), "").unwrap();
    let mut fx = Fixture { dir: dir.clone(), children: vec![] };
    let session_socket = dir.join("skiffd.sock");
    let data_socket = skiff_core::socket::data_socket(&session_socket);

    fx.children.push(command(&skiffd(), &dir, "skiffd.log").spawn().unwrap());
    wait_for(&session_socket).await;
    fx.children.push(data_service(&dir));
    wait_for(&data_socket).await;

    // The session plane, accepted as the app accepts it.
    let selection = HostSelection::new(session_socket.clone(), dir.join("bootstrap-id"));
    let mut pending = selection.connect().await.unwrap();
    pending.daemon_info().await.unwrap();
    let host = pending.activate().await.unwrap();
    assert_eq!(host.identity_source(), IdentitySource::Host);
    let sessions = host.connection();
    let host_id = sessions.hello().host.host_id.clone();
    let session_instance = sessions.hello().instance_id.clone();

    // The data plane: the same Host, an instance of its own.
    let plane = selection.connect_data(&host_id).await.unwrap();
    let first = plane.connection().hello().clone();
    assert_eq!(first.host, sessions.hello().host, "one Host: id, os and account");
    assert_ne!(first.instance_id, session_instance);
    assert!(data::project_list(&plane.connection()).await.unwrap().is_empty());

    // A live PTY on the session plane.
    let spec = skiff_core::session::SessionSpec { cwd: Some(dir.clone()), cols: 80, rows: 24, ..Default::default() };
    let s = legacy_create(&sessions, spec).await.unwrap();
    legacy_input(&sessions, &s.id, b"echo before-$((6*7))\n".to_vec()).unwrap();
    screen_shows(&sessions, &s.id, "before-42").await;

    // Stop the data plane. Its connection is lost; nothing is replayed.
    let mut service = fx.children.pop().unwrap();
    term(&service);
    assert!(service.wait().unwrap().success(), "skiff-host exits cleanly on SIGTERM");
    assert!(!data_socket.exists(), "it removes its own socket");
    let lost = data::project_list(&plane.connection()).await;
    assert!(matches!(lost, Err(Error::TransportLost)), "{lost:?}");
    let down = selection.connect_data(&host_id).await.err().unwrap();
    assert!(matches!(down.downcast_ref(), Some(DataError::NotRunning)), "{down:#}");

    // The PTY, its process and the session connection are untouched.
    assert!(!sessions.is_closed());
    legacy_input(&sessions, &s.id, b"echo during-$((6*8))\n".to_vec()).unwrap();
    screen_shows(&sessions, &s.id, "during-48").await;
    let rows = legacy_inventory(&sessions).await.unwrap();
    assert_eq!(rows.iter().find(|r| r.id == s.id).unwrap().pid, s.pid);

    // Start it again: the same Host, a new instance; skiffd's is unchanged.
    fx.children.push(data_service(&dir));
    wait_for(&data_socket).await;
    let again = selection.connect_data(&host_id).await.unwrap();
    assert_eq!(again.connection().continuity(), Continuity::NewInstance);
    assert_eq!(again.connection().hello().host.host_id, host_id);
    assert_ne!(again.connection().hello().instance_id, first.instance_id);
    assert_eq!(sessions.hello().instance_id, session_instance);
    legacy_input(&sessions, &s.id, b"echo after-$((6*9))\n".to_vec()).unwrap();
    screen_shows(&sessions, &s.id, "after-54").await;

    legacy_kill(&sessions, &s.id).await.unwrap();
    drop(again);
    drop(plane);
    drop(host);
    drop(fx);
}
