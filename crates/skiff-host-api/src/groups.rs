//! M1g session groups: named layouts of sessions that the Host keeps for every
//! client. Same JSON as the existing desktop/skiffd `Group`.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SplitDir {
    Row,
    Col,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Layout {
    Pane {
        session: String,
    },
    Split {
        dir: SplitDir,
        /// Share of `a`; the Host clamps it.
        ratio: f32,
        a: Box<Layout>,
        b: Box<Layout>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Group {
    /// Empty on create; the Host assigns one.
    #[serde(default)]
    pub id: String,
    pub name: String,
    pub layout: Layout,
    #[serde(default)]
    pub focus: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
}
