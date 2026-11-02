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

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use skiff_core::{
    config::{self, Config, ProjectConfig},
    project::AgentInfo,
    session::{Role, SessionInfo, SessionSpec, SessionState},
};

use crate::{Client, Coordinated, Coordinator};

mod identity;
mod folder;
mod project;
mod watch;

pub use identity::{choose, continue_line, fleet_name, fleet_scope, name, project_name, scope_fleet, with_model, DEFAULT_NAME, FLEET};
pub use folder::{apply, cache_dir, configured, default_folder, ensure, folder, home, make_folder, no_folder, refuse_live_folder, setup, skill_home, sync_skill, AGENTS_FILE};
pub use project::{edit_file, file_status, first_message, fleet_agent, fleet_folder, fleet_projects, fleet_template, instructions, project, project_agent, project_folder, project_template, reset_file, write_fleet, FileStatus, Scope, Status, SKIPPER_FILE};
pub use project::{commanding_fleet, find_fleet, find_project, is_fleet, open, open_fleet, open_project, project_skippers_in, start_fleet, start_project, too_old_for_fleet, too_old_for_project, Open, ProjectSkipper, FLEET_SKIPPER_SINCE, PROJECT_SKIPPER_SINCE};
pub use watch::{build_of, detach, is_watch_command, outliving, stale_holder, start_watch, unit_for, watch_lock, watch_status, write_holder, WatchHolder, WatchLock, WatchOutcome, CLI_VERSION, RESTART_WAIT};
#[cfg(unix)]
pub use watch::{ensure_watch, restart_watch};
pub(crate) use folder::{lock, move_record, real, refusal, setup_in, template, MARKER};
#[cfg(test)]
use project::spec;
use folder::{hash, refresh, write_file, written};

/// `[skiff_agent]` as Settings and the app's Skipper row read it, with the
/// folder's state and the command each task type starts. Reads the config
/// and the folder; changes nothing. The Host serves it (`skipper@1`).
pub fn settings(cfg: &Config) -> Result<skiff_host_api::HostSkipper> {
    use crate::agent_folder::{self, Kind};
    let (folder, mut folder_error) = match folder(cfg) {
        Ok(f) => (Some(f.display().to_string()), None),
        Err(e) => (None, Some(format!("{e:#}"))),
    };
    let configured = configured(cfg)?;
    let folder_status = match (&folder, agent_folder::kind(&configured)) {
        (None, _) => "refused",
        (_, Ok(Kind::Skiff)) => "ready",
        (_, Ok(Kind::New | Kind::Empty)) => {
            folder_error = Some(no_folder(&configured));
            "missing"
        }
        _ => "other",
    };
    let a = &cfg.skiff_agent;
    // The command lines come from this config, so the list builds the same ones skiffd would.
    let tasks = skiff_core::tasks::resolve(&a.tasks, &skiff_core::project::agents(), &configured)
        .into_iter()
        .map(|t| skiff_host_api::HostTaskType {
            name: t.name,
            agent: t.agent,
            model: t.model,
            file: t.file.display().to_string(),
            instructions: t.instructions,
            command: t.command,
            error: t.error,
        })
        .collect();
    let types = format!("{}/", skiff_core::tasks::tilde(&configured.join(skiff_core::tasks::TYPES)));
    Ok(skiff_host_api::HostSkipper {
        name: a.name.clone(),
        skipper_name: a.skipper_name.clone(),
        agent: a.agent.clone(),
        model: a.model.clone(),
        folder,
        folder_error,
        configured: configured.display().to_string(),
        folder_status: folder_status.into(),
        continue_on_restore: a.continue_on_restore,
        project_agent: a.project_agent.clone(),
        project_model: a.project_model.clone(),
        tasks,
        types,
        // A refused folder has no files to show.
        instructions: host_instructions(cfg).unwrap_or_default(),
    })
}
/// [`instructions`] as the Host serves them (`skipper@1`).
pub fn host_instructions(cfg: &Config) -> Result<Vec<skiff_host_api::HostInstruction>> {
    use skiff_host_api::{HostFileScope as H, HostInstruction};
    let status = |s: Status| match s {
        Status::None => "none",
        Status::Template => "template",
        Status::Edited => "edited",
        Status::EditedTemplateChanged => "edited-template-changed",
    };
    Ok(instructions(cfg)?
        .into_iter()
        .map(|f| HostInstruction {
            scope: match f.scope {
                Scope::Skipper => H::Skipper,
                Scope::Fleet(n) => H::Fleet(n),
                Scope::Project(n) => H::Project(n),
            },
            file: f.file,
            status: status(f.status).into(),
            modified: f.modified,
            size: f.size,
        })
        .collect())
}

/// The Skipper scope a Host request names. A task type's file is not a
/// Skipper's: it has no template.
pub fn scope_of(s: &skiff_host_api::HostFileScope) -> Result<Scope> {
    use skiff_host_api::HostFileScope as H;
    Ok(match s {
        H::Skipper => Scope::Skipper,
        H::Fleet(n) => Scope::Fleet(n.clone()),
        H::Project(n) => Scope::Project(n.clone()),
        H::Type(t) => bail!("The task type {t} has no template."),
    })
}

/// The path in a Skipper folder of the file `s` names, as its row shows it.
pub fn host_file(s: &skiff_host_api::HostFileScope) -> Result<String> {
    Ok(match s {
        skiff_host_api::HostFileScope::Type(t) => format!("types/{}.md", skiff_core::tasks::check_name(t)?),
        s => {
            let scope = scope_of(s)?;
            if let Scope::Fleet(n) | Scope::Project(n) = &scope {
                if n.is_empty() || n == "." || n == ".." || n.contains(['/', '\\']) {
                    bail!("the name {n:?} cannot name a folder");
                }
            }
            scope.file()
        }
    })
}

/// The largest instruction file `skipper_file@1` writes.
pub const FILE_MAX: usize = 1 << 20;

/// The full path of the file `s` names on this Host: a Skipper's, or a
/// task type's `types/<type>.md`.
fn host_path(cfg: &Config, s: &skiff_host_api::HostFileScope) -> Result<PathBuf> {
    match s {
        skiff_host_api::HostFileScope::Type(t) => Ok(folder(cfg)?.join("types").join(format!("{}.md", skiff_core::tasks::check_name(t)?))),
        s => {
            let scope = scope_of(s)?;
            scope.template(cfg, &folder(cfg)?)?;
            Ok(folder(cfg)?.join(scope.file()))
        }
    }
}

/// A file's revision: the hash of its bytes, or empty with no file.
fn revision(path: &Path) -> String {
    std::fs::read(path).map(|b| hash(&b)).unwrap_or_default()
}

/// Edit on this Host: the file `s` names, written first when it is
/// missing. A Skipper's comes from its template; a task type's is its
/// `# <type>` line.
pub fn edit_host_file(cfg: &Config, s: &skiff_host_api::HostFileScope) -> Result<PathBuf> {
    match s {
        skiff_host_api::HostFileScope::Type(t) => skiff_core::tasks::edit_file(&folder(cfg)?, t),
        s => edit_file(cfg, &scope_of(s)?),
    }
}

/// `skipper_file@1` read: the file and its revision. A missing file is
/// written first, as Edit does.
pub fn host_read(cfg: &Config, s: &skiff_host_api::HostFileScope) -> Result<skiff_host_api::HostSkipperFile> {
    let path = edit_host_file(cfg, s)?;
    let text = match std::fs::read_to_string(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        r => r.with_context(|| format!("read {}", path.display()))?,
    };
    Ok(skiff_host_api::HostSkipperFile { revision: revision(&path), text })
}

/// What a `skipper_file@1` write did.
pub enum Wrote {
    Done(skiff_host_api::HostSkipperFile),
    /// The file changed since the revision the client read.
    Changed,
}

/// `skipper_file@1` write: `text` replaces the file when it is still at
/// `rev`. A Skipper file keeps the hash of Skiff's last write, so the
/// person's text reads as edited.
pub fn host_write(cfg: &Config, s: &skiff_host_api::HostFileScope, text: &str, rev: &str) -> Result<Wrote> {
    if text.len() > FILE_MAX {
        bail!("The file is over 1 MiB.");
    }
    let _lock = lock()?;
    let path = host_path(cfg, s)?;
    if revision(&path) != rev {
        return Ok(Wrote::Changed);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    config::write_atomic(&path, text)?;
    Ok(Wrote::Done(skiff_host_api::HostSkipperFile { text: text.to_string(), revision: revision(&path) }))
}
