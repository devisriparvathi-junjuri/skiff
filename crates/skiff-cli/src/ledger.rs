//! `skiff ledger`: the Skiff agent's task record, a SQLite file in its folder.
//! The agent reads and writes it through these commands only, so a bad edit,
//! a compaction or a second writer cannot break it.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use rusqlite::{params, Connection, TransactionBehavior};
use skiff_client::Client;
use skiff_core::{
    session::{Role, SessionInfo},
    state,
};

use crate::{print, resolve, sessions, LedgerCmd};
pub use skiff_client::ledger::{path, Row, GRAND};
use skiff_client::ledger::{gen, heads, mark_live, roll_up, rows, GONE, INTERRUPTED, OPEN, VERSION, WORKING};

const STATES: [&str; 4] = ["sent", "pending", "blocked", "done"];
const TASKS: &str = "
CREATE TABLE IF NOT EXISTS tasks (
  id          INTEGER PRIMARY KEY AUTOINCREMENT,
  uid         TEXT NOT NULL UNIQUE,
  kind        TEXT NOT NULL DEFAULT 'task' CHECK (kind IN ('task', 'plan', 'handoff')),
  parent      TEXT,
  pane_id     TEXT,
  pane_gen    INTEGER,
  project     TEXT NOT NULL DEFAULT '',
  task        TEXT NOT NULL,
  sent_at     TEXT NOT NULL,
  state       TEXT NOT NULL CHECK (state IN ('sent', 'pending', 'blocked', 'done')),
  result_path TEXT,
  ref         TEXT,
  updated_at  TEXT NOT NULL,
  type        TEXT,
  socket      TEXT,
  reported_at TEXT,
  title       TEXT,
  closed_at   TEXT,
  gone_at     TEXT,
  owner       TEXT NOT NULL DEFAULT 'grand',
  reopened_at TEXT,
  work_dir    TEXT,
  flushed_at  TEXT
);
";

/// A task's state change, a new task, or a task that moves into work clears
/// its work head's `reopened_at`: the work may close by itself again (see
/// [`auto_close`]). The head of a hand-off's task is two steps up.
const REOPEN_ENDS: &str = "
CREATE TRIGGER IF NOT EXISTS reopen_ends_on_state AFTER UPDATE OF state, parent ON tasks
WHEN OLD.state IS NOT NEW.state OR OLD.parent IS NOT NEW.parent
BEGIN
  UPDATE tasks SET reopened_at = NULL
  WHERE uid = coalesce((SELECT h.parent FROM tasks h WHERE h.uid = NEW.parent), NEW.parent, NEW.uid) AND reopened_at IS NOT NULL;
END;
CREATE TRIGGER IF NOT EXISTS reopen_ends_on_add AFTER INSERT ON tasks
WHEN NEW.parent IS NOT NULL
BEGIN
  UPDATE tasks SET reopened_at = NULL
  WHERE uid = coalesce((SELECT h.parent FROM tasks h WHERE h.uid = NEW.parent), NEW.parent) AND reopened_at IS NOT NULL;
END;
";

/// The one Skiff agent session that changes this ledger, and its socket.
/// `skiff skipper` and the agent's `skiff watch` record it.
const OWNER: &str = "
CREATE TABLE IF NOT EXISTS owner (
  id      INTEGER PRIMARY KEY CHECK (id = 1),
  session TEXT NOT NULL,
  socket  TEXT NOT NULL,
  since   TEXT NOT NULL
);
";

/// Every uid the ledger gave out, so a removed row's uid is not given again.
const UIDS: &str = "CREATE TABLE IF NOT EXISTS uids (uid TEXT PRIMARY KEY);";

/// UTC, to the second, set by SQLite.
const NOW: &str = "strftime('%Y-%m-%dT%H:%M:%SZ', 'now')";

/// Opens the ledger, making it on first use. The agent's folder is made
/// first when it is missing, as the app makes it at start.
fn open() -> Result<Connection> {
    if std::env::var_os("SKIFF_LEDGER").is_none() {
        skiff_client::skipper::make_folder(&skiff_core::config::load()?)?;
    }
    open_at(&path()?)
}

fn open_at(file: &Path) -> Result<Connection> {
    let mut db = connect(file)?;
    if outdated(&db)? {
        drop(db);
        let old = file.with_extension("db.old");
        std::fs::rename(file, &old).with_context(|| format!("move {} aside", file.display()))?;
        for ext in ["db-wal", "db-shm"] {
            let _ = std::fs::remove_file(file.with_extension(ext));
        }
        eprintln!("skiff: the ledger had an old format; it is now {}, and a new ledger starts empty", old.display());
        db = connect(file)?;
    }
    db.execute_batch(TASKS)?;
    db.execute_batch(UIDS)?;
    db.execute_batch(OWNER)?;
    migrate(&mut db)?;
    db.execute_batch(REOPEN_ENDS)?;
    // Only up: an older skiff that opens the file stamps its own version, and
    // must not undo a newer one's. The columns, not the stamp, tell the shape.
    let v: i64 = db.pragma_query_value(None, "user_version", |r| r.get(0))?;
    if v < VERSION {
        db.pragma_update(None, "user_version", VERSION)?;
    }
    Ok(db)
}

/// Each step adds one column in place, when the file lacks it:
///
/// - 3 to 4: `socket`. Every row before it went out on the live daemon, the
///   only one with a Skiff agent.
/// - 4 to 5: `reported_at`. Every row stays not reported: the migration
///   cannot tell what the person saw.
/// - 5 to 6: `title`. Every row stays with none: the app takes one from the
///   task text.
/// - 6 to 7: `closed_at`. All work stays open: only the person can say that
///   work is over.
/// - 7 to 8: `gone_at`. No row has it: the next read with the session list
///   closes the tasks whose panes are gone.
/// - 8 to 9: `owner`, and the `handoff` kind. The table is built again in
///   the new shape, each row with its id and owner `grand`: before project
///   Skippers, the grand Skipper owned every row.
/// - 9 to 10: `reopened_at`. No work has it: work that is done and reported
///   closes at the next write (see [`auto_close`]).
/// - 10 to 11: `work_dir` and `flushed_at`. No row has them: a row from
///   before work folders keeps its result path, with no folder.
fn migrate(db: &mut Connection) -> Result<()> {
    add_column(db, "socket", |tx| {
        tx.execute("UPDATE tasks SET socket = ?1 WHERE kind = 'task'", [state::live_socket().display().to_string()])?;
        Ok(())
    })?;
    add_column(db, "reported_at", |_| Ok(()))?;
    add_column(db, "title", |_| Ok(()))?;
    add_column(db, "closed_at", |_| Ok(()))?;
    add_column(db, "gone_at", |_| Ok(()))?;
    add_owner(db)?;
    add_column(db, "reopened_at", |_| Ok(()))?;
    add_column(db, "work_dir", |_| Ok(()))?;
    add_column(db, "flushed_at", |_| Ok(()))
}

/// The 8 to 9 step. SQLite cannot change a CHECK in place, so the rows move
/// to a new table, in one transaction.
fn add_owner(db: &mut Connection) -> Result<()> {
    let has = |db: &Connection| -> rusqlite::Result<bool> {
        db.query_row("SELECT count(*) FROM pragma_table_info('tasks') WHERE name = 'owner'", [], |r| r.get::<_, i64>(0)).map(|n| n > 0)
    };
    if has(db)? {
        return Ok(());
    }
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if !has(&tx)? {
        let old = "id, uid, kind, parent, pane_id, pane_gen, project, task, sent_at, state, result_path, ref, updated_at, type, socket, reported_at, title, closed_at, gone_at";
        // Version 9's shape: the later columns come in the next steps.
        tx.execute_batch(&TASKS.replace("IF NOT EXISTS tasks", "tasks_9").replace(",\n  reopened_at TEXT,\n  work_dir    TEXT,\n  flushed_at  TEXT", ""))?;
        tx.execute_batch(&format!("INSERT INTO tasks_9 ({old}, owner) SELECT {old}, 'grand' FROM tasks; DROP TABLE tasks; ALTER TABLE tasks_9 RENAME TO tasks;"))?;
    }
    tx.commit()?;
    Ok(())
}

/// Adds the column `name TEXT` to `tasks` and runs `fill`, unless it is
/// there. One transaction, so two first opens at once add it once.
fn add_column(db: &mut Connection, name: &str, fill: impl FnOnce(&Connection) -> Result<()>) -> Result<()> {
    let has = |db: &Connection| -> rusqlite::Result<bool> {
        db.query_row("SELECT count(*) FROM pragma_table_info('tasks') WHERE name = ?1", [name], |r| r.get::<_, i64>(0)).map(|n| n > 0)
    };
    if has(db)? {
        return Ok(());
    }
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if !has(&tx)? {
        tx.execute_batch(&format!("ALTER TABLE tasks ADD COLUMN {name} TEXT"))?;
        fill(&tx)?;
    }
    tx.commit()?;
    Ok(())
}

fn connect(file: &Path) -> Result<Connection> {
    let db = Connection::open(file).with_context(|| format!("open {}", file.display()))?;
    db.busy_timeout(std::time::Duration::from_secs(5))?;
    db.pragma_update(None, "journal_mode", "WAL")?;
    Ok(db)
}

/// A `tasks` table from before version 3, which this build does not read.
fn outdated(db: &Connection) -> Result<bool> {
    let cols: i64 = db.query_row("SELECT count(*) FROM pragma_table_info('tasks') WHERE name IN ('uid', 'type')", [], |r| r.get(0))?;
    let table: i64 = db.query_row("SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'tasks'", [], |r| r.get(0))?;
    Ok(table > 0 && cols < 2)
}

/// A uid no row ever had: 8 hex characters with at least one letter.
fn new_uid(db: &Connection) -> Result<String> {
    loop {
        let uid = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
        if uid.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        if db.execute("INSERT OR IGNORE INTO uids (uid) VALUES (?1)", [&uid])? == 1 {
            return Ok(uid);
        }
    }
}

/// Lowercase ASCII words joined by `-`, at most 40 characters. Other
/// characters split words. Empty text gives `task`.
fn slug(text: &str) -> String {
    let mut out = String::new();
    for w in text.split(|c: char| !c.is_ascii_alphanumeric()).filter(|w| !w.is_empty()) {
        let w = w.to_ascii_lowercase();
        let sep = usize::from(!out.is_empty());
        if out.len() + sep + w.len() > 40 {
            if out.is_empty() {
                out = w[..40].to_string();
            }
            break;
        }
        if sep == 1 {
            out.push('-');
        }
        out.push_str(&w);
    }
    if out.is_empty() {
        "task".into()
    } else {
        out
    }
}

/// A row's work folder, in `results`: the folder next to the ledger, or a
/// project or fleet Skipper's (see [`results_dir`]). Everything for the task
/// goes in it. The name is `<uid>-<slug>`, from the title, else the task
/// text, and never changes:
///
/// - a plan, a hand-off, or a task with no plan: `<results>/<uid>-<slug>/`;
/// - a task in a plan or under a hand-off: `<uid>-<slug>/` in that row's
///   folder (see [`container_folder`]).
///
/// Nothing is made on disk.
fn work_folder(results: &Path, uid: &str, label: &str, container: Option<&Row>) -> PathBuf {
    let name = format!("{uid}-{}", slug(label));
    match container {
        Some(p) => container_folder(results, p).join(name),
        None => results.join(name),
    }
}

/// The result in a work folder: `plan.md` for a plan, else `result.md`.
fn result_in(folder: &Path, plan: bool) -> PathBuf {
    folder.join(if plan { "plan.md" } else { "result.md" })
}

/// The title, else the task text: what a work folder's slug comes from.
fn label<'a>(title: Option<&'a str>, task: &'a str) -> &'a str {
    title.map(str::trim).filter(|t| !t.is_empty()).unwrap_or(task)
}

/// The folder a task under `p` goes in: `p`'s work folder, or for a row from
/// before work folders, the plan folder it had (see [`plan_folder`]).
fn container_folder(results: &Path, p: &Row) -> PathBuf {
    match &p.work_dir {
        Some(d) => PathBuf::from(d),
        None => plan_folder(results, p),
    }
}

/// Makes a work folder. The ledger records it only when this works.
fn make_work_folder(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("make the work folder {}", dir.display()))
}

/// The results folder of `owner`'s rows: `results/` next to the ledger for
/// the grand Skipper, `projects/<project>/results/` for a project Skipper,
/// `fleets/<fleet>/results/` for a fleet Skipper.
pub fn results_dir(ledger: &Path, owner: &str) -> PathBuf {
    let dir = ledger.parent().unwrap_or(Path::new("."));
    match (owner, skiff_client::skipper::scope_fleet(owner)) {
        (GRAND, _) => dir.join("results"),
        (_, Some(fleet)) => dir.join("fleets").join(fleet).join("results"),
        _ => dir.join("projects").join(owner).join("results"),
    }
}

/// For a row from before work folders: the folder of the plan's `plan.md`,
/// or the folder of a hand-off's tasks. A plan whose result is elsewhere
/// gets the folder it would have had.
fn plan_folder(results: &Path, p: &Row) -> PathBuf {
    if let Some(dir) = p.result_path.as_deref().map(Path::new).filter(|r| r.file_name() == Some("plan.md".as_ref())).and_then(Path::parent) {
        return dir.to_path_buf();
    }
    results.join(format!("{}-{}-{}", p.sent_at.get(..10).unwrap_or(&p.sent_at), p.uid, slug(&p.task)))
}

/// The open tasks (state sent, pending or blocked), oldest first. Plans are
/// left out. This is the read path for other commands, such as `skiff
/// watch`. No `ledger.db` yet gives an empty list, and makes no file.
pub fn open_tasks() -> Result<Vec<Row>> {
    open_tasks_at(&path()?)
}

fn open_tasks_at(file: &Path) -> Result<Vec<Row>> {
    if !file.exists() {
        return Ok(Vec::new());
    }
    let db = open_at(file)?;
    rows(&db, "WHERE kind IN ('task', 'handoff') AND state IN ('sent', 'pending', 'blocked')", None)
}

/// The done tasks of `owner` the person was not told of yet. `skiff watch`
/// names the count at its start. No `ledger.db` yet gives 0, and makes no file.
pub fn unreported(owner: &str) -> Result<usize> {
    unreported_at(&path()?, owner)
}

fn unreported_at(file: &Path, owner: &str) -> Result<usize> {
    if !file.exists() {
        return Ok(0);
    }
    let db = open_at(file)?;
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
    close_gone_in(&mut open_at(&file)?, here, all)
}

fn close_gone_in(db: &mut Connection, here: &Path, all: &[SessionInfo]) -> Result<Vec<Row>> {
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
fn sweep(db: &mut Connection, here: &Path, all: Option<&[SessionInfo]>) -> Result<()> {
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
    rows(&open_at(&file)?, "WHERE kind IN ('task', 'handoff') AND state = 'done' AND gone_at IS NOT NULL AND reported_at IS NULL", None)
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
        &open_at(&file)?,
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
    rows(&open_at(&file)?, "WHERE kind = 'handoff' AND state = 'done' AND gone_at IS NULL AND reported_at IS NULL", None)
}

fn state(s: &str) -> Result<&str> {
    match STATES.iter().find(|x| **x == s) {
        Some(x) => Ok(x),
        None => bail!("unknown state {s:?}. Use one of: {}", STATES.join(", ")),
    }
}

/// The live sessions, when the daemon answers.
async fn live(socket: &Path) -> Option<Vec<SessionInfo>> {
    let c = Client::connect(socket).await.ok()?;
    c.list_sessions().await.ok()
}

/// A pane as other commands take it, checked against the live sessions.
async fn pane(socket: &Path, arg: &str) -> Result<SessionInfo> {
    let c = crate::connect(socket).await?;
    let all = c.list_sessions().await?;
    resolve::session(&all, arg).cloned()
}

/// The project whose folder holds the pane's folder, else the folder's name.
fn project_of(s: &SessionInfo) -> String {
    let cfg = skiff_core::config::load().ok();
    cfg.iter()
        .flat_map(|c| &c.projects)
        .filter(|p| s.cwd.starts_with(&p.path))
        .max_by_key(|p| p.path.components().count())
        .map(|p| p.name.clone())
        .unwrap_or_else(|| s.cwd.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default())
}

/// The row `arg` names: its id (digits only) or its uid.
fn one(db: &Connection, arg: &str) -> Result<Row> {
    let found = if !arg.is_empty() && arg.bytes().all(|b| b.is_ascii_digit()) {
        rows(db, "WHERE id = ?1", Some(&arg.parse::<i64>().context("id out of range")?))?
    } else {
        rows(db, "WHERE uid = ?1", Some(&arg.to_ascii_lowercase()))?
    };
    found.into_iter().next().with_context(|| format!("no ledger row {arg}"))
}

/// The work head `arg` names: a plan, or a task or hand-off with no plan.
fn head(db: &Connection, arg: &str) -> Result<Row> {
    let r = one(db, arg)?;
    if let Some(p) = &r.parent {
        let above = one(db, p).map(|p| p.kind).unwrap_or_else(|_| "plan".into());
        bail!("{} {} is in {above} {p}. Name the {above}. Nothing changed", r.kind, r.uid);
    }
    Ok(r)
}

/// The row `--parent arg` adds under, without a change: the hand-off or
/// plan `arg` names or holds a task of, else the lone task a new plan wraps.
fn container(db: &Connection, arg: &str) -> Result<Row> {
    let r = one(db, arg)?;
    match &r.parent {
        Some(p) if !r.is_handoff() => one(db, p),
        _ => Ok(r),
    }
}

/// The work head above `r`: the row at the top of its parents.
fn top(db: &Connection, r: &Row) -> Result<Row> {
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
fn under(db: &Connection, r: &Row) -> Result<Vec<Row>> {
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
fn join(db: &Connection, file: &Path, arg: &str) -> Result<Row> {
    let r = one(db, arg)?;
    let r = match &r.parent {
        Some(p) if !r.is_handoff() => one(db, p)?,
        _ => r,
    };
    if r.is_plan() || r.is_handoff() {
        db.execute("UPDATE tasks SET closed_at = NULL WHERE id = ?1", [top(db, &r)?.id])?;
        return one(db, &r.uid);
    }
    let uid = new_uid(db)?;
    // The new plan gets its own folder. The task's folder stays where it is.
    let dir = work_folder(&results_dir(file, &r.owner), &uid, label(r.title.as_deref(), &r.task), None);
    make_work_folder(&dir)?;
    let result = result_in(&dir, true).display().to_string();
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
enum View {
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
fn keep(rows: &mut Vec<Row>, view: View) {
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
fn rank(bucket: Option<&str>) -> u8 {
    match bucket {
        Some(WORKING) => 0,
        Some(OPEN) => 1,
        _ => 2,
    }
}

/// Each plan with its tasks under it, and each hand-off with its tasks under
/// it, by id. A row whose parent is not in `rows` stands alone. The number
/// is the depth: 0 at the top.
fn tree(rows: Vec<Row>) -> Vec<(Row, usize)> {
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
fn print_rows(rows: &[(Row, usize)], closed: bool) {
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
fn print_one(r: &Row, closed: Option<&str>) {
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

/// A row for [`insert`].
struct New {
    plan: bool,
    /// The plan's id or uid.
    parent: Option<String>,
    pane_id: Option<String>,
    pane_gen: Option<i64>,
    project: String,
    task: String,
    title: Option<String>,
    state: String,
    result: Option<String>,
    reference: Option<String>,
    task_type: Option<String>,
    /// The socket of the pane.
    socket: Option<String>,
    /// [`GRAND`], or a project Skipper's project.
    owner: String,
    /// A hand-off: a task for a project Skipper.
    handoff: bool,
}

/// Adds a row with a new uid, and makes its work folder (see
/// [`work_folder`]). The result path is `result.md` in it (`plan.md` for a
/// plan) unless the row names one. A parent is joined as [`join`] does.
/// Returns the id, the uid, the result path and the plan's uid.
fn insert(db: &mut Connection, file: &Path, n: New) -> Result<(i64, String, String, Option<String>)> {
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let parent = n.parent.as_deref().map(|p| join(&tx, file, p)).transpose()?;
    let uid = new_uid(&tx)?;
    // A hand-off's folder goes with the project Skipper's, not in the plan's folder.
    let (results, above) = if n.handoff { (results_dir(file, &n.project), None) } else { (results_dir(file, &n.owner), parent.as_ref()) };
    let dir = work_folder(&results, &uid, label(n.title.as_deref(), &n.task), above);
    make_work_folder(&dir)?;
    let result = n.result.unwrap_or_else(|| result_in(&dir, n.plan).display().to_string());
    let kind = match (n.plan, n.handoff) {
        (true, _) => "plan",
        (_, true) => "handoff",
        _ => "task",
    };
    tx.execute(
        &format!(
            "INSERT INTO tasks (uid, kind, parent, pane_id, pane_gen, project, task, sent_at, state, result_path, ref, updated_at, type, socket, title, owner, work_dir)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, {NOW}, ?8, ?9, ?10, {NOW}, ?11, ?12, ?13, ?14, ?15)"
        ),
        params![
            uid,
            kind,
            parent.as_ref().map(|p| &p.uid),
            n.pane_id,
            n.pane_gen,
            n.project,
            n.task,
            n.state,
            result,
            n.reference,
            n.task_type,
            n.socket,
            n.title,
            n.owner,
            dir.display().to_string()
        ],
    )?;
    let id = tx.last_insert_rowid();
    tx.commit()?;
    Ok((id, uid, result, parent.map(|p| p.uid)))
}

/// The session that owns the ledger, and its socket.
#[derive(Debug, Clone, PartialEq)]
pub struct Owner {
    pub session: String,
    pub socket: String,
}

fn owner(db: &Connection) -> Result<Option<Owner>> {
    let mut st = db.prepare("SELECT session, socket FROM owner WHERE id = 1")?;
    let mut it = st.query_map([], |r| Ok(Owner { session: r.get(0)?, socket: r.get(1)? }))?;
    Ok(it.next().transpose()?)
}

fn set_owner(db: &Connection, session: &str, socket: &Path) -> Result<()> {
    db.execute(
        &format!("INSERT INTO owner (id, session, socket, since) VALUES (1, ?1, ?2, {NOW}) ON CONFLICT (id) DO UPDATE SET session = ?1, socket = ?2, since = {NOW}"),
        params![session, socket.display().to_string()],
    )?;
    Ok(())
}

/// The owner is `session` on `socket`.
fn is_owner(cur: Option<&Owner>, session: &str, socket: &Path) -> bool {
    cur.is_some_and(|o| o.session == session && state::same_socket(Path::new(&o.socket), socket))
}

/// The Skiff agent session of `socket` may take the ledger from `cur`: when
/// it has no owner, when the owner was on the same daemon (a new Skiff agent
/// session there replaces the old one), or when `socket` is the live one. A
/// daemon runs one Skiff agent, so the live agent always wins, and an agent
/// on another socket never takes the live agent's ledger.
fn may_claim(cur: Option<&Owner>, socket: &Path) -> bool {
    match cur {
        None => true,
        Some(o) => state::same_socket(Path::new(&o.socket), socket) || state::is_live(socket),
    }
}

/// Records `session`, the Skiff agent's session on `socket`, as the owner,
/// when [`may_claim`] allows it. `skiff skipper` and the agent's `skiff
/// watch` call it, so a new Skiff agent session takes over at its start.
pub fn claim(socket: &Path, session: &str) -> Result<()> {
    claim_at(&path()?, socket, session)
}

fn claim_at(file: &Path, socket: &Path, session: &str) -> Result<()> {
    if !file.parent().is_some_and(Path::is_dir) {
        return Ok(());
    }
    let db = open_at(file)?;
    let tx = db.unchecked_transaction()?;
    let cur = owner(&tx)?;
    if is_owner(cur.as_ref(), session, socket) {
        return Ok(());
    }
    if !may_claim(cur.as_ref(), socket) {
        let o = cur.expect("only an owner refuses");
        let name = main_name();
        bail!("this ledger is {name}'s on {}. Do ledger work from {name} there", o.socket);
    }
    set_owner(&tx, session, socket)?;
    tx.commit()?;
    Ok(())
}

/// Who changes the ledger: a coordinator, or the person.
#[derive(Debug, Clone, PartialEq)]
pub enum Caller {
    /// The grand Skipper: the Skiff agent session that owns the ledger.
    Grand,
    /// A project or fleet Skipper, with its scope: a project's name, or
    /// `fleet:<fleet>`.
    Project(String),
    /// A shell pane, or a shell outside Skiff with `--person`.
    Person,
}

impl Caller {
    /// The owner of the rows this caller adds. The person's rows are the
    /// grand Skipper's.
    fn owner(&self) -> &str {
        match self {
            Caller::Project(p) => p,
            Caller::Grand | Caller::Person => GRAND,
        }
    }

    /// The caller may change `r`: the person any row, a coordinator its own.
    fn owns(&self, r: &Row) -> bool {
        *self == Caller::Person || r.owner == self.owner()
    }

    /// Refuses a row the caller does not own.
    fn check(&self, r: &Row) -> Result<()> {
        if self.owns(r) {
            return Ok(());
        }
        let ask = if r.owner == GRAND { format!("Ask {}", main_name()) } else { format!("Ask that {}", skipper_name()) };
        bail!("{} is {}'s {}. {ask} to change it", r.uid, who(&r.owner), kind_word(&r.kind))
    }

    /// A project Skipper may also report the hand-off it was given: set its
    /// state and its result, and nothing else.
    fn check_report(&self, r: &Row) -> Result<()> {
        match self {
            Caller::Project(p) if r.is_handoff() && r.project == *p => Ok(()),
            _ => self.check(r),
        }
    }

    /// Refuses a parent the caller may not add under. A coordinator adds
    /// under its own plans. Under a hand-off, only its project's Skipper
    /// (or the person) adds; the grand Skipper cannot. Returns the owner of
    /// the new row.
    fn check_parent(&self, p: &Row) -> Result<String> {
        if p.is_handoff() {
            return match self {
                Caller::Project(me) if *me == p.project => Ok(me.clone()),
                Caller::Person => Ok(p.project.clone()),
                _ => bail!("{} is a hand-off to {}. Ask that Skipper to add tasks under it", p.uid, who(&p.project)),
            };
        }
        self.check(p)?;
        Ok(if *self == Caller::Person { p.owner.clone() } else { self.owner().to_string() })
    }
}

/// What a change to the ledger from `me` (`$SKIFF_SESSION`), whose session
/// on `socket` is `s`, may do.
#[derive(Debug, PartialEq)]
enum Writer {
    Yes(Caller),
    /// The Skiff agent of this socket takes the ledger first.
    Claim,
    /// A Skipper a fleet Skipper took over: it ends when idle.
    Retired,
    No,
}

/// A shell pane is a person: it may. A shell outside Skiff (no `me`) may only
/// with `person` (`--person`): an agent there has no `SKIFF_SESSION` either,
/// and nothing else tells the two apart. The owner may, as the grand
/// Skipper. A project Skipper may, for its project. Any other pane is
/// refused, a Skiff agent on another socket included, unless [`may_claim`]
/// lets the Skiff agent of `socket` take it.
fn writer(me: Option<&str>, s: Option<&SessionInfo>, cur: Option<&Owner>, socket: &Path, person: bool) -> Writer {
    let Some(me) = me else { return if person { Writer::Yes(Caller::Person) } else { Writer::No } };
    if is_owner(cur, me, socket) {
        return Writer::Yes(Caller::Grand);
    }
    match s {
        Some(s) if s.coordinator && may_claim(cur, socket) => Writer::Claim,
        Some(s) if s.retiring => Writer::Retired,
        Some(s) if s.skipper.is_some() && !s.coordinator => Writer::Yes(Caller::Project(s.skipper.clone().unwrap_or_default())),
        Some(s) if s.role == Role::Shell && !s.coordinator => Writer::Yes(Caller::Person),
        _ => Writer::No,
    }
}

/// Who calls, from `$SKIFF_SESSION` and the live sessions. Refuses a pane
/// that may not change the ledger. It runs before the change.
async fn check_writer(socket: &Path, db: &Connection, person: bool) -> Result<Caller> {
    let me = std::env::var("SKIFF_SESSION").ok().filter(|s| !s.is_empty());
    let cur = owner(db)?;
    let all = match &me {
        Some(m) if !is_owner(cur.as_ref(), m, socket) => live(socket).await.unwrap_or_default(),
        _ => Vec::new(),
    };
    let s = me.as_deref().and_then(|m| all.iter().find(|s| s.id == m));
    match writer(me.as_deref(), s, cur.as_ref(), socket, person) {
        Writer::Yes(c) => Ok(c),
        Writer::Claim => set_owner(db, &s.expect("a claim has a session").id, socket).map(|_| Caller::Grand),
        Writer::Retired => {
            let s = s.expect("a retired caller has a session");
            let project = project_of(s);
            let fleet = skiff_core::config::load().ok().and_then(|c| c.projects.into_iter().find(|p| p.name == project)).and_then(|p| p.fleet);
            match fleet {
                Some(f) => bail!("the {f} Skipper commands {project} now. This session ends when it is idle"),
                None => bail!("a fleet Skipper commands {project} now. This session ends when it is idle"),
            }
        }
        Writer::No if me.is_none() => bail!("this shell is outside Skiff, so Skiff cannot tell a person from an agent. If you are the person, run the command again with --person"),
        Writer::No => bail!("{}", refusal(cur.as_ref(), s.is_some_and(|s| s.coordinator))),
    }
}

/// The refusal for an agent pane. `skipper`: the caller is Skipper on
/// another socket, which may not take the ledger.
fn refusal(cur: Option<&Owner>, skipper: bool) -> String {
    match cur {
        Some(o) if skipper => format!("this ledger is {}'s on {}. Ask {} there to make this change", main_name(), o.socket, main_name()),
        Some(_) => format!("only {} and the project and fleet {}s change the ledger. Ask {} to make this change", main_name(), skipper_name(), main_name()),
        None => format!("this ledger has no {} yet. Start {} in the Skiff app, then ask it to make this change", main_name(), main_name()),
    }
}

/// "Admiral", "the <project> project Skipper" or "the <fleet> fleet
/// Skipper": a row's owner, or a hand-off's scope.
fn who(owner: &str) -> String {
    match (owner, skiff_client::skipper::scope_fleet(owner)) {
        (GRAND, _) => main_name(),
        (_, Some(f)) => format!("the {f} fleet {}", skipper_name()),
        (p, _) => format!("the {p} project {}", skipper_name()),
    }
}

fn main_name() -> String {
    skiff_client::skipper::name(&skiff_core::config::load().unwrap_or_default())
}

fn skipper_name() -> String {
    skiff_core::config::load().unwrap_or_default().skiff_agent.skipper_word()
}

/// A fleet Skipper sends only into the projects of its fleet, as
/// `projects.toml` has it now. Refuses a pane in another project, with
/// both names. Any other caller may send anywhere.
fn check_scope(caller: &Caller, s: &SessionInfo) -> Result<()> {
    let Caller::Project(scope) = caller else { return Ok(()) };
    let Some(fleet) = skiff_client::skipper::scope_fleet(scope) else { return Ok(()) };
    let cfg = skiff_core::config::load()?;
    let project = project_of(s);
    if cfg.projects.iter().any(|p| p.name == project && p.fleet.as_deref() == Some(fleet)) {
        return Ok(());
    }
    bail!("pane {} is in {project}, which is not in {fleet}. Nothing changed", resolve::display_name(s))
}

/// What `handoff SCOPE` names: a project or a fleet, as an owner key.
#[derive(Debug, PartialEq)]
enum Scope {
    Project(String),
    Fleet(String),
}

/// Reads SCOPE: `project:NAME`, `fleet:NAME`, or a bare name that is one
/// of the two. A bare name that is both is refused with both spellings.
fn scope(cfg: &skiff_core::config::Config, arg: &str) -> Result<Scope> {
    if let Some(p) = arg.strip_prefix("project:") {
        return Ok(Scope::Project(p.to_string()));
    }
    if let Some(f) = skiff_client::skipper::scope_fleet(arg) {
        if !skiff_client::skipper::is_fleet(cfg, f) {
            bail!("no fleet {f:?} in {}", skiff_core::config::config_path().display());
        }
        return Ok(Scope::Fleet(f.to_string()));
    }
    let project = cfg.projects.iter().any(|p| p.name == arg);
    match (project, skiff_client::skipper::is_fleet(cfg, arg)) {
        (true, true) => bail!("{arg} is a project and a fleet. Use project:{arg} or fleet:{arg}. Nothing changed"),
        (false, true) => Ok(Scope::Fleet(arg.to_string())),
        _ => Ok(Scope::Project(arg.to_string())),
    }
}

/// "hand-off" for `handoff`, else the kind as it is.
fn kind_word(kind: &str) -> &str {
    match kind {
        "handoff" => "hand-off",
        k => k,
    }
}

/// What `ledger send` and `ledger add --send` type, and how.
pub struct SendOpts {
    /// Default: the task text and its result path.
    pub text: Vec<String>,
    pub enter: bool,
    pub shell: bool,
}

/// Why `row`, whose pane is `s` now, cannot be sent: only a pending task, or
/// one a restart interrupted, has a send to make.
fn unsendable(row: &Row, s: Option<&SessionInfo>) -> Option<String> {
    if row.is_plan() {
        return Some(format!("ledger row {} is a plan: it has no pane to send to", row.uid));
    }
    if row.state != "pending" && !row.interrupted_in(s) {
        return Some(format!("task {} is {}, not pending or interrupted. Nothing was sent", row.uid, row.state));
    }
    s.is_none().then(|| format!("the pane of task {} is gone. Nothing was sent", row.uid))
}

/// The text a task goes out with when no text is given. A hand-off says how
/// to report it.
fn message(row: &Row) -> String {
    let task = row.task.trim();
    if row.is_handoff() {
        let path = row.result_path.as_deref().unwrap_or("your results folder");
        let folder = row.work_dir.as_deref().map(|d| format!(" Put everything for this hand-off in {d}. Your tasks for it get their folders in it.")).unwrap_or_default();
        return format!(
            "Hand-off {uid}: {task}{folder} Add your tasks for it with --parent {uid}. When they are done, write the result to {path}, then run: skiff ledger update {uid} --state done --result {path}",
            uid = row.uid
        );
    }
    match (row.work_dir.as_deref(), row.result_path.as_deref()) {
        (Some(d), Some(p)) => format!("{task} Put everything for this task in {d}: the result, screenshots, scratch copies and worktrees. Write your result to {p}."),
        (None, Some(p)) => format!("{task} Write your result to {p}."),
        _ => task.to_string(),
    }
}

/// The state a task takes after `send` exits with `code`. A refusal typed
/// nothing, so the task stays pending. Exit 8 typed the text but started no
/// turn: the pane needs the person.
fn state_after(code: i32) -> Option<&'static str> {
    match code {
        0 => Some("sent"),
        8 => Some("blocked"),
        _ => None,
    }
}

/// Sends the pending task `row` to its pane with `send`'s checks, then
/// records the outcome. Returns `send`'s exit code.
async fn send_row(socket: &Path, db: &Connection, row: &Row, o: SendOpts) -> Result<i32> {
    if let Some(other) = row.socket_other_than(socket) {
        bail!("task {} went to a pane on {other}, not on {}. Nothing was sent", row.uid, socket.display());
    }
    let c = crate::connect(socket).await?;
    let all = c.list_sessions().await?;
    let s = row.pane_id.as_deref().and_then(|p| resolve::session(&all, p).ok());
    if let Some(why) = unsendable(row, s) {
        bail!(why);
    }
    let s = s.expect("checked");
    let text = if o.text.is_empty() { vec![message(row)] } else { o.text };
    let code = sessions::send_to(&c, s, sessions::Send { text: &text, enter: o.enter, raw: false, shell: o.shell }).await?;
    match state_after(code) {
        Some(st) => {
            // Only from the state read: a send that raced this one keeps its own.
            db.execute(
                &format!("UPDATE tasks SET state = ?2, pane_gen = ?3, updated_at = {NOW} WHERE id = ?1 AND state = ?4"),
                params![row.id, st, gen(s), row.state],
            )?;
            if st != "sent" {
                eprintln!("skiff: task {} is {st}", row.uid);
            }
        }
        None => eprintln!("skiff: task {} stays {}", row.uid, row.state),
    }
    Ok(code)
}

/// Stamps `reported_at` on the done rows `ids` name, in one transaction.
/// A plan with tasks stamps its done tasks and leaves the others; a plan with
/// none is stamped itself. A hand-off is stamped itself. A row that is not
/// done, or that `caller` does not own, refuses the lot. A first stamp stays.
/// `all`: every done task the caller owns, and every done plan with no task.
/// Returns how many rows took a new stamp.
fn report(db: &mut Connection, caller: &Caller, ids: &[String], all: bool) -> Result<usize> {
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let stamp = format!("UPDATE tasks SET reported_at = {NOW} WHERE state = 'done' AND reported_at IS NULL");
    let mut n = 0;
    if all {
        let mine = if *caller == Caller::Person { "1" } else { "owner = ?1" };
        let sql = format!("{stamp} AND {mine} AND (kind IN ('task', 'handoff') OR NOT EXISTS (SELECT 1 FROM tasks t WHERE t.parent = tasks.uid))");
        n += if *caller == Caller::Person { tx.execute(&sql, [])? } else { tx.execute(&sql, [caller.owner()])? };
    }
    for id in ids {
        let r = one(&tx, id)?;
        caller.check(&r)?;
        let tasks = if r.is_plan() { rows(&tx, "WHERE parent = ?1", Some(&r.uid))? } else { Vec::new() };
        if !tasks.is_empty() {
            n += tx.execute(&format!("{stamp} AND parent = ?1"), [&r.uid])?;
            continue;
        }
        if r.state != "done" {
            bail!("{} {} is {}, not done. Nothing changed", r.kind, r.uid, r.state);
        }
        n += tx.execute(&format!("{stamp} AND id = ?1"), [r.id])?;
    }
    tx.commit()?;
    Ok(n)
}

/// A row set to a state other than done opens again. That is new work: its
/// report stamp goes, and its work opens again.
fn restate(db: &Connection, r: &Row, st: Option<&str>) -> Result<()> {
    if st.is_some_and(|s| s != "done") {
        db.execute("UPDATE tasks SET reported_at = NULL WHERE id = ?1", [r.id])?;
        db.execute("UPDATE tasks SET closed_at = NULL WHERE id = ?1", [top(db, r)?.id])?;
    }
    Ok(())
}

/// Stamps `closed_at` on the work `ids` name, in one transaction, and
/// `reported_at` on its done tasks not yet reported. Work with a task that is
/// not done, a hand-off's tasks included, refuses the lot, as does work
/// `caller` does not own. A first stamp stays. Returns how many work items
/// took a new stamp.
fn close(db: &mut Connection, caller: &Caller, ids: &[String]) -> Result<usize> {
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let mut n = 0;
    for id in ids {
        let r = head(&tx, id)?;
        caller.check(&r)?;
        let mut tasks = under(&tx, &r)?;
        if !r.is_plan() {
            tasks.insert(0, r.clone());
        }
        if let Some(t) = tasks.iter().find(|t| t.state != "done") {
            bail!("{} {} is {}, not done. Set it done or remove it first. Nothing changed", t.kind, t.uid, t.state);
        }
        if r.closed_at.is_some() {
            continue;
        }
        for t in tasks.iter().filter(|t| t.reported_at.is_none()) {
            tx.execute(&format!("UPDATE tasks SET reported_at = {NOW} WHERE id = ?1 AND kind IN ('task', 'handoff')"), [t.id])?;
        }
        n += tx.execute(&format!("UPDATE tasks SET closed_at = {NOW}, reopened_at = NULL WHERE id = ?1"), [r.id])?;
    }
    tx.commit()?;
    Ok(n)
}

/// Clears `closed_at` on the work `ids` name and stamps `reopened_at`, in one
/// transaction: the work stays open, also when it is done and reported, until
/// a task in it changes state. Open work takes the stamp too, so it does not
/// close by itself. Returns how many work items took it.
fn reopen(db: &mut Connection, caller: &Caller, ids: &[String]) -> Result<usize> {
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let mut n = 0;
    for id in ids {
        let r = head(&tx, id)?;
        caller.check(&r)?;
        n += tx.execute(&format!("UPDATE tasks SET closed_at = NULL, reopened_at = {NOW} WHERE id = ?1"), [r.id])?;
    }
    tx.commit()?;
    Ok(n)
}

/// Closes, as `close` does, each open work item whose tasks are all done and
/// reported: a plan's tasks, a hand-off and its tasks, or a lone task. A plan
/// with no task stays open, as does work with `reopened_at`. Returns the work
/// heads it closed.
fn auto_close(db: &mut Connection) -> Result<Vec<Row>> {
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let all = rows(&tx, "", None)?;
    let held: std::collections::HashSet<String> = {
        let mut st = tx.prepare("SELECT uid FROM tasks WHERE reopened_at IS NOT NULL")?;
        let out = st.query_map([], |r| r.get::<_, String>(0))?.collect::<rusqlite::Result<_>>()?;
        out
    };
    let head = heads(&all);
    // Each open work head, with whether every task in it is done and reported.
    let mut ready: std::collections::BTreeMap<&str, bool> = std::collections::BTreeMap::new();
    for r in &all {
        let h = head[&r.uid].as_str();
        if r.is_plan() && r.uid == h {
            continue;
        }
        let done = r.state == "done" && r.reported_at.is_some();
        ready.entry(h).and_modify(|ok| *ok &= done).or_insert(done);
    }
    let mut out = Vec::new();
    for (h, ok) in ready {
        let Some(r) = all.iter().find(|r| r.uid == h) else { continue };
        if !ok || r.closed_at.is_some() || held.contains(h) {
            continue;
        }
        if tx.execute(&format!("UPDATE tasks SET closed_at = {NOW} WHERE id = ?1 AND closed_at IS NULL"), [r.id])? == 1 {
            out.push(r.clone());
        }
    }
    tx.commit()?;
    Ok(out)
}

fn work_word(n: usize) -> &'static str {
    if n == 1 {
        "work item"
    } else {
        "work items"
    }
}

fn rows_word(n: usize) -> &'static str {
    if n == 1 {
        "row"
    } else {
        "rows"
    }
}

/// `7d`, `12h`, `30m` or `45s`, in seconds.
fn age(s: &str) -> Result<i64> {
    let unit = match s.chars().last() {
        Some('d') => 86_400,
        Some('h') => 3_600,
        Some('m') => 60,
        Some('s') => 1,
        _ => bail!("cannot read the age {s:?}. Use a number and d, h, m or s, such as 7d"),
    };
    let n: i64 = s[..s.len() - 1].parse().with_context(|| format!("cannot read the age {s:?}. Use a number and d, h, m or s, such as 7d"))?;
    Ok(n * unit)
}

/// The rows `prune` takes: the work `caller` owns closed at least `secs`
/// ago, each with its tasks. Open work never goes. `yes`: delete them.
fn prune(db: &mut Connection, caller: &Caller, secs: i64, yes: bool) -> Result<Vec<Row>> {
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let cutoff: String = tx.query_row("SELECT strftime('%Y-%m-%dT%H:%M:%SZ', 'now', ?1)", [format!("-{secs} seconds")], |r| r.get(0))?;
    let all = rows(&tx, "", None)?;
    let head = heads(&all);
    let old: std::collections::HashSet<&str> = all
        .iter()
        .filter(|r| caller.owns(r) && r.closed_at.as_deref().is_some_and(|t| t <= cutoff.as_str()))
        .map(|r| r.uid.as_str())
        .collect();
    let gone: Vec<Row> = all.iter().filter(|r| old.contains(head[&r.uid].as_str())).cloned().collect();
    if yes {
        for r in &gone {
            // The row only: a result file stays where it is.
            tx.execute("DELETE FROM tasks WHERE id = ?1", [r.id])?;
        }
        tx.commit()?;
    }
    Ok(gone)
}

/// Flushes the work folders `dirs` by the Disk use rule (see
/// [`skiff_client::disk`]), before their rows go. Prints what went, and each
/// folder that stays with the reason. Without the session list nothing goes.
fn flush_folders(socket: &Path, sessions: Option<&[SessionInfo]>, dirs: &[PathBuf]) -> Result<()> {
    let file = path()?;
    let folder = file.parent().context("the ledger has no folder")?;
    let (out, stays) = skiff_client::disk::flush_work(folder, &folder.join("cache"), &file, sessions, socket, dirs);
    if out.files > 0 || out.rows > 0 {
        println!("flushed {} {}, {}", out.files, if out.files == 1 { "file" } else { "files" }, skiff_client::agent_folder::size(out.bytes));
    }
    for (d, why) in stays {
        println!("kept {}: {why}", d.display());
    }
    for e in out.errors {
        println!("kept: {e}");
    }
    Ok(())
}

/// The files and bytes under `dir`, never through a link.
fn folder_size(dir: &Path) -> (u64, u64) {
    let mut out = (0, 0);
    let mut todo = vec![dir.to_path_buf()];
    while let Some(d) = todo.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            match std::fs::symlink_metadata(e.path()) {
                Ok(m) if m.is_dir() => todo.push(e.path()),
                Ok(m) => out = (out.0 + 1, out.1 + if m.file_type().is_symlink() { 0 } else { m.len() }),
                Err(_) => {}
            }
        }
    }
    out
}

/// The state `ledger add` stores. `--send` adds the task as pending and
/// sends it. Else a task waits for `ledger send`, and a plan is sent.
fn add_state(st: Option<&str>, plan: bool, send: bool) -> Result<&str> {
    match st {
        _ if send => Ok("pending"),
        Some(s) => state(s),
        None if plan => Ok("sent"),
        None => Ok("pending"),
    }
}

/// The exit code: `send`'s for `send` and `add --send`, else 0.
/// Runs one `skiff ledger` command. After a command that writes, work that
/// is done and reported closes by itself (see [`auto_close`]).
pub async fn run(socket: &Path, cmd: LedgerCmd, person: bool) -> Result<i32> {
    let writes = !matches!(cmd, LedgerCmd::Ls { .. } | LedgerCmd::Show { .. } | LedgerCmd::Dir { .. });
    let code = run_one(socket, cmd, person).await?;
    if writes {
        for r in auto_close(&mut open()?)? {
            eprintln!("skiff: closed {} {}: every task is done and reported", r.kind, r.uid);
        }
    }
    Ok(code)
}

async fn run_one(socket: &Path, cmd: LedgerCmd, person: bool) -> Result<i32> {
    match cmd {
        LedgerCmd::Add { pane: p, task, title, plan: is_plan, parent, project, state: st, result, reference, task_type, send, enter, shell } => {
            let task_type = task_type.as_deref().map(skiff_core::tasks::check_name).transpose()?.map(str::to_string);
            let st = add_state(st.as_deref(), is_plan, send)?.to_string();
            let target = match p {
                Some(p) if !is_plan => Some(pane(socket, &p).await?),
                _ => None,
            };
            let (pane_id, pane_gen, project) = match &target {
                Some(s) => (Some(s.id.clone()), gen(s), project.unwrap_or_else(|| project_of(s))),
                None => (None, None, project.unwrap_or_default()),
            };
            let file = path()?;
            let here = pane_id.as_ref().map(|_| socket.display().to_string());
            let mut db = open_at(&file)?;
            let caller = check_writer(socket, &db, person).await?;
            if let Some(s) = &target {
                check_scope(&caller, s)?;
            }
            let owner = match parent.as_deref() {
                Some(p) => caller.check_parent(&container(&db, p)?)?,
                None => caller.owner().to_string(),
            };
            let new = New { plan: is_plan, parent, pane_id, pane_gen, project, task, title, state: st, result, reference, task_type, socket: here, owner, handoff: false };
            let (id, uid, result, plan) = insert(&mut db, &file, new)?;
            let dir = one(&db, &uid)?.work_dir.unwrap_or_default();
            println!("id {id}\nuid {uid}\nfolder {dir}\nresult {result}");
            if let Some(p) = plan {
                // A task under a hand-off names the hand-off.
                println!("{} {p}", one(&db, &p).map(|r| r.kind).unwrap_or_else(|_| "plan".into()));
            }
            if send {
                let row = one(&db, &uid)?;
                return send_row(socket, &db, &row, SendOpts { text: Vec::new(), enter, shell }).await;
            }
        }
        LedgerCmd::Handoff { project, task, title, parent } => {
            let file = path()?;
            let mut db = open_at(&file)?;
            let caller = check_writer(socket, &db, person).await?;
            if matches!(caller, Caller::Project(_)) {
                bail!("a project or fleet Skipper does not hand off. Add your own tasks with skiff ledger add. Nothing changed");
            }
            let cfg = skiff_core::config::load()?;
            let target = scope(&cfg, &project)?;
            if let Some(p) = parent.as_deref() {
                let c = container(&db, p)?;
                if c.is_handoff() {
                    bail!("{p} is in hand-off {}: a hand-off goes under a plan of Admiral's. Nothing changed", c.uid);
                }
                caller.check_parent(&c)?;
            }
            let c = crate::connect(socket).await?;
            let (ps, key, name) = match target {
                Scope::Project(p) => {
                    // One Skipper per project: its fleet's Skipper takes the work.
                    if let Some(f) = skiff_client::skipper::commanding_fleet(&cfg, &c.list_sessions().await?, &p) {
                        bail!("{p} is in {f}, which has a fleet Skipper. Use: skiff ledger handoff {f}. Nothing changed");
                    }
                    (crate::skipper::start_project(socket, &c, &p).await?, p.clone(), p)
                }
                Scope::Fleet(f) => {
                    let fs = crate::skipper::start_fleet(socket, &c, &f, crate::skipper::Over::Refuse).await?;
                    (fs.skipper, skiff_client::skipper::fleet_scope(&f), f)
                }
            };
            let s = &ps.session;
            let new = New {
                plan: false,
                parent,
                pane_id: Some(s.id.clone()),
                pane_gen: gen(s),
                project: key,
                task,
                title,
                state: "pending".into(),
                result: None,
                reference: None,
                task_type: None,
                socket: Some(socket.display().to_string()),
                owner: GRAND.into(),
                handoff: true,
            };
            let (id, uid, result, plan) = insert(&mut db, &file, new)?;
            let dir = one(&db, &uid)?.work_dir.unwrap_or_default();
            println!("id {id}\nuid {uid}\nfolder {dir}\nresult {result}");
            if let Some(p) = plan {
                // A task under a hand-off names the hand-off.
                println!("{} {p}", one(&db, &p).map(|r| r.kind).unwrap_or_else(|_| "plan".into()));
            }
            let row = one(&db, &uid)?;
            let mut text = message(&row);
            if ps.created {
                // A new agent takes its instructions first, then the task, in one turn.
                text = format!("{} Then take this hand-off. {text}", skiff_client::skipper::first_message(&skiff_core::config::load()?, &ps.dir)?);
                crate::skipper::ready(&c, &s.id).await?;
                println!("started the {name} Skipper");
            }
            return send_row(socket, &db, &row, SendOpts { text: vec![text], enter: true, shell: false }).await;
        }
        LedgerCmd::Send { id, text, enter, shell } => {
            let db = open()?;
            let caller = check_writer(socket, &db, person).await?;
            let row = one(&db, &id)?;
            caller.check(&row)?;
            return send_row(socket, &db, &row, SendOpts { text, enter, shell }).await;
        }
        LedgerCmd::Update { id, state: st, result, task, title, pane: p, project, parent, reference, task_type } => {
            let st = st.as_deref().map(state).transpose()?;
            let task_type = task_type.as_deref().map(skiff_core::tasks::check_name).transpose()?;
            let file = path()?;
            let mut db = open_at(&file)?;
            let caller = check_writer(socket, &db, person).await?;
            let row = one(&db, &id)?;
            // A project Skipper reports the hand-off it was given: its state and result.
            let report_only = task.is_none() && title.is_none() && p.is_none() && project.is_none() && parent.is_none() && reference.is_none() && task_type.is_none();
            if report_only {
                caller.check_report(&row)?;
            } else {
                caller.check(&row)?;
            }
            if let Some(p) = parent.as_deref() {
                if caller.check_parent(&container(&db, p)?)? != row.owner {
                    bail!("{} is {}'s {}, and {p} is not. Move it under a plan of {}", row.uid, who(&row.owner), kind_word(&row.kind), who(&row.owner));
                }
            }
            if row.is_plan() && (p.is_some() || parent.is_some()) {
                bail!("ledger row {id} is a plan: it has no pane and no plan above it");
            }
            // A new pane, or a new send to the same one, starts from the
            // pane's program as it is now.
            let (pane_id, gen) = match (&p, st) {
                (Some(p), _) => {
                    let s = pane(socket, p).await?;
                    check_scope(&caller, &s)?;
                    (Some(s.id.clone()), Some(gen(&s)))
                }
                (None, Some("sent")) if !row.is_plan() && row.socket_other_than(socket).is_none() => {
                    let all = live(socket).await.unwrap_or_default();
                    let s = row.pane_id.as_deref().and_then(|p| resolve::session(&all, p).ok());
                    (None, s.map(gen))
                }
                _ => (None, None),
            };
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            if parent.as_deref().is_some_and(|p| one(&tx, p).is_ok_and(|p| p.id == row.id)) {
                bail!("task {} cannot join its own work. Nothing changed", row.uid);
            }
            let parent = parent.as_deref().map(|p| join(&tx, &file, p)).transpose()?.map(|p| p.uid);
            // A new pane is on this socket.
            let here = pane_id.as_ref().map(|_| socket.display().to_string());
            tx.execute(
                &format!(
                    "UPDATE tasks SET state = coalesce(?2, state), result_path = coalesce(?3, result_path), task = coalesce(?4, task),
                     pane_id = coalesce(?5, pane_id), project = coalesce(?6, project), parent = coalesce(?7, parent), ref = coalesce(?8, ref),
                     pane_gen = CASE WHEN ?9 THEN ?10 ELSE pane_gen END, type = coalesce(?11, type), socket = coalesce(?12, socket), title = coalesce(?13, title),
                     closed_at = CASE WHEN ?7 IS NULL THEN closed_at END, gone_at = CASE WHEN ?2 IS NULL AND ?5 IS NULL THEN gone_at END,
                     updated_at = {NOW} WHERE id = ?1"
                ),
                params![row.id, st, result, task, pane_id, project, parent, reference, gen.is_some(), gen.flatten(), task_type, here, title],
            )?;
            restate(&tx, &one(&tx, &row.uid)?, st)?;
            tx.commit()?;
        }
        LedgerCmd::Ls { state: st, all, closed, unreported, json } => {
            if let Some(s) = st.as_deref().filter(|s| *s != INTERRUPTED && *s != GONE) {
                state(s)?;
            }
            let view = if all {
                View::All
            } else if closed {
                View::Closed
            } else if unreported {
                View::Unreported
            } else {
                View::Open
            };
            let mut db = open()?;
            let all = live(socket).await;
            sweep(&mut db, socket, all.as_deref())?;
            let mut list = rows(&db, "", None)?;
            mark_live(socket, all.as_deref(), &mut list);
            roll_up(&mut list);
            keep(&mut list, view);
            if let Some(s) = &st {
                list.retain(|r| r.shown_state() == s);
            }
            let mut list = tree(list);
            // Stable: each work keeps its tasks under it.
            list.sort_by_key(|(r, _)| rank(r.bucket));
            if json {
                print::json(&list.iter().map(|(r, _)| r).collect::<Vec<_>>())?;
            } else {
                print_rows(&list, matches!(view, View::Closed | View::All));
            }
        }
        LedgerCmd::Report { ids, all } => {
            let mut db = open()?;
            let caller = check_writer(socket, &db, person).await?;
            let n = report(&mut db, &caller, &ids, all)?;
            println!("reported {n} {}", rows_word(n));
        }
        LedgerCmd::Close { ids } => {
            let mut db = open()?;
            let caller = check_writer(socket, &db, person).await?;
            let n = close(&mut db, &caller, &ids)?;
            println!("closed {n} {}", work_word(n));
        }
        LedgerCmd::Reopen { ids } => {
            let mut db = open()?;
            let caller = check_writer(socket, &db, person).await?;
            let n = reopen(&mut db, &caller, &ids)?;
            println!("reopened {n} {}", work_word(n));
        }
        LedgerCmd::Prune { older_than, yes, flush } => {
            let secs = age(&older_than)?;
            let mut db = open()?;
            let caller = check_writer(socket, &db, person).await?;
            let sessions = live(socket).await;
            // The rows first, so the rule still sees them while it judges their folders.
            let mut gone = prune(&mut db, &caller, secs, false)?;
            if gone.is_empty() {
                println!("no work was closed more than {older_than} ago. Nothing changed");
                return Ok(0);
            }
            let dirs: Vec<PathBuf> = gone.iter().filter_map(|r| r.work_dir.as_deref().map(PathBuf::from)).collect();
            if yes && flush {
                flush_folders(socket, sessions.as_deref(), &dirs)?;
            }
            let gone_now = if yes { prune(&mut db, &caller, secs, true)? } else { Vec::new() };
            let n = gone.len();
            mark_live(socket, sessions.as_deref(), &mut gone);
            print_rows(&tree(gone), true);
            if yes {
                let n = gone_now.len();
                println!("removed {n} {}. {}", rows_word(n), if flush { "Files outside the work folders stay" } else { "The work folders and result files stay" });
            } else {
                let (files, bytes) = dirs.iter().map(|d| folder_size(d)).fold((0, 0), |a, b| (a.0 + b.0, a.1 + b.1));
                println!(
                    "would remove {n} {}. Nothing changed. Their work folders hold {files} files ({}). Run again with --yes to remove the rows, and add --flush to flush their folders",
                    rows_word(n),
                    skiff_client::agent_folder::size(bytes)
                );
            }
        }
        LedgerCmd::Show { id, json } => {
            let mut db = open()?;
            let all = live(socket).await;
            sweep(&mut db, socket, all.as_deref())?;
            let r = one(&db, &id)?;
            // The whole work: a plan's state and the bucket come from its tasks.
            let h = top(&db, &r)?;
            let mut work = under(&db, &h)?;
            work.push(h.clone());
            mark_live(socket, all.as_deref(), &mut work);
            roll_up(&mut work);
            let closed = h.closed_at.clone();
            let r = work.into_iter().find(|w| w.uid == r.uid).context("the row left the ledger")?;
            if json {
                print::json(&r)?;
            } else {
                print_one(&r, closed.as_deref());
            }
        }
        LedgerCmd::Dir { id } => {
            let r = one(&open()?, &id)?;
            match (&r.work_dir, &r.result_path) {
                (Some(d), _) => println!("{d}"),
                (None, Some(p)) => {
                    eprintln!("skiff: row {} is from before work folders and has none. Its result: {p}", r.uid);
                    return Ok(1);
                }
                (None, None) => {
                    eprintln!("skiff: row {} is from before work folders and has none", r.uid);
                    return Ok(1);
                }
            }
        }
        LedgerCmd::Rm { id, flush } => {
            let db = open()?;
            let caller = check_writer(socket, &db, person).await?;
            let row = one(&db, &id)?;
            caller.check(&row)?;
            if row.is_plan() || row.is_handoff() {
                let n: i64 = db.query_row("SELECT count(*) FROM tasks WHERE parent = ?1", [&row.uid], |r| r.get(0))?;
                if n > 0 {
                    bail!("{} {id} still has {n} tasks. Remove them first", row.kind);
                }
            }
            if flush {
                if let Some(d) = &row.work_dir {
                    flush_folders(socket, live(socket).await.as_deref(), &[PathBuf::from(d)])?;
                }
            }
            // The row. Without --flush, its folder and result file stay where they are.
            db.execute("DELETE FROM tasks WHERE id = ?1", [row.id])?;
            match (&row.work_dir, flush) {
                (Some(d), false) if Path::new(d).exists() => println!("removed {} {}. Its work folder stays: {d}", kind_word(&row.kind), row.uid),
                _ => println!("removed {} {}", kind_word(&row.kind), row.uid),
            }
        }
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_defaults_a_task_to_pending_and_a_plan_to_sent() {
        assert_eq!(add_state(None, false, false).unwrap(), "pending");
        assert_eq!(add_state(None, true, false).unwrap(), "sent");
        assert_eq!(add_state(Some("done"), false, false).unwrap(), "done");
        assert_eq!(add_state(None, false, true).unwrap(), "pending");
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("skiff-ledger-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn task(t: &str) -> New {
        New {
            plan: false,
            parent: None,
            pane_id: Some("abc123456789".into()),
            pane_gen: Some(1000),
            project: "app".into(),
            task: t.into(),
            title: None,
            state: "sent".into(),
            result: None,
            reference: None,
            task_type: None,
            socket: Some("/tmp/here.sock".into()),
            owner: GRAND.into(),
            handoff: false,
        }
    }

    #[test]
    fn supported_ledger_schemas_keep_their_columns_and_version() {
        let columns = [
            "id", "uid", "kind", "parent", "pane_id", "pane_gen", "project", "task", "sent_at", "state", "result_path", "ref", "updated_at", "type", "socket", "reported_at", "title", "closed_at", "gone_at", "owner", "reopened_at", "work_dir", "flushed_at",
        ];
        let fixtures = [
            (3, TASKS.replace(",\n  socket      TEXT,\n  reported_at TEXT,\n  title       TEXT,\n  closed_at   TEXT,\n  gone_at     TEXT,\n  owner       TEXT NOT NULL DEFAULT 'grand',\n  reopened_at TEXT,\n  work_dir    TEXT,\n  flushed_at  TEXT", "")),
            (4, TASKS.replace(",\n  reported_at TEXT,\n  title       TEXT,\n  closed_at   TEXT,\n  gone_at     TEXT,\n  owner       TEXT NOT NULL DEFAULT 'grand',\n  reopened_at TEXT,\n  work_dir    TEXT,\n  flushed_at  TEXT", "")),
            (6, TASKS.replace(",\n  closed_at   TEXT,\n  gone_at     TEXT,\n  owner       TEXT NOT NULL DEFAULT 'grand',\n  reopened_at TEXT,\n  work_dir    TEXT,\n  flushed_at  TEXT", "")),
            (8, TASKS.replace("('task', 'plan', 'handoff')", "('task', 'plan')").replace(",\n  owner       TEXT NOT NULL DEFAULT 'grand',\n  reopened_at TEXT,\n  work_dir    TEXT,\n  flushed_at  TEXT", "")),
            (9, TASKS.replace(",\n  reopened_at TEXT,\n  work_dir    TEXT,\n  flushed_at  TEXT", "")),
            (10, TASKS.replace(",\n  work_dir    TEXT,\n  flushed_at  TEXT", "")),
        ];
        for (version, schema) in fixtures {
            let dir = scratch(&format!("schema-v{version}"));
            let file = dir.join("ledger.db");
            let db = Connection::open(&file).unwrap();
            db.execute_batch(&format!("{schema}; PRAGMA user_version = {version};")).unwrap();
            drop(db);
            let db = open_at(&file).unwrap();
            let got: Vec<String> = db
                .prepare("SELECT name FROM pragma_table_info('tasks') ORDER BY cid")
                .unwrap()
                .query_map([], |r| r.get(0))
                .unwrap()
                .map(Result::unwrap)
                .collect();
            assert_eq!(got, columns);
            assert_eq!(db.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0)).unwrap(), VERSION);
            std::fs::remove_dir_all(&dir).unwrap();
        }
    }

    // SPLIT-SCAFFOLD
    #[test]
    fn work_folder_paths_cover_each_owner_and_container() {
        let dir = scratch("path-matrix");
        let file = dir.join("ledger.db");
        let mut db = open_at(&file).unwrap();
        let (_, grand, _, _) = insert(&mut db, &file, task("Grand task")).unwrap();
        let (_, project, _, _) = insert(&mut db, &file, New { owner: "web".into(), title: Some("Project title".into()), ..task("project task") }).unwrap();
        let (_, fleet, _, _) = insert(&mut db, &file, New { owner: "fleet:Harbor".into(), ..task("fleet task") }).unwrap();
        let (_, plan, _, _) = insert(&mut db, &file, New { plan: true, pane_id: None, title: Some("Plan title".into()), ..task("plan task") }).unwrap();
        let (_, child, _, _) = insert(&mut db, &file, New { parent: Some(plan.clone()), ..task("plan child") }).unwrap();
        let (_, handoff, _, _) = insert(&mut db, &file, New { handoff: true, project: "web".into(), title: Some("Hand off".into()), ..task("handoff task") }).unwrap();
        let (_, handoff_child, _, _) = insert(&mut db, &file, New { parent: Some(handoff.clone()), owner: "web".into(), ..task("handoff child") }).unwrap();
        let root = dir.join("results");
        assert_eq!(one(&db, &grand).unwrap().work_dir, Some(root.join(format!("{grand}-grand-task")).display().to_string()));
        assert_eq!(one(&db, &project).unwrap().work_dir, Some(dir.join("projects/web/results").join(format!("{project}-project-title")).display().to_string()));
        assert_eq!(one(&db, &fleet).unwrap().work_dir, Some(dir.join("fleets/Harbor/results").join(format!("{fleet}-fleet-task")).display().to_string()));
        let plan_dir = root.join(format!("{plan}-plan-title"));
        assert_eq!(one(&db, &child).unwrap().work_dir, Some(plan_dir.join(format!("{child}-plan-child")).display().to_string()));
        let handoff_dir = dir.join("projects/web/results").join(format!("{handoff}-hand-off"));
        assert_eq!(one(&db, &handoff_child).unwrap().work_dir, Some(handoff_dir.join(format!("{handoff_child}-handoff-child")).display().to_string()));
        let old = Row { work_dir: None, result_path: Some(dir.join("results/old-plan/plan.md").display().to_string()), ..one(&db, &plan).unwrap() };
        assert_eq!(container_folder(&root, &old), dir.join("results/old-plan"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    // SPLIT-SCAFFOLD
    #[test]
    fn ls_json_order_and_table_columns_are_pinned() {
        let dir = scratch("presentation");
        let file = dir.join("ledger.db");
        let mut db = open_at(&file).unwrap();
        let (_, first, _, _) = insert(&mut db, &file, New { reference: Some("T-1".into()), task_type: Some("build".into()), ..task("first") }).unwrap();
        let (_, second, _, _) = insert(&mut db, &file, New { owner: "web".into(), socket: Some("/tmp/other.sock".into()), ..task("second") }).unwrap();
        let mut rows = rows(&db, "", None).unwrap();
        rows.iter_mut().find(|r| r.uid == second).unwrap().elsewhere = true;
        skiff_client::ledger::buckets(&mut rows);
        let rows = tree(rows);
        let json = serde_json::to_string_pretty(&rows.iter().map(|(r, _)| r).collect::<Vec<_>>()).unwrap();
        assert!(json.find(&first).unwrap() < json.find(&second).unwrap());
        assert!(rows.iter().any(|(r, _)| r.reference.is_some()));
        assert!(rows.iter().any(|(r, _)| r.task_type.is_some()));
        assert!(rows.iter().any(|(r, _)| r.elsewhere));
        assert!(rows.iter().any(|(r, _)| r.owner != GRAND));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    // SPLIT-SCAFFOLD
    #[tokio::test]
    async fn only_writes_auto_close_the_ledger() {
        let dir = scratch("command-boundary");
        let file = dir.join("ledger.db");
        let mut db = open_at(&file).unwrap();
        let (_, uid, _, _) = insert(&mut db, &file, New { state: "done".into(), ..task("done") }).unwrap();
        drop(db);
        let prior = std::env::var_os("SKIFF_LEDGER");
        unsafe { std::env::set_var("SKIFF_LEDGER", &file) };
        let socket = Path::new("/tmp/no-ledger-daemon.sock");
        for cmd in [
            LedgerCmd::Ls { state: None, all: false, closed: false, unreported: false, json: true },
            LedgerCmd::Show { id: uid.clone(), json: true },
            LedgerCmd::Dir { id: uid.clone() },
        ] {
            assert_eq!(run(socket, cmd, true).await.unwrap(), 0);
            assert!(one(&open_at(&file).unwrap(), &uid).unwrap().closed_at.is_none());
        }
        assert_eq!(run(socket, LedgerCmd::Report { ids: vec![uid.clone()], all: false }, true).await.unwrap(), 0);
        assert!(one(&open_at(&file).unwrap(), &uid).unwrap().closed_at.is_some());
        match prior {
            Some(value) => unsafe { std::env::set_var("SKIFF_LEDGER", value) },
            None => unsafe { std::env::remove_var("SKIFF_LEDGER") },
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn reads_only_open_rows_and_stores_the_type() {
        let dir = scratch("open");
        let file = dir.join("ledger.db");
        assert!(open_tasks_at(&file).unwrap().is_empty() && !file.exists());
        let mut db = open_at(&file).unwrap();
        insert(&mut db, &file, task("fix bug")).unwrap();
        insert(&mut db, &file, New { state: "done".into(), ..task("seo") }).unwrap();
        let (id, _, _, _) = insert(&mut db, &file, New { state: "pending".into(), task_type: Some("review".into()), ..task("docs") }).unwrap();
        let t = open_tasks_at(&file).unwrap();
        assert_eq!(t.iter().map(|r| (r.task.as_str(), r.state.as_str())).collect::<Vec<_>>(), [("fix bug", "sent"), ("docs", "pending")]);
        let r = one(&db, &id.to_string()).unwrap();
        assert_eq!(serde_json::to_value(&r).unwrap()["type"], "review");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_version_3_ledger_takes_sockets_in_place_and_keeps_its_rows() {
        let dir = scratch("v3");
        let file = dir.join("ledger.db");
        {
            let v3 = TASKS.replace("type        TEXT,\n  socket      TEXT,\n  reported_at TEXT,\n  title       TEXT,\n  closed_at   TEXT,\n  gone_at     TEXT", "type        TEXT");
            assert!(!v3.contains("socket"));
            let db = Connection::open(&file).unwrap();
            db.execute_batch(&v3).unwrap();
            db.execute_batch(UIDS).unwrap();
            db.execute_batch(
                "INSERT INTO tasks (uid, kind, pane_id, task, sent_at, state, updated_at) VALUES ('a1b2c3d4', 'task', 'abc', 'live work', 'x', 'sent', 'x');
                 INSERT INTO tasks (uid, kind, task, sent_at, state, updated_at) VALUES ('e5f6a7b8', 'plan', 'a plan', 'x', 'sent', 'x');
                 PRAGMA user_version = 3;",
            )
            .unwrap();
        }
        let db = open_at(&file).unwrap();
        let list = rows(&db, "", None).unwrap();
        assert_eq!(list.iter().map(|r| r.uid.as_str()).collect::<Vec<_>>(), ["a1b2c3d4", "e5f6a7b8"]);
        // The task went out on the live daemon; a plan has no pane.
        assert_eq!(list[0].socket.as_deref(), Some(state::live_socket().to_str().unwrap()));
        assert_eq!(list[1].socket, None);
        assert_eq!(db.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0)).unwrap(), VERSION);
        assert_eq!(owner(&db).unwrap(), None);
        assert!(!dir.join("ledger.db.old").exists());
        // A newer skiff's stamp stays.
        db.pragma_update(None, "user_version", VERSION + 1).unwrap();
        drop(db);
        let db = open_at(&file).unwrap();
        assert_eq!(db.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0)).unwrap(), VERSION + 1);
        // A second open changes nothing.
        drop(db);
        assert_eq!(rows(&open_at(&file).unwrap(), "", None).unwrap().len(), 2);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_version_4_ledger_takes_reported_at_and_title_in_place_and_keeps_its_rows() {
        let dir = scratch("v4");
        let file = dir.join("ledger.db");
        {
            let v4 = TASKS.replace(",\n  reported_at TEXT,\n  title       TEXT,\n  closed_at   TEXT,\n  gone_at     TEXT", "");
            assert!(!v4.contains("reported_at") && !v4.contains("title"));
            let db = Connection::open(&file).unwrap();
            db.execute_batch(&v4).unwrap();
            db.execute_batch(UIDS).unwrap();
            db.execute_batch(OWNER).unwrap();
            db.execute_batch(
                "INSERT INTO tasks (uid, kind, pane_id, task, sent_at, state, updated_at, socket) VALUES ('a1b2c3d4', 'task', 'abc', 'old work', 'x', 'done', 'x', '/tmp/s.sock');
                 INSERT INTO tasks (uid, kind, pane_id, task, sent_at, state, updated_at, socket) VALUES ('e5f6a7b8', 'task', 'abc', 'live work', 'x', 'sent', 'x', '/tmp/s.sock');
                 PRAGMA user_version = 4;",
            )
            .unwrap();
        }
        let db = open_at(&file).unwrap();
        let list = rows(&db, "", None).unwrap();
        assert_eq!(list.iter().map(|r| (r.uid.as_str(), r.reported_at.as_deref())).collect::<Vec<_>>(), [("a1b2c3d4", None), ("e5f6a7b8", None)]);
        // The v4 socket stays as it was.
        assert_eq!(list[0].socket.as_deref(), Some("/tmp/s.sock"));
        assert_eq!(db.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0)).unwrap(), VERSION);
        assert_eq!(unreported_at(&file, GRAND).unwrap(), 1);
        assert!(list.iter().all(|r| r.title.is_none()));
        db.execute("UPDATE tasks SET title = 'Old work' WHERE uid = 'a1b2c3d4'", []).unwrap();
        assert_eq!(one(&db, "a1b2c3d4").unwrap().title.as_deref(), Some("Old work"));
        // A second open adds nothing.
        drop(db);
        let db = open_at(&file).unwrap();
        let n: i64 = db.query_row("SELECT count(*) FROM pragma_table_info('tasks') WHERE name = 'reported_at'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
        assert_eq!(rows(&db, "", None).unwrap().len(), 2);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The uids `ls` lists in `view`, with `state` if given, as `run` does it.
    fn listed(db: &Connection, view: View, state: Option<&str>) -> Vec<String> {
        let mut list = rows(db, "", None).unwrap();
        roll_up(&mut list);
        keep(&mut list, view);
        if let Some(s) = state {
            list.retain(|r| r.shown_state() == s);
        }
        tree(list).into_iter().map(|(r, _)| r.uid).collect()
    }

    #[test]
    fn ls_lists_open_work_closed_work_and_untold_tasks() {
        let dir = scratch("view");
        let file = dir.join("ledger.db");
        let mut db = open_at(&file).unwrap();
        let done = |t: &str| New { state: "done".into(), ..task(t) };
        let (_, open, _, _) = insert(&mut db, &file, task("open")).unwrap();
        let (_, told, _, _) = insert(&mut db, &file, done("told")).unwrap();
        let (_, untold, _, _) = insert(&mut db, &file, done("untold")).unwrap();
        let (_, p, _, _) = insert(&mut db, &file, New { plan: true, pane_id: None, ..task("half") }).unwrap();
        let (_, p_open, _, _) = insert(&mut db, &file, New { parent: Some(p.clone()), ..task("a") }).unwrap();
        let (_, p_told, _, _) = insert(&mut db, &file, New { parent: Some(p.clone()), ..done("b") }).unwrap();
        let (_, q, _, _) = insert(&mut db, &file, New { plan: true, pane_id: None, ..task("over") }).unwrap();
        let (_, q_done, _, _) = insert(&mut db, &file, New { parent: Some(q.clone()), ..done("c") }).unwrap();
        assert_eq!(report(&mut db, &Caller::Person, &[told.clone(), p_told.clone()], false).unwrap(), 2);
        assert_eq!(close(&mut db, &Caller::Person, &[told.clone(), q.clone()]).unwrap(), 2);
        let s = |v: &[&String]| v.iter().map(|u| u.to_string()).collect::<Vec<_>>();

        // Done work stays listed until it is closed, told or not.
        assert_eq!(listed(&db, View::Open, None), s(&[&open, &untold, &p, &p_open, &p_told]));
        assert_eq!(listed(&db, View::Closed, None), s(&[&told, &q, &q_done]));
        assert_eq!(listed(&db, View::All, None), s(&[&open, &told, &untold, &p, &p_open, &p_told, &q, &q_done]));
        // Close told the person of c.
        assert_eq!(listed(&db, View::Unreported, None), s(&[&untold]));
        assert_eq!(listed(&db, View::Open, Some("done")), s(&[&untold, &p_told]));

        // Interrupted and gone work is open.
        let mut list = rows(&db, "", None).unwrap();
        list.iter_mut().find(|r| r.uid == open).unwrap().live = Some(false);
        keep(&mut list, View::Open);
        assert!(list.iter().any(|r| r.uid == open && r.shown_state() == GONE));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn done_and_reported_work_closes_by_itself_until_reopened() {
        let dir = scratch("autoclose");
        let file = dir.join("ledger.db");
        let mut db = open_at(&file).unwrap();
        let closed = |db: &Connection, uid: &str| one(db, uid).unwrap().closed_at.is_some();
        let done = |t: &str| New { state: "done".into(), ..task(t) };
        let (_, lone, _, _) = insert(&mut db, &file, done("lone")).unwrap();
        let (_, p, _, _) = insert(&mut db, &file, New { plan: true, pane_id: None, ..task("p") }).unwrap();
        let (_, a, _, _) = insert(&mut db, &file, New { parent: Some(p.clone()), ..done("a") }).unwrap();
        let (_, h, _, _) = insert(&mut db, &file, New { parent: Some(p.clone()), handoff: true, ..done("h") }).unwrap();
        let (_, k, _, _) = insert(&mut db, &file, New { parent: Some(h.clone()), owner: "web".into(), ..task("k") }).unwrap();
        let (_, empty, _, _) = insert(&mut db, &file, New { plan: true, pane_id: None, ..task("empty") }).unwrap();
        // Done is not enough: the person was not told yet.
        assert!(auto_close(&mut db).unwrap().is_empty());
        report(&mut db, &Caller::Person, &[], true).unwrap();
        // The hand-off's task is still sent: the plan stays open. A plan with no task stays open.
        assert_eq!(auto_close(&mut db).unwrap().iter().map(|r| r.uid.clone()).collect::<Vec<_>>(), [lone.clone()]);
        assert!(!closed(&db, &p) && !closed(&db, &empty));
        db.execute("UPDATE tasks SET state = 'done' WHERE uid = ?1", [&k]).unwrap();
        report(&mut db, &Caller::Person, &[], true).unwrap();
        auto_close(&mut db).unwrap();
        assert!(closed(&db, &p));
        // Reopened work stays open, until a task in it changes state.
        reopen(&mut db, &Caller::Person, &[p.clone(), lone.clone()]).unwrap();
        assert!(auto_close(&mut db).unwrap().is_empty());
        assert!(!closed(&db, &p) && !closed(&db, &lone));
        db.execute("UPDATE tasks SET state = 'sent' WHERE uid = ?1", [&a]).unwrap();
        db.execute("UPDATE tasks SET state = 'done' WHERE uid = ?1", [&a]).unwrap();
        auto_close(&mut db).unwrap();
        assert!(closed(&db, &p) && !closed(&db, &lone));
        // A hand-off's task clears the mark two steps up.
        reopen(&mut db, &Caller::Person, std::slice::from_ref(&p)).unwrap();
        db.execute("UPDATE tasks SET state = 'pending' WHERE uid = ?1", [&k]).unwrap();
        assert!(one(&db, &p).unwrap().closed_at.is_none());
        let held: Option<String> = db.query_row("SELECT reopened_at FROM tasks WHERE uid = ?1", [&p], |r| r.get(0)).unwrap();
        assert!(held.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_version_9_ledger_takes_reopened_at_and_keeps_its_rows() {
        let dir = scratch("v9");
        let file = dir.join("ledger.db");
        let db = Connection::open(&file).unwrap();
        db.execute_batch(&TASKS.replace(",\n  reopened_at TEXT", "")).unwrap();
        db.execute_batch(&format!("INSERT INTO tasks (uid, task, sent_at, state, updated_at) VALUES ('a1b2c3d4', 't', 'x', 'done', 'x'); PRAGMA user_version = 9;")).unwrap();
        drop(db);
        let db = open_at(&file).unwrap();
        assert_eq!(rows(&db, "", None).unwrap().len(), 1);
        assert!(skiff_client::ledger::open_read(&file).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn work_is_working_while_an_agent_is_on_a_task_else_open_until_closed() {
        let dir = scratch("buckets");
        let file = dir.join("ledger.db");
        let mut db = open_at(&file).unwrap();
        let (_, lone, _, _) = insert(&mut db, &file, task("lone")).unwrap();
        let (_, p, _, _) = insert(&mut db, &file, New { plan: true, pane_id: None, ..task("p") }).unwrap();
        let (_, p_on, _, _) = insert(&mut db, &file, New { parent: Some(p.clone()), ..task("a") }).unwrap();
        insert(&mut db, &file, New { parent: Some(p.clone()), state: "pending".into(), ..task("b") }).unwrap();
        let (_, q, _, _) = insert(&mut db, &file, New { state: "done".into(), ..task("q") }).unwrap();
        close(&mut db, &Caller::Person, std::slice::from_ref(&q)).unwrap();
        let mut list = rows(&db, "", None).unwrap();
        list.iter_mut().find(|r| r.uid == p_on).unwrap().agent_on = true;
        skiff_client::ledger::buckets(&mut list);
        let b = |uid: &str| list.iter().find(|r| r.uid == uid).unwrap().bucket.unwrap();
        // A plan and every task in it share one bucket.
        assert!(list.iter().filter(|r| r.work() == p).all(|r| r.bucket == Some(WORKING)));
        assert_eq!((b(&lone), b(&q)), (OPEN, skiff_client::ledger::FINISHED));
        assert!(rank(Some(WORKING)) < rank(Some(OPEN)));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn close_takes_done_work_on_its_head_and_reopen_clears_it() {
        let dir = scratch("close");
        let file = dir.join("ledger.db");
        let mut db = open_at(&file).unwrap();
        let done = |t: &str| New { state: "done".into(), ..task(t) };
        let (_, a, _, _) = insert(&mut db, &file, done("a")).unwrap();
        let (_, open, _, _) = insert(&mut db, &file, New { state: "pending".into(), ..task("open") }).unwrap();
        let (_, p, _, _) = insert(&mut db, &file, New { plan: true, pane_id: None, ..task("p") }).unwrap();
        let (_, p_done, _, _) = insert(&mut db, &file, New { parent: Some(p.clone()), ..done("c") }).unwrap();
        let closed = |db: &Connection, uid: &str| one(db, uid).unwrap().closed_at;

        // Work with a task that is not done refuses the lot.
        let err = close(&mut db, &Caller::Person, &[a.clone(), open.clone()]).unwrap_err().to_string();
        assert_eq!(err, format!("task {open} is pending, not done. Set it done or remove it first. Nothing changed"));
        assert!(closed(&db, &a).is_none());
        // A task in a plan is not a work head.
        let err = close(&mut db, &Caller::Person, std::slice::from_ref(&p_done)).unwrap_err().to_string();
        assert_eq!(err, format!("task {p_done} is in plan {p}. Name the plan. Nothing changed"));
        // The stamp goes on the head, and the person was told of its tasks.
        assert_eq!(close(&mut db, &Caller::Person, &[a.clone(), p.clone()]).unwrap(), 2);
        assert!(closed(&db, &p).is_some() && closed(&db, &p_done).is_none());
        assert!(one(&db, &p_done).unwrap().reported_at.is_some() && one(&db, &a).unwrap().reported_at.is_some());
        assert!(one(&db, &p).unwrap().reported_at.is_none());
        // A first stamp stays.
        db.execute("UPDATE tasks SET closed_at = '2020-01-01T00:00:00Z' WHERE uid = ?1", [&a]).unwrap();
        assert_eq!(close(&mut db, &Caller::Person, std::slice::from_ref(&a)).unwrap(), 0);
        assert_eq!(closed(&db, &a).as_deref(), Some("2020-01-01T00:00:00Z"));

        // Open work takes the stamp too, so it does not close by itself.
        assert_eq!(reopen(&mut db, &Caller::Person, &[a.clone(), open.clone()]).unwrap(), 2);
        assert!(closed(&db, &a).is_none());
        // A task set back from done opens its work again.
        close(&mut db, &Caller::Person, std::slice::from_ref(&a)).unwrap();
        restate(&db, &one(&db, &p_done).unwrap(), Some("sent")).unwrap();
        assert!(closed(&db, &p).is_none() && one(&db, &p_done).unwrap().reported_at.is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_follow_up_joins_its_work_and_a_lone_task_gets_a_plan() {
        let dir = scratch("join");
        let file = dir.join("ledger.db");
        let mut db = open_at(&file).unwrap();
        let first = New { state: "done".into(), title: Some("Build 177".into()), reference: Some("#177".into()), task_type: Some("build".into()), ..task("Build issue 177") };
        let (_, lone, lone_path, none) = insert(&mut db, &file, first).unwrap();
        assert_eq!(none, None);
        close(&mut db, &Caller::Person, std::slice::from_ref(&lone)).unwrap();

        // The follow-up wraps the lone task in a plan, and the closed work opens.
        let (_, check, check_path, plan) = insert(&mut db, &file, New { parent: Some(lone.clone()), ..task("Check the app") }).unwrap();
        let p = one(&db, plan.as_deref().unwrap()).unwrap();
        let t = one(&db, &lone).unwrap();
        assert!(p.is_plan() && p.closed_at.is_none() && t.closed_at.is_none());
        assert_eq!((p.task.as_str(), p.title.as_deref(), p.reference.as_deref(), p.task_type.as_deref()), ("Build issue 177", Some("Build 177"), Some("#177"), Some("build")));
        assert_eq!((t.parent.as_deref(), t.result_path.as_deref()), (Some(p.uid.as_str()), Some(lone_path.as_str())));
        // The new plan has its own folder, named from the title. The lone task's folder stays.
        let folder = dir.join("results").join(format!("{}-build-177", p.uid));
        assert_eq!((p.work_dir.as_deref(), p.result_path.as_deref()), (folder.to_str(), Some(folder.join("plan.md").to_str().unwrap())));
        assert_eq!(check_path, folder.join(format!("{check}-check-the-app")).join("result.md").display().to_string());
        assert_eq!(t.work_dir, Some(dir.join("results").join(format!("{lone}-build-177")).display().to_string()));
        assert!(folder.is_dir() && dir.join("results").join(format!("{lone}-build-177")).is_dir());

        // A second follow-up names the plan, and a closed plan opens.
        db.execute("UPDATE tasks SET state = 'done' WHERE uid = ?1", [&check]).unwrap();
        close(&mut db, &Caller::Person, std::slice::from_ref(&p.uid)).unwrap();
        let (_, _, _, again) = insert(&mut db, &file, New { parent: Some(p.uid.clone()), ..task("Commit it") }).unwrap();
        assert_eq!(again.as_deref(), Some(p.uid.as_str()));
        assert!(one(&db, &p.uid).unwrap().closed_at.is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_version_6_ledger_takes_closed_at_in_place_and_all_work_stays_open() {
        let dir = scratch("v6");
        let file = dir.join("ledger.db");
        {
            let v6 = TASKS.replace(",\n  closed_at   TEXT,\n  gone_at     TEXT", "");
            assert!(!v6.contains("closed_at"));
            let db = Connection::open(&file).unwrap();
            db.execute_batch(&v6).unwrap();
            db.execute_batch(UIDS).unwrap();
            db.execute_batch(OWNER).unwrap();
            db.execute_batch(
                "INSERT INTO tasks (uid, kind, pane_id, task, sent_at, state, updated_at, socket, reported_at, title) VALUES ('a1b2c3d4', 'task', 'abc', 'told', 'x', 'done', 'x', '/tmp/s.sock', 'y', 'Told');
                 INSERT INTO tasks (uid, kind, pane_id, task, sent_at, state, updated_at, socket) VALUES ('e5f6a7b8', 'task', 'abc', 'live', 'x', 'sent', 'x', '/tmp/s.sock');
                 PRAGMA user_version = 6;",
            )
            .unwrap();
        }
        let db = open_at(&file).unwrap();
        let list = rows(&db, "", None).unwrap();
        assert_eq!(list.iter().map(|r| (r.uid.as_str(), r.closed_at.as_deref())).collect::<Vec<_>>(), [("a1b2c3d4", None), ("e5f6a7b8", None)]);
        assert_eq!((list[0].reported_at.as_deref(), list[0].title.as_deref()), (Some("y"), Some("Told")));
        assert_eq!(db.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0)).unwrap(), VERSION);
        assert_eq!(listed(&db, View::Open, None).len(), 2);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn report_stamps_done_rows_once_and_refuses_open_ones() {
        let dir = scratch("report");
        let file = dir.join("ledger.db");
        let mut db = open_at(&file).unwrap();
        let done = |t: &str| New { state: "done".into(), ..task(t) };
        let (_, a, _, _) = insert(&mut db, &file, done("a")).unwrap();
        let (_, b, _, _) = insert(&mut db, &file, done("b")).unwrap();
        let (_, open, _, _) = insert(&mut db, &file, task("open")).unwrap();
        let (_, p, _, _) = insert(&mut db, &file, New { plan: true, pane_id: None, ..task("p") }).unwrap();
        let (_, p_done, _, _) = insert(&mut db, &file, New { parent: Some(p.clone()), ..done("c") }).unwrap();
        let (_, p_open, _, _) = insert(&mut db, &file, New { parent: Some(p.clone()), ..task("d") }).unwrap();
        let stamp = |db: &Connection, uid: &str| one(db, uid).unwrap().reported_at;
        assert_eq!(unreported_at(&file, GRAND).unwrap(), 3);

        assert_eq!(report(&mut db, &Caller::Person, std::slice::from_ref(&a), false).unwrap(), 1);
        assert!(stamp(&db, &a).is_some());
        // A second run keeps the first stamp.
        db.execute("UPDATE tasks SET reported_at = '2020-01-01T00:00:00Z' WHERE uid = ?1", [&a]).unwrap();
        assert_eq!(report(&mut db, &Caller::Person, std::slice::from_ref(&a), false).unwrap(), 0);
        assert_eq!(stamp(&db, &a).as_deref(), Some("2020-01-01T00:00:00Z"));
        // An open task refuses the lot.
        let err = report(&mut db, &Caller::Person, &[b.clone(), open.clone()], false).unwrap_err().to_string();
        assert_eq!(err, format!("task {open} is sent, not done. Nothing changed"));
        assert!(stamp(&db, &b).is_none());
        // A plan stamps its done tasks only.
        assert_eq!(report(&mut db, &Caller::Person, std::slice::from_ref(&p), false).unwrap(), 1);
        assert!(stamp(&db, &p_done).is_some() && stamp(&db, &p_open).is_none() && stamp(&db, &p).is_none());
        // --all stamps every done task not reported, and only those.
        assert_eq!(report(&mut db, &Caller::Person, &[], true).unwrap(), 1);
        assert!(stamp(&db, &b).is_some() && stamp(&db, &open).is_none());
        assert_eq!(stamp(&db, &a).as_deref(), Some("2020-01-01T00:00:00Z"));
        assert_eq!(unreported_at(&file, GRAND).unwrap(), 0);

        // A task that opens again loses its stamp.
        restate(&db, &one(&db, &b).unwrap(), Some("done")).unwrap();
        assert!(stamp(&db, &b).is_some());
        restate(&db, &one(&db, &b).unwrap(), None).unwrap();
        assert!(stamp(&db, &b).is_some());
        restate(&db, &one(&db, &b).unwrap(), Some("pending")).unwrap();
        assert!(stamp(&db, &b).is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn prune_takes_only_work_closed_long_ago_with_its_tasks() {
        let dir = scratch("prune");
        let file = dir.join("ledger.db");
        let mut db = open_at(&file).unwrap();
        let done = |t: &str| New { state: "done".into(), ..task(t) };
        let (_, old, _, _) = insert(&mut db, &file, done("old")).unwrap();
        let (_, new, _, _) = insert(&mut db, &file, done("new")).unwrap();
        let (_, told, _, _) = insert(&mut db, &file, done("told, still open")).unwrap();
        let (_, open, _, _) = insert(&mut db, &file, task("open")).unwrap();
        let (_, p, _, _) = insert(&mut db, &file, New { plan: true, pane_id: None, ..task("p") }).unwrap();
        let (_, p_a, _, _) = insert(&mut db, &file, New { parent: Some(p.clone()), ..done("a") }).unwrap();
        let (_, p_b, _, _) = insert(&mut db, &file, New { parent: Some(p.clone()), ..done("b") }).unwrap();
        close(&mut db, &Caller::Person, &[old.clone(), new.clone(), p.clone()]).unwrap();
        report(&mut db, &Caller::Person, std::slice::from_ref(&told), false).unwrap();
        for uid in [&old, &p] {
            db.execute("UPDATE tasks SET closed_at = '2020-01-01T00:00:00Z' WHERE uid = ?1", [uid]).unwrap();
        }
        db.execute("UPDATE tasks SET reported_at = '2020-01-01T00:00:00Z' WHERE uid = ?1", [&told]).unwrap();
        let uids = |rows: Vec<Row>| rows.into_iter().map(|r| r.uid).collect::<Vec<_>>();
        let all = || uids(rows(&open_at(&file).unwrap(), "", None).unwrap());
        let before = all();

        // A dry run lists and changes nothing.
        let want = vec![old.clone(), p.clone(), p_a.clone(), p_b.clone()];
        assert_eq!(uids(prune(&mut db, &Caller::Person, 7 * 86_400, false).unwrap()), want);
        assert_eq!(all(), before);
        assert_eq!(uids(prune(&mut db, &Caller::Person, 7 * 86_400, true).unwrap()), want);
        // Work told long ago but not closed stays.
        assert_eq!(all(), [new.clone(), told.clone(), open.clone()]);
        // A short window takes the new one too.
        db.execute("UPDATE tasks SET closed_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now', '-2 hours') WHERE uid = ?1", [&new]).unwrap();
        assert!(prune(&mut db, &Caller::Person, age("7d").unwrap(), true).unwrap().is_empty());
        assert_eq!(uids(prune(&mut db, &Caller::Person, age("1h").unwrap(), true).unwrap()), [new]);
        assert_eq!(age("30m").unwrap(), 1800);
        assert!(age("7").is_err() && age("xd").is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn a_task_on_another_socket_is_never_gone_and_is_not_sent_from_here() {
        let dir = scratch("elsewhere");
        let file = dir.join("ledger.db");
        let mut db = open_at(&file).unwrap();
        let (_, mine, _, _) = insert(&mut db, &file, task("mine")).unwrap();
        let (_, theirs, _, _) = insert(&mut db, &file, New { socket: Some("/tmp/other.sock".into()), state: "pending".into(), ..task("theirs") }).unwrap();
        let here = Path::new("/tmp/here.sock");
        let mut list = rows(&db, "", None).unwrap();
        // No daemon answers here, as for a pane on another socket.
        mark_live(here, None, &mut list);
        let r = |uid: &str| list.iter().find(|r| r.uid == uid).unwrap().clone();
        assert!(!r(&mine).elsewhere);
        assert!(r(&theirs).elsewhere);
        assert_eq!(r(&theirs).socket_other_than(here), Some("/tmp/other.sock"));
        // Even a daemon that does not know the pane does not make it gone.
        let mut t = r(&theirs);
        t.live = None;
        assert_eq!(t.shown_state(), "pending");
        let err = send_row(here, &db, &r(&theirs), SendOpts { text: vec![], enter: true, shell: false }).await.unwrap_err().to_string();
        assert!(err.contains("/tmp/other.sock") && !err.contains("gone"), "{err}");
        assert_eq!(one(&db, &theirs).unwrap().state, "pending");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_task_whose_pane_is_gone_closes_done_and_stays_unreported() {
        let dir = scratch("sweep");
        let file = dir.join("ledger.db");
        let mut db = open_at(&file).unwrap();
        let here = Path::new("/tmp/here.sock");
        let (_, gone, result, _) = insert(&mut db, &file, task("lost")).unwrap();
        let (_, waits, _, _) = insert(&mut db, &file, New { state: "pending".into(), pane_id: Some("dead00000000".into()), ..task("queued") }).unwrap();
        let (_, kept, _, _) = insert(&mut db, &file, New { pane_id: Some("feed00000000".into()), ..task("live pane") }).unwrap();
        let (_, prefix, _, _) = insert(&mut db, &file, New { pane_id: Some("feed".into()), ..task("imported prefix") }).unwrap();
        let (_, theirs, _, _) = insert(&mut db, &file, New { socket: Some("/tmp/other.sock".into()), ..task("elsewhere") }).unwrap();
        let (_, done, _, _) = insert(&mut db, &file, New { state: "done".into(), ..task("finished") }).unwrap();
        let (_, plan, _, _) = insert(&mut db, &file, New { plan: true, pane_id: None, ..task("p") }).unwrap();
        let (_, child, _, _) = insert(&mut db, &file, New { parent: Some(plan.clone()), ..task("in plan") }).unwrap();
        // A restarted pane still exists: its task is interrupted, not gone.
        let all = [pane("feed00000000", "agent", false)];
        let closed = close_gone_in(&mut db, here, &all).unwrap();
        let mut uids: Vec<&str> = closed.iter().map(|r| r.uid.as_str()).collect();
        uids.sort_unstable();
        let mut want = [gone.as_str(), waits.as_str(), child.as_str()];
        want.sort_unstable();
        assert_eq!(uids, want);
        let r = one(&db, &gone).unwrap();
        assert_eq!((r.state.as_str(), r.shown_state(), r.reported_at.as_deref()), ("done", "gone", None));
        assert!(r.gone_at.is_some());
        assert_eq!(r.result_path.as_deref(), Some(result.as_str()));
        for uid in [&kept, &prefix, &theirs] {
            assert_ne!(one(&db, uid).unwrap().state, "done", "{uid}");
        }
        assert!(one(&db, &done).unwrap().gone_at.is_none());
        // The plan rolls up to done; only the person closes it.
        let mut list = rows(&db, "", None).unwrap();
        roll_up(&mut list);
        let p = list.iter().find(|r| r.uid == plan).unwrap();
        assert_eq!((p.state.as_str(), p.closed_at.as_deref()), ("done", None));
        assert_eq!(unreported_at(&file, GRAND).unwrap(), 4);
        // A second sweep finds nothing.
        assert!(close_gone_in(&mut db, here, &all).unwrap().is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn pane(id: &str, role: &str, coordinator: bool) -> SessionInfo {
        let mut s: SessionInfo = serde_json::from_value(serde_json::json!({
            "id": id, "label": "x", "role": role, "cwd": "/", "command": "claude",
            "args": [], "state": "idle", "cols": 80, "rows": 24, "pid": null, "exit_code": null,
        }))
        .unwrap();
        s.coordinator = coordinator;
        s
    }

    #[test]
    fn only_the_owner_a_person_or_the_live_agent_changes_the_ledger() {
        let live = state::live_socket();
        let test = Path::new("/tmp/test.sock");
        let on = |session: &str, socket: &Path| Owner { session: session.into(), socket: socket.display().to_string() };
        let owner = on("96cb7de8aaaa", &live);
        let skipper = pane("96cb7de8aaaa", "agent", true);
        let worker = pane("02900b71aaaa", "agent", false);
        let shell = pane("70808970aaaa", "shell", false);
        let w = |me: &SessionInfo, cur: Option<&Owner>, socket: &Path| writer(Some(&me.id), Some(me), cur, socket, false);
        // The owner, a person outside Skiff and a shell pane may.
        assert_eq!(w(&skipper, Some(&owner), &live), Writer::Yes(Caller::Grand));
        assert_eq!(writer(None, None, Some(&owner), &live, true), Writer::Yes(Caller::Person));
        assert_eq!(w(&shell, Some(&owner), &live), Writer::Yes(Caller::Person));
        // Another agent pane may not, on any socket.
        assert_eq!(w(&worker, Some(&owner), &live), Writer::No);
        assert_eq!(w(&worker, None, &live), Writer::No);
        // A second Skiff agent on a test daemon may not take the live ledger.
        let dev = pane("4d43fa26aaaa", "agent", true);
        assert_eq!(w(&dev, Some(&owner), test), Writer::No);
        // A session the daemon does not list is not a person.
        assert_eq!(writer(Some("deadbeef"), None, Some(&owner), &live, false), Writer::No);
        // A shell outside Skiff without --person may be an agent: refused.
        assert_eq!(writer(None, None, Some(&owner), &live, false), Writer::No);
        assert_eq!(writer(None, None, None, &live, false), Writer::No);
        // --person does not let an agent pane through.
        assert_eq!(writer(Some(&worker.id), Some(&worker), Some(&owner), &live, true), Writer::No);
        // Ownership moves: a new Skiff agent on the owner's daemon takes it,
        // the live agent takes it from any socket, and a first one takes it.
        let next = pane("f0afaa59aaaa", "agent", true);
        assert_eq!(w(&next, Some(&owner), &live), Writer::Claim);
        assert_eq!(w(&skipper, Some(&on("4d43fa26aaaa", test)), &live), Writer::Claim);
        assert_eq!(w(&dev, None, test), Writer::Claim);
        assert_eq!(refusal(Some(&owner), true), format!("this ledger is Admiral's on {}. Ask Admiral there to make this change", live.display()));
        assert_eq!(refusal(Some(&owner), false), "only Admiral and the project and fleet Skippers change the ledger. Ask Admiral to make this change");
        assert!(refusal(None, false).starts_with("this ledger has no Admiral yet."));
    }

    #[test]
    fn a_project_skipper_changes_only_its_rows_and_reports_its_hand_off() {
        let live = state::live_socket();
        let owner = Owner { session: "96cb7de8aaaa".into(), socket: live.display().to_string() };
        let mut gesher = pane("5e5e5e5eaaaa", "agent", false);
        gesher.skipper = Some("gesher".into());
        // The CLI knows the caller from its session.
        let me = writer(Some(&gesher.id), Some(&gesher), Some(&owner), &live, false);
        assert_eq!(me, Writer::Yes(Caller::Project("gesher".into())));
        let Writer::Yes(me) = me else { unreachable!() };

        let dir = scratch("owners");
        let file = dir.join("ledger.db");
        let mut db = open_at(&file).unwrap();
        // The grand Skipper's plan, its own task, and a hand-off to gesher.
        let (_, plan, _, _) = insert(&mut db, &file, New { plan: true, pane_id: None, ..task("Ship it") }).unwrap();
        let (_, mine_g, _, _) = insert(&mut db, &file, New { parent: Some(plan.clone()), ..task("grand work") }).unwrap();
        let ho = New { parent: Some(plan.clone()), project: "gesher".into(), handoff: true, ..task("Chapter polish") };
        let (_, hand, hand_path, _) = insert(&mut db, &file, ho).unwrap();
        // The hand-off's result is in the project Skipper's folder.
        assert!(hand_path.starts_with(dir.join("projects/gesher/results").to_str().unwrap()), "{hand_path}");
        // gesher adds its task under the hand-off: the row is gesher's.
        let parent = container(&db, &hand).unwrap();
        let sub_owner = me.check_parent(&parent).unwrap();
        assert_eq!(sub_owner, "gesher");
        let (_, sub, sub_path, _) = insert(&mut db, &file, New { parent: Some(hand.clone()), owner: sub_owner, ..task("chapter nav") }).unwrap();
        assert!(sub_path.contains(&format!("projects/gesher/results/")) && sub_path.contains(&hand), "{sub_path}");
        let sub = one(&db, &sub).unwrap();
        assert_eq!(sub.parent.as_deref(), Some(hand.as_str()));

        // gesher changes its row, reports the hand-off, and nothing of the grand Skipper's.
        assert!(me.check(&sub).is_ok());
        let hand_row = one(&db, &hand).unwrap();
        assert!(me.check_report(&hand_row).is_ok());
        assert!(me.check(&hand_row).is_err());
        let err = me.check(&one(&db, &mine_g).unwrap()).unwrap_err().to_string();
        assert_eq!(err, format!("{mine_g} is Admiral's task. Ask Admiral to change it"));
        assert!(me.check_parent(&one(&db, &plan).unwrap()).is_err());
        // Another project Skipper reports no hand-off of gesher's.
        let other = Caller::Project("skiff".into());
        assert!(other.check_report(&hand_row).is_err() && other.check(&sub).is_err());
        assert_eq!(other.check(&sub).unwrap_err().to_string(), format!("{} is the gesher project Skipper's task. Ask that Skipper to change it", sub.uid));
        let err = Caller::Grand.check_parent(&hand_row).unwrap_err().to_string();
        assert_eq!(err, format!("{hand} is a hand-off to the gesher project Skipper. Ask that Skipper to add tasks under it"));
        // The grand Skipper cannot change gesher's task or add under the hand-off; the person can.
        assert!(Caller::Grand.check(&sub).is_err());
        assert!(Caller::Grand.check_parent(&hand_row).is_err());
        assert!(Caller::Grand.check(&hand_row).is_ok());
        assert!(Caller::Person.check(&sub).is_ok());
        assert_eq!(Caller::Person.check_parent(&hand_row).unwrap(), "gesher");
        // report --all from gesher stamps only its rows.
        db.execute("UPDATE tasks SET state = 'done'", []).unwrap();
        assert_eq!(report(&mut db, &me, &[], true).unwrap(), 1);
        assert!(one(&db, &mine_g).unwrap().reported_at.is_none());
        assert!(report(&mut db, &me, std::slice::from_ref(&mine_g), false).is_err());
        // The tree nests three deep, and the whole work is the plan's.
        let list = rows(&db, "", None).unwrap();
        let depth: Vec<(String, usize)> = tree(list.clone()).into_iter().map(|(r, d)| (r.uid, d)).collect();
        assert_eq!(depth, [(plan.clone(), 0), (mine_g.clone(), 1), (hand.clone(), 1), (sub.uid.clone(), 2)]);
        assert!(heads(&list).values().all(|h| *h == plan));
        // Closing the plan takes the hand-off's task too; gesher cannot close it.
        assert!(close(&mut db, &me, std::slice::from_ref(&plan)).is_err());
        assert_eq!(close(&mut db, &Caller::Grand, std::slice::from_ref(&plan)).unwrap(), 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_version_8_ledger_takes_owners_in_one_step_and_keeps_its_rows() {
        let dir = scratch("v8");
        let file = dir.join("ledger.db");
        {
            let v8 = TASKS.replace(",\n  owner       TEXT NOT NULL DEFAULT 'grand'", "").replace(", 'handoff'", "");
            assert!(!v8.contains("owner") && !v8.contains("handoff"));
            let db = Connection::open(&file).unwrap();
            db.execute_batch(&v8).unwrap();
            db.execute_batch(UIDS).unwrap();
            db.execute_batch(OWNER).unwrap();
            db.execute_batch(
                "INSERT INTO tasks (id, uid, kind, task, sent_at, state, updated_at) VALUES (7, 'e5f6a7b8', 'plan', 'a plan', 'x', 'sent', 'x');
                 INSERT INTO tasks (id, uid, kind, parent, pane_id, task, sent_at, state, updated_at) VALUES (9, 'a1b2c3d4', 'task', 'e5f6a7b8', 'abc', 'work', 'x', 'sent', 'x');
                 PRAGMA user_version = 8;",
            )
            .unwrap();
        }
        let mut db = open_at(&file).unwrap();
        let list = rows(&db, "", None).unwrap();
        assert_eq!(list.iter().map(|r| (r.id, r.uid.as_str(), r.owner.as_str())).collect::<Vec<_>>(), [(7, "e5f6a7b8", GRAND), (9, "a1b2c3d4", GRAND)]);
        assert_eq!(db.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0)).unwrap(), VERSION);
        // The new shape takes a hand-off, and ids go on from the old ones.
        let (id, _, _, _) = insert(&mut db, &file, New { handoff: true, project: "gesher".into(), ..task("polish") }).unwrap();
        assert_eq!(id, 10);
        assert!(skiff_client::ledger::open_read(&file).is_ok());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn claim_records_the_owner_and_keeps_the_live_one() {
        let dir = scratch("claim");
        let file = dir.join("ledger.db");
        let live = state::live_socket();
        let test = Path::new("/tmp/test.sock");
        claim_at(&file, &live, "96cb7de8aaaa").unwrap();
        let get = || owner(&open_at(&file).unwrap()).unwrap().unwrap();
        assert_eq!(get(), Owner { session: "96cb7de8aaaa".into(), socket: live.display().to_string() });
        // A Skiff agent on another socket does not take it.
        let err = claim_at(&file, test, "4d43fa26aaaa").unwrap_err().to_string();
        assert_eq!(err, format!("this ledger is Admiral's on {}. Do ledger work from Admiral there", live.display()));
        assert_eq!(get().session, "96cb7de8aaaa");
        // A restart on the live daemon moves it.
        claim_at(&file, &live, "f0afaa59aaaa").unwrap();
        assert_eq!(get().session, "f0afaa59aaaa");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn an_old_ledger_moves_aside_and_a_new_one_starts_empty() {
        let dir = scratch("old");
        let file = dir.join("ledger.db");
        Connection::open(&file).unwrap().execute_batch("CREATE TABLE tasks (id INTEGER PRIMARY KEY, pane_id TEXT, task TEXT);").unwrap();
        let db = open_at(&file).unwrap();
        assert!(rows(&db, "", None).unwrap().is_empty());
        assert!(dir.join("ledger.db.old").is_file());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn delete_and_add_reuse_no_id_and_no_uid() {
        let dir = scratch("reuse");
        let file = dir.join("ledger.db");
        let mut db = open_at(&file).unwrap();
        let mut seen = std::collections::HashSet::new();
        let mut last = 0;
        for i in 0..20 {
            let (id, uid, _, _) = insert(&mut db, &file, task(&format!("t{i}"))).unwrap();
            assert!(id > last, "id {id} after {last}");
            assert!(seen.insert(uid.clone()), "uid {uid} again");
            assert!(uid.len() == 8 && uid.bytes().all(|b| b.is_ascii_hexdigit()) && !uid.bytes().all(|b| b.is_ascii_digit()));
            assert_eq!(one(&db, &uid).unwrap().id, id);
            assert_eq!(one(&db, &id.to_string()).unwrap().uid, uid);
            last = id;
            // The newest row goes, as `skiff ledger rm` does it.
            db.execute("DELETE FROM tasks WHERE id = ?1", [id]).unwrap();
        }
        assert!(one(&db, &last.to_string()).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_plan_lists_its_tasks_under_it_with_paths_in_its_folder() {
        let dir = scratch("plan");
        let file = dir.join("ledger.db");
        let mut db = open_at(&file).unwrap();
        let (_, lone, lone_path, _) = insert(&mut db, &file, task("Fix the login bug!")).unwrap();
        let plan_row = New { plan: true, pane_id: None, pane_gen: None, reference: Some("JIRA-12".into()), ..task("Release 0.7: notes & builds") };
        let (pid, puid, plan_path, _) = insert(&mut db, &file, plan_row).unwrap();
        let (_, a, a_path, _) = insert(&mut db, &file, New { parent: Some(puid.clone()), ..task("Write the release notes") }).unwrap();
        let (_, b, b_path, _) = insert(&mut db, &file, New { parent: Some(pid.to_string()), ..task("Build the Linux packages") }).unwrap();
        let results = dir.join("results");
        assert_eq!(lone_path, results.join(format!("{lone}-fix-the-login-bug")).join("result.md").display().to_string());
        let folder = results.join(format!("{puid}-release-0-7-notes-builds"));
        assert_eq!(plan_path, folder.join("plan.md").display().to_string());
        assert_eq!(a_path, folder.join(format!("{a}-write-the-release-notes")).join("result.md").display().to_string());
        assert_eq!(b_path, folder.join(format!("{b}-build-the-linux-packages")).join("result.md").display().to_string());
        // `add` makes each work folder, and no file.
        for d in [results.join(format!("{lone}-fix-the-login-bug")), folder.join(format!("{a}-write-the-release-notes"))] {
            assert!(d.is_dir() && std::fs::read_dir(&d).unwrap().next().is_none(), "{}", d.display());
        }
        assert_eq!(one(&db, &a).unwrap().work_dir, Some(folder.join(format!("{a}-write-the-release-notes")).display().to_string()));
        // A task in a plan names its plan.
        let (_, x, _, plan) = insert(&mut db, &file, New { parent: Some(a.clone()), ..task("x") }).unwrap();
        assert_eq!(plan.as_deref(), Some(puid.as_str()));
        db.execute("DELETE FROM tasks WHERE uid = ?1", [&x]).unwrap();

        let shown: Vec<_> = tree(rows(&db, "", None).unwrap()).into_iter().map(|(r, child)| (r.uid, child)).collect();
        assert_eq!(shown, [(lone.clone(), 0), (puid.clone(), 0), (a, 1), (b, 1)]);
        let p = one(&db, &puid).unwrap();
        assert!(p.is_plan() && p.pane_id.is_none() && p.reference.as_deref() == Some("JIRA-12"));
        // The watcher gets the open tasks, not the plan.
        assert_eq!(open_tasks_at(&file).unwrap().len(), 3);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_hand_off_gets_a_folder_with_its_project_and_an_old_plan_keeps_its_folder() {
        let dir = scratch("handoff-dir");
        let file = dir.join("ledger.db");
        let mut db = open_at(&file).unwrap();
        let (_, p, _, _) = insert(&mut db, &file, New { plan: true, pane_id: None, title: Some("Ship it".into()), ..task("p") }).unwrap();
        // A hand-off under Admiral's plan goes with the project Skipper's results, not in the plan's folder.
        let (_, h, h_path, _) = insert(&mut db, &file, New { parent: Some(p.clone()), handoff: true, project: "web".into(), title: Some("Fix web".into()), ..task("h") }).unwrap();
        let h_dir = dir.join("projects/web/results").join(format!("{h}-fix-web"));
        assert_eq!(h_path, h_dir.join("result.md").display().to_string());
        // The project Skipper's task under it nests in it.
        let (_, k, k_path, _) = insert(&mut db, &file, New { parent: Some(h.clone()), owner: "web".into(), ..task("Do the fix") }).unwrap();
        assert_eq!(k_path, h_dir.join(format!("{k}-do-the-fix")).join("result.md").display().to_string());
        assert!(h_dir.join(format!("{k}-do-the-fix")).is_dir());
        // A named result path stays, and the folder is still made.
        let (_, n, n_path, _) = insert(&mut db, &file, New { result: Some("/elsewhere/r.md".into()), ..task("named") }).unwrap();
        assert_eq!(n_path, "/elsewhere/r.md");
        assert!(dir.join("results").join(format!("{n}-named")).is_dir());
        // A plan from before work folders: a new task goes in the folder it had.
        db.execute("UPDATE tasks SET work_dir = NULL, result_path = ?2 WHERE uid = ?1", params![p, dir.join("results/2026-10-01-old-plan/plan.md").display().to_string()]).unwrap();
        let (_, t, t_path, _) = insert(&mut db, &file, New { parent: Some(p.clone()), ..task("late") }).unwrap();
        assert_eq!(t_path, dir.join("results/2026-10-01-old-plan").join(format!("{t}-late")).join("result.md").display().to_string());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_version_10_ledger_takes_work_dir_and_flushed_at_in_place_and_keeps_its_rows() {
        let dir = scratch("v10");
        let file = dir.join("ledger.db");
        {
            let v10 = TASKS.replace(",\n  work_dir    TEXT,\n  flushed_at  TEXT", "");
            assert_ne!(v10, TASKS);
            let db = Connection::open(&file).unwrap();
            db.execute_batch(&v10).unwrap();
            db.execute(
                "INSERT INTO tasks (uid, kind, project, task, sent_at, state, result_path, updated_at) VALUES ('ab12cd34', 'task', 'p', 'old', 'x', 'done', '/r/old.md', 'x')",
                [],
            )
            .unwrap();
            // A reader takes the version 10 file: the new columns read as empty.
            let r = skiff_client::ledger::rows(&db, "", None).unwrap();
            assert_eq!((r[0].work_dir.as_deref(), r[0].flushed_at.as_deref()), (None, None));
        }
        let db = open_at(&file).unwrap();
        let r = one(&db, "ab12cd34").unwrap();
        assert_eq!((r.result_path.as_deref(), r.work_dir.as_deref()), (Some("/r/old.md"), None));
        db.execute("UPDATE tasks SET flushed_at = 'now' WHERE uid = 'ab12cd34'", []).unwrap();
        assert_eq!(one(&db, "ab12cd34").unwrap().flushed_at.as_deref(), Some("now"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn prune_with_a_flush_takes_the_done_folders_and_keeps_one_that_just_changed() {
        let dir = scratch("prune-flush").canonicalize().unwrap();
        let file = dir.join("ledger.db");
        let mut db = open_at(&file).unwrap();
        let (_, old, _, _) = insert(&mut db, &file, New { state: "done".into(), ..task("old work") }).unwrap();
        let (_, fresh, _, _) = insert(&mut db, &file, New { state: "done".into(), ..task("fresh work") }).unwrap();
        let (_, open, _, _) = insert(&mut db, &file, task("open work")).unwrap();
        let dirs_of: std::collections::HashMap<String, PathBuf> =
            [&old, &fresh, &open].into_iter().map(|u| (u.clone(), PathBuf::from(one(&db, u).unwrap().work_dir.unwrap()))).collect();
        let folder = |uid: &str| dirs_of[uid].clone();
        let age = |p: &Path, secs: u64| {
            std::fs::File::options().write(true).open(p).unwrap().set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(secs)).unwrap();
        };
        for (uid, secs) in [(&old, 3600), (&fresh, 300), (&open, 3600)] {
            std::fs::write(folder(uid).join("result.md"), "r").unwrap();
            std::fs::create_dir_all(folder(uid).join("shots")).unwrap();
            std::fs::write(folder(uid).join("shots/a.png"), "png").unwrap();
            age(&folder(uid).join("result.md"), secs);
            age(&folder(uid).join("shots/a.png"), secs);
        }
        close(&mut db, &Caller::Person, &[old.clone(), fresh.clone()]).unwrap();
        db.execute("UPDATE tasks SET closed_at = '2000-01-01T00:00:00Z' WHERE closed_at IS NOT NULL", []).unwrap();
        // The listing names the closed work only; its folders are judged before the rows go.
        let gone = prune(&mut db, &Caller::Person, 86_400, false).unwrap();
        let dirs: Vec<PathBuf> = gone.iter().filter_map(|r| r.work_dir.as_deref().map(PathBuf::from)).collect();
        assert_eq!(dirs, [folder(&old), folder(&fresh)]);
        let (out, stays) = skiff_client::disk::flush_work(&dir, &dir.join("cache"), &file, Some(&[]), Path::new("/tmp/here.sock"), &dirs);
        assert_eq!(out.files, 2);
        assert!(!folder(&old).exists());
        assert_eq!(stays, [(folder(&fresh), "it changed in the last 15 minutes".to_string())]);
        // Then the rows go; the kept folder and the open work stay.
        assert_eq!(prune(&mut db, &Caller::Person, 86_400, true).unwrap().len(), 2);
        assert!(one(&db, &old).is_err() && folder(&open).join("result.md").exists());
        assert!(dir.join("results").join(format!("{fresh}-fresh-work/result.md")).exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_restarted_pane_interrupts_a_running_task() {
        let dir = scratch("gen");
        let file = dir.join("ledger.db");
        let mut db = open_at(&file).unwrap();
        insert(&mut db, &file, task("t")).unwrap();
        let mut r = rows(&db, "", None).unwrap().remove(0);
        std::fs::remove_dir_all(&dir).unwrap();
        let s = |started_at: u64| -> SessionInfo {
            serde_json::from_value(serde_json::json!({
                "id": "abc123456789", "label": "x", "role": "agent", "cwd": "/", "command": "claude", "args": [],
                "state": "idle", "cols": 80, "rows": 24, "pid": 1, "exit_code": null, "started_at": started_at,
            }))
            .unwrap()
        };
        // The same program: a reload keeps the start time.
        assert!(!r.interrupted_in(Some(&s(1000))));
        // A restore starts a new shell.
        assert!(r.interrupted_in(Some(&s(2000))));
        // A pane that is gone is LIVE no, not interrupted.
        assert!(!r.interrupted_in(None));
        // A daemon too old to send a start time tells nothing.
        assert!(!r.interrupted_in(Some(&s(0))));
        // Only work that ran: a pending task was never delivered.
        r.state = "pending".into();
        assert!(!r.interrupted_in(Some(&s(2000))));
        r.state = "blocked".into();
        assert!(r.interrupted_in(Some(&s(2000))));
    }

    #[test]
    fn only_a_pending_task_is_sent_and_a_refusal_keeps_it_pending() {
        let dir = scratch("send");
        let file = dir.join("ledger.db");
        let mut db = open_at(&file).unwrap();
        let (_, uid, result, _) = insert(&mut db, &file, New { state: "pending".into(), ..task("fix the tests") }).unwrap();
        let (_, plan_uid, _, _) = insert(&mut db, &file, New { plan: true, pane_id: None, ..task("p") }).unwrap();
        let mut r = one(&db, &uid).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        let pane = |started_at: u64| -> SessionInfo {
            serde_json::from_value(serde_json::json!({
                "id": "abc123456789", "label": "x", "role": "agent", "cwd": "/", "command": "claude", "args": [],
                "state": "idle", "cols": 80, "rows": 24, "pid": 1, "exit_code": null, "started_at": started_at,
            }))
            .unwrap()
        };
        let (same, restarted) = (pane(1000), pane(2000));
        assert_eq!(unsendable(&r, Some(&same)), None);
        assert!(unsendable(&r, None).unwrap().contains("gone"));
        let folder = dir.join("results").join(format!("{uid}-fix-the-tests"));
        assert_eq!(result, folder.join("result.md").display().to_string());
        assert_eq!(
            message(&r),
            format!("fix the tests Put everything for this task in {}: the result, screenshots, scratch copies and worktrees. Write your result to {result}.", folder.display())
        );
        // A row from before work folders keeps the old text.
        let old = Row { work_dir: None, ..r.clone() };
        assert_eq!(message(&old), format!("fix the tests Write your result to {result}."));
        r.state = "sent".into();
        assert!(unsendable(&r, Some(&same)).unwrap().contains("not pending"));
        // A restart interrupted it: it can go again.
        assert_eq!(unsendable(&r, Some(&restarted)), None);
        assert!(unsendable(&one(&db, &plan_uid).unwrap(), Some(&same)).unwrap().contains("plan"));
        assert_eq!(state_after(0), Some("sent"));
        assert_eq!(state_after(8), Some("blocked"));
        for refused in [1, 3, 4, 5, 6, 7, 9] {
            assert_eq!(state_after(refused), None);
        }
    }

    #[test]
    fn a_gone_pane_shows_gone_and_a_plan_follows_its_tasks() {
        let dir = scratch("roll");
        let file = dir.join("ledger.db");
        let mut db = open_at(&file).unwrap();
        let (_, p, _, _) = insert(&mut db, &file, New { plan: true, pane_id: None, ..task("p") }).unwrap();
        let (_, a, _, _) = insert(&mut db, &file, New { parent: Some(p.clone()), ..task("a") }).unwrap();
        insert(&mut db, &file, New { parent: Some(p.clone()), state: "done".into(), ..task("b") }).unwrap();
        let (_, empty, _, _) = insert(&mut db, &file, New { plan: true, pane_id: None, state: "blocked".into(), ..task("e") }).unwrap();
        let mut list = rows(&db, "", None).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        let st = |list: &[Row], uid: &str| list.iter().find(|r| r.uid == uid).unwrap().shown_state().to_string();
        roll_up(&mut list);
        assert_eq!(st(&list, &p), "sent");
        // A plan with no task keeps the state it was given.
        assert_eq!(st(&list, &empty), "blocked");
        let ra = list.iter_mut().find(|r| r.uid == a).unwrap();
        ra.live = Some(false);
        assert_eq!(ra.shown_state(), GONE);
        ra.state = "done".into();
        // Done work stays done when its pane closes.
        assert_eq!(ra.shown_state(), "done");
        roll_up(&mut list);
        assert_eq!(st(&list, &p), "done");
    }

    #[test]
    fn a_restarted_skipper_recovers_only_interrupted_handoffs_once() {
        let dir = scratch("skipper-restart");
        let file = dir.join("ledger.db");
        let mut db = open_at(&file).unwrap();
        let mut old = pane("abc123456789", "shell", false);
        old.started_at = 2000;
        let mut new = pane("newskipper00", "agent", false);
        new.started_at = 3000;
        for scope in ["web", "fleet:Harbor"] {
            let hand = |st: &str| New { handoff: true, project: scope.into(), state: st.into(), ..task("resume me") };
            let (_, sent, result, _) = insert(&mut db, &file, hand("sent")).unwrap();
            let (_, blocked, _, _) = insert(&mut db, &file, hand("blocked")).unwrap();
            let mut unchanged = Vec::new();
            for n in [hand("done"), hand("pending"), New { pane_gen: Some(2000), ..hand("sent") },
                New { handoff: false, ..hand("sent") }, New { project: "other".into(), ..hand("sent") }] {
                let (_, uid, _, _) = insert(&mut db, &file, n).unwrap();
                unchanged.push((uid.clone(), one(&db, &uid).unwrap()));
            }
            skiff_client::fleet::restart_rows(&file, scope, &new, &[old.clone()]).unwrap();
            for uid in [&sent, &blocked] {
                let r = one(&db, uid).unwrap();
                assert_eq!((r.state.as_str(), r.pane_id.as_deref(), r.pane_gen), ("pending", Some(new.id.as_str()), Some(3000)));
                assert_eq!((r.owner.as_str(), r.project.as_str()), (GRAND, scope));
                assert!(!r.interrupted_in(Some(&new)));
            }
            assert_eq!(one(&db, &sent).unwrap().result_path.as_deref(), Some(result.as_str()));
            for (uid, before) in unchanged {
                assert_eq!(serde_json::to_value(one(&db, &uid).unwrap()).unwrap(), serde_json::to_value(before).unwrap());
            }
            let notes = skiff_client::ledger::notes(&file, GRAND).unwrap();
            assert_eq!(notes.len(), 1);
            assert!(notes[0].1.contains(&sent) && notes[0].1.contains(&blocked) && notes[0].1.contains("Resend"));
            skiff_client::fleet::restart_rows(&file, scope, &new, &[old.clone(), new.clone()]).unwrap();
            assert_eq!(skiff_client::ledger::notes(&file, GRAND).unwrap(), notes);
            skiff_client::ledger::drop_notes(&file, &[notes[0].0]).unwrap();
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_takeover_moves_every_row_and_gives_the_hand_offs_the_fleet_skipper() {
        let dir = scratch("takeover");
        let file = dir.join("ledger.db");
        let mut db = open_at(&file).unwrap();
        let mine = |t: &str| New { owner: "web".into(), project: "web".into(), ..task(t) };
        let (_, open, _, _) = insert(&mut db, &file, New { title: Some("Update client".into()), ..mine("update") }).unwrap();
        let (_, closed, closed_path, _) = insert(&mut db, &file, New { state: "done".into(), ..mine("old") }).unwrap();
        let hand = |st: &str| New { handoff: true, pane_id: Some("websk0000000".into()), state: st.into(), project: "web".into(), ..task("h") };
        let (_, sent, _, _) = insert(&mut db, &file, hand("sent")).unwrap();
        let (_, blocked, _, _) = insert(&mut db, &file, hand("blocked")).unwrap();
        let (_, finished, _, _) = insert(&mut db, &file, hand("done")).unwrap();
        let (_, other, _, _) = insert(&mut db, &file, New { owner: "api".into(), ..task("api") }).unwrap();
        let fs = pane("fleet0000000", "agent", false);
        let t = vec![skiff_client::fleet::Taken {
            project: "web".into(),
            session: "websk0000000".into(),
            tasks: vec![skiff_client::fleet::Task { uid: open.clone(), title: "Update client".into() }],
            handoffs: vec![sent.clone(), blocked.clone()],
        }];
        skiff_client::fleet::move_rows(&file, Path::new("/tmp/here.sock"), "Harbor", &fs, &t, true).unwrap();
        let r = |uid: &str| one(&db, uid).unwrap();
        assert_eq!(r(&open).owner, "fleet:Harbor");
        assert_eq!((r(&closed).owner.as_str(), r(&closed).result_path.as_deref()), ("fleet:Harbor", Some(closed_path.as_str())));
        assert_eq!(r(&other).owner, "api");
        for (uid, st) in [(&sent, "sent"), (&blocked, "pending")] {
            let h = r(uid);
            assert_eq!((h.owner.as_str(), h.project.as_str(), h.state.as_str(), h.pane_id.as_deref()), (GRAND, "fleet:Harbor", st, Some("fleet0000000")));
        }
        assert_eq!(r(&finished).project, "web");
        let notes = skiff_client::ledger::notes(&file, "fleet:Harbor").unwrap();
        assert_eq!(notes.len(), 1);
        assert!(notes[0].1.starts_with("The Harbor Skipper took over web: 1 open task"), "{}", notes[0].1);
        // The fleet Skipper reports a hand-off it took over, and adds under it.
        let me = Caller::Project("fleet:Harbor".into());
        assert!(me.check_report(&r(&sent)).is_ok());
        assert_eq!(me.check_parent(&r(&sent)).unwrap(), "fleet:Harbor");
        assert!(me.check(&r(&open)).is_ok());
        assert!(Caller::Project("web".into()).check(&r(&open)).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_fleet_skippers_rows_go_to_its_folder_and_a_name_that_is_both_is_refused() {
        let l = Path::new("/a/ledger.db");
        assert_eq!(results_dir(l, "fleet:Harbor"), Path::new("/a/fleets/Harbor/results"));
        assert_eq!(results_dir(l, "web"), Path::new("/a/projects/web/results"));
        assert_eq!(who("fleet:Harbor"), "the Harbor fleet Skipper");
        let cfg = skiff_core::config::parse(
            "[[project]]\nname = \"web\"\npath = \"/w\"\nfleet = \"Harbor\"\n\n[[project]]\nname = \"api\"\npath = \"/a\"\nfleet = \"Port\"\n\n[[project]]\nname = \"Port\"\npath = \"/p\"\n",
        )
        .unwrap();
        assert_eq!(scope(&cfg, "web").unwrap(), Scope::Project("web".into()));
        assert_eq!(scope(&cfg, "Harbor").unwrap(), Scope::Fleet("Harbor".into()));
        let err = scope(&cfg, "Port").unwrap_err().to_string();
        assert!(err.contains("project:Port") && err.contains("fleet:Port"), "{err}");
        assert_eq!(scope(&cfg, "fleet:Port").unwrap(), Scope::Fleet("Port".into()));
        assert_eq!(scope(&cfg, "project:Port").unwrap(), Scope::Project("Port".into()));
        assert!(scope(&cfg, "fleet:Nope").is_err());
    }

    #[test]
    fn a_retiring_skipper_is_refused() {
        let live = Path::new("/tmp/live.sock");
        let mut s = pane("retired00000", "agent", false);
        s.retiring = true;
        assert_eq!(writer(Some("retired00000"), Some(&s), None, live, false), Writer::Retired);
        let mut f = pane("fleet0000000", "agent", false);
        f.skipper = Some("fleet:Harbor".into());
        assert_eq!(writer(Some("fleet0000000"), Some(&f), None, live, false), Writer::Yes(Caller::Project("fleet:Harbor".into())));
    }
}
