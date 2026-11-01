//! M3 agent list: the agent CLIs the Host account can start, as its own
//! environment and settings say (its login PATH, its `projects.toml`). Never
//! the client's. Changes go to `skiff-host` (`agent_config@1`), each from
//! the revision of the `[agents]` table it was read at.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostAgent {
    pub id: String,
    /// The command line a new session of this agent runs on the Host.
    pub command: String,
    /// Empty for an agent the Host account added itself.
    #[serde(default)]
    pub default_command: String,
    #[serde(default)]
    pub custom: bool,
    /// Found on the Host account's PATH.
    pub installed: bool,
    /// Offered to start. Only an installed agent is enabled.
    pub enabled: bool,
    /// What starts it on the Host, from its config: "Used by task type
    /// review". It cannot be removed then. Absent: nothing, or an older Host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub used_by: Option<String>,
}

/// One change to the Host's `[agents]` table (`agent_config@1`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "key", rename_all = "snake_case")]
pub enum AgentEdit {
    /// The agents the + menu offers, by id.
    Enabled { agents: Vec<String> },
    /// The agent's command line. Empty restores a known agent's default; a
    /// new id adds a custom agent.
    Command { agent: String, command: String },
    /// The agent's id. Its task types follow it.
    Rename { agent: String, name: String },
    /// Takes the agent away: a known one until Restore, a custom one for good.
    Remove { agent: String },
    /// Brings back the known agents taken away.
    Restore,
}
