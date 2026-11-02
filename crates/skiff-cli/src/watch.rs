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
    time::Duration,
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

use crate::{ledger, print, resolve, sessions};

mod model;
mod runtime;
mod state;
mod terminal;

pub use model::{wake_line, Options};
pub use terminal::{input_box_rows, read_front, rows_of, Front};
use model::{Fact, Now, Pane, Wake};
#[cfg(test)]
use runtime::*;
use state::*;
use terminal::desktop_notify;
#[cfg(test)]
use terminal::{busy_line, front, input_box};

/// How often the ledger and the session list are read again without an event.
const TICK: Duration = Duration::from_secs(2);
/// How long the watcher waits for skiffd to come back: a reload, or a restart.
const RECONNECT_LIMIT: Duration = Duration::from_secs(600);
/// How long a new watcher waits for the lock, so an old one can finish.
const LOCK_WAIT: Duration = Duration::from_secs(10);
/// Failed ledger reads in a row before the watcher reports a failure.
const LEDGER_FAILS: u32 = 3;
/// How old a new `skiff` file must be before the watcher runs it: a build
/// may still write it.
const SETTLE: Duration = Duration::from_secs(2);

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
    runtime::run(socket, o).await
}

#[cfg(test)]
pub(crate) mod tests;
