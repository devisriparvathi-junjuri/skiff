//! The long-running process: the lock, the build handover, the event loop, the ledger reads, routing and delivery.

use super::*;

pub(super) async fn run(socket: &Path, o: Options) -> Result<i32> {
    // Another socket never follows the live agent's ledger.
    if let Some(dir) = ledger::path()?.parent() {
        skiff_client::skipper::refuse_live_folder(dir)?;
    }
    // Checked before the lock, so a stray watcher never holds the lock that
    // the agent's watcher needs.
    if let Ok(c) = Client::connect(socket).await {
        let known = matches!(c.request(Request::Ping).await, Ok(Response::Pong { protocol, .. }) if protocol >= 27);
        let all = sessions::sorted(&c).await?;
        if let Some(refusal) = not_the_agent(&all, &o.skipper, known) {
            eprintln!("{refusal}");
            return Ok(2);
        }
        // The app starts the agent without `skiff skipper`: its watcher records the owner.
        if let Some(s) = all.iter().find(|s| s.coordinator && s.state != SessionState::Done) {
            if let Err(e) = ledger::claim(socket, &s.id) {
                log(format!("{e:#}"));
            }
        }
    }
    let Some(_lock) = lock(socket).await? else {
        log("another watcher runs for this daemon");
        return Ok(0);
    };
    log(format!("watching for {}", o.skipper));
    let mut ws = Watchers::default();
    let mut build = Build::now();
    let mut lost: Option<Instant> = None;
    loop {
        if let Ok(c) = Client::connect(socket).await {
            lost = None;
            // A new connection has no registration yet.
            for w in ws.values_mut() {
                w.sent = None;
                w.unregistered = false;
            }
            match follow(&c, &o, &mut ws, &mut build).await {
                Ok(code) => return Ok(code),
                Err(e) => log(format!("lost skiffd: {e:#}")),
            }
        }
        let since = *lost.get_or_insert_with(Instant::now);
        if since.elapsed() > RECONNECT_LIMIT {
            log("skiffd did not come back");
            return Ok(1);
        }
        sleep(Duration::from_secs(1)).await;
    }
}

/// The lock beside the socket. It frees itself when the process ends, however
/// it ends. `None` when another watcher keeps it past [`LOCK_WAIT`].
pub(super) async fn lock(socket: &Path) -> Result<Option<File>> {
    let path = socket.with_extension("watch.lock");
    let f = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .with_context(|| format!("open {}", path.display()))?;
    let until = Instant::now() + LOCK_WAIT;
    loop {
        match f.try_lock() {
            Ok(()) => {
                // The app reads who holds the lock, to tell a watcher that did
                // not register and to restart it. Not fatal: the lock still works.
                let mut f = f;
                if let Err(e) = skiff_client::skipper::write_holder(&mut f, socket) {
                    log(format!("cannot write the watch lock: {e:#}"));
                }
                return Ok(Some(f));
            }
            Err(std::fs::TryLockError::WouldBlock) if Instant::now() < until => sleep(Duration::from_millis(250)).await,
            Err(std::fs::TryLockError::WouldBlock) => return Ok(None),
            Err(std::fs::TryLockError::Error(e)) => return Err(e).with_context(|| format!("lock {}", path.display())),
        }
    }
}

/// One watcher per coordinator, by owner: [`ledger::GRAND`], or a project.
pub(super) type Watchers = HashMap<String, Watcher>;

/// The `skiff` file this watcher runs, as it was at the start. A build or an
/// update puts a new file at the path, and the running watcher keeps the old
/// code: its rules, such as who owns a row, fall behind the ledger. It then
/// runs the new file in its place, with the same arguments.
pub(super) struct Build {
    pub(super) path: PathBuf,
    pub(super) stamp: Stamp,
}

/// Device, inode and change time.
pub(super) type Stamp = (u64, u64, Option<std::time::SystemTime>);

impl Build {
    pub(super) fn now() -> Option<Build> {
        let path = std::env::current_exe().ok()?;
        let stamp = Self::stamp(&path)?;
        Some(Build { path, stamp })
    }

    #[cfg(unix)]
    pub(super) fn stamp(path: &Path) -> Option<Stamp> {
        use std::os::unix::fs::MetadataExt;
        let m = std::fs::metadata(path).ok()?;
        Some((m.dev(), m.ino(), m.modified().ok()))
    }

    /// Windows keeps a running file: no new build comes while it runs.
    #[cfg(not(unix))]
    pub(super) fn stamp(_: &Path) -> Option<Stamp> {
        None
    }

    /// Another file is at the path, written [`SETTLE`] ago or more.
    pub(super) fn replaced(&self) -> bool {
        Self::stamp(&self.path).is_some_and(|s| s != self.stamp && s.2.and_then(|t| t.elapsed().ok()).is_some_and(|age| age >= SETTLE))
    }

    /// Runs the new file in this process. The lock closes with the old
    /// program, and the new one takes it. Returns only on a failure.
    #[cfg(unix)]
    fn take_over(&self) -> std::io::Error {
        use std::os::unix::process::CommandExt;
        std::process::Command::new(&self.path).args(std::env::args_os().skip(1)).exec()
    }

    #[cfg(not(unix))]
    fn take_over(&self) -> std::io::Error {
        std::io::Error::other("not on this system")
    }
}

/// One connection: a step on each event and each tick.
pub(super) async fn follow(c: &Client, o: &Options, ws: &mut Watchers, build: &mut Option<Build>) -> Result<i32> {
    // Listen before the first read, so no change falls between the two.
    let mut events = c.events();
    let mut tick = interval(TICK);
    tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let note = |ws: &mut Watchers, e: &Event| ws.values_mut().for_each(|w| w.note(e));
    loop {
        if let Some(b) = build.as_ref().filter(|b| b.replaced()) {
            log(format!("{} is a new build: it takes over", b.path.display()));
            let e = b.take_over();
            log(format!("cannot run the new build: {e}"));
            *build = None;
        }
        if let Some(code) = step(c, o, ws).await? {
            return Ok(code);
        }
        tokio::select! {
            e = events.recv() => {
                match e {
                    Err(RecvError::Closed) => bail!("skiffd closed the connection"),
                    Ok(e) => note(ws, &e),
                    Err(RecvError::Lagged(_)) => {}
                }
                // One step reads everything, so a burst counts once.
                loop {
                    match events.try_recv() {
                        Ok(e) => note(ws, &e),
                        Err(TryRecvError::Lagged(_)) => continue,
                        Err(TryRecvError::Closed) => bail!("skiffd closed the connection"),
                        Err(TryRecvError::Empty) => break,
                    }
                }
            }
            _ = tick.tick() => {}
        }
    }
}

/// The live coordinators, each with its owner: the grand Skipper first, then
/// each project Skipper.
pub(super) fn targets(all: &[SessionInfo]) -> Vec<(String, &SessionInfo)> {
    let live = |s: &&SessionInfo| s.state != SessionState::Done;
    let grand = all.iter().filter(live).find(|s| s.coordinator).map(|s| (ledger::GRAND.to_string(), s));
    let projects = all.iter().filter(live).filter(|s| !s.coordinator).filter_map(|s| s.skipper.clone().map(|p| (p, s)));
    grand.into_iter().chain(projects).collect()
}

/// Reads the ledger and the session list, tells what is new, and delivers
/// each coordinator's held lines when it can take them. Each line goes to
/// the coordinator that owns the task. `Some` stops the watcher.
pub(super) async fn step(c: &Client, o: &Options, ws: &mut Watchers) -> Result<Option<i32>> {
    let all = sessions::sorted(c).await?;
    let targets = targets(&all);
    if targets.is_empty() {
        log("no Skipper runs");
        return Ok(Some(0));
    }
    // A coordinator that ended starts over when one comes back.
    ws.retain(|k, w| targets.iter().any(|(o, s)| o == k && w.skipper.as_deref() == Some(s.id.as_str())));
    for (owner, s) in &targets {
        ws.entry(owner.clone()).or_default().skipper = Some(s.id.clone());
    }
    // Each coordinator's own watcher registers on its own session, so the
    // app shows each Skipper's row from its own registration.
    for (owner, s) in &targets {
        ws.get_mut(owner).expect("added above").register(c, &s.id).await?;
    }
    // A read that fails (the file busy past its timeout) skips this step: the
    // next tick reads again. An empty list would forget the open panes.
    let here = skiff_core::state::socket();
    match ledger::close_gone(&here, &all) {
        Ok(closed) => {
            for r in closed {
                log(format!("task {} is done: its pane {} is gone", r.uid, print::short_id(r.pane_id.as_deref().unwrap_or(""))));
            }
        }
        Err(e) => log(format!("cannot close the tasks of gone panes: {e:#}")),
    }
    let tasks = match ledger::open_tasks() {
        Ok(t) => {
            let (mine, line) = on_socket(t, &here);
            let w = ws.values_mut().next().expect("a target");
            if line != w.elsewhere {
                if !line.is_empty() {
                    log(&line);
                }
                w.elsewhere = line;
            }
            mine
        }
        Err(e) => {
            log(format!("cannot read the ledger: {e:#}"));
            for w in ws.values_mut() {
                w.after_ledger(Err(&format!("{e:#}")));
            }
            for (owner, s) in &targets {
                ws.get_mut(owner).expect("added above").register(c, &s.id).await?;
            }
            return Ok(None);
        }
    };
    let gone = match ledger::gone_unreported() {
        Ok(rows) => on_socket(rows, &here).0,
        Err(e) => {
            log(format!("cannot read the tasks of gone panes: {e:#}"));
            Vec::new()
        }
    };
    let handed = match ledger::handed_back() {
        Ok(rows) => rows,
        Err(e) => {
            log(format!("cannot read the reported hand-offs: {e:#}"));
            Vec::new()
        }
    };
    if targets.iter().any(|(o, _)| skiff_client::skipper::scope_fleet(o).is_some()) {
        match skiff_core::config::load() {
            Ok(cfg) => {
                for (owner, _) in &targets {
                    let Some(fleet) = skiff_client::skipper::scope_fleet(owner) else { continue };
                    let now = skiff_client::skipper::fleet_projects(&cfg, fleet).into_iter().map(|p| p.name.clone()).collect();
                    if ws.get_mut(owner).expect("added above").fleet_is(fleet, now) {
                        if let Err(e) = skiff_client::skipper::write_fleet(&cfg, fleet) {
                            log(format!("cannot write the {fleet} Skipper's SKIPPER.md: {e:#}"));
                        }
                    }
                }
            }
            Err(e) => log(format!("cannot read the fleets: {e:#}")),
        }
    }
    let coordinators: Vec<&str> = targets.iter().map(|(_, s)| s.id.as_str()).collect();
    if let Some(w) = ws.get_mut(ledger::GRAND).filter(|w| !w.counted) {
        match ledger::unreported(ledger::GRAND) {
            Ok(n) => w.count_unreported(n),
            Err(e) => log(format!("cannot count the done tasks not reported: {e:#}")),
        }
    }
    let plan_rows = match ledger::plan_tasks() {
        Ok(mut rows) => {
            for r in &mut rows {
                r.elsewhere = r.socket_other_than(&here).is_some();
            }
            rows
        }
        Err(e) => {
            log(format!("cannot read the plans: {e:#}"));
            Vec::new()
        }
    };
    let told = route(ws, &targets, &tasks, &gone, &handed, &plan_rows, &all);
    for (owner, wake) in told {
        let to = if owner == ledger::GRAND { String::new() } else { format!(" (to the {owner} Skipper)") };
        let plan = match ws.get(&owner).and_then(|w| w.waits_for(&wake)) {
            Some(p) => format!(" (held for plan {p})"),
            None => String::new(),
        };
        log(format!("{} {} {}{}{to}{plan}", wake.phrase(), print::short_id(&wake.id), wake.name, wake.tail()));
        if wake.fact == Fact::Waiting && o.notify {
            let title = format!("{} ({}) is waiting", wake.name, print::short_id(&wake.id));
            desktop_notify(&title, "It needs you. Answer it in its pane.");
        }
    }
    // Each watcher reports what it follows, its own coordinator's panes,
    // on its own coordinator's registration.
    for (owner, s) in &targets {
        let w = ws.get_mut(owner).expect("added above");
        w.register(c, &s.id).await?;
        deliver(c, w, owner, s, &all, &coordinators, &here).await?;
        w.register(c, &s.id).await?;
    }
    Ok(None)
}

/// Gives each coordinator's watcher the rows it owns and the panes it
/// follows, and returns what each one tells, by owner. The grand Skipper
/// alone hears of a hand-off a project Skipper reported.
pub(super) fn route(
    ws: &mut Watchers,
    targets: &[(String, &SessionInfo)],
    tasks: &[ledger::Row],
    gone: &[ledger::Row],
    handed: &[ledger::Row],
    plan_rows: &[ledger::Row],
    all: &[SessionInfo],
) -> Vec<(String, Wake)> {
    let coordinators: Vec<&str> = targets.iter().map(|(_, s)| s.id.as_str()).collect();
    let owned = |rows: &[ledger::Row], owner: &str| rows.iter().filter(|r| r.owner == owner).cloned().collect::<Vec<_>>();
    let mut told = Vec::new();
    for (owner, _) in targets {
        let w = ws.entry(owner.clone()).or_default();
        w.after_ledger(Ok(()));
        let mut wakes = w.closed(&owned(gone, owner));
        if owner == ledger::GRAND {
            wakes.extend(w.handed(handed, all));
        }
        let mine = owned(tasks, owner);
        let (groups, interrupted) = group(&mine, all, &coordinators);
        w.follows = Some(follows(&groups));
        wakes.extend(w.take(&groups, &interrupted));
        w.plans_are(&groups, &mine, &owned(plan_rows, owner), all);
        told.extend(wakes.into_iter().map(|wk| (owner.clone(), wk)));
    }
    told
}

/// Types the held lines of `owner`'s watcher into its coordinator `skipper`
/// when it can take them. The start line goes alone, before any wake. A
/// refused one waits.
async fn deliver(c: &Client, w: &mut Watcher, owner: &str, skipper: &SessionInfo, all: &[SessionInfo], coordinators: &[&str], here: &Path) -> Result<()> {
    if let Some(line) = w.unreported.clone() {
        if ready(c, skipper).await {
            let d = submit(c, skipper, &line).await?;
            if d != Delivery::Refused {
                w.unreported = None;
            }
            let msg = report(d, &line);
            log(&msg);
            w.after_delivery(d, &msg);
        }
        return Ok(());
    }
    // A takeover or a rename left lines for this Skipper in the ledger.
    let file = skiff_client::ledger::path()?;
    let notes = skiff_client::ledger::notes(&file, owner).unwrap_or_else(|e| {
        log(format!("cannot read the notes: {e:#}"));
        Vec::new()
    });
    if (!w.lines.is_empty() || !notes.is_empty()) && ready(c, skipper).await {
        let text: Vec<&str> = w.lines.iter().map(String::as_str).chain(notes.iter().map(|n| n.1.as_str())).collect();
        let line = format!("[watch] {}", text.join(" "));
        let d = submit(c, skipper, &line).await?;
        if d != Delivery::Refused {
            w.lines.clear();
            let ids: Vec<i64> = notes.iter().map(|n| n.0).collect();
            if let Err(e) = skiff_client::ledger::drop_notes(&file, &ids) {
                log(format!("cannot drop the notes: {e:#}"));
            }
        }
        let msg = report(d, &line);
        log(&msg);
        w.after_delivery(d, &msg);
        return Ok(());
    }
    if w.ready_line().is_some() && ready(c, skipper).await {
        // The check above took time. Read the rows again, so a task sent
        // meanwhile is not named.
        match ledger::open_tasks() {
            Ok(tasks) => {
                let mine: Vec<ledger::Row> = on_socket(tasks, here).0.into_iter().filter(|r| r.owner == owner).collect();
                w.drop_stale(&group(&mine, all, coordinators).0);
            }
            Err(e) => {
                log(format!("cannot read the ledger: {e:#}"));
                return Ok(());
            }
        }
        let Some(line) = w.ready_line() else { return Ok(()) };
        let d = submit(c, skipper, &line).await?;
        let msg = w.delivered(d, &line);
        log(&msg);
        w.after_delivery(d, &msg);
    }
    Ok(())
}

/// Types `line` into the agent's pane and presses Enter. The check before it
/// read the screen. The daemon refuses the line if the person typed after
/// the list this step read.
pub(super) async fn submit(c: &Client, skipper: &SessionInfo, line: &str) -> Result<Delivery> {
    let events = c.events();
    Ok(if !c.submit(&skipper.id, line.as_bytes().to_vec(), true, true, skipper.input_gen).await? {
        Delivery::Refused
    } else if sessions::turn_started(c, &skipper.id, events).await? {
        Delivery::Taken
    } else {
        Delivery::NoTurn
    })
}

/// The agent can take a line: it is idle, an agent Skiff knows is in front,
/// and its input box shows and is empty. A shell in front, or a draft, holds
/// the line.
pub(super) async fn ready(c: &Client, s: &SessionInfo) -> bool {
    s.state == SessionState::Idle && sessions::is_agent(s) && read_front(c, s).await.ok() == Some(Front::Ready)
}

