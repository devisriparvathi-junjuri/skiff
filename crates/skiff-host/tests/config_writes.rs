//! skiff-host as the only writer of the Host's `[[project]]` tables and
//! project worktrees: on a private socket and private state (never the live
//! socket, config or Host id), reached as the app reaches it, over this
//! Mac's socket and over a fake `ssh` forward, with a stand-in skiffd for
//! the Host's sessions.
//!
//! One test function: the state follows the first socket set in a process,
//! and the guard removes every fixture, also when an assertion fails.
#![cfg(unix)]

use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use serde_json::{json, Value};
use skiff_client::host::{
    data,
    local::HostSelection,
    ssh::{SshOptions, SshTarget},
    HostConnection,
};
use skiff_host_api::{Capability, Error, Hello, Message, ProjectEdit, ProjectFleet, Request, Response};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::UnixListener,
    task::JoinHandle,
};

mod common;
use common::{fake_ssh, git, repo, start, Guard};

/// The sessions the stand-in skiffd lists. `None`: it answers nothing.
type Sessions = Arc<Mutex<Option<Vec<Value>>>>;

/// A stand-in skiffd at `socket`: one reply per `list_sessions`, as skiffd
/// writes it; closes the connection when `sessions` is `None`.
fn skiffd(socket: &Path, sessions: Sessions) -> JoinHandle<()> {
    let listener = UnixListener::bind(socket).unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else { return };
            let sessions = sessions.clone();
            tokio::spawn(async move {
                let (rd, mut wr) = stream.into_split();
                let mut lines = BufReader::new(rd).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let ask: Value = serde_json::from_str(&line).unwrap();
                    assert_eq!(ask["cmd"], "list_sessions", "skiff-host asks skiffd nothing else");
                    let Some(list) = sessions.lock().unwrap().clone() else { return };
                    let reply = json!({"id": ask["id"], "type": "sessions", "sessions": list});
                    let _ = wr.write_all(format!("{reply}\n").as_bytes()).await;
                }
            });
        }
    })
}

fn session(cwd: &Path, state: &str) -> Value {
    json!({"id": "s1", "label": "claude · fix", "role": "agent", "cwd": cwd, "command": "claude", "args": [],
           "state": state, "cols": 80, "rows": 24, "pid": null, "exit_code": null})
}

fn conflict<T: std::fmt::Debug>(r: Result<T, Error>) -> bool {
    matches!(r, Err(Error::Conflict { .. }))
}

#[tokio::test(flavor = "multi_thread")]
async fn skiff_host_is_the_writer_of_the_hosts_projects_and_worktrees() {
    let dir = PathBuf::from("/tmp").join(format!("shw-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let _guard = Guard(dir.clone());
    let socket = dir.join("skiffd.sock");
    skiff_core::state::set_socket(socket.clone());
    assert!(skiff_core::state::private_dir().is_some(), "never the live state");

    let app = dir.join("app");
    repo(&app);
    let other = dir.join("other");
    repo(&other);
    let third = dir.join("third");
    repo(&third);
    let config = skiff_core::config::config_path();
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    // A client table and a comment: the Host's writes keep both as they are.
    let text = format!(
        "[appearance]\ntheme = \"harbor\"\n\n# the main app\n[[project]]\nname = \"app\"\npath = \"{}\"\ncolor = \"#123456\"\n\n\
         [[project]]\nname = \"other\"\npath = \"{}\"\n",
        app.display(),
        other.display()
    );
    std::fs::write(&config, &text).unwrap();
    let file = || std::fs::read_to_string(&config).unwrap();

    let sessions: Sessions = Arc::new(Mutex::new(Some(Vec::new())));
    let _skiffd = skiffd(&socket, sessions.clone());
    let hello = skiff_host::hello().unwrap();
    for c in [Capability::project_config(), Capability::worktrees()] {
        assert!(hello.capabilities.contains(&c), "{c:?}");
    }
    let host = hello.host.host_id.clone();
    let data_socket = skiff_core::socket::data_socket(&socket);
    let service = start(&data_socket, hello.clone());

    // Two routes to one Host: this Mac's socket, and an SSH forward to it.
    let local = HostSelection::new(socket.clone(), dir.join("bootstrap-id")).connect_data(&host).await.unwrap();
    let l = local.connection();
    let remote = match fake_ssh(&dir) {
        Some(ssh) => Some(
            HostSelection::ssh(
                SshTarget::new("devbox", socket.to_string_lossy().into_owned()),
                SshOptions { program: ssh, config: None },
                dir.join("fwd"),
                Some(host.clone()),
            )
            .unwrap()
            .connect_data(&host)
            .await
            .unwrap(),
        ),
        None => None,
    };
    let r = remote.as_ref().map(|p| p.connection()).unwrap_or_else(|| l.clone());

    // Parity: both routes read the same rows and the same revision.
    let (rows, rev) = data::project_rows(&l).await.unwrap();
    let rev = rev.expect("a Host with project_config sends its revision");
    assert_eq!(data::project_rows(&r).await.unwrap(), (rows.clone(), Some(rev.clone())));
    assert_eq!(rev, skiff_core::config::projects_revision(&skiff_core::config::load().unwrap().projects));

    // A change made from that revision: written, and a new revision.
    let rev2 = data::project_edit(&l, &rev, "app", ProjectEdit::Color { color: "#abcdef".into() }).await.unwrap();
    assert_ne!(rev2, rev);
    assert!(file().contains("color = \"#abcdef\"") && file().contains("# the main app") && file().contains("theme = \"harbor\""));
    // The same change again, as a replay would send it: refused, not applied twice.
    let after = file();
    assert!(conflict(data::project_edit(&l, &rev, "app", ProjectEdit::Color { color: "#abcdef".into() }).await));
    // Another writer's change from the old read: refused, the file as it was.
    assert!(conflict(data::project_edit(&r, &rev, "app", ProjectEdit::Fleet { fleet: "F".into() }).await));
    assert_eq!(file(), after);
    // From the new revision, over SSH: written, and both routes read it.
    let rev3 = data::project_edit(&r, &rev2, "app", ProjectEdit::Fleet { fleet: "F".into() }).await.unwrap();
    assert_eq!(data::project_rows(&l).await.unwrap().1.as_deref(), Some(rev3.as_str()));
    assert_eq!(data::project_get(&l, "app").await.unwrap().fleet.as_deref(), Some("F"));
    assert_eq!(data::project_rows(&l).await.unwrap(), data::project_rows(&r).await.unwrap());

    // Validation: typed refusals, nothing written.
    let before = file();
    for (edit, project) in [
        (ProjectEdit::Color { color: "red".into() }, "app"),
        (ProjectEdit::Short { short: "toolong".into() }, "app"),
        (ProjectEdit::Fleet { fleet: "two\nlines".into() }, "app"),
        (ProjectEdit::Theme { theme: Some("x".repeat(65)) }, "app"),
    ] {
        let got = data::project_edit(&r, &rev3, project, edit.clone()).await;
        assert!(matches!(got, Err(Error::Invalid { .. })), "{edit:?}: {got:?}");
    }
    let got = data::project_edit(&r, &rev3, "nope", ProjectEdit::Files { on: true }).await;
    assert!(matches!(got, Err(Error::NotFound { .. })), "{got:?}");
    let got = data::project_arrange(&r, &rev3, vec!["app".into(), "ghost".into()], vec![]).await;
    assert!(matches!(got, Err(Error::NotFound { .. })), "{got:?}");
    for path in ["relative/dir", "", "/a\nb"] {
        let got = data::project_inspect(&r, path).await;
        assert!(matches!(got, Err(Error::Invalid { .. })), "{path:?}: {got:?}");
    }
    assert_eq!(file(), before);

    // Concurrent writers from one read: exactly one is written.
    let mut writers = Vec::new();
    for i in 0..8u8 {
        let c = if i % 2 == 0 { l.clone() } else { r.clone() };
        let rev = rev3.clone();
        writers.push(tokio::spawn(async move {
            data::project_edit(&c, &rev, "other", ProjectEdit::Short { short: format!("o{i}") }).await
        }));
    }
    let mut won = Vec::new();
    for (i, w) in writers.into_iter().enumerate() {
        match w.await.unwrap() {
            Ok(rev) => won.push((i, rev)),
            Err(Error::Conflict { .. }) => {}
            Err(e) => panic!("writer {i}: {e:?}"),
        }
    }
    assert_eq!(won.len(), 1, "{won:?}");
    let (winner, rev4) = won.pop().unwrap();
    assert_eq!(data::project_get(&l, "other").await.unwrap().short, format!("o{winner}"));
    assert_eq!(file().matches("short = ").count(), 1);

    // Add, arrange and remove, over SSH. The inspect sends bytes, not paths.
    let folder = data::project_inspect(&r, third.to_str().unwrap()).await.unwrap();
    let real = third.canonicalize().unwrap();
    assert_eq!((folder.root.as_deref(), folder.error.as_deref()), (real.to_str(), None));
    let (added, rev5) = data::project_add(&r, &rev4, third.to_str().unwrap(), Some("third".into()), None, None).await.unwrap();
    assert_eq!((added.name.as_str(), added.closed), ("third", false));
    assert!(added.theme.is_some(), "a new project gets a theme of its own");
    // The same folder or name again: a conflict, not a second table.
    assert!(conflict(data::project_add(&r, &rev5, third.to_str().unwrap(), None, None, None).await));
    let rev5 = data::project_rows(&l).await.unwrap().1.unwrap();
    let rev6 = data::project_arrange(
        &l,
        &rev5,
        vec!["third".into(), "app".into()],
        vec![ProjectFleet { project: "third".into(), fleet: "F".into() }],
    )
    .await
    .unwrap();
    let order: Vec<_> = data::project_list(&r).await.unwrap().into_iter().map(|p| (p.name, p.fleet)).collect();
    assert_eq!(order, [("third".into(), Some("F".into())), ("app".into(), Some("F".into())), ("other".into(), None)]);
    let rev7 = data::project_edit(&r, &rev6, "third", ProjectEdit::Closed { closed: true }).await.unwrap();
    assert!(data::project_get(&l, "third").await.unwrap().closed);
    data::project_remove(&r, &rev7, "third").await.unwrap();
    assert!(matches!(data::project_get(&l, "third").await, Err(Error::NotFound { .. })));
    assert!(third.join(".git").is_dir(), "the folder stays");
    assert!(conflict(data::project_remove(&r, &rev7, "other").await), "a stale remove is refused");

    // Worktrees: made by the Host's git beside the project, never elsewhere.
    for bad in ["../escape", "-x", "a b", "", "x..y", "x.lock"] {
        let got = data::worktree_add(&r, "app", bad, None, false).await;
        assert!(matches!(got, Err(Error::Invalid { .. })), "{bad:?}: {got:?}");
    }
    assert!(!dir.join("escape").exists() && !dir.join("app-worktrees").exists());
    let w = data::worktree_add(&r, "app", "feat/x", None, false).await.unwrap();
    assert_eq!(Path::new(&w.path), dir.join("app-worktrees/feat-x").canonicalize().unwrap());
    assert!(data::worktree_add(&r, "app", "feat/x", None, false).await.is_err(), "git refuses the same branch twice");
    assert!(matches!(data::worktree_add(&r, "nope", "y", None, false).await, Err(Error::NotFound { .. })));

    // Removal: the Host's sessions and changes decide, not the client.
    let wt = PathBuf::from(&w.path);
    *sessions.lock().unwrap() = Some(vec![session(&wt.join("sub"), "working")]);
    let got = data::worktree_remove(&r, "app", &w.path, true).await;
    assert!(matches!(&got, Err(Error::Conflict { message }) if message.contains("still running")), "{got:?}");
    assert!(wt.is_dir());
    // skiffd out of reach: nothing is removed, even with force.
    *sessions.lock().unwrap() = None;
    let got = data::worktree_remove(&r, "app", &w.path, true).await;
    assert!(matches!(&got, Err(Error::OperationFailed { message }) if message.contains("sessions")), "{got:?}");
    assert!(wt.is_dir());
    // A finished session does not hold it; uncommitted changes do, unless forced.
    *sessions.lock().unwrap() = Some(vec![session(&wt, "done")]);
    std::fs::write(wt.join("a.txt"), "changed\n").unwrap();
    let got = data::worktree_remove(&l, "app", &w.path, false).await;
    assert!(matches!(&got, Err(Error::Conflict { message }) if message.contains("uncommitted")), "{got:?}");
    assert!(wt.is_dir());
    let main = app.to_str().unwrap();
    assert!(matches!(data::worktree_remove(&l, "app", main, true).await, Err(Error::Invalid { .. })));
    let got = data::worktree_remove(&l, "app", other.to_str().unwrap(), true).await;
    assert!(matches!(got, Err(Error::NotFound { .. })), "{got:?}");
    data::worktree_remove(&l, "app", &w.path, true).await.unwrap();
    assert!(!wt.exists());

    // Branches: the existing ones to check out, and a base for a new one.
    git(&app, &["branch", "old"]);
    git(&app, &["remote", "add", "origin", "/nowhere"]);
    git(&app, &["update-ref", "refs/remotes/origin/far", "HEAD"]);
    let b = data::branch_list(&r, "app").await.unwrap();
    assert_eq!(b.head.as_deref(), Some("main"));
    let seen: Vec<_> = b.branches.iter().map(|b| (b.name.as_str(), b.remote, b.worktree)).collect();
    for want in [("main", false, true), ("feat/x", false, false), ("old", false, false), ("origin/far", true, false)] {
        assert!(seen.contains(&want), "{want:?} in {seen:?}");
    }
    for (bad, base) in [("main", None), ("nosuch", None), ("old", Some("main"))] {
        let got = data::worktree_add(&r, "app", bad, base, true).await;
        assert!(matches!(got, Err(Error::Invalid { .. })), "{bad:?}: {got:?}");
    }
    let old = data::worktree_add(&r, "app", "old", None, true).await.unwrap();
    assert_eq!((old.branch.as_deref(), Path::new(&old.path)), (Some("old"), dir.join("app-worktrees/old").canonicalize().unwrap().as_path()));
    let far = data::worktree_add(&r, "app", "far", None, true).await.unwrap();
    assert_eq!(far.branch.as_deref(), Some("far"), "a remote's branch becomes a local one");
    let from = data::worktree_add(&r, "app", "feat/y", Some("old"), false).await.unwrap();
    assert_eq!(from.branch.as_deref(), Some("feat/y"));
    assert!(data::branch_list(&r, "app").await.unwrap().branches.iter().any(|b| b.name == "old" && b.worktree));

    // The client table and the comment stayed; the Host made no client file.
    assert!(file().contains("[appearance]\ntheme = \"harbor\"") && file().contains("# the main app"));
    assert!(!skiff_core::config::client::path().exists());

    // A Host without these capabilities: refused before anything is sent,
    // and refused by the service itself.
    let mut reads_only = hello.clone();
    reads_only.capabilities.retain(|c| *c != Capability::project_config() && *c != Capability::worktrees());
    let edit = Request::ProjectEdit { project: "app".into(), revision: rev3.clone(), edit: ProjectEdit::Files { on: true } };
    for request in [edit.clone(), Request::WorktreeAdd { project: "app".into(), branch: "z".into(), base: None, checkout: false }] {
        assert!(matches!(skiff_host::answer(request, &reads_only).await, Err(Error::UnsupportedCapability { .. })));
    }
    let before = file();
    drop((local, remote));
    service.abort();
    let _ = service.await;
    let limited = start(&data_socket, reads_only);
    let plane = HostSelection::new(socket.clone(), dir.join("bootstrap-id")).connect_data(&host).await.unwrap();
    assert!(matches!(plane.connection().request(edit).await, Err(Error::UnsupportedCapability { .. })));
    assert!(data::project_list(&plane.connection()).await.is_ok(), "reads still work");
    assert_eq!(file(), before);
    drop(plane);
    limited.abort();

    no_replay(&dir, &hello).await;
}

/// A change whose reply is lost is reported, and never sent again: the
/// connection sends it once, and nothing reconnects to repeat it.
async fn no_replay(dir: &Path, hello: &Hello) {
    let path = dir.join("drop.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let (connections, edits) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let (c2, e2, hello2) = (connections.clone(), edits.clone(), hello.clone());
    let stub = tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else { return };
            c2.fetch_add(1, Ordering::SeqCst);
            let (rd, mut wr) = stream.into_split();
            let mut lines = BufReader::new(rd).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let ask: Value = serde_json::from_str(&line).unwrap();
                if ask["request"]["cmd"] == "hello" {
                    let id = ask["id"].as_u64().unwrap();
                    let reply = Message::Reply { id, result: Ok(Response::Hello { hello: hello2.clone() }) };
                    wr.write_all(format!("{}\n", serde_json::to_string(&reply).unwrap()).as_bytes()).await.unwrap();
                } else {
                    // Applied, as far as the client can tell, then the link drops.
                    e2.fetch_add(1, Ordering::SeqCst);
                    break;
                }
            }
        }
    });
    let stream = tokio::net::UnixStream::connect(&path).await.unwrap();
    let c = HostConnection::connect_with_timeout(stream, None, Duration::from_secs(5)).await.unwrap();
    let got = data::project_edit(&c, "r", "app", ProjectEdit::Files { on: true }).await;
    assert!(matches!(got, Err(Error::TransportLost)), "{got:?}");
    let got = data::worktree_add(&c, "app", "lost", None, false).await;
    assert!(matches!(got, Err(Error::TransportLost)), "a lost connection sends nothing more: {got:?}");
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!((connections.load(Ordering::SeqCst), edits.load(Ordering::SeqCst)), (1, 1));
    stub.abort();
}
