//! `skiff watch`: follows the panes of the open tasks in the Skiff agent's
//! ledger, for that agent. The app starts it with the agent's session and it
//! stops when that session ends. A pane that waits notifies the person. A
//! pane that goes idle after work or after a prompt, exits or is gone wakes
//! the agent: one line typed into its input box while it is idle and the box
//! is empty. The daemon types it only if the person typed nothing since that
//! check. An idle pane with a pending task wakes the agent once per idle
//! stretch, and the line names the oldest pending uid and the `skiff ledger
//! send` command for it. A task whose pane
//! restarted since the send (a restore runs a new shell under the old id)
//! wakes the agent once as interrupted. Plans have no pane and are not
//! followed, but a plan holds the idle lines of its tasks' panes: a pane
//! whose open tasks are all `sent` in one plan wakes the agent only when
//! every task in that plan ended, in one line for the plan (see [`ended`]).
//! Its other lines (exited, restarted, gone, pending) go at once. At its start, a run counts the done tasks not yet reported and,
//! if there are any, tells the agent once, in a line of its own. Each step
//! sets done the open tasks whose panes are gone (see
//! [`ledger::close_gone`]), and a task closed that way after the start wakes
//! the agent once, whichever command closed it.
//!
//! It keeps no state of its own. The ledger, the session list and each pane's
//! unread flag are the input, so a new run rebuilds the same view. One lock
//! file per daemon socket keeps it to one copy. When a build or an update
//! puts a new `skiff` at its path, the watcher runs the new one in its place.
//!
//! Best effort: the list, the ledger read and the submit are separate reads,
//! and the state events can lag. Held pending lines are checked against a
//! new ledger read just before the submit, and a line whose task left pending
//! is dropped. A row that changes after that read is not seen.

use std::{
    collections::{HashMap, HashSet},
    fs::File,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use alacritty_terminal::{
    grid::Dimensions,
    index::{Column, Line},
    term::cell::Flags,
};
use anyhow::{bail, Context, Result};
use skiff_client::Client;
use skiff_core::{
    protocol::{Event, Request, Response},
    session::{now_ms, SessionInfo, SessionState, WatchFailure},
};
use tokio::{
    sync::broadcast::error::{RecvError, TryRecvError},
    time::{interval, sleep, Instant, MissedTickBehavior},
};

use crate::{ledger, peek, print, resolve, sessions};

/// How often the ledger and the session list are read again without an event.
const TICK: Duration = Duration::from_secs(2);
/// How long the watcher waits for skiffd to come back: a reload, or a restart.
mod terminal;
mod model;
pub use model::{wake_line, Options};
pub use terminal::{busy_line, front, input_box, input_box_rows, read_front, rows_of, Front};
use terminal::desktop_notify;
use model::{Fact, Now, Pane, Wake};

const RECONNECT_LIMIT: Duration = Duration::from_secs(600);
/// How long a new watcher waits for the lock, so an old one can finish.
const LOCK_WAIT: Duration = Duration::from_secs(10);
/// Failed ledger reads in a row before the watcher reports a failure.
const LEDGER_FAILS: u32 = 3;
/// How old a new `skiff` file must be before the watcher runs it: a build
/// may still write it.
const SETTLE: Duration = Duration::from_secs(2);


#[derive(Default)]
struct Watcher {
    /// The agent's full session id, once resolved.
    skipper: Option<String>,
    panes: HashMap<String, Pane>,
    held: Vec<Wake>,
    /// Sessions the daemon marked unread since the last step.
    turned: HashSet<String>,
    /// Sessions that showed a prompt since the last step.
    prompted: HashSet<String>,
    /// Sessions that worked or showed a prompt since the last step.
    busy: HashSet<String>,
    /// The failure to show in the app, sent with `Watching`.
    failure: Option<WatchFailure>,
    /// Failed ledger reads in a row.
    bad_reads: u32,
    /// The live panes with an open task, from the last good ledger read.
    /// `None` before the first one.
    follows: Option<Vec<String>>,
    /// What this connection registered last. `None`: nothing yet.
    sent: Option<(Option<WatchFailure>, Option<Vec<String>>)>,
    /// skiffd refused `Watching` on this connection: it is older than 27.
    unregistered: bool,
    /// What the log last said of tasks on other sockets.
    elsewhere: String,
    /// The done tasks not reported were counted in this run.
    counted: bool,
    /// The line that names them, until the agent takes it.
    unreported: Option<String>,
    /// The tasks closed for a gone pane and not reported, as last read.
    /// `None` before the first read: those are in the count.
    gone: Option<HashSet<String>>,
    /// The hand-offs project Skippers set done and not reported, as last
    /// read. `None` before the first read: those are in the count.
    handed: Option<HashSet<String>>,
    /// A fleet Skipper's projects, as last read. `None` before the first
    /// read, and for any other Skipper.
    fleet: Option<Vec<String>>,
    /// Lines of the watcher's own, such as a fleet change, until the agent
    /// takes them.
    lines: Vec<String>,
    /// The plan of each pane whose open tasks are all `sent` and in one plan,
    /// by pane key, as last read. Its idle line waits for the plan.
    plan_of: HashMap<String, String>,
    /// Each plan in `plan_of`: how many of its tasks ended, of how many.
    plans: HashMap<String, (usize, usize)>,
}

impl Watcher {
    /// Notes that the fleet `fleet` has `now` for projects. A change since
    /// the last read holds one line. True on a change: the caller writes
    /// `SKIPPER.md` again.
    fn fleet_is(&mut self, fleet: &str, now: Vec<String>) -> bool {
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
fn ended(r: &ledger::Row, all: &[SessionInfo]) -> bool {
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
fn on_socket(tasks: Vec<ledger::Row>, here: &Path) -> (Vec<ledger::Row>, String) {
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
enum Delivery {
    /// A turn started: the agent took it.
    Taken,
    /// The person typed in the pane after the check. Nothing was typed.
    Refused,
    /// Typed and Enter pressed, but no turn started.
    NoTurn,
}

/// What to log of a delivery.
fn report(d: Delivery, line: &str) -> String {
    match d {
        Delivery::Refused => format!("held: the person typed in the agent's pane: {line}"),
        Delivery::Taken => format!("woke the agent: {line}"),
        Delivery::NoTurn => format!("undelivered: no turn started; the line can be in the agent's input box: {line}"),
    }
}

/// The open rows that name one live pane, or one pane that no session
/// resolves to.
struct Group<'a> {
    /// The resolved session id, else the ledger's pane value.
    key: String,
    s: Option<&'a SessionInfo>,
    /// Its pending uids, oldest first.
    pending: Vec<String>,
    /// All its open uids, oldest first: what a gone pane leaves open.
    open: Vec<String>,
}

/// The open rows by pane, and the interrupted tasks apart. Rows that resolve
/// to one session (a full id and an imported prefix) make one group. The
/// coordinators' own panes are skipped: a hand-off at work is followed by
/// its row, not its pane. A pending hand-off is not: its project Skipper's
/// idle pane names it.
fn group<'a>(tasks: &'a [ledger::Row], all: &'a [SessionInfo], coordinators: &[&str]) -> (Vec<Group<'a>>, Vec<(&'a str, &'a SessionInfo)>) {
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
fn follows(groups: &[Group]) -> Vec<String> {
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
    fn note(&mut self, e: &Event) {
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
    fn track(&mut self, key: &str, s: Option<&SessionInfo>, pending: &[String]) -> Option<Wake> {
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
    fn interrupted(&mut self, uid: &str, s: &SessionInfo) -> Option<Wake> {
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
    fn take(&mut self, groups: &[Group], interrupted: &[(&str, &SessionInfo)]) -> Vec<Wake> {
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
    fn closed(&mut self, rows: &[ledger::Row]) -> Vec<Wake> {
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
    fn handed(&mut self, rows: &[ledger::Row], all: &[SessionInfo]) -> Vec<Wake> {
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
    fn plans_are(&mut self, groups: &[Group], tasks: &[ledger::Row], plan_rows: &[ledger::Row], all: &[SessionInfo]) {
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
    fn waits_for(&self, h: &Wake) -> Option<&str> {
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
    fn ready_line(&self) -> Option<String> {
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
    fn drop_stale(&mut self, groups: &[Group]) {
        self.held.retain(|h| {
            h.fact != Fact::Pending
                || groups.iter().any(|g| g.key == h.pane && h.task.as_ref().is_some_and(|t| g.pending.contains(t)))
        });
    }

    /// Takes the count of done tasks not yet reported, once per run. Above
    /// 0 it holds one line that names the count and the command to list them.
    fn count_unreported(&mut self, n: usize) {
        if std::mem::replace(&mut self.counted, true) || n == 0 {
            return;
        }
        let tasks = if n == 1 { "task" } else { "tasks" };
        self.unreported = Some(format!("[watch] {n} done {tasks} not reported: skiff ledger ls --unreported"));
    }

    /// Settles the held lines after a delivery, and says what to log. A line
    /// that started no turn can sit in the input box: Enter is not pressed
    /// again, as it could submit twice.
    fn delivered(&mut self, d: Delivery, line: &str) -> String {
        if d != Delivery::Refused {
            let waits: Vec<bool> = self.held.iter().map(|h| self.waits_for(h).is_some()).collect();
            let mut waits = waits.into_iter();
            self.held.retain(|_| waits.next().unwrap_or(false));
        }
        report(d, line)
    }

    /// The failure after a delivery: an undelivered line sets one, a line
    /// taken clears it, and a refused line changes nothing.
    fn after_delivery(&mut self, d: Delivery, msg: &str) {
        match d {
            Delivery::NoTurn => self.failure = Some(WatchFailure { kind: "undelivered".into(), message: msg.into(), at: now_ms() }),
            Delivery::Taken if self.failure.as_ref().is_some_and(|f| f.kind == "undelivered") => self.failure = None,
            Delivery::Taken | Delivery::Refused => {}
        }
    }

    /// The failure after a ledger read: [`LEDGER_FAILS`] bad reads in a row
    /// set one, a good read clears it.
    fn after_ledger(&mut self, read: Result<(), &str>) {
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
    async fn register(&mut self, c: &Client, skipper: &str) -> Result<()> {
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

/// The refusal for a session that is not a Skipper's, the grand Skipper's or
/// a project Skipper's, if it is not. A daemon older than 27 may not mark the
/// coordinator: `coordinator_known` is false then, and any session passes.
fn not_the_agent(all: &[SessionInfo], skipper: &str, coordinator_known: bool) -> Option<String> {
    let s = resolve::session(all, skipper).ok()?;
    (coordinator_known && !s.coordinator && s.skipper.is_none()).then(|| format!("skiff watch: {skipper} is not a Skipper's session"))
}

fn log(msg: impl std::fmt::Display) {
    println!("{} {msg}", skiff_core::session::now_ms() / 1000);
}

/// Runs while a Skipper runs: the grand Skipper or a project Skipper. Exit
/// codes: 0 when none runs or another watcher runs; 1 when skiffd does not
/// come back; 2 when the session is not a Skipper's.
pub async fn watch(socket: &Path, o: Options) -> Result<i32> {
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
async fn lock(socket: &Path) -> Result<Option<File>> {
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
type Watchers = HashMap<String, Watcher>;

/// The `skiff` file this watcher runs, as it was at the start. A build or an
/// update puts a new file at the path, and the running watcher keeps the old
/// code: its rules, such as who owns a row, fall behind the ledger. It then
/// runs the new file in its place, with the same arguments.
struct Build {
    path: PathBuf,
    stamp: Stamp,
}

/// Device, inode and change time.
type Stamp = (u64, u64, Option<std::time::SystemTime>);

impl Build {
    fn now() -> Option<Build> {
        let path = std::env::current_exe().ok()?;
        let stamp = Self::stamp(&path)?;
        Some(Build { path, stamp })
    }

    #[cfg(unix)]
    fn stamp(path: &Path) -> Option<Stamp> {
        use std::os::unix::fs::MetadataExt;
        let m = std::fs::metadata(path).ok()?;
        Some((m.dev(), m.ino(), m.modified().ok()))
    }

    /// Windows keeps a running file: no new build comes while it runs.
    #[cfg(not(unix))]
    fn stamp(_: &Path) -> Option<Stamp> {
        None
    }

    /// Another file is at the path, written [`SETTLE`] ago or more.
    fn replaced(&self) -> bool {
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
async fn follow(c: &Client, o: &Options, ws: &mut Watchers, build: &mut Option<Build>) -> Result<i32> {
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
fn targets(all: &[SessionInfo]) -> Vec<(String, &SessionInfo)> {
    let live = |s: &&SessionInfo| s.state != SessionState::Done;
    let grand = all.iter().filter(live).find(|s| s.coordinator).map(|s| (ledger::GRAND.to_string(), s));
    let projects = all.iter().filter(live).filter(|s| !s.coordinator).filter_map(|s| s.skipper.clone().map(|p| (p, s)));
    grand.into_iter().chain(projects).collect()
}

/// Reads the ledger and the session list, tells what is new, and delivers
/// each coordinator's held lines when it can take them. Each line goes to
/// the coordinator that owns the task. `Some` stops the watcher.
async fn step(c: &Client, o: &Options, ws: &mut Watchers) -> Result<Option<i32>> {
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
fn route(
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
async fn submit(c: &Client, skipper: &SessionInfo, line: &str) -> Result<Delivery> {
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
async fn ready(c: &Client, s: &SessionInfo) -> bool {
    s.state == SessionState::Idle && sessions::is_agent(s) && read_front(c, s).await.ok() == Some(Front::Ready)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn idle_counts_after_work_or_while_unread_and_is_told_once() {
        let mut p = Pane::default();
        // Quiet and seen: nothing to tell.
        assert_eq!(p.observe(Now::Idle { unread: false }), None);
        // A start-up screen: work with no turn marked.
        assert_eq!(p.observe(Now::Working), None);
        assert_eq!(p.observe(Now::Idle { unread: false }), None);
        assert_eq!(p.observe(Now::Waiting), Some(Fact::Waiting));
        assert_eq!(p.observe(Now::Waiting), None);
        assert_eq!(p.observe(Now::Working), None);
        assert_eq!(p.observe(Now::Idle { unread: true }), Some(Fact::Idle));
        assert_eq!(p.observe(Now::Idle { unread: true }), None);
        // Marked seen: nothing new until it works again.
        assert_eq!(p.observe(Now::Idle { unread: false }), None);
        // A new run: unread output still needs attention.
        assert_eq!(Pane::default().observe(Now::Idle { unread: true }), Some(Fact::Idle));
        assert_eq!(p.observe(Now::Exit), Some(Fact::Exit));
        assert_eq!(p.observe(Now::Gone), Some(Fact::Gone));
        assert_eq!(p.observe(Now::Gone), None);
    }

    #[test]
    fn held_wakes_make_one_line() {
        let w = |fact, name: &str, id: &str| Wake { fact, pane: id.into(), name: name.into(), id: id.into(), task: None, failed: false };
        assert_eq!(wake_line(&[w(Fact::Idle, "api", "abc12345xyz")]), "[watch] pane api abc12345 is idle. Read it, then mark it seen.");
        assert_eq!(
            wake_line(&[w(Fact::Idle, "api", "abc12345"), w(Fact::Gone, "", "def67890")]),
            "[watch] pane api abc12345 is idle; pane def67890 is gone. Read them, then mark each seen."
        );
        // A gone pane with open tasks: they are reassigned or marked done, not marked seen.
        let gone = |id: &str, t: &str| Wake { fact: Fact::Gone, pane: id.into(), name: String::new(), id: id.into(), task: Some(t.into()), failed: false };
        assert_eq!(
            wake_line(&[gone("4ac9e7a1", "d0e7b2f4")]),
            "[watch] pane 4ac9e7a1 is gone: task d0e7b2f4 is open. Reassign it or mark it done."
        );
        assert_eq!(
            wake_line(&[gone("4ac9e7a1", "d0e7b2f4, 5b1c9e2a")]),
            "[watch] pane 4ac9e7a1 is gone: tasks d0e7b2f4, 5b1c9e2a are open. Reassign each task or mark it done."
        );
        assert_eq!(
            wake_line(&[w(Fact::Idle, "api", "abc12345"), gone("4ac9e7a1", "d0e7b2f4")]),
            "[watch] pane api abc12345 is idle; pane 4ac9e7a1 is gone: task d0e7b2f4 is open. Read the others, then mark each seen. Reassign each task of a gone pane or mark it done."
        );
        let mut w = Watcher::default();
        let s = info("abc12345xyz", "idle", true);
        assert!(w.interrupted("3f9a01bc", &s).is_some());
        // Once per run, however long the row stays open.
        assert!(w.interrupted("3f9a01bc", &s).is_none());
        assert_eq!(wake_line(&w.held), "[watch] pane abc12345xyz abc12345 restarted: task 3f9a01bc is interrupted. Read it, then mark it seen.");
    }

    fn info(id: &str, state: &str, unread: bool) -> SessionInfo {
        serde_json::from_value(serde_json::json!({
            "id": id, "label": id, "role": "agent", "cwd": "/", "command": "claude", "args": [],
            "state": state, "cols": 80, "rows": 24, "pid": null, "exit_code": null, "unread": unread,
        }))
        .unwrap()
    }

    #[test]
    fn deliveries_and_ledger_reads_set_and_clear_the_failure() {
        let mut w = Watcher::default();
        w.after_delivery(Delivery::Refused, "held");
        assert!(w.failure.is_none());
        w.after_delivery(Delivery::NoTurn, "undelivered: x");
        assert_eq!(w.failure.as_ref().map(|f| (f.kind.as_str(), f.message.as_str())), Some(("undelivered", "undelivered: x")));
        w.after_delivery(Delivery::Refused, "held");
        assert_eq!(w.failure.as_ref().map(|f| f.kind.as_str()), Some("undelivered"));
        w.after_delivery(Delivery::Taken, "woke");
        assert!(w.failure.is_none());

        w.after_ledger(Err("busy"));
        w.after_ledger(Err("busy"));
        assert!(w.failure.is_none());
        w.after_ledger(Err("busy"));
        assert_eq!(w.failure.as_ref().map(|f| (f.kind.as_str(), f.message.as_str())), Some(("ledger", "busy")));
        w.after_ledger(Ok(()));
        assert!(w.failure.is_none());
        // A good read starts the count again.
        w.after_ledger(Err("busy"));
        w.after_ledger(Err("busy"));
        assert!(w.failure.is_none());
    }

    #[test]
    fn the_first_step_names_the_unreported_count_once() {
        let mut w = Watcher::default();
        w.count_unreported(4);
        assert_eq!(w.unreported.as_deref(), Some("[watch] 4 done tasks not reported: skiff ledger ls --unreported"));
        // Once per run: a later count, or a new connection, adds nothing.
        w.unreported = None;
        w.count_unreported(5);
        assert_eq!(w.unreported, None);
        let mut w = Watcher::default();
        w.count_unreported(0);
        assert!(w.counted && w.unreported.is_none());
        let mut w = Watcher::default();
        w.count_unreported(1);
        assert_eq!(w.unreported.as_deref(), Some("[watch] 1 done task not reported: skiff ledger ls --unreported"));
    }

    #[test]
    fn a_fleet_change_holds_one_line_after_the_first_read() {
        let mut w = Watcher::default();
        assert!(!w.fleet_is("Harbor", vec!["api".into(), "web".into()]));
        assert!(!w.fleet_is("Harbor", vec!["api".into(), "web".into()]));
        assert!(w.lines.is_empty());
        assert!(w.fleet_is("Harbor", vec!["web".into(), "cli".into()]));
        assert_eq!(w.lines, ["Harbor changed: cli joined, api left. Read SKIPPER.md again."]);
    }

    #[test]
    fn a_watch_line_goes_to_the_coordinator_that_owns_the_task() {
        let mut grand = info("aaaa1111", "idle", false);
        grand.coordinator = true;
        let mut gesher = info("bbbb2222", "working", false);
        gesher.skipper = Some("gesher".into());
        gesher.name = Some("gesher Skipper".into());
        let all = [grand, gesher, info("cccc3333", "idle", true), info("dddd4444", "idle", true)];
        let targets = targets(&all);
        assert_eq!(targets.iter().map(|(o, s)| (o.as_str(), s.id.as_str())).collect::<Vec<_>>(), [("grand", "aaaa1111"), ("gesher", "bbbb2222")]);
        // A grand task on one pane, gesher's task on another, and the
        // hand-off at work on the gesher Skipper's own pane.
        let mine = row(1, "u1", "cccc3333", "sent", None);
        let sub = ledger::Row { owner: "gesher".into(), ..row(2, "u2", "dddd4444", "sent", None) };
        let hand = ledger::Row { kind: "handoff".into(), project: "gesher".into(), ..row(3, "u3", "bbbb2222", "sent", None) };
        let mut ws = Watchers::default();
        let told = route(&mut ws, &targets, &[mine, sub, hand.clone()], &[], &[], &[], &all);
        let told: Vec<(&str, &str)> = told.iter().map(|(o, w)| (o.as_str(), w.id.as_str())).collect();
        assert_eq!(told, [("grand", "cccc3333"), ("gesher", "dddd4444")]);
        // Each watcher follows its own coordinator's task panes, and registers
        // them on its own coordinator. The Skipper's pane is no task pane.
        assert_eq!(ws["grand"].follows.as_deref(), Some(&["cccc3333".to_string()][..]));
        assert_eq!(ws["gesher"].follows.as_deref(), Some(&["dddd4444".to_string()][..]));
        assert_eq!(wake_line(&ws["gesher"].held), "[watch] pane dddd4444 dddd4444 is idle. Read it, then mark it seen.");

        // gesher reports the hand-off: the grand Skipper alone hears it, once.
        let done = ledger::Row { state: "done".into(), ..hand };
        assert!(ws.get_mut("grand").unwrap().handed(&[], &[]).is_empty());
        let told = route(&mut ws, &targets, &[], &[], std::slice::from_ref(&done), &[], &all);
        assert_eq!(told.iter().map(|(o, w)| (o.as_str(), w.fact)).collect::<Vec<_>>(), [("grand", Fact::Reported)]);
        let grand_held: Vec<Wake> = ws["grand"].held.iter().filter(|h| h.fact == Fact::Reported).cloned().collect();
        assert_eq!(
            wake_line(&grand_held),
            "[watch] the gesher Skipper reported hand-off u3. Read its result. Tell the person, then run skiff ledger report u3."
        );
        assert!(route(&mut ws, &targets, &[], &[], std::slice::from_ref(&done), &[], &all).is_empty());
    }

    #[test]
    fn a_plan_gives_one_line_when_every_task_ended() {
        let mut grand = info("aaaa1111", "idle", false);
        grand.coordinator = true;
        let in_plan = |id, uid: &str, pane: &str| ledger::Row { parent: Some("p1".into()), ..row(id, uid, pane, "sent", None) };
        let tasks = [in_plan(1, "t1", "cccc3333"), in_plan(2, "t2", "dddd4444"), in_plan(3, "t3", "eeee5555")];
        let alone = row(4, "t4", "ffff6666", "sent", None);
        let open: Vec<ledger::Row> = tasks.iter().cloned().chain([alone]).collect();
        let mut ws = Watchers::default();
        // Two plan panes end their turns and the third works: their lines wait.
        // A task with no plan, and a waiting plan pane, are told at once.
        let all = [grand.clone(), info("cccc3333", "idle", true), info("dddd4444", "idle", true), info("eeee5555", "working", false), info("ffff6666", "idle", true)];
        route(&mut ws, &targets(&all), &open, &[], &[], &tasks, &all);
        assert_eq!(ws["grand"].ready_line().as_deref(), Some("[watch] pane ffff6666 ffff6666 is idle. Read it, then mark it seen."));
        ws.get_mut("grand").unwrap().delivered(Delivery::Taken, "");
        assert_eq!(ws["grand"].held.len(), 2);
        assert_eq!(ws["grand"].ready_line(), None);
        let all = [grand.clone(), info("cccc3333", "idle", true), info("dddd4444", "idle", true), info("eeee5555", "waiting", false)];
        let told = route(&mut ws, &targets(&all), &tasks, &[], &[], &tasks, &all);
        assert_eq!(told.iter().map(|(_, w)| w.fact).collect::<Vec<_>>(), [Fact::Waiting]);
        assert_eq!(ws["grand"].ready_line(), None);
        // The last one ends its turn: one line for the plan.
        let all = [grand, info("cccc3333", "idle", true), info("dddd4444", "idle", true), info("eeee5555", "idle", true)];
        route(&mut ws, &targets(&all), &tasks, &[], &[], &tasks, &all);
        assert_eq!(
            ws["grand"].ready_line().as_deref(),
            Some(
                "[watch] plan p1: 3 of 3 tasks ended: pane cccc3333 cccc3333, pane dddd4444 dddd4444, pane eeee5555 eeee5555. \
                 Read the results, mark each pane seen and set each task done, then run skiff ledger report p1."
            )
        );
        ws.get_mut("grand").unwrap().delivered(Delivery::Taken, "");
        assert!(ws["grand"].held.is_empty());
    }

    #[test]
    fn the_grand_skipper_never_hears_a_project_row() {
        let mut grand = info("aaaa1111", "idle", false);
        grand.coordinator = true;
        let mut gesher = info("7508753f0000", "working", false);
        gesher.skipper = Some("gesher".into());
        // gesher's task 9e0caa67 on pane 7902ac72, idle after a turn.
        let sub = ledger::Row { owner: "gesher".into(), ..row(357, "9e0caa67", "7902ac72405d", "sent", None) };
        let all = [grand.clone(), gesher, info("7902ac72405d", "idle", true)];
        let mut ws = Watchers::default();
        let told = route(&mut ws, &targets(&all), std::slice::from_ref(&sub), &[], &[], &[], &all);
        assert_eq!(told.iter().map(|(o, w)| (o.as_str(), w.id.as_str())).collect::<Vec<_>>(), [("gesher", "7902ac72405d")]);
        assert!(ws["grand"].held.is_empty());
        assert_eq!(ws["grand"].follows.as_deref(), Some(&[][..]));
        // With no gesher Skipper running, the row goes to no one.
        let all = [grand, info("7902ac72405d", "idle", true)];
        let mut ws = Watchers::default();
        assert!(route(&mut ws, &targets(&all), &[sub], &[], &[], &[], &all).is_empty());
        assert!(ws["grand"].held.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn a_new_file_at_the_path_is_a_new_build() {
        let dir = std::env::temp_dir().join(format!("skiff-watch-build-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("skiff");
        std::fs::write(&path, "old").unwrap();
        let b = Build { stamp: Build::stamp(&path).unwrap(), path: path.clone() };
        assert!(!b.replaced());
        // A build writes a new file, then links it in place of the old one.
        let new = dir.join("skiff.new");
        std::fs::write(&new, "new").unwrap();
        std::fs::rename(&new, &path).unwrap();
        // Too fresh: the build may still write it.
        assert!(!b.replaced());
        let old = std::time::SystemTime::now() - SETTLE * 2;
        File::options().write(true).open(&path).unwrap().set_modified(old).unwrap();
        assert!(b.replaced());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn only_the_agents_session_is_watched() {
        let mut agent = info("abc12345", "idle", false);
        agent.coordinator = true;
        let all = [agent, info("def67890", "idle", false)];
        assert_eq!(not_the_agent(&all, "abc12345", true), None);
        assert_eq!(not_the_agent(&all, "def67890", true).as_deref(), Some("skiff watch: def67890 is not a Skipper's session"));
        // An older daemon cannot tell, and an unknown session ends the watch as before.
        assert_eq!(not_the_agent(&all, "def67890", false), None);
        assert_eq!(not_the_agent(&all, "zzz", true), None);
    }

    #[test]
    fn only_a_turn_end_counts_as_a_turn() {
        let mut w = Watcher::default();
        // A rename, a theme or a resize on an unread pane.
        w.note(&Event::SessionUpdated { session: info("a", "idle", true) });
        assert!(w.turned.is_empty());
        w.note(&Event::TurnEnded { session: "a".into() });
        assert!(w.turned.contains("a"));

        // Told idle once; metadata events after it wake nothing again.
        let mut w = Watcher::default();
        let s = info("a", "idle", true);
        assert_eq!(w.track("a", Some(&s), &[]).map(|k| k.fact), Some(Fact::Idle));
        w.held.clear();
        for _ in 0..3 {
            w.note(&Event::SessionUpdated { session: s.clone() });
            assert_eq!(w.track("a", Some(&s), &[]), None);
            w.turned.clear();
        }
        assert!(w.held.is_empty());
        // A real second turn is told.
        w.note(&Event::TurnEnded { session: "a".into() });
        assert_eq!(w.track("a", Some(&s), &[]).map(|k| k.fact), Some(Fact::Idle));
    }

    /// A turn the program said ended in an error is told as failed.
    #[test]
    fn a_turn_that_ended_in_an_error_is_told_failed() {
        let mut w = Watcher::default();
        let mut s = info("a", "idle", true);
        s.program_status = serde_json::from_value(serde_json::json!({"state": "error", "msg": "boom"})).unwrap();
        let wake = w.track("a", Some(&s), &[]).unwrap();
        assert_eq!((wake.fact, wake.failed), (Fact::Idle, true));
        assert_eq!(wake_line(&[wake]), "[watch] pane a a failed. Read it, then mark it seen.");
        let mut w = Watcher::default();
        s.program_status = serde_json::from_value(serde_json::json!({"state": "done"})).unwrap();
        assert!(!w.track("a", Some(&s), &[]).unwrap().failed);
    }

    #[test]
    fn a_held_idle_line_goes_when_the_pane_works_or_waits() {
        for state in ["working", "waiting"] {
            let mut w = Watcher::default();
            w.track("a", Some(&info("a", "idle", true)), &[]);
            w.track("b", Some(&info("b", "idle", true)), &[]);
            assert_eq!(w.held.len(), 2);
            w.track("a", Some(&info("a", state, false)), &[]);
            assert_eq!(w.held.iter().map(|h| h.pane.as_str()).collect::<Vec<_>>(), ["b"], "{state}");
        }
        // An exit stays held while the pane is gone.
        let mut w = Watcher::default();
        w.track("a", None, &[]);
        w.track("a", None, &[]);
        assert_eq!(w.held.len(), 1);
    }

    #[test]
    fn a_refused_line_is_held_and_one_with_no_turn_is_undelivered() {
        let mut w = Watcher::default();
        w.track("a", Some(&info("a", "idle", true)), &[]);
        let line = wake_line(&w.held);
        assert!(w.delivered(Delivery::Refused, &line).starts_with("held:"));
        assert_eq!(w.held.len(), 1);
        assert!(w.delivered(Delivery::NoTurn, &line).starts_with("undelivered:"));
        assert!(w.held.is_empty());
        w.track("b", Some(&info("b", "idle", true)), &[]);
        assert!(w.delivered(Delivery::Taken, &line).starts_with("woke the agent"));
        assert!(w.held.is_empty());
    }

    #[test]
    fn a_task_skiff_closed_for_a_gone_pane_is_told_once_after_the_start() {
        let mut w = Watcher::default();
        let done = |uid: &str, pane: &str| row(1, uid, pane, "done", None);
        // The first read only notes them: the unreported count names them.
        assert!(w.closed(&[done("aaaa1111", "4ac9e7a1")]).is_empty());
        let wakes = w.closed(&[done("aaaa1111", "4ac9e7a1"), done("d0e7b2f4", "5b1c9e2a"), done("e5f6a7b8", "5b1c9e2a")]);
        assert_eq!(wakes.len(), 1);
        assert_eq!(wake_line(&w.held), "[watch] pane 5b1c9e2a is gone: tasks d0e7b2f4, e5f6a7b8 are done with it. Tell the person, then run skiff ledger report d0e7b2f4 e5f6a7b8.");
        // Told once.
        assert!(w.closed(&[done("d0e7b2f4", "5b1c9e2a"), done("e5f6a7b8", "5b1c9e2a")]).is_empty());
        // The line survives a step with no open task left on that pane.
        w.take(&[], &[]);
        assert_eq!(w.held.len(), 1);
        // Reported before the line went: it is dropped.
        w.closed(&[done("d0e7b2f4", "5b1c9e2a")]);
        assert!(w.held.is_empty());
        // With other lines, the others come first.
        let idle = Wake { fact: Fact::Idle, pane: "abc12345".into(), name: "api".into(), id: "abc12345".into(), task: None, failed: false };
        let closed = Wake { fact: Fact::Closed, pane: "gone:4ac9e7a1".into(), name: String::new(), id: "4ac9e7a1".into(), task: Some("d0e7b2f4".into()), failed: false };
        assert_eq!(
            wake_line(&[closed, idle]),
            "[watch] pane api abc12345 is idle; pane 4ac9e7a1 is gone: task d0e7b2f4 is done with it. Read it, then mark it seen. Tell the person, then run skiff ledger report d0e7b2f4."
        );
    }

    pub(crate) fn row(id: i64, uid: &str, pane: &str, state: &str, gen: Option<i64>) -> ledger::Row {
        ledger::Row {
            id,
            uid: uid.into(),
            kind: "task".into(),
            parent: None,
            pane_id: Some(pane.into()),
            pane_gen: gen,
            project: String::new(),
            task: "t".into(),
            sent_at: String::new(),
            state: state.into(),
            result_path: None,
            reference: None,
            updated_at: String::new(),
            task_type: None,
            socket: None,
            reported_at: None,
            title: None,
            closed_at: None,
            gone_at: None,
            owner: ledger::GRAND.into(),
            work_dir: None,
            flushed_at: None,
            bucket: None,
            agent_on: false,
            elsewhere: false,
            live: None,
            interrupted: None,
        }
    }

    fn born(mut s: SessionInfo, started_at: u64) -> SessionInfo {
        s.started_at = started_at;
        s
    }

    /// One step with these rows and sessions, as `step` runs it.
    fn run(w: &mut Watcher, rows: &[ledger::Row], all: &[SessionInfo]) -> Vec<Fact> {
        let (groups, interrupted) = group(rows, all, &["skipper"]);
        w.take(&groups, &interrupted).iter().map(|k| k.fact).collect()
    }

    fn held(w: &Watcher) -> Vec<(Fact, Option<&str>)> {
        w.held.iter().map(|h| (h.fact, h.task.as_deref())).collect()
    }

    fn state(w: &mut Watcher, id: &str, states: &[SessionState]) {
        for &state in states {
            w.note(&Event::State { session: id.into(), state });
        }
    }

    #[test]
    fn a_prompt_then_idle_is_told_once() {
        let seq = |nows: &[Now]| {
            let mut p = Pane::default();
            nows.iter().filter_map(|&n| p.observe(n)).collect::<Vec<_>>()
        };
        let seen = Now::Idle { unread: false };
        let unread = Now::Idle { unread: true };
        assert_eq!(seq(&[Now::Waiting, Now::Working, seen, seen]), [Fact::Waiting, Fact::Idle]);
        // The working step not seen.
        assert_eq!(seq(&[Now::Waiting, seen]), [Fact::Waiting, Fact::Idle]);
        // Each prompt notifies the person; one idle after the last.
        assert_eq!(
            seq(&[Now::Waiting, Now::Working, Now::Waiting, Now::Working, seen]),
            [Fact::Waiting, Fact::Waiting, Fact::Idle]
        );
        // A turn after the prompt: one idle, not one more for the prompt.
        assert_eq!(seq(&[Now::Waiting, Now::Working, unread, seen]), [Fact::Waiting, Fact::Idle]);
        assert_eq!(seq(&[Now::Waiting, Now::Exit, seen]), [Fact::Waiting, Fact::Exit]);
        assert_eq!(seq(&[Now::Waiting, Now::Gone]), [Fact::Waiting, Fact::Gone]);
        // A start-up screen is still no turn.
        assert_eq!(seq(&[Now::Working, seen, seen]), []);
    }

    #[test]
    fn an_idle_pane_with_a_pending_task_wakes_once_per_idle_stretch() {
        let mut w = Watcher::default();
        let rows = [row(1, "7c2e91ab", "a", "pending", None)];
        assert_eq!(run(&mut w, &rows, &[info("a", "idle", false)]), [Fact::Pending]);
        assert_eq!(wake_line(&w.held), "[watch] pane a a is idle: task 7c2e91ab is pending: skiff ledger send 7c2e91ab --enter. Read it, then mark it seen.");
        assert_eq!(run(&mut w, &rows, &[info("a", "idle", false)]), []);
        assert_eq!(w.held.len(), 1);
        // It works: the held line goes. The next idle tells it again.
        assert_eq!(run(&mut w, &rows, &[info("a", "working", false)]), []);
        assert!(w.held.is_empty());
        assert_eq!(run(&mut w, &rows, &[info("a", "idle", false)]), [Fact::Pending]);
        // Waiting drops it too.
        assert_eq!(run(&mut w, &rows, &[info("a", "waiting", false)]), [Fact::Waiting]);
        assert!(w.held.is_empty());
        // Joined with other parts as today.
        let b = info("b", "idle", true);
        assert_eq!(run(&mut w, &rows, &[info("a", "idle", false), b]), [Fact::Pending]);
        let all = [info("a", "idle", false), info("b", "idle", true)];
        let rows = [row(1, "7c2e91ab", "a", "pending", None), row(2, "9d01ffe2", "b", "sent", None)];
        assert_eq!(run(&mut w, &rows, &all), [Fact::Idle]);
        assert_eq!(
            wake_line(&w.held),
            "[watch] pane a a is idle: task 7c2e91ab is pending: skiff ledger send 7c2e91ab --enter; pane b b is idle. Read them, then mark each seen."
        );
    }

    #[test]
    fn a_pending_task_on_an_unread_pane_is_one_part() {
        let mut w = Watcher::default();
        let rows = [row(1, "u1", "a", "pending", None)];
        w.note(&Event::TurnEnded { session: "a".into() });
        assert_eq!(run(&mut w, &rows, &[info("a", "idle", true)]), [Fact::Pending]);
        assert_eq!(held(&w), [(Fact::Pending, Some("u1"))]);
        // A prompt answered with a pending row: one part for both.
        let mut w = Watcher::default();
        assert_eq!(run(&mut w, &rows, &[info("a", "waiting", false)]), [Fact::Waiting]);
        assert_eq!(run(&mut w, &rows, &[info("a", "working", false)]), []);
        assert_eq!(run(&mut w, &rows, &[info("a", "idle", false)]), [Fact::Pending]);
        assert_eq!(held(&w), [(Fact::Pending, Some("u1"))]);
        assert_eq!(run(&mut w, &rows, &[info("a", "idle", false)]), []);
    }

    #[test]
    fn a_late_turn_end_does_not_repeat_a_pending_line() {
        let rows = [row(1, "u1", "a", "pending", None)];
        let a = info("a", "idle", true);
        // Delivered, then the turn's end comes in.
        let mut w = Watcher::default();
        assert_eq!(run(&mut w, &rows, std::slice::from_ref(&a)), [Fact::Pending]);
        w.delivered(Delivery::Taken, "");
        w.note(&Event::TurnEnded { session: "a".into() });
        assert_eq!(run(&mut w, &rows, std::slice::from_ref(&a)), []);
        assert!(w.held.is_empty());
        // Still held: it stays one part.
        let mut w = Watcher::default();
        run(&mut w, &rows, std::slice::from_ref(&a));
        w.note(&Event::TurnEnded { session: "a".into() });
        assert_eq!(run(&mut w, &rows, std::slice::from_ref(&a)), []);
        assert_eq!(held(&w), [(Fact::Pending, Some("u1"))]);
    }

    #[test]
    fn a_new_pending_row_upgrades_a_held_idle_line() {
        let mut w = Watcher::default();
        let a = [info("a", "idle", true)];
        assert_eq!(run(&mut w, &[row(1, "s1", "a", "sent", None)], &a), [Fact::Idle]);
        // A row is added; the pane stays unread with no new state.
        let rows = [row(1, "s1", "a", "sent", None), row(2, "u1", "a", "pending", None)];
        assert_eq!(run(&mut w, &rows, &a), [Fact::Pending]);
        assert_eq!(held(&w), [(Fact::Pending, Some("u1"))]);
        // The oldest uid changes: u1 removed, u2 added.
        let rows = [row(1, "s1", "a", "sent", None), row(3, "u2", "a", "pending", None)];
        assert_eq!(run(&mut w, &rows, &a), [Fact::Pending]);
        assert_eq!(held(&w), [(Fact::Pending, Some("u2"))]);
    }

    #[test]
    fn the_next_pending_task_goes_once_the_told_one_leaves_pending() {
        let a = [info("a", "idle", false)];
        let both = [row(1, "u1", "a", "pending", None), row(2, "u2", "a", "pending", None)];
        // u1 removed while u2 stays pending: u2 at once.
        let mut w = Watcher::default();
        run(&mut w, &both, &a);
        w.delivered(Delivery::Taken, "");
        assert_eq!(run(&mut w, &both[1..], &a), [Fact::Pending]);
        assert_eq!(held(&w), [(Fact::Pending, Some("u2"))]);
        // u1 sent, then pending again with no state change: told again.
        let mut w = Watcher::default();
        let u1 = |state| [row(1, "u1", "a", state, None)];
        run(&mut w, &u1("pending"), &a);
        w.delivered(Delivery::Taken, "");
        assert_eq!(run(&mut w, &u1("sent"), &a), []);
        assert_eq!(run(&mut w, &u1("pending"), &a), [Fact::Pending]);
        // u1 cancelled (done or removed) while held: no line, no idle part.
        let mut w = Watcher::default();
        run(&mut w, &u1("pending"), &a);
        assert_eq!(run(&mut w, &[], &a), []);
        assert!(w.held.is_empty());
        // u1 sent before watch sees work: the held line goes.
        let mut w = Watcher::default();
        run(&mut w, &u1("pending"), &a);
        assert_eq!(run(&mut w, &u1("sent"), &a), []);
        assert!(w.held.is_empty());
    }

    #[test]
    fn state_events_between_two_lists_count() {
        use SessionState::{Idle, Waiting, Working};
        let seen = [info("a", "idle", false)];
        // A prompt shown and answered between two seen-idle lists, no turn.
        let mut w = Watcher::default();
        let sent = [row(1, "s1", "a", "sent", None)];
        assert_eq!(run(&mut w, &sent, &seen), []);
        state(&mut w, "a", &[Waiting, Working, Idle]);
        assert_eq!(run(&mut w, &sent, &seen), [Fact::Idle]);
        assert_eq!(run(&mut w, &sent, &seen), []);
        // A told pending task is told again after unseen work.
        let mut w = Watcher::default();
        let rows = [row(1, "u1", "a", "pending", None)];
        assert_eq!(run(&mut w, &rows, &seen), [Fact::Pending]);
        w.delivered(Delivery::Taken, "");
        state(&mut w, "a", &[Working, Idle]);
        assert_eq!(run(&mut w, &rows, &seen), [Fact::Pending]);
        // A held one goes with the work, and comes back as one part.
        state(&mut w, "a", &[Waiting, Working, Idle]);
        assert_eq!(run(&mut w, &rows, &seen), [Fact::Pending]);
        assert_eq!(held(&w), [(Fact::Pending, Some("u1"))]);
        // An idle event alone is no work.
        state(&mut w, "a", &[Idle]);
        assert_eq!(run(&mut w, &rows, &seen), []);
    }

    #[test]
    fn a_row_changed_after_the_wake_is_dropped_before_the_line() {
        let mut w = Watcher::default();
        let all = [info("a", "idle", false), info("b", "idle", true)];
        let rows = [row(1, "u1", "a", "pending", None), row(2, "s1", "b", "sent", None)];
        run(&mut w, &rows, &all);
        assert_eq!(w.held.len(), 2);
        // The read after `ready`: u1 was sent meanwhile.
        let rows = [row(1, "u1", "a", "sent", None), row(2, "s1", "b", "sent", None)];
        w.drop_stale(&group(&rows, &all, &["skipper"]).0);
        assert_eq!(held(&w), [(Fact::Idle, None)]);
    }

    #[test]
    fn a_new_generation_forgets_the_pane() {
        let rows = [row(1, "u1", "a", "pending", None)];
        let mut w = Watcher::default();
        assert_eq!(run(&mut w, &rows, &[born(info("a", "idle", false), 1)]), [Fact::Pending]);
        w.delivered(Delivery::Taken, "");
        // A reload keeps the generation: nothing new.
        assert_eq!(run(&mut w, &rows, &[born(info("a", "idle", false), 1)]), []);
        // A restore, seen only after a reconnect, already seen-idle.
        assert_eq!(run(&mut w, &rows, &[born(info("a", "idle", false), 2)]), [Fact::Pending]);
        // A sent row from the old generation is interrupted beside it.
        let mut w = Watcher::default();
        let rows = [row(1, "s1", "a", "sent", Some(1)), row(2, "u1", "a", "pending", None)];
        assert_eq!(run(&mut w, &rows, &[born(info("a", "idle", false), 2)]), [Fact::Interrupted, Fact::Pending]);
        assert_eq!(
            wake_line(&w.held),
            "[watch] pane a a restarted: task s1 is interrupted; pane a a is idle: task u1 is pending: skiff ledger send u1 --enter. Read them, then mark each seen."
        );
    }

    #[test]
    fn rows_that_name_one_pane_make_one_group() {
        let mut w = Watcher::default();
        let all = [info("abc12345xyz", "idle", false)];
        // An imported prefix row and a full-id row, the newer one first in id order.
        let rows = [row(4, "u1", "abc12345", "pending", None), row(7, "u2", "abc12345xyz", "pending", None)];
        let (groups, _) = group(&rows, &all, &["skipper"]);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].pending, ["u1", "u2"]);
        assert_eq!(run(&mut w, &rows, &all), [Fact::Pending]);
        assert_eq!(held(&w), [(Fact::Pending, Some("u1"))]);
        // The agent's own pane is skipped.
        let all = [info("skipper", "idle", false)];
        assert!(group(&[row(1, "u1", "skipper", "pending", None)], &all, &["skipper"]).0.is_empty());
    }

    #[test]
    fn follows_names_live_panes_with_an_open_task() {
        let all = [info("b", "working", false), info("a", "idle", false), info("skipper", "idle", false)];
        // A gone pane and the agent's own pane are not followed. Each pane once.
        let rows = [
            row(1, "u1", "b", "sent", None),
            row(2, "u2", "a", "pending", None),
            row(3, "u3", "b", "blocked", None),
            row(4, "u4", "gone", "sent", None),
            row(5, "u5", "skipper", "pending", None),
        ];
        assert_eq!(follows(&group(&rows, &all, &["skipper"]).0), ["a", "b"]);
        assert!(follows(&group(&[], &all, &["skipper"]).0).is_empty());
    }

    #[test]
    fn a_task_on_another_socket_is_never_gone_here() {
        let here = Path::new("/tmp/here.sock");
        let mut theirs = row(1, "u1", "a", "sent", None);
        theirs.socket = Some("/tmp/other.sock".into());
        let mut mine = row(2, "u2", "b", "sent", None);
        mine.socket = Some(here.display().to_string());
        let (kept, line) = on_socket(vec![theirs, mine], here);
        assert_eq!(kept.iter().map(|r| r.uid.as_str()).collect::<Vec<_>>(), ["u2"]);
        assert_eq!(line, "tasks u1 belong to /tmp/other.sock; not followed here");
        // Neither pane is on this daemon: only the task of this socket is told gone.
        let mut w = Watcher::default();
        assert_eq!(run(&mut w, &kept, &[]), [Fact::Gone]);
        assert!(!wake_line(&w.held).contains("u1"));
        // With only the other socket's task, nothing is told, and no reassign.
        let mut w = Watcher::default();
        assert!(run(&mut w, &on_socket(vec![row(1, "u1", "a", "sent", None)].into_iter().map(|mut r| { r.socket = Some("/tmp/other.sock".into()); r }).collect(), here).0, &[]).is_empty());
        assert!(w.held.is_empty());
    }

    #[test]
    fn exit_and_gone_win_over_a_pending_task() {
        let rows = [row(1, "u1", "a", "pending", None)];
        let mut w = Watcher::default();
        assert_eq!(run(&mut w, &rows, &[info("a", "done", false)]), [Fact::Exit]);
        assert_eq!(wake_line(&w.held), "[watch] pane a a exited. Read it, then mark it seen.");
        let mut w = Watcher::default();
        run(&mut w, &rows, &[info("a", "idle", false)]);
        // Gone after a held pending line: the gone line replaces it.
        assert_eq!(run(&mut w, &rows, &[]), [Fact::Gone]);
        // It names the open task, which is reassigned or marked done, not marked seen.
        assert_eq!(held(&w), [(Fact::Gone, Some("u1"))]);
        assert_eq!(wake_line(&w.held), "[watch] pane a a is gone: task u1 is open. Reassign it or mark it done.");
    }

    #[test]
    fn a_pending_line_is_held_when_refused_and_dropped_with_no_turn() {
        let rows = [row(1, "u1", "a", "pending", None)];
        let a = [info("a", "idle", false)];
        let mut w = Watcher::default();
        run(&mut w, &rows, &a);
        let line = wake_line(&w.held);
        w.delivered(Delivery::Refused, &line);
        assert_eq!(held(&w), [(Fact::Pending, Some("u1"))]);
        // A failed ledger read skips the step: the next one keeps the line.
        assert_eq!(run(&mut w, &rows, &a), []);
        assert_eq!(held(&w), [(Fact::Pending, Some("u1"))]);
        w.delivered(Delivery::NoTurn, &line);
        assert!(w.held.is_empty());
        // Not told again in this idle stretch.
        assert_eq!(run(&mut w, &rows, &a), []);
    }

    fn rows(lines: &[&str]) -> Vec<(String, String)> {
        lines.iter().map(|l| (l.to_string(), l.to_string())).collect()
    }

    // SPLIT-SCAFFOLD
    #[test]
    fn now_of_covers_session_states_and_exit() {
        assert_eq!(Now::of(None, false), Now::Gone);
        assert_eq!(Now::of(Some(&info("a", "working", false)), false), Now::Working);
        assert_eq!(Now::of(Some(&info("a", "waiting", false)), false), Now::Waiting);
        assert_eq!(Now::of(Some(&info("a", "idle", false)), true), Now::Idle { unread: true });
    }

    // SPLIT-SCAFFOLD
    #[test]
    fn watching_registration_shape_stays_stable() {
        let w = Watcher::default();
        assert!(w.follows.is_none() && w.failure.is_none());
    }

    // SPLIT-SCAFFOLD
    #[test]
    fn input_rows_keep_wide_and_dim_cells() {
        let rows = vec![("─".repeat(20), "─".repeat(20)), ("❯ 修正".into(), "❯".into()), ("─".repeat(20), "─".repeat(20))];
        assert_eq!(input_box(&rows, "claude"), Some(false));
    }

    // SPLIT-SCAFFOLD
    #[test]
    fn refusal_keeps_held_wakes_and_follows() {
        let mut w = Watcher::default();
        w.follows = Some(vec!["pane".into()]);
        w.held.push(Wake { fact: Fact::Idle, pane: "pane".into(), name: "pane".into(), id: "pane".into(), task: None, failed: false });
        let line = wake_line(&w.held);
        w.delivered(Delivery::Refused, &line);
        assert_eq!(w.follows, Some(vec!["pane".into()]));
        assert_eq!(w.held.len(), 1);
    }

    #[test]
    fn finds_an_empty_box_and_a_draft() {
        let rule = "─".repeat(40);
        assert_eq!(input_box(&rows(&["done.", &rule, "❯", &rule, "  ? for shortcuts"]), "claude"), Some(false));
        let mut tall = rows(&[&rule, "❯ x", &rule]);
        tall.extend(rows(&[""; 30]));
        assert_eq!(input_box(&tall, "claude"), Some(true));
        assert_eq!(input_box(&rows(&["done.", &rule, "❯ fix the", "  tests", &rule]), "claude"), Some(true));
        // The dim hint is not a draft.
        let hint = vec![(rule.clone(), rule.clone()), ("❯ Try \"fix lint\"".into(), "❯".into()), (rule.clone(), rule.clone())];
        assert_eq!(input_box(&hint, "claude"), Some(false));
        assert_eq!(input_box(&rows(&["$ ls", "a b"]), "claude"), None);
        assert!(busy_line(&rows(&["* Working (2s · esc to interrupt)", &rule, "❯", &rule])));
        assert!(!busy_line(&rows(&["done.", &rule, "❯", &rule])));
        assert_eq!(input_box(&rows(&["text", "›", "", "  ? for shortcuts"]), "codex"), Some(false));
        assert_eq!(input_box(&rows(&["text", "› hello", ""]), "codex"), Some(true));
        assert_eq!(front(&rows(&["done.", &rule, "❯", &rule]), "claude"), Front::Ready);
        assert_eq!(front(&rows(&["done.", &rule, "❯ fix the", &rule]), "claude"), Front::Draft);
        // The busy line wins over an empty box.
        assert_eq!(front(&rows(&["* Working (2s · esc to interrupt)", &rule, "❯", &rule]), "claude"), Front::Busy);
        assert_eq!(front(&rows(&["$ ls", "a b"]), "claude"), Front::NoBox);
    }
}
