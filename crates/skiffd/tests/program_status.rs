//! OSC 7501, the Program Status Protocol: the reports coding agents send,
//! replayed through the emulator, and what the pool makes of them.
//!
//! The fixtures are PTY output captured from Codex 0.166.0, Claude Code
//! 2.1.316, Grok 1.0.70, Antigravity (agy) 1.4.12 and OpenCode 2.2.5 on
//! macOS, one read per line, in hex. Their visible text is blanked to `x`;
//! escape sequences and titles are as captured, but for the paths cut from
//! agy's and Grok's messages and Grok's titles. Claude Code, Grok, agy and
//! OpenCode ran with a terminal that answered their probe ("answered") and
//! with one that did not.

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
    /// The root record's app at each change of it.
    apps: Vec<Option<String>>,
    /// The session state at each change of the root record, from working.
    states: Vec<SessionState>,
    turns: usize,
    /// Not compared across reads: a read keeps only its last title.
    titles: Vec<String>,
}

/// Replays `reads` as the pool reads reports. Time and output are counted
/// in bytes fed (1 ms a byte), so a turn's length does not depend on how
/// the bytes were split. The user types before each working stretch but the
/// first `untyped` ones: a program's own startup work.
fn replay(reads: &[Vec<u8>], untyped: usize) -> Heard {
    let mut screen = Screen::new(120, 40);
    let mut records = Records::default();
    let mut state = SessionState::Working;
    let mut heard = Heard::default();
    let (mut fed, mut input, mut stretches) = (0u64, 0u64, 0usize);
    for read in reads {
        let signals = screen.feed(read);
        fed += read.len() as u64;
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
                let (o, n) = (old.map(|r| r.state), new.as_ref().map(|r| r.state));
                if n == Some(State::Working) && o != n {
                    stretches += 1;
                    input += (stretches > untyped) as u64;
                }
                heard.turns += records.turns.step(o, n, fed, input, fed) as usize;
                state = program_status::session_state(new.as_ref().map(|r| r.state), state);
                heard.states.push(state);
                heard.apps.push(new.as_ref().and_then(|r| r.app.clone()));
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
    // Codex works once on its own as it starts, before the first prompt.
    let untyped = name.starts_with("codex") as usize;
    let replay = |reads: &[Vec<u8>]| replay(reads, untyped);
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

/// Grok asks with XTVERSION, not DA1, and reports only once its probe is
/// answered. Not signed in, it waits for the sign-in, and it clears its
/// report as it quits.
#[test]
fn grok_answered_reports_its_sign_in() {
    use SessionState as S;
    use State as P;
    let heard = same_in_any_reads("grok-1.0.70-signin-answered.hex");
    assert_eq!(heard.probes, 1);
    assert_eq!(
        heard.roots,
        [
            Some((P::Blocked, Some(Kind::Auth), Some("Sign in to Grok".into()))),
            None,
        ]
    );
    assert_eq!(heard.apps, [Some("grok-build".into()), None]);
    // The clear takes the sign-in prompt with it; the screen rules decide
    // from working, and the shell's return then makes it idle.
    assert_eq!(heard.states, [S::Waiting, S::Working]);
    assert_eq!(heard.turns, 0);
}

#[test]
fn grok_unanswered_only_probes() {
    let heard = same_in_any_reads("grok-1.0.70-signin-unanswered.hex");
    assert_eq!((heard.probes, heard.roots.len()), (1, 0));
}

/// Signed in, Grok reports each turn: `working` with what it does in
/// `msg`, `done` at the end of a reply, `blocked` for a permission, and
/// `idle` once a rejected permission cancels the turn. Its title keeps its
/// spinner either way.
#[test]
fn grok_answered_reports_its_turns() {
    use SessionState as S;
    use State as P;
    let heard = same_in_any_reads("grok-1.0.70-answered.hex");
    assert_eq!(heard.probes, 1);
    let working = |msg: &str| Some((P::Working, None, Some(msg.to_string())));
    assert_eq!(
        heard.roots,
        [
            Some((P::Idle, None, None)),
            working("Waiting for response…"),
            working("Responding"),
            Some((P::Done, None, Some("Turn complete in 2.4s.".into()))),
            working("Waiting for response…"),
            working("Thinking"),
            working("Responding"),
            Some((
                P::Blocked,
                Some(Kind::Permission),
                Some("Allow Edit to hello.txt?".into())
            )),
            working("Running: Write `hello.txt"),
            Some((P::Idle, None, None)),
            None,
        ]
    );
    assert!(heard.apps[..10]
        .iter()
        .all(|a| a.as_deref() == Some("grok-build")));
    assert_eq!(
        heard.states,
        [
            S::Idle,
            S::Working,
            S::Working,
            S::Idle,
            S::Working,
            S::Working,
            S::Working,
            S::Waiting,
            S::Working,
            S::Idle,
            S::Idle
        ]
    );
    assert_eq!(heard.turns, 1);
    // Its title says the same in both modes; while it reports, the
    // reports decide and the title rule waits.
    let read: Vec<_> = heard
        .titles
        .iter()
        .filter_map(|t| title_state("grok", t))
        .collect();
    assert!(read.contains(&TitleState::Busy) && read.contains(&TitleState::Action));
}

#[test]
fn grok_unanswered_only_probes_but_keeps_its_title() {
    let heard = same_in_any_reads("grok-1.0.70-unanswered.hex");
    assert_eq!((heard.probes, heard.roots.len()), (1, 0));
    // Unanswered, its title is what tells its turns and its prompt.
    let read: Vec<_> = heard
        .titles
        .iter()
        .filter_map(|t| title_state("grok", t))
        .collect();
    assert!(read.contains(&TitleState::Busy) && read.contains(&TitleState::Action));
    assert_eq!(read.last(), Some(&TitleState::Ready));
}

/// Antigravity probes after its DA1 and takes a reply however late. It ends
/// a turn with `done` and the reply's text, and a turned-down prompt with
/// `idle`. It sets no title.
#[test]
fn agy_answered_reports_its_turns() {
    use SessionState as S;
    use State as P;
    let heard = same_in_any_reads("agy-1.4.12-answered.hex");
    assert_eq!(heard.probes, 1);
    assert_eq!(
        heard.roots,
        [
            Some((P::Idle, None, None)),
            Some((P::Working, None, None)),
            Some((P::Done, None, Some("Hi! How can I help you today?".into()))),
            Some((P::Idle, None, None)),
            Some((P::Working, None, None)),
            Some((
                P::Blocked,
                Some(Kind::Permission),
                Some("Approve WriteFile: Write: hello.txt".into())
            )),
            Some((P::Idle, None, None)),
        ]
    );
    assert!(heard
        .apps
        .iter()
        .all(|a| a.as_deref() == Some("antigravity")));
    assert_eq!(
        heard.states,
        [
            S::Idle,
            S::Working,
            S::Idle,
            S::Idle,
            S::Working,
            S::Waiting,
            S::Idle
        ]
    );
    assert_eq!(heard.turns, 1);
    assert!(heard.titles.is_empty(), "{:?}", heard.titles);
}

#[test]
fn agy_unanswered_only_probes() {
    let heard = same_in_any_reads("agy-1.4.12-unanswered.hex");
    assert_eq!((heard.probes, heard.roots.len()), (1, 0));
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
/// the probe must still reach the program first. Neither reply is input.
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
        0,
        "a reply counted as input"
    );
    pool.kill(&info.id).unwrap();
}

/// A program that dies mid-frame leaves a synchronized update open. Its
/// timeout ends it with no more output: the report, the title and the
/// text apply.
#[tokio::test(flavor = "multi_thread")]
async fn a_frame_left_open_applies_at_its_timeout() {
    let pool = pool();
    let info = pool
        .create(SessionSpec {
            command: Some("env".into()),
            args: vec![
                "sh".into(),
                "-c".into(),
                r#"sleep 0.3; printf '\033[?2026h\033]0;Frozen\007\033]7501;state=working\007left open'; sleep 10"#.into(),
            ],
            cols: 80,
            rows: 24,
            ..Default::default()
        })
        .unwrap();
    let on = until(&pool, &info.id, 4, "the frame never applied", |i| {
        i.program_status.is_some() && i.title.as_deref() == Some("Frozen")
    })
    .await;
    assert_eq!(on.state, SessionState::Working);
    let text = pool.tail_text(&info.id, 5).unwrap();
    assert!(text.concat().contains("left open"), "{text:?}");
    pool.kill(&info.id).unwrap();
}

/// A report ends with the program that sent it: here a command whose
/// output carries one, at a shell. The prompt's return drops it, and a
/// report printed at the prompt itself is not taken.
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
    // printf is the shell's own: the prompt is in front.
    pool.write(&info.id, b"printf '\\033]7501;state=blocked\\007'\r")
        .unwrap();
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let ignored = pool.get(&info.id).unwrap().info();
    assert!(
        ignored.program_status.is_none() && ignored.state == SessionState::Idle,
        "{ignored:?}"
    );
    pool.write(
        &info.id,
        b"sh -c \"sleep 1.5; printf '\\033]7501;state=working\\007'; sleep 2\"\r",
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

/// OpenCode 2.2.5 reports whether or not its probe is answered. A turn is
/// `working`, with a child record for its session that it clears at the
/// end, and the root goes back to `idle`: it sends no `done` or `error`,
/// and no `clear` as it quits. So its `idle` after a turn ends the turn.
#[test]
fn opencode_reports_in_both_modes_and_its_idle_ends_a_turn() {
    use SessionState as S;
    use State as P;
    for name in [
        "opencode-2.2.5-answered.hex",
        "opencode-2.2.5-unanswered.hex",
    ] {
        let heard = same_in_any_reads(name);
        assert_eq!(heard.probes, 1, "{name}");
        let states: Vec<_> = heard
            .roots
            .iter()
            .map(|r| r.as_ref().map(|r| r.0))
            .collect();
        assert_eq!(
            states,
            [
                Some(P::Idle),
                Some(P::Working),
                Some(P::Working),
                Some(P::Idle)
            ],
            "{name}"
        );
        assert!(heard.apps.iter().all(|a| a.as_deref() == Some("opencode")));
        assert_eq!(heard.states, [S::Idle, S::Working, S::Working, S::Idle]);
        // It never says done: its idle after a turn the user started ends it.
        assert_eq!(heard.turns, 1, "{name}");
    }
}
