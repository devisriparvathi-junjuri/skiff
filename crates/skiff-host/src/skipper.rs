//! Skipper and the agents on this Host, through the data plane (M6, #166 R7
//! and R8): settings changes and the folder change (`skipper_config@1`),
//! the instruction files (`skipper_file@1`),
//! Skipper's session and its watcher (`skipper_run@1`), fleet Skippers and
//! their takeover (`skipper_run@2`), and the `[agents]`
//! table (`agent_config@1`). The same `skiff-client` code the app runs on
//! its own desktop does the work, against this Host's config and skiffd.
//!
//! The watcher is this service's to keep: [`supervise`] checks on Skipper's
//! session now and then and starts `skiff watch` when none runs. It never
//! stops or replaces a watcher; the lock beside the socket keeps it to one.
use std::{path::PathBuf, time::Duration};

use skiff_client::{agent_folder, fleet, host::project_agent, skipper, Client};
use skiff_core::{
    config,
    session::{SessionInfo, SessionState},
};
use skiff_host_api::{AgentEdit, Error, Request, Response, SkipperOpened};

use crate::{failed, refused, SESSIONS_TIMEOUT};

/// How often [`supervise`] looks for Skipper's session without a watcher.
const WATCH_EVERY: Duration = Duration::from_secs(15);

/// One request of the three families. The caller checked the capability.
pub async fn answer(request: Request) -> Result<Response, Error> {
    match request {
        Request::SkipperEdit { edit } => {
            let settings = blocking(move || {
                skipper::apply(&edit).map_err(refused)?;
                settings()
            })
            .await?;
            Ok(Response::Skipper { settings: Box::new(settings) })
        }
        // A file by what it is for. The Host makes the path; a name that
        // is not its fleet, project or task type is refused.
        Request::SkipperFileRead { scope } => {
            let file = blocking(move || skipper::host_read(&config::load().map_err(failed)?, &scope).map_err(invalid)).await?;
            Ok(Response::SkipperFile { file })
        }
        Request::SkipperFileWrite { scope, text, revision } => {
            let wrote = blocking(move || skipper::host_write(&config::load().map_err(failed)?, &scope, &text, &revision).map_err(invalid)).await?;
            match wrote {
                skipper::Wrote::Done(file) => Ok(Response::SkipperFile { file }),
                skipper::Wrote::Changed => Err(Error::Conflict { message: "The file changed on this Host since it was read.".into() }),
            }
        }
        Request::SkipperFileReset { scope } => {
            let settings = blocking(move || {
                let cfg = config::load().map_err(failed)?;
                skipper::reset_file(&cfg, &skipper::scope_of(&scope).map_err(invalid)?).map_err(invalid)?;
                settings()
            })
            .await?;
            Ok(Response::Skipper { settings: Box::new(settings) })
        }
        Request::SkipperFolderPreview { path } => {
            let (panes, skipper) = live().await;
            let preview = blocking(move || {
                let home = skipper::home().map_err(failed)?;
                let to = agent_folder::resolve(&path, &home, &home).map_err(invalid)?;
                let cfg = config::load().map_err(failed)?;
                agent_folder::preview(&cfg, &to, &panes).map_err(failed)
            })
            .await?;
            Ok(Response::SkipperFolder { preview: Box::new(preview.to_host(skipper.is_some())) })
        }
        Request::SkipperFolderChange { path, mode, end_skipper, anyway } => {
            let (mut panes, skipper) = live().await;
            if let Some(s) = skipper {
                if !end_skipper {
                    return Err(Error::Conflict { message: "Skipper runs. It ends before the change.".into() });
                }
                let c = connect().await?;
                c.kill(&s.id).await.map_err(failed)?;
                panes.remove(&s.id);
            }
            let done = blocking(move || {
                let cfg = config::load().map_err(failed)?;
                agent_folder::change(&cfg, &PathBuf::from(path), mode.into(), &panes, anyway).map_err(failed)
            })
            .await?;
            Ok(Response::SkipperFolderDone { done: done.to_host() })
        }
        Request::SkipperOpen { restore } => {
            let c = connect().await?;
            let how = match restore {
                Some(expect) => skipper::Open::Restore { expect },
                None => skipper::Open::Start,
            };
            let Some(found) = skipper::open(&c, how).await.map_err(failed)? else { return Ok(Response::Ok) };
            let watch_error = ensure_watch(found.session.id.clone()).await.err();
            let session = skiff_client::host::project_session(found.session)?;
            Ok(Response::SkipperOpened {
                opened: Box::new(SkipperOpened { session, created: found.created, typed: found.typed, watch_error }),
            })
        }
        Request::SkipperWatch { session } => {
            let s = coordinator().await?;
            if s.as_ref().map(|s| s.id.as_str()) != Some(session.as_str()) {
                return Err(Error::NotFound { message: "This session is not Skipper's on this Host.".into() });
            }
            let started = ensure_watch(session).await.map_err(|message| Error::OperationFailed { message })?;
            Ok(Response::Watch { started })
        }
        Request::SkipperProject { project } => {
            let id = blocking(move || skipper::start_project(&skiff_core::state::socket(), &cli(), &project).map_err(failed)).await?;
            let c = connect().await?;
            let s = c.list_sessions().await.map_err(failed)?.into_iter().find(|s| s.id == id);
            let s = s.ok_or_else(|| Error::OperationFailed { message: format!("session {id} is gone") })?;
            Ok(Response::Session { session: Box::new(skiff_client::host::project_session(s)?) })
        }
        Request::SkipperFleet { fleet } => {
            let c = connect().await?;
            let daemon = c.daemon_info().await.map_err(failed)?;
            if skipper::too_old_for_fleet(&daemon.version) {
                return Err(Error::OperationFailed { message: format!("skiffd {} on this Host cannot start a fleet Skipper. Update Skiff on this Host first", daemon.version) });
            }
            let id = blocking(move || skipper::start_fleet(&skiff_core::state::socket(), &cli(), &fleet).map_err(failed)).await?;
            let s = c.list_sessions().await.map_err(failed)?.into_iter().find(|s| s.id == id);
            let s = s.ok_or_else(|| Error::OperationFailed { message: format!("session {id} is gone") })?;
            Ok(Response::Session { session: Box::new(skiff_client::host::project_session(s)?) })
        }
        Request::FleetTakeoverPreview { fleet, join } => {
            let c = connect().await?;
            let taken = fleet::preview(&c, &fleet, join.as_deref()).await.map_err(failed)?;
            Ok(Response::FleetTakeover { taken: taken.iter().map(fleet::Taken::to_host).collect() })
        }
        Request::FleetTakeover { fleet, join } => {
            let c = connect().await?;
            let taken = fleet::takeover(&c, &skiff_core::state::socket(), &fleet, Some(&join), true).await.map_err(failed)?;
            Ok(Response::FleetTakeover { taken: taken.iter().map(fleet::Taken::to_host).collect() })
        }
        Request::FleetRename { old, new } => {
            let c = connect().await?;
            fleet::rename(&c, &old, new.trim()).await.map_err(refused)?;
            Ok(Response::Ok)
        }
        Request::AgentConfig => {
            let revision = blocking(|| Ok(config::agents_revision(&config::load().map_err(failed)?.agents))).await?;
            Ok(Response::AgentConfig { agents: agents().await?, revision })
        }
        Request::AgentEdit { revision, edit } => {
            let ((), revision) = blocking(move || {
                config::edit_agents_at(&revision, || match &edit {
                    AgentEdit::Enabled { agents } => config::set_enabled_agents(agents),
                    AgentEdit::Command { agent, command } => config::set_agent_command(agent, command),
                    AgentEdit::Rename { agent, name } => config::rename_agent(agent, name),
                    AgentEdit::Remove { agent } => config::remove_agent(agent),
                    AgentEdit::Restore => config::restore_agents(),
                })
                .map_err(refused)
            })
            .await?;
            Ok(Response::AgentConfig { agents: agents().await?, revision })
        }
        _ => Err(Error::OperationFailed { message: "not a Skipper or agent request".into() }),
    }
}

/// Keeps a watcher on Skipper's session while this service runs.
pub async fn supervise() {
    let mut tick = tokio::time::interval(WATCH_EVERY);
    loop {
        tick.tick().await;
        let Ok(Some(s)) = coordinator().await else { continue };
        if s.watch.is_none() {
            let _ = ensure_watch(s.id).await;
        }
    }
}

/// Starts `skiff watch` for `session` unless one runs. True when it started
/// one. A failure is said in this service's log too.
async fn ensure_watch(session: String) -> Result<bool, String> {
    let socket = skiff_core::state::socket();
    let run = tokio::task::spawn_blocking(move || skipper::ensure_watch(&socket, &cli(), skipper::CLI_VERSION, &session, &[], None)).await;
    let out = match run {
        Ok(Ok(o)) => Ok(matches!(o, skipper::WatchOutcome::Started | skipper::WatchOutcome::Replaced { .. })),
        Ok(Err(e)) => Err(format!("{e:#}")),
        Err(e) => Err(e.to_string()),
    };
    if let Err(e) = &out {
        eprintln!("skiff watch: {e}");
    }
    out
}

/// The `skiff` CLI beside this binary, as a managed install has it, else
/// the one on `PATH`.
fn cli() -> PathBuf {
    std::env::current_exe()
        .ok()
        .map(|e| e.with_file_name("skiff"))
        .filter(|p| p.is_file())
        .unwrap_or_else(|| PathBuf::from("skiff"))
}

/// Skipper's live session on this Host.
async fn coordinator() -> Result<Option<SessionInfo>, Error> {
    let c = connect().await?;
    let all = tokio::time::timeout(SESSIONS_TIMEOUT, c.list_sessions()).await.map_err(|_| Error::TimedOut)?.map_err(failed)?;
    Ok(all.into_iter().find(|s| s.coordinator && s.state != SessionState::Done))
}

/// The live panes and Skipper's session. With no skiffd, none run.
async fn live() -> (agent_folder::Panes, Option<SessionInfo>) {
    let Ok(c) = connect().await else { return Default::default() };
    match tokio::time::timeout(SESSIONS_TIMEOUT, agent_folder::sessions(&c)).await {
        Ok(Ok(found)) => found,
        _ => Default::default(),
    }
}

/// The agents as this Host's skiffd lists them: installed from its PATH.
async fn agents() -> Result<Vec<skiff_host_api::HostAgent>, Error> {
    let c = connect().await?;
    let mut list = tokio::time::timeout(SESSIONS_TIMEOUT, c.list_agents()).await.map_err(|_| Error::TimedOut)?.map_err(failed)?;
    // What uses each agent comes from this Host's config, whatever its skiffd build.
    skiff_core::project::mark_used(&mut list);
    Ok(list.into_iter().map(project_agent).collect())
}

/// The skiffd this service is paired with.
async fn connect() -> Result<Client, Error> {
    let socket = skiff_core::state::socket();
    match tokio::time::timeout(SESSIONS_TIMEOUT, Client::connect(&socket)).await {
        Ok(Ok(c)) => Ok(c),
        _ => Err(Error::OperationFailed { message: "Skiff cannot reach this Host's sessions service.".into() }),
    }
}

fn settings() -> Result<skiff_host_api::HostSkipper, Error> {
    let cfg = config::load().map_err(failed)?;
    skipper::settings(&cfg).map_err(failed)
}

fn invalid(e: anyhow::Error) -> Error {
    Error::Invalid { message: format!("{e:#}") }
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T, Error> + Send + 'static) -> Result<T, Error> {
    tokio::task::spawn_blocking(f).await.unwrap_or_else(|e| Err(failed(e)))
}
