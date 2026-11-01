//! `skiff skipper`: attach to the Skiff agent, or start it first. From any
//! terminal, an SSH login included.

use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::{Duration, Instant},
};

use anyhow::{bail, Context, Result};
use skiff_client::{
    agent_folder::{self, Kind, Mode, Preview},
    skipper, Client,
};

use crate::attach;

/// How long a daemon this command started may take to answer.
const START: Duration = Duration::from_secs(10);

pub async fn run(socket: &Path, no_attach: bool) -> Result<()> {
    let c = connect_or_start(socket).await?;
    let info = c.daemon_info().await?;
    if info.protocol < skiff_core::PROTOCOL {
        bail!("skiffd {} is too old for this skiff. {}", info.version, crate::daemon::advice(&c, &info).await);
    }
    let Some(found) = skipper::open(&c, skipper::Open::Start).await? else {
        bail!("skiffd did not start the Skiff agent");
    };
    let s = found.session;
    let name = s.name.clone().unwrap_or_else(|| s.label.clone());
    // This session alone changes the ledger from now on.
    if let Err(e) = crate::ledger::claim(socket, &s.id) {
        eprintln!("skiff: {e:#}");
    }
    let exe = std::env::current_exe().context("find this skiff binary")?;
    if let Err(e) = skipper::start_watch(socket, &exe, &s.id) {
        eprintln!("skiff: {name} runs, but its watcher did not start: {e:#}");
    }
    if no_attach {
        println!("{}", s.id);
        return Ok(());
    }
    if found.created {
        eprintln!("skiff: started {name}");
    } else if found.typed {
        eprintln!("skiff: started the agent in {name}'s shell");
    }
    attach::attach(&c, &s.id, None).await
}

/// `skiff skipper --project`: finds or starts the project Skipper of
/// `project`, then attaches unless `no_attach`.
pub async fn run_project(socket: &Path, project: &str, no_attach: bool) -> Result<()> {
    let c = connect_or_start(socket).await?;
    let ps = start_project(socket, &c, project).await?;
    let s = ps.session;
    if ps.created {
        match greet(&c, &s.id, &skipper::first_message(&skiff_core::config::load()?, &ps.dir)?).await? {
            0 => {}
            code => bail!("the {project} Skipper started, but its first message did not go in (exit {code}). Tell it to read {}", ps.dir.join(skipper::SKIPPER_FILE).display()),
        }
    }
    if no_attach {
        println!("{}", s.id);
        return Ok(());
    }
    if ps.created {
        eprintln!("skiff: started the {project} Skipper");
    }
    attach::attach(&c, &s.id, None).await
}

/// Finds or starts the project Skipper of `project`, and the watcher, which
/// follows project Skippers too. A new one has no message yet: see [`greet`].
pub async fn start_project(socket: &Path, c: &Client, project: &str) -> Result<skipper::ProjectSkipper> {
    let ps = skipper::open_project(c, project).await?;
    let exe = std::env::current_exe().context("find this skiff binary")?;
    if let Err(e) = skipper::start_watch(socket, &exe, &ps.session.id) {
        eprintln!("skiff: the {project} Skipper runs, but the watcher did not start: {e:#}");
    }
    Ok(ps)
}

/// `skiff skipper --fleet`: finds or starts the fleet Skipper of `fleet`,
/// then attaches unless `no_attach`. Project Skippers in the fleet are
/// taken over after a y/N, or at once with `yes`.
pub async fn run_fleet(socket: &Path, fleet: &str, no_attach: bool, yes: bool) -> Result<()> {
    let c = connect_or_start(socket).await?;
    let fs = start_fleet(socket, &c, fleet, Over::Ask { yes }).await?;
    let s = fs.skipper.session;
    if fs.skipper.created {
        let mut text = skipper::first_message(&skiff_core::config::load()?, &fs.skipper.dir)?;
        if !fs.taken.is_empty() {
            text = format!("{text} {}", skiff_client::fleet::line(fleet, &fs.taken));
        }
        match greet(&c, &s.id, &text).await? {
            0 => {}
            code => bail!("the {fleet} Skipper started, but its first message did not go in (exit {code}). Tell it to read {}", fs.skipper.dir.join(skipper::SKIPPER_FILE).display()),
        }
    }
    if no_attach {
        println!("{}", s.id);
        return Ok(());
    }
    if fs.skipper.created {
        eprintln!("skiff: started the {fleet} Skipper");
    }
    attach::attach(&c, &s.id, None).await
}

/// What [`start_fleet`] does with project Skippers in the fleet.
pub enum Over {
    /// Name them and ask y/N on the terminal; `yes` takes them over at once.
    Ask { yes: bool },
    /// Refuse: a hand-off cannot stop agents.
    Refuse,
}

/// A fleet Skipper found or started, and what it took over.
pub struct FleetStart {
    pub skipper: skipper::ProjectSkipper,
    pub taken: Vec<skiff_client::fleet::Taken>,
}

/// Finds or starts the fleet Skipper of `fleet`, and the watcher. Project
/// Skippers in the fleet are taken over as `over` says. A new one has no
/// message yet: see [`greet`].
pub async fn start_fleet(socket: &Path, c: &Client, fleet: &str, over: Over) -> Result<FleetStart> {
    let cfg = skiff_core::config::load()?;
    let all = c.list_sessions().await?;
    let file = skiff_client::ledger::path()?;
    let taken = skiff_client::fleet::taken(&cfg, &all, &file, fleet, None)?;
    if !taken.is_empty() {
        let names = taken.iter().map(|t| t.project.as_str()).collect::<Vec<_>>().join(", ");
        match over {
            Over::Refuse => bail!("{fleet} has project Skippers: {names}. Start the {fleet} Skipper from the fleet's menu in Skiff, or hand off to one project"),
            Over::Ask { yes: false } => {
                use std::io::IsTerminal;
                if !std::io::stdin().is_terminal() {
                    bail!("{fleet} has project Skippers: {names}. Run this in a terminal to answer, or add --yes");
                }
                eprintln!("{}", skiff_client::fleet::summary(&taken));
                if !ask(&format!("Start the {fleet} Skipper?"))? {
                    bail!("Nothing changed");
                }
            }
            Over::Ask { yes: true } => {}
        }
    }
    let ps = skipper::open_fleet(c, fleet).await?;
    let exe = std::env::current_exe().context("find this skiff binary")?;
    if let Err(e) = skipper::start_watch(socket, &exe, &ps.session.id) {
        eprintln!("skiff: the {fleet} Skipper runs, but the watcher did not start: {e:#}");
    }
    let taken = if taken.is_empty() { taken } else { skiff_client::fleet::takeover(c, socket, fleet, None, !ps.created).await? };
    for t in &taken {
        eprintln!("skiff: the {fleet} Skipper took over the {} Skipper", t.project);
    }
    Ok(FleetStart { skipper: ps, taken })
}

/// How long a new agent has to show its input box.
const READY: Duration = Duration::from_secs(90);

/// Waits until the new agent in `id` shows an empty input box, then sends
/// `text` with Enter. Returns `send`'s exit code.
pub async fn greet(c: &Client, id: &str, text: &str) -> Result<i32> {
    let s = ready(c, id).await?;
    crate::sessions::send_to(c, &s, crate::sessions::Send { text: &[text.to_string()], enter: true, raw: false, shell: false }).await
}

/// The session `id` once a known agent is in front, idle, with an empty
/// input box. Fails when it ends, or after [`READY`].
pub async fn ready(c: &Client, id: &str) -> Result<skiff_core::session::SessionInfo> {
    use skiff_core::session::SessionState;
    let since = Instant::now();
    loop {
        let all = c.list_sessions().await?;
        let Some(s) = all.into_iter().find(|s| s.id == id) else { bail!("session {id} is gone") };
        if s.state == SessionState::Done {
            bail!("session {id} exited before its agent was ready");
        }
        if s.state == SessionState::Idle && crate::sessions::is_agent(&s) && crate::watch::read_front(c, &s).await.ok() == Some(crate::watch::Front::Ready) {
            return Ok(s);
        }
        if since.elapsed() > READY {
            bail!("the agent in session {id} did not show its input box within {}s", READY.as_secs());
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// Connects. With no daemon at the socket, starts one, as the app does.
async fn connect_or_start(socket: &Path) -> Result<Client> {
    if let Ok(c) = Client::connect(socket).await {
        return crate::check_daemon(c).await;
    }
    // One start at a time: a second `skiff skipper` waits here, then finds
    // the daemon the first one started.
    let _lock = start_lock(socket).await?;
    if let Ok(c) = Client::connect(socket).await {
        return crate::check_daemon(c).await;
    }
    start_daemon(socket)?;
    let since = Instant::now();
    loop {
        if let Ok(c) = Client::connect(socket).await {
            if c.daemon_info().await.is_ok() {
                return crate::check_daemon(c).await;
            }
        }
        if since.elapsed() > START {
            bail!("skiffd did not start at {}; see {}", socket.display(), socket.with_extension("log").display());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// The lock beside the socket that a daemon start holds. It frees itself
/// when this process ends.
async fn start_lock(socket: &Path) -> Result<std::fs::File> {
    let path = socket.with_extension("start.lock");
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    }
    let f = std::fs::File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .with_context(|| format!("open {}", path.display()))?;
    let since = Instant::now();
    loop {
        match f.try_lock() {
            Ok(()) => return Ok(f),
            Err(std::fs::TryLockError::WouldBlock) if since.elapsed() < START => tokio::time::sleep(Duration::from_millis(50)).await,
            Err(std::fs::TryLockError::WouldBlock) => bail!("another skiff is starting skiffd at {}", socket.display()),
            Err(std::fs::TryLockError::Error(e)) => return Err(e).with_context(|| format!("lock {}", path.display())),
        }
    }
}

/// `SKIFF_DAEMON`, else the `skiffd` beside this binary, else the one on `PATH`.
fn daemon_binary() -> PathBuf {
    if let Some(p) = std::env::var_os("SKIFF_DAEMON") {
        return PathBuf::from(p);
    }
    std::env::current_exe()
        .ok()
        .map(|e| e.with_file_name("skiffd"))
        .filter(|p| p.is_file())
        .unwrap_or_else(|| PathBuf::from("skiffd"))
}

/// Starts skiffd on `socket` apart from this login: its own scope, session
/// and keyring (see [`skipper::outliving`] and [`skipper::detach`]), so it
/// outlives this terminal and an SSH logout. Logs beside the socket, as the app does.
fn start_daemon(socket: &Path) -> Result<()> {
    let log = socket.with_extension("log");
    if let Some(dir) = log.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    }
    // Keep the last daemon's log: it saw what the new one restores.
    let _ = std::fs::rename(&log, log.with_extension("log.1"));
    let file = std::fs::File::create(&log).with_context(|| format!("create {}", log.display()))?;
    let bin = daemon_binary();
    let mut cmd = skipper::outliving(&bin, &skipper::unit_for("skiffd", socket));
    cmd.env("SKIFF_SOCKET", socket).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::from(file));
    skipper::detach(&mut cmd);
    let mut child = cmd.spawn().with_context(|| format!("start {}", bin.display()))?;
    // It forks nothing: reap it if it ends while this command runs.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// `skiff skipper --move-to PATH`: the Change… dialog of Settings ›
/// Skippers › Folder as text. Prints the preview, asks, then copies,
/// checks and switches (or switches to a Skiff folder). Never without a
/// terminal to ask in: there is no `--yes`.
pub async fn move_to(socket: &Path, path: &Path, empty: bool, dry_run: bool) -> Result<()> {
    let cwd = std::env::current_dir().context("find this folder")?;
    let to = agent_folder::resolve(&path.to_string_lossy(), &cwd, &skipper::home()?)?;
    // A daemon that does not run has no Skipper and no running tasks.
    let c = Client::connect(socket).await.ok();
    let (panes, running) = match &c {
        Some(c) => agent_folder::sessions(c).await?,
        None => Default::default(),
    };
    let cfg = skiff_core::config::load()?;
    let main = skipper::name(&cfg);
    let p = agent_folder::preview(&cfg, &to, &panes)?;
    print!("{}", preview_text(&p, running.is_some(), &main));
    let mode = match p.kind {
        Kind::Current => bail!("{main} uses this folder now"),
        Kind::Refused => bail!("{}", p.refused.as_deref().unwrap_or("Skiff cannot use this folder")),
        Kind::Skiff if empty => bail!("{} is a Skiff folder. Skiff switches to it as it is; --empty does not apply", p.to.display()),
        Kind::Skiff => Mode::Switch,
        _ if empty || !p.can_copy => Mode::Empty,
        _ => {
            if let Some(why) = &p.copy_refused {
                bail!("{why} Add --empty to start empty");
            }
            Mode::Copy
        }
    };
    println!(
        "Skiff will {}.",
        match mode {
            Mode::Copy => "copy the current folder here, check the copy, then switch",
            Mode::Empty => "set up a new folder here with a new ledger, then switch",
            Mode::Switch => "switch to this folder",
        }
    );
    if dry_run {
        return Ok(());
    }
    use std::io::IsTerminal;
    if !std::io::stdin().is_terminal() {
        bail!("--move-to asks before it changes the folder. Run it in a terminal, or use --dry-run");
    }
    let anyway = !p.running.is_empty();
    if anyway && !ask("Tasks still run. Their results stay in the old folder. Change anyway?")? {
        return Ok(());
    }
    let what = match mode {
        Mode::Copy => "Copy and switch?",
        Mode::Empty => "Start empty and switch?",
        Mode::Switch => "Switch to this folder?",
    };
    if !ask(what)? {
        return Ok(());
    }
    if let (Some(c), Some(s)) = (&c, &running) {
        c.kill(&s.id).await.with_context(|| format!("end {main}'s session"))?;
    }
    let mut panes = panes;
    if let Some(s) = &running {
        panes.remove(&s.id);
    }
    let d = agent_folder::change(&cfg, &to, mode, &panes, anyway)?;
    println!("{main} now uses {}.", d.to.display());
    if let Some(n) = &d.copied {
        println!("Copied and checked: {} tasks, {} results ({}).", n.tasks, n.results, agent_folder::size(n.bytes));
    }
    if let Some(note) = &d.moved_note {
        println!("The old folder stays, with every file. Skiff added {}.", note.display());
    }
    if let Some(w) = &d.warning {
        eprintln!("skiff: {w}");
    }
    println!("{main} starts in the new folder at the next `skiff skipper`, with a new conversation.");
    Ok(())
}

/// The preview as the dialog shows it.
fn preview_text(p: &Preview, skipper_runs: bool, main: &str) -> String {
    use std::fmt::Write;
    let mut s = String::new();
    let _ = writeln!(s, "From  {}\nTo    {}", p.from.display(), p.to.display());
    let kind = match p.kind {
        Kind::Current => "in use now",
        Kind::New => "a new folder",
        Kind::Empty => "an empty folder",
        Kind::Skiff => "a Skiff folder: switch to it, no copy, no merge",
        Kind::Refused => "refused",
    };
    let _ = writeln!(s, "The folder is {kind}.");
    // The caller says why a folder is refused.
    if p.refused.is_some() {
        return s;
    }
    if let (true, Some(c)) = (matches!(p.kind, Kind::New | Kind::Empty) && p.can_copy, &p.from_counts) {
        let _ = writeln!(
            s,
            "Copy: AGENTS.md{}, the ledger ({} tasks: {} open, {} done), results/ ({} files, {}){}.",
            if c.agents_edited { " (edited)" } else { "" },
            c.tasks,
            c.open,
            c.done,
            c.results,
            agent_folder::size(c.bytes),
            if c.other.is_empty() { String::new() } else { format!(", and {}", c.other.join(", ")) }
        );
        let _ = writeln!(s, "Setup makes the links again. The old folder stays, with every file, and gets MOVED.md.");
    }
    if let (Kind::Skiff, Some(t)) = (p.kind, &p.to_counts) {
        let _ = writeln!(s, "This folder: {} tasks, {} open. Skiff does not merge two ledgers.", t.tasks, t.open);
    }
    if p.moved.is_some() {
        let _ = writeln!(s, "{main} moved away from this folder before. Tasks added since then are only in the other folder.");
    }
    if p.kind != Kind::Current {
        let _ = writeln!(s, "The conversation does not move.");
    }
    if skipper_runs {
        let _ = writeln!(s, "{main} runs in {}. Its session ends before the change.", p.from.display());
    }
    for r in &p.running {
        let _ = writeln!(s, "Still runs: {}  {}", r.uid, r.task);
    }
    if let Some(l) = &p.ledger_env {
        let _ = writeln!(s, "SKIFF_LEDGER is set to {l}. The change does not touch it.");
    }
    s
}

/// A yes or no on the terminal. No is the default.
fn ask(question: &str) -> Result<bool> {
    use std::io::Write;
    eprint!("{question} [y/N] ");
    std::io::stderr().flush()?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    Ok(matches!(line.trim(), "y" | "Y" | "yes"))
}
