use std::path::{Path, PathBuf};
use anyhow::{Context, Result};
use super::types::Config;

/// `SKIFF_CONFIG` overrides. Otherwise `<config dir>/skiff/projects.toml`,
/// or the socket's private copy (see [`crate::state`]).
pub fn config_path() -> PathBuf {
    if let Ok(p) = std::env::var("SKIFF_CONFIG") {
        return PathBuf::from(p);
    }
    match crate::state::private_dir() {
        Some(d) => d.join("config").join("projects.toml"),
        None => crate::state::real_config_path(),
    }
}

/// Held across each read-modify-write of the file, so two edits at once do
/// not drop one another.
static EDIT: std::sync::Mutex<()> = std::sync::Mutex::new(());

thread_local! {
    /// The files whose edit lock this thread holds, outermost first. A nested
    /// edit of the same file, such as one checked against a revision, runs
    /// under the outer lock.
    static HELD: std::cell::RefCell<Vec<PathBuf>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// The edit lock: the mutex for threads in this process, and a lock on
/// `projects.toml.lock` for other processes, such as the app and the `skiff` CLI.
pub(super) struct EditLock {
    _file: Option<std::fs::File>,
    _guard: Option<std::sync::MutexGuard<'static, ()>>,
}

impl Drop for EditLock {
    fn drop(&mut self) {
        HELD.with(|h| h.borrow_mut().pop());
    }
}

pub(super) fn edit_lock() -> Result<EditLock> {
    edit_lock_for(&config_path())
}

/// The edit lock, with the lock file beside the file `file` names, links
/// followed: every spelling of one file takes one lock. Taken again on a
/// thread that holds it for that file, it is already held; for another file,
/// that file's lock is taken under the mutex this thread already holds.
/// Without the lock file nothing is written: another process could write
/// over the edit.
pub(super) fn edit_lock_for(file: &Path) -> Result<EditLock> {
    let file = target(file)?;
    let (same, nested) = HELD.with(|h| {
        let h = h.borrow();
        (h.contains(&file), !h.is_empty())
    });
    if same {
        HELD.with(|h| h.borrow_mut().push(file));
        return Ok(EditLock { _file: None, _guard: None });
    }
    let guard = (!nested).then(|| EDIT.lock().unwrap_or_else(|e| e.into_inner()));
    let mut path = file.as_os_str().to_owned();
    path.push(".lock");
    let path = PathBuf::from(path);
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .with_context(|| format!("open the edit lock {}", path.display()))?;
    lock.lock().with_context(|| format!("take the edit lock {}", path.display()))?;
    HELD.with(|h| h.borrow_mut().push(file));
    Ok(EditLock { _file: Some(lock), _guard: guard })
}

/// The file `file` names, links followed, in its folder's canonical path:
/// the one name every spelling of it shares, whether or not the file exists
/// yet. Its folder is made if missing.
pub(super) fn target(file: &Path) -> Result<PathBuf> {
    let mut file = file.to_path_buf();
    let mut links = 0;
    while let Ok(to) = std::fs::read_link(&file) {
        links += 1;
        if links > 40 {
            anyhow::bail!("{}: too many links", file.display());
        }
        file = match file.parent() {
            Some(dir) => dir.join(to),
            None => to,
        };
    }
    let name = file
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("{} names no file", file.display()))?
        .to_owned();
    let dir = match file.parent() {
        Some(d) if !d.as_os_str().is_empty() => d.to_path_buf(),
        _ => PathBuf::from("."),
    };
    std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    let dir = dir.canonicalize().with_context(|| format!("resolve {}", dir.display()))?;
    Ok(dir.join(name))
}

/// The `[[project]]` tables, or the `[agents]` table, changed after they
/// were read: a Host write made against that read is refused, not applied
/// over the change. It holds what changed: `projects` or `agents`.
#[derive(Debug)]
pub struct Stale(&'static str);

impl std::fmt::Display for Stale {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "The {} changed on this Host since they were read.", self.0)
    }
}
impl std::error::Error for Stale {}

/// Why a project edit was refused, by kind, so the Host can answer with a
/// typed error. Its text is the message the person reads.
#[derive(Debug)]
pub enum Refused {
    /// The value is not one the key takes.
    Invalid(String),
    /// No project has that name.
    NoProject(String),
    /// The name or folder is already a project.
    Taken(String),
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Self::Invalid(m) | Self::Taken(m) => f.write_str(m),
            Self::NoProject(name) => write!(f, "no such project: {name}"),
        }
    }
}
impl std::error::Error for Refused {}

/// The projects as read, as a short token: equal tokens, equal `[[project]]`
/// tables. The rest of the file (keys, appearance, agents) does not count.
pub fn projects_revision(projects: &[super::types::ProjectConfig]) -> String {
    revision_of(projects)
}

/// The `[agents]` table as read, as a short token, as [`projects_revision`].
pub fn agents_revision(agents: &super::types::Agents) -> String {
    revision_of(agents)
}

fn revision_of<T: serde::Serialize + ?Sized>(value: &T) -> String {
    // FNV-1a: the same token from every build, so a restarted service agrees.
    let bytes = serde_json::to_vec(value).unwrap_or_default();
    let hash = bytes.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3));
    format!("{hash:016x}")
}

/// Longest name, fleet or theme id a project takes, in characters.
pub const LABEL_MAX: usize = 64;

/// A one-line label: trimmed, at most [`LABEL_MAX`] characters, no control characters.
pub fn check_label<'a>(what: &str, value: &'a str) -> Result<&'a str> {
    let value = value.trim();
    if value.chars().count() > LABEL_MAX || value.chars().any(char::is_control) {
        return Err(Refused::Invalid(format!("the {what} takes one line of at most {LABEL_MAX} characters")).into());
    }
    Ok(value)
}

/// Runs `edit` under the edit lock, only if the `[[project]]` tables are
/// still at `revision`; else [`Stale`] and nothing is written. Answers what
/// `edit` returns and the revision after it.
pub fn edit_projects<T>(revision: &str, edit: impl FnOnce() -> Result<T>) -> Result<(T, String)> {
    let _edit = edit_lock()?;
    if projects_revision(&load()?.projects) != revision {
        return Err(Stale("projects").into());
    }
    let out = edit()?;
    Ok((out, projects_revision(&load()?.projects)))
}

/// [`edit_projects`] for the `[agents]` table: `edit` runs only while it is
/// still at `revision`.
pub fn edit_agents_at<T>(revision: &str, edit: impl FnOnce() -> Result<T>) -> Result<(T, String)> {
    let _edit = edit_lock()?;
    if agents_revision(&load()?.agents) != revision {
        return Err(Stale("agents").into());
    }
    let out = edit()?;
    Ok((out, agents_revision(&load()?.agents)))
}

/// Writes a temp file beside the target and renames it over, so a reader
/// never sees a half-written file. Writes through a symlink to its target,
/// the file whose edit lock [`edit_lock_for`] takes.
pub fn write_atomic(file: &Path, text: &str) -> Result<()> {
    let file = target(file)?;
    let name = file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = file.with_file_name(format!(".{name}.{}.tmp", std::process::id()));
    let done = std::fs::write(&tmp, text)
        .with_context(|| format!("write {}", tmp.display()))
        .and_then(|_| std::fs::rename(&tmp, &file).with_context(|| format!("write {}", file.display())));
    if done.is_err() {
        // A half-written temp file never stays beside the config.
        let _ = std::fs::remove_file(&tmp);
    }
    done
}

/// A missing file is an empty config, not an error.
pub fn load() -> Result<Config> {
    load_from(&config_path())
}

pub fn load_from(path: &Path) -> Result<Config> {
    if !path.exists() {
        return Ok(Config::default());
    }
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("read {}", path.display()))?;
    parse(&text).with_context(|| format!("parse {}", path.display()))
}

pub fn parse(text: &str) -> Result<Config> {
    let mut cfg: Config = toml::from_str(text)?;
    for p in &mut cfg.projects {
        p.path = expand_home(&p.path);
    }
    if let Some(f) = &mut cfg.skiff_agent.folder {
        *f = expand_home(f);
    }
    Ok(cfg)
}

/// `[skiff_agent]` keys an older Skiff wrote. A read ignores them, and
/// every write takes them out: Skippers are always on.
pub(super) const DROPPED: [&str; 2] = ["enabled", "project_skippers"];

/// Writes `doc` to `file` as [`write_atomic`] does, without the
/// [`DROPPED`] keys, and without `instructions` in a `[skiff_agent.tasks]`
/// row: a type's file is `types/<type>.md` in the agent's folder now.
pub(super) fn write_doc(file: &Path, doc: &mut toml_edit::DocumentMut) -> Result<()> {
    if let Some(t) = doc.get_mut("skiff_agent").and_then(|t| t.as_table_like_mut()) {
        for k in DROPPED {
            t.remove(k);
        }
        if let Some(tasks) = t.get_mut("tasks").and_then(|t| t.as_table_like_mut()) {
            for (_, row) in tasks.iter_mut() {
                if let Some(row) = row.as_inline_table_mut() {
                    if row.remove("instructions").is_some() {
                        row.fmt();
                    }
                } else if let Some(row) = row.as_table_like_mut() {
                    row.remove("instructions");
                }
            }
        }
    }
    write_atomic(file, &doc.to_string())
}

/// Opens a top-level table in the config file, creating it above the projects.
pub(super) fn edit_table(name: &str, f: impl FnOnce(&mut toml_edit::Table) -> Result<()>) -> Result<()> {
    edit_table_in(&config_path(), name, f)
}

/// [`edit_table`] in another file of the same format, under its own lock.
pub(super) fn edit_table_in(file: &Path, name: &str, f: impl FnOnce(&mut toml_edit::Table) -> Result<()>) -> Result<()> {
    let _edit = edit_lock_for(file)?;
    let file = file.to_path_buf();
    let text = if file.exists() {
        std::fs::read_to_string(&file).with_context(|| format!("read {}", file.display()))?
    } else {
        String::new()
    };
    let mut doc: toml_edit::DocumentMut = text
        .parse()
        .with_context(|| format!("parse {}", file.display()))?;
    let created = !doc.contains_key(name);
    let item = doc
        .entry(name)
        .or_insert_with(|| toml_edit::Item::Table(toml_edit::Table::new()));
    let table = item
        .as_table_mut()
        .ok_or_else(|| anyhow::anyhow!("`{name}` in {} is not a table", file.display()))?;
    f(table)?;
    if created {
        // A new table goes above the [[project]] list, not after it.
        table.set_position(Some(-1));
    }
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    write_doc(&file, &mut doc)
}

/// `~/x` for paths under the home directory, so the file moves between machines.
pub(super) fn home_relative(p: &Path) -> String {
    match dirs::home_dir().and_then(|h| p.strip_prefix(h).ok().map(Path::to_path_buf)) {
        Some(rest) => format!("~/{}", rest.display()),
        None => p.display().to_string(),
    }
}

pub(super) fn expand_home(p: &Path) -> PathBuf {
    let Some(rest) = p.to_str().and_then(|s| s.strip_prefix("~/")) else {
        return p.to_path_buf();
    };
    match dirs::home_dir() {
        Some(home) => home.join(rest),
        None => p.to_path_buf(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every spelling of one file is one lock, taken again on this thread
    /// without waiting on itself. Separate processes: tests/config_lock.rs.
    #[test]
    fn a_link_and_its_file_share_one_reentrant_lock() {
        let d = std::env::temp_dir().join(format!("skiff-lock-unit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("real")).unwrap();
        let real = d.join("real").join("projects.toml");
        std::fs::write(&real, "").unwrap();
        let alias = d.join("alias.toml");
        std::os::unix::fs::symlink(&real, &alias).unwrap();
        assert_eq!(target(&alias).unwrap(), target(&real).unwrap());
        // Not yet there: its folder is made, and the name is the same.
        let fresh = d.join("new").join("deep").join("projects.toml");
        assert_eq!(target(&fresh).unwrap(), d.canonicalize().unwrap().join("new/deep/projects.toml"));

        let (tx, rx) = std::sync::mpsc::channel();
        let (r, a) = (real.clone(), alias.clone());
        std::thread::spawn(move || {
            let outer = edit_lock_for(&r).unwrap();
            let inner = edit_lock_for(&a).unwrap();
            drop(inner);
            drop(outer);
            tx.send(()).unwrap();
        });
        rx.recv_timeout(std::time::Duration::from_secs(10)).expect("a nested lock through a link waits on itself");
        assert!(d.join("real").join("projects.toml.lock").exists());
        assert!(!d.join("alias.toml.lock").exists());

        // A lock that cannot be made refuses the edit.
        let blocked = d.join("blocked.toml");
        std::fs::create_dir_all(d.join("blocked.toml.lock")).unwrap();
        assert!(edit_lock_for(&blocked).is_err());
        HELD.with(|h| assert!(h.borrow().is_empty()));
        std::fs::remove_dir_all(&d).unwrap();
    }
}
