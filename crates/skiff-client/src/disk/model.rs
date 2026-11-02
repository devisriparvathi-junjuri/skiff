//! The tree as the page reads it in JSON, the policy constants, and the words of a verdict.

use super::checkout::Checkout;

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// A work folder stays in use this long after its last change, in seconds.
pub const HOLD_WORK: i64 = 15 * 60;
/// A file outside the work folders is held this long after its last change.
pub const HOLD_LOOSE: i64 = 24 * 3600;
/// A folder of more files than this, all with one verdict, is sent as one
/// row: a build folder needs no row for each of its files.
pub(super) const FOLD_FILES: u64 = 50;

/// Files a flush never removes, at any depth.
pub(super) const KEEP: &[&str] = &[
    "ledger.db",
    "ledger.db-wal",
    "ledger.db-shm",
    "ledger.db.old",
    "ledger.md.bak",
    "SKIPPER.md",
    "AGENTS.md",
    "CLAUDE.md",
    "GEMINI.md",
    ".skiff-agent",
    ".agents",
    ".claude",
    "avatar.toml",
];

/// What the rule says of a file.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Hash)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// A live signal: work not done, a working pane, a recent change.
    Use,
    /// Outside the work folders, and changed in the last day.
    Held,
    /// Never removed while the reason holds.
    Prot,
    /// A flush removes it.
    Stale,
}

/// The rule's answer for a file, and why. The page words it.
#[derive(Serialize, Clone, PartialEq, Eq, Debug)]
pub struct Verdict {
    pub kind: Kind,
    /// The reason: `done`, `no-folder`, `image`, `clean-checkout` (stale);
    /// `working`, `open`, `plan-open`, `pane-working`, `pane`, `changed`
    /// (in use); `changed` (held); `skiff-file`, `pinned`, `unreadable`,
    /// `dirty`, `ahead`, `no-upstream`, `checkout`, `no-daemon`, `no-ledger`
    /// (protected).
    pub why: &'static str,
    /// The work's title, a pane's name, or empty.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub label: String,
    /// The last change that holds it, in Unix seconds: for `changed`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub changed: Option<i64>,
}

impl Verdict {
    pub(super) fn new(kind: Kind, why: &'static str) -> Self {
        Verdict { kind, why, label: String::new(), changed: None }
    }

    pub(super) fn label(mut self, l: impl Into<String>) -> Self {
        self.label = l.into();
        self
    }

    pub fn stale(&self) -> bool {
        self.kind == Kind::Stale
    }
}

/// A work folder's row, for the page.
#[derive(Serialize, Clone, Debug)]
pub struct Work {
    pub uid: String,
    pub kind: String,
    pub title: String,
}

/// A file or folder of the tree.
#[derive(Serialize, Clone, Debug)]
pub struct Node {
    /// The name; a folder's ends with `/`.
    pub name: String,
    /// From the agent folder, with `/`. Empty for the folder itself.
    pub path: String,
    pub bytes: u64,
    pub files: u64,
    /// The newest change of a file in it, in Unix seconds; a folder with no
    /// file has its own.
    pub mtime: i64,
    /// A file's verdict, or a folded folder's (see [`FOLD_FILES`]).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<Verdict>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<Node>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub dir: bool,
    /// One of Skiff's own folders: never removed, even when empty.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub frame: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub work: Option<Work>,
    /// A git checkout: a linked worktree goes with `git worktree remove`.
    #[serde(skip)]
    pub(super) checkout: Option<Checkout>,
    #[serde(skip)]
    pub(super) abs: PathBuf,
}

/// What the page shows.
#[derive(Serialize, Debug)]
pub struct Usage {
    pub folder: PathBuf,
    pub tree: Node,
    /// When it was counted, in Unix seconds.
    pub now: i64,
    /// The daemon answered: the panes were checked.
    pub daemon: bool,
    pub ledger_error: Option<String>,
    /// Entries the walk could not read.
    pub unreadable: u64,
    pub hold_work: i64,
    pub hold_loose: i64,
}

/// One item the page asks a flush to remove, as the count showed it.
#[derive(Deserialize, Serialize, Clone, Debug, PartialEq)]
pub struct Expected {
    /// From the agent folder, with `/`.
    pub path: String,
    pub bytes: u64,
    pub files: u64,
    pub mtime: i64,
}

/// What a flush removed and kept.
#[derive(Serialize, Default, Debug, PartialEq)]
pub struct Flushed {
    pub files: u64,
    pub bytes: u64,
    /// Items or files that changed since the count, or are no longer stale.
    pub kept: u64,
    /// The rows whose work folder went: they now show "Result flushed".
    pub rows: u64,
    /// One line for each item that could not go, such as a refused
    /// `git worktree remove`.
    pub errors: Vec<String>,
}


/// A verdict in words, as the page writes it.
pub fn words(v: &Verdict) -> String {
    let what = match v.why {
        "working" => "the task is working",
        "open" => "the task is open",
        "plan-open" => "the plan has a task that is not done",
        "pane-working" => "its pane is working",
        "pane" => "a live pane works in it",
        "changed" => "it changed in the last 15 minutes",
        "skiff-file" => "it is a Skiff file",
        "pinned" => "it is a pinned image",
        "unreadable" => "Skiff cannot read it",
        "dirty" => "a git checkout in it has changes",
        "ahead" => "a git checkout in it has commits that are not pushed",
        "no-upstream" => "a git checkout in it has no upstream",
        "checkout" => "git cannot read a checkout in it",
        "no-daemon" => "skiffd did not answer",
        "no-ledger" => "Skiff cannot read the ledger",
        _ => "it is not stale",
    };
    what.to_string()
}
