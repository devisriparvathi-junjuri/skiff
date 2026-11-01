use std::path::PathBuf;
use serde::{Serialize, Deserialize};

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
pub struct Config {
    #[serde(default)]
    pub keys: Keys,
    #[serde(default)]
    pub agents: Agents,
    #[serde(default)]
    pub appearance: Appearance,
    #[serde(default)]
    pub open: Open,
    #[serde(default)]
    pub skiff_agent: SkiffAgent,
    #[serde(default, rename = "project")]
    pub projects: Vec<ProjectConfig>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
pub struct Appearance {
    /// The theme id the app and every terminal without its own theme use.
    /// Absent: Harbor.
    pub theme: Option<String>,
    /// Terminal text size in points; the app text scales with it. Absent: 13.
    pub font_size: Option<u8>,
    /// Linux: `header-bar` or `menu-bar`. Absent: the header bar on GNOME,
    /// else the menu bar. The app reads it when the window opens.
    pub menu_layout: Option<String>,
    /// Mission control's filter: `running` shows only the sessions that work
    /// or wait, `all` shows every session. Absent: `running`.
    pub mission_filter: Option<String>,
    /// The app's language, as a code such as `es`. A language Skiff has no
    /// words for shows English. Absent: the system language. The app reads
    /// it when the window opens.
    pub language: Option<String>,
}

/// Which apps Skiff hands things to.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
pub struct Open {
    /// File kinds that open on selection in the peek.
    #[serde(default)]
    pub peek: Vec<String>,
    /// Shows a changed file's diff. `{target}` becomes what to compare.
    /// Absent: `git diff --color=always`.
    pub diff: Option<String>,
    /// Opens text files. `{path}` becomes the file. Absent: the OS default app.
    pub text: Option<String>,
    /// Opens `.md` files. Absent: the OS default app.
    pub markdown: Option<String>,
    /// Opens `.html` files. Absent: the OS default app.
    pub html: Option<String>,
    /// Opens images, such as one in the Images library. Absent: the OS
    /// default app. Never in the peek.
    #[serde(default)]
    pub image: Option<String>,
}

/// `[skiff_agent]`: the one agent session that coordinates the others. The
/// app reads and writes it; the daemon passes it by. `enabled` and
/// `project_skippers` from an older Skiff are read and ignored, and the
/// next write takes them out ([`super::persistence::DROPPED`]).
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
pub struct SkiffAgent {
    /// The label on its row, its pane and its key. Absent: Admiral.
    pub name: Option<String>,
    /// The label for project and fleet Skippers. Absent: Skipper.
    pub skipper_name: Option<String>,
    /// The agent id it starts. Absent: the first enabled agent.
    pub agent: Option<String>,
    /// The model it starts with. Absent: the agent's default.
    pub model: Option<String>,
    /// A folder in place of `~/skiff-agent`. Only by hand in projects.toml:
    /// the app checks it and refuses one that would mix with other work.
    pub folder: Option<PathBuf>,
    /// A restored pane continues the last conversation in the folder.
    #[serde(default)]
    pub continue_on_restore: bool,
    /// The agent id project and fleet Skippers start. Absent: the
    /// Skipper's agent. A project's `skipper_agent` wins over it.
    pub project_agent: Option<String>,
    /// The model project and fleet Skippers start with. Absent: the agent's
    /// default.
    pub project_model: Option<String>,
    /// `[skiff_agent.tasks]`: the agent and model each task type starts.
    #[serde(default, skip_serializing_if = "TaskTypes::is_empty")]
    pub tasks: TaskTypes,
    /// `cache_days`: at app start, cache files older than this many days go.
    /// `0`: never. Absent: [`CACHE_DAYS`].
    pub cache_days: Option<u32>,
}

/// Cache files older than this many days go at app start, unless
/// `[skiff_agent] cache_days` says otherwise.
pub const CACHE_DAYS: u32 = 30;

/// The main Skipper's name until Settings has one.
pub const ADMIRAL: &str = "Admiral";

/// The role word: the old default name of the main Skipper, and the tail of
/// a project or fleet Skipper's session name ("gesher Skipper").
pub const SKIPPER: &str = "Skipper";

impl SkiffAgent {
    /// The main Skipper's name: the one from Settings, else [`ADMIRAL`].
    pub fn name(&self) -> String {
        match self.name.as_deref().map(str::trim) {
            None | Some("") => ADMIRAL.into(),
            Some(n) => n.into(),
        }
    }

    /// The tail of a project or fleet Skipper's session name: the name from
    /// Settings, else "Skipper". The main name is not used there.
    pub fn skipper_word(&self) -> String {
        self.skipper_name.as_deref().map(str::trim).filter(|n| !n.is_empty()).unwrap_or(SKIPPER).into()
    }

    /// The age in days at which auto flush removes a cache file. `0`: off.
    pub fn cache_days(&self) -> u32 {
        self.cache_days.unwrap_or(CACHE_DAYS)
    }
}

/// One `[skiff_agent.tasks]` row: `review = { agent = "claude", model = "opus" }`.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
pub struct TaskRow {
    /// An agent id from `[agents]`.
    pub agent: String,
    /// Absent: the agent's default model.
    pub model: Option<String>,
}

/// `[skiff_agent.tasks]` in file order, which is the order Settings shows.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TaskTypes(pub Vec<(String, TaskRow)>);

impl TaskTypes {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn get(&self, name: &str) -> Option<&TaskRow> {
        self.0.iter().find(|(k, _)| k == name).map(|(_, r)| r)
    }
}

impl Serialize for TaskTypes {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut m = s.serialize_map(Some(self.0.len()))?;
        for (k, v) in &self.0 {
            m.serialize_entry(k, v)?;
        }
        m.end()
    }
}

impl<'de> Deserialize<'de> for TaskTypes {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct Rows;
        impl<'de> serde::de::Visitor<'de> for Rows {
            type Value = TaskTypes;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a table of task types")
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(self, mut m: A) -> Result<TaskTypes, A::Error> {
                let mut rows = Vec::new();
                while let Some((k, v)) = m.next_entry::<String, TaskRow>()? {
                    rows.push((k, v));
                }
                Ok(TaskTypes(rows))
            }
        }
        d.deserialize_map(Rows)
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
pub struct Agents {
    /// Agent ids the + menu offers. Absent: every installed agent.
    pub enabled: Option<Vec<String>>,
    /// Command lines that replace the defaults, by agent id.
    #[serde(default)]
    pub commands: std::collections::BTreeMap<String, String>,
    /// Default agents the user took out. Restore brings them back.
    #[serde(default)]
    pub removed: Vec<String>,
}

/// Key overrides by action: `palette = "ctrl+shift+p"`. The app holds the
/// defaults and the action names; unknown actions are ignored.
pub type Keys = std::collections::BTreeMap<String, String>;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct ProjectConfig {
    pub name: String,
    /// Rail label, 3 characters. Derived from `name` when absent.
    pub short: Option<String>,
    pub path: PathBuf,
    pub color: Option<String>,
    /// Image for the rail: a path, relative to the project or absolute.
    /// Absent: detect one in the repo. Empty: no icon.
    pub icon: Option<String>,
    /// Image behind the terminals: a path, relative to the project or absolute.
    pub background: Option<String>,
    /// How opaque the panes are over the background image, in percent. Absent: 85.
    pub background_opacity: Option<u8>,
    #[serde(default)]
    pub agents: Vec<String>,
    /// Shows a Files tree under each worktree. Absent: shown for a plain
    /// folder, hidden for a git project. The app reads and writes it, always
    /// as `true` or `false`; the daemon passes it by.
    pub files: Option<bool>,
    /// `false` hides the Changes section and the +/- counts. Absent: shown.
    pub changes: Option<bool>,
    /// Theme id for this project's chrome and its terminals. Absent: the app theme.
    pub theme: Option<String>,
    /// Closed: kept with its settings, but out of the rail until it opens again.
    #[serde(default)]
    pub closed: bool,
    /// The fleet this project sits in on the rail. Absent: on the rail by itself.
    pub fleet: Option<String>,
    /// The agent id this project's Skipper starts, in place of
    /// `[skiff_agent] project_agent`. Only by hand: Settings has no row for it.
    pub skipper_agent: Option<String>,
}

impl ProjectConfig {
    pub fn short(&self) -> String {
        self.short.clone().unwrap_or_else(|| short_name(&self.name))
    }
}

/// The default rail label: the first 3 letters or digits, lower case.
pub fn short_name(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(3)
        .collect::<String>()
        .to_lowercase()
}

/// What Skiff would add for `dir`, before anything is written.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct FolderInfo {
    /// The git top level, or `dir` itself when it is in no repository.
    /// `None` when the folder cannot be added.
    pub root: Option<PathBuf>,
    pub name: String,
    pub short: String,
    pub color: String,
    pub branch: Option<String>,
    pub worktrees: usize,
    /// The icon Skiff found in the repo, as a data URL, and its path
    /// relative to the root.
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub icon_path: Option<String>,
    /// Why the folder cannot be added.
    pub error: Option<String>,
}
