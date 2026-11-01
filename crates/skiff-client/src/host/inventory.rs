//! Lossless local inventory boundary. Exhaustive mappings deliberately make
//! new core fields/enum variants a compile error until explicitly projected.
use skiff_core::session as core;
use skiff_host_api::{inventory as api, Error};

pub(super) fn opaque_path(value: std::path::PathBuf) -> Result<String, Error> {
    value
        .into_os_string()
        .into_string()
        .map_err(|_| Error::MalformedContract {
            message: "non-UTF-8 path in session inventory".into(),
        })
}

pub fn project_session(value: core::SessionInfo) -> Result<api::InventorySession, Error> {
    let core::SessionInfo {
        id,
        label,
        role,
        cwd,
        command,
        foreground_program,
        args,
        state,
        cols,
        rows,
        pid,
        exit_code,
        theme,
        name,
        title,
        started_at,
        last_output_at,
        output_bytes,
        away,
        was,
        resume,
        unread,
        agent_exit,
        peek,
        interactive,
        input_gen,
        coordinator,
        skipper,
        retiring,
        watch,
    } = value;
    Ok(api::InventorySession {
        id,
        label,
        role: project_role(role),
        cwd: opaque_path(cwd)?,
        command,
        foreground_program,
        args,
        state: project_sessionstate(state),
        cols,
        rows,
        pid,
        exit_code,
        theme,
        name,
        title,
        started_at,
        last_output_at,
        output_bytes,
        away: away
            .into_iter()
            .map(project_away)
            .collect::<Result<_, _>>()?,
        was: was.map(project_was),
        resume: resume.map(project_was),
        unread,
        agent_exit,
        peek,
        interactive,
        input_gen,
        coordinator,
        project_skipper: skipper,
        retiring,
        watch: watch.map(project_watch),
    })
}

pub(super) fn project_role(value: core::Role) -> api::Role {
    match value {
        core::Role::Agent => api::Role::Agent,
        core::Role::Task => api::Role::Task,
        core::Role::Server => api::Role::Server,
        core::Role::Shell => api::Role::Shell,
    }
}

pub(super) fn project_sessionstate(value: core::SessionState) -> api::SessionState {
    match value {
        core::SessionState::Working => api::SessionState::Working,
        core::SessionState::Waiting => api::SessionState::Waiting,
        core::SessionState::Idle => api::SessionState::Idle,
        core::SessionState::Done => api::SessionState::Done,
    }
}

fn project_watch(value: core::WatchInfo) -> api::WatchInfo {
    let core::WatchInfo {
        since,
        pids,
        failure,
        follows,
    } = value;
    api::WatchInfo {
        since,
        pids,
        failure: failure.map(project_failure),
        follows,
    }
}

fn project_failure(value: core::WatchFailure) -> api::WatchFailure {
    let core::WatchFailure { kind, message, at } = value;
    api::WatchFailure { kind, message, at }
}

fn project_was(value: core::Was) -> api::Was {
    let core::Was { agent, title } = value;
    api::Was { agent, title }
}

pub(super) fn project_away(value: core::Away) -> Result<api::Away, Error> {
    let core::Away { path, command, at } = value;
    Ok(api::Away {
        path: opaque_path(path)?,
        command,
        at,
    })
}

pub fn restore_session(value: api::InventorySession) -> core::SessionInfo {
    let api::InventorySession {
        id,
        label,
        role,
        cwd,
        command,
        foreground_program,
        args,
        state,
        cols,
        rows,
        pid,
        exit_code,
        theme,
        name,
        title,
        started_at,
        last_output_at,
        output_bytes,
        away,
        was,
        resume,
        unread,
        agent_exit,
        peek,
        interactive,
        input_gen,
        coordinator,
        project_skipper: skipper,
        retiring,
        watch,
    } = value;
    core::SessionInfo {
        id,
        label,
        role: restore_role(role),
        cwd: cwd.into(),
        command,
        foreground_program,
        args,
        state: restore_sessionstate(state),
        cols,
        rows,
        pid,
        exit_code,
        theme,
        name,
        title,
        started_at,
        last_output_at,
        output_bytes,
        away: away.into_iter().map(restore_away).collect(),
        was: was.map(restore_was),
        resume: resume.map(restore_was),
        unread,
        agent_exit,
        peek,
        interactive,
        input_gen,
        coordinator,
        skipper,
        retiring,
        watch: watch.map(restore_watch),
    }
}

pub(super) fn restore_role(value: api::Role) -> core::Role {
    match value {
        api::Role::Agent => core::Role::Agent,
        api::Role::Task => core::Role::Task,
        api::Role::Server => core::Role::Server,
        api::Role::Shell => core::Role::Shell,
    }
}

pub(super) fn restore_sessionstate(value: api::SessionState) -> core::SessionState {
    match value {
        api::SessionState::Working => core::SessionState::Working,
        api::SessionState::Waiting => core::SessionState::Waiting,
        api::SessionState::Idle => core::SessionState::Idle,
        api::SessionState::Done => core::SessionState::Done,
    }
}

fn restore_watch(value: api::WatchInfo) -> core::WatchInfo {
    let api::WatchInfo {
        since,
        pids,
        failure,
        follows,
    } = value;
    core::WatchInfo {
        since,
        pids,
        failure: failure.map(restore_failure),
        follows,
    }
}

fn restore_failure(value: api::WatchFailure) -> core::WatchFailure {
    let api::WatchFailure { kind, message, at } = value;
    core::WatchFailure { kind, message, at }
}

fn restore_was(value: api::Was) -> core::Was {
    let api::Was { agent, title } = value;
    core::Was { agent, title }
}

pub(super) fn restore_away(value: api::Away) -> core::Away {
    let api::Away { path, command, at } = value;
    core::Away {
        path: path.into(),
        command,
        at,
    }
}
