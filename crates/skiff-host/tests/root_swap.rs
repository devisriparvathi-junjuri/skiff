//! Review M4 P2: a known worktree's folder replaced by a link to another
//! folder while git runs. The count once reopened the path and read the
//! other folder's file. A controlled `git` makes the swap at the diff, the
//! gap between the worktree check and the count, as another process could.
//! The diff text of an untracked file is held to the same root.
//!
//! Its own process: `git` here is the fake one on PATH, and the state is
//! private (never the live socket, config or Host id).
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;

use skiff_host_api::{ChangeStatus, Error, Request, Response};

mod common;
use common::{Guard};

#[tokio::test(flavor = "multi_thread")]
async fn a_worktree_replaced_while_git_runs_is_refused_not_read() {
    let dir = std::env::temp_dir().join(format!("skiff-root-swap-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let dir = dir.canonicalize().unwrap();
    let _guard = Guard(dir.clone());
    skiff_core::state::set_socket(dir.join("skiffd.sock"));
    assert!(skiff_core::state::private_dir().is_some(), "never the live state");

    let (root, outside, bin) = (dir.join("repo"), dir.join("outside"), dir.join("bin"));
    for d in [&root, &outside, &bin] {
        std::fs::create_dir_all(d).unwrap();
    }
    std::fs::write(root.join("victim.txt"), "1\n").unwrap();
    std::fs::write(outside.join("victim.txt"), "1\n2\n3\n").unwrap();
    let config = skiff_core::config::config_path();
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    std::fs::write(&config, format!("[[project]]\nname = \"app\"\npath = \"{}\"\n", root.display())).unwrap();

    // `git [-C <dir>] ...` (a worktree's runs start in its held folder, with
    // no `-C`): the root is its only worktree, `victim.txt` is untracked, and
    // once `swap` exists the diff replaces the root's path with a link to
    // `outside` (with `diff-swap` too, status does it).
    let swap = dir.join("swap");
    let script = format!(
        "#!/bin/sh\n[ \"$1\" = -C ] && shift 2\ncase \"$1\" in\n\
         worktree) printf 'worktree %s\\0HEAD 0000000\\0branch refs/heads/main\\0\\0' '{root}' ;;\n\
         status) printf '?? victim.txt\\0'; [ -e '{swap}' ] && [ -e '{dir}/diff-swap' ] && mv '{root}' '{dir}/original-repo' && ln -s '{outside}' '{root}'; exit 0 ;;\n\
         diff) [ -e '{swap}' ] && mv '{root}' '{dir}/original-repo' && ln -s '{outside}' '{root}'; exit 1 ;;\n\
         *) exit 1 ;;\nesac\n",
        root = root.display(),
        swap = swap.display(),
        dir = dir.display(),
        outside = outside.display(),
    );
    let git = bin.join("git");
    std::fs::write(&git, script).unwrap();
    std::fs::set_permissions(&git, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::env::set_var("PATH", format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default()));

    let hello = skiff_host::hello().unwrap();
    let ask = || skiff_host::answer(Request::GitChanges { worktree: root.to_str().unwrap().into() }, &hello);

    // Left alone: the root's own file, one line.
    let Ok(Response::Changes { changes, .. }) = ask().await else { panic!("no changes") };
    let one: Vec<_> = changes.iter().map(|c| (c.path.as_str(), c.status, c.added, c.removed)).collect();
    assert_eq!(one, [("victim.txt", ChangeStatus::A, Some(1), Some(0))]);

    // Any other path is opened before the check: a FIFO there never blocks.
    let fifo = dir.join("fifo");
    let c_fifo = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
    // SAFETY: a NUL-terminated path.
    assert_eq!(unsafe { libc::mkfifo(c_fifo.as_ptr(), 0o600) }, 0);
    let read = Request::GitChanges { worktree: fifo.to_str().unwrap().into() };
    let read = tokio::time::timeout(std::time::Duration::from_secs(10), skiff_host::answer(read, &hello)).await;
    let Ok(read) = read else {
        let _ = std::thread::spawn(move || std::fs::write(&fifo, ""));
        panic!("GitChanges blocked on a FIFO");
    };
    assert!(matches!(read, Err(Error::NotFound { .. })), "{read:?}");

    // Swapped at the diff: refused, never the outside file's three lines.
    std::fs::write(&swap, "").unwrap();
    let got = ask().await;
    assert!(std::fs::symlink_metadata(&root).unwrap().file_type().is_symlink(), "the swap ran");
    assert!(matches!(&got, Err(Error::OperationFailed { message }) if message.contains("replaced")), "{got:?}");

    // Diff text: put the root back, then swap it once git status ran.
    std::fs::remove_file(&root).unwrap();
    std::fs::rename(dir.join("original-repo"), &root).unwrap();
    let diff = || skiff_host::answer(Request::GitDiff { worktree: root.to_str().unwrap().into(), file: Some("victim.txt".into()) }, &hello);
    std::fs::remove_file(&swap).unwrap();
    let Ok(Response::Diff { diff: own }) = diff().await else { panic!("no diff") };
    assert!(own.untracked && own.text.ends_with("+1\n"), "{own:?}");
    std::fs::write(&swap, "").unwrap();
    std::fs::write(dir.join("diff-swap"), "").unwrap();
    let got = diff().await;
    assert!(std::fs::symlink_metadata(&root).unwrap().file_type().is_symlink(), "the swap ran");
    assert!(matches!(&got, Err(Error::OperationFailed { message }) if message.contains("replaced")), "{got:?}");
}
