//! Track the current foreground program without shell hooks or agent changes.
use super::{away::{proc, Tree}, Front, Session, SessionPool};
use skiff_core::{protocol::Event, session::{SessionState, Was}};
use std::{
    collections::VecDeque,
    path::Path,
    sync::{atomic::Ordering, Arc, Condvar, Mutex},
    time::Duration,
};

/// The watcher reads the group in front of each session this often.
const TICK: Duration = Duration::from_secs(1);

/// A front that can change under a group that stays is read again every
/// this many ticks. See [`settled`].
const RECHECK_TICKS: u32 = 3;

/// Set by a reader thread that saw a new group in front. The watcher wakes
/// and reads it, so the state does not wait for the next tick.
static WAKE: (Mutex<bool>, Condvar) = (Mutex::new(false), Condvar::new());

/// Wakes the foreground watcher before its next tick.
pub(super) fn wake() {
    *WAKE.0.lock().unwrap() = true;
    WAKE.1.notify_one();
}

/// Sleeps one tick, or less when a reader thread wakes the watcher.
fn wait_tick() {
    let (woken, changed) = &WAKE;
    let mut woken = woken.lock().unwrap();
    if !*woken {
        woken = changed.wait_timeout(woken, TICK).unwrap().0;
    }
    *woken = false;
}

impl SessionPool {
    /// Reads the group in front of each session once a tick, and sooner
    /// when a reader thread saw it change. Only a changed group reads
    /// `/proc`, or a recheck every few ticks of a front that can change
    /// without one.
    pub fn spawn_foreground_watcher(self: &Arc<Self>) {
        let pool = self.clone();
        let _ = std::thread::Builder::new()
            .name("foreground".into())
            .spawn(move || {
                let mut tick: u32 = 0;
                loop {
                    wait_tick();
                    tick = tick.wrapping_add(1);
                    pool.scan_foreground(tick.is_multiple_of(RECHECK_TICKS));
                }
            });
    }

    /// One pass over the sessions. `recheck` also reads the fronts that are
    /// not settled under a group the watcher already read.
    fn scan_foreground(&self, recheck: bool) {
        let Some(_quiet) = self.quiet() else { return };
        let sessions: Vec<_> = self.sessions.read().unwrap().values().cloned().collect();
        let mut tree = Tree::default();
        for session in sessions {
            self.classify(&session, recheck, &mut tree);
        }
    }

    /// Reads the program in front of one session: its name for the client,
    /// the agent to offer after a restore, and the rules for its state.
    /// The group in front is one ioctl. The rest reads `/proc`, so a group
    /// two reads agreed on is skipped while its front is settled. `tree` is
    /// kept for a pass over many sessions.
    fn classify(&self, session: &Session, recheck: bool, tree: &mut Tree) {
        let pid = {
            let info = session.info.lock().unwrap();
            if info.state == SessionState::Done {
                return;
            }
            info.pid
        };
        let Some(group) = session.foreground_group() else { return };
        if group == session.group.load(Ordering::Relaxed) && (!recheck || settled(session.front())) {
            return;
        }
        let Some(name) = proc::name(group) else { return };
        let argv = proc::argv(group);
        let agent = agent_program(&name, &argv).or_else(|| {
            // The launch wrapper shares its group with the initial agent.
            // An interactive shell can also start a pipeline in one group.
            agent_in_group(group, tree)
        });
        let front = match agent {
            Some(agent) => Front::Agent(agent),
            None if !session.shell.load(Ordering::Relaxed) => Front::Program,
            None if Some(group) == pid => Front::Shell,
            None => Front::Command,
        };
        let same_front = session.front() == front;
        // Reports made under the group before are its program's.
        self.status_group(session, group);
        self.set_front(session, front);
        let agent = agent.map(str::to_owned);
        let program = agent.clone().unwrap_or(name);
        let (changed, was_changed, same_program) = {
            let mut info = session.info.lock().unwrap();
            // Keep the agent in front and its title, so a restore can offer it.
            // Agents lead the title with a status glyph that changes as they work.
            let title = info
                .title
                .as_deref()
                .map(|t| t.trim_start_matches(|c: char| !c.is_alphanumeric()).to_string())
                .filter(|t| !t.is_empty());
            // An agent's own name ("Claude Code") is no title. Keep the last real one.
            let title = title.filter(|t| !generic_title(t)).or_else(|| {
                info.was.as_ref().filter(|w| Some(&w.agent) == agent.as_ref()).and_then(|w| w.title.clone())
            });
            let was = agent.map(|agent| Was { agent, title });
            // An agent in front ends the offer to resume.
            let resumed = was.is_some() && info.resume.take().is_some();
            let was_changed = was.is_some() && info.was != was;
            if was_changed {
                info.was = was;
            }
            let same_program = info.foreground_program.as_deref() == Some(&program);
            if same_program && !resumed {
                (None, was_changed, true)
            } else {
                info.foreground_program = Some(program);
                (Some(info.clone()), was_changed, same_program)
            }
        };
        // A read that changes nothing settles the group, so the next tick
        // skips it. A read that changes the front or the program clears the
        // cache instead, so the next pass reads whatever is in front then:
        // the group can change back before that pass, and a stale cache would
        // make it look unchanged. One read can also land between a fork and
        // its exec, when the process still has its parent's name: the second
        // read corrects it.
        session.group.store(if same_front && same_program { group } else { 0 }, Ordering::Relaxed);
        if was_changed {
            self.dirty.notify_one();
        }
        if let Some(session) = changed {
            let _ = self.events.send(Event::SessionUpdated { session });
        }
    }
}

/// The title an agent shows before a conversation has a topic: its own name.
pub(crate) fn generic_title(title: &str) -> bool {
    let t = title.trim().to_lowercase();
    matches!(
        t.as_str(),
        "claude" | "claude code" | "codex" | "openai codex" | "opencode" | "gemini" | "gemini cli" | "antigravity" | "agy" | "grok" | "grok build" | "hermes" | "hermes agent" | "kimi" | "kimi code"
    )
}

/// True when the front cannot change while the group in front stays, so
/// there is nothing to read. Only a shell at its prompt is that: it runs
/// each job in a group of its own. Every other front is read again now and
/// then. A command or an unknown program can start an agent later (`npx`
/// does). An agent can end while its group stays: the launch wrapper execs
/// the fallback shell in its place, and a script can run the agent and then
/// another program. An agent recheck is two reads of its leader, so it costs
/// little.
fn settled(front: Front) -> bool {
    matches!(front, Front::Shell)
}

fn agent_in_group(group: u32, tree: &mut Tree) -> Option<&'static str> {
    let mut queue = VecDeque::from([group]);
    while let Some(pid) = queue.pop_front() {
        for child in tree.children(pid) {
            if proc::group(child) != Some(group) {
                continue;
            }
            if let Some(name) = proc::name(child) {
                if let Some(agent) = agent_program(&name, &proc::argv(child)) {
                    return Some(agent);
                }
            }
            queue.push_back(child);
        }
    }
    None
}

/// The agent a program name stands for, by the name the rest of the daemon uses.
pub(crate) fn known(name: &str) -> Option<&'static str> {
    match name {
        "claude" => Some("claude"),
        "codex" => Some("codex"),
        "gemini" | "agy" => Some("gemini"),
        "grok" => Some("grok"),
        "opencode" => Some("opencode"),
        "hermes" => Some("hermes"),
        "kimi" => Some("kimi"),
        _ => None,
    }
}

fn agent_program(name: &str, argv: &[String]) -> Option<&'static str> {
    let base = |s: &str| {
        Path::new(s)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string()
    };
    if let Some(agent) = known(name).or_else(|| argv.first().and_then(|s| known(&base(s)))) {
        return Some(agent);
    }
    // JavaScript CLIs run as node/bun, Python ones (hermes) as python. Only
    // inspect the script, never prompt arguments.
    if !matches!(name, "node" | "bun" | "deno") && !name.starts_with("python") {
        return None;
    }
    let script = argv.get(1)?;
    if script.starts_with('-') {
        return None;
    }
    if let Some(agent) = known(
        base(script)
            .trim_end_matches(".js")
            .trim_end_matches(".mjs"),
    ) {
        return Some(agent);
    }
    for (package, agent) in [
        ("/@anthropic-ai/claude-code/", "claude"),
        ("/@openai/codex/", "codex"),
        ("/@google/gemini-cli/", "gemini"),
    ] {
        if script.contains(package) {
            return Some(agent);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn detects_binary_and_script_agents_without_reading_prompt_arguments() {
        assert_eq!(agent_program("codex", &[]), Some("codex"));
        assert_eq!(
            agent_program(
                "node",
                &[
                    "node".into(),
                    "/opt/node_modules/@anthropic-ai/claude-code/cli.js".into()
                ]
            ),
            Some("claude")
        );
        assert_eq!(
            agent_program("node", &["node".into(), "/opt/bin/gemini".into()]),
            Some("gemini")
        );
        assert_eq!(
            agent_program("python3", &["python3".into(), "/home/u/.local/bin/hermes".into()]),
            Some("hermes")
        );
        assert_eq!(agent_program("hermes", &[]), Some("hermes"));
        assert_eq!(
            agent_program("bash", &["bash".into(), "-c".into(), "codex".into()]),
            None
        );
        assert_eq!(
            agent_program(
                "node",
                &["node".into(), "server.js".into(), "claude".into()]
            ),
            None
        );
    }

    #[test]
    fn only_a_shell_at_its_prompt_skips_the_recheck() {
        assert!(settled(Front::Shell));
        // An agent can end while its group stays, under the launch wrapper
        // or a script. A job or an unknown program can grow an agent.
        assert!(!settled(Front::Agent("claude")));
        assert!(!settled(Front::Command));
        assert!(!settled(Front::Program));
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn foreground_follows_agent_restart_and_shell_return() {
        use skiff_core::session::SessionSpec;
        let dir = std::env::temp_dir().join(format!("skiff-foreground-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for agent in ["claude", "codex"] {
            std::os::unix::fs::symlink("/bin/sleep", dir.join(agent)).unwrap();
        }
        let pool = SessionPool::new();
        let info = pool
            .create(SessionSpec {
                command: Some("/bin/bash".into()),
                args: vec!["--noprofile".into(), "--norc".into(), "-i".into()],
                name: Some("My session".into()),
                ..Default::default()
            })
            .unwrap();
        async fn wait(pool: &SessionPool, id: &str, expected: &str) {
            let until = std::time::Instant::now() + Duration::from_secs(3);
            loop {
                // No recheck: each step here changes the group in front.
                pool.scan_foreground(false);
                if pool.get(id).unwrap().info().foreground_program.as_deref() == Some(expected) {
                    break;
                }
                assert!(
                    std::time::Instant::now() < until,
                    "foreground did not become {expected}"
                );
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }
        wait(&pool, &info.id, "bash").await;
        // Two reads that agree settle the group: the shell at its prompt
        // is then skipped until the group in front changes.
        let session = pool.get(&info.id).unwrap();
        pool.scan_foreground(false);
        assert_eq!(Some(session.group.load(Ordering::Relaxed)), session.foreground_group());
        for agent in ["claude", "codex"] {
            pool.write(
                &info.id,
                format!("{} 30\r", dir.join(agent).display()).as_bytes(),
            )
            .unwrap();
            wait(&pool, &info.id, agent).await;
            // One read saw the agent. Let the shell take the terminal back
            // before the next read, so that read must find the shell from
            // a group the watcher read only once.
            let job = session.foreground_group();
            pool.write(&info.id, b"\x03").unwrap();
            let until = std::time::Instant::now() + Duration::from_secs(3);
            while session.foreground_group() == job {
                assert!(std::time::Instant::now() < until, "{agent} did not end");
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            wait(&pool, &info.id, "bash").await;
        }
        assert_eq!(
            pool.get(&info.id).unwrap().info().name.as_deref(),
            Some("My session")
        );
        pool.kill(&info.id).unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }
}
