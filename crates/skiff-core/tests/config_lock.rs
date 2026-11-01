//! The edit lock between processes, as the Host's `skiff-host` and the
//! `skiff` CLI take it: each writer here is this test binary run again as a
//! child, editing a private projects.toml through `config::edit_projects`.
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use skiff_core::config;

const SOURCE: &str = "[[project]]\nname = \"app\"\npath = \"/private/fixture\"\ncolor = \"#000000\"\n";

/// A writer: runs only as a child of the tests below. It edits under the
/// revision it is given, marks `inside-<name>` once it holds the lock, waits
/// for `release`, then writes. Exit 0: written; 3: stale; 1: refused.
#[test]
fn writer_child() {
    let Ok(barrier) = std::env::var("SKIFF_LOCK_BARRIER") else { return };
    let barrier = PathBuf::from(barrier);
    let name = std::env::var("SKIFF_LOCK_NAME").unwrap();
    let revision = std::env::var("SKIFF_LOCK_REVISION").unwrap();
    let out = config::edit_projects(&revision, || {
        std::fs::write(barrier.join(format!("inside-{name}")), "")?;
        let start = Instant::now();
        while !barrier.join("release").exists() {
            anyhow::ensure!(start.elapsed() < Duration::from_secs(30), "no release");
            std::thread::sleep(Duration::from_millis(20));
        }
        match std::env::var("SKIFF_LOCK_COLOR") {
            Ok(color) => config::set_project_color("app", &color),
            // A nested edit of another table, under the same lock.
            Err(_) => config::set_skiff_agent(config::SkiffAgentKey::Name(Some(&name))),
        }
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
    let d = std::env::temp_dir().join(format!("skiff-config-lock-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(d.join("barrier")).unwrap();
    Dir(d.canonicalize().unwrap())
}

fn revision(file: &Path) -> String {
    config::projects_revision(&config::load_from(file).unwrap().projects)
}

fn writer(d: &Path, config: &Path, name: &str, color: Option<&str>, revision: &str) -> Child {
    let mut cmd = Command::new(std::env::current_exe().unwrap());
    cmd.args(["--exact", "writer_child", "--nocapture", "--test-threads=1"])
        .env("SKIFF_CONFIG", config)
        .env("SKIFF_LOCK_BARRIER", d.join("barrier"))
        .env("SKIFF_LOCK_NAME", name)
        .env("SKIFF_LOCK_REVISION", revision)
        .env("HOME", d)
        .env("XDG_CONFIG_HOME", d.join("xdg"));
    match color {
        Some(c) => cmd.env("SKIFF_LOCK_COLOR", c),
        None => cmd.env_remove("SKIFF_LOCK_COLOR"),
    };
    cmd.spawn().unwrap()
}

fn wait_for(path: &Path) {
    let start = Instant::now();
    while !path.exists() {
        assert!(start.elapsed() < Duration::from_secs(30), "{} never appeared", path.display());
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn code(mut child: Child) -> i32 {
    child.wait().unwrap().code().unwrap_or(-1)
}

/// Two writers on one file, by the file's own name and by `alias`: the
/// second waits for the first and is then stale, so one color is written
/// and the other refused, never both answered as written.
fn second_writer_waits(tag: &str, alias: impl FnOnce(&Path, &Path) -> PathBuf) {
    let d = dir(tag);
    std::fs::create_dir_all(d.0.join("real")).unwrap();
    let real = d.0.join("real").join("projects.toml");
    std::fs::write(&real, SOURCE).unwrap();
    let other = alias(&d.0, &real);
    let rev = revision(&real);
    assert_eq!(revision(&other), rev, "{tag}: one file");

    let a = writer(&d.0, &real, "a", Some("#111111"), &rev);
    wait_for(&d.0.join("barrier").join("inside-a"));
    let b = writer(&d.0, &other, "b", Some("#222222"), &rev);
    std::thread::sleep(Duration::from_millis(800));
    assert!(!d.0.join("barrier").join("inside-b").exists(), "{tag}: b edited while a held the lock");
    std::fs::write(d.0.join("barrier").join("release"), "").unwrap();
    assert_eq!(code(a), 0, "{tag}: a writes");
    assert_eq!(code(b), 3, "{tag}: b is stale after a");
    assert!(!d.0.join("barrier").join("inside-b").exists(), "{tag}: b never ran its edit");

    let text = std::fs::read_to_string(&real).unwrap();
    assert!(text.contains("#111111") && !text.contains("#222222"), "{tag}: {text}");
    assert!(std::fs::symlink_metadata(&other).is_ok(), "{tag}: the alias stays");
    assert!(!d.0.join("real").join(".projects.toml.lock").exists());
    assert!(d.0.join("real").join("projects.toml.lock").exists(), "{tag}: the lock is beside the file");
}

#[test]
fn the_same_name_waits() {
    second_writer_waits("same", |_, real| real.to_path_buf());
}

#[test]
fn a_link_to_the_file_waits() {
    second_writer_waits("file-link", |d, real| {
        let alias = d.join("alias.toml");
        std::os::unix::fs::symlink(real, &alias).unwrap();
        alias
    });
}

#[test]
fn a_relative_link_through_another_link_waits() {
    second_writer_waits("chain", |d, _| {
        std::fs::create_dir_all(d.join("links")).unwrap();
        std::os::unix::fs::symlink("../real/projects.toml", d.join("links").join("one.toml")).unwrap();
        std::os::unix::fs::symlink("one.toml", d.join("links").join("two.toml")).unwrap();
        d.join("links").join("two.toml")
    });
}

#[test]
fn a_link_to_the_folder_waits() {
    second_writer_waits("dir-link", |d, _| {
        std::os::unix::fs::symlink(d.join("real"), d.join("linked")).unwrap();
        d.join("linked").join("projects.toml")
    });
}

/// No lock, no write: a folder where the lock goes refuses the edit, by
/// either name of the file, and leaves it as it was.
#[test]
fn a_lock_that_cannot_be_taken_refuses_the_write() {
    let d = dir("refused");
    let real = d.0.join("projects.toml");
    std::fs::write(&real, SOURCE).unwrap();
    let alias = d.0.join("alias.toml");
    std::os::unix::fs::symlink(&real, &alias).unwrap();
    std::fs::create_dir_all(d.0.join("projects.toml.lock")).unwrap();
    std::fs::write(d.0.join("barrier").join("release"), "").unwrap();
    let rev = revision(&real);
    for (name, file) in [("real", &real), ("alias", &alias)] {
        let out = writer(&d.0, file, name, Some("#333333"), &rev).wait_with_output().unwrap();
        assert_eq!(out.status.code(), Some(1), "{name}");
        assert!(!d.0.join("barrier").join(format!("inside-{name}")).exists(), "{name}: edited without the lock");
    }
    assert_eq!(std::fs::read_to_string(&real).unwrap(), SOURCE);
}

/// The first write into a folder that is not there yet: the folder and the
/// lock are made, and a nested edit of another table runs under the lock.
#[test]
fn a_first_write_makes_its_folder_and_lock() {
    let d = dir("first");
    let file = d.0.join("new").join("deep").join("projects.toml");
    std::fs::write(d.0.join("barrier").join("release"), "").unwrap();
    let child = writer(&d.0, &file, "first", None, &config::projects_revision(&[]));
    assert_eq!(code(child), 0);
    assert_eq!(config::load_from(&file).unwrap().skiff_agent.name.as_deref(), Some("first"));
    assert!(d.0.join("new").join("deep").join("projects.toml.lock").exists());
}
