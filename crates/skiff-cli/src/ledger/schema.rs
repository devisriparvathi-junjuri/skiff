//! The ledger's SQLite schema: opening a ledger, and the migrations that bring an older one up to date.

use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::{Connection, TransactionBehavior};
use skiff_core::state;

use super::path;
use skiff_client::ledger::VERSION;

pub(super) const TASKS: &str = "
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
/// [`super::auto_close`]). The head of a hand-off's task is two steps up.
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
pub(super) const OWNER: &str = "
CREATE TABLE IF NOT EXISTS owner (
  id      INTEGER PRIMARY KEY CHECK (id = 1),
  session TEXT NOT NULL,
  socket  TEXT NOT NULL,
  since   TEXT NOT NULL
);
";

/// Every uid the ledger gave out, so a removed row's uid is not given again.
pub(super) const UIDS: &str = "CREATE TABLE IF NOT EXISTS uids (uid TEXT PRIMARY KEY);";

/// UTC, to the second, set by SQLite.
pub(super) const NOW: &str = "strftime('%Y-%m-%dT%H:%M:%SZ', 'now')";

/// Opens the ledger, making it on first use. The agent's folder is made
/// first when it is missing, as the app makes it at start.
pub(super) fn open() -> Result<Connection> {
    if std::env::var_os("SKIFF_LEDGER").is_none() {
        skiff_client::skipper::make_folder(&skiff_core::config::load()?)?;
    }
    open_at(&path()?)
}

pub(super) fn open_at(file: &Path) -> Result<Connection> {
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
///   closes at the next write (see [`super::auto_close`]).
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
