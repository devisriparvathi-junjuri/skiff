use skiff_core::config::{self, SkiffAgentKey};

// Its own test binary: SKIFF_CONFIG is process-wide.
#[test]
fn skiff_agent_keys_round_trip() {
    let dir = std::env::temp_dir().join(format!("skiff-agent-config-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("SKIFF_CONFIG", dir.join("projects.toml"));

    assert_eq!(config::load().unwrap().skiff_agent, config::SkiffAgent::default());

    config::set_skiff_agent(SkiffAgentKey::Name(Some(" Bosun "))).unwrap();
    config::set_skiff_agent(SkiffAgentKey::SkipperName(Some(" Skipper "))).unwrap();
    config::set_skiff_agent(SkiffAgentKey::Agent(Some("codex"))).unwrap();
    config::set_skiff_agent(SkiffAgentKey::Folder(Some(&dir))).unwrap();
    config::set_skiff_agent(SkiffAgentKey::ContinueOnRestore(true)).unwrap();
    config::set_skiff_agent(SkiffAgentKey::Model(Some(" opus "))).unwrap();
    config::set_skiff_agent(SkiffAgentKey::ProjectModel(Some("sonnet"))).unwrap();
    let a = config::load().unwrap().skiff_agent;
    assert_eq!(a.name.as_deref(), Some("Bosun"));
    assert_eq!(a.skipper_name.as_deref(), Some("Skipper"));
    assert_eq!(a.agent.as_deref(), Some("codex"));
    assert_eq!(a.folder.as_deref(), Some(dir.as_path()));
    assert!(a.continue_on_restore);
    assert_eq!(a.model.as_deref(), Some("opus"));
    assert_eq!(a.project_model.as_deref(), Some("sonnet"));
    // A model is one word.
    assert!(config::set_skiff_agent(SkiffAgentKey::Model(Some("op us"))).is_err());

    // Empty or off takes the key out, so the default applies.
    config::set_skiff_agent(SkiffAgentKey::Name(Some(""))).unwrap();
    config::set_skiff_agent(SkiffAgentKey::SkipperName(Some(""))).unwrap();
    config::set_skiff_agent(SkiffAgentKey::ContinueOnRestore(false)).unwrap();
    config::set_skiff_agent(SkiffAgentKey::Model(None)).unwrap();
    config::set_skiff_agent(SkiffAgentKey::ProjectModel(Some(""))).unwrap();
    let text = std::fs::read_to_string(dir.join("projects.toml")).unwrap();
    assert!(!text.contains("name") && !text.contains("continue_on_restore") && !text.contains("model"), "{text}");

    // `enabled` and `project_skippers` from an older Skiff: read and
    // ignored, then gone at the next write of projects.toml.
    let old = format!("{text}enabled = true\nproject_skippers = false\n");
    std::fs::write(dir.join("projects.toml"), &old).unwrap();
    assert_eq!(config::load().unwrap().skiff_agent.agent.as_deref(), Some("codex"));
    config::set_skiff_agent(SkiffAgentKey::Name(Some("Mate"))).unwrap();
    let text = std::fs::read_to_string(dir.join("projects.toml")).unwrap();
    assert!(!text.contains("enabled") && !text.contains("project_skippers") && text.contains("codex"), "{text}");

    std::fs::remove_dir_all(&dir).unwrap();
}
