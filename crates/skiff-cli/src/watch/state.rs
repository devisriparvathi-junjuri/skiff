//! The watcher state machine: panes grouped by task, held lines, plans, the failure, and the registration with skiffd.

use super::*;

#[derive(Default)]
pub(super) struct Watcher {
    /// The agent's full session id, once resolved.
    pub(super) skipper: Option<String>,
    pub(super) panes: HashMap<String, Pane>,
    pub(super) held: Vec<Wake>,
    /// Sessions the daemon marked unread since the last step.
    pub(super) turned: HashSet<String>,
    /// Sessions that showed a prompt since the last step.
    prompted: HashSet<String>,
    /// Sessions that worked or showed a prompt since the last step.
    pub(super) busy: HashSet<String>,
    /// The failure to show in the app, sent with `Watching`.
    pub(super) failure: Option<WatchFailure>,
    /// Failed ledger reads in a row.
    bad_reads: u32,
    /// The live panes with an open task, from the last good ledger read.
    /// `None` before the first one.
    pub(super) follows: Option<Vec<String>>,
    /// What this connection registered last. `None`: nothing yet.
    pub(super) sent: Option<(Option<WatchFailure>, Option<Vec<String>>)>,
    /// skiffd refused `Watching` on this connection: it is older than 27.
    pub(super) unregistered: bool,
    /// What the log last said of tasks on other sockets.
    pub(super) elsewhere: String,
    /// The done tasks not reported were counted in this run.
    pub(super) counted: bool,
    /// The line that names them, until the agent takes it.
    pub(super) unreported: Option<String>,
    /// The tasks closed for a gone pane and not reported, as last read.
    /// `None` before the first read: those are in the count.
    pub(super) gone: Option<HashSet<String>>,
    /// The hand-offs project Skippers set done and not reported, as last
    /// read. `None` before the first read: those are in the count.
    pub(super) handed: Option<HashSet<String>>,
    /// A fleet Skipper's projects, as last read. `None` before the first
    /// read, and for any other Skipper.
    pub(super) fleet: Option<Vec<String>>,
    /// Lines of the watcher's own, such as a fleet change, until the agent
    /// takes them.
    pub(super) lines: Vec<String>,
    /// The plan of each pane whose open tasks are all `sent` and in one plan,
    /// by pane key, as last read. Its idle line waits for the plan.
    plan_of: HashMap<String, String>,
    /// Each plan in `plan_of`: how many of its tasks ended, of how many.
    pub(super) plans: HashMap<String, (usize, usize)>,
}

impl Watcher {
    /// Notes that the fleet `fleet` has `now` for projects. A change since
    /// the last read holds one line. True on a change: the caller writes
    /// `SKIPPER.md` again.
    pub(super) fn fleet_is(&mut self, fleet: &str, now: Vec<String>) -> bool {
        let Some(old) = self.fleet.replace(now.clone()) else { return false };
        if old == now {
            return false;
        }
        self.lines.push(fleet_line(fleet, &old, &now));
        true
    }
}

/// The line for a fleet that changed: who joined, who left.
fn fleet_line(fleet: &str, old: &[String], now: &[String]) -> String {
    let joined: Vec<&str> = now.iter().filter(|p| !old.contains(p)).map(String::as_str).collect();
    let left: Vec<&str> = old.iter().filter(|p| !now.contains(p)).map(String::as_str).collect();
    let mut parts = Vec::new();
    if !joined.is_empty() {
        parts.push(format!("{} joined", joined.join(", ")));
    }
    if !left.is_empty() {
        parts.push(format!("{} left", left.join(", ")));
    }
    format!("{fleet} changed: {}. Read SKIPPER.md again.", parts.join(", "))
}

/// A task in a plan ended: it is `done` or `blocked`, or it is `sent` and
/// its pane ended its turn, exited, restarted or is gone. A pending task, a
/// pane that works or waits, a hand-off not done and a task on another
/// daemon still run. `r.elsewhere` marks a task on another daemon.
pub(super) fn ended(r: &ledger::Row, all: &[SessionInfo]) -> bool {
    match r.state.as_str() {
        "done" | "blocked" => true,
        "sent" if !r.is_handoff() && !r.elsewhere => {
            let s = r.pane_id.as_deref().and_then(|p| resolve::session(all, p).ok());
            r.interrupted_in(s) || !matches!(Now::of(s, false), Now::Working | Now::Waiting)
        }
        _ => false,
    }
}

/// The plan of the pane `g`: the one plan that holds all its open tasks,
/// when each is `sent`. A pending or blocked task, or a task with no plan,
/// keeps the pane's lines its own.
fn plan_of(g: &Group, tasks: &[ledger::Row], plans: &HashSet<&str>) -> Option<String> {
    let mut rows = tasks.iter().filter(|t| g.open.contains(&t.uid));
    let first = rows.next()?;
    let plan = first.parent.as_deref().filter(|p| plans.contains(p))?;
    (first.state == "sent" && rows.all(|t| t.state == "sent" && t.parent.as_deref() == Some(plan))).then(|| plan.to_string())
}

/// The line for a plan whose tasks all ended, with the panes whose idle
/// lines waited for it.
fn plan_line(plan: &str, (ended, total): (usize, usize), wakes: &[&Wake]) -> String {
    let tasks = if total == 1 { "task" } else { "tasks" };
    let panes: Vec<String> = wakes
        .iter()
        .map(|w| {
            let who = if w.name.is_empty() { String::new() } else { format!("{} ", w.name) };
            format!("pane {who}{}", print::short_id(&w.id))
        })
        .collect();
    let seen = if panes.len() == 1 { "mark its pane seen" } else { "mark each pane seen" };
    format!(
        "plan {plan}: {ended} of {total} {tasks} ended: {}. Read the results, {seen} and set each task done, then run skiff ledger report {plan}.",
        panes.join(", ")
    )
}

/// The open tasks whose panes are on `here`, and a log line for the others.
/// A pane id names a pane only on its own daemon: a task on another socket
/// is never gone from here, and never reassigned from here.
pub(super) fn on_socket(tasks: Vec<ledger::Row>, here: &Path) -> (Vec<ledger::Row>, String) {
    let (others, mine): (Vec<_>, Vec<_>) = tasks.into_iter().partition(|t| t.socket_other_than(here).is_some());
    let mut by: Vec<(String, Vec<&str>)> = Vec::new();
    for t in &others {
        let s = t.socket.clone().unwrap_or_default();
        match by.iter_mut().find(|(k, _)| *k == s) {
            Some((_, uids)) => uids.push(&t.uid),
            None => by.push((s, vec![&t.uid])),
        }
    }
    let line = by.iter().map(|(s, uids)| format!("tasks {} belong to {s}; not followed here", uids.join(", "))).collect::<Vec<_>>().join("; ");
    (mine, line)
}

/// What came of a line typed into the agent's pane.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum Delivery {
    /// A turn started: the agent took it.
    Taken,
    /// The person typed in the pane after the check. Nothing was typed.
    Refused,
    /// Typed and Enter pressed, but no turn started.
    NoTurn,
}

/// What to log of a delivery.
pub(super) fn report(d: Delivery, line: &str) -> String {
    match d {
        Delivery::Refused => format!("held: the person typed in the agent's pane: {line}"),
        Delivery::Taken => format!("woke the agent: {line}"),
        Delivery::NoTurn => format!("undelivered: no turn started; the line can be in the agent's input box: {line}"),
    }
}

/// The open rows that name one live pane, or one pane that no session
/// resolves to.
pub(super) struct Group<'a> {
    /// The resolved session id, else the ledger's pane value.
    key: String,
    pub(super) s: Option<&'a SessionInfo>,
    /// Its pending uids, oldest first.
    pub(super) pending: Vec<String>,
    /// All its open uids, oldest first: what a gone pane leaves open.
    pub(super) open: Vec<String>,
}

/// The open rows by pane, and the interrupted tasks apart. Rows that resolve
/// to one session (a full id and an imported prefix) make one group. The
/// coordinators' own panes are skipped: a hand-off at work is followed by
/// its row, not its pane. A pending hand-off is not: its project Skipper's
/// idle pane names it.
pub(super) fn group<'a>(tasks: &'a [ledger::Row], all: &'a [SessionInfo], coordinators: &[&str]) -> (Vec<Group<'a>>, Vec<(&'a str, &'a SessionInfo)>) {
    let (mut groups, mut interrupted) = (Vec::<Group>::new(), Vec::new());
    for t in tasks {
        let Some(pane) = t.pane_id.as_deref() else { continue };
        let s = resolve::session(all, pane).ok();
        if s.is_some_and(|s| coordinators.contains(&s.id.as_str())) && !(t.is_handoff() && t.state == "pending") {
            continue;
        }
        if let Some(s) = s.filter(|s| t.interrupted_in(Some(s))) {
            interrupted.push((t.uid.as_str(), s));
            continue;
        }
        let key = s.map_or(pane, |s| s.id.as_str());
        let i = match groups.iter().position(|g| g.key == key) {
            Some(i) => i,
            None => {
                groups.push(Group { key: key.to_string(), s, pending: Vec::new(), open: Vec::new() });
                groups.len() - 1
            }
        };
        if t.state == "pending" {
            groups[i].pending.push(t.uid.clone());
        }
        groups[i].open.push(t.uid.clone());
    }
    (groups, interrupted)
}

/// The live panes the groups follow, sorted: what the app shows as work to
/// watch. A gone pane and an interrupted task are told once, not followed.
pub(super) fn follows(groups: &[Group]) -> Vec<String> {
    let mut ids: Vec<String> = groups.iter().filter_map(|g| g.s.map(|s| s.id.clone())).collect();
    ids.sort_unstable();
    ids
}

impl Watcher {
    /// The app marks a pane seen as soon as the person looks at it, so the
    /// unread flag can be gone by the next list. The daemon's turn-end event
    /// keeps it. A rename, a theme or a resize is no turn. A prompt or work
    /// between two lists shows only in the state events, so they are kept
    /// too. The list stays the authority for what the pane is now.
    pub(super) fn note(&mut self, e: &Event) {
        match e {
            Event::TurnEnded { session } => {
                self.turned.insert(session.clone());
            }
            Event::State { session, state: SessionState::Waiting } => {
                self.prompted.insert(session.clone());
                self.busy.insert(session.clone());
            }
            Event::State { session, state: SessionState::Working } => {
                self.busy.insert(session.clone());
            }
            _ => {}
        }
    }

    /// Takes the state of the pane `key` now, with its pending uids, oldest
    /// first. Returns the fact to tell, if any, and holds a wake for the
    /// agent unless the pane waits. An idle pane with a pending task is told
    /// once per idle stretch with the oldest uid, in place of a plain idle.
    pub(super) fn track(&mut self, key: &str, s: Option<&SessionInfo>, pending: &[String]) -> Option<Wake> {
        let pane = self.panes.entry(key.to_string()).or_default();
        if let Some(s) = s {
            pane.name = resolve::display_name(s).to_string();
            // A restore runs a new program under the old id. Also across a
            // reconnect: what this run saw of the old one does not count.
            let generation = s.generation();
            if pane.generation.is_some() && pane.generation != generation {
                pane.waited = false;
                pane.pending_told = None;
            }
            pane.generation = generation;
        }
        let seen = |set: &HashSet<String>| s.is_some_and(|s| set.contains(&s.id));
        let turned = seen(&self.turned);
        // A new turn ended: tell it, though the last idle was told.
        if turned && pane.told == Some(Fact::Idle) {
            pane.told = None;
        }
        if seen(&self.prompted) {
            pane.waited = true;
        }
        let now = Now::of(s, turned);
        // An idle line is old news once the pane works or waits again, and
        // the next idle stretch tells the pending task again.
        if seen(&self.busy) || matches!(now, Now::Working | Now::Waiting) {
            pane.pending_told = None;
            self.held.retain(|h| !(h.pane == key && matches!(h.fact, Fact::Idle | Fact::Pending)));
        }
        // The told task was sent, closed or removed: the next one can go.
        if pane.pending_told.as_ref().is_some_and(|u| !pending.contains(u)) {
            pane.pending_told = None;
        }
        let mut fact = pane.observe(now);
        let mut task = None;
        if let (Now::Idle { .. }, Some(uid)) = (now, pending.first()) {
            if pane.pending_told.as_ref() != Some(uid) {
                pane.pending_told = Some(uid.clone());
                pane.waited = false;
                // It covers the turn too: no plain idle line after it.
                pane.told = Some(Fact::Idle);
                fact = Some(Fact::Pending);
                task = Some(uid.clone());
            } else if fact == Some(Fact::Idle) {
                // A late turn end in the same idle stretch.
                fact = None;
            }
        }
        let fact = fact?;
        let id = s.map(|s| s.id.clone()).unwrap_or_else(|| key.to_string());
        // The program said its turn ended in an error: say so, not idle.
        let failed = fact == Fact::Idle
            && s.and_then(|s| s.program_status.as_ref()).is_some_and(|p| p.state == "error");
        let wake = Wake { fact, pane: key.to_string(), name: pane.name.clone(), id, task, failed };
        if fact != Fact::Waiting {
            // The newest fact for a pane replaces an older one still held.
            self.held.retain(|h| h.pane != key);
            self.held.push(wake.clone());
        }
        Some(wake)
    }

    /// Takes a task whose pane restarted since the send. It is followed by
    /// its uid, not its pane: the pane's new program is not the task. Told
    /// once per run.
    pub(super) fn interrupted(&mut self, uid: &str, s: &SessionInfo) -> Option<Wake> {
        let key = format!("task:{uid}");
        let pane = self.panes.entry(key.clone()).or_default();
        pane.name = resolve::display_name(s).to_string();
        let fact = pane.observe(Now::Interrupted)?;
        let wake = Wake { fact, pane: key, name: pane.name.clone(), id: s.id.clone(), task: Some(uid.to_string()), failed: false };
        self.held.push(wake.clone());
        Some(wake)
    }

    /// One step's read of the rows: tracks each pane and each interrupted
    /// task, then settles the held lines. Returns the facts to log.
    pub(super) fn take(&mut self, groups: &[Group], interrupted: &[(&str, &SessionInfo)]) -> Vec<Wake> {
        let mut open = HashSet::new();
        let mut wakes = Vec::new();
        for (uid, s) in interrupted {
            open.insert(format!("task:{uid}"));
            wakes.extend(self.interrupted(uid, s));
        }
        for g in groups {
            open.insert(g.key.clone());
            let mut wake = self.track(&g.key, g.s, &g.pending);
            // A gone pane cannot be marked seen: the line names its open tasks.
            if let Some(w) = wake.as_mut().filter(|w| w.fact == Fact::Gone && !g.open.is_empty()) {
                w.task = Some(g.open.join(", "));
                if let Some(h) = self.held.iter_mut().find(|h| h.pane == g.key) {
                    h.task = w.task.clone();
                }
            }
            wakes.extend(wake);
        }
        self.turned.clear();
        self.prompted.clear();
        self.busy.clear();
        self.panes.retain(|k, _| open.contains(k));
        // A row the agent closed needs no line. One skiff closed, or a reported hand-off, does.
        self.held.retain(|h| matches!(h.fact, Fact::Closed | Fact::Reported) || open.contains(&h.pane));
        self.drop_stale(groups);
        wakes
    }

    /// Takes the tasks skiff closed because their panes are gone, not yet
    /// reported. Each one new since the last read holds one line per pane.
    /// The first read only notes them: the unreported count names them.
    pub(super) fn closed(&mut self, rows: &[ledger::Row]) -> Vec<Wake> {
        let now: HashSet<String> = rows.iter().map(|r| r.uid.clone()).collect();
        // Reported, or reassigned, since: the held line is old news.
        self.held.retain(|h| h.fact != Fact::Closed || h.task.as_deref().is_some_and(|t| t.split(", ").all(|u| now.contains(u))));
        let Some(known) = self.gone.replace(now) else { return Vec::new() };
        let mut wakes: Vec<Wake> = Vec::new();
        for r in rows.iter().filter(|r| !known.contains(&r.uid)) {
            let pane = r.pane_id.clone().unwrap_or_default();
            match wakes.iter_mut().find(|w| w.id == pane) {
                Some(w) => w.task = w.task.take().map(|t| format!("{t}, {}", r.uid)),
                None => wakes.push(Wake {
                    fact: Fact::Closed,
                    pane: format!("gone:{pane}"),
                    name: self.panes.get(&pane).map(|p| p.name.clone()).unwrap_or_default(),
                    id: pane,
                    task: Some(r.uid.clone()),
                    failed: false,
                }),
            }
        }
        for w in &wakes {
            self.held.retain(|h| h.pane != w.pane && h.pane != w.id);
            self.held.push(w.clone());
        }
        wakes
    }

    /// Takes the hand-offs project Skippers set done, not yet reported. Each
    /// one new since the last read holds one line. The first read only notes
    /// them: the unreported count names them.
    pub(super) fn handed(&mut self, rows: &[ledger::Row], all: &[SessionInfo]) -> Vec<Wake> {
        let now: HashSet<String> = rows.iter().map(|r| r.uid.clone()).collect();
        self.held.retain(|h| h.fact != Fact::Reported || h.task.as_deref().is_some_and(|t| now.contains(t)));
        let Some(known) = self.handed.replace(now) else { return Vec::new() };
        let wakes: Vec<Wake> = rows
            .iter()
            .filter(|r| !known.contains(&r.uid))
            .map(|r| Wake {
                fact: Fact::Reported,
                pane: format!("handoff:{}", r.uid),
                // The project Skipper's session name ("gesher Skipper"), as the sidebar shows it.
                name: all
                    .iter()
                    .find(|s| s.skipper.as_deref() == Some(r.project.as_str()) && s.state != SessionState::Done)
                    .map(|s| resolve::display_name(s).into_owned())
                    .unwrap_or_else(|| format!("{} Skipper", skiff_client::skipper::scope_fleet(&r.project).unwrap_or(&r.project))),
                id: r.pane_id.clone().unwrap_or_default(),
                task: Some(r.uid.clone()),
                failed: false,
            })
            .collect();
        self.held.extend(wakes.iter().cloned());
        wakes
    }

    /// Notes the plan of each pane in `groups`, and how far each such plan
    /// is, from `plan_rows`: the owner's tasks of the plans not closed.
    pub(super) fn plans_are(&mut self, groups: &[Group], tasks: &[ledger::Row], plan_rows: &[ledger::Row], all: &[SessionInfo]) {
        let ids: HashSet<&str> = plan_rows.iter().filter_map(|r| r.parent.as_deref()).collect();
        self.plan_of = groups.iter().filter_map(|g| plan_of(g, tasks, &ids).map(|p| (g.key.clone(), p))).collect();
        self.plans = self
            .plan_of
            .values()
            .map(|p| {
                let rows: Vec<&ledger::Row> = plan_rows.iter().filter(|r| r.parent.as_deref() == Some(p.as_str())).collect();
                (p.clone(), (rows.iter().filter(|r| ended(r, all)).count(), rows.len()))
            })
            .collect();
    }

    /// The plan a held line waits for: an idle line of a pane in a plan
    /// whose tasks have not all ended. `None`: the line can go.
    pub(super) fn waits_for(&self, h: &Wake) -> Option<&str> {
        let p = self.plan_held(h)?;
        self.plans.get(p).is_none_or(|(n, total)| n < total).then_some(p)
    }

    /// The plan whose line carries the held line `h`: an idle line, not a
    /// pending one, of a pane in a plan.
    fn plan_held(&self, h: &Wake) -> Option<&str> {
        (h.fact == Fact::Idle).then(|| self.plan_of.get(&h.pane).map(String::as_str)).flatten()
    }

    /// The one line for the held lines that can go now. A plan whose tasks
    /// all ended gives one part for its panes. `None`: every held line waits.
    pub(super) fn ready_line(&self) -> Option<String> {
        let mut plans: Vec<(&str, Vec<&Wake>)> = Vec::new();
        let mut rest: Vec<Wake> = Vec::new();
        for h in self.held.iter().filter(|h| self.waits_for(h).is_none()) {
            match self.plan_held(h) {
                Some(p) => match plans.iter_mut().find(|(k, _)| *k == p) {
                    Some((_, ws)) => ws.push(h),
                    None => plans.push((p, vec![h])),
                },
                None => rest.push(h.clone()),
            }
        }
        if plans.is_empty() && rest.is_empty() {
            return None;
        }
        let parts: Vec<String> = plans.iter().map(|(p, ws)| plan_line(p, self.plans[*p], ws)).collect();
        Some(if rest.is_empty() { format!("[watch] {}", parts.join(" ")) } else if parts.is_empty() { wake_line(&rest) } else { format!("{} {}", wake_line(&rest), parts.join(" ")) })
    }

    /// Drops a held pending line whose task is no longer pending on its pane
    /// in this ledger read.
    pub(super) fn drop_stale(&mut self, groups: &[Group]) {
        self.held.retain(|h| {
            h.fact != Fact::Pending
                || groups.iter().any(|g| g.key == h.pane && h.task.as_ref().is_some_and(|t| g.pending.contains(t)))
        });
    }

    /// Takes the count of done tasks not yet reported, once per run. Above
    /// 0 it holds one line that names the count and the command to list them.
    pub(super) fn count_unreported(&mut self, n: usize) {
        if std::mem::replace(&mut self.counted, true) || n == 0 {
            return;
        }
        let tasks = if n == 1 { "task" } else { "tasks" };
        self.unreported = Some(format!("[watch] {n} done {tasks} not reported: skiff ledger ls --unreported"));
    }

    /// Settles the held lines after a delivery, and says what to log. A line
    /// that started no turn can sit in the input box: Enter is not pressed
    /// again, as it could submit twice.
    pub(super) fn delivered(&mut self, d: Delivery, line: &str) -> String {
        if d != Delivery::Refused {
            let waits: Vec<bool> = self.held.iter().map(|h| self.waits_for(h).is_some()).collect();
            let mut waits = waits.into_iter();
            self.held.retain(|_| waits.next().unwrap_or(false));
        }
        report(d, line)
    }

    /// The failure after a delivery: an undelivered line sets one, a line
    /// taken clears it, and a refused line changes nothing.
    pub(super) fn after_delivery(&mut self, d: Delivery, msg: &str) {
        match d {
            Delivery::NoTurn => self.failure = Some(WatchFailure { kind: "undelivered".into(), message: msg.into(), at: now_ms() }),
            Delivery::Taken if self.failure.as_ref().is_some_and(|f| f.kind == "undelivered") => self.failure = None,
            Delivery::Taken | Delivery::Refused => {}
        }
    }

    /// The failure after a ledger read: [`LEDGER_FAILS`] bad reads in a row
    /// set one, a good read clears it.
    pub(super) fn after_ledger(&mut self, read: Result<(), &str>) {
        match read {
            Ok(()) => {
                self.bad_reads = 0;
                if self.failure.as_ref().is_some_and(|f| f.kind == "ledger") {
                    self.failure = None;
                }
            }
            Err(e) => {
                self.bad_reads += 1;
                if self.bad_reads >= LEDGER_FAILS && self.failure.as_ref().is_none_or(|f| f.kind == "ledger") {
                    self.failure = Some(WatchFailure { kind: "ledger".into(), message: e.into(), at: now_ms() });
                }
            }
        }
    }

    /// Tells skiffd this connection watches `skipper`, with the failure and
    /// the panes it follows, when that changed since the last report on this
    /// connection.
    pub(super) async fn register(&mut self, c: &Client, skipper: &str) -> Result<()> {
        let now = (self.failure.clone(), self.follows.clone());
        if self.unregistered || self.sent.as_ref() == Some(&now) {
            return Ok(());
        }
        let req = Request::Watching { session: skipper.to_string(), failure: now.0.clone(), follows: now.1.clone() };
        match c.request(req).await? {
            Response::Ok => self.sent = Some(now),
            Response::Error { message } if message.starts_with("bad request") => {
                log("skiffd is too old to show this watcher in the app");
                self.unregistered = true;
            }
            Response::Error { message } => log(format!("cannot register with skiffd: {message}")),
            other => log(format!("cannot register with skiffd: unexpected reply {other:?}")),
        }
        Ok(())
    }
}
