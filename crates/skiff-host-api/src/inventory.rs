//! M1c read-only desktop inventory projection, not a session lifecycle model.
//! Paths are opaque UTF-8 host strings, not local filesystem handles. Defaults
//! and omission rules intentionally match the existing desktop JSON output.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Copy)]
#[serde(rename_all = "lowercase")]
pub enum SessionState {
    Working,
    Waiting,
    Idle,
    Done,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Copy, Default)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Agent,
    Task,
    Server,
    #[default]
    Shell,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InventorySession {
    pub id: String,
    pub label: String,
    pub role: Role,
    pub cwd: String,
    pub command: String,
    #[serde(default)]
    pub foreground_program: Option<String>,
    pub args: Vec<String>,
    pub state: SessionState,
    pub cols: u16,
    pub rows: u16,
    pub pid: Option<u32>,
    pub exit_code: Option<i32>,
    #[serde(default)]
    pub theme: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub started_at: u64,
    #[serde(default)]
    pub last_output_at: u64,
    #[serde(default)]
    pub output_bytes: u64,
    #[serde(default)]
    pub away: Vec<Away>,
    #[serde(default)]
    pub was: Option<Was>,
    #[serde(default)]
    pub resume: Option<Was>,
    #[serde(default)]
    pub unread: bool,
    #[serde(default)]
    pub agent_exit: Option<i32>,
    #[serde(default)]
    pub peek: bool,
    #[serde(default)]
    pub interactive: bool,
    #[serde(default)]
    pub input_gen: u64,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub coordinator: bool,
    /// A project or fleet Skipper: its scope, a project's name or
    /// `fleet:<fleet>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_skipper: Option<String>,
    /// A project Skipper a fleet Skipper took over. It is a plain session
    /// until the Host's skiffd ends it at its next idle.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub retiring: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub watch: Option<WatchInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WatchInfo {
    pub since: u64,
    pub pids: Vec<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<WatchFailure>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub follows: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WatchFailure {
    pub kind: String,
    pub message: String,
    pub at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Was {
    pub agent: String,
    #[serde(default)]
    pub title: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Away {
    pub path: String,
    pub command: String,
    pub at: u64,
}
