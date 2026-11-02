//! Settings › Skippers › Instructions: one file per Skipper, written once,
//! with its status from the hash in the marker. Its own test binary: HOME
//! and SKIFF_CONFIG are process-wide.

use skiff_client::skipper::{self, Scope, Status};
use skiff_core::config;

#[test]
fn instruction_files_are_written_once_and_keep_their_status() {
    let root = std::env::temp_dir().join(format!("skiff-skipper-files-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    std::fs::create_dir_all(home.join("code/app")).unwrap();
    std::fs::create_dir_all(home.join("code/api")).unwrap();
    let home = home.canonicalize().unwrap();
    std::env::set_var("HOME", &home);
    std::env::set_var("SKIFF_CONFIG", root.join("config/projects.toml"));
    std::env::remove_var("HERMES_HOME");
    let toml = format!(
        "[[project]]\nname = \"api\"\npath = \"{0}/code/api\"\nfleet = \"Harbor\"\n\n[[project]]\nname = \"app\"\npath = \"{0}/code/app\"\n",
        home.display()
    );
    std::fs::create_dir_all(root.join("config")).unwrap();
    std::fs::write(root.join("config/projects.toml"), toml).unwrap();
    let cfg = config::load().unwrap();
    let top = skipper::folder(&cfg).unwrap();

    // Tree order: the Skipper, each fleet, the projects in no fleet, then the fleets' projects. No file yet.
    let rows = skipper::instructions(&cfg).unwrap();
    let files: Vec<&str> = rows.iter().map(|r| r.file.as_str()).collect();
    assert_eq!(files, ["AGENTS.md", "fleets/Harbor/SKIPPER.md", "projects/app/SKIPPER.md", "projects/api/SKIPPER.md"]);
    assert!(rows.iter().all(|r| r.status == Status::None && r.size.is_none()));

    // Edit writes a missing file from the template, with its folder.
    let app = Scope::Project("app".into());
    let path = skipper::edit_file(&cfg, &app).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.starts_with("# Project Skipper: app") && text.contains(&format!("Read {}/AGENTS.md first", top.display())), "{} {text}", top.display());
    // The scope section only: no copy of AGENTS.md.
    assert!(!text.contains("the Skiff agent. You coordinate"), "{text}");
    assert_eq!(skipper::file_status(&cfg, &app).unwrap().status, Status::Template);
    assert_eq!(skipper::file_status(&cfg, &Scope::Skipper).unwrap().status, Status::Template);

    // The fleet file names no project.
    let fleet = Scope::Fleet("Harbor".into());
    let text = std::fs::read_to_string(skipper::edit_file(&cfg, &fleet).unwrap()).unwrap();
    assert!(text.starts_with("# Fleet Skipper: Harbor") && !text.contains("code/api"), "{text}");

    // An edit stays. A second Edit does not write over it.
    std::fs::write(&path, "mine\n").unwrap();
    assert_eq!(skipper::file_status(&cfg, &app).unwrap().status, Status::Edited);
    skipper::edit_file(&cfg, &app).unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "mine\n");

    // A template change: a file with status template is written again at the
    // next start; an edited one stays and shows the change.
    std::fs::write(root.join("config/projects.toml"), format!("{}\n[skiff_agent]\nname = \"Bosun\"\n", std::fs::read_to_string(root.join("config/projects.toml")).unwrap())).unwrap();
    let cfg = config::load().unwrap();
    assert_eq!(skipper::file_status(&cfg, &Scope::Skipper).unwrap().status, Status::Template);
    skipper::ensure(&cfg).unwrap();
    assert!(std::fs::read_to_string(top.join("AGENTS.md")).unwrap().contains("You are Bosun"));
    std::fs::write(top.join("AGENTS.md"), "my base\n").unwrap();
    std::fs::write(root.join("config/projects.toml"), std::fs::read_to_string(root.join("config/projects.toml")).unwrap().replace("Bosun", "Mate")).unwrap();
    let cfg = config::load().unwrap();
    assert_eq!(skipper::file_status(&cfg, &Scope::Skipper).unwrap().status, Status::EditedTemplateChanged);
    skipper::ensure(&cfg).unwrap();
    assert_eq!(std::fs::read_to_string(top.join("AGENTS.md")).unwrap(), "my base\n");

    // Reset: the template again, the old file in .bak, replaced at the next reset.
    let row = skipper::reset_file(&cfg, &Scope::Skipper).unwrap();
    assert_eq!(row.status, Status::Template);
    assert_eq!(std::fs::read_to_string(top.join("AGENTS.md.bak")).unwrap(), "my base\n");
    assert!(std::fs::read_to_string(top.join("AGENTS.md")).unwrap().contains("You are Mate"));
    // CLAUDE.md still reads the new file.
    assert!(std::fs::read_to_string(top.join("CLAUDE.md")).unwrap().contains("You are Mate"));
    skipper::reset_file(&cfg, &app).unwrap();
    assert_eq!(std::fs::read_to_string(top.join("projects/app/SKIPPER.md.bak")).unwrap(), "mine\n");

    // The first message names both files.
    let msg = skipper::first_message(&cfg, &top.join("projects/app")).unwrap();
    assert_eq!(
        msg,
        format!(
            "Read {0}/AGENTS.md, then {0}/projects/app/SKIPPER.md. SKIPPER.md wins where the two differ. Then do the First steps.",
            top.display()
        )
    );
    let _ = std::fs::remove_dir_all(&root);
}
