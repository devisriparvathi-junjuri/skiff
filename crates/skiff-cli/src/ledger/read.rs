//! The read side: the queries, liveness, the work hierarchy, the list views and the table rendering.

use std::path::Path;

use anyhow::{bail, Context, Result};
use rusqlite::{params, Connection, TransactionBehavior};
use skiff_client::Client;
use skiff_client::ledger::{heads, rows, OPEN, WORKING};
use skiff_core::session::SessionInfo;

use crate::{print, resolve};

use super::{path, Row, GRAND, NOW};

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

/// Which rows `ls` lists.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum View {
    /// The work that is not closed.
    Open,
    /// Only closed work.
    Closed,
    All,
    /// Only the done tasks the person was not told of, under their plans.
    Unreported,
}

/// Keeps the rows in `view`. A work's rows go together: a task in a plan is
/// closed with its plan.
pub(super) fn keep(rows: &mut Vec<Row>, view: View) {
    use std::collections::HashSet;
    let head = heads(rows);
    let closed: HashSet<String> = rows.iter().filter(|r| r.closed_at.is_some()).map(|r| r.uid.clone()).collect();
    let untold = |r: &Row| !r.is_plan() && r.state == "done" && r.reported_at.is_none();
    let plans: HashSet<String> = rows.iter().filter(|r| untold(r)).filter_map(|r| r.parent.clone()).collect();
    rows.retain(|r| match view {
        View::Open => !closed.contains(&head[&r.uid]),
        View::Closed => closed.contains(&head[&r.uid]),
        View::All => true,
        View::Unreported => untold(r) || plans.contains(&r.uid),
    });
}

/// The order of the groups in `ls`.
pub(super) fn rank(bucket: Option<&str>) -> u8 {
    match bucket {
        Some(WORKING) => 0,
        Some(OPEN) => 1,
        _ => 2,
    }
}

/// Each plan with its tasks under it, and each hand-off with its tasks under
/// it, by id. A row whose parent is not in `rows` stands alone. The number
/// is the depth: 0 at the top.
pub(super) fn tree(rows: Vec<Row>) -> Vec<(Row, usize)> {
    let uids: std::collections::HashSet<String> = rows.iter().map(|r| r.uid.clone()).collect();
    let under = |r: &Row| r.parent.as_ref().is_some_and(|p| uids.contains(p));
    let (children, top): (Vec<Row>, Vec<Row>) = rows.into_iter().partition(under);
    fn add(out: &mut Vec<(Row, usize)>, r: Row, depth: usize, children: &[Row]) {
        let uid = r.uid.clone();
        out.push((r, depth));
        for c in children.iter().filter(|c| c.parent.as_deref() == Some(&uid)) {
            if depth < 3 {
                add(out, c.clone(), depth + 1, children);
            }
        }
    }
    let mut out = Vec::new();
    for r in top {
        add(&mut out, r, 0, &children);
    }
    out
}

/// A result path as the table shows it: from the agent's folder when it is in
/// there, else with `~`.
fn shown_path(p: &str) -> String {
    let p = Path::new(p);
    let base = path().ok().and_then(|l| l.parent().map(Path::to_path_buf));
    match base.as_deref().and_then(|b| p.strip_prefix(b).ok()) {
        Some(rel) => rel.display().to_string(),
        None => print::tilde(p),
    }
}

/// Rows grouped by bucket. WORK names each group on its first row. `closed`:
/// add the CLOSED column, the work head's stamp.
pub(super) fn print_rows(rows: &[(Row, usize)], closed: bool) {
    let yn = |l: Option<bool>| match l {
        Some(true) => "yes",
        Some(false) => "no",
        None => "?",
    };
    let refs = rows.iter().any(|(r, _)| r.reference.is_some());
    let types = rows.iter().any(|(r, _)| r.task_type.is_some());
    let sockets = rows.iter().any(|(r, _)| r.elsewhere);
    let owners = rows.iter().any(|(r, _)| r.owner != GRAND);
    let table: Vec<Vec<String>> = rows
        .iter()
        .enumerate()
        .map(|(i, (r, child))| {
            let first = i == 0 || rows[i - 1].0.bucket != r.bucket;
            let mut row = vec![
                if first { r.bucket.unwrap_or_default().to_string() } else { String::new() },
                if *child > 0 { format!("{}└ {}", "  ".repeat(child - 1), r.id) } else { r.id.to_string() },
                r.uid.clone(),
                r.shown_state().to_string(),
                if r.is_plan() {
                    "plan".into()
                } else if r.is_handoff() {
                    format!("handoff {}", print::short_id(r.pane_id.as_deref().unwrap_or("")))
                } else {
                    print::short_id(r.pane_id.as_deref().unwrap_or("")).to_string()
                },
                if r.is_plan() { "-".into() } else if r.elsewhere { "elsewhere".into() } else { yn(r.live).to_string() },
                r.project.clone(),
                r.sent_at.clone(),
                r.result_path.as_deref().map(shown_path).unwrap_or_default(),
            ];
            if refs {
                row.push(r.reference.clone().unwrap_or_default());
            }
            if types {
                row.push(r.task_type.clone().unwrap_or_default());
            }
            if sockets {
                row.push(if r.elsewhere { r.socket.clone().unwrap_or_default() } else { String::new() });
            }
            if owners {
                row.push(r.owner.clone());
            }
            if closed {
                row.push(r.closed_at.clone().unwrap_or_default());
            }
            row.push(one_line(&r.task));
            row
        })
        .collect();
    let mut header = vec!["WORK", "ID", "UID", "STATE", "PANE", "LIVE", "PROJECT", "SENT", "RESULT"];
    if refs {
        header.push("REF");
    }
    if types {
        header.push("TYPE");
    }
    if sockets {
        header.push("SOCKET");
    }
    if owners {
        header.push("OWNER");
    }
    if closed {
        header.push("CLOSED");
    }
    header.push("TASK");
    print::table(&header, &table);
}

/// The text on one line, for a table cell: line breaks and runs of space
/// become one space.
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// One row, a field per line, as `show` prints it. `closed`: the work
/// head's stamp.
pub(super) fn print_one(r: &Row, closed: Option<&str>) {
    let yn = |l: Option<bool>| l.map_or("?", |l| if l { "yes" } else { "no" });
    let mut f: Vec<(&str, String)> = vec![("id", r.id.to_string()), ("uid", r.uid.clone()), ("kind", r.kind.clone()), ("state", r.shown_state().to_string())];
    if let Some(p) = &r.parent {
        f.push(("plan", p.clone()));
    }
    if !r.is_plan() {
        f.push(("pane", r.pane_id.clone().unwrap_or_default()));
        f.push(("live", if r.elsewhere { "elsewhere".into() } else { yn(r.live).into() }));
        if let Some(s) = &r.socket {
            f.push(("socket", s.clone()));
        }
    }
    f.extend([
        ("owner", r.owner.clone()),
        ("project", r.project.clone()),
        ("sent", r.sent_at.clone()),
        ("updated", r.updated_at.clone()),
        ("reported", r.reported_at.clone().unwrap_or_else(|| "no".into())),
        ("work", r.bucket.unwrap_or_default().to_string()),
        ("closed", closed.unwrap_or("no").to_string()),
    ]);
    if let Some(d) = &r.work_dir {
        f.push(("folder", d.clone()));
    }
    if let Some(p) = &r.result_path {
        f.push(("result", p.clone()));
    }
    if let Some(x) = &r.flushed_at {
        f.push(("flushed", x.clone()));
    }
    if let Some(x) = &r.title {
        f.push(("title", x.clone()));
    }
    if let Some(x) = &r.reference {
        f.push(("ref", x.clone()));
    }
    if let Some(x) = &r.task_type {
        f.push(("type", x.clone()));
    }
    f.push(("task", r.task.clone()));
    print::table(&["", ""], &f.into_iter().map(|(k, v)| vec![k.to_string(), v]).collect::<Vec<_>>());
}
