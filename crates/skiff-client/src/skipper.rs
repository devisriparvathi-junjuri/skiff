//! The Skiff agent (Skipper): one agent session that coordinates the others.
//! The app and the `skiff skipper` command share this code, so they set up
//! the same folder and find the same session.
//!
//! The daemon marks the session as the coordinator and finds or starts it in
//! one guarded request ([`Client::coordinator`]). Nothing here guesses it from
//! the folder.
//!
//! The template and the `skiff-cli` skill are built in. The skill is copied
//! out next to `projects.toml`, and the folder links to that copy, so the
//! skill updates with Skiff.
//!
//! This module is the public compatibility facade; its implementation lives in child modules.

mod identity;
mod folder;
mod project;
mod watch;
mod host;

pub use identity::{choose, continue_line, fleet_name, fleet_scope, name, project_name, scope_fleet, with_model, DEFAULT_NAME, FLEET};
pub use folder::{apply, cache_dir, configured, default_folder, ensure, folder, home, make_folder, no_folder, refuse_live_folder, setup, skill_home, sync_skill, AGENTS_FILE};
pub use project::{edit_file, file_status, first_message, fleet_agent, fleet_folder, fleet_projects, fleet_template, instructions, project, project_agent, project_folder, project_template, reset_file, write_fleet, FileStatus, Scope, Status, SKIPPER_FILE};
pub use project::{commanding_fleet, find_fleet, find_project, is_fleet, open, open_fleet, open_project, project_skippers_in, start_fleet, start_project, too_old_for_fleet, too_old_for_project, Open, ProjectSkipper, FLEET_SKIPPER_SINCE, PROJECT_SKIPPER_SINCE};
pub use watch::{build_of, detach, is_watch_command, outliving, stale_holder, start_watch, unit_for, watch_lock, watch_status, write_holder, WatchHolder, WatchLock, WatchOutcome, CLI_VERSION, RESTART_WAIT};
#[cfg(unix)]
pub use watch::{ensure_watch, restart_watch};
pub use host::{edit_host_file, host_file, host_instructions, host_read, host_write, scope_of, settings, Wrote, FILE_MAX};
pub(crate) use folder::{lock, move_record, real, refusal, setup_in, template, MARKER};
#[cfg(test)]
use project::spec;
