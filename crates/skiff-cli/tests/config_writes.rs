//! The `skiff` binary's config writes, against a private projects.toml:
//! with a Host writer in another process, and Open with across the copy to
//! client.toml.
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

use skiff_core::config;

const SKIFF: &str = env!("CARGO_BIN_EXE_skiff");

/// The Host: runs only as a child of the tests below, and edits as
/// `skiff-host` does, through `config::edit_projects`. It marks `inside` once
/// it holds the lock and writes after `release`. Exit 0: written; 3: stale.
#[test]
fn host_child() {
    let Ok(barrier) = std::env::var("SKIFF_HOST_BARRIER") else { return };
    let barrier = PathBuf::from(barrier);
    let revision = std::env::var("SKIFF_HOST_REVISION").unwrap();
    let out = config::edit_projects(&revision, || {
        std::fs::write(barrier.join("inside"), "")?;
        let start = Instant::now();
        while !barrier.join("release").exists() {
            anyhow::ensure!(start.elapsed() < Duration::from_secs(30), "no release");
            std::thread::sleep(Duration::from_millis(20));
        }
        config::set_project_color("app", "#111111")
    });
    match out {
        Ok(_) => std::process::exit(0),
        Err(e) if e.is::<config::Stale>() => std::process::exit(3),
        Err(e) => {
            eprintln!("refused: {e:#}");
            std::process::exit(1)
        }
    }
}

struct Dir(PathBuf);
impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn dir(tag: &str) -> Dir {
    let d = std::env::temp_dir().join(format!("skiff-cli-config-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(d.join("barrier")).unwrap();
    Dir(d.canonicalize().unwrap())
}

/// The CLI with only private state: its own config, home and socket.
fn skiff(d: &Path, config: &Path) -> Command {
    let mut cmd = Command::new(SKIFF);
    cmd.arg("--socket")
        .arg(d.join("skiffd.sock"))
        .env("SKIFF_CONFIG", config)
        .env("HOME", d)
        .env("XDG_CONFIG_HOME", d.join("xdg"))
        .env_remove("SKIFF_SESSION");
    cmd
}

fn ok(out: Output) -> String {
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).unwrap()
}

fn host(d: &Path, config: &Path, revision: &str) -> std::process::Child {
    Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "host_child", "--nocapture", "--test-threads=1"])
        .env("SKIFF_CONFIG", config)
        .env("SKIFF_HOST_BARRIER", d.join("barrier"))
        .env("SKIFF_HOST_REVISION", revision)
        .env("HOME", d)
        .spawn()
        .unwrap()
}

/// The Host edits projects.toml by its real name and the CLI by a link to
/// it: the CLI waits for the Host's edit, and both changes are kept. A Host
/// edit made against the projects before the CLI's change is then stale.
#[test]
fn the_cli_and_the_host_take_one_lock_through_a_link() {
    let d = dir("shared");
    std::fs::create_dir_all(d.0.join("host")).unwrap();
    let real = d.0.join("host").join("projects.toml");
    std::fs::write(&real, "[[project]]\nname = \"app\"\npath = \"/private/fixture\"\ncolor = \"#000000\"\n").unwrap();
    let alias = d.0.join("alias.toml");
    std::os::unix::fs::symlink(&real, &alias).unwrap();
    let theme = skiff_core::theme::builtin_ids().into_iter().next().unwrap();
    let rev = config::projects_revision(&config::load_from(&real).unwrap().projects);

    let h = host(&d.0, &real, &rev);
    let start = Instant::now();
    while !d.0.join("barrier").join("inside").exists() {
        assert!(start.elapsed() < Duration::from_secs(30), "the Host never took the lock");
        std::thread::sleep(Duration::from_millis(20));
    }
    let mut cli = skiff(&d.0, &alias).args(["theme", "project", "set", "app", &theme]).spawn().unwrap();
    std::thread::sleep(Duration::from_millis(800));
    assert!(cli.try_wait().unwrap().is_none(), "the CLI wrote while the Host held the lock");
    std::fs::write(d.0.join("barrier").join("release"), "").unwrap();
    assert_eq!(h.wait_with_output().unwrap().status.code(), Some(0));
    assert!(cli.wait().unwrap().success());
    let p = config::load_from(&real).unwrap().projects.remove(0);
    assert_eq!((p.color.as_deref(), p.theme.as_deref()), (Some("#111111"), Some(theme.as_str())));
    assert!(std::fs::symlink_metadata(&alias).unwrap().file_type().is_symlink(), "the link stays a link");

    // The CLI changed the projects: a Host edit read before it is refused.
    let before = config::projects_revision(&config::load_from(&real).unwrap().projects);
    ok(skiff(&d.0, &alias).args(["theme", "project", "clear", "app"]).output().unwrap());
    let text = std::fs::read_to_string(&real).unwrap();
    assert_eq!(host(&d.0, &real, &before).wait().unwrap().code(), Some(3));
    assert_eq!(std::fs::read_to_string(&real).unwrap(), text);
}

fn rows(d: &Path, config: &Path) -> Vec<(String, Option<String>, String)> {
    let out = ok(skiff(d, config).args(["open-with", "ls", "--json"]).output().unwrap());
    let rows: Vec<serde_json::Value> = serde_json::from_str(&out).unwrap();
    rows.into_iter()
        .map(|r| {
            (
                r["key"].as_str().unwrap().to_string(),
                r["command"].as_str().map(String::from),
                r["opens"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

fn row(d: &Path, config: &Path, key: &str) -> (Option<String>, String) {
    let (_, command, opens) = rows(d, config).into_iter().find(|r| r.0 == key).unwrap();
    (command, opens)
}

/// `open-with ls` shows what `set` and `clear` wrote: projects.toml's
/// `[open]` before the first client setting, client.toml after it.
/// projects.toml keeps its bytes for Skiff 1.
#[test]
fn open_with_lists_what_it_set_before_and_after_the_copy() {
    let d = dir("open");
    let file = d.0.join("projects.toml");
    let source = "# mine\n[open]\ntext = \"old-editor {file}\"\nmarkdown = \"md {file}\"\npeek = [\"markdown\"]\n\n[[project]]\nname = \"app\"\npath = \"/private/fixture\"\n";
    std::fs::write(&file, source).unwrap();
    let client = d.0.join("client.toml");

    // Before the copy: projects.toml, and reading writes nothing.
    assert_eq!(row(&d.0, &file, "text"), (Some("old-editor {file}".into()), "app".into()));
    assert_eq!(row(&d.0, &file, "markdown"), (Some("md {file}".into()), "peek".into()));
    assert_eq!(row(&d.0, &file, "diff"), (None, "peek".into()));
    assert!(!client.exists());

    // Set: the copy, then the change, and the list shows the change.
    ok(skiff(&d.0, &file).args(["open-with", "set", "text", "new-editor {file}", "--peek"]).output().unwrap());
    assert!(client.exists());
    assert_eq!(row(&d.0, &file, "text"), (Some("new-editor {file}".into()), "peek".into()));
    assert_eq!(row(&d.0, &file, "markdown"), (Some("md {file}".into()), "peek".into()), "copied");

    // Where it opens, alone.
    ok(skiff(&d.0, &file).args(["open-with", "set", "markdown", "--app"]).output().unwrap());
    ok(skiff(&d.0, &file).args(["open-with", "set", "html", "--peek"]).output().unwrap());
    assert_eq!(row(&d.0, &file, "markdown"), (Some("md {file}".into()), "app".into()));
    assert_eq!(row(&d.0, &file, "html"), (None, "peek".into()));

    // Clear: the default, not projects.toml's old command.
    ok(skiff(&d.0, &file).args(["open-with", "clear", "text"]).output().unwrap());
    assert_eq!(row(&d.0, &file, "text"), (None, "peek".into()));
    ok(skiff(&d.0, &file).args(["open-with", "set", "text", "--app"]).output().unwrap());
    assert_eq!(row(&d.0, &file, "text"), (None, "app".into()));

    // The table form reads the same file.
    let table = ok(skiff(&d.0, &file).args(["open-with", "ls"]).output().unwrap());
    assert!(!table.contains("old-editor"), "{table}");
    assert!(table.contains("md {file}"), "{table}");

    // The desktop reads the same values.
    let open = config::client::load_from(&file, &client).unwrap().open;
    assert_eq!((open.text, open.markdown.as_deref(), open.peek), (None, Some("md {file}"), vec!["html".to_string()]));
    assert_eq!(std::fs::read_to_string(&file).unwrap(), source);
}
