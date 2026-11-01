//! `~/.config/skiff/projects.toml`. Declarative only: no plugins, no hooks.

mod types;
pub mod client;
mod persistence;
mod projects;
mod settings;

pub use types::*;
pub use persistence::{config_path, write_atomic, load, load_from, parse, projects_revision, edit_projects, agents_revision, edit_agents_at, check_label, Stale, Refused, LABEL_MAX};
pub use projects::{inspect_folder, add_project, set_project_icon, set_project_short, set_project_fleet, set_project_background, set_project_background_opacity, set_project_color, set_project_changes, set_project_theme, set_project_files, set_project_closed, remove_project, reorder_projects, arrange_projects};
pub use settings::{set_enabled_agents, set_agent_command, remove_agent, rename_agent, restore_agents, set_app_theme, set_font_size, set_menu_layout, set_mission_filter, set_language, MENU_LAYOUTS, OPEN_KEYS, set_open, set_open_peek, set_skiff_agent, SkiffAgentKey, add_task_type, set_task_type, rename_task_type, remove_task_type};

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r##"
[theme]
file = "themes/harbor.css"

[keys]
palette      = "ctrl+shift+p"
next-waiting = "ctrl+shift+j"

[[project]]
name   = "skiff"
short  = "skf"
path   = "~/code/skiff"
color  = "#b69cff"
editor = "zed"
agents = ["claude", "codex"]
layout = "stacked"

[[project]]
name   = "storefront"
path   = "~/code/storefront"
color  = "#f28fd0"
agents = ["gemini"]
layout = "side-by-side"
server = { cmd = "npm run dev", port_env = "PORT" }
"##;

    #[test]
    fn parses_legacy_sample_with_current_defaults() {
        let cfg = parse(SAMPLE).unwrap();
        assert_eq!(cfg.projects.len(), 2);
        assert_eq!(cfg.projects[0].short(), "skf");
        assert_eq!(cfg.projects[1].short(), "sto");
        assert_eq!(cfg.keys.get("palette").map(String::as_str), Some("ctrl+shift+p"));
        assert!(!cfg.projects[0].path.to_string_lossy().starts_with('~'));
        // Obsolete keys in an older file do not block the current settings.
        assert_eq!(cfg.appearance, Appearance::default());
        assert_eq!(cfg.projects[0].agents, ["claude", "codex"]);
        assert_eq!(cfg.projects[1].agents, ["gemini"]);
    }
}
