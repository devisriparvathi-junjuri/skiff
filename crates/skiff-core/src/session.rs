use std::{path::PathBuf, time::Duration};

/// The pause before Enter. A TUI without bracketed paste guesses pastes
/// from fast input, and takes an Enter that comes too soon as a newline.
pub const ENTER_DELAY: Duration = Duration::from_millis(200);

use serde::{Deserialize, Serialize};

pub type SessionId = String;

/// Derived from the PTY stream and the foreground process only. Skiff
/// installs nothing into agent configs.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SessionState {
    /// New text arrived recently. A shell at its prompt is never working.
    Working,
    /// An agent shows its approval prompt, or another program rang the bell
    /// and has had no input since.
    Waiting,
    /// No new text for a while.
    Idle,
    /// The process exited.
    Done,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Agent,
    Task,
    Server,
    #[default]
    Shell,
}

/// What a client asks the daemon to start.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct SessionSpec {
    /// Display name. Empty means: use the command's file name.
    #[serde(default)]
    pub label: String,
    /// Initial session name. A user can replace this name later.
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub role: Role,
    /// Working directory. `None` means the daemon's home directory.
    #[serde(default)]
    pub cwd: Option<PathBuf>,
    /// Program to run. `None` or empty means the login shell.
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    /// Variables the program gets on top of the daemon's own, as a task
    /// type's instruction file for opencode. Not saved: a restore starts a
    /// shell. An older daemon drops them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env: Vec<(String, String)>,
    /// A tool the app runs in its peek: not saved, not restored, not listed
    /// as a session. Runs the command as is, with no shell after it.
    #[serde(default)]
    pub peek: bool,
    /// The Skiff agent's session. Never on the wire: only the daemon's
    /// guarded [`Request::Coordinator`] and a restore set it.
    ///
    /// [`Request::Coordinator`]: crate::protocol::Request::Coordinator
    #[serde(skip)]
    pub coordinator: bool,
    /// A project or fleet Skipper: its scope, a project's name or
    /// `fleet:<fleet>`. The CLI starts it with at most one live per scope.
    /// Not saved: a restore brings the pane back as a plain shell.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skipper: Option<String>,
    #[serde(default = "default_cols")]
    pub cols: u16,
    #[serde(default = "default_rows")]
    pub rows: u16,
}

fn default_cols() -> u16 {
    120
}
fn default_rows() -> u16 {
    40
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct SessionInfo {
    pub id: SessionId,
    pub label: String,
    pub role: Role,
    pub cwd: PathBuf,
    pub command: String,
    /// Program in the PTY's foreground process group, when available.
    #[serde(default)]
    pub foreground_program: Option<String>,
    pub args: Vec<String>,
    pub state: SessionState,
    pub cols: u16,
    pub rows: u16,
    pub pid: Option<u32>,
    pub exit_code: Option<i32>,
    /// This terminal's own theme id; wins over the project's.
    #[serde(default)]
    pub theme: Option<String>,
    /// The name Skiff or the user gave. It wins over `title`.
    #[serde(default)]
    pub name: Option<String>,
    /// The terminal title the program set (OSC 0 or 2), if any.
    #[serde(default)]
    pub title: Option<String>,
    /// Unix ms. Defaulted so an older daemon still deserializes.
    #[serde(default)]
    pub started_at: u64,
    /// Unix ms of the last PTY read.
    #[serde(default)]
    pub last_output_at: u64,
    /// Total bytes read from this session's PTY. A reload keeps the count;
    /// a restored shell starts again at zero.
    #[serde(default)]
    pub output_bytes: u64,
    /// Other worktrees the session's processes ran in during the last minute.
    #[serde(default)]
    pub away: Vec<Away>,
    /// The last agent seen in front, kept across restores so the pane can offer
    /// to resume it.
    #[serde(default)]
    pub was: Option<Was>,
    /// `was` as a restore found it. The pane offers to resume it until an
    /// agent runs there again.
    #[serde(default)]
    pub resume: Option<Was>,
    /// An agent finished its turn, or a command finished in a shell, and no
    /// client has seen the pane since.
    #[serde(default)]
    pub unread: bool,
    /// The exit code of the last agent the launch wrapper ran here, until a
    /// client sees the pane or an agent runs there again.
    #[serde(default)]
    pub agent_exit: Option<i32>,
    /// Started for the app's peek. See [`SessionSpec::peek`].
    #[serde(default)]
    pub peek: bool,
    /// A peek tool took the terminal (raw input or the alternate screen): it
    /// is a terminal tool, and the app shows it.
    #[serde(default)]
    pub interactive: bool,
    /// Counts the input the session took. A [`Request::Submit`] names the
    /// count it read, and the daemon refuses it when input came after.
    ///
    /// [`Request::Submit`]: crate::protocol::Request::Submit
    #[serde(default)]
    pub input_gen: u64,
    /// The Skiff agent's session (Skipper). At most one live session per
    /// daemon has it. A restore keeps it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub coordinator: bool,
    /// A project or fleet Skipper: its scope. See [`SessionSpec::skipper`].
    /// A reload keeps it; a restore does not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skipper: Option<String>,
    /// A Skipper a fleet Skipper took over. Its mark is gone, and skiffd
    /// ends it at its next idle: a shell at once, an agent once its turn
    /// ends. A reload keeps it; a restore does not.
    /// See [`Request::Retire`](crate::protocol::Request::Retire).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub retiring: bool,
    /// The `skiff watch` processes registered for this session. `None` when
    /// none is, or from a daemon older than protocol 27. Not saved, not
    /// restored, not in a reload handover.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub watch: Option<WatchInfo>,
    /// What the program in front last reported about itself with OSC 7501
    /// (Program Status Protocol): its root record. While it is set, it
    /// decides `state`. `None` when the program reports nothing, or from a
    /// daemon older than this field. A reload keeps it; a restore does not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub program_status: Option<ProgramStatus>,
}

/// A program's own report of its state, from OSC 7501. Strings, not enums,
/// so a client reads a value a newer daemon adds.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct ProgramStatus {
    /// "idle", "working", "blocked", "done" or "error". `state` shows
    /// "blocked" as waiting, and "done" and "error" as idle.
    pub state: String,
    /// What a "blocked" program waits for: "permission", "question" or "auth".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// The program's message, decoded, as plain text with no control characters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub msg: Option<String>,
    /// The program's title for the task, decoded and cleaned like `msg`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Percent done, 0 to 100, while working or blocked. `None`: no figure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<u8>,
    /// The program's name for itself, such as "codex" or "claude-code".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
}

/// The watchers registered for a session with [`Request::Watching`].
///
/// [`Request::Watching`]: crate::protocol::Request::Watching
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct WatchInfo {
    /// Unix ms of the oldest live registration.
    pub since: u64,
    /// Watcher pids, from the connection's peer credentials.
    pub pids: Vec<u32>,
    /// The newest failure any registered watcher reports.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<WatchFailure>,
    /// The live panes the watchers follow: those with an open ledger task,
    /// sorted. `None` when no watcher reports it (one older than this field,
    /// or one that has not read the ledger yet): unknown, not nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub follows: Option<Vec<SessionId>>,
}

/// A watcher's last line did not reach the agent, or it cannot read the ledger.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct WatchFailure {
    /// "undelivered": a line was typed and no turn started.
    /// "ledger": the ledger could not be read 3 steps in a row.
    pub kind: String,
    /// The watch log line, for the tooltip.
    pub message: String,
    /// Unix ms.
    pub at: u64,
}

impl SessionInfo {
    /// The pane's process generation: its start time. A reload keeps the
    /// session and its program, so it keeps the generation. A restore or a
    /// new start runs a new program under the old id, so the generation
    /// changes. `None` from a daemon too old to send a start time.
    pub fn generation(&self) -> Option<u64> {
        (self.started_at != 0).then_some(self.started_at)
    }
}

/// An agent that ran in a session, and the last title it set.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Was {
    /// The agent kind, as the foreground watcher names it: "claude", "codex"...
    pub agent: String,
    #[serde(default)]
    pub title: Option<String>,
}

/// A worktree other than the session's own, where one of its processes ran.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Away {
    /// The worktree root: the deepest folder with a `.git` entry.
    pub path: PathBuf,
    /// The name of the last process seen there.
    pub command: String,
    /// Unix ms of the last time a process was seen there.
    pub at: u64,
}

/// Milliseconds since the Unix epoch.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
