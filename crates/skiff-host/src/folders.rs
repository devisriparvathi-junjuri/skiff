//! `folders@1`: the Host account's folders, for the folder picker. Folders
//! and links to folders only, never a file name, size or content. A git
//! repository is read from its own files; no git runs here, so no repo
//! config or hook in a listed folder runs either.
//!
//! The Recent list lives beside `projects.toml`. This service adds a folder
//! to it when a project is added there, and when it sees a session or group
//! of its skiffd in one. Clients only read it.
use std::{
    fs,
    io::{ErrorKind, Read},
    path::{Component, Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use skiff_host_api::{Error, FolderEntry, FolderKind, FolderListing, FolderPlaces, FolderRepo, PlaceRepo};

/// Entries one listing sends.
pub const MAX_ENTRIES: usize = 2000;
/// Entries one listing reads from disk, hidden ones and files included.
const READ_LIMIT: usize = 40_000;
/// Folders the Recent list keeps.
pub const MAX_RECENT: usize = 20;

/// The account's home folder, links resolved.
pub fn home() -> Result<String, Error> {
    let home = dirs::home_dir().ok_or_else(|| Error::NotFound { message: "This Host account has no home folder.".into() })?;
    real_dir(&home).map(|p| text(&p))
}

/// The folders in `path`, by name. See [`FolderListing`].
pub fn list(path: &str, hidden: bool, filter: Option<&str>) -> Result<FolderListing, Error> {
    let filter = filter.map(str::to_lowercase).filter(|f| !f.is_empty());
    let real = real_dir(&full_path(path)?)?;
    let dir = fs::read_dir(&real).map_err(|e| io_error(e, &real))?;
    let (mut entries, mut truncated) = (Vec::new(), false);
    for (read, entry) in dir.enumerate() {
        if read == READ_LIMIT {
            truncated = true;
            break;
        }
        let Ok(entry) = entry else { continue };
        // A name that is not UTF-8 cannot travel as text without changing.
        let Ok(name) = entry.file_name().into_string() else { continue };
        if (!hidden && name.starts_with('.')) || filter.as_ref().is_some_and(|f| !name.to_lowercase().contains(f)) {
            continue;
        }
        let kind = match entry.file_type() {
            Ok(t) if t.is_dir() => FolderKind::Dir,
            Ok(t) if t.is_symlink() && fs::metadata(entry.path()).is_ok_and(|m| m.is_dir()) => FolderKind::Link,
            _ => continue,
        };
        entries.push((name, kind));
    }
    entries.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()).then_with(|| a.0.cmp(&b.0)));
    if entries.len() > MAX_ENTRIES {
        entries.truncate(MAX_ENTRIES);
        truncated = true;
    }
    let entries = entries
        .into_iter()
        .map(|(name, kind)| {
            let repo = repo_state(&real.join(&name));
            FolderEntry { name, kind, repo }
        })
        .collect();
    Ok(FolderListing {
        path: path.to_string(),
        parent: real.parent().map(text),
        real: text(&real),
        entries,
        truncated,
    })
}

/// Makes the folder `name` in `parent`. Answers its real path.
pub fn create(parent: &str, name: &str) -> Result<String, Error> {
    check_name(name)?;
    let parent = real_dir(&full_path(parent)?)?;
    let made = parent.join(name);
    match fs::create_dir(&made) {
        Ok(()) => real_dir(&made).map(|p| text(&p)),
        Err(e) if e.kind() == ErrorKind::AlreadyExists => {
            Err(Error::Conflict { message: format!("{} has a folder with this name already.", file_name(&parent)) })
        }
        Err(e) => Err(io_error(e, &parent)),
    }
}

/// One folder name: no `/`, no NUL, not `.` or `..`, at most 255 bytes.
pub fn check_name(name: &str) -> Result<(), Error> {
    if name.is_empty() || name == "." || name == ".." || name.len() > 255 || name.contains(['/', '\0']) {
        return Err(Error::Invalid { message: "Type one folder name, with no /.".into() });
    }
    Ok(())
}

/// A full path the client names. Never resolved against this service's
/// working folder, and never `~`: the Host resolves home itself.
fn full_path(path: &str) -> Result<PathBuf, Error> {
    let p = Path::new(path);
    if !p.is_absolute() || path.chars().any(char::is_control) {
        return Err(Error::Invalid { message: format!("{path} is not a full folder path on this Host") });
    }
    Ok(p.to_path_buf())
}

/// `path` with every link resolved, when it is a folder.
fn real_dir(path: &Path) -> Result<PathBuf, Error> {
    let real = fs::canonicalize(path).map_err(|e| io_error(e, path))?;
    if !real.is_dir() {
        return Err(gone());
    }
    Ok(real)
}

fn gone() -> Error {
    Error::NotFound { message: "This folder is no longer on this Host.".into() }
}

fn io_error(e: std::io::Error, path: &Path) -> Error {
    match e.kind() {
        ErrorKind::NotFound | ErrorKind::NotADirectory => gone(),
        ErrorKind::PermissionDenied => {
            Error::Permission { message: format!("This Host does not let your account open {}.", file_name(path)) }
        }
        _ => Error::OperationFailed { message: format!("{}: {e}", path.display()) },
    }
}

fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(|| text(path), |n| n.to_string_lossy().into_owned())
}

fn text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// `dir`'s git state when it is a repository's top folder: from `.git`,
/// a folder, or a file that names one (a linked worktree). Read from the
/// files, bounded, never through git.
pub fn repo_state(dir: &Path) -> Option<FolderRepo> {
    let dot = dir.join(".git");
    let meta = fs::symlink_metadata(&dot).ok()?;
    let (gitdir, common) = if meta.is_dir() {
        (dot.clone(), dot)
    } else if meta.is_file() {
        let named = small(&dot)?;
        let gitdir = dir.join(named.strip_prefix("gitdir:")?.trim());
        let common = small(&gitdir.join("commondir")).map_or_else(|| gitdir.clone(), |c| gitdir.join(c.trim()));
        (gitdir, common)
    } else {
        return None;
    };
    let head = small(&gitdir.join("HEAD"))?;
    let branch = head.trim().strip_prefix("ref: refs/heads/").map(str::to_string);
    let moved = branch
        .as_deref()
        .filter(|b| Path::new(b).components().all(|c| matches!(c, Component::Normal(_))))
        .and_then(|b| modified(&common.join("refs/heads").join(b)))
        .or_else(|| modified(&gitdir.join("logs/HEAD")));
    Some(FolderRepo { branch, last_commit: moved })
}

/// A small text file: at most 4 KiB, never a link's target folder.
fn small(path: &Path) -> Option<String> {
    let mut out = String::new();
    fs::File::open(path).ok()?.take(4096).read_to_string(&mut out).ok()?;
    Some(out)
}

fn modified(path: &Path) -> Option<u64> {
    let at = fs::metadata(path).ok()?.modified().ok()?;
    Some(at.duration_since(UNIX_EPOCH).ok()?.as_secs())
}

/// Unix ms now.
pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

/// How far one repository scan goes.
#[derive(Debug, Clone, Copy)]
pub struct ScanBounds {
    /// Folder levels below the root: its own folders are level 1.
    pub depth: usize,
    pub time: std::time::Duration,
    pub repos: usize,
}
impl ScanBounds {
    /// What `folder_places` scans home with.
    pub const PLACES: Self = Self { depth: 4, time: std::time::Duration::from_secs(5), repos: 200 };
}

/// Folders a scan never enters: built or fetched files, not projects.
const SKIP: [&str; 4] = ["node_modules", "target", "vendor", ".cache"];

/// The git repositories under `root`, by path, within `bounds`. It enters
/// no hidden folder, no folder in [`SKIP`], no link and no repository.
/// Answers `false` with what it found when a bound stopped it.
pub fn scan(root: &Path, bounds: ScanBounds) -> (Vec<PlaceRepo>, bool) {
    let until = std::time::Instant::now() + bounds.time;
    let (mut found, mut level, mut complete) = (Vec::new(), vec![root.to_path_buf()], true);
    'levels: for depth in 1..=bounds.depth {
        let mut next = Vec::new();
        for dir in level {
            let Ok(read) = fs::read_dir(&dir) else { continue };
            for entry in read.flatten() {
                if std::time::Instant::now() >= until || found.len() >= bounds.repos {
                    complete = false;
                    break 'levels;
                }
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name.starts_with('.') || SKIP.contains(&name.as_ref()) || !entry.file_type().is_ok_and(|t| t.is_dir()) {
                    continue;
                }
                let path = entry.path();
                match repo_state(&path) {
                    Some(r) => found.push(PlaceRepo { path: text(&path), branch: r.branch, last_commit: r.last_commit }),
                    None if depth < bounds.depth => next.push(path),
                    None => {}
                }
            }
        }
        level = next;
    }
    found.sort_by(|a, b| a.path.cmp(&b.path));
    (found, complete)
}

/// The last scan of home, kept for the next `folder_places`.
struct Scans {
    repos: Vec<PlaceRepo>,
    running: bool,
    done: Option<std::time::Instant>,
}
static SCANS: std::sync::Mutex<Scans> = std::sync::Mutex::new(Scans { repos: Vec::new(), running: false, done: None });
/// A scan this recent is not run again: the picker asks while one runs.
const FRESH: std::time::Duration = std::time::Duration::from_secs(10);

/// Recent, and the last scan's repositories. Starts a scan of home in the
/// background unless one runs or one just finished.
pub fn places() -> Result<FolderPlaces, Error> {
    let home = PathBuf::from(home()?);
    let mut scans = SCANS.lock().unwrap_or_else(|e| e.into_inner());
    if !scans.running && scans.done.is_none_or(|d| d.elapsed() > FRESH) {
        scans.running = true;
        std::thread::spawn(move || {
            let (repos, _) = scan(&home, ScanBounds::PLACES);
            let mut scans = SCANS.lock().unwrap_or_else(|e| e.into_inner());
            *scans = Scans { repos, running: false, done: Some(std::time::Instant::now()) };
        });
    }
    Ok(FolderPlaces { recent: recent::read(), repos: scans.repos.clone(), scanning: scans.running })
}

/// The Recent list: folders and when each was last seen in use.
pub mod recent {
    use super::*;
    use std::sync::Mutex;

    static WRITE: Mutex<()> = Mutex::new(());

    #[derive(serde::Serialize, serde::Deserialize)]
    struct Seen {
        path: String,
        /// Unix ms.
        at: u64,
    }

    /// Beside the `projects.toml` this process uses.
    pub fn path() -> PathBuf {
        skiff_core::config::config_path().with_file_name("recent-folders.json")
    }

    fn load(file: &Path) -> Vec<Seen> {
        fs::read(file).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    /// The folders, newest first, that are still folders on this Host.
    pub fn read() -> Vec<String> {
        load(&path()).into_iter().map(|s| s.path).filter(|p| Path::new(p).is_dir()).collect()
    }

    /// Notes each folder as in use at its time (Unix ms). A folder already
    /// listed keeps its later time. Keeps the newest [`MAX_RECENT`].
    pub fn note(seen: impl IntoIterator<Item = (String, u64)>) {
        let _held = WRITE.lock().unwrap_or_else(|e| e.into_inner());
        let file = path();
        let mut list = load(&file);
        let before = serde_json::to_vec(&list).unwrap_or_default();
        for (path, at) in seen {
            if !Path::new(&path).is_absolute() {
                continue;
            }
            match list.iter_mut().find(|s| s.path == path) {
                Some(s) => s.at = s.at.max(at),
                None => list.push(Seen { path, at }),
            }
        }
        list.sort_by_key(|s| std::cmp::Reverse(s.at));
        list.truncate(MAX_RECENT);
        let Ok(bytes) = serde_json::to_vec(&list) else { return };
        if bytes == before {
            return;
        }
        // Whole or not at all: a crash never leaves half a list.
        let tmp = file.with_extension(format!("json.{}", std::process::id()));
        if fs::write(&tmp, &bytes).and_then(|()| fs::rename(&tmp, &file)).is_err() {
            let _ = fs::remove_file(&tmp);
        }
    }
}
