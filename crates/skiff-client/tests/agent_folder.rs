//! Settings › Skippers › Folder (#135): the rules, the path, the copy,
//! the switch and the rollback. Its own test binary: HOME and SKIFF_CONFIG
//! are process-wide, so the tests take turns.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Mutex,
};

use rusqlite::Connection;
use skiff_client::{
    agent_folder::{self, Kind, Mode, Panes, Step, FAIL_AT},
    skipper,
};
use skiff_core::config;

static TURN: Mutex<()> = Mutex::new(());

/// A fresh home and config for one test. Returns the home.
fn world(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("skiff-folder-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let home = home.canonicalize().unwrap();
    std::env::set_var("HOME", &home);
    std::env::set_var("SKIFF_CONFIG", root.join("config/projects.toml"));
    std::env::remove_var("HERMES_HOME");
    std::env::remove_var("SKIFF_LEDGER");
    *FAIL_AT.lock().unwrap() = None;
    home
}

fn done(home: &Path) {
    let _ = std::fs::remove_dir_all(home.parent().unwrap());
}

fn cfg() -> config::Config {
    config::load().unwrap()
}

/// Every file's bytes and every link's target, by path.
fn snapshot(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            let rel = p.strip_prefix(dir).unwrap().to_string_lossy().into_owned();
            let m = p.symlink_metadata().unwrap();
            if m.file_type().is_symlink() {
                out.insert(rel, std::fs::read_link(&p).unwrap().to_string_lossy().as_bytes().to_vec());
            } else if m.is_dir() {
                out.insert(rel, b"<dir>".to_vec());
                stack.push(p);
            } else {
                out.insert(rel, std::fs::read(&p).unwrap());
            }
        }
    }
    out
}

/// The default folder, set up, with 3 tasks (done, pending, and sent in a
/// live pane), their results, an edited AGENTS.md and a file of the person's.
fn current(home: &Path) -> PathBuf {
    let dir = skipper::setup(&home.join("skiff-agent"), "Skipper").unwrap();
    std::fs::write(dir.join("AGENTS.md"), "mine").unwrap();
    std::fs::write(dir.join("notes.md"), "notes").unwrap();
    let r = dir.join("results");
    let db = Connection::open(dir.join("ledger.db")).unwrap();
    db.pragma_update(None, "journal_mode", "WAL").unwrap();
    db.execute_batch(
        "CREATE TABLE tasks (id INTEGER PRIMARY KEY AUTOINCREMENT, uid TEXT NOT NULL UNIQUE, kind TEXT NOT NULL DEFAULT 'task',
           parent TEXT, pane_id TEXT, pane_gen INTEGER, project TEXT NOT NULL DEFAULT '', task TEXT NOT NULL, sent_at TEXT NOT NULL,
           state TEXT NOT NULL, result_path TEXT, ref TEXT, updated_at TEXT NOT NULL, type TEXT);
         CREATE TABLE uids (uid TEXT PRIMARY KEY);",
    )
    .unwrap();
    for (uid, state, pane) in [("aaaa0001", "done", "gone-pane"), ("bbbb0002", "pending", "busy-pane"), ("cccc0003", "sent", "live-pane")] {
        let result = r.join(format!("{uid}.md"));
        std::fs::write(&result, format!("result {uid}")).unwrap();
        db.execute(
            "INSERT INTO tasks (uid, pane_id, pane_gen, task, sent_at, state, result_path, updated_at) VALUES (?1, ?2, 5, 'do it', 'now', ?3, ?4, 'now')",
            rusqlite::params![uid, pane, state, result.to_string_lossy()],
        )
        .unwrap();
        db.execute("INSERT INTO uids VALUES (?1)", [uid]).unwrap();
    }
    // A work folder (ledger version 11) moves as its result does.
    db.execute_batch("ALTER TABLE tasks ADD COLUMN work_dir TEXT").unwrap();
    db.execute("UPDATE tasks SET work_dir = ?1 WHERE uid = 'bbbb0002'", [r.join("bbbb0002-x").to_string_lossy()]).unwrap();
    // A result outside the folder stays where it is.
    db.execute("UPDATE tasks SET result_path = '/elsewhere/x.md' WHERE uid = 'aaaa0001'", []).unwrap();
    std::fs::write(r.join("aaaa0001.md"), "kept").unwrap();
    dir
}

fn panes() -> Panes {
    Panes::from([("live-pane".to_string(), Some(5)), ("busy-pane".to_string(), Some(5))])
}

fn result_paths(ledger: &Path) -> BTreeMap<String, String> {
    let db = Connection::open(ledger).unwrap();
    let mut q = db.prepare("SELECT uid, result_path FROM tasks").unwrap();
    q.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap().map(Result::unwrap).collect()
}

fn kind(home: &Path, cfg: &config::Config, input: &str) -> (Kind, Option<String>) {
    let to = agent_folder::resolve(input, home, home).unwrap();
    let p = agent_folder::preview(cfg, &to, &Panes::new()).unwrap();
    (p.kind, p.refused)
}

#[test]
fn every_rule_refuses_with_its_reason() {
    let _t = TURN.lock().unwrap_or_else(|e| e.into_inner());
    let home = world("rules");
    let project = home.join("code/app");
    std::fs::create_dir_all(project.join(".git")).unwrap();
    std::fs::create_dir_all(home.join("repo/.git")).unwrap();
    std::fs::create_dir_all(home.join("repo/deep/er")).unwrap();
    std::fs::create_dir_all(home.join(".hermes/profiles/x")).unwrap();
    std::fs::create_dir_all(home.join(".claude")).unwrap();
    std::fs::create_dir_all(home.join("profile")).unwrap();
    std::fs::write(home.join("profile/SOUL.md"), "x").unwrap();
    std::fs::create_dir_all(home.join("busy")).unwrap();
    std::fs::write(home.join("busy/notes.md"), "x").unwrap();
    std::fs::write(home.join("file"), "x").unwrap();
    std::fs::create_dir_all(home.join("Documents")).unwrap();
    std::fs::create_dir_all(home.join("empty")).unwrap();
    let cfg = config::parse(&format!("[[project]]\nname = \"app\"\npath = \"{}\"\n", project.display())).unwrap();
    skipper::setup(&home.join("skiff-agent"), "Skipper").unwrap();

    // Each choice is equal: the visible and hidden suggestions, any other dot-folder,
    // Documents, another volume's path.
    assert_eq!(kind(&home, &cfg, "~/skiff-agent").0, Kind::Current);
    assert_eq!(kind(&home, &cfg, "~/.skiff-agent"), (Kind::New, None));
    assert_eq!(kind(&home, &cfg, "~/.skiff"), (Kind::New, None));
    assert_eq!(kind(&home, &cfg, "~/Documents/Skipper"), (Kind::New, None));
    assert_eq!(kind(&home, &cfg, "~/empty"), (Kind::Empty, None));
    assert_eq!(kind(&home, &cfg, "~/.config/skipper"), (Kind::New, None));

    for (input, says) in [
        ("~/.hermes", "Hermes profile"),
        ("~/.hermes/profiles/x", "Hermes profile"),
        ("~/profile", "Hermes profile"),
        ("~/.claude", "home folder of claude"),
        ("~/.claude/agent", "home folder of claude"),
        ("~/.codex", "home folder of codex"),
        ("~/repo", "inside a Git repository"),
        ("~/repo/deep/er", "inside a Git repository"),
        ("~/repo/deep/new", "inside a Git repository"),
        ("~/code/app", "project app"),
        ("~/code", "project app"),
        ("~/code/app/sub", "project app"),
        ("~", "home folder holds"),
        ("/", "root folder"),
        ("~/busy", "holds other files"),
        ("~/file", "is a file"),
        ("~/skiff-agent/results/x", "inside the current Skiff folder"),
        ("~/x.skiff-new.123", "unfinished copy"),
    ] {
        let (k, why) = kind(&home, &cfg, input);
        assert_eq!(k, Kind::Refused, "{input}");
        assert!(why.as_deref().unwrap_or_default().contains(says), "{input}: {why:?}");
    }
    let config_dir = config::config_path().parent().unwrap().display().to_string();
    assert!(kind(&home, &cfg, &format!("{config_dir}/agent")).1.unwrap().contains("config folder"));

    // HERMES_HOME counts wherever it is.
    std::env::set_var("HERMES_HOME", home.join("hh"));
    assert!(kind(&home, &cfg, "~/hh/x").1.unwrap().contains("Hermes profile"));
    std::env::remove_var("HERMES_HOME");

    // The override in projects.toml meets the same rules. A relative path is refused.
    for bad in ["rel/agent", "~/repo/agent", "~/.hermes"] {
        let cfg = config::parse(&format!("[skiff_agent]\nfolder = \"{bad}\"\n")).unwrap();
        assert!(skipper::folder(&cfg).is_err(), "{bad}");
    }
    for dot in [".skiff-agent", ".skiff"] {
        let cfg = config::parse(&format!("[skiff_agent]\nfolder = \"~/{dot}\"\n")).unwrap();
        assert_eq!(skipper::folder(&cfg).unwrap(), home.join(dot));
    }
    // A Skiff folder at the old suggestion, ~/.skiff, is still switched to as it is.
    skipper::setup(&home.join(".skiff"), "Skipper").unwrap();
    assert_eq!(kind(&home, &cfg, "~/.skiff"), (Kind::Skiff, None));
    done(&home);
}

#[test]
fn a_chosen_path_is_stored_once_canonical() {
    let _t = TURN.lock().unwrap_or_else(|e| e.into_inner());
    let home = world("paths");
    std::fs::create_dir_all(home.join("real")).unwrap();
    std::os::unix::fs::symlink(home.join("real"), home.join("link")).unwrap();
    let want = home.join("x");
    for input in ["./x", "x", "~/x", "~/a/../x", &want.display().to_string()] {
        assert_eq!(agent_folder::resolve(input, &home, &home).unwrap(), want, "{input}");
    }
    // A link resolves when it is chosen, and so does `..` after it.
    assert_eq!(agent_folder::resolve("~/link/sub", &home, &home).unwrap(), home.join("real/sub"));
    assert!(agent_folder::resolve("  ", &home, &home).is_err());

    skipper::setup(&home.join("skiff-agent"), "Skipper").unwrap();
    let to = agent_folder::resolve("./link/agent", &home, &home).unwrap();
    agent_folder::change(&cfg(), &to, Mode::Empty, &Panes::new(), false).unwrap();
    let text = std::fs::read_to_string(config::config_path()).unwrap();
    assert!(text.contains(&format!("folder = \"{}\"", home.join("real/agent").display())), "{text}");
    // Another working folder finds the same folder.
    let before = std::env::current_dir().unwrap();
    std::env::set_current_dir(home.join("real")).unwrap();
    assert_eq!(skipper::folder(&cfg()).unwrap(), home.join("real/agent"));
    std::env::set_current_dir(before).unwrap();
    done(&home);
}

#[test]
fn a_copy_keeps_every_row_and_moves_the_result_paths() {
    let _t = TURN.lock().unwrap_or_else(|e| e.into_inner());
    let home = world("copy");
    let old = current(&home);
    let before = snapshot(&old);
    let to = agent_folder::resolve("~/.skiff-agent", &home, &home).unwrap();

    let p = agent_folder::preview(&cfg(), &to, &panes()).unwrap();
    assert_eq!(p.kind, Kind::New);
    assert!(p.can_copy && p.copy_refused.is_none());
    let c = p.from_counts.unwrap();
    assert_eq!((c.tasks, c.open, c.done, c.results), (3, 2, 1, 3));
    assert!(c.agents_edited && c.other == ["notes.md"], "{c:?}");
    // Only the sent task runs: a pending one is not delivered yet.
    assert_eq!(p.running.iter().map(|r| r.uid.as_str()).collect::<Vec<_>>(), ["cccc0003"]);

    // A running task stops the change by default.
    let e = agent_folder::change(&cfg(), &to, Mode::Copy, &panes(), false).unwrap_err().to_string();
    assert!(e.contains("1 task still runs"), "{e}");
    assert!(differ(&snapshot(&old), &before).is_empty(), "{:?}", differ(&snapshot(&old), &before));
    assert!(!to.exists());

    let d = agent_folder::change(&cfg(), &to, Mode::Copy, &panes(), true).unwrap();
    assert!(d.warning.is_none(), "{:?}", d.warning);
    assert_eq!(skipper::folder(&cfg()).unwrap(), to);
    // Every row and uid; results in the old folder now point to the new one,
    // except the task that still runs, and a result outside the folder.
    let paths = result_paths(&to.join("ledger.db"));
    assert_eq!(paths["aaaa0001"], "/elsewhere/x.md");
    assert_eq!(paths["bbbb0002"], to.join("results/bbbb0002.md").display().to_string());
    assert_eq!(paths["cccc0003"], old.join("results/cccc0003.md").display().to_string());
    let work: String = Connection::open(to.join("ledger.db")).unwrap().query_row("SELECT work_dir FROM tasks WHERE uid = 'bbbb0002'", [], |r| r.get(0)).unwrap();
    assert_eq!(work, to.join("results/bbbb0002-x").display().to_string());
    let uids: Vec<String> = Connection::open(to.join("ledger.db")).unwrap().prepare("SELECT uid FROM uids ORDER BY uid").unwrap().query_map([], |r| r.get(0)).unwrap().map(Result::unwrap).collect();
    assert_eq!(uids, ["aaaa0001", "bbbb0002", "cccc0003"]);
    // The person's files come along; setup makes the links and the marker again.
    assert_eq!(std::fs::read_to_string(to.join("AGENTS.md")).unwrap(), "mine");
    assert_eq!(std::fs::read_to_string(to.join("CLAUDE.md")).unwrap(), "mine");
    assert_eq!(std::fs::read_to_string(to.join("notes.md")).unwrap(), "notes");
    assert_eq!(std::fs::read_to_string(to.join("results/bbbb0002.md")).unwrap(), "result bbbb0002");
    assert!(to.join(".skiff-agent").is_file() && to.join(".claude/skills/skiff-cli/SKILL.md").is_file());
    assert_eq!(d.copied.unwrap().tasks, 3);
    // The old folder keeps every file, and gets MOVED.md.
    let mut after = snapshot(&old);
    let note = String::from_utf8(after.remove("MOVED.md").unwrap()).unwrap();
    assert!(note.contains(&to.display().to_string()), "{note}");
    assert!(differ(&after, &before).is_empty(), "{:?}", differ(&after, &before));
    assert!(no_staging(&home));

    // Back: the old folder is a Skiff folder. Switch only, no merge.
    let back = agent_folder::resolve("~/skiff-agent", &home, &home).unwrap();
    let p = agent_folder::preview(&cfg(), &back, &panes()).unwrap();
    assert_eq!(p.kind, Kind::Skiff);
    assert!(p.moved.is_some() && p.to_counts.unwrap().tasks == 3);
    assert!(agent_folder::change(&cfg(), &back, Mode::Copy, &panes(), true).is_err(), "never a copy into a Skiff folder");
    agent_folder::change(&cfg(), &back, Mode::Switch, &Panes::new(), false).unwrap();
    assert!(!std::fs::read_to_string(config::config_path()).unwrap().contains("folder"), "the default takes the key out");
    assert!(!old.join("MOVED.md").exists());
    assert!(differ(&snapshot(&old), &before).is_empty(), "{:?}", differ(&snapshot(&old), &before));
    // The copy stays as it was.
    assert_eq!(result_paths(&to.join("ledger.db")).len(), 3);
    done(&home);
}

/// The paths whose bytes differ, for a short failure message.
fn differ(a: &BTreeMap<String, Vec<u8>>, b: &BTreeMap<String, Vec<u8>>) -> Vec<String> {
    let keys: std::collections::BTreeSet<&String> = a.keys().chain(b.keys()).collect();
    keys.into_iter().filter(|k| a.get(*k) != b.get(*k)).cloned().collect()
}

fn no_staging(home: &Path) -> bool {
    std::fs::read_dir(home).unwrap().all(|e| !e.unwrap().file_name().to_string_lossy().contains(".skiff-new."))
}

#[test]
fn a_failure_at_any_step_changes_nothing() {
    let _t = TURN.lock().unwrap_or_else(|e| e.into_inner());
    let home = world("rollback");
    let old = current(&home);
    let before = snapshot(&old);
    let file = config::config_path();
    config::set_skiff_agent(config::SkiffAgentKey::Name(Some("Skipper"))).unwrap();
    let toml = std::fs::read(&file).unwrap();
    let new = agent_folder::resolve("~/Documents/Skipper", &home, &home).unwrap();
    std::fs::create_dir_all(home.join("empty")).unwrap();
    let empty = agent_folder::resolve("~/empty", &home, &home).unwrap();

    for s in [Step::Stage, Step::Copy, Step::Ledger, Step::Check, Step::Setup, Step::Rename, Step::Config] {
        for to in [&new, &empty] {
            *FAIL_AT.lock().unwrap() = Some(s);
            let e = agent_folder::change(&cfg(), to, Mode::Copy, &panes(), true).unwrap_err();
            assert!(format!("{e:#}").starts_with("Could not "), "{s:?}: {e:#}");
            assert_eq!(std::fs::read(&file).unwrap(), toml, "{s:?}");
            assert!(differ(&snapshot(&old), &before).is_empty(), "{s:?}: {:?}", differ(&snapshot(&old), &before));
            assert!(no_staging(&home), "{s:?}");
            assert!(!new.exists() && !home.join("Documents").exists(), "{s:?}: the parent it made is gone");
            assert!(empty.is_dir() && std::fs::read_dir(&empty).unwrap().next().is_none(), "{s:?}");
        }
    }
    // A staging folder a crash left is removed at the next change.
    *FAIL_AT.lock().unwrap() = None;
    std::fs::create_dir_all(home.join("empty.skiff-new.999999")).unwrap();
    std::fs::write(home.join("empty.skiff-new.999999/x"), "x").unwrap();
    // MOVED.md is the last step: the switch stands, with a warning.
    *FAIL_AT.lock().unwrap() = Some(Step::Moved);
    let d = agent_folder::change(&cfg(), &empty, Mode::Copy, &panes(), true).unwrap();
    assert!(d.warning.unwrap().contains("MOVED.md"));
    assert_eq!(skipper::folder(&cfg()).unwrap(), empty);
    assert!(no_staging(&home));
    *FAIL_AT.lock().unwrap() = None;
    done(&home);
}

#[test]
fn start_empty_sets_up_a_new_ledger() {
    let _t = TURN.lock().unwrap_or_else(|e| e.into_inner());
    let home = world("empty");
    // First run: nothing at ~/skiff-agent, so there is nothing to copy.
    let to = agent_folder::resolve("~/.skiff-agent", &home, &home).unwrap();
    let p = agent_folder::preview(&cfg(), &to, &Panes::new()).unwrap();
    assert!(!p.can_copy && p.from_counts.is_none());
    assert!(agent_folder::change(&cfg(), &to, Mode::Copy, &Panes::new(), false).is_err());
    agent_folder::change(&cfg(), &to, Mode::Empty, &Panes::new(), false).unwrap();
    assert!(to.join("AGENTS.md").is_file() && !to.join("ledger.db").exists());
    assert!(!home.join("skiff-agent").exists(), "the default is never made");
    assert_eq!(skipper::ensure(&cfg()).unwrap(), to);
    done(&home);
}

#[test]
fn the_folder_is_made_without_skipper_files() {
    let _t = TURN.lock().unwrap_or_else(|e| e.into_inner());
    let home = world("made");
    let dir = skipper::make_folder(&cfg()).unwrap();
    assert_eq!(dir, home.join("skiff-agent"));
    assert!(dir.join(".skiff-agent").is_file() && dir.join("cache").is_dir());
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(std::fs::metadata(dir.join("cache")).unwrap().permissions().mode() & 0o777, 0o700);
    for f in ["AGENTS.md", "CLAUDE.md", "GEMINI.md", ".claude", ".agents", "results"] {
        assert!(dir.join(f).symlink_metadata().is_err(), "{f}");
    }
    assert_eq!(skipper::cache_dir(&cfg()).unwrap(), dir.join("cache"));
    assert_eq!(skiff_client::cache::ensure(&cfg()).unwrap(), dir.join("cache"));
    // A second start changes nothing.
    let before = snapshot(&dir);
    skipper::make_folder(&cfg()).unwrap();
    assert!(differ(&snapshot(&dir), &before).is_empty());
    done(&home);
}

#[test]
fn the_override_is_made_in_place_of_the_default() {
    let _t = TURN.lock().unwrap_or_else(|e| e.into_inner());
    let home = world("override");
    let to = home.join("agents/mine");
    config::set_skiff_agent(config::SkiffAgentKey::Folder(Some(&to))).unwrap();
    assert_eq!(skipper::make_folder(&cfg()).unwrap(), to);
    assert!(to.join("cache").is_dir() && to.join(".skiff-agent").is_file());
    assert_eq!(skipper::cache_dir(&cfg()).unwrap(), to.join("cache"));
    assert!(!home.join("skiff-agent").exists());
    done(&home);
}

#[test]
fn skipper_setup_fills_the_folder_the_app_made() {
    let _t = TURN.lock().unwrap_or_else(|e| e.into_inner());
    let home = world("setup-made");
    let dir = skipper::make_folder(&cfg()).unwrap();
    std::fs::write(dir.join("cache/0123456789abcdef.png"), b"\x89PNG\r\n\x1a\n").unwrap();
    assert_eq!(skipper::ensure(&cfg()).unwrap(), dir);
    assert!(dir.join("AGENTS.md").is_file() && dir.join("results").is_dir() && dir.join(".claude").symlink_metadata().is_ok());
    assert!(dir.join("cache/0123456789abcdef.png").is_file());
    // The fleet and project Skippers set up through the same rule.
    skipper::setup(&dir, "Skipper").unwrap();
    done(&home);
}

#[test]
fn a_folder_that_cannot_be_made_gives_one_line() {
    let _t = TURN.lock().unwrap_or_else(|e| e.into_inner());
    let home = world("unmade");
    // A file where a disk would be mounted: nothing can go under it.
    std::fs::write(home.join("disk"), "").unwrap();
    let to = home.join("disk/skiff-agent");
    config::set_skiff_agent(config::SkiffAgentKey::Folder(Some(&to))).unwrap();
    let line = format!("Skiff cannot make the folder {}.", to.display());
    assert_eq!(format!("{:#}", skipper::make_folder(&cfg()).unwrap_err()), line);
    assert_eq!(format!("{:#}", skiff_client::cache::ensure(&cfg()).unwrap_err()), line);
    // No fallback: the cache path stays in the folder.
    assert_eq!(skipper::cache_dir(&cfg()).unwrap(), to.join("cache"));
    // A refused override gives the same line.
    config::set_skiff_agent(config::SkiffAgentKey::Folder(Some(&home))).unwrap();
    assert_eq!(format!("{:#}", skipper::make_folder(&cfg()).unwrap_err()), format!("Skiff cannot make the folder {}.", home.display()));
    done(&home);
}
