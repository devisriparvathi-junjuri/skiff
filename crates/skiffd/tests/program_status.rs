//! OSC 7501, the Program Status Protocol: the reports Codex and Claude Code
//! send, replayed through the emulator, and what the pool makes of them.
//!
//! The fixtures are PTY output captured from Codex 0.166.0 and Claude Code
//! 2.1.316 on macOS, one read per line, in hex. Their visible text is blanked
//! to `x`; escape sequences and titles are as captured. Claude Code ran with
//! a terminal that answered its probe ("answered") and with one that did not.

use std::{path::PathBuf, sync::Arc, time::Duration};

use skiff_core::session::{SessionInfo, SessionSpec, SessionState};
use skiffd::{
    program_status::{self, Kind, Records, State},
    screen::{title_state, Screen, StatusSignal, TitleState},
    session::SessionPool,
};

fn fixture(name: &str) -> Vec<Vec<u8>> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/osc7501")
        .join(name);
    let hex = std::fs::read_to_string(&path).unwrap();
    hex.lines()
        .map(|l| {
            (0..l.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&l[i..i + 2], 16).unwrap())
                .collect()
        })
        .collect()
}

/// A root record as the tests compare it: state, kind and message.
type Root = Option<(State, Option<Kind>, Option<String>)>;

/// What a replay heard.
#[derive(Debug, Default, PartialEq)]
struct Heard {
    probes: usize,
    /// The root record at each change of it.
    roots: Vec<Root>,
    /// The session state at each change of the root record, from working.
    states: Vec<SessionState>,
    turns: usize,
    /// Not compared across reads: a read keeps only its last title.
    titles: Vec<String>,
}

fn replay(reads: &[Vec<u8>]) -> Heard {
    let mut screen = Screen::new(120, 40);
    let mut records = Records::default();
    let mut state = SessionState::Working;
    let mut heard = Heard::default();
    for read in reads {
        let signals = screen.feed(read);
        if let Some(Some(t)) = signals.title {
            heard.titles.push(t);
        }
        for signal in signals.status {
            let old = records.root().cloned();
            match signal {
                StatusSignal::Reset => records.clear(),
                StatusSignal::Report(body) if program_status::is_probe(&body) => heard.probes += 1,
                StatusSignal::Report(body) => {
                    if let Some(report) = program_status::parse(&body) {
                        records.apply(report);
                    }
                }
            }
            let new = records.root().cloned();
            if new != old {
                heard.turns +=
                    program_status::ends_turn(old.map(|r| r.state), new.as_ref().map(|r| r.state))
                        as usize;
                state = program_status::session_state(new.as_ref().map(|r| r.state), state);
                heard.states.push(state);
                heard.roots.push(new.map(|r| (r.state, r.kind, r.msg)));
            }
        }
    }
    heard
}

/// The same bytes in reads of 1 to 64 bytes, from a fixed seed.
fn rechunk(reads: &[Vec<u8>], mut seed: u64) -> Vec<Vec<u8>> {
    let all = reads.concat();
    let mut out = Vec::new();
    let mut at = 0;
    while at < all.len() {
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let n = ((seed >> 33) % 64 + 1) as usize;
        out.push(all[at..(at + n).min(all.len())].to_vec());
        at += n;
    }
    out
}

fn same_in_any_reads(name: &str) -> Heard {
    let reads = fixture(name);
    let heard = replay(&reads);
    let untitled = |h: Heard| Heard {
        titles: Vec::new(),
        ..h
    };
    let want = untitled(replay(&reads));
    for seed in 1..=8 {
        assert_eq!(
            untitled(replay(&rechunk(&reads, seed))),
            want,
            "{name}, seed {seed}"
        );
    }
    let bytes: Vec<Vec<u8>> = reads.concat().into_iter().map(|b| vec![b]).collect();
    assert_eq!(untitled(replay(&bytes)), want, "{name}, one byte a read");
    heard
}

#[test]
fn codex_reports_its_turns_without_a_probe() {
    use SessionState as S;
    use State as P;
    let heard = same_in_any_reads("codex-0.166.0.hex");
    assert_eq!(heard.probes, 0);
    let states: Vec<Option<State>> = heard
        .roots
        .iter()
        .map(|r| r.as_ref().map(|r| r.0))
        .collect();
    assert_eq!(
        states,
        [
            Some(P::Idle),
            Some(P::Working),
            Some(P::Idle),
            Some(P::Working),
            Some(P::Done),
            Some(P::Idle),
            Some(P::Working),
            Some(P::Blocked),
            Some(P::Working),
            Some(P::Idle),
            None
        ]
    );
    assert_eq!(
        heard.roots[7],
        Some((P::Blocked, Some(Kind::Permission), None))
    );
    assert_eq!(
        heard.states,
        [
            S::Idle,
            S::Working,
            S::Idle,
            S::Working,
            S::Idle,
            S::Idle,
            S::Working,
            S::Waiting,
            S::Working,
            S::Idle,
            S::Idle
        ]
    );
    // One turn ended with a result: the `done`. The startup and the denied
    // command end with `idle`.
    assert_eq!(heard.turns, 1);
}

#[test]
fn claude_code_answered_reports_its_turns() {
    use SessionState as S;
    use State as P;
    let heard = same_in_any_reads("claude-2.1.316-answered.hex");
    assert_eq!(heard.probes, 1);
    let msg = Some("approve Bash: mkdir /tmp/osc7501-deny-test".to_string());
    assert_eq!(
        heard.roots,
        [
            Some((P::Idle, None, None)),
            Some((P::Working, None, None)),
            Some((P::Done, None, None)),
            Some((P::Working, None, None)),
            Some((P::Blocked, Some(Kind::Permission), msg)),
            Some((P::Idle, None, None)),
            None,
        ]
    );
    assert_eq!(
        heard.states,
        [
            S::Idle,
            S::Working,
            S::Idle,
            S::Working,
            S::Waiting,
            S::Idle,
            S::Idle
        ]
    );
    assert_eq!(heard.turns, 1);
    // Answered, it keeps its spinner out of the title, so the title rules
    // would read no work at all: the reports must decide.
    assert!(
        heard
            .titles
            .iter()
            .all(|t| title_state("claude", t) != Some(TitleState::Busy)),
        "{:?}",
        heard.titles
    );
}

#[test]
fn claude_code_unanswered_only_probes() {
    let heard = same_in_any_reads("claude-2.1.316-unanswered.hex");
    assert_eq!((heard.probes, heard.roots.len()), (1, 0));
    // Unanswered, its title carries the spinner, as the title rules expect.
    assert!(heard
        .titles
        .iter()
        .any(|t| title_state("claude", t) == Some(TitleState::Busy)));
}

fn pool() -> Arc<SessionPool> {
    std::env::set_var("SHELL", "/bin/bash");
    let pool = SessionPool::new();
    pool.spawn_idle_watcher();
    pool.spawn_foreground_watcher();
    pool
}

/// Waits until the session's info passes `ok`, or fails after `secs`.
async fn until(
    pool: &SessionPool,
    id: &str,
    secs: u64,
    what: &str,
    ok: impl Fn(&SessionInfo) -> bool,
) -> SessionInfo {
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

/// The program sends the probe and DA1 in one write, as Claude Code does. A
/// client answers DA1 the moment it sees the output; the daemon's reply to
/// the probe must still reach the program first, and count as no input.
#[tokio::test(flavor = "multi_thread")]
async fn the_probe_reply_comes_before_a_clients_da1_reply() {
    let pool = pool();
    let info = pool
        .create(SessionSpec {
            command: Some("env".into()),
            args: vec![
                "sh".into(),
                "-c".into(),
                r#"stty raw -echo; sleep 0.5; printf '\033]7501;?\007\033[c'; head -c 17 | od -An -tx1; sleep 10"#.into(),
            ],
            ..Default::default()
        })
        .unwrap();
    let (_, mut output) = pool.get(&info.id).unwrap().attach();
    let mut seen = Vec::new();
    let end = tokio::time::Instant::now() + Duration::from_secs(5);
    while !seen.windows(3).any(|w| w == b"\x1b[c") {
        let chunk = tokio::time::timeout_at(end, output.recv())
            .await
            .expect("no probe")
            .unwrap();
        seen.extend_from_slice(&chunk);
    }
    // The client's answer to DA1, as xterm.js gives it.
    pool.write(&info.id, b"\x1b[?1;2c").unwrap();
    let mut text = String::new();
    while !text.contains("63") {
        let chunk = tokio::time::timeout_at(end, output.recv())
            .await
            .expect("no answer read back")
            .unwrap();
        text.extend(
            String::from_utf8_lossy(&chunk)
                .chars()
                .filter(char::is_ascii_hexdigit),
        );
    }
    assert_eq!(
        text, "1b5d373530313b3f1b5c1b5b3f313b3263",
        "the program read the replies in another order"
    );
    assert_eq!(
        pool.get(&info.id).unwrap().info().input_gen,
        1,
        "the probe reply counted as input"
    );
    pool.kill(&info.id).unwrap();
}

/// A report ends with the program that sent it: here a command whose
/// output carries one, at a shell. The prompt's return drops it.
#[tokio::test(flavor = "multi_thread")]
async fn the_shell_prompt_drops_the_report() {
    let pool = pool();
    let info = pool
        .create(SessionSpec {
            command: Some("/bin/bash".into()),
            args: vec!["--noprofile".into(), "--norc".into(), "-i".into()],
            ..Default::default()
        })
        .unwrap();
    until(&pool, &info.id, 6, "shell did not go idle", |i| {
        i.state == SessionState::Idle
    })
    .await;
    pool.write(
        &info.id,
        b"printf '\\033]7501;state=working\\007'; sleep 2\r",
    )
    .unwrap();
    until(&pool, &info.id, 4, "no report", |i| {
        i.program_status.is_some() && i.state == SessionState::Working
    })
    .await;
    let back = until(
        &pool,
        &info.id,
        8,
        "the prompt did not drop the report",
        |i| i.program_status.is_none(),
    )
    .await;
    assert_eq!(back.state, SessionState::Idle);
    pool.kill(&info.id).unwrap();
}
