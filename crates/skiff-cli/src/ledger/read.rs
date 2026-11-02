use std::path::Path;

use anyhow::{bail, Context, Result};
use rusqlite::{params, Connection, TransactionBehavior};
use skiff_client::Client;
use skiff_client::ledger::rows;
use skiff_core::session::SessionInfo;

use crate::{print, resolve};

use super::{path, Row, NOW};

const STATES: [&str; 4] = ["sent", "pending", "blocked", "done"];

/// The open tasks (state sent, pending or blocked), oldest first. Plans are
/// left out. This is the read path for other commands, such as `skiff
/// watch`. No `ledger.db` yet gives an empty list, and makes no file.
pub fn open_tasks() -> Result<Vec<Row>> {
    open_tasks_at(&path()?)
}

pub(super) fn open_tasks_at(file: &Path) -> Result<Vec<Row>> {
    if !file.exists() {
        return Ok(Vec::new());
    }
    let db = super::open_at(file)?;
    rows(&db, "WHERE kind IN ('task', 'handoff') AND state IN ('sent', 'pending', 'blocked')", None)
}

/// The done tasks of `owner` the person was not told of yet. `skiff watch`
/// names the count at its start. No `ledger.db` yet gives 0, and makes no file.
pub fn unreported(owner: &str) -> Result<usize> {
    unreported_at(&path()?, owner)
}

pub(super) fn unreported_at(file: &Path, owner: &str) -> Result<usize> {
    if !file.exists() {
        return Ok(0);
    }
    let db = super::open_at(file)?;
    let n: i64 = db.query_row(
        "SELECT count(*) FROM tasks WHERE kind IN ('task', 'handoff') AND state = 'done' AND reported_at IS NULL AND owner = ?1",
        [owner],
        |r| r.get(0),
    )?;
    Ok(usize::try_from(n).unwrap_or(0))
}

/// Sets `done` each open task on `here` whose pane no session in `all` has,
/// and stamps `gone_at`. A task on another socket is left alone: its pane
/// cannot be checked from here. A restarted pane still exists, so an
/// interrupted task stays. The report stamp stays empty: the person is told
/// once. `all` must be a full list from `here`'s daemon. Returns the tasks it
/// closed.
pub fn close_gone(here: &Path, all: &[SessionInfo]) -> Result<Vec<Row>> {
    let file = path()?;
    if !file.exists() {
        return Ok(Vec::new());
    }
    close_gone_in(&mut super::open_at(&file)?, here, all)
}

pub(super) fn close_gone_in(db: &mut Connection, here: &Path, all: &[SessionInfo]) -> Result<Vec<Row>> {
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let open = rows(&tx, "WHERE kind IN ('task', 'handoff') AND state IN ('sent', 'pending', 'blocked') AND pane_id IS NOT NULL", None)?;
    // A stored id may be a prefix: any session it could name keeps the task.
    let gone: Vec<Row> = open
        .into_iter()
        .filter(|r| r.socket_other_than(here).is_none() && r.pane_id.as_deref().is_some_and(|p| !all.iter().any(|s| s.id.starts_with(p))))
        .collect();
    let mut out = Vec::new();
    for r in gone {
        let n = tx.execute(&format!("UPDATE tasks SET state = 'done', gone_at = {NOW}, updated_at = {NOW} WHERE id = ?1 AND state = ?2"), params![r.id, r.state])?;
        if n == 1 {
            out.push(one(&tx, &r.uid)?);
        }
    }
    tx.commit()?;
    Ok(out)
}

/// Runs [`close_gone_in`] when the daemon answered, and says which tasks it
/// closed. With no answer, nothing is gone.
pub(super) fn sweep(db: &mut Connection, here: &Path, all: Option<&[SessionInfo]>) -> Result<()> {
    let Some(all) = all else { return Ok(()) };
    for r in close_gone_in(db, here, all)? {
        eprintln!("skiff: task {} is done: its pane {} is gone", r.uid, print::short_id(r.pane_id.as_deref().unwrap_or("")));
    }
    Ok(())
}

/// The tasks skiff set done because their panes were gone, not yet reported.
/// No `ledger.db` yet gives none, and makes no file.
pub fn gone_unreported() -> Result<Vec<Row>> {
    let file = path()?;
    if !file.exists() {
        return Ok(Vec::new());
    }
    rows(&super::open_at(&file)?, "WHERE kind IN ('task', 'handoff') AND state = 'done' AND gone_at IS NOT NULL AND reported_at IS NULL", None)
}

/// The tasks of each plan that is not closed, in any state, oldest first.
/// `skiff watch` holds a plan task's idle line until every one of them
/// ended. No `ledger.db` yet gives none, and makes no file.
pub fn plan_tasks() -> Result<Vec<Row>> {
    let file = path()?;
    if !file.exists() {
        return Ok(Vec::new());
    }
    rows(
        &super::open_at(&file)?,
        "WHERE kind IN ('task', 'handoff') AND parent IN (SELECT uid FROM tasks WHERE kind = 'plan' AND closed_at IS NULL)",
        None,
    )
}

/// The hand-offs a project Skipper set done, not yet reported: the grand
/// Skipper's watcher tells it each once. No `ledger.db` yet gives none.
pub fn handed_back() -> Result<Vec<Row>> {
    let file = path()?;
    if !file.exists() {
        return Ok(Vec::new());
    }
    rows(&super::open_at(&file)?, "WHERE kind = 'handoff' AND state = 'done' AND gone_at IS NULL AND reported_at IS NULL", None)
}

pub(super) fn state(s: &str) -> Result<&str> {
    match STATES.iter().find(|x| **x == s) {
        Some(x) => Ok(x),
        None => bail!("unknown state {s:?}. Use one of: {}", STATES.join(", ")),
    }
}

/// The live sessions, when the daemon answers.
pub(super) async fn live(socket: &Path) -> Option<Vec<SessionInfo>> {
    let c = Client::connect(socket).await.ok()?;
    c.list_sessions().await.ok()
}

/// A pane as other commands take it, checked against the live sessions.
pub(super) async fn pane(socket: &Path, arg: &str) -> Result<SessionInfo> {
    let c = crate::connect(socket).await?;
    let all = c.list_sessions().await?;
    resolve::session(&all, arg).cloned()
}

/// The project whose folder holds the pane's folder, else the folder's name.
pub(super) fn project_of(s: &SessionInfo) -> String {
    let cfg = skiff_core::config::load().ok();
    cfg.iter()
        .flat_map(|c| &c.projects)
        .filter(|p| s.cwd.starts_with(&p.path))
        .max_by_key(|p| p.path.components().count())
        .map(|p| p.name.clone())
        .unwrap_or_else(|| s.cwd.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default())
}

/// The row `arg` names: its id (digits only) or its uid.
pub(super) fn one(db: &Connection, arg: &str) -> Result<Row> {
    let found = if !arg.is_empty() && arg.bytes().all(|b| b.is_ascii_digit()) {
        rows(db, "WHERE id = ?1", Some(&arg.parse::<i64>().context("id out of range")?))?
    } else {
        rows(db, "WHERE uid = ?1", Some(&arg.to_ascii_lowercase()))?
    };
    found.into_iter().next().with_context(|| format!("no ledger row {arg}"))
}

/// The work head `arg` names: a plan, or a task or hand-off with no plan.
pub(super) fn head(db: &Connection, arg: &str) -> Result<Row> {
    let r = one(db, arg)?;
    if let Some(p) = &r.parent {
        let above = one(db, p).map(|p| p.kind).unwrap_or_else(|_| "plan".into());
        bail!("{} {} is in {above} {p}. Name the {above}. Nothing changed", r.kind, r.uid);
    }
    Ok(r)
}

/// The row `--parent arg` adds under, without a change: the hand-off or
/// plan `arg` names or holds a task of, else the lone task a new plan wraps.
pub(super) fn container(db: &Connection, arg: &str) -> Result<Row> {
    let r = one(db, arg)?;
    match &r.parent {
        Some(p) if !r.is_handoff() => one(db, p),
        _ => Ok(r),
    }
}

/// The work head above `r`: the row at the top of its parents.
pub(super) fn top(db: &Connection, r: &Row) -> Result<Row> {
    let mut at = r.clone();
    for _ in 0..4 {
        match &at.parent {
            Some(p) => at = one(db, p)?,
            None => break,
        }
    }
    Ok(at)
}

/// The rows under `r`, its hand-offs' tasks included.
pub(super) fn under(db: &Connection, r: &Row) -> Result<Vec<Row>> {
    let mut out = rows(db, "WHERE parent = ?1", Some(&r.uid))?;
    let handoffs: Vec<String> = out.iter().filter(|t| t.is_handoff()).map(|t| t.uid.clone()).collect();
    for h in handoffs {
        out.extend(rows(db, "WHERE parent = ?1", Some(&h))?);
    }
    Ok(out)
}

/// The row a task joins with `--parent arg`: a hand-off `arg` names or holds
/// a task of, the plan `arg` names or holds a task of, or a new plan that
/// wraps the task with no plan `arg` names. The new plan takes the task's
/// text, title, project, ref, type, send time, owner and close stamp. Closed
/// work opens again: the task is new work in it. Run it in a transaction.
pub(super) fn join(db: &Connection, file: &Path, arg: &str) -> Result<Row> {
    let r = one(db, arg)?;
    let r = match &r.parent {
        Some(p) if !r.is_handoff() => one(db, p)?,
        _ => r,
    };
    if r.is_plan() || r.is_handoff() {
        db.execute("UPDATE tasks SET closed_at = NULL WHERE id = ?1", [top(db, &r)?.id])?;
        return one(db, &r.uid);
    }
    let uid = super::new_uid(db)?;
    // The new plan gets its own folder. The task's folder stays where it is.
    let dir = super::work_folder(&super::results_dir(file, &r.owner), &uid, super::label(r.title.as_deref(), &r.task), None);
    super::make_work_folder(&dir)?;
    let result = super::result_in(&dir, true).display().to_string();
    db.execute(
        &format!(
            "INSERT INTO tasks (uid, kind, project, task, sent_at, state, result_path, ref, updated_at, type, title, owner, work_dir)
             VALUES (?1, 'plan', ?2, ?3, ?4, 'sent', ?5, ?6, {NOW}, ?7, ?8, ?9, ?10)"
        ),
        params![uid, r.project, r.task, r.sent_at, result, r.reference, r.task_type, r.title, r.owner, dir.display().to_string()],
    )?;
    db.execute(&format!("UPDATE tasks SET parent = ?2, closed_at = NULL, updated_at = {NOW} WHERE id = ?1"), params![r.id, uid])?;
    one(db, &uid)
}
