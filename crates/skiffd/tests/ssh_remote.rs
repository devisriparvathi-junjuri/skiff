//! M2 over a real SSH stream-local forward: a private skiffd, a private
//! loopback sshd with throwaway keys and known_hosts, and the system `ssh`
//! with a private config (`-F`). Nothing reads or writes the user's ~/.ssh,
//! and no other host is contacted. Skips when this machine has no usable sshd.
#![cfg(unix)]

use std::{
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

use skiff_client::{
    host::{
        local::{
            legacy_attach, legacy_create, legacy_kill, legacy_output, HostSelection, IdentitySource, SelectedHost,
        },
        ssh::{SshError, SshOptions, SshTarget},
        Continuity, OutputGap, TerminalFrame,
    },
    Client,
};
use skiff_core::session::SessionSpec;
use skiff_host_api::Capability;

mod common;

const SSHD: &str = "/usr/sbin/sshd";
const SSH: &str = "/usr/bin/ssh";

struct Fixture {
    dir: PathBuf,
    daemon: Child,
    sshd: Option<Child>,
    port: u16,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop_sshd();
        // Our own `ssh` clients, by their private config path only.
        let _ = Command::new("pkill").arg("-f").arg(self.dir.join("ssh_config")).status();
        let stopped = common::stop(&mut self.daemon, &self.dir);
        if std::thread::panicking() {
            for log in ["daemon.log", "sshd.log"] {
                eprintln!("--- {log}\n{}", std::fs::read_to_string(self.dir.join(log)).unwrap_or_default());
            }
        }
        if stopped {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
}

impl Fixture {
    fn socket(&self) -> PathBuf {
        self.dir.join("skiffd.sock")
    }

    fn write(&self, name: &str, text: &str) {
        std::fs::write(self.dir.join(name), text).unwrap();
    }

    fn sshd_config(&self, name: &str, forwarding: bool) {
        let d = self.dir.display();
        self.write(name, &format!(
            "Port {}\nListenAddress 127.0.0.1\nHostKey {d}/host_key\nAuthorizedKeysFile {d}/authorized_keys\n\
             PidFile {d}/sshd.pid\nStrictModes no\nUsePAM no\nPasswordAuthentication no\n\
             KbdInteractiveAuthentication no\nAllowStreamLocalForwarding {}\nLogLevel VERBOSE\n",
            self.port,
            if forwarding { "yes" } else { "no" }
        ));
    }

    /// Starts sshd; false when this machine cannot run one unprivileged.
    fn start_sshd(&mut self, config: &str) -> bool {
        self.stop_sshd();
        let mut child = Command::new(SSHD)
            .arg("-D")
            .arg("-f")
            .arg(self.dir.join(config))
            .arg("-E")
            .arg(self.dir.join("sshd.log"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .unwrap();
        let until = Instant::now() + Duration::from_secs(5);
        while Instant::now() < until {
            if std::net::TcpStream::connect(("127.0.0.1", self.port)).is_ok() {
                self.sshd = Some(child);
                return true;
            }
            if child.try_wait().unwrap().is_some() {
                return false;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = child.kill();
        let _ = child.wait();
        false
    }

    /// Stops the listener and every connection it serves: its process group.
    fn stop_sshd(&mut self) {
        if let Some(mut child) = self.sshd.take() {
            // SAFETY: a plain signal to this test's own sshd process group.
            unsafe { libc::kill(-(child.id() as i32), libc::SIGTERM) };
            let _ = child.wait();
        }
    }

    fn options(&self, config: &str) -> SshOptions {
        SshOptions { program: SSH.into(), config: Some(self.dir.join(config)) }
    }

    fn selection(&self, config: &str, socket: &Path, forward: &str) -> HostSelection {
        let target = SshTarget::new("fixture", socket.to_string_lossy().into_owned());
        HostSelection::ssh(target, self.options(config), self.dir.join(forward), None).unwrap()
    }
}

fn keygen(path: &Path) {
    let ok = Command::new("ssh-keygen").args(["-q", "-t", "ed25519", "-N", ""]).arg("-f").arg(path).status().unwrap();
    assert!(ok.success());
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

fn fixture(name: &str) -> Option<Fixture> {
    fixture_with(name, false)
}

fn fixture_with(name: &str, no_identity: bool) -> Option<Fixture> {
    if !Path::new(SSHD).is_file() || !Path::new(SSH).is_file() {
        eprintln!("skipped: no {SSHD} or {SSH}");
        return None;
    }
    // Short: the forward's socket path must fit sun_path.
    let dir = std::env::temp_dir().join(format!("skssh-{name}{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    if no_identity {
        // A store skiffd refuses to read: its pong then reports no identity,
        // as an older skiffd's does.
        std::fs::create_dir_all(dir.join("config")).unwrap();
        std::fs::write(dir.join("config/local-host-id"), "not an id").unwrap();
        std::fs::set_permissions(dir.join("config/local-host-id"), std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let log = std::fs::File::create(dir.join("daemon.log")).unwrap();
    let daemon = Command::new(env!("CARGO_BIN_EXE_skiffd"))
        .env("SKIFF_SOCKET", dir.join("skiffd.sock"))
        .env("SKIFF_STATE", dir.join("workspace.json"))
        .env("SKIFF_CONFIG", dir.join("config/projects.toml"))
        .env("SKIFF_LEDGER", dir.join("ledger.db"))
        .env("SKIFF_SHELL_ENV", "0")
        .env("SHELL", "/bin/sh")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log)
        .spawn()
        .unwrap();
    let mut f = Fixture { dir, daemon, sshd: None, port: free_port() };
    keygen(&f.dir.join("host_key"));
    keygen(&f.dir.join("other_host_key"));
    keygen(&f.dir.join("client_key"));
    std::fs::copy(f.dir.join("client_key.pub"), f.dir.join("authorized_keys")).unwrap();
    let key = |name: &str| {
        let text = std::fs::read_to_string(f.dir.join(name)).unwrap();
        text.split_whitespace().take(2).collect::<Vec<_>>().join(" ")
    };
    let known = format!("[127.0.0.1]:{} {}\n", f.port, key("host_key.pub"));
    let changed = format!("[127.0.0.1]:{} {}\n", f.port, key("other_host_key.pub"));
    f.write("known_hosts", &known);
    f.write("changed_hosts", &changed);
    f.write("empty_hosts", "");
    for (config, hosts) in [("ssh_config", "known_hosts"), ("unknown_config", "empty_hosts"), ("changed_config", "changed_hosts")] {
        let d = f.dir.display();
        f.write(config, &format!(
            "Host fixture\n  HostName 127.0.0.1\n  Port {}\n  IdentityFile {d}/client_key\n  IdentitiesOnly yes\n  \
             UserKnownHostsFile {d}/{hosts}\n  GlobalKnownHostsFile /dev/null\n",
            f.port
        ));
    }
    f.sshd_config("sshd_config", true);
    f.sshd_config("sshd_noforward", false);
    if !f.start_sshd("sshd_config") {
        eprintln!("skipped: {SSHD} does not run unprivileged here");
        return None;
    }
    Some(f)
}

async fn local_client(socket: &Path) -> Client {
    let until = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(c) = Client::connect(socket).await {
            if c.daemon_info().await.is_ok() {
                return c;
            }
        }
        assert!(Instant::now() < until, "skiffd did not answer");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn select(selection: &HostSelection) -> anyhow::Result<SelectedHost> {
    let mut pending = selection.connect().await?;
    pending.daemon_info().await?;
    pending.activate().await
}

async fn until_seen(output: &mut skiff_client::host::TerminalOutput, needle: &str) {
    let mut seen = String::new();
    let until = tokio::time::Instant::now() + Duration::from_secs(10);
    while !seen.contains(needle) {
        match tokio::time::timeout_at(until, output.recv()).await {
            Ok(Ok(TerminalFrame::Snapshot { data, .. } | TerminalFrame::Output { data, .. })) => {
                seen.push_str(&String::from_utf8_lossy(&data))
            }
            Ok(Err(e)) => panic!("output ended ({e:?}) before {needle:?} in {seen:?}"),
            Err(_) => panic!("no {needle:?} in {seen:?}"),
        }
    }
}

fn ssh_error(e: &anyhow::Error) -> Option<&SshError> {
    e.downcast_ref::<SshError>()
}

#[tokio::test]
async fn a_remote_terminal_over_a_real_forward_survives_a_dropped_link() {
    let Some(mut f) = fixture("a") else { return };
    // The remote CLI starts the work, as M2 expects.
    let host_side = local_client(&f.socket()).await;
    let shell = host_side
        .create_session(SessionSpec { label: "remote".into(), command: Some("/bin/sh".into()), cols: 80, rows: 24, ..Default::default() })
        .await
        .unwrap();

    let selection = f.selection("ssh_config", &f.socket(), "fwd");
    assert!(!selection.is_local());
    assert!(selection.process_control().is_none(), "a remote endpoint is never managed");
    let host = select(&selection).await.unwrap();
    assert_eq!(host.identity_source(), IdentitySource::Host);
    let hello = host.connection().hello().clone();
    assert_eq!(host.connection().continuity(), Continuity::FirstConnection);
    // The same protocol: terminals, and M3's lifecycle, groups and agents.
    for cap in [Capability::session_terminal(), Capability::session_lifecycle(), Capability::session_groups(), Capability::session_agents()] {
        assert!(hello.capabilities.contains(&cap), "{cap:?}");
    }
    // The forward's peer is `ssh`: its pid is never the daemon's.
    assert_ne!(host.client().daemon_pid.map(|p| p as u32), f.daemon.id().into());

    let conn = host.connection();
    let listed = skiff_client::host::local::legacy_inventory(&conn).await.unwrap();
    assert!(listed.iter().any(|s| s.id == shell.id));
    let mut output = legacy_output(&conn, &shell.id).unwrap();
    legacy_attach(&conn, &shell.id).await.unwrap();
    skiff_client::host::local::legacy_input(&conn, &shell.id, b"echo M2-$((40+2))\n".to_vec()).unwrap();
    until_seen(&mut output, "M2-42").await;

    // The link drops: ssh ends. The stream closes and input is refused.
    f.stop_sshd();
    let _ = Command::new("pkill").arg("-f").arg(f.dir.join("ssh_config")).status();
    let until = Instant::now() + Duration::from_secs(10);
    while !conn.is_closed() {
        assert!(Instant::now() < until, "the dropped link was not noticed");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(output.recv().await.err(), Some(OutputGap::Closed));
    assert!(skiff_client::host::local::legacy_input(&conn, &shell.id, b"echo LOST\n".to_vec()).is_err());
    // The remote session runs on, the same child.
    let same = host_side.list_sessions().await.unwrap().into_iter().find(|s| s.id == shell.id).unwrap();
    assert_eq!(same.pid, shell.pid);
    drop(conn);
    drop(host);

    // While the link is down, connecting fails and waits before ssh runs again.
    let down = select(&selection).await.err().unwrap();
    assert!(matches!(ssh_error(&down), Some(SshError::Unreachable(_) | SshError::Exited { .. })), "{down:#}");
    let waiting = select(&selection).await.err().unwrap();
    assert!(matches!(ssh_error(&waiting), Some(SshError::Waiting { .. })), "{waiting:#}");

    // Back up: the same skiffd run answers, with a fresh subscription and snapshot.
    assert!(f.start_sshd("sshd_config"));
    let until = Instant::now() + Duration::from_secs(10);
    let again = loop {
        match select(&selection).await {
            Ok(host) => break host,
            Err(e) => {
                assert!(Instant::now() < until, "no reconnect: {e:#}");
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }
    };
    assert_eq!(again.connection().continuity(), Continuity::SameInstance);
    assert_eq!(again.connection().hello().host.host_id, hello.host.host_id);
    let conn = again.connection();
    let mut output = legacy_output(&conn, &shell.id).unwrap();
    legacy_attach(&conn, &shell.id).await.unwrap();
    // The snapshot redraws what ran before; LOST was never sent.
    until_seen(&mut output, "M2-42").await;
    skiff_client::host::local::legacy_input(&conn, &shell.id, b"echo BACK-$((1+1))\n".to_vec()).unwrap();
    until_seen(&mut output, "BACK-2").await;
    let screen = host_side.screen(&shell.id, 24).await.unwrap().join("\n");
    assert!(!screen.contains("LOST"), "{screen}");

    // The app quits: its forward goes, and the remote session stays.
    drop(conn);
    drop(again);
    drop(selection);
    tokio::time::sleep(Duration::from_millis(200)).await;
    // Each forward's own directory goes; the shared private parent stays.
    let left: Vec<_> = std::fs::read_dir(f.dir.join("fwd")).unwrap().collect();
    assert!(left.is_empty(), "the forward's directory is removed: {left:?}");
    let after = host_side.list_sessions().await.unwrap().into_iter().find(|s| s.id == shell.id).unwrap();
    assert_eq!((after.pid, after.state == skiff_core::session::SessionState::Done), (shell.pid, false));
}

#[tokio::test]
async fn unknown_or_changed_keys_closed_forwarding_and_a_wrong_socket_are_explained_and_refused() {
    let Some(mut f) = fixture("b") else { return };
    let _ = local_client(&f.socket()).await;
    let unknown = select(&f.selection("unknown_config", &f.socket(), "u")).await.err().unwrap();
    assert!(matches!(ssh_error(&unknown), Some(SshError::HostKeyUnknown(_))), "{unknown:#}");
    let changed = select(&f.selection("changed_config", &f.socket(), "c")).await.err().unwrap();
    assert!(matches!(ssh_error(&changed), Some(SshError::HostKeyChanged(_))), "{changed:#}");
    // Strict checking learned nothing: the fixture's known_hosts files are unchanged.
    assert_eq!(std::fs::read_to_string(f.dir.join("empty_hosts")).unwrap(), "");

    let missing = select(&f.selection("ssh_config", Path::new("/nonexistent/skiffd.sock"), "m")).await.err().unwrap();
    assert!(matches!(ssh_error(&missing), Some(SshError::NoSkiffd { .. })), "{missing:#}");

    // The sshd that blocks forwards, with the fixture's runtime folder: the
    // probe looks at the one socket there, a link to the fixture's.
    let run = f.dir.join("run/skiff");
    std::fs::create_dir_all(&run).unwrap();
    std::os::unix::fs::symlink(f.socket(), run.join("skiffd.sock")).unwrap();
    let text = std::fs::read_to_string(f.dir.join("sshd_noforward")).unwrap();
    f.write("sshd_noforward_rt", &format!("{text}SetEnv XDG_RUNTIME_DIR={}/run\n", f.dir.display()));
    assert!(f.start_sshd("sshd_noforward_rt"));
    let refused = select(&f.selection("ssh_config", &f.socket(), "r")).await.err().unwrap();
    // This sshd says no more than for a missing socket. The probe, over the
    // same sshd, sees the live socket: the cause is the blocked forward.
    let Some(SshError::NoSkiffd { .. }) = ssh_error(&refused).cloned() else {
        return assert!(matches!(ssh_error(&refused), Some(SshError::ForwardingRefused(_))), "{refused:#}");
    };
    let target = SshTarget::new("fixture", f.socket().to_string_lossy().into_owned());
    let run = skiff_client::host::check::run_probe(&target, &f.options("ssh_config")).await.unwrap();
    let skiff_client::host::check::ProbeRun::Probed(probe, _) = run else { panic!("the probe did not run") };
    assert!(probe.socket_live, "{probe:?}");
    let refined = ssh_error(&refused).cloned().unwrap().refine(probe.socket_live);
    assert!(matches!(refined, SshError::ForwardingRefused(_)), "{refined}");
    assert!(refined.to_string().contains("AllowStreamLocalForwarding"), "{refined}");
    // A missing socket stays a missing skiffd.
    let missing = ssh_error(&missing).cloned().unwrap().refine(false);
    assert!(matches!(missing, SshError::NoSkiffd { .. }), "{missing}");
}

#[tokio::test]
async fn a_remote_skiffd_without_identity_is_refused_and_no_local_identity_stands_in() {
    let Some(f) = fixture_with("c", true) else { return };
    let info = local_client(&f.socket()).await.daemon_info().await.unwrap();
    assert_eq!(info.identity, None);
    let selection = f.selection("ssh_config", &f.socket(), "i");
    let refused = select(&selection).await.err().unwrap();
    assert!(refused.to_string().contains("reports no Host identity"), "{refused:#}");
    // No bootstrap file was read or made for the remote Host.
    assert!(!f.dir.join("i/local-host-id").exists());
    assert!(std::fs::read_dir(&f.dir).unwrap().all(|e| e.unwrap().file_name() != "local-host-id"));
}

/// M4 over a real forward: the Host's `skiff-host` (a real process with the
/// Host's private state, started on the Host side by the test, never by the
/// app) serves that Host's projects and Git status through a second forward
/// to the socket beside its skiffd. Both planes report one Host. Stopping
/// the data plane leaves the remote terminal working.
#[tokio::test]
async fn remote_projects_and_git_status_come_from_the_hosts_data_plane_over_a_real_forward() {
    use skiff_client::host::data::{self, DataError};
    let Some(f) = fixture("d") else { return };
    let _ = local_client(&f.socket()).await;
    let repo = f.dir.join("host repo");
    std::fs::create_dir_all(&repo).unwrap();
    let git = |args: &[&str]| {
        let ok = Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false", "-C"])
            .arg(&repo)
            .args(args)
            .output()
            .unwrap();
        assert!(ok.status.success(), "{}", String::from_utf8_lossy(&ok.stderr));
    };
    git(&["init", "-q"]);
    std::fs::write(repo.join("a.txt"), "one\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-qm", "init"]);
    std::fs::write(repo.join("a.txt"), "one\ntwo\n").unwrap();
    std::fs::create_dir_all(f.dir.join("config")).unwrap();
    f.write("config/projects.toml", &format!("[[project]]\nname = \"on the Host\"\npath = \"{}\"\nfleet = \"far\"\n", repo.display()));
    // M6: a private ledger with one result inside its folder and one outside.
    let ledger = f.dir.join("agent/ledger.db");
    std::fs::create_dir_all(f.dir.join("agent/results")).unwrap();
    std::fs::write(f.dir.join("agent/results/r.md"), "# Result\n").unwrap();
    let inside = f.dir.join("agent/results/r.md").display().to_string();
    let outside = f.dir.join("config/projects.toml").display().to_string();
    let sql = format!(
        "CREATE TABLE tasks (id INTEGER PRIMARY KEY, uid TEXT UNIQUE, kind TEXT, parent TEXT, pane_id TEXT, pane_gen INTEGER, project TEXT, task TEXT,
           sent_at TEXT, state TEXT, result_path TEXT, ref TEXT, updated_at TEXT, type TEXT, socket TEXT, reported_at TEXT, title TEXT, closed_at TEXT, gone_at TEXT, owner TEXT NOT NULL DEFAULT 'grand', reopened_at TEXT);
         INSERT INTO tasks (uid, kind, project, task, sent_at, state, result_path, updated_at) VALUES ('aaaa0001', 'task', '', 'in', 'x', 'done', '{inside}', 'x');
         INSERT INTO tasks (uid, kind, project, task, sent_at, state, result_path, updated_at) VALUES ('aaaa0002', 'task', '', 'out', 'x', 'done', '{outside}', 'x');
         PRAGMA user_version = 10;"
    );
    let made = Command::new("sqlite3").arg(&ledger).arg(sql).output();
    let ledger_made = made.is_ok_and(|o| o.status.success());
    if !ledger_made {
        eprintln!("skipped the M6 ledger check: no sqlite3");
    }

    let binary = std::env::var_os("SKIFF_HOST_BIN").map(PathBuf::from).unwrap_or_else(|| Path::new(env!("CARGO_BIN_EXE_skiffd")).with_file_name("skiff-host"));
    assert!(binary.is_file(), "build skiff-host first: cargo build -p skiff-host (or set SKIFF_HOST_BIN)");
    let start = || {
        Command::new(&binary)
            .arg("--session-socket")
            .arg(f.socket())
            .env("SKIFF_CONFIG", f.dir.join("config/projects.toml"))
            .env("SKIFF_LEDGER", &ledger)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(std::fs::File::options().create(true).append(true).open(f.dir.join("skiff-host.log")).unwrap())
            .spawn()
            .unwrap()
    };
    struct Service(Child);
    impl Drop for Service {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let service = Service(start());
    let data_socket = skiff_core::socket::data_socket(&f.socket());
    let until = Instant::now() + Duration::from_secs(10);
    while std::os::unix::net::UnixStream::connect(&data_socket).is_err() {
        assert!(Instant::now() < until, "skiff-host did not listen");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let selection = f.selection("ssh_config", &f.socket(), "fwd");
    let host = select(&selection).await.unwrap();
    let sessions = host.connection();
    let host_id = sessions.hello().host.host_id.clone();
    let plane = selection.connect_data(&host_id).await.unwrap();
    let c = plane.connection();
    assert_eq!(c.hello().host, sessions.hello().host, "one Host on both forwards");
    assert_ne!(c.hello().instance_id, sessions.hello().instance_id);

    let projects = data::project_list(&c).await.unwrap();
    assert_eq!(projects.len(), 1);
    assert_eq!((projects[0].name.as_str(), projects[0].fleet.as_deref()), ("on the Host", Some("far")));
    let worktree = projects[0].worktrees[0].path.clone();
    assert_eq!(Path::new(&worktree).canonicalize().unwrap(), repo.canonicalize().unwrap());
    let changes = data::git_changes(&c, &worktree).await.unwrap().changes;
    assert_eq!(changes.len(), 1);
    assert_eq!((changes[0].path.as_str(), changes[0].added, changes[0].removed), ("a.txt", Some(1), Some(0)));
    // Its diff text comes from the Host's git over the same forward.
    let diff = data::git_diff(&c, &worktree, Some("a.txt")).await.unwrap();
    assert!(diff.text.contains("a/a.txt") && !diff.truncated && !diff.binary, "{diff:?}");
    // M5: its files, listed and read on the Host over the same forward.
    let listing = data::file_list(&c, &worktree, "", false).await.unwrap();
    assert_eq!(listing.entries.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(), ["a.txt"]);
    let read = data::file_read(&c, &worktree, "a.txt", skiff_host_api::ReadPurpose::Copy).await.unwrap();
    assert_eq!(read.bytes.as_deref(), Some(b"one\ntwo\n".as_slice()));
    assert!(matches!(data::file_read(&c, &worktree, "../x", skiff_host_api::ReadPurpose::Preview).await, Err(skiff_host_api::Error::NotFound { .. })));
    // M6: the Host's ledger rows and a result by its row, over the same forward;
    // a result outside the ledger's folder is not served.
    if ledger_made {
        let rows = data::ledger_rows(&c).await.unwrap();
        assert_eq!(rows.iter().map(|r| (r.uid.as_str(), r.result_exists)).collect::<Vec<_>>(), [("aaaa0001", true), ("aaaa0002", false)]);
        assert_eq!(data::ledger_result(&c, "aaaa0001").await.unwrap().text.as_deref(), Some("# Result\n"));
        assert!(matches!(data::ledger_result(&c, "aaaa0002").await, Err(skiff_host_api::Error::NotFound { .. })));
        assert!(!data::ledger_stamp(&c).await.unwrap().is_empty());
        assert!(data::skipper_settings(&c).await.is_ok());
    }

    // A terminal on the session plane, then the data plane stops on the Host.
    let spec = SessionSpec { cwd: Some(repo.clone()), cols: 80, rows: 24, ..Default::default() };
    let s = legacy_create(&sessions, spec).await.unwrap();
    let mut output = legacy_output(&sessions, &s.id).unwrap();
    legacy_attach(&sessions, &s.id).await.unwrap();
    // SAFETY: the test's own child.
    unsafe { libc::kill(service.0.id() as i32, libc::SIGTERM) };
    drop(service);
    assert!(matches!(data::project_list(&c).await, Err(skiff_host_api::Error::TransportLost)));
    drop(plane);
    let down = selection.connect_data(&host_id).await.err().unwrap();
    assert!(matches!(down.downcast_ref(), Some(DataError::NotRunning)), "{down:#}");
    skiff_client::host::local::legacy_input(&sessions, &s.id, b"echo STILL-$((7*6))\n".to_vec()).unwrap();
    until_seen(&mut output, "STILL-42").await;
    assert!(!sessions.is_closed());
    legacy_kill(&sessions, &s.id).await.unwrap();
}

/// #166 slice 3, over the loopback sshd. Unknown key: a scan learns it into
/// a temp file only, a trust adds one known_hosts line, a second trust adds
/// none. Changed key: replace leaves only the new one. Install: one secret
/// through askpass adds the key once; a second install adds nothing, and a
/// wrong secret changes nothing. The server's home is a temp folder
/// (sshd SetEnv HOME), and every known_hosts is the fixture's: nothing
/// touches this account's ~/.ssh.
#[tokio::test]
async fn a_host_key_is_trusted_once_and_a_key_is_installed_once() {
    use skiff_client::host::{check, keys};
    let Some(mut f) = fixture("k") else { return };
    let d = f.dir.clone();
    let target = SshTarget::new("fixture", "");
    let lines = |name: &str| std::fs::read_to_string(d.join(name)).unwrap().lines().filter(|l| !l.is_empty()).count();
    let probed = |r: check::ProbeRun| matches!(r, check::ProbeRun::Probed(..));

    // Unknown key.
    let unknown = f.options("unknown_config");
    assert!(!probed(check::run_probe(&target, &unknown).await.unwrap()));
    let scan = keys::scan_host_key(&target, &unknown, &d).await.unwrap();
    assert_eq!(scan.file, d.join("empty_hosts"));
    assert_eq!((scan.lines.len(), scan.fingerprints.len(), scan.known.len()), (1, 1, 0), "{scan:?}");
    assert_eq!(lines("empty_hosts"), 0, "the scan wrote nothing the person owns");
    keys::trust_host_key(&scan, false).await.unwrap();
    keys::trust_host_key(&scan, false).await.unwrap();
    assert_eq!(lines("empty_hosts"), 1);
    assert!(probed(check::run_probe(&target, &unknown).await.unwrap()), "strict checking now passes");

    // Changed key: the old one shows, and replace keeps only the new one.
    let changed = f.options("changed_config");
    let scan = keys::scan_host_key(&target, &changed, &d).await.unwrap();
    assert_eq!(scan.known.len(), 1);
    assert_ne!(scan.known[0], scan.fingerprints[0]);
    keys::trust_host_key(&scan, true).await.unwrap();
    assert_eq!(lines("changed_hosts"), 1);
    assert!(probed(check::run_probe(&target, &changed).await.unwrap()));

    // Install. A locked key stands in for the password, which an
    // unprivileged sshd cannot check: the same askpass path carries it.
    std::fs::create_dir_all(d.join("home")).unwrap();
    for (name, pass) in [("locked_key", "pass one"), ("new_key", "")] {
        assert!(Command::new("ssh-keygen").args(["-q", "-t", "ed25519", "-N", pass, "-f"]).arg(d.join(name)).status().unwrap().success());
    }
    let mut authorized = std::fs::read_to_string(d.join("authorized_keys")).unwrap();
    authorized.push_str(&std::fs::read_to_string(d.join("locked_key.pub")).unwrap());
    f.write("authorized_keys", &authorized);
    let conf = f.dir.join("sshd_config");
    let text = std::fs::read_to_string(&conf).unwrap().replace(
        &format!("AuthorizedKeysFile {}/authorized_keys", d.display()),
        &format!("AuthorizedKeysFile {0}/authorized_keys {0}/home/.ssh/authorized_keys\nSetEnv HOME={0}/home", d.display()),
    );
    f.write("sshd_home", &text);
    assert!(f.start_sshd("sshd_home"));
    for (config, key) in [("locked_config", "locked_key"), ("new_config", "new_key")] {
        f.write(config, &format!(
            "Host fixture\n  HostName 127.0.0.1\n  Port {}\n  IdentityFile {}/{key}\n  IdentitiesOnly yes\n  UserKnownHostsFile {}/known_hosts\n  GlobalKnownHostsFile /dev/null\n",
            f.port, d.display(), d.display()
        ));
    }
    let new_key = std::fs::read_to_string(d.join("new_key.pub")).unwrap();
    let with_new = f.options("new_config");
    assert!(!probed(check::run_probe(&target, &with_new).await.unwrap()), "the new key is not there yet");
    let locked = f.options("locked_config");
    let wrong = keys::install_key(&target, &locked, "publickey", &new_key, "wrong".into()).await;
    assert_eq!(wrong, Err(keys::StepError::Refused));
    assert!(!d.join("home/.ssh/authorized_keys").exists(), "a refused secret changed nothing");
    let first = keys::install_key(&target, &locked, "publickey", &new_key, "pass one".into()).await.unwrap();
    assert_eq!(first, keys::Installed { added: true, ssh_dir_own: true, file_own: true });
    let second = keys::install_key(&target, &locked, "publickey", &new_key, "pass one".into()).await.unwrap();
    assert!(!second.added);
    let installed = std::fs::read_to_string(d.join("home/.ssh/authorized_keys")).unwrap();
    assert_eq!(installed.lines().filter(|l| l.trim() == new_key.trim()).count(), 1, "{installed}");
    assert!(probed(check::run_probe(&target, &with_new).await.unwrap()), "the new key logs in with no prompt");
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(std::fs::metadata(d.join("home/.ssh/authorized_keys")).unwrap().permissions().mode() & 0o777, 0o600);

    // Fix permissions: the file is put aside and a new one holds the key.
    let fixed = keys::fix_key_file(&target, &locked, "publickey", &new_key, "pass one".into()).await;
    assert_eq!(fixed, Ok(()));
    let aside = std::fs::read_dir(d.join("home/.ssh")).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().starts_with("authorized_keys.skiff-")).count();
    assert_eq!(aside, 1);
    assert!(probed(check::run_probe(&target, &with_new).await.unwrap()));
}

/// #166 slice 5, over the loopback sshd. The server's home and runtime
/// folder are temp folders (sshd SetEnv), so the skiffd it starts listens
/// in the fixture, never on this account's own socket. With no systemd
/// user manager in that session, the install takes the fallback start. A
/// second install changes nothing. Linger is never touched here.
#[tokio::test]
async fn skiff_installs_once_into_the_home_and_its_skiffd_answers() {
    use skiff_client::host::{check, install};
    let Some(mut f) = fixture("n") else { return };
    let d = f.dir.clone();
    for sub in ["home", "run", "beside", "path"] {
        std::fs::create_dir_all(d.join(sub)).unwrap();
    }
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(d.join("run"), std::fs::Permissions::from_mode(0o700)).unwrap();
    let text = std::fs::read_to_string(d.join("sshd_config")).unwrap();
    // A skiff installed on this computer must not answer for the stand-ins:
    // a silent one heads PATH.
    std::fs::write(d.join("path/skiff"), "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(d.join("path/skiff"), std::fs::Permissions::from_mode(0o755)).unwrap();
    f.write("sshd_install", &format!("{text}SetEnv HOME={0}/home XDG_RUNTIME_DIR={0}/run PATH={0}/path:/usr/bin:/bin\n", d.display()));
    assert!(f.start_sshd("sshd_install"));
    let linger = || Command::new("loginctl").args(["show-user", &std::env::var("USER").unwrap_or_default(), "-p", "Linger", "--value"]).output().map(|o| o.stdout).unwrap_or_default();
    let linger_before = linger();

    // This test's own build: the real skiffd, and stand-ins for the others.
    let skiffd = Path::new(env!("CARGO_BIN_EXE_skiffd"));
    std::fs::copy(skiffd, d.join("beside/skiffd")).unwrap();
    for b in ["skiff-host", "skiff"] {
        std::fs::write(d.join("beside").join(b), "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(d.join("beside").join(b), std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let version = "9.9.9-test";
    let arch = std::env::consts::ARCH;
    let b = install::dev_bundle(version, arch, &d.join("beside")).await.unwrap();
    let target = SshTarget::new("fixture", "");
    let options = f.options("ssh_config");
    let name = "skiffd.sock";
    let probe = |o: &SshOptions| {
        let (t, o) = (target.clone(), o.clone());
        async move {
            match check::run_probe(&t, &o).await.unwrap() {
                check::ProbeRun::Probed(p, _) => p,
                _ => panic!("the probe did not run"),
            }
        }
    };
    let before = probe(&options).await;
    assert_eq!((before.managed.as_ref(), before.socket_live, before.systemd), (None, false, false), "{before:?}");
    assert_eq!(before.runtime_dir, d.join("run").to_string_lossy());

    let first = install::install(&target, &options, &b, version, before.free_kb, "fixture", &|_| {}).await.unwrap();
    let pidfile = d.join("home/.local/share/skiff/server/skiffd.pid");
    // Stops only the skiffd this test started, by its pid file.
    struct Stop(PathBuf);
    impl Drop for Stop {
        fn drop(&mut self) {
            if let Ok(pid) = std::fs::read_to_string(&self.0).map(|p| p.trim().parse::<i32>().unwrap_or(0)) {
                if pid > 0 {
                    // SAFETY: a signal to the one process this test started.
                    unsafe { libc::kill(pid, libc::SIGTERM) };
                }
            }
        }
    }
    let _stop = Stop(pidfile.clone());
    assert_eq!((first.changed, first.how.as_str(), first.running, first.cli_linked), (true, "fallback", true, true), "{first:?}");
    let pid = std::fs::read_to_string(&pidfile).unwrap();
    let after = probe(&options).await;
    assert_eq!(after.managed.as_ref().map(|m| m.version.as_str()), Some(version));
    assert!(after.socket_live && after.fallback, "{after:?}");
    assert!(d.join("run/skiff").join(name).exists(), "skiffd listens in the fixture's runtime folder");
    // The probe reports what skiffd there says, through a CLI it finds. The
    // managed CLI is a stand-in that prints nothing, so a cargo one answers
    // `status` for the one socket.
    let cargo = d.join("home/.cargo/bin");
    std::fs::create_dir_all(&cargo).unwrap();
    let status = r#"#!/bin/sh
printf '{"skiff":"9.9.9-test","skiffd":{"version":"9.9.9-test","protocol":27}}\n'
"#;
    std::fs::write(cargo.join("skiff"), status).unwrap();
    std::fs::set_permissions(cargo.join("skiff"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let told = probe(&options).await;
    assert!(told.socket.ends_with("/skiff/skiffd.sock") && told.socket_live, "{told:?}");
    assert_eq!(told.status.and_then(|s| s.skiffd).map(|d| d.version).as_deref(), Some(version));
    std::fs::remove_dir_all(d.join("home/.cargo")).unwrap();
    let server = d.join("home/.local/share/skiff/server");
    let key = std::fs::read_link(server.join("current")).unwrap();
    assert!(key.to_string_lossy().starts_with(&format!("{version}+dev-")), "{key:?}");
    let mtime = std::fs::metadata(server.join(&key).join("skiffd")).unwrap().modified().unwrap();

    // A second install changes nothing, and the same skiffd keeps running.
    let second = install::install(&target, &options, &b, version, after.free_kb, "fixture", &|_| {}).await.unwrap();
    assert_eq!((second.changed, second.running), (false, true), "{second:?}");
    assert_eq!(std::fs::read_to_string(&pidfile).unwrap(), pid);
    assert_eq!(std::fs::metadata(server.join(&key).join("skiffd")).unwrap().modified().unwrap(), mtime);
    assert!(!std::fs::read_dir(&server).unwrap().flatten().any(|e| e.file_name().to_string_lossy().starts_with(".incoming")), "no upload left behind");

    // The whole check list passes over the real forward; keep-running is
    // a warning (no systemd user manager here).
    let input = check::CheckInput {
        target: target.clone(),
        name: "fixture".into(),
        pin: None,
        forward_dir: d.join("chk"),
        home: None,
        agent: false,
        installed: None,
        version: version.into(),
        build: None,
        linger_refused: false,
        update_failed: None,
    };
    let report = check::check(&input, &options).await;
    assert!(report.ok, "{:#?}", report.rows);
    let keep = report.rows.last().unwrap();
    assert_eq!(keep.state, check::State::Warning);
    // Start on a running one changes nothing either.
    let again = install::start(&target, &options).await.unwrap();
    assert!(again.running);
    assert_eq!(std::fs::read_to_string(&pidfile).unwrap(), pid);
    assert_eq!(linger(), linger_before, "this account's linger did not change");
}
