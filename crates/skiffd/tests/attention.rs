//! The bell and the unread flag: what sets each, for a shell, a program
//! and an agent. One file, so every test here can use bash as the shell.

use std::{path::PathBuf, sync::Arc, time::Duration};

use skiff_core::session::{SessionInfo, SessionSpec, SessionState};
use skiffd::session::SessionPool;

fn pool() -> Arc<SessionPool> {
    std::env::set_var("SHELL", "/bin/bash");
    let pool = SessionPool::new();
    pool.spawn_idle_watcher();
    pool.spawn_foreground_watcher();
    pool
}

/// Waits until the session's info passes `ok`, or fails after `secs`.
async fn until(pool: &SessionPool, id: &str, secs: u64, what: &str, ok: impl Fn(&SessionInfo) -> bool) -> SessionInfo {
    let end = std::time::Instant::now() + Duration::from_secs(secs);
    loop {
        let info = pool.get(id).unwrap().info();
        if ok(&info) {
            return info;
        }
        assert!(std::time::Instant::now() < end, "{what}: {info:?}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn bash(pool: &Arc<SessionPool>) -> SessionInfo {
    pool.create(SessionSpec {
        command: Some("/bin/bash".into()),
        args: vec!["--noprofile".into(), "--norc".into(), "-i".into()],
        ..Default::default()
    })
    .unwrap()
}

/// A program with an agent's name that runs `script` with sh, so the
/// daemon takes it for that agent. It is this test binary under that name,
/// run as [`fake_agent_runs_its_script`] with [`FAKE_AGENT_ARGS`]. A script
/// would not do: macOS names its process after the interpreter, and a
/// renamed copy of a system shell is killed for its signature.
fn fake_agent(name: &str, script: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("skiff-attention-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join(name);
    std::fs::copy(std::env::current_exe().unwrap(), &file).unwrap();
    std::fs::write(file.with_extension("sh"), script).unwrap();
    file
}

const FAKE_AGENT_ARGS: [&str; 4] = [
    "fake_agent_runs_its_script",
    "--exact",
    "-q",
    "--test-threads=1",
];

/// The fake agent's body. In this binary's own run there is no script
/// beside it, and it does nothing.
#[test]
fn fake_agent_runs_its_script() {
    let script = std::env::current_exe().unwrap().with_extension("sh");
    if !script.is_file() {
        return;
    }
    let status = std::process::Command::new("/bin/sh")
        .arg(&script)
        .status()
        .unwrap();
    std::process::exit(status.code().unwrap_or(1));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_command_that_ends_in_a_shell_leaves_it_unread() {
    let pool = pool();
    let info = bash(&pool);
    until(&pool, &info.id, 6, "shell did not go idle", |i| i.state == SessionState::Idle).await;
    assert!(!pool.get(&info.id).unwrap().info().unread);
    pool.write(&info.id, b"sleep 1.5; echo built\r").unwrap();
    let done = until(&pool, &info.id, 6, "the command's end did not set unread", |i| i.unread).await;
    assert_eq!(done.state, SessionState::Idle);
    pool.seen(&info.id).unwrap();
    assert!(!pool.get(&info.id).unwrap().info().unread, "seen did not clear unread");
    pool.kill(&info.id).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_program_that_rings_the_bell_waits_until_input() {
    let pool = pool();
    let info = pool
        .create(SessionSpec {
            command: Some("env".into()),
            args: vec!["sh".into(), "-c".into(), "sleep 0.5; printf 'look\\a'; sleep 8".into()],
            ..Default::default()
        })
        .unwrap();
    until(&pool, &info.id, 5, "the bell did not set waiting", |i| i.state == SessionState::Waiting).await;
    pool.write(&info.id, b"y").unwrap();
    until(&pool, &info.id, 2, "input did not answer the bell", |i| i.state != SessionState::Waiting).await;
    pool.kill(&info.id).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn an_agent_waits_at_its_prompt_and_its_turn_end_is_unread() {
    let pool = pool();
    // It draws itself and goes quiet. After a line of input it works, asks for
    // approval without a bell, works again after the answer, and stops. Later
    // it prints a recap with no busy line, which is no turn.
    let agent = fake_agent(
        "codex",
        r#"printf 'Codex ready\n'
read task
for i in 1 2 3 4 5; do printf 'Working (%ss - esc to interrupt)\n' "$i"; sleep 0.4; done
printf 'Would you like to run the following command?\n  $ touch x\n'
printf 'Press enter to confirm or esc to cancel\n'
sleep 0.4; printf '\r'
read answer
printf '\033[2J\033[H'
for i in 1 2 3 4 5; do printf 'Working (%ss - esc to interrupt)\n' "$i"; sleep 0.4; done
printf '\033[2J\033[HDone.\n'
read again
for i in 1 2 3 4 5; do printf 'recap %s\n' "$i"; sleep 0.4; done
sleep 30"#,
    );
    let info = pool
        .create(SessionSpec {
            command: Some(agent.display().to_string()),
            args: FAKE_AGENT_ARGS.map(String::from).to_vec(),
            cols: 100,
            rows: 30,
            ..Default::default()
        })
        .unwrap();
    let ready = until(&pool, &info.id, 8, "the agent did not go idle after it started", |i| i.state == SessionState::Idle).await;
    assert!(!ready.unread, "an agent that only started got the unread flag");

    pool.write(&info.id, b"do it\n").unwrap();
    until(&pool, &info.id, 8, "the approval prompt did not set waiting", |i| i.state == SessionState::Waiting).await;
    // The prompt stays for as long as it is on the screen.
    tokio::time::sleep(Duration::from_millis(3500)).await;
    let asked = pool.get(&info.id).unwrap().info();
    assert_eq!(asked.state, SessionState::Waiting);
    assert!(!asked.unread, "a prompt is a bell, not a result");

    pool.write(&info.id, b"\n").unwrap();
    until(&pool, &info.id, 4, "the answer did not end waiting", |i| i.state == SessionState::Working).await;
    let done = until(&pool, &info.id, 10, "the end of the turn did not set unread", |i| i.unread).await;
    assert_eq!(done.state, SessionState::Idle);

    pool.seen(&info.id).unwrap();
    pool.write(&info.id, b"\n").unwrap();
    until(&pool, &info.id, 4, "the recap did not count as output", |i| i.state == SessionState::Working).await;
    let recap = until(&pool, &info.id, 8, "the agent did not go idle after the recap", |i| i.state == SessionState::Idle).await;
    assert!(!recap.unread, "text with no busy line set the unread flag");
    pool.kill(&info.id).unwrap();
    let _ = std::fs::remove_dir_all(agent.parent().unwrap());
}

#[tokio::test(flavor = "multi_thread")]
async fn the_exit_code_of_a_program_survives_the_hand_off() {
    let pool = pool();
    let info = pool
        .create(SessionSpec {
            command: Some("env".into()),
            args: vec!["sh".into(), "-c".into(), "echo boom; exit 3".into()],
            ..Default::default()
        })
        .unwrap();
    let after = until(&pool, &info.id, 6, "the hand-off lost the exit code", |i| i.agent_exit.is_some()).await;
    assert_eq!(after.agent_exit, Some(3));
    assert_eq!(after.label, "bash");
    assert_ne!(after.state, SessionState::Done, "the pane did not get a shell");
    pool.seen(&info.id).unwrap();
    assert_eq!(pool.get(&info.id).unwrap().info().agent_exit, None);
    pool.kill(&info.id).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_retired_skipper_loses_its_mark_at_once_and_ends_when_idle() {
    let pool = pool();
    let info = pool
        .create(SessionSpec {
            command: Some("/bin/bash".into()),
            args: vec!["--noprofile".into(), "--norc".into(), "-i".into()],
            skipper: Some("web".into()),
            ..Default::default()
        })
        .unwrap();
    until(&pool, &info.id, 6, "shell did not go idle", |i| i.state == SessionState::Idle).await;
    // A plain session is no Skipper to retire.
    let plain = bash(&pool);
    assert!(pool.retire(&plain.id).is_err());
    // It works: the mark goes now, the session stays until its turn ends.
    pool.write(&info.id, b"/bin/sh -c 'for i in 1 2 3 4 5 6 7 8; do echo tick; sleep 0.3; done'\r").unwrap();
    until(&pool, &info.id, 4, "shell did not work", |i| i.state == SessionState::Working).await;
    pool.retire(&info.id).unwrap();
    let now = pool.get(&info.id).unwrap().info();
    assert!(now.skipper.is_none() && now.retiring, "{now:?}");
    let end = std::time::Instant::now() + Duration::from_secs(12);
    while pool.get(&info.id).is_some() {
        assert!(std::time::Instant::now() < end, "the retiring session did not end when idle");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    pool.kill(&plain.id).unwrap();
}
