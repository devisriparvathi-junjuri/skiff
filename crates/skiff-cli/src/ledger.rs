//! `skiff ledger`: the Skiff agent's task record, a SQLite file in its folder.
//! The agent reads and writes it through these commands only, so a bad edit,
//! a compaction or a second writer cannot break it.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use rusqlite::{params, Connection, TransactionBehavior};
use skiff_core::{
    session::{Role, SessionInfo},
    state,
};

use crate::{print, resolve, sessions, LedgerCmd};
pub use skiff_client::ledger::{path, Row, GRAND};
use skiff_client::ledger::{gen, heads, mark_live, roll_up, rows, GONE, INTERRUPTED};

mod schema;
mod paths;
mod read;
mod write;
mod commands;

use schema::NOW;
use paths::{label, make_work_folder, new_uid, result_in, work_folder};
pub use paths::results_dir;
use write::*;
pub use write::{claim, Caller, SendOpts};
pub use read::{close_gone, gone_unreported, handed_back, open_tasks, plan_tasks, unreported};
use read::{container, head, join, keep, live, one, pane, print_one, print_rows, project_of, rank, state, sweep, top, tree, under, View};
#[cfg(test)]
use read::{close_gone_in, open_tasks_at, unreported_at};
#[cfg(test)]
use paths::container_folder;
#[cfg(test)]
use schema::{OWNER, TASKS, UIDS};
#[cfg(test)]
use skiff_client::ledger::{OPEN, VERSION, WORKING};

fn open() -> Result<Connection> {
    schema::open()
}

fn open_at(file: &Path) -> Result<Connection> {
    schema::open_at(file)
}




/// The exit code: `send`'s for `send` and `add --send`, else 0.
/// Runs one `skiff ledger` command. After a command that writes, work that
/// is done and reported closes by itself (see [`auto_close`]).
pub async fn run(socket: &Path, cmd: LedgerCmd, person: bool) -> Result<i32> {
    let writes = !matches!(cmd, LedgerCmd::Ls { .. } | LedgerCmd::Show { .. } | LedgerCmd::Dir { .. });
    let code = commands::run_one(socket, cmd, person).await?;
    if writes {
        for r in auto_close(&mut open()?)? {
            eprintln!("skiff: closed {} {}: every task is done and reported", r.kind, r.uid);
        }
    }
    Ok(code)
}


#[cfg(test)]
mod tests;
