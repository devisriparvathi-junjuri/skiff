//! M4 data plane: the Host's projects, fleets and worktrees (`projects@1`),
//! a worktree's changed files (`git_status@1`) and their diff text
//! (`git_diff@1`); changes to the Host's `[[project]]` tables
//! (`project_config@1`) and to its project worktrees (`worktrees@1`).
//! Served by `skiff-host`, the Host's restartable data-plane service, never
//! by skiffd. Every path is a path on the Host: opaque to the client, never
//! opened, resolved or checked on the client's machine.
use serde::{Deserialize, Serialize};

/// One git worktree of a project, as the Host's git reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostWorktree {
    pub path: String,
    /// Short branch name. `None` when detached.
    pub branch: Option<String>,
    /// Commit sha.
    pub head: String,
    pub is_main: bool,
    pub locked: bool,
    pub prunable: bool,
}

/// A project's branches, for New worktree (`branch_list`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostBranches {
    /// The branch the project's folder has checked out: a new branch
    /// starts here when `worktree_add` names no base. `None` when detached.
    pub head: Option<String>,
    /// Newest commit first, at most 1000.
    pub branches: Vec<HostBranch>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostBranch {
    /// Short: `main`, or `origin/main` for a remote's.
    pub name: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub remote: bool,
    /// A worktree has it checked out. Never set on a remote's.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub worktree: bool,
}

/// One project of the Host's `projects.toml`, joined with its worktrees.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostProject {
    pub name: String,
    pub short: String,
    pub path: String,
    pub color: Option<String>,
    pub agents: Vec<String>,
    /// The rail image as a data URL: bytes, never a path.
    #[serde(default)]
    pub icon: Option<String>,
    /// The background image's path on the Host. Only that Host can read it;
    /// its bytes are a files capability, not this one.
    #[serde(default)]
    pub background: Option<String>,
    #[serde(default)]
    pub background_opacity: Option<u8>,
    pub worktrees: Vec<HostWorktree>,
    /// `folder`: a plain folder in no git repository, with the folder as its
    /// only worktree and no changes. Absent from an older Host: `git`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default)]
    pub default_branch: Option<String>,
    /// Why `worktrees` is empty, e.g. the path is missing on the Host.
    pub error: Option<String>,
    /// Out of the rail. Its worktrees are not read.
    #[serde(default)]
    pub closed: bool,
    #[serde(default)]
    pub fleet: Option<String>,
    /// `changes = false` in the Host's config: its Changes section is hidden.
    /// Absent: shown.
    #[serde(default)]
    pub changes: Option<bool>,
    /// The project's own theme id. Absent: the app theme. Left out when
    /// absent, so a row without it reads as before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme: Option<String>,
    /// Shows a Files tree under each worktree. Left out when off.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub files: bool,
}

/// What the Host would add for a folder of its own, before anything is written.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostFolder {
    /// The repository's main worktree on the Host. `None`: not in a repository.
    pub root: Option<String>,
    pub name: String,
    pub short: String,
    pub color: String,
    pub branch: Option<String>,
    pub worktrees: usize,
    /// The icon the Host found in the repository, as a data URL: bytes, never a path.
    #[serde(default)]
    pub icon: Option<String>,
    /// Why the folder cannot be added.
    pub error: Option<String>,
}

/// One change to one project's settings. Images (icon, background) are not
/// here: they need their bytes sent, which no capability offers yet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "key", rename_all = "snake_case")]
pub enum ProjectEdit {
    /// Rail initials, at most 3 characters. Empty: from the name.
    Short { short: String },
    /// Empty: on the rail by itself.
    Fleet { fleet: String },
    /// `#rrggbb`.
    Color { color: String },
    /// `None`: the app theme.
    Theme { theme: Option<String> },
    Changes { on: bool },
    Files { on: bool },
    /// Percent, 40 to 100. `None`: the default.
    BackgroundOpacity { percent: Option<u8> },
    /// Out of the rail, settings kept; or back on it.
    Closed { closed: bool },
}

/// A project's fleet in an arrangement. Empty: on the rail by itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectFleet {
    pub project: String,
    pub fleet: String,
}

/// One changed file in a worktree against its base: on a branch, the fork
/// point from the project's default branch; else HEAD. Staged and unstaged
/// edits count together.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostChange {
    /// Relative to the worktree root.
    pub path: String,
    /// `M` modified, `A` added or untracked, `D` deleted, `U` unmerged.
    pub status: ChangeStatus,
    /// Lines added and removed. `None` for a binary file.
    pub added: Option<u32>,
    pub removed: Option<u32>,
    /// The file is the same in the working tree and HEAD: the change sits
    /// in the branch's commits. False for every uncommitted edit, and from
    /// a Host that does not say.
    #[serde(default)]
    pub committed: bool,
}

/// A worktree's changed files, and the branch they count from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostChanges {
    /// The project's default branch, when the worktree is on another branch
    /// and its files count from where it left that one. Absent: against HEAD,
    /// as on the default branch, or from a Host that does not say.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    /// Sorted by path.
    pub changes: Vec<HostChange>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChangeStatus {
    M,
    A,
    D,
    U,
}

/// A diff as plain text, for a viewer on the client. Never colored, never
/// from the Host's external diff tool or textconv filter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostDiff {
    /// Unified diff text. Empty: the file has no change.
    pub text: String,
    /// The diff was longer than the Host's limit (512 KiB): `text` is its
    /// start, cut at a line end.
    pub truncated: bool,
    /// The file is binary: `text` holds git's one-line note only.
    pub binary: bool,
    /// The file is untracked: it is compared with an empty file.
    pub untracked: bool,
}
