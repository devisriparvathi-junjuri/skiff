//! M1d session-update boundary. Exhaustive matches make a new core event
//! variant a compile error until it is deliberately projected.
use skiff_core::protocol as core;
use skiff_host_api::{Error, SessionUpdate};

use super::inventory::{
    project_away, project_session, project_sessionstate, restore_away, restore_session,
    restore_sessionstate,
};

pub(super) fn project_update(value: core::Event) -> Result<SessionUpdate, Error> {
    Ok(match value {
        core::Event::State { session, state } => SessionUpdate::State {
            session,
            state: project_sessionstate(state),
        },
        core::Event::Away { session, away } => SessionUpdate::Away {
            session,
            away: away.into_iter().map(project_away).collect::<Result<_, _>>()?,
        },
        core::Event::Title { session, title } => SessionUpdate::Title { session, title },
        core::Event::Exit { session, code } => SessionUpdate::Exit { session, code },
        core::Event::SessionCreated { session } => SessionUpdate::SessionCreated {
            session: project_session(session)?,
        },
        core::Event::TurnEnded { session } => SessionUpdate::TurnEnded { session },
        core::Event::SessionUpdated { session } => SessionUpdate::SessionUpdated {
            session: project_session(session)?,
        },
        core::Event::SessionRemoved { session } => SessionUpdate::SessionRemoved { session },
        core::Event::GroupsChanged {} => SessionUpdate::GroupsChanged,
        // The legacy Client routes these to per-session output receivers.
        core::Event::Output { .. } | core::Event::Snapshot { .. } => {
            return Err(Error::MalformedContract {
                message: "terminal output is not a session update".into(),
            })
        }
    })
}

pub(super) fn restore_update(value: SessionUpdate) -> core::Event {
    match value {
        SessionUpdate::State { session, state } => core::Event::State {
            session,
            state: restore_sessionstate(state),
        },
        SessionUpdate::Away { session, away } => core::Event::Away {
            session,
            away: away.into_iter().map(restore_away).collect(),
        },
        SessionUpdate::Title { session, title } => core::Event::Title { session, title },
        SessionUpdate::Exit { session, code } => core::Event::Exit { session, code },
        SessionUpdate::SessionCreated { session } => core::Event::SessionCreated {
            session: restore_session(session),
        },
        SessionUpdate::TurnEnded { session } => core::Event::TurnEnded { session },
        SessionUpdate::SessionUpdated { session } => core::Event::SessionUpdated {
            session: restore_session(session),
        },
        SessionUpdate::SessionRemoved { session } => core::Event::SessionRemoved { session },
        SessionUpdate::GroupsChanged => core::Event::GroupsChanged {},
    }
}
