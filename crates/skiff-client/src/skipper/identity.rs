//! Skipper identity, scope, naming, agent selection and command helpers.

use super::*;

/// The main Skipper's name until Settings has one. The app keeps the same default.
pub const DEFAULT_NAME: &str = config::ADMIRAL;

/// The scope prefix of a fleet: a fleet Skipper's mark and its rows' owner
/// are `fleet:<fleet>`. A project's are its bare name.
pub const FLEET: &str = "fleet:";

/// The scope of the fleet `fleet`: `fleet:<fleet>`.
pub fn fleet_scope(fleet: &str) -> String {
    format!("{FLEET}{fleet}")
}

/// The fleet a scope names, or `None` for a project's.
pub fn scope_fleet(scope: &str) -> Option<&str> {
    scope.strip_prefix(FLEET)
}

/// The configured main Skipper name, or Admiral.
pub fn name(cfg: &Config) -> String {
    cfg.skiff_agent.name()
}

/// A project Skipper's session name: the project, then the configured name
/// or "Skipper" ("gesher Skipper"). Unique per project, and never the grand
/// Skipper's, so `skiff send "gesher Skipper"` reaches one session.
pub fn project_name(cfg: &Config, project: &str) -> String {
    format!("{project} {}", cfg.skiff_agent.skipper_word())
}

/// A fleet Skipper's session name: the fleet, then the configured name or
/// "Skipper" ("Harbor Skipper").
pub fn fleet_name(cfg: &Config, fleet: &str) -> String {
    format!("{fleet} {}", cfg.skiff_agent.skipper_word())
}

/// The agent the Skiff agent starts: the one from Settings while it is
/// installed and on (a custom agent is on), else the first enabled one.
/// Settings shows the same.
pub fn choose<'a>(agents: &'a [AgentInfo], id: Option<&str>) -> Option<&'a AgentInfo> {
    agents
        .iter()
        .find(|a| Some(a.id.as_str()) == id && a.installed && (a.enabled || a.custom))
        .or_else(|| agents.iter().find(|a| a.enabled))
}

/// The line that continues the newest conversation in the folder, from each
/// CLI's help (2026-10-13). Only the Skiff agent's own folder runs it: it
/// holds no other conversation. A custom agent has none. Keep in step with
/// `CONTINUE` in apps/desktop/src/terminal/resume.ts, which Settings shows.
pub fn continue_line(agent: &str, cmd: &str) -> Option<String> {
    let cmd = cmd.trim();
    // A subcommand goes after the program and before the user's flags.
    let sub = |words: &str| match cmd.split_once(char::is_whitespace) {
        Some((program, flags)) => format!("{program} {words} {}", flags.trim()),
        None => format!("{cmd} {words}"),
    };
    match agent {
        "claude" | "opencode" | "grok" | "gemini" => Some(format!("{cmd} --continue")),
        "codex" => Some(sub("resume --last")),
        // `--continue` takes the newest session anywhere; `--in` keeps it to this folder.
        "hermes" => Some(format!("{cmd} --resume latest --in .")),
        _ => None,
    }
}

/// The agent's command with the model flag, as a task type adds it. An
/// agent that takes its model in its command, or no model, gives the plain
/// command: Settings shows "in the command" for it.
pub fn with_model(agent: &AgentInfo, model: Option<&str>) -> String {
    skiff_core::tasks::command(agent, model).unwrap_or_else(|_| agent.command.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::super::spec;
    use super::*;

    /// A project Skipper is named after its project and the Skiff agent's
    /// name, so no two sessions share a name.
    #[test]
    fn a_project_skipper_takes_its_project_and_the_agents_name() {
        // The two stored names stay independent.
        let cfg = skiff_core::config::parse("[skiff_agent]\nname = \"Skipper\"\n").unwrap();
        assert_eq!(project_name(&cfg, "gesher"), "gesher Skipper");
        assert_eq!(name(&cfg), "Skipper");
        let cfg = skiff_core::config::parse("[skiff_agent]\nname = \"Captain\"\n").unwrap();
        assert_eq!(project_name(&cfg, "skiff"), "skiff Skipper");
        assert_eq!(name(&cfg), "Captain");
        let cfg = skiff_core::config::parse("[skiff_agent]\nname = \"Captain\"\nskipper_name = \"Mate\"\n").unwrap();
        assert_eq!(project_name(&cfg, "skiff"), "skiff Mate");
        let cfg = skiff_core::config::parse("").unwrap();
        assert_eq!(project_name(&cfg, "p"), "p Skipper");
        assert_eq!(name(&cfg), "Admiral");
    }

    #[test]
    fn continue_lines() {
        assert_eq!(continue_line("claude", "claude --model x").as_deref(), Some("claude --model x --continue"));
        assert_eq!(continue_line("codex", "codex --yolo").as_deref(), Some("codex resume --last --yolo"));
        assert_eq!(continue_line("codex", "codex").as_deref(), Some("codex resume --last"));
        assert_eq!(continue_line("mine", "mine").as_deref(), None);
        // The person's bypass flag stays on resume.
        assert_eq!(
            continue_line("claude", "claude --dangerously-skip-permissions").as_deref(),
            Some("claude --dangerously-skip-permissions --continue")
        );
        assert_eq!(continue_line("gemini", "agy --dangerously-skip-permissions").as_deref(), Some("agy --dangerously-skip-permissions --continue"));
        // The built-in line resumes with the agent's own prompts.
        assert_eq!(continue_line("gemini", "agy").as_deref(), Some("agy --continue"));
    }

    #[test]
    fn launch_starts_the_persons_command() {
        let agent = |command: &str| AgentInfo {
            id: "codex".into(),
            command: command.into(),
            default_command: "codex".into(),
            custom: false,
            installed: true,
            enabled: true,
            used_by: None,
        };
        let s = spec(&with_model(&agent("codex"), None), "Skipper", PathBuf::from("/tmp"));
        assert_eq!((s.command.as_deref(), s.args.len()), (Some("codex"), 0));
        let s = spec(&with_model(&agent(" codex --yolo "), Some("gpt-5")), "Skipper", PathBuf::from("/tmp"));
        assert_eq!((s.command.as_deref(), s.args), (Some("codex"), vec!["--yolo".to_string(), "-m".into(), "gpt-5".into()]));
        // An agent with no model flag takes its model in its command.
        let mut gemini = agent("gemini");
        gemini.id = "gemini".into();
        assert_eq!(with_model(&gemini, Some("pro")), "gemini");
    }
}
