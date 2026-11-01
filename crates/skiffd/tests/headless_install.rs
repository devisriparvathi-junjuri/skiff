//! #166 slice 6b: install.sh on a server's own shell, in a temp home and
//! runtime folder. `sh < install.sh` stands in for `curl … | sh`. A fake
//! curl serves the bundles from a folder, and fake systemctl and loginctl
//! answer "no user manager", so nothing reaches this account's own user
//! manager or the network: the fallback start runs, as in a container.

use std::{
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

use skiff_client::{host::install, Client};
use skiff_core::{protocol::Event, session::SessionSpec};

const SERVER: &str = include_str!("../../skiff-client/src/host/server.sh");

struct Server {
    dir: PathBuf,
    /// The skiffd pids this test started, stopped on drop.
    pids: Vec<i32>,
}

impl Drop for Server {
    fn drop(&mut self) {
        for &pid in &self.pids {
            // SAFETY: a signal to a process this test started.
            unsafe { libc::kill(pid, libc::SIGTERM) };
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

impl Server {
    fn path(&self, p: &str) -> PathBuf {
        self.dir.join(p)
    }

    fn tool(&self, name: &str, body: &str) {
        let p = self.path("bin").join(name);
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// install.sh as the release job writes it, for `release` and the
    /// bundles in `dist`.
    fn install_sh(&self, release: &str) -> String {
        let sums = std::fs::read_to_string(self.path("dist/SHA256SUMS")).unwrap();
        SERVER.replacen("release=''", &format!("release='{release}'"), 1).replacen("sums=''", &format!("sums='{sums}'"), 1).replacen("pubkey=''", "pubkey='RWQtest'", 1)
    }

    /// Runs a script the way `curl … | sh` does: the script on stdin.
    fn pipe(&self, script: &str, args: &[&str]) -> (bool, String) {
        let mut child = Command::new("sh")
            .arg("-s")
            .arg("--")
            .args(args)
            .env_clear()
            .env("HOME", self.path("home"))
            .env("XDG_RUNTIME_DIR", self.path("run"))
            .env("PATH", format!("{}:/usr/bin:/bin", self.path("bin").display()))
            .env("USER", std::env::var("USER").unwrap_or_default())
            // Every build picks this in the fixture's runtime folder; said for clarity.
            .env("SKIFF_SOCKET", self.path("run/skiff/skiffd.sock"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        use std::io::Write;
        child.stdin.take().unwrap().write_all(script.as_bytes()).unwrap();
        let out = child.wait_with_output().unwrap();
        let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        (out.status.success(), text)
    }

    fn pid(&self) -> Option<i32> {
        std::fs::read_to_string(self.path("home/.local/share/skiff/server/skiffd.pid")).ok()?.trim().parse().ok()
    }
}

async fn client(socket: &Path) -> Client {
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

async fn seen(out: &mut skiff_client::Output, needle: &str) {
    let mut text = String::new();
    let until = tokio::time::Instant::now() + Duration::from_secs(10);
    while !text.contains(needle) {
        match tokio::time::timeout_at(until, out.recv()).await {
            Ok(Ok(Event::Output { data, .. } | Event::Snapshot { data, .. })) => text.push_str(&String::from_utf8_lossy(&data)),
            Ok(Ok(_)) => {}
            Ok(Err(e)) => panic!("output ended ({e}) before {needle:?} in {text:?}"),
            Err(_) => panic!("no {needle:?} in {text:?}"),
        }
    }
}

#[tokio::test]
async fn install_sh_installs_updates_with_handover_and_uninstalls() {
    let skiffd = Path::new(env!("CARGO_BIN_EXE_skiffd"));
    let skiff = skiffd.with_file_name("skiff");
    if !skiff.exists() {
        eprintln!("skipped: build the skiff CLI first (cargo build -p skiff-cli)");
        return;
    }
    let arch = match std::env::consts::ARCH {
        a @ ("x86_64" | "aarch64") => a,
        _ => return,
    };
    if !cfg!(target_os = "linux") {
        return;
    }
    let dir = std::env::temp_dir().join(format!("skiff-headless-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut s = Server { dir, pids: Vec::new() };
    for sub in ["home", "run", "bin", "dist", "beside"] {
        std::fs::create_dir_all(s.path(sub)).unwrap();
    }
    std::fs::set_permissions(s.path("run"), std::fs::Permissions::from_mode(0o700)).unwrap();
    // No user manager, no linger, and downloads from dist/ only.
    s.tool("systemctl", "exit 1");
    s.tool("loginctl", "exit 1");
    s.tool("curl", &format!("while [ $# -gt 1 ]; do [ \"$1\" = -o ] && out=$2; shift; done\nexec cp \"{}/${{1##*/}}\" \"$out\"", s.path("dist").display()));
    std::fs::copy(skiffd, s.path("beside/skiffd")).unwrap();
    std::fs::copy(&skiff, s.path("beside/skiff")).unwrap();
    // The real skiff-host: install.sh starts it beside skiffd (slice 7).
    let host = std::env::var_os("SKIFF_HOST_BIN").map(PathBuf::from).unwrap_or_else(|| skiffd.with_file_name("skiff-host"));
    assert!(host.is_file(), "build skiff-host first: cargo build -p skiff-host (or set SKIFF_HOST_BIN)");
    std::fs::copy(&host, s.path("beside/skiff-host")).unwrap();
    let mut sums = String::new();
    for v in ["9.9.8-test", "9.9.9-test"] {
        let b = install::dev_bundle(v, arch, &s.path("beside")).await.unwrap();
        std::fs::write(s.path("dist").join(&b.name), &b.bytes).unwrap();
        let out = Command::new("sha256sum").arg(s.path("dist").join(&b.name)).output().unwrap();
        sums.push_str(&format!("{}  {}\n", String::from_utf8_lossy(&out.stdout).split_whitespace().next().unwrap(), b.name));
    }
    std::fs::write(s.path("dist/SHA256SUMS"), sums.trim_end()).unwrap();
    let server = s.path("home/.local/share/skiff/server");
    let socket = s.path("run/skiff/skiffd.sock");

    // Install: the final words name the address to add in the app.
    let (ok, text) = s.pipe(&s.install_sh("9.9.8-test"), &[]);
    s.pids.extend(s.pid());
    let host_pid = || -> Option<i32> { std::fs::read_to_string(server.join("skiff-host.pid")).ok()?.trim().parse().ok() };
    let first_host = host_pid().expect("install.sh started skiff-host");
    s.pids.push(first_host);
    // SAFETY: signal 0 only asks whether the process is there.
    assert_eq!(unsafe { libc::kill(first_host, 0) }, 0, "skiff-host runs");
    assert!(!text.contains("does not stay running"), "{text}");
    assert!(ok, "{text}");
    let me = std::env::var("USER").unwrap_or_default();
    assert!(text.contains(&format!("Skiff 9.9.8-test runs on this server, as {me}.")), "{text}");
    assert!(text.contains("Settings › Hosts › Add SSH Host…") && text.contains(&format!("  {me}@")), "{text}");
    assert!(text.contains("its SHA-256 only: minisign is not installed here"), "{text}");
    assert!(text.contains("It runs until the server restarts."), "no user manager: {text}");
    assert_eq!(std::fs::read_to_string(server.join("installed-by")).unwrap().trim(), "install.sh");
    assert!(server.join("current/install.sh").exists(), "the bundle carries install.sh for --uninstall");
    let link = s.path("home/.local/bin/skiff");
    assert_eq!(std::fs::read_link(&link).unwrap(), server.join("current/skiff"));

    // A session with text on its screen.
    let c = client(&socket).await;
    let daemon = c.daemon_info().await.unwrap().pid;
    let session = c
        .create_session(SessionSpec { command: Some("/bin/sh".into()), cwd: Some(s.path("home")), cols: 60, rows: 8, name: Some("build".into()), ..Default::default() })
        .await
        .unwrap();
    let mut out = c.output(&session.id);
    c.subscribe(&session.id).await.unwrap();
    c.write(&session.id, b"stty -echo; PS1=''; echo before-$((40+2))\r".to_vec()).await.unwrap();
    seen(&mut out, "before-42").await;
    drop(out);
    drop(c);

    // Update by running it again: the reload keeps the session.
    let (ok, text) = s.pipe(&s.install_sh("9.9.9-test"), &[]);
    assert!(ok, "{text}");
    assert!(text.contains("It was 9.9.8-test; its sessions kept running."), "{text}");
    // skiff-host restarts on the new version (slice 6 with slice 7's restart).
    let second_host = host_pid().unwrap();
    s.pids.push(second_host);
    assert_ne!(second_host, first_host, "skiff-host restarted on the update");
    let c = client(&socket).await;
    assert_eq!(c.daemon_info().await.unwrap().pid, daemon, "skiffd kept its pid");
    let after = c.list_sessions().await.unwrap().into_iter().find(|x| x.id == session.id).expect("the session survived");
    assert_eq!(after.pid, session.pid);
    let mut out = c.output(&session.id);
    c.subscribe(&session.id).await.unwrap();
    seen(&mut out, "before-42").await;

    // The same version again changes nothing; an older one never goes in.
    let (ok, text) = s.pipe(&s.install_sh("9.9.9-test"), &[]);
    assert!(ok && text.contains("Skiff 9.9.9-test is installed already. Nothing changed."), "{text}");
    let (ok, text) = s.pipe(&s.install_sh("9.9.8-test"), &[]);
    assert!(!ok && text.contains("newer than Skiff 9.9.8-test. Skiff never moves down."), "{text}");
    let current = std::fs::read_link(server.join("current")).unwrap();
    assert!(current.to_string_lossy().starts_with("9.9.9-test+dev-"), "{current:?}");

    // A bundle that fails the signature check never goes in.
    s.tool("minisign", "exit 1");
    let (ok, text) = s.pipe(&s.install_sh("9.9.9-test"), &[]);
    assert!(!ok && text.contains("is not signed by Skiff. Nothing changed."), "{text}");
    std::fs::remove_file(s.path("bin/minisign")).unwrap();

    // Uninstall with a live session and no terminal: it names the session
    // and stops. With --yes it removes the install and stops skiffd.
    let script = std::fs::read_to_string(server.join("current/install.sh")).unwrap();
    let (ok, text) = s.pipe(&script, &["--uninstall"]);
    assert!(!ok && text.contains("Uninstall ends these sessions") && text.contains("build"), "{text}");
    assert!(server.join("current").exists());
    let pid = s.pid().unwrap();
    let (ok, text) = s.pipe(&script, &["--uninstall", "--yes"]);
    assert!(ok && text.contains("Skiff is uninstalled from this server."), "{text}");
    assert!(!server.exists() && std::fs::symlink_metadata(&link).is_err(), "the managed folder and our link are gone");
    let until = Instant::now() + Duration::from_secs(5);
    // SAFETY: signal 0 only asks whether the process is there.
    while unsafe { libc::kill(pid, 0) } == 0 {
        assert!(Instant::now() < until, "skiffd {pid} still runs");
        std::thread::sleep(Duration::from_millis(50));
    }
    // The same uninstall as the app's: skiff-host stops too.
    // SAFETY: signal 0 only asks whether the process is there.
    while unsafe { libc::kill(second_host, 0) } == 0 {
        assert!(Instant::now() < until, "skiff-host {second_host} still runs");
        std::thread::sleep(Duration::from_millis(50));
    }
}
