//! Fleet Skippers: the takeover and the rename. The app's backend, the CLI
//! and skiff-host call this code, so a takeover is the same from each.
//!
//! A takeover happens when a project with a project Skipper comes under a
//! running fleet Skipper: the project joins its fleet, or the fleet Skipper
//! starts. It runs on the person's confirm, in this order: the ledger, then
//! the daemon, then `projects.toml`. A failure at any step leaves a state
//! that the same move repairs.

use std::path::Path;

use anyhow::{bail, Context, Result};
use rusqlite::{params, TransactionBehavior};
use serde::Serialize;
use skiff_core::{
    config::{self, Config},
    session::SessionInfo,
};

use crate::{ledger, skipper, Client};

/// One open task a takeover moves, as the confirm names it.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Task {
    pub uid: String,
    pub title: String,
}

/// One project Skipper a takeover takes over.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Taken {
    pub project: String,
    /// The project Skipper's session.
    pub session: String,
    /// Its open tasks: sent, pending or blocked.
    pub tasks: Vec<Task>,
    /// The uids of the open hand-offs it holds.
    pub handoffs: Vec<String>,
}

impl Taken {
    /// As a Host answers it (`skipper_run@2`).
    pub fn to_host(&self) -> skiff_host_api::TakenSkipper {
        skiff_host_api::TakenSkipper {
            project: self.project.clone(),
            session: self.session.clone(),
            tasks: self.tasks.iter().map(|t| skiff_host_api::TakenTask { uid: t.uid.clone(), title: t.title.clone() }).collect(),
            handoffs: self.handoffs.clone(),
        }
    }
}

/// The title a task shows: its `--title`, else its first sentence.
fn title_of(r: &ledger::Row) -> String {
    if let Some(t) = r.title.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
        return t.to_string();
    }
    let t = r.task.trim();
    let end = t.find(". ").map(|i| i + 1).unwrap_or(t.len());
    t[..end].chars().take(60).collect()
}

/// The project Skippers a takeover of `fleet` takes over: those of the
/// fleet's projects, and of `join` when it joins. `file` is the ledger.
pub fn taken(cfg: &Config, all: &[SessionInfo], file: &Path, fleet: &str, join: Option<&str>) -> Result<Vec<Taken>> {
    let mut projects: Vec<String> = skipper::fleet_projects(cfg, fleet).into_iter().map(|p| p.name.clone()).collect();
    if let Some(j) = join.filter(|j| !projects.iter().any(|p| p == j)) {
        projects.push(j.to_string());
    }
    let db = file.exists().then(|| ledger::open_read(file)).transpose()?;
    let mut out = Vec::new();
    for p in projects {
        let Some(s) = skipper::find_project(all, &p) else { continue };
        let (tasks, handoffs) = match &db {
            Some(db) => (
                ledger::rows(db, "WHERE owner = ?1 AND kind = 'task' AND state IN ('sent', 'pending', 'blocked')", Some(&p))?
                    .iter()
                    .map(|r| Task { uid: r.uid.clone(), title: title_of(r) })
                    .collect(),
                ledger::rows(db, "WHERE kind = 'handoff' AND project = ?1 AND state != 'done'", Some(&p))?.into_iter().map(|r| r.uid).collect(),
            ),
            None => (Vec::new(), Vec::new()),
        };
        out.push(Taken { project: p, session: s.id.clone(), tasks, handoffs });
    }
    Ok(out)
}

/// What a takeover of `fleet` would take over now. Changes nothing.
pub async fn preview(c: &Client, fleet: &str, join: Option<&str>) -> Result<Vec<Taken>> {
    let cfg = config::load()?;
    taken(&cfg, &c.list_sessions().await?, &ledger::path()?, fleet, join)
}

/// The line the fleet Skipper gets, once, for a takeover.
pub fn line(fleet: &str, t: &[Taken]) -> String {
    let projects: Vec<&str> = t.iter().map(|t| t.project.as_str()).collect();
    let tasks: Vec<&str> = t.iter().flat_map(|t| &t.tasks).map(|t| t.uid.as_str()).collect();
    let handoffs: Vec<&str> = t.iter().flat_map(|t| &t.handoffs).map(String::as_str).collect();
    let mut s = format!("The {fleet} Skipper took over {}: {}", projects.join(", "), count(tasks.len(), "open task", "open tasks"));
    if !tasks.is_empty() {
        s += &format!(" ({})", tasks.join(", "));
    }
    match handoffs.len() {
        0 => {}
        1 => s += &format!(" and hand-off {}", handoffs[0]),
        _ => s += &format!(" and hand-offs {}", handoffs.join(", ")),
    }
    s + ". Run skiff ledger ls, then read SKIPPER.md again."
}

/// The confirm's text: each project Skipper with its open tasks by title
/// and its hand-offs, one per line. The app and the CLI show the same.
pub fn summary(t: &[Taken]) -> String {
    t.iter()
        .map(|p| {
            let mut s = format!("{} Skipper: {}", p.project, count(p.tasks.len(), "open task", "open tasks"));
            if !p.tasks.is_empty() {
                s += &format!(" ({})", p.tasks.iter().map(|t| t.title.as_str()).collect::<Vec<_>>().join(", "));
            }
            if !p.handoffs.is_empty() {
                s += &format!(", {}", count(p.handoffs.len(), "hand-off", "hand-offs"));
            }
            s + "."
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn count(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// Moves the rows of each project Skipper in `t` to the fleet Skipper
/// `fs`, in one transaction, and leaves the takeover line for the watcher.
/// Every row the project Skipper owns, open or closed, takes the owner
/// `fleet:<fleet>`; state, pane, parent, result path and stamps stay. Each
/// open hand-off to the project gets the fleet Skipper as its pane and the
/// fleet as its scope; `pending` and `blocked` become `pending`.
/// `note`: leave the takeover line for the watcher. A new fleet Skipper gets
/// it in its first message instead.
pub fn move_rows(file: &Path, socket: &Path, fleet: &str, fs: &SessionInfo, t: &[Taken], note: bool) -> Result<()> {
    let scope = skipper::fleet_scope(fleet);
    let mut db = ledger::open_write(file)?;
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    const NOW: &str = "strftime('%Y-%m-%dT%H:%M:%SZ', 'now')";
    for p in t {
        tx.execute("UPDATE tasks SET owner = ?2 WHERE owner = ?1", params![p.project, scope])?;
        tx.execute(
            &format!(
                "UPDATE tasks SET pane_id = ?2, pane_gen = ?3, socket = ?4, project = ?5,
                 state = CASE WHEN state IN ('pending', 'blocked') THEN 'pending' ELSE state END, updated_at = {NOW}
                 WHERE kind = 'handoff' AND project = ?1 AND state != 'done'"
            ),
            params![p.project, fs.id, ledger::gen(fs), socket.display().to_string(), scope],
        )?;
    }
    if note {
        tx.execute(&format!("INSERT INTO notes (owner, text, at) VALUES (?1, ?2, {NOW})"), params![scope, line(fleet, t)])?;
    }
    tx.commit()?;
    Ok(())
}

/// A restarted project or fleet Skipper takes its interrupted hand-offs
/// back as pending. Keep their owner, and leave one resend note per owner.
pub fn restart_rows(file: &Path, scope: &str, skipper: &SessionInfo, all: &[SessionInfo]) -> Result<()> {
    if !file.exists() {
        return Ok(());
    }
    let mut db = ledger::open_write(file)?;
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let mut owners = std::collections::BTreeMap::<String, Vec<String>>::new();
    for r in ledger::rows(&tx, "WHERE kind = 'handoff' AND project = ?1 AND state IN ('sent', 'blocked')", Some(&scope))? {
        let old = all.iter().find(|s| Some(s.id.as_str()) == r.pane_id.as_deref());
        if !r.interrupted_in(old) {
            continue;
        }
        tx.execute(
            "UPDATE tasks SET pane_id = ?2, pane_gen = ?3, state = 'pending', updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now') WHERE uid = ?1",
            params![r.uid, skipper.id, ledger::gen(skipper)],
        )?;
        owners.entry(r.owner).or_default().push(r.uid);
    }
    for (owner, uids) in owners {
        let text = format!("The {scope} Skipper restarted. Hand-offs {} moved to pane {} and are pending. Resend them with skiff ledger send <uid> --enter.", uids.join(", "), skipper.id);
        tx.execute("INSERT INTO notes (owner, text, at) VALUES (?1, ?2, strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))", params![owner, text])?;
    }
    tx.commit()?;
    Ok(())
}

/// The takeover: the fleet Skipper of `fleet` takes over each project
/// Skipper of its projects, and of `join`, which joins the fleet last.
/// The fleet Skipper must run. Returns what it took over. With nothing to
/// take over, `join` still joins. `note`: see [`move_rows`].
pub async fn takeover(c: &Client, socket: &Path, fleet: &str, join: Option<&str>, note: bool) -> Result<Vec<Taken>> {
    let cfg = config::load()?;
    let all = c.list_sessions().await?;
    let Some(fs) = skipper::find_fleet(&all, fleet) else { bail!("the {fleet} Skipper does not run") };
    let file = ledger::path()?;
    let t = taken(&cfg, &all, &file, fleet, join)?;
    if !t.is_empty() {
        if file.exists() {
            move_rows(&file, socket, fleet, fs, &t, note).context("move the rows")?;
        }
        for p in &t {
            c.retire(&p.session).await.with_context(|| format!("stop the {} Skipper", p.project))?;
        }
    }
    if let Some(j) = join {
        config::set_project_fleet(j, fleet)?;
    }
    skipper::write_fleet(&config::load()?, fleet)?;
    Ok(t)
}

/// Rename fleet…: the fleet `old` becomes `new` in one move. The ledger
/// first, in one transaction: the owner `fleet:<old>`, the scope of its
/// hand-offs and notes, and the result paths under its folder. Then the
/// folder, then the fleet Skipper's mark and name, then `projects.toml`.
/// The fleet Skipper gets one line.
pub async fn rename(c: &Client, old: &str, new: &str) -> Result<()> {
    let cfg = config::load()?;
    if skipper::is_fleet(&cfg, new) {
        bail!("A fleet named {new} exists. Use another name");
    }
    let from = skipper::fleet_folder(&cfg, old)?;
    let to = skipper::fleet_folder(&cfg, new)?;
    if to.exists() {
        bail!("{} exists. Use another name", to.display());
    }
    let (a, b) = (skipper::fleet_scope(old), skipper::fleet_scope(new));
    let all = c.list_sessions().await?;
    let fs = skipper::find_fleet(&all, old).cloned();
    let file = ledger::path()?;
    if file.exists() {
        let mut db = ledger::open_write(&file)?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute("UPDATE tasks SET owner = ?2 WHERE owner = ?1", params![a, b])?;
        tx.execute("UPDATE tasks SET project = ?2 WHERE project = ?1", params![a, b])?;
        tx.execute("UPDATE notes SET owner = ?2 WHERE owner = ?1", params![a, b])?;
        let (pa, pb) = (from.display().to_string(), to.display().to_string());
        tx.execute(
            "UPDATE tasks SET result_path = ?2 || substr(result_path, length(?1) + 1) WHERE substr(result_path, 1, length(?1) + 1) = ?1 || '/'",
            params![pa, pb],
        )?;
        let work_dirs: i64 = tx.query_row("SELECT count(*) FROM pragma_table_info('tasks') WHERE name = 'work_dir'", [], |r| r.get(0))?;
        if work_dirs > 0 {
            tx.execute(
                "UPDATE tasks SET work_dir = ?2 || substr(work_dir, length(?1) + 1) WHERE substr(work_dir, 1, length(?1) + 1) = ?1 || '/'",
                params![pa, pb],
            )?;
        }
        if fs.is_some() {
            let text = format!("The fleet {old} is {new} now. Your folder is {pb}. Read SKIPPER.md there again.");
            tx.execute("INSERT INTO notes (owner, text, at) VALUES (?1, ?2, strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))", params![b, text])?;
        }
        tx.commit()?;
    }
    if from.exists() {
        std::fs::rename(&from, &to).with_context(|| format!("move {} to {}", from.display(), to.display()))?;
        // SKIPPER.md keeps its status: a file Skiff wrote gets the new name at the next start.
        let (fa, fb) = (skipper::Scope::Fleet(old.into()).file(), skipper::Scope::Fleet(new.into()).file());
        let _lock = skipper::lock()?;
        skipper::move_record(&skipper::folder(&cfg)?, &fa, &fb)?;
    }
    if let Some(s) = &fs {
        c.rescope(&s.id, &b, &skipper::fleet_name(&cfg, new)).await?;
    }
    let members: Vec<(String, String)> = skipper::fleet_projects(&cfg, old).into_iter().map(|p| (p.name.clone(), new.to_string())).collect();
    let order: Vec<String> = cfg.projects.iter().map(|p| p.name.clone()).collect();
    config::arrange_projects(&order, &members)?;
    if fs.is_some() {
        skipper::write_fleet(&config::load()?, new)?;
    }
    Ok(())
}
