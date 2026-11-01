//! M1g session lifecycle: what a new session starts with. Defaults and omission
//! rules match the existing desktop/skiffd `SessionSpec` JSON. Paths are opaque
//! UTF-8 host strings; there is no coordinator flag (only the daemon's guarded
//! coordinator request sets it, and it is never on the wire).
use serde::{Deserialize, Serialize};

use crate::inventory::Role;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSpec {
    /// Display name. Empty means: use the command's file name.
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub role: Role,
    /// `None` means the Host account's home directory.
    #[serde(default)]
    pub cwd: Option<String>,
    /// `None` or empty means the login shell.
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    /// A tool for the peek: not saved, restored or listed as a session.
    #[serde(default)]
    pub peek: bool,
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
