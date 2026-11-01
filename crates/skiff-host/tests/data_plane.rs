//! The data-plane service in this process, on a private socket and private
//! state (never the live socket, config or Host id), reached the way the app
//! reaches it: `HostSelection::connect_data`, over this Mac's socket or a
//! fake `ssh` that forwards to a "remote" socket on this machine.
//!
//! One test function: the state follows the first socket set in a process,
//! and the guard removes every fixture, also when an assertion fails.
#![cfg(unix)]

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use skiff_client::host::{
    data::{self, DataError},
    local::HostSelection,
    ssh::{SshOptions, SshTarget},
    Continuity,
};
use skiff_host_api::{
    Capability, ChangeStatus, Error, Hello, HostId, InstanceId, Request, Response,
};

mod common;
use common::{fake_ssh, git, repo, start, Guard};

#[tokio::test(flavor = "multi_thread")]
async fn the_data_plane_serves_this_hosts_projects_and_git_only() {
    // Short: forward sockets must fit in sun_path.
    let dir = PathBuf::from("/tmp").join(format!("sh-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let _guard = Guard(dir.clone());
    let session = dir.join("skiffd.sock");
    // Private state, as skiffd and skiff-host take it from their socket.
    skiff_core::state::set_socket(session.clone());
    assert!(skiff_core::state::private_dir().is_some(), "never the live state");

    let app = dir.join("app");
    repo(&app);
    git(&app, &["worktree", "add", "-q", "-b", "feat", dir.join("app-feat").to_str().unwrap()]);
    std::fs::write(dir.join("app-feat/a.txt"), "one\ntwo\n").unwrap();
    std::fs::write(dir.join("app-feat/new.txt"), "x\ny\n").unwrap();
    let outside = dir.join("outside");
    repo(&outside);
    std::fs::write(outside.join("a.txt"), "changed\n").unwrap();
    let config = skiff_core::config::config_path();
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    std::fs::write(
        &config,
        format!(
            "[[project]]\nname = \"app\"\npath = \"{}\"\nfleet = \"F\"\ncolor = \"#123456\"\n\n\
             [[project]]\nname = \"shut\"\npath = \"{}\"\nclosed = true\n\n\
             [[project]]\nname = \"gone\"\npath = \"{}\"\n",
            app.display(),
            outside.display(),
            dir.join("missing").display()
        ),
    )
    .unwrap();

    // Identity: the store beside the private config, shared with skiffd.
    let hello = skiff_host::hello().unwrap();
    let stored = skiff_core::host_identity::load(&skiff_core::host_identity::path()).unwrap();
    assert_eq!(hello.host.host_id.as_str(), stored);
    assert_eq!(hello.capabilities, skiff_host::capabilities());
    for c in [Capability::projects(), Capability::git_status(), Capability::git_diff()] {
        assert!(hello.capabilities.contains(&c), "{c:?}");
    }
    let host = hello.host.host_id.clone();
    let mut service = start(&skiff_core::socket::data_socket(&session), hello.clone());

    // This Mac's endpoint, as the app selects it.
    let selection = HostSelection::new(session.clone(), dir.join("bootstrap-id"));
    let plane = selection.connect_data(&host).await.unwrap();
    let c = plane.connection();
    assert_eq!(c.continuity(), Continuity::FirstConnection);

    // Parity: the very rows the app read itself before, field for field.
    let listed = data::project_list(&c).await.unwrap();
    let before = skiff_core::project::list().unwrap();
    assert_eq!(serde_json::to_value(&listed).unwrap(), serde_json::to_value(&before).unwrap());
    let names: Vec<_> = listed.iter().map(|p| (p.name.as_str(), p.closed, p.fleet.as_deref())).collect();
    assert_eq!(names, [("app", false, Some("F")), ("shut", true, None), ("gone", false, None)]);
    assert_eq!(listed[0].worktrees.len(), 2);
    assert!(listed[2].error.as_deref().is_some_and(|e| e.contains("does not exist")));
    let one = data::project_get(&c, "app").await.unwrap();
    assert_eq!(one, listed[0]);
    assert!(matches!(data::project_get(&c, "nope").await, Err(Error::NotFound { .. })));

    // Git status, the same as the app's own read, for a known worktree.
    let feat = listed[0].worktrees.iter().find(|w| w.branch.as_deref() == Some("feat")).unwrap().path.clone();
    let changes = data::git_changes(&c, &feat).await.unwrap();
    let local = skiff_core::git::changes(Path::new(&feat)).unwrap();
    assert_eq!(serde_json::to_value(&changes.changes).unwrap(), serde_json::to_value(&local.files).unwrap());
    assert_eq!(changes.base, local.base);
    assert_eq!(changes.changes.iter().map(|c| (c.path.as_str(), c.status, c.committed)).collect::<Vec<_>>(), [("a.txt", ChangeStatus::M, false), ("new.txt", ChangeStatus::A, false)]);
    // Review M4 P2: an allowed worktree's untracked link to a FIFO outside
    // it once blocked the request and read the outside data. Now the link
    // counts as git counts it, one line, and the FIFO is never opened.
    let fifo = dir.join("outside-fifo");
    let c_fifo = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
    // SAFETY: a NUL-terminated path.
    assert_eq!(unsafe { libc::mkfifo(c_fifo.as_ptr(), 0o600) }, 0);
    std::os::unix::fs::symlink(&fifo, app.join("link-fifo")).unwrap();
    let main = app.to_str().unwrap();
    let read = tokio::time::timeout(Duration::from_secs(10), data::git_changes(&c, main)).await;
    let Ok(read) = read else {
        // Unblock the stuck reader so the run can end, then fail.
        let _ = std::thread::spawn(move || std::fs::write(&fifo, "1\n2\n"));
        panic!("git_changes blocked on a FIFO outside the worktree");
    };
    let link = read.unwrap().changes.into_iter().find(|c| c.path == "link-fifo").unwrap();
    assert_eq!((link.status, link.added, link.removed), (ChangeStatus::A, Some(1), Some(0)));
    std::fs::remove_file(app.join("link-fifo")).unwrap();

    // Diff text: the same as the Host's own read, plain and bounded.
    let one = data::git_diff(&c, &feat, Some("a.txt")).await.unwrap();
    let held = skiff_core::git::Root::open(Path::new(&feat)).unwrap();
    let local = skiff_core::git::diff_in(&held, Path::new(&feat), Some("a.txt")).unwrap();
    assert_eq!(serde_json::to_value(&one).unwrap(), serde_json::to_value(&local).unwrap());
    assert!(one.text.contains("+two") && !one.untracked && !one.truncated && !one.binary, "{one:?}");
    let new = data::git_diff(&c, &feat, Some("new.txt")).await.unwrap();
    assert!(new.untracked && new.text.contains("+x\n+y\n"), "{new:?}");
    let all = data::git_diff(&c, &feat, None).await.unwrap();
    assert!(all.text.contains("a/a.txt") && !all.text.contains("new.txt"), "{all:?}");
    // Over the limit: cut at a line end, said so, and still one frame.
    let big = "0123456789abcdef\n".repeat(skiff_core::git::DIFF_LIMIT / 8);
    std::fs::write(dir.join("app-feat/big.txt"), &big).unwrap();
    let cut = data::git_diff(&c, &feat, Some("big.txt")).await.unwrap();
    assert!(cut.truncated && cut.text.len() <= skiff_core::git::DIFF_LIMIT && cut.text.ends_with('\n'));
    std::fs::remove_file(dir.join("app-feat/big.txt")).unwrap();
    // Only a file below a known worktree: never another path or project.
    for (wt, file) in [(feat.as_str(), "../outside/a.txt"), (feat.as_str(), "/etc/hosts"), (outside.to_str().unwrap(), "a.txt"), ("/", "etc/hosts")] {
        let got = data::git_diff(&c, wt, Some(file)).await;
        assert!(matches!(got, Err(Error::NotFound { .. })), "{wt} {file}: {got:?}");
    }

    // Git runs in no folder the client names unless it is a worktree of an
    // open project: not a closed project's, not any other repo or path.
    for path in [outside.to_str().unwrap(), "/", dir.join("missing").to_str().unwrap(), "relative"] {
        assert!(matches!(data::git_changes(&c, path).await, Err(Error::NotFound { .. })), "{path}");
    }
    // The service offers nothing else: session families are skiffd's.
    assert!(matches!(
        c.request(Request::AgentList).await,
        Err(Error::UnsupportedCapability { .. })
    ));
    assert!(matches!(
        skiff_host::answer(Request::SessionInventory, &hello).await,
        Err(Error::UnsupportedCapability { .. })
    ));

    // Another Host's service at this socket is refused before any request.
    let other = HostId::try_from("not-this-host".to_string()).unwrap();
    let refused = selection.connect_data(&other).await.err().unwrap();
    assert!(matches!(refused.downcast_ref(), Some(Error::IdentityChanged { .. })), "{refused:#}");

    // A restart: the old connection is lost and nothing is replayed; the
    // next one is the same Host, a new instance.
    service.abort();
    let _ = service.await;
    drop(plane);
    let lost = c.request(Request::ProjectList).await;
    assert!(matches!(lost, Err(Error::TransportLost)), "{lost:?}");
    assert!(matches!(
        selection.connect_data(&host).await.err().and_then(|e| e.downcast::<DataError>().ok()),
        Some(DataError::NotRunning)
    ));
    let mut restarted = hello.clone();
    restarted.instance_id = InstanceId::try_from("second-run".to_string()).unwrap();
    service = start(&skiff_core::socket::data_socket(&session), restarted);
    let again = selection.connect_data(&host).await.unwrap();
    assert_eq!(again.connection().continuity(), Continuity::NewInstance);
    assert_eq!(again.connection().hello().host.host_id, host);
    assert_eq!(data::project_list(&again.connection()).await.unwrap(), listed);

    // An oversize or unreadable frame ends that connection only.
    raw_frames(&skiff_core::socket::data_socket(&session)).await;
    assert!(data::project_list(&again.connection()).await.is_ok());
    drop(again);
    service.abort();

    remote(&dir, &host, &hello).await;
}

/// A broken client gets its connection closed, without a reply.
async fn raw_frames(socket: &Path) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut s = tokio::net::UnixStream::connect(socket).await.unwrap();
    s.write_all(b"not json\n").await.unwrap();
    let mut buf = Vec::new();
    let n = tokio::time::timeout(Duration::from_secs(5), s.read_to_end(&mut buf)).await.unwrap().unwrap();
    assert_eq!(n, 0);
}

/// An SSH Host: the forward goes to the socket beside that Host's skiffd,
/// named by path only; a Host without the service is "not running", and
/// nothing is started for it.
async fn remote(dir: &Path, host: &HostId, hello: &Hello) {
    // It logs each remote path to dir/forwards.
    let Some(ssh) = fake_ssh(dir) else { return };
    let far = dir.join("far");
    std::fs::create_dir_all(&far).unwrap();
    let remote_session = far.join("skiffd.sock");
    let forwards = dir.join("fwd");
    let selection = HostSelection::ssh(
        SshTarget::new("devbox", remote_session.to_string_lossy().into_owned()),
        SshOptions { program: ssh.clone(), config: None },
        forwards.clone(),
        Some(host.clone()),
    )
    .unwrap();

    // No service on that Host: not running, and a wait before the next try.
    let e = selection.connect_data(host).await.err().unwrap();
    assert!(matches!(e.downcast_ref(), Some(DataError::NotRunning)), "{e:#}");
    let e = selection.connect_data(host).await.err().unwrap();
    assert!(matches!(e.downcast_ref(), Some(DataError::Waiting { .. })), "{e:#}");
    assert!(!far.join("skiffd-host.sock").exists(), "nothing was started on the Host");

    // With the service there: its own forward to the socket beside skiffd's.
    let remote_data = skiff_core::socket::data_socket(&remote_session);
    let service = start(&remote_data, hello.clone());
    let selection = HostSelection::ssh(
        SshTarget::new("devbox", remote_session.to_string_lossy().into_owned()),
        SshOptions { program: ssh, config: None },
        forwards,
        Some(host.clone()),
    )
    .unwrap();
    let plane = selection.connect_data(host).await.unwrap();
    let rows = data::project_list(&plane.connection()).await.unwrap();
    assert_eq!(rows.len(), 3);
    let asked = std::fs::read_to_string(dir.join("forwards")).unwrap();
    assert!(asked.lines().all(|l| l == remote_data.to_str().unwrap()), "{asked}");
    // A remote path is sent as typed and answered by that Host.
    let feat = rows[0].worktrees[1].path.clone();
    assert!(data::git_changes(&plane.connection(), &feat).await.is_ok());
    drop(plane);
    service.abort();
}

/// The wire stays the envelope: one reply kind per request.
#[test]
fn every_data_request_has_one_reply_kind() {
    let list = Response::Projects { projects: vec![], revision: None };
    let changes = Response::Changes { changes: vec![], base: None };
    assert!(Request::ProjectList.accepts(&list));
    assert!(!Request::ProjectList.accepts(&Response::Ok));
    assert!(!Request::ProjectGet { name: "x".into() }.accepts(&list));
    assert!(Request::GitChanges { worktree: "/x".into() }.accepts(&changes));
    assert!(!Request::GitChanges { worktree: "/x".into() }.accepts(&Response::Ok));
}
