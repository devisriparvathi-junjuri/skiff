//! Session commands: ls, new, send, rename, kill.

use std::{io::Read, path::{Path, PathBuf}, time::Duration};

use anyhow::{bail, Context, Result};
use skiff_client::Client;
use skiff_core::{
    config::TaskTypes,
    group::Group,
    project::AgentInfo,
    session::{Role, SessionInfo, SessionSpec, SessionState},
    tasks::Start,
};

use crate::{callsign::callsign, groups, layout::Side, ledger, print, resolve, watch};

/// Sessions in a stable order: oldest first, then by id. Tools in the app's
/// peek are not sessions to the user, so they are left out.
pub async fn sorted(c: &Client) -> Result<Vec<SessionInfo>> {
    let mut all = c.list_sessions().await?;
    all.retain(|s| !s.peek);
    all.sort_by(|a, b| a.started_at.cmp(&b.started_at).then_with(|| a.id.cmp(&b.id)));
    Ok(all)
}

pub async fn ls(c: &Client, json: bool) -> Result<()> {
    let all = sorted(c).await?;
    if json {
        return print::json(&all);
    }
    let rows: Vec<Vec<String>> = all
        .iter()
        .map(|s| {
            let mut cmd = s.command.clone();
            for a in &s.args {
                cmd.push(' ');
                cmd.push_str(a);
            }
            vec![
                print::short_id(&s.id).to_string(),
                resolve::display_name(s).to_string(),
                state(s.state).to_string(),
                role(s.role).to_string(),
                print::tilde(&s.cwd),
                cmd,
            ]
        })
        .collect();
    print::table(&["ID", "NAME", "STATE", "ROLE", "CWD", "COMMAND"], &rows);
    Ok(())
}

pub fn state(s: SessionState) -> &'static str {
    match s {
        SessionState::Working => "working",
        SessionState::Waiting => "waiting",
        SessionState::Idle => "idle",
        SessionState::Done => "done",
    }
}

pub fn role(r: Role) -> &'static str {
    match r {
        Role::Agent => "agent",
        Role::Task => "task",
        Role::Server => "server",
        Role::Shell => "shell",
    }
}

pub struct New {
    pub name: Option<String>,
    pub cwd: Option<PathBuf>,
    pub agent: Option<String>,
    /// A `[skiff_agent.tasks]` row: its agent with its model.
    pub task_type: Option<String>,
    /// With `task_type`: this model in place of the row's.
    pub model: Option<String>,
    pub role: Option<Role>,
    pub command: Vec<String>,
}

/// Builds the spec the way the app's `newSession` does, so the sidebar
/// shows a CLI session like one made from the + menu.
pub async fn spec(c: &Client, o: New) -> Result<SessionSpec> {
    let here = std::env::current_dir().context("current directory")?;
    let cwd = match &o.cwd {
        Some(d) => here.join(d),
        None => here,
    };
    if !cwd.is_dir() {
        bail!("{} is not a directory", cwd.display());
    }
    // `SessionSpec::default()` has 0x0; serde's 120x40 default applies only
    // when decoding. Use the app's size for a pane it has not measured.
    let mut spec = SessionSpec {
        cwd: Some(cwd),
        cols: 120,
        rows: 40,
        ..SessionSpec::default()
    };
    let mut flags = Vec::new();
    let words: Vec<String> = if o.task_type.is_some() || o.agent.is_some() {
        let cfg = skiff_core::config::load();
        // A task type reads its rows and its instruction file in the agent's folder.
        let (rows, dir) = match o.task_type {
            Some(_) => {
                let cfg = cfg.as_ref().map_err(|e| anyhow::anyhow!("{e:#}"))?;
                (cfg.skiff_agent.tasks.clone(), skiff_client::skipper::folder(cfg)?)
            }
            None => Default::default(),
        };
        let agents = c.list_agents().await?;
        let start = agent_line(&agents, &rows, &o, &dir)?;
        if !start.env.is_empty() {
            needs_env(c, start.agent).await?;
        }
        // The image cache's read flag goes after the model, as one argv word each.
        flags = cfg.map(|cfg| skiff_client::cache::flags(&cfg, start.agent)).unwrap_or_default();
        spec.env = start.env;
        start.words
    } else {
        o.command
    };
    let agent = o.agent.is_some() || o.task_type.is_some();
    spec.role = o.role.unwrap_or(if agent { Role::Agent } else { Role::Shell });
    if let Some((first, rest)) = words.split_first() {
        spec.command = Some(first.clone());
        spec.args = rest.to_vec();
        spec.args.extend(flags);
    }
    if spec.role == Role::Agent {
        // The app gives an agent a call sign as both label and name.
        let label = match (o.name, &o.task_type) {
            (Some(n), _) => n,
            // A task type names its session after the type.
            (None, Some(t)) => {
                let live = c.list_sessions().await?;
                numbered(t, live.iter().filter(|s| s.state != SessionState::Done).flat_map(|s| [s.label.as_str()].into_iter().chain(s.name.as_deref())))
            }
            (None, None) => {
                let live = c.list_sessions().await?;
                callsign(live.iter().map(|s| s.label.as_str()))
            }
        };
        spec.name = Some(label.clone());
        spec.label = label;
    } else {
        // Empty label: the daemon uses the program's file name.
        spec.label = o.agent.unwrap_or_default();
        spec.name = o.name;
    }
    Ok(spec)
}

/// What `--type` or `--agent` starts: the person's command from Settings ›
/// Agents, else the built-in one, which keeps the agent's permission
/// prompts. A task type adds its model and its instruction file, read from
/// the agent's folder `dir`.
fn agent_line<'a>(agents: &'a [AgentInfo], rows: &TaskTypes, o: &New, dir: &Path) -> Result<Start<'a>> {
    if let Some(t) = &o.task_type {
        let start = skiff_core::tasks::start(rows, agents, t, o.model.as_deref(), dir)?;
        if !start.agent.installed {
            bail!("agent {:?} of task type {t} is not installed: {} is not on skiffd's PATH", start.agent.id, start.agent.command);
        }
        return Ok(start);
    }
    let id = o.agent.as_deref().unwrap_or_default();
    let Some(a) = agents.iter().find(|a| a.id == id) else {
        let ids: Vec<&str> = agents.iter().map(|a| a.id.as_str()).collect();
        bail!("no agent {id:?}; known: {}", ids.join(", "));
    };
    if !a.installed {
        bail!("agent {id:?} is not installed: {} is not on skiffd's PATH", a.command);
    }
    Ok(Start { agent: a, words: a.command.split_whitespace().map(String::from).collect(), env: vec![] })
}

/// The first skiffd that passes a session its variables. An older one drops
/// them, and the agent would start without its instruction file.
const ENV_SINCE: (u64, u64, u64) = (0, 6, 29);

/// Refuses a start that needs variables on a skiffd older than [`ENV_SINCE`].
async fn needs_env(c: &Client, a: &AgentInfo) -> Result<()> {
    let info = c.daemon_info().await?;
    let v: Vec<u64> = info.version.split(['.', '-', '+']).take(3).map(|n| n.parse().unwrap_or(0)).collect();
    if (v.first().copied().unwrap_or(0), v.get(1).copied().unwrap_or(0), v.get(2).copied().unwrap_or(0)) < ENV_SINCE {
        bail!("skiffd {} cannot give {} its instruction file. {}", info.version, a.id, crate::daemon::advice(c, &info).await);
    }
    Ok(())
}

/// `name`, else `name 2`, `name 3` and on: the first one no live session uses.
pub fn numbered<'a>(name: &str, used: impl IntoIterator<Item = &'a str>) -> String {
    let used: std::collections::HashSet<&str> = used.into_iter().collect();
    (1..)
        .map(|n| if n == 1 { name.to_string() } else { format!("{name} {n}") })
        .find(|n| !used.contains(n.as_str()))
        .expect("a free name")
}

/// Starts a session. With `group`, the group is found first, so a typo
/// starts nothing.
pub async fn new(c: &Client, mut o: New, group: Option<&str>, split: Option<Side>, json: bool) -> Result<()> {
    let target = match group {
        Some(arg) => {
            let all = c.list_groups().await?;
            let g = resolve::group(&all, arg)?.clone();
            groups::room(&g, &groups::Place { split, of: None })?;
            if o.cwd.is_none() {
                o.cwd = group_cwd(c, &g).await?;
            }
            Some((all, g))
        }
        None => None,
    };
    let spec = spec(c, o).await?;
    let info = c.create_session(spec).await?;
    if let Some((all, g)) = target {
        let live = c.list_sessions().await?;
        let place = groups::Place { split, of: None };
        if let Err(e) = groups::place(c, &all, g, &info.id, place, &live).await {
            eprintln!("skiff: started {} but did not place it", info.id);
            return Err(e);
        }
    }
    if json {
        return print::json(&info);
    }
    println!("{}", info.id);
    Ok(())
}

/// Where the app starts a session in `g`: the group's folder, else the
/// folder of its first session. A folder that is gone counts as none.
async fn group_cwd(c: &Client, g: &Group) -> Result<Option<PathBuf>> {
    let dir = match (&g.cwd, crate::layout::filled(&g.layout).first()) {
        (Some(d), _) => Some(PathBuf::from(d)),
        (None, Some(first)) => c.list_sessions().await?.into_iter().find(|s| &s.id == first).map(|s| s.cwd),
        (None, None) => None,
    };
    Ok(dir.filter(|d| d.is_dir()))
}


/// Why `send` must not type into `s`, as an exit code and a message.
/// `here` is `SKIFF_SESSION`; `caller_shell` is the caller's `SHELL`.
/// Checks run in a fixed order; `--shell` relaxes only the last one.
pub fn refusal(s: &SessionInfo, here: Option<&str>, shell_ok: bool, caller_shell: Option<&str>) -> Option<(i32, String)> {
    let who = format!("{} ({})", resolve::display_name(s), print::short_id(&s.id));
    if here.is_some_and(|h| h == s.id) {
        return Some((7, format!("{who} is your own session. Send to another session.")));
    }
    match s.state {
        SessionState::Waiting => return Some((3, waiting_msg(&resolve::display_name(s), &s.id))),
        SessionState::Working => {
            return Some((4, format!("{who} is busy. Skiff does not type into a working session.")))
        }
        SessionState::Done => return Some((6, format!("{who} is done. Its program exited."))),
        SessionState::Idle => {}
    }
    if shell_ok {
        return None;
    }
    let prog = s.foreground_program.as_deref().map(basename).unwrap_or("");
    if prog.is_empty() {
        return Some((
            5,
            format!("Skiff cannot identify the foreground program of {who}. Use --shell to send anyway."),
        ));
    }
    let caller = caller_shell.map(basename).unwrap_or("");
    if SHELLS.contains(&prog) || (!caller.is_empty() && prog == caller) {
        // `skiff new -- CMD` and custom agents run under a `sh` launch
        // wrapper, so their own program does not show.
        if prog == "sh" && s.role != Role::Shell {
            return Some((
                5,
                format!("Skiff cannot identify the program in front of {who}; it shows the launch shell. Use --shell to send anyway."),
            ));
        }
        return Some((5, format!("{who} has a shell ({prog}) in front. Use --shell to type a command into it.")));
    }
    None
}

/// The message for a session that needs a person: `send` and `wait` share it.
pub fn waiting_msg(name: &str, id: &str) -> String {
    format!("{name} ({}) is waiting (approval prompt or bell). Check its pane in Skiff.", print::short_id(id))
}

const SHELLS: [&str; 9] = ["bash", "zsh", "fish", "sh", "dash", "ksh", "tcsh", "csh", "nu"];

/// A program's file name. A login shell's leading `-` is dropped.
fn basename(p: &str) -> &str {
    let p = p.trim();
    let name = p.rsplit('/').next().unwrap_or(p);
    name.strip_prefix('-').unwrap_or(name)
}

pub struct Send<'a> {
    pub text: &'a [String],
    pub enter: bool,
    pub raw: bool,
    pub shell: bool,
}

/// Exit codes: 0 when sent; 3 waiting, 4 working, 5 a shell or unknown
/// program in front, 6 done, 7 own session, 9 a draft or the busy line in a
/// known agent, or input after the check. A refusal types nothing.
/// 8: Enter went to a known agent, but no turn started.
pub async fn send(c: &Client, arg: &str, o: Send<'_>) -> Result<i32> {
    let all = c.list_sessions().await?;
    let s = resolve::session(&all, arg)?;
    send_to(c, s, o).await
}

/// [`send`] to a session from a list read just now: its `input_gen` guards
/// the submit.
pub async fn send_to(c: &Client, s: &SessionInfo, o: Send<'_>) -> Result<i32> {
    let here = std::env::var("SKIFF_SESSION").ok().filter(|h| !h.is_empty());
    let caller_shell = std::env::var("SHELL").ok();
    if let Some((code, msg)) = refusal(s, here.as_deref(), o.shell, caller_shell.as_deref()) {
        eprintln!("skiff: {msg}");
        return Ok(code);
    }
    let who = format!("{} ({})", resolve::display_name(s), print::short_id(&s.id));
    let (enter, raw) = (o.enter, o.raw);
    // Only a known agent proves a turn by its state, and only its screen
    // shows a draft. A shell command can finish with no output.
    let agent = !raw && is_agent(s);
    if agent {
        if let Some(msg) = front_refusal(watch::read_front(c, s).await?) {
            eprintln!("skiff: {who} {msg}");
            return Ok(9);
        }
    }
    let mut data = if o.text == ["-"] {
        let mut buf = Vec::new();
        std::io::stdin().read_to_end(&mut buf).context("read stdin")?;
        buf
    } else {
        o.text.join(" ").into_bytes()
    };
    if !raw {
        data = enter_keys(&data);
    }
    // `--enter` presses Enter once. A newline at the end of the text, as a
    // heredoc or a file has, would only add a blank line before it.
    if enter {
        while data.last() == Some(&b'\r') || data.last() == Some(&b'\n') {
            data.pop();
        }
    }
    // Listen before the submit, so no change falls before the listener.
    let events = (agent && enter).then(|| c.events());
    // A paste, so the program keeps the text's newlines as text. skiffd
    // types it only if the session took no input since the list.
    if !c.submit(&s.id, data, !raw, enter, s.input_gen).await? {
        eprintln!("skiff: {who} took input after the check. Nothing was typed.");
        return Ok(9);
    }
    let Some(events) = events else { return Ok(0) };
    if turn_started(c, &s.id, events).await? {
        return Ok(0);
    }
    // Never press Enter again: it can submit twice or add a blank line.
    eprintln!("skiff: {who}: text sent, but no turn started. It can be in the input box. Check its pane.");
    Ok(8)
}

/// Why `send` must not type into a known agent whose screen shows `f`.
pub fn front_refusal(f: watch::Front) -> Option<&'static str> {
    match f {
        watch::Front::Draft => Some("has text in its input box. Skiff does not add to a draft. The person sends or clears it."),
        watch::Front::Busy => Some("shows its busy line. Skiff does not type into a working agent."),
        watch::Front::Ready | watch::Front::NoBox => None,
    }
}

/// How long an agent has to start a turn after Enter.
const TURN_WAIT: Duration = Duration::from_secs(5);

/// The program in front is an agent Skiff knows. The daemon names a known
/// agent by its id ("claude", "codex"...), whatever binary runs it.
pub fn is_agent(s: &SessionInfo) -> bool {
    let prog = s.foreground_program.as_deref().map(basename).unwrap_or("");
    skiff_core::project::KNOWN_AGENTS.iter().any(|(id, _)| *id == prog)
}

/// True when the session leaves idle within `TURN_WAIT`: the agent took the
/// message. An exit or a removal also counts, as the program reacted.
pub async fn turn_started(
    c: &Client,
    id: &str,
    mut events: tokio::sync::broadcast::Receiver<skiff_core::protocol::Event>,
) -> Result<bool> {
    use skiff_core::protocol::Event;
    use tokio::sync::broadcast::error::RecvError;
    let deadline = tokio::time::Instant::now() + TURN_WAIT;
    loop {
        match tokio::time::timeout_at(deadline, events.recv()).await {
            Err(_) => break,
            Ok(Ok(Event::State { session, state })) if session == id && state != SessionState::Idle => {
                return Ok(true)
            }
            Ok(Ok(Event::Exit { session, .. } | Event::SessionRemoved { session })) if session == id => {
                return Ok(true)
            }
            Ok(Ok(_)) => {}
            // Missed events: read the state again.
            Ok(Err(RecvError::Lagged(_))) => {
                if !still_idle(c, id).await? {
                    return Ok(true);
                }
            }
            Ok(Err(RecvError::Closed)) => bail!("skiffd closed the connection"),
        }
    }
    // The paste can start work before the listener exists. Read the state.
    Ok(!still_idle(c, id).await?)
}

async fn still_idle(c: &Client, id: &str) -> Result<bool> {
    let all = c.list_sessions().await?;
    Ok(all.iter().any(|s| s.id == id && s.state == SessionState::Idle))
}

/// Newlines as the Enter key sends them: `\r`.
fn enter_keys(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    let mut i = 0;
    while i < data.len() {
        match data[i] {
            b'\r' if data.get(i + 1) == Some(&b'\n') => {
                out.push(b'\r');
                i += 1;
            }
            b'\n' => out.push(b'\r'),
            b => out.push(b),
        }
        i += 1;
    }
    out
}

pub async fn rename(c: &Client, arg: &str, name: &str) -> Result<()> {
    let all = c.list_sessions().await?;
    let s = resolve::session(&all, arg)?;
    c.rename_session(&s.id, name).await?;
    Ok(())
}

/// Clears the session's unread mark, as focusing its pane in the app does.
pub async fn seen(c: &Client, arg: &str) -> Result<()> {
    let all = c.list_sessions().await?;
    let s = resolve::session(&all, arg)?;
    c.seen(&s.id).await
}

/// The exit code of `new` or `group new` from a Skipper with no `--name`.
pub const UNNAMED: i32 = 2;

/// The hint for an unnamed session or group from a Skipper.
pub fn name_rule(group: bool) -> &'static str {
    if group {
        "a Skipper names each group: --name after the work it holds. Nothing started."
    } else {
        "a Skipper names each session: --name \"#N feature\" for one issue (#136 kanban and calendar), else 2 to 4 words for the task, without the project name. Nothing started."
    }
}

/// The caller (`$SKIFF_SESSION`) is a Skipper: Admiral, or a project or
/// fleet Skipper. A shell pane, an agent pane or a shell outside Skiff is not.
pub async fn coordinator_calls(c: &Client) -> Result<bool> {
    let Some(me) = std::env::var("SKIFF_SESSION").ok().filter(|s| !s.is_empty()) else { return Ok(false) };
    Ok(c.list_sessions().await?.iter().find(|s| s.id == me).is_some_and(is_coordinator))
}

fn is_coordinator(s: &SessionInfo) -> bool {
    s.coordinator || s.skipper.is_some()
}

/// `kill`'s exit code when a session has an open ledger task.
pub const KILL_HELD: i32 = 10;

/// Resolves every argument first, so a typo kills nothing. A session with an
/// open ledger task stops the whole kill, unless `force`.
pub async fn kill(c: &Client, socket: &Path, args: &[String], force: bool) -> Result<i32> {
    let all = c.list_sessions().await?;
    let mut ids: Vec<String> = Vec::new();
    for a in args {
        let id = resolve::session(&all, a)?.id.clone();
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    if !force {
        let open = ledger::open_tasks()?;
        let held = held(&ids, &open, socket);
        if !held.is_empty() {
            for (id, r) in &held {
                let name = all.iter().find(|s| &s.id == *id).map(resolve::display_name).unwrap_or_default();
                let title = r.title.as_deref().unwrap_or(&r.task);
                eprintln!("skiff: {name} ({}) has open task {}: {title}", print::short_id(id), r.uid);
            }
            eprintln!("skiff: nothing killed. Close the tasks, or use --force.");
            return Ok(KILL_HELD);
        }
    }
    let mut failed = 0;
    for id in &ids {
        if let Err(e) = c.kill(id).await {
            eprintln!("skiff: kill {}: {e:#}", print::short_id(id));
            failed += 1;
        }
    }
    if failed > 0 {
        bail!("{failed} of {} sessions not killed", ids.len());
    }
    Ok(0)
}

/// Each of `ids` with an open task in `open` on this socket. A stored pane id
/// may be a prefix. A task on another daemon names another pane.
fn held<'a>(ids: &'a [String], open: &'a [ledger::Row], here: &Path) -> Vec<(&'a String, &'a ledger::Row)> {
    ids.iter()
        .flat_map(|id| {
            open.iter()
                .filter(move |r| r.socket_other_than(here).is_none() && r.pane_id.as_deref().is_some_and(|p| id.starts_with(p)))
                .map(move |r| (id, r))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use skiff_core::session::{SessionInfo, SessionState};
    use std::path::Path;

    use super::{agent_line, front_refusal, held, is_coordinator, numbered, refusal, New};
    use skiff_core::{
        config::{TaskRow, TaskTypes},
        project::AgentInfo,
    };

    fn agent(id: &str, command: &str) -> AgentInfo {
        AgentInfo { id: id.into(), command: command.into(), default_command: id.into(), custom: false, installed: true, enabled: true, used_by: None }
    }

    fn new(agent: Option<&str>, task_type: Option<&str>) -> New {
        New { name: None, cwd: None, agent: agent.map(Into::into), task_type: task_type.map(Into::into), model: None, role: None, command: vec![] }
    }

    #[test]
    fn kill_is_held_by_an_open_task_on_this_daemon() {
        let here = Path::new("/run/skiff.sock");
        let ids = vec!["aaaa1111ffff".to_string(), "bbbb2222ffff".to_string()];
        let other = crate::ledger::Row { socket: Some("/run/other.sock".into()), ..crate::watch::tests::row(2, "u2", "bbbb2222", "sent", None) };
        let open = [crate::watch::tests::row(1, "u1", "aaaa1111", "blocked", None), other];
        let held = held(&ids, &open, here);
        // A stored prefix matches; a task on another daemon names another pane.
        assert_eq!(held.iter().map(|(id, r)| (id.as_str(), r.uid.as_str())).collect::<Vec<_>>(), [("aaaa1111ffff", "u1")]);
    }

    #[test]
    fn only_a_skipper_must_name_what_it_starts() {
        let shell = s("shell", SessionState::Idle, None);
        let agent = s("agent", SessionState::Idle, None);
        let admiral = SessionInfo { coordinator: true, ..agent.clone() };
        let project = SessionInfo { skipper: Some("skiff".into()), ..agent.clone() };
        let fleet = SessionInfo { skipper: Some("fleet:work".into()), ..agent.clone() };
        assert!(!is_coordinator(&shell) && !is_coordinator(&agent));
        assert!(is_coordinator(&admiral) && is_coordinator(&project) && is_coordinator(&fleet));
    }

    #[test]
    fn new_agent_and_new_type_start_the_persons_command() {
        let rows = TaskTypes(vec![("review".into(), TaskRow { agent: "codex".into(), model: Some("gpt-5".into()) })]);
        let dir = Path::new("/no/such/skiff-agent");
        let line = |agents: &[AgentInfo], o: &New| agent_line(agents, &rows, o, dir).unwrap().words.join(" ");
        // The built-in line: no permission bypass.
        let plain = [agent("claude", "claude"), agent("codex", "codex")];
        assert_eq!(line(&plain, &new(Some("claude"), None)), "claude");
        assert_eq!(line(&plain, &new(None, Some("review"))), "codex -m gpt-5");
        // The person's override, with its flags, on both paths.
        let mine = [agent("claude", "claude --dangerously-skip-permissions"), agent("codex", "codex --yolo")];
        assert_eq!(line(&mine, &new(Some("claude"), None)), "claude --dangerously-skip-permissions");
        assert_eq!(line(&mine, &new(Some("codex"), None)), "codex --yolo");
        assert_eq!(line(&mine, &new(None, Some("review"))), "codex --yolo -m gpt-5");
    }

    #[test]
    fn new_type_gives_the_agent_its_instruction_file() {
        let dir = std::env::temp_dir().join(format!("skiff-new-type agent-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("types")).unwrap();
        for t in ["review", "fast", "ui"] {
            std::fs::write(dir.join(format!("types/{t}.md")), "Review only.").unwrap();
        }
        let row = |agent: &str| TaskRow { agent: agent.into(), model: None };
        let rows = TaskTypes(vec![("review".into(), row("claude")), ("fast".into(), row("gemini")), ("ui".into(), row("opencode")), ("plain".into(), row("gemini"))]);
        let agents = [agent("claude", "claude"), agent("gemini", "agy"), agent("opencode", "opencode")];
        let f = dir.join("types/review.md").display().to_string();
        // The path stays one word: the daemon gets it as one argument.
        assert_eq!(agent_line(&agents, &rows, &new(None, Some("review")), &dir).unwrap().words, ["claude", "--append-system-prompt-file", f.as_str()]);
        let ui = agent_line(&agents, &rows, &new(None, Some("ui")), &dir).unwrap();
        assert_eq!((ui.words, ui.env[0].0.as_str()), (vec!["opencode".to_string()], "OPENCODE_CONFIG_CONTENT"));
        // An agent with no way to take it: an error, and nothing starts. No file: the plain command.
        let e = agent_line(&agents, &rows, &new(None, Some("fast")), &dir).unwrap_err().to_string();
        assert!(e.contains("gemini cannot take an instruction file"), "{e}");
        assert_eq!(agent_line(&agents, &rows, &new(None, Some("plain")), &dir).unwrap().words, ["agy"]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_task_type_session_takes_the_first_free_number() {
        assert_eq!(numbered("review", ["spec", "Captain Kraken"]), "review");
        assert_eq!(numbered("review", ["review", "review 3"]), "review 2");
        assert_eq!(numbered("review", ["review", "review 2"]), "review 3");
    }

    #[test]
    fn newlines_become_enter() {
        assert_eq!(super::enter_keys(b"a\nb\r\nc\r"), b"a\rb\rc\r");
    }

    fn s(role: &str, state: SessionState, fg: Option<&str>) -> SessionInfo {
        let mut s: SessionInfo = serde_json::from_value(serde_json::json!({
            "id": "abcdef0123", "label": "x", "role": role, "cwd": "/", "command": "claude",
            "args": [], "state": "idle", "cols": 80, "rows": 24, "pid": null, "exit_code": null,
        }))
        .unwrap();
        s.state = state;
        s.foreground_program = fg.map(String::from);
        s
    }

    fn code(x: &SessionInfo, here: Option<&str>, shell: bool, caller: Option<&str>) -> i32 {
        refusal(x, here, shell, caller).map_or(0, |r| r.0)
    }

    #[test]
    fn order_and_codes() {
        use SessionState::*;
        let idle = s("agent", Idle, Some("claude"));
        assert_eq!(code(&idle, Some("abcdef0123"), true, None), 7);
        assert_eq!(code(&s("agent", Waiting, Some("claude")), Some("abcdef0123"), true, None), 7);
        assert_eq!(code(&s("agent", Waiting, Some("bash")), None, true, None), 3);
        assert_eq!(code(&s("agent", Working, None), None, true, None), 4);
        assert_eq!(code(&s("shell", Done, None), None, true, None), 6);
        assert_eq!(code(&idle, Some("other"), false, None), 0);
    }

    #[test]
    fn shell_check() {
        use SessionState::Idle;
        // An agent that exited to a shell, and a nested shell, need --shell.
        assert_eq!(code(&s("agent", Idle, Some("bash")), None, false, None), 5);
        assert_eq!(code(&s("agent", Idle, Some("/usr/bin/zsh")), None, false, None), 5);
        assert_eq!(code(&s("agent", Idle, Some("bash")), None, true, None), 0);
        // Unknown program.
        assert_eq!(code(&s("shell", Idle, None), None, false, None), 5);
        assert_eq!(code(&s("shell", Idle, Some("")), None, false, None), 5);
        assert_eq!(code(&s("shell", Idle, None), None, true, None), 0);
        // The caller's SHELL counts as a shell.
        assert_eq!(code(&s("shell", Idle, Some("xonsh")), None, false, Some("/usr/bin/xonsh")), 5);
        assert_eq!(code(&s("shell", Idle, Some("xonsh")), None, false, Some("")), 0);
        // A shell-role pane with an agent in front.
        assert_eq!(code(&s("shell", Idle, Some("codex")), None, false, Some("/bin/bash")), 0);
    }

    #[test]
    fn a_draft_or_the_busy_line_refuses_and_an_empty_box_sends() {
        use crate::watch::Front;
        assert!(front_refusal(Front::Draft).is_some());
        assert!(front_refusal(Front::Busy).is_some());
        assert!(front_refusal(Front::Ready).is_none());
        // No box to read: the state checks decide, as before.
        assert!(front_refusal(Front::NoBox).is_none());
    }

    #[test]
    fn known_agents_only() {
        use SessionState::Idle;
        for fg in ["claude", "codex", "gemini", "grok", "opencode", "hermes"] {
            assert!(super::is_agent(&s("shell", Idle, Some(fg))), "{fg}");
        }
        for fg in [None, Some("bash"), Some("sh"), Some("vim"), Some("node")] {
            assert!(!super::is_agent(&s("agent", Idle, fg)), "{fg:?}");
        }
    }
}
