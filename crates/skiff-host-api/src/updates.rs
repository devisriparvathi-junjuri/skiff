//! M1d read-only session updates: every existing non-output skiffd event,
//! delivered as `Event::SessionUpdate` to connections that negotiated
//! `session_updates@1`. No request subscribes, replays or acknowledges them.
use serde::{Deserialize, Serialize};

use crate::inventory::{Away, InventorySession, SessionState};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SessionUpdate {
    State {
        session: String,
        state: SessionState,
    },
    Away {
        session: String,
        away: Vec<Away>,
    },
    Title {
        session: String,
        title: Option<String>,
    },
    Exit {
        session: String,
        code: Option<i32>,
    },
    SessionCreated {
        session: InventorySession,
    },
    TurnEnded {
        session: String,
    },
    SessionUpdated {
        session: InventorySession,
    },
    SessionRemoved {
        session: String,
    },
    GroupsChanged,
}
