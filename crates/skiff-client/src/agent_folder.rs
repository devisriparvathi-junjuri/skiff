//! Settings › Skippers › Folder (#135): choose where the Skiff agent's
//! folder is. The person picks where; the folder there decides what Skiff
//! does:
//!
//! - a new or empty folder: copy the current folder into it, or start empty;
//! - a Skiff folder (it has the marker): switch to it. No copy, no merge;
//! - anything else is refused, with one line that says why.
//!
//! A "move" is a copy, a check and a switch. The copy is built in a staging
//! folder beside the target, checked against the old folder, renamed into
//! place, and only then does projects.toml change. The old folder keeps every
//! file and gets `MOVED.md`. Skiff never deletes it. A failure before the
//! config write leaves projects.toml and the old folder as they were, and
//! removes what this change made.
//!
//! The app and `skiff skipper --move-to` share this code. Both end Skipper's
//! session first: it works in the old folder.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    path::{Path, PathBuf},
};

use anyhow::{bail, Context, Result};
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use skiff_core::{
    config::{self, Config, SkiffAgentKey},
    session::SessionInfo,
};

use crate::{
    skipper::{self, real, MARKER},
    Client,
};

/// In a staging folder's name: `<target>.skiff-new.<pid>`.
pub const STAGING: &str = ".skiff-new.";

/// The note a copy leaves in the old folder.
pub const MOVED: &str = "MOVED.md";

/// The ledger and the files SQLite keeps beside it. The ledger is copied with
/// SQLite, never as a file.
const LEDGER: [&str; 4] = ["ledger.db", "ledger.db-wal", "ledger.db-shm", "ledger.db-journal"];

/// What the folder at the target is.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// Skipper uses it now.
    Current,
    /// Nothing is there yet.
    New,
    /// An empty folder.
    Empty,
    /// A Skiff folder: it has the marker.
    Skiff,
    /// See [`Preview::refused`].
    Refused,
}

/// What a change does.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// Copy the current folder into a new or empty folder, then switch.
    Copy,
    /// Set up a new or empty folder with a new ledger, then switch.
    Empty,
    /// Switch to a Skiff folder as it is.
    Switch,
}

/// A folder's contents, for the preview and the done step.
#[derive(Serialize, Debug, Clone, Default, PartialEq, Eq)]
pub struct Counts {
    /// Rows in the ledger, plans included.
    pub tasks: u64,
    /// Rows that are not done.
    pub open: u64,
    pub done: u64,
    /// Files under `results/`, and their size in bytes.
    pub results: u64,
    pub bytes: u64,
    /// `AGENTS.md` differs from the template.
    pub agents_edited: bool,
    /// Top-level files and folders the person added.
    pub other: Vec<String>,
}

/// An open task whose pane still runs: its child writes in the old folder.
#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct Running {
    pub uid: String,
    pub task: String,
    pub pane_id: String,
}

/// Everything the dialog shows before anything changes.
#[derive(Serialize, Debug, Clone)]
pub struct Preview {
    /// The folder Skipper uses now, as projects.toml names it.
    pub from: PathBuf,
    /// The chosen folder: absolute and canonical. Only this is stored.
    pub to: PathBuf,
    pub kind: Kind,
    /// Why the target cannot be used, in one line.
    pub refused: Option<String>,
    /// The current folder is a Skiff folder, so Copy can run.
    pub can_copy: bool,
    /// Why Copy cannot run there, when Start empty still can (free space).
    pub copy_refused: Option<String>,
    /// The current folder's contents. `None` when it is not there.
    pub from_counts: Option<Counts>,
    /// The target's contents, for a Skiff folder.
    pub to_counts: Option<Counts>,
    /// `MOVED.md` in the target: Skipper moved away from it before.
    pub moved: Option<String>,
    /// Open tasks in the current ledger whose pane still runs.
    pub running: Vec<Running>,
    /// `SKIFF_LEDGER`, when set: the ledger commands use that file, and the
    /// change does not touch it.
    pub ledger_env: Option<String>,
}

/// What a change did.
#[derive(Serialize, Debug, Clone)]
pub struct Done {
    pub from: PathBuf,
    pub to: PathBuf,
    pub mode: Mode,
    /// What the copy carried, checked against the old folder.
    pub copied: Option<Counts>,
    /// The note in the old folder, when the copy left one.
    pub moved_note: Option<PathBuf>,
    /// A step after the switch that did not work. The switch stands.
    pub warning: Option<String>,
}

impl Counts {
    pub fn to_host(&self) -> skiff_host_api::SkipperFolderCounts {
        skiff_host_api::SkipperFolderCounts {
            tasks: self.tasks,
            open: self.open,
            done: self.done,
            results: self.results,
            bytes: self.bytes,
            agents_edited: self.agents_edited,
            other: self.other.clone(),
        }
    }
}

impl Preview {
    /// The preview as a Host answers it. `skipper_runs`: its session runs.
    pub fn to_host(&self, skipper_runs: bool) -> skiff_host_api::SkipperFolderPreview {
        let kind = match self.kind {
            Kind::Current => "current",
            Kind::New => "new",
            Kind::Empty => "empty",
            Kind::Skiff => "skiff",
            Kind::Refused => "refused",
        };
        skiff_host_api::SkipperFolderPreview {
            from: self.from.display().to_string(),
            to: self.to.display().to_string(),
            kind: kind.into(),
            refused: self.refused.clone(),
            can_copy: self.can_copy,
            copy_refused: self.copy_refused.clone(),
            from_counts: self.from_counts.as_ref().map(Counts::to_host),
            to_counts: self.to_counts.as_ref().map(Counts::to_host),
            moved: self.moved.clone(),
            running: self
                .running
                .iter()
                .map(|r| skiff_host_api::SkipperRunningTask { uid: r.uid.clone(), task: r.task.clone(), pane_id: r.pane_id.clone() })
                .collect(),
            ledger_env: self.ledger_env.clone(),
            skipper_runs,
        }
    }
}

impl Done {
    pub fn to_host(&self) -> skiff_host_api::SkipperFolderDone {
        skiff_host_api::SkipperFolderDone {
            from: self.from.display().to_string(),
            to: self.to.display().to_string(),
            mode: self.mode.into(),
            copied: self.copied.as_ref().map(Counts::to_host),
            moved_note: self.moved_note.as_ref().map(|p| p.display().to_string()),
            warning: self.warning.clone(),
        }
    }
}

impl From<Mode> for skiff_host_api::SkipperFolderMode {
    fn from(m: Mode) -> Self {
        match m {
            Mode::Copy => Self::Copy,
            Mode::Empty => Self::Empty,
            Mode::Switch => Self::Switch,
        }
    }
}

impl From<skiff_host_api::SkipperFolderMode> for Mode {
    fn from(m: skiff_host_api::SkipperFolderMode) -> Self {
        use skiff_host_api::SkipperFolderMode as M;
        match m {
            M::Copy => Self::Copy,
            M::Empty => Self::Empty,
            M::Switch => Self::Switch,
        }
    }
}

/// The live panes: id to generation. A task runs while its pane is there and
/// has the generation the send saw.
pub type Panes = HashMap<String, Option<i64>>;

/// The live panes, and Skipper's session when it runs.
pub async fn sessions(c: &Client) -> Result<(Panes, Option<SessionInfo>)> {
    let all = c.list_sessions().await?;
    let panes = all.iter().map(|s| (s.id.clone(), s.generation().and_then(|g| i64::try_from(g).ok()))).collect();
    let skipper = all.into_iter().find(|s| s.coordinator);
    Ok((panes, skipper))
}

/// A chosen folder as an absolute, canonical path. `~` is the home folder.
/// A relative path resolves against `base`: the home folder in the app, the
/// shell's working folder in the CLI. Links resolve now, never later.
pub fn resolve(input: &str, base: &Path, home: &Path) -> Result<PathBuf> {
    let s = input.trim();
    if s.is_empty() {
        bail!("Type a folder.");
    }
    let p = if s == "~" {
        home.to_path_buf()
    } else if let Some(rest) = s.strip_prefix("~/") {
        home.join(rest)
    } else if Path::new(s).is_absolute() {
        PathBuf::from(s)
    } else {
        base.join(s)
    };
    if !p.is_absolute() {
        bail!("{} is not a full path.", p.display());
    }
    Ok(real(&p))
}

/// Reads the target and says what a change to it would do. Changes nothing.
pub fn preview(cfg: &Config, to: &Path, panes: &Panes) -> Result<Preview> {
    let home = skipper::home()?;
    let from = skipper::configured(cfg)?;
    let to = real(to);
    let name = skipper::name(cfg);
    let real_from = real(&from);
    let can_copy = real_from.join(MARKER).is_file();
    let from_counts = real_from.is_dir().then(|| counts(&real_from, &name));
    let ledger = real_from.join("ledger.db");
    let running = if ledger.is_file() { running(&ledger, panes)? } else { Vec::new() };
    let moved = std::fs::read_to_string(to.join(MOVED)).ok();
    let mut p = Preview {
        from,
        to: to.clone(),
        kind: Kind::Refused,
        refused: None,
        can_copy,
        copy_refused: None,
        from_counts,
        to_counts: None,
        moved,
        running,
        ledger_env: std::env::var_os("SKIFF_LEDGER").map(|v| v.to_string_lossy().into_owned()),
    };
    if to == real_from {
        p.kind = Kind::Current;
        return Ok(p);
    }
    p.kind = kind(&to)?;
    p.refused = refusal(&to, &real_from, &home, cfg);
    if p.refused.is_some() {
        p.kind = Kind::Refused;
        return Ok(p);
    }
    if p.kind == Kind::Skiff {
        p.to_counts = Some(counts(&to, &name));
    } else if can_copy {
        p.copy_refused = space(&to, &real_from);
    }
    Ok(p)
}

/// What is at `to`, before the rules. A file or a folder with other files
/// is `Refused`.
pub fn kind(to: &Path) -> Result<Kind> {
    Ok(match to.symlink_metadata() {
        Err(_) => Kind::New,
        Ok(m) if !m.is_dir() => Kind::Refused,
        Ok(_) if to.join(MARKER).exists() => Kind::Skiff,
        Ok(_) if std::fs::read_dir(to).with_context(|| format!("read {}", to.display()))?.next().is_none() => Kind::Empty,
        Ok(_) => Kind::Refused,
    })
}

/// Why `to` cannot be the folder: the shared rules, then the ones for a change.
fn refusal(to: &Path, from: &Path, home: &Path, cfg: &Config) -> Option<String> {
    if to.to_str().is_none() {
        return Some("Skiff stores folder names in UTF-8 only. Choose another folder.".into());
    }
    if let Some(why) = skipper::refusal(to, home, cfg) {
        return Some(why);
    }
    if to.starts_with(from) {
        return Some("This folder is inside the current Skiff folder. A copy cannot go into itself.".into());
    }
    if from.starts_with(to) {
        return Some("This folder holds the current Skiff folder. Choose a folder of its own.".into());
    }
    match to.symlink_metadata() {
        Ok(m) if !m.is_dir() => return Some("This is a file. Choose a folder.".into()),
        Ok(_) if kind(to).ok() == Some(Kind::Refused) => {
            return Some("This folder holds other files. Choose a new or empty folder, or a Skiff folder.".into())
        }
        _ => {}
    }
    let parent = existing(to.parent().unwrap_or(to));
    if !writable(&parent) {
        return Some(format!("You cannot write in {}. Choose another folder.", parent.display()));
    }
    None
}

/// The nearest folder at or above `p` that exists.
fn existing(p: &Path) -> PathBuf {
    p.ancestors().find(|a| a.is_dir()).unwrap_or(Path::new("/")).to_path_buf()
}

fn writable(p: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(c) = std::ffi::CString::new(p.as_os_str().as_bytes()) else { return false };
    // SAFETY: a plain syscall on a valid C string.
    unsafe { libc::access(c.as_ptr(), libc::W_OK) == 0 }
}

/// Free bytes on the volume that holds `p`.
fn free(p: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(p.as_os_str().as_bytes()).ok()?;
    // SAFETY: statvfs fills the struct it is given.
    unsafe {
        let mut s: libc::statvfs = std::mem::zeroed();
        (libc::statvfs(c.as_ptr(), &mut s) == 0).then(|| s.f_bavail as u64 * s.f_frsize as u64)
    }
}

/// Refuses a copy when the volume has less than its size plus 10 %.
fn space(to: &Path, from: &Path) -> Option<String> {
    let need = manifest(from).ok()?.values().map(|e| if let Entry::File(n) = e { *n } else { 0 }).sum::<u64>()
        + std::fs::metadata(from.join("ledger.db")).map(|m| m.len()).unwrap_or(0);
    let need = need + need / 10;
    let has = free(&existing(to))?;
    (has < need).then(|| format!("The volume has {} free. The copy needs {}.", size(has), size(need)))
}

/// Bytes as the dialog shows them.
pub fn size(n: u64) -> String {
    match n {
        n if n < 1024 => format!("{n} B"),
        n if n < 1024 * 1024 => format!("{:.1} KB", n as f64 / 1024.0),
        n if n < 1024 * 1024 * 1024 => format!("{:.1} MB", n as f64 / (1024.0 * 1024.0)),
        n => format!("{:.1} GB", n as f64 / (1024.0 * 1024.0 * 1024.0)),
    }
}

/// The folder's ledger, results and the person's files.
pub fn counts(dir: &Path, name: &str) -> Counts {
    let mut c = Counts::default();
    if let Ok(db) = read_only(&dir.join("ledger.db")) {
        let n = |sql: &str| db.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap_or(0) as u64;
        c.tasks = n("SELECT count(*) FROM tasks");
        c.done = n("SELECT count(*) FROM tasks WHERE state = 'done'");
        c.open = c.tasks - c.done;
    }
    if let Ok(m) = manifest(dir) {
        for (p, e) in &m {
            if let (true, Entry::File(n)) = (p.starts_with("results/"), e) {
                c.results += 1;
                c.bytes += n;
            }
        }
        c.other = m
            .keys()
            .filter(|p| !p.contains('/'))
            .filter(|p| !["AGENTS.md", "CLAUDE.md", "GEMINI.md", ".claude", ".agents", "results", "ledger.md.bak"].contains(&p.as_str()))
            .cloned()
            .collect();
    }
    c.agents_edited = std::fs::read_to_string(dir.join("AGENTS.md")).is_ok_and(|t| t != skipper::template(name));
    c
}

/// Opens a ledger to read. It opens read-write, never creates, and refuses
/// writes: a read-only open of a WAL ledger leaves `-wal` and `-shm` files
/// behind, and a read-write one removes them on close, as `skiff ledger` does.
fn read_only(file: &Path) -> Result<Connection> {
    if !file.is_file() {
        bail!("{} is not there", file.display());
    }
    let db = Connection::open_with_flags(file, OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX)
        .with_context(|| format!("open {}", file.display()))?;
    db.busy_timeout(std::time::Duration::from_secs(5))?;
    db.pragma_update(None, "query_only", true)?;
    Ok(db)
}

fn has_column(db: &Connection, column: &str) -> Result<bool> {
    Ok(db.query_row("SELECT count(*) FROM pragma_table_info('tasks') WHERE name = ?1", [column], |r| r.get::<_, i64>(0))? > 0)
}

/// A row's task still runs: it is sent or blocked, its pane is there, and
/// the pane did not restart since the send. Rows from `ledger.md` can hold
/// a short pane id.
fn runs(state: &str, pane: Option<&str>, gen: Option<i64>, panes: &Panes) -> bool {
    let Some(pane) = pane.filter(|p| !p.is_empty()) else { return false };
    if !matches!(state, "sent" | "blocked") {
        return false;
    }
    let live = panes.get(pane).or_else(|| panes.iter().find(|(id, _)| id.starts_with(pane)).map(|(_, g)| g));
    match live {
        None => false,
        Some(now) => !(gen.is_some() && now.is_some() && *now != gen),
    }
}

/// The open tasks in `ledger` whose pane still runs.
fn running(ledger: &Path, panes: &Panes) -> Result<Vec<Running>> {
    let db = read_only(ledger)?;
    let uid = if has_column(&db, "uid")? { "uid" } else { "CAST(id AS TEXT)" };
    let gen = if has_column(&db, "pane_gen")? { "pane_gen" } else { "NULL" };
    let mut q = db.prepare(&format!("SELECT {uid}, task, state, pane_id, {gen} FROM tasks WHERE state IN ('sent', 'blocked')"))?;
    let rows = q.query_map([], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, Option<String>>(3)?, r.get::<_, Option<i64>>(4)?))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (uid, task, state, pane, gen) = row?;
        if runs(&state, pane.as_deref(), gen, panes) {
            out.push(Running { uid, task, pane_id: pane.unwrap_or_default() });
        }
    }
    Ok(out)
}

/// One entry of a folder, for the check.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Entry {
    Dir,
    File(u64),
    Link(PathBuf),
}

/// What a copy carries, by path from `dir`. The template links, the marker
/// and `MOVED.md` are left out: setup makes the links and the marker again,
/// so the skill link points at the current copy of the skill. The ledger is
/// left out: it is copied with SQLite.
fn manifest(dir: &Path) -> Result<BTreeMap<String, Entry>> {
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out)?;
    Ok(out)
}

fn skipped(rel: &str, link: bool) -> bool {
    [MARKER, MOVED].contains(&rel)
        || LEDGER.contains(&rel)
        // The image cache stays in the old folder: live sessions typed its paths.
        || rel == "cache"
        || (link && ["CLAUDE.md", "GEMINI.md", ".claude", ".agents/skills/skiff-cli"].contains(&rel))
}

fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, Entry>) -> Result<()> {
    let mut entries: Vec<_> = std::fs::read_dir(dir).with_context(|| format!("read {}", dir.display()))?.collect::<Result<_, _>>()?;
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let p = e.path();
        let rel = p.strip_prefix(root).unwrap().to_string_lossy().into_owned();
        let m = p.symlink_metadata().with_context(|| format!("read {}", p.display()))?;
        if skipped(&rel, m.file_type().is_symlink()) {
            continue;
        }
        if m.file_type().is_symlink() {
            out.insert(rel, Entry::Link(std::fs::read_link(&p)?));
        } else if m.is_dir() {
            out.insert(rel, Entry::Dir);
            walk(root, &p, out)?;
        } else if m.is_file() {
            out.insert(rel, Entry::File(m.len()));
        }
        // Sockets and pipes are not copied, and not counted.
    }
    Ok(())
}

/// Copies what [`manifest`] lists: files with their modes, links as links.
fn copy_tree(from: &Path, to: &Path) -> Result<()> {
    for (rel, e) in manifest(from)? {
        let (src, dst) = (from.join(&rel), to.join(&rel));
        match e {
            Entry::Dir => {
                std::fs::create_dir(&dst).with_context(|| format!("create {}", dst.display()))?;
                std::fs::set_permissions(&dst, src.metadata()?.permissions())?;
            }
            Entry::File(_) => {
                std::fs::copy(&src, &dst).with_context(|| format!("copy {}", src.display()))?;
            }
            Entry::Link(target) => {
                std::os::unix::fs::symlink(&target, &dst).with_context(|| format!("link {}", dst.display()))?;
            }
        }
    }
    Ok(())
}

/// Copies the ledger with SQLite, so a write at the same time leaves a
/// consistent copy, and leaves the old file as it is. In the copy, a result
/// path or a work folder in the old folder moves to the new one, except for a task that still
/// runs: its child writes the old path.
fn copy_ledger(src: &Path, dst: &Path, old: &[PathBuf], new: &Path, panes: &Panes) -> Result<()> {
    let from = read_only(src)?;
    let dst_text = dst.to_str().context("the folder name is not UTF-8")?;
    // VACUUM INTO writes only the new file, but query_only refuses it.
    from.pragma_update(None, "query_only", false)?;
    from.execute("VACUUM INTO ?1", [dst_text]).with_context(|| format!("copy {}", src.display()))?;
    let copy = Connection::open(dst).with_context(|| format!("open {}", dst.display()))?;
    let gen = if has_column(&copy, "pane_gen")? { "pane_gen" } else { "NULL" };
    type Row = (i64, String, String, Option<String>, Option<i64>);
    let tx = copy.unchecked_transaction()?;
    // The result path, then the work folder (from ledger version 11), the same way.
    let cols: &[&str] = if has_column(&tx, "work_dir")? { &["result_path", "work_dir"] } else { &["result_path"] };
    for col in cols {
        let rows: Vec<Row> = tx
            .prepare(&format!("SELECT id, {col}, state, pane_id, {gen} FROM tasks WHERE {col} IS NOT NULL"))?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?
            .collect::<Result<_, _>>()?;
        for (id, path, state, pane, g) in rows {
            if runs(&state, pane.as_deref(), g, panes) {
                continue;
            }
            let moved = old.iter().find_map(|o| Path::new(&path).strip_prefix(o).ok().map(|rest| new.join(rest)));
            if let Some(p) = moved {
                tx.execute(&format!("UPDATE tasks SET {col} = ?2 WHERE id = ?1"), rusqlite::params![id, p.to_string_lossy()])?;
            }
        }
    }
    tx.commit()?;
    Ok(())
}

/// The copy of the ledger is whole: SQLite's integrity check passes, and it
/// has the same rows, by uid, as the old one.
fn check_ledger(src: &Path, copy: &Path) -> Result<u64> {
    let (a, b) = (read_only(src)?, read_only(copy)?);
    let ok: String = b.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
    if ok != "ok" {
        bail!("the copied ledger fails its integrity check: {ok}");
    }
    let ids = |db: &Connection| -> Result<BTreeSet<String>> {
        let col = if has_column(db, "uid")? { "uid" } else { "CAST(id AS TEXT)" };
        Ok(db.prepare(&format!("SELECT {col} FROM tasks"))?.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?)
    };
    let (x, y) = (ids(&a)?, ids(&b)?);
    if x != y {
        bail!("the copied ledger has {} rows, the old one {}", y.len(), x.len());
    }
    Ok(x.len() as u64)
}

/// The steps of a change, in order. A test can make one fail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Stage,
    Copy,
    Ledger,
    Check,
    Setup,
    Rename,
    Config,
    Moved,
}

impl Step {
    fn says(self) -> &'static str {
        match self {
            Step::Stage => "make the staging folder",
            Step::Copy => "copy the files",
            Step::Ledger => "copy the ledger",
            Step::Check => "check the copy",
            Step::Setup => "set up the new folder",
            Step::Rename => "rename the copy into place",
            Step::Config => "write projects.toml",
            Step::Moved => "write MOVED.md in the old folder",
        }
    }
}

#[doc(hidden)]
pub static FAIL_AT: std::sync::Mutex<Option<Step>> = std::sync::Mutex::new(None);

/// Runs one step. The error names the step and the path.
fn step<T>(s: Step, path: &Path, f: impl FnOnce() -> Result<T>) -> Result<T> {
    let failed = *FAIL_AT.lock().unwrap() == Some(s);
    let out = if failed { Err(anyhow::anyhow!("failed on purpose")) } else { f() };
    out.with_context(|| format!("Could not {} ({})", s.says(), path.display()))
}

/// Changes the folder: `to` must be a path from [`resolve`]. Skipper's
/// session must have ended. Open tasks whose pane still runs stop the change
/// unless `anyway`: their child reports to the old ledger.
pub fn change(cfg: &Config, to: &Path, mode: Mode, panes: &Panes, anyway: bool) -> Result<Done> {
    let _lock = skipper::lock()?;
    // Read again under the lock: the folder can have changed since the preview.
    let p = preview(cfg, to, panes)?;
    if let Some(why) = &p.refused {
        bail!("{why}");
    }
    let ok = match (mode, p.kind) {
        (_, Kind::Current) => bail!("Admiral uses this folder now."),
        (Mode::Copy, Kind::New | Kind::Empty) => {
            if !p.can_copy {
                bail!("{} is not a Skiff folder, so there is nothing to copy. Start empty instead.", p.from.display());
            }
            if let Some(why) = &p.copy_refused {
                bail!("{why}");
            }
            true
        }
        (Mode::Empty, Kind::New | Kind::Empty) | (Mode::Switch, Kind::Skiff) => true,
        _ => false,
    };
    if !ok {
        bail!("{} is not a folder for this change. Choose the folder again.", p.to.display());
    }
    if !p.running.is_empty() && !anyway {
        let n = p.running.len();
        bail!("{n} task{} still run{} in {}. Switch when they are done.", if n == 1 { "" } else { "s" }, if n == 1 { "s" } else { "" }, p.from.display());
    }
    let name = skipper::name(cfg);
    let skill = skipper::sync_skill()?;
    let to = p.to.clone();
    let from = real(&p.from);
    let mut done = Done { from: p.from.clone(), to: to.clone(), mode, copied: None, moved_note: None, warning: None };

    if mode == Mode::Switch {
        step(Step::Setup, &to, || skipper::setup_in(&to, &name, &skill))?;
        step(Step::Config, &to, || write_folder(&to))?;
        // The note is false now: Skipper works here again.
        if p.moved.is_some() {
            if let Err(e) = std::fs::remove_file(to.join(MOVED)) {
                done.warning = Some(format!("Could not remove {}: {e}", to.join(MOVED).display()));
            }
        }
        return Ok(done);
    }

    let parent = to.parent().context("the folder has no parent")?.to_path_buf();
    let was_empty = p.kind == Kind::Empty;
    // Parents this change makes, outermost first, so a failure removes them.
    let made: Vec<PathBuf> = parent.ancestors().take_while(|a| !a.exists()).map(Path::to_path_buf).collect::<Vec<_>>().into_iter().rev().collect();
    let staging = parent.join(format!("{}{STAGING}{}", to.file_name().context("the folder has no name")?.to_string_lossy(), std::process::id()));
    let undo = |staged: bool| {
        if staged {
            let _ = std::fs::remove_dir_all(&staging);
        }
        for d in made.iter().rev() {
            let _ = std::fs::remove_dir(d);
        }
    };

    let built = (|| -> Result<Option<Counts>> {
        step(Step::Stage, &staging, || {
            std::fs::create_dir_all(&parent)?;
            remove_stale(&parent, to.file_name().unwrap());
            std::fs::create_dir(&staging)?;
            Ok(())
        })?;
        let mut copied = None;
        if mode == Mode::Copy {
            step(Step::Copy, &from, || copy_tree(&from, &staging))?;
            let ledger = from.join("ledger.db");
            let has_ledger = ledger.is_file();
            if has_ledger {
                let old = [p.from.clone(), from.clone()];
                step(Step::Ledger, &ledger, || copy_ledger(&ledger, &staging.join("ledger.db"), &old, &to, panes))?;
            }
            step(Step::Check, &staging, || {
                if manifest(&from)? != manifest(&staging)? {
                    bail!("the copied files differ from the old folder");
                }
                if has_ledger {
                    check_ledger(&ledger, &staging.join("ledger.db"))?;
                }
                Ok(())
            })?;
            copied = Some(counts(&staging, &name));
        }
        step(Step::Setup, &staging, || {
            // The staging folder is this change's: mark it, so setup fills it in.
            std::fs::write(staging.join(MARKER), "1\n")?;
            skipper::setup_in(&staging, &name, &skill)
        })?;
        step(Step::Rename, &to, || std::fs::rename(&staging, &to).map_err(Into::into))?;
        Ok(copied)
    })();
    match built {
        Ok(c) => done.copied = c,
        Err(e) => {
            undo(true);
            return Err(e);
        }
    }

    // projects.toml changes last. Until then the old folder is the folder.
    if let Err(e) = step(Step::Config, &to, || write_folder(&to)) {
        let _ = std::fs::remove_dir_all(&to);
        if was_empty {
            let _ = std::fs::create_dir(&to);
        }
        undo(false);
        return Err(e);
    }

    if mode == Mode::Copy {
        let note = from.join(MOVED);
        match step(Step::Moved, &note, || write_moved(&note, &to)) {
            Ok(()) => done.moved_note = Some(note),
            Err(e) => done.warning = Some(format!("{e:#}")),
        }
    }
    Ok(done)
}

/// `[skiff_agent] folder` = the canonical path. The default folder takes the
/// key out.
fn write_folder(to: &Path) -> Result<()> {
    let default = real(&skipper::default_folder()?);
    config::set_skiff_agent(SkiffAgentKey::Folder((to != default).then_some(to)))
}

fn write_moved(note: &Path, to: &Path) -> Result<()> {
    let date: String = Connection::open_in_memory()?.query_row("SELECT date('now')", [], |r| r.get(0))?;
    let text = format!(
        "# Admiral moved\n\nAdmiral moved to {} on {date}.\n\nThis folder is a copy kept by Skiff. Tasks added after that date are only in the new folder. You can delete this folder.\n\nTo go back, choose this folder in Settings › Skippers › Folder › Change….\n",
        to.display()
    );
    std::fs::write(note, text)?;
    Ok(())
}

/// Removes staging folders for `name` that a crashed change left. A folder
/// whose process still runs is another change's; it stays.
fn remove_stale(parent: &Path, name: &std::ffi::OsStr) {
    let prefix = format!("{}{STAGING}", name.to_string_lossy());
    let Ok(entries) = std::fs::read_dir(parent) else { return };
    for e in entries.flatten() {
        let n = e.file_name().to_string_lossy().into_owned();
        let Some(pid) = n.strip_prefix(&prefix).and_then(|p| p.parse::<i32>().ok()) else { continue };
        // SAFETY: signal 0 only asks whether the process is there.
        let alive = pid != std::process::id() as i32
            && unsafe { libc::kill(pid, 0) == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM) };
        if !alive {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
}
