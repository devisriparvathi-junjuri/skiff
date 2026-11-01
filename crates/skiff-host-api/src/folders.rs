//! M5 data plane: the Host account's folders, for the folder picker
//! (`folders@1`). Served by `skiff-host`. It lists folders only: no file
//! name, size or content. Every path is a Host path, opaque to the client:
//! it goes only into another Host request, never to an opener or a
//! filesystem call on the client's machine.
use serde::{Deserialize, Serialize};

/// A folder entry as it is on the Host's disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FolderKind {
    Dir,
    /// A link to a folder. Opening it lists its real path.
    Link,
}

/// A git repository's state, read from its files: no git runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FolderRepo {
    /// Short branch name. `None` when detached.
    pub branch: Option<String>,
    /// Unix seconds when the branch last moved. `None` when unknown.
    pub last_commit: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FolderEntry {
    pub name: String,
    pub kind: FolderKind,
    /// Set when the entry is a git repository's top folder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<FolderRepo>,
}

/// One folder, by name, folders and links to folders only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FolderListing {
    /// The path as the request named it.
    pub path: String,
    /// The same folder with every link resolved. A pick is always this one.
    pub real: String,
    /// The folder above `real`. `None` at the root.
    pub parent: Option<String>,
    pub entries: Vec<FolderEntry>,
    /// More than 2000 entries: the rest are left out.
    pub truncated: bool,
}

/// A git repository the Host found under its account's home.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaceRepo {
    pub path: String,
    pub branch: Option<String>,
    pub last_commit: Option<u64>,
}

/// The picker's Places.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FolderPlaces {
    /// Folders the Host saw in use, newest first, at most 20.
    pub recent: Vec<String>,
    /// From the last finished scan, by path.
    pub repos: Vec<PlaceRepo>,
    /// A scan runs now: `repos` may change. Ask again.
    pub scanning: bool,
}
