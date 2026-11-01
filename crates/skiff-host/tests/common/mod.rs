//! What the skiff-host tests share: a private folder that goes with the
//! test, a git repository, the service on a socket, a fake `ssh` forward, a
//! private home and a ledger. Each test binary takes what it needs.
#![allow(dead_code)]

use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
};

use skiff_host_api::Hello;
use tokio::{net::UnixListener, task::JoinHandle};

/// Removes the test's private folder when the test ends.
pub struct Guard(pub PathBuf);
impl Drop for Guard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false", "-c", "init.defaultBranch=main", "-C"])
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

/// A repository at `dir` with one commit of `a.txt`.
pub fn repo(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    git(dir, &["init", "-q"]);
    std::fs::write(dir.join("a.txt"), "one\n").unwrap();
    git(dir, &["add", "."]);
    git(dir, &["commit", "-qm", "init"]);
}

/// The service at `socket` with `hello`, until the handle is aborted.
pub fn start(socket: &Path, hello: Hello) -> JoinHandle<()> {
    let _ = std::fs::remove_file(socket);
    let listener = UnixListener::bind(socket).unwrap();
    tokio::spawn(async move {
        let _ = skiff_host::serve(listener, Arc::new(hello)).await;
    })
}

/// A fake ssh in `dir`: binds the -L path and pipes each connection to the
/// remote path, which here is a socket on this machine. Each remote path it
/// is asked for goes to `dir/forwards`. `None` without /usr/bin/python3.
pub fn fake_ssh(dir: &Path) -> Option<PathBuf> {
    if !Path::new("/usr/bin/python3").is_file() {
        eprintln!("skipped the SSH route: no /usr/bin/python3");
        return None;
    }
    let script = r#"#!/usr/bin/python3
import socket,sys,threading
D=sys.argv[0].rsplit('/',1)[0]
spec=sys.argv[sys.argv.index('-L')+1]
local,remote=spec.split(':',1)
open(D+'/forwards','a').write(remote+'\n')
s=socket.socket(socket.AF_UNIX); s.bind(local); s.listen()
def pipe(a,b):
 try:
  while True:
   d=a.recv(65536)
   if not d: break
   b.sendall(d)
 except OSError: pass
 for x in (a,b):
  try: x.shutdown(socket.SHUT_RDWR)
  except OSError: pass
while True:
 c,_=s.accept()
 r=socket.socket(socket.AF_UNIX)
 try: r.connect(remote)
 except OSError:
  c.close(); continue
 threading.Thread(target=pipe,args=(c,r),daemon=True).start()
 threading.Thread(target=pipe,args=(r,c),daemon=True).start()
"#;
    let ssh = dir.join("fake-ssh");
    std::fs::write(&ssh, script).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o755)).unwrap();
    Some(ssh)
}

/// A private home for this process, set before any thread runs, with the
/// Skiff overrides cleared. The live folder guard
/// ([`skiff_client::skipper::refuse_live_folder`]) compares against the
/// person's projects.toml and `~/skiff-agent`; with this home both are the
/// fixture's own, so no test reads the person's config.
pub fn private_home(dir: &Path) -> PathBuf {
    let home = dir.join("home");
    std::fs::create_dir_all(&home).unwrap();
    std::env::set_var("HOME", &home);
    for k in ["XDG_CONFIG_HOME", "SKIFF_CONFIG", "SKIFF_STATE", "SKIFF_LEDGER", "SKIFF_REAL_STATE", "SKIFF_SESSION"] {
        std::env::remove_var(k);
    }
    assert!(skiff_core::state::real_config_path().starts_with(&home), "the person's config");
    assert!(skiff_core::state::real_agent_folders().iter().all(|f| f.starts_with(&home)), "the person's agent folder");
    home
}

/// The ledger's table as `skiff ledger` makes it.
pub const SCHEMA: &str = "CREATE TABLE tasks (id INTEGER PRIMARY KEY, uid TEXT UNIQUE, kind TEXT, parent TEXT, pane_id TEXT, pane_gen INTEGER,
    project TEXT, task TEXT, sent_at TEXT, state TEXT, result_path TEXT, ref TEXT, updated_at TEXT, type TEXT, socket TEXT,
    reported_at TEXT, title TEXT, closed_at TEXT, gone_at TEXT, owner TEXT NOT NULL DEFAULT 'grand', reopened_at TEXT); PRAGMA user_version = 10;";

/// One row, as `skiff ledger add` leaves it.
#[allow(clippy::too_many_arguments)]
pub fn add(db: &rusqlite::Connection, uid: &str, kind: &str, parent: Option<&str>, state: &str, result: Option<&str>, socket: Option<&str>, closed: Option<&str>) {
    db.execute(
        "INSERT INTO tasks (uid, kind, parent, pane_id, pane_gen, project, task, sent_at, state, result_path, updated_at, socket, closed_at)
         VALUES (?1, ?2, ?3, ?4, 1, 'web', 'Task ' || ?1, '2026-10-21 10:00:00', ?5, ?6, '2026-10-21 10:00:00', ?7, ?8)",
        rusqlite::params![uid, kind, parent, (kind == "task").then(|| format!("pane-{uid}")), state, result, socket, closed],
    )
    .unwrap();
}
