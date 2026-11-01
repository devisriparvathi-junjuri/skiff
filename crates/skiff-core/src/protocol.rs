//! Newline-delimited JSON over a Unix socket.
//!
//! Client → daemon: one [`Envelope`] per line.
//! Daemon → client: one [`ServerMessage`] per line, either a [`Reply`] that
//! carries the request `id`, or an [`Event`] with no `id`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::group::Group;
use crate::project::AgentInfo;
use crate::session::{Away, SessionId, SessionInfo, SessionSpec, SessionState, WatchFailure};

/// Base64 for byte payloads so the wire stays valid UTF-8 JSON.
pub mod b64 {
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(v: &[u8], s: S) -> Result<S::Ok, S::Error> {
        STANDARD.encode(v).serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let s = String::deserialize(d)?;
        STANDARD.decode(s).map_err(serde::de::Error::custom)
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Request {
    Ping,
    ListSessions,
    /// Small process data for Settings › skiffd activity.
    DaemonMetrics,
    CreateSession {
        spec: SessionSpec,
    },
    Write {
        session: SessionId,
        #[serde(with = "b64")]
        data: Vec<u8>,
        /// The data is a paste. The daemon wraps it in bracketed paste when
        /// the program turned that on, so the program does not take its
        /// newlines, or a fast Enter after it, as part of the text.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        paste: bool,
    },
    /// Types `data` and, with `enter`, presses Enter after
    /// [`ENTER_DELAY`], as one step: only if the session took no input since
    /// its `input_gen` was `expect`. For text a program sends on its own,
    /// so it never mixes with what the person types. Answers
    /// [`Response::Ok`] when sent, [`Response::Refused`] when not.
    ///
    /// [`ENTER_DELAY`]: crate::session::ENTER_DELAY
    Submit {
        session: SessionId,
        #[serde(with = "b64")]
        data: Vec<u8>,
        /// As in [`Request::Write`].
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        paste: bool,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        enter: bool,
        expect: u64,
    },
    Resize {
        session: SessionId,
        cols: u16,
        rows: u16,
    },
    /// Start receiving output for this session on this connection. The daemon
    /// sends one [`Event::Snapshot`] first, then [`Event::Output`] from there on.
    /// The reply can arrive before or after the snapshot.
    Subscribe {
        session: SessionId,
    },
    Unsubscribe {
        session: SessionId,
    },
    /// Sets the user's name for a session. Empty clears it.
    RenameSession {
        session: SessionId,
        name: String,
    },
    /// Sets one terminal's theme. `null` returns it to the project's.
    SetSessionTheme {
        session: SessionId,
        #[serde(default)]
        theme: Option<String>,
    },
    /// The session's pane has the keys: the user has seen it. Clears
    /// `unread` and `agent_exit`.
    Seen {
        session: SessionId,
    },
    /// The last `rows` rows of the session's text above the agent's input
    /// box, for mission control. Answers [`Response::Screen`].
    Screen {
        session: SessionId,
        rows: u16,
    },
    /// Terminate the process and drop the session.
    Kill {
        session: SessionId,
    },
    /// The known agent CLIs, installed and enabled.
    ListAgents,
    /// Runs an agent's own session list command in a folder, for the
    /// resume list. Answers with what it printed.
    ListAgentSessions {
        command: Vec<String>,
        cwd: PathBuf,
    },
    ListGroups,
    /// Empty `group.id` creates a group. Otherwise replaces that group.
    SaveGroup {
        group: Group,
    },
    /// `group` is the group id.
    DeleteGroup {
        group: String,
    },
    /// Puts the groups in this order of ids. Groups left out keep their
    /// order after the named ones; unknown ids are skipped.
    OrderGroups {
        groups: Vec<String>,
    },
    /// Moves skiffd onto another binary in place. The pid, sessions, PTYs and
    /// terminals stay. `binary` is an absolute path; `None` is the daemon's
    /// own executable as it is on disk now. A failure answers with an error
    /// from the old daemon, which carries on. Success ends the connection
    /// with no reply; only a new connection can confirm it (same `pid` in
    /// [`Response::Pong`], same session ids and pids).
    ///
    /// Keep this request's JSON form, `{"cmd":"reload","binary":"/path"}`,
    /// across protocol changes: it is how an app moves an older daemon on.
    Reload {
        #[serde(default)]
        binary: Option<PathBuf>,
    },
    /// Stops skiffd as SIGTERM from outside does: it saves the layout and
    /// ends every session. Answers `ok` first. A pane uses this for a
    /// deliberate stop: skiffd ignores signals from its own sessions.
    Shutdown,
    /// Finds the live Skiff agent session (the coordinator), or starts it
    /// from `spec`, as one guarded step: two quick calls get the same
    /// session. With no `spec`, it only finds. A coordinator that is a bare
    /// shell (restored, or its agent quit) is not live: the daemon types
    /// `resume` into it when the shell was restored from an agent, else
    /// `start`, and presses Enter. `expect` is the `input_gen` the client
    /// read; input after it refuses the line. Answers
    /// [`Response::Coordinator`], or [`Response::Sessions`] with none found.
    Coordinator {
        #[serde(default)]
        spec: Option<SessionSpec>,
        #[serde(default)]
        start: Option<String>,
        #[serde(default)]
        resume: Option<String>,
        #[serde(default)]
        expect: Option<u64>,
    },
    /// Takes a project Skipper over: clears its Skipper mark and sets it
    /// retiring, so skiffd ends it at its next idle. An idle one ends now.
    /// Answers [`Response::Ok`].
    Retire {
        session: SessionId,
    },
    /// Gives a fleet Skipper its fleet's new name: its Skipper mark becomes
    /// `scope` and its name `name`, in one step. Answers
    /// [`Response::Session`].
    Rescope {
        session: SessionId,
        scope: String,
        name: String,
    },
    /// This connection watches `session` for the Skiff agent. The
    /// registration lasts until the connection closes. A second call on the
    /// same connection replaces it: the watcher sends it again to report or
    /// clear a failure, or when the panes it follows change. Answers `Ok`, or
    /// `Error` when the session does not exist.
    Watching {
        session: SessionId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        failure: Option<WatchFailure>,
        /// The live panes with an open ledger task that the watcher follows.
        /// `None` before its first ledger read, and from an older watcher.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        follows: Option<Vec<SessionId>>,
    },
}

/// Who answers a `ping`, from the daemon itself: never from a socket's peer
/// credentials or pid, which name `ssh` on a forwarded socket.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    /// The Host's durable id. See [`crate::host_identity`].
    pub host_id: String,
    /// New on every start and every reload, so a client knows that its
    /// subscriptions and snapshots are from another run.
    pub instance_id: String,
    pub os: String,
    pub account: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Envelope {
    pub id: u64,
    #[serde(flatten)]
    pub request: Request,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    Ok,
    Pong {
        version: String,
        /// 0 from daemons that predate the field.
        #[serde(default)]
        protocol: u32,
        /// The handover version the daemon writes on reload. `None`: it
        /// cannot reload.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reload_state: Option<u32>,
        /// The daemon's process id, which a reload keeps.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pid: Option<u32>,
        /// The daemon is a debug build. `None` from daemons that predate the field.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        debug: Option<bool>,
        /// The Host this daemon serves, and which run of it answers. `None`
        /// from daemons that predate the field, or that cannot read the
        /// Host's id.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        identity: Option<Identity>,
    },
    Sessions {
        sessions: Vec<SessionInfo>,
    },
    DaemonMetrics {
        metrics: DaemonMetrics,
    },
    Session {
        session: SessionInfo,
    },
    Agents {
        agents: Vec<AgentInfo>,
    },
    Output {
        stdout: String,
    },
    Screen {
        lines: Vec<String>,
    },
    Groups {
        groups: Vec<Group>,
    },
    Group {
        group: Group,
    },
    Error {
        message: String,
    },
    /// The answer to [`Request::Coordinator`]. `created`: it started now.
    /// `typed`: a start or resume line went into its shell now.
    Coordinator {
        session: SessionInfo,
        created: bool,
        typed: bool,
    },
    /// A [`Request::Submit`] typed nothing: input came after its check.
    Refused,
}

/// One daemon process, without child-process measurements.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct DaemonMetrics {
    pub version: String,
    pub pid: u32,
    pub started_at: u64,
    pub memory_bytes: u64,
    pub session_count: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Reply {
    pub id: u64,
    #[serde(flatten)]
    pub response: Response,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    Output {
        session: SessionId,
        #[serde(with = "b64")]
        data: Vec<u8>,
        /// Set on the first output read after input, for the latency trace.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        echo: Option<EchoTimes>,
    },
    /// Bytes that redraw the current screen and scrollback. Starts with a
    /// full reset. Sized for the session's current `cols` x `rows`.
    Snapshot {
        session: SessionId,
        #[serde(with = "b64")]
        data: Vec<u8>,
        cols: u16,
        rows: u16,
    },
    State {
        session: SessionId,
        state: SessionState,
    },
    /// The worktrees other than its own where the session works changed.
    Away {
        session: SessionId,
        away: Vec<Away>,
    },
    /// The program set or cleared its terminal title.
    Title {
        session: SessionId,
        title: Option<String>,
    },
    Exit {
        session: SessionId,
        code: Option<i32>,
    },
    SessionCreated {
        session: SessionInfo,
    },
    /// An agent finished a turn, or a program ended and left the shell in
    /// front: there is a result to read. Sent for each one, though `unread`
    /// is set already.
    TurnEnded {
        session: SessionId,
    },
    /// A session's info changed outside the state and title events.
    SessionUpdated {
        session: SessionInfo,
    },
    SessionRemoved {
        session: SessionId,
    },
    /// A group was saved, deleted, or lost a killed session. Clients reload groups.
    GroupsChanged {},
}

/// When skiffd wrote input to the PTY, and when it read the first output
/// after that write. Unix time in microseconds.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub struct EchoTimes {
    pub wrote: u64,
    pub read: u64,
}

impl Request {
    /// The `cmd` name, for logs.
    pub fn name(&self) -> &'static str {
        match self {
            Request::Ping => "ping",
            Request::ListSessions => "list_sessions",
            Request::DaemonMetrics => "daemon_metrics",
            Request::CreateSession { .. } => "create_session",
            Request::Write { .. } => "write",
            Request::Submit { .. } => "submit",
            Request::Resize { .. } => "resize",
            Request::Subscribe { .. } => "subscribe",
            Request::Unsubscribe { .. } => "unsubscribe",
            Request::RenameSession { .. } => "rename_session",
            Request::SetSessionTheme { .. } => "set_session_theme",
            Request::Seen { .. } => "seen",
            Request::Screen { .. } => "screen",
            Request::Kill { .. } => "kill",
            Request::ListAgents => "list_agents",
            Request::ListAgentSessions { .. } => "list_agent_sessions",
            Request::ListGroups => "list_groups",
            Request::SaveGroup { .. } => "save_group",
            Request::DeleteGroup { .. } => "delete_group",
            Request::OrderGroups { .. } => "order_groups",
            Request::Reload { .. } => "reload",
            Request::Shutdown => "shutdown",
            Request::Coordinator { .. } => "coordinator",
            Request::Watching { .. } => "watching",
            Request::Retire { .. } => "retire",
            Request::Rescope { .. } => "rescope",
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(untagged)]
pub enum ServerMessage {
    Reply(Reply),
    Event(Event),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pong_identity_is_optional() {
        let old = r#"{"id":1,"type":"pong","version":"0.6.24","protocol":27,"pid":7}"#;
        let ServerMessage::Reply(Reply { response: Response::Pong { identity, .. }, .. }) = serde_json::from_str(old).unwrap() else { panic!() };
        assert_eq!(identity, None);
        let id = Identity { host_id: "h".into(), instance_id: "i".into(), os: "macos".into(), account: "uid:501".into() };
        let pong = Response::Pong { version: "v".into(), protocol: 27, reload_state: None, pid: None, debug: None, identity: Some(id.clone()) };
        let json = serde_json::to_value(&pong).unwrap();
        assert_eq!(json["identity"], serde_json::json!({"host_id":"h","instance_id":"i","os":"macos","account":"uid:501"}));
        let Response::Pong { identity, .. } = serde_json::from_value(json).unwrap() else { panic!() };
        assert_eq!(identity, Some(id));
    }

    #[test]
    fn session_info_watch_is_optional() {
        let mut v = serde_json::json!({
            "id": "s1", "label": "claude", "role": "agent", "cwd": "/", "command": "claude", "args": [],
            "state": "idle", "cols": 80, "rows": 24, "pid": null, "exit_code": null,
        });
        let s: SessionInfo = serde_json::from_value(v.clone()).unwrap();
        assert_eq!(s.watch, None);
        assert!(!serde_json::to_string(&s).unwrap().contains("watch"));
        v["watch"] = serde_json::json!({ "since": 1, "pids": [41234], "failure": { "kind": "ledger", "message": "busy", "at": 2 } });
        let s: SessionInfo = serde_json::from_value(v).unwrap();
        let back: SessionInfo = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(
            back.watch.as_ref().map(|w| w.pids.clone()),
            Some(vec![41234])
        );
        assert_eq!(back, s);
    }
}
