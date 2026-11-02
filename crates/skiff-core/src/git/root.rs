use super::{bounded, branch_base_with, git_bounded, git_in, Deadline, GIT_TIMEOUT, PREFLIGHT_LIMIT, STATUS_LIMIT};

use std::{
    collections::{HashMap, HashSet},
    path::Path,
};

#[cfg(not(unix))]
use std::path::PathBuf;

use anyhow::{bail, Context, Result};

/// One changed file in a worktree, against its base: the branch's fork
/// point, else HEAD. Staged and unstaged edits count together.
#[derive(serde::Serialize, Debug, Clone, PartialEq, Eq)]
pub struct Change {
    /// Relative to the worktree root.
    pub path: String,
    /// `M` modified, `A` added or untracked, `D` deleted, `U` unmerged.
    pub status: char,
    /// Lines added and removed. `None` for a binary file.
    pub added: Option<u32>,
    pub removed: Option<u32>,
    /// The file is the same in the working tree and HEAD: the change sits in
    /// the branch's commits. False for every uncommitted edit.
    pub committed: bool,
}

/// What a worktree changed: its files, and the branch they count from.
#[derive(serde::Serialize, Debug, Clone, PartialEq, Eq)]
pub struct Changes {
    /// The project's default branch, when the worktree is on another branch
    /// and the files count from where it left that one. `None`: against HEAD.
    pub base: Option<String>,
    /// Sorted by path.
    pub files: Vec<Change>,
}

/// Untracked files larger than this show no line count.
pub(super) const COUNT_LIMIT: u64 = 1 << 20;

/// The worktree folder held open, so a count reads the folder that was
/// checked even if its path is swapped for another folder or a link.
pub struct Root {
    pub(crate) dir: std::fs::File,
    #[cfg(not(unix))]
    pub(super) path: PathBuf,
}

impl Root {
    /// Opened before `dir` is known to be a worktree: anything but a folder
    /// fails, and a FIFO there never blocks.
    pub fn open(dir: &Path) -> Result<Root> {
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::custom_flags(&mut options, libc::O_DIRECTORY | libc::O_NONBLOCK);
        let file = options.open(dir).with_context(|| format!("open {}", dir.display()))?;
        if !file.metadata()?.is_dir() {
            bail!("{} is not a folder", dir.display());
        }
        Ok(Root {
            dir: file,
            #[cfg(not(unix))]
            path: dir.to_path_buf(),
        })
    }

    /// Fails unless `dir` still names this folder.
    pub(crate) fn check(&self, dir: &Path) -> Result<()> {
        if !same_file(&self.dir.metadata()?, &std::fs::metadata(dir)?) {
            bail!("{} was replaced while it was read", dir.display());
        }
        Ok(())
    }
}

#[cfg(unix)]
fn same_file(a: &std::fs::Metadata, b: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    (a.dev(), a.ino()) == (b.dev(), b.ino())
}

#[cfg(not(unix))]
fn same_file(_: &std::fs::Metadata, b: &std::fs::Metadata) -> bool {
    b.is_dir()
}

/// The changed files of the worktree at `dir`. On a branch, every file that
/// differs between the fork point and the working tree, committed or not;
/// on the default branch, the uncommitted files.
pub fn changes(dir: &Path) -> Result<Changes> {
    changes_in(&Root::open(dir)?, dir)
}

/// [`changes`] of `root`, opened before `dir` was checked as a worktree.
/// Git runs in `root` itself, and `dir` must name `root` before and after;
/// untracked files are counted under `root` itself. Every git run together
/// takes at most [`GIT_TIMEOUT`].
pub fn changes_in(root: &Root, dir: &Path) -> Result<Changes> {
    root.check(dir)?;
    // Bounded, with no optional lock: a read never holds the index against
    // the person's own git, and a hung git does not keep a request slot.
    let deadline = Deadline::after(GIT_TIMEOUT);
    let run = |args: &[&str], limit: usize| -> Result<String> {
        let (out, cut) = git_bounded(root, dir, args, limit, deadline)?;
        if cut {
            bail!("git {} printed more than Skiff reads", args.join(" "));
        }
        Ok(String::from_utf8_lossy(&out).into_owned())
    };
    let base = branch_base_with(|args| run(args, PREFLIGHT_LIMIT));
    let against = base.as_ref().map_or("HEAD", |b| b.sha.as_str());
    let status = run(&["status", "--porcelain=v1", "-z", "--no-renames", "--untracked-files=all"], STATUS_LIMIT)?;
    // Fails in a repository with no commit yet: every file then shows no count.
    let numstat = run(&["diff", against, "--numstat", "-z", "--no-renames"], STATUS_LIMIT).unwrap_or_default();
    let mut counts = HashMap::new();
    for rec in numstat.split('\0').filter(|r| !r.is_empty()) {
        let mut f = rec.splitn(3, '\t');
        let (Some(a), Some(r), Some(p)) = (f.next(), f.next(), f.next()) else { continue };
        counts.insert(p.to_string(), (a.parse().ok(), r.parse().ok()));
    }
    // What the branch did to each tracked file, from its fork point. Empty
    // on the default branch: there the status letters tell.
    let mut branch: Vec<(char, String)> = Vec::new();
    if base.is_some() {
        let names = run(&["diff", against, "--name-status", "-z", "--no-renames"], STATUS_LIMIT)?;
        let mut f = names.split('\0');
        while let (Some(st), Some(path)) = (f.next(), f.next()) {
            let status = match st.as_bytes().first() {
                Some(b'A') => 'A',
                Some(b'D') => 'D',
                Some(b'U') => 'U',
                _ => 'M',
            };
            branch.push((status, path.to_string()));
        }
    }
    // Swapped while git ran: its output may be another folder's.
    root.check(dir)?;
    let did: HashMap<&str, char> = branch.iter().map(|(s, p)| (p.as_str(), *s)).collect();
    let mut list = Vec::new();
    let mut live = HashSet::new();
    for rec in status.split('\0').filter(|r| r.len() > 3) {
        let (xy, path) = rec.split_at(3);
        let (x, y) = (xy.as_bytes()[0], xy.as_bytes()[1]);
        let status = match (x, y) {
            (b'?', _) => 'A',
            (b'U', _) | (_, b'U') | (b'A', b'A') | (b'D', b'D') => 'U',
            (b'D', _) | (_, b'D') => 'D',
            (b'A', _) => 'A',
            _ => did.get(path).copied().unwrap_or('M'),
        };
        let (added, removed) = match counts.get(path) {
            Some(&c) => c,
            None if x == b'?' => (count_lines(root, path), Some(0)),
            None => (None, None),
        };
        live.insert(path.to_string());
        list.push(Change { path: path.to_string(), status, added, removed, committed: false });
    }
    for (status, path) in branch {
        if live.contains(&path) {
            continue;
        }
        let (added, removed) = counts.get(&path).copied().unwrap_or((None, None));
        list.push(Change { path, status, added, removed, committed: true });
    }
    list.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(Changes { base: base.map(|b| b.branch), files: list })
}

/// Lines in a new file at `rel` under the worktree `root`, as `git diff`
/// would count them. It never leaves the worktree and never blocks:
/// - a symlink is one line, its target, as git shows it; it is not followed
/// - a path through a symlinked folder, `..` or an absolute path: `None`
/// - a FIFO, device, socket or folder: `None`, never opened for reading
/// - a binary file, or one over [`COUNT_LIMIT`] bytes: `None`, and no
///   more than the limit is read even if the file grows meanwhile
pub(super) fn count_lines(root: &Root, rel: &str) -> Option<u32> {
    use std::io::Read;
    let file = open_beneath(root, rel)?;
    let file = match file {
        Beneath::Symlink(_) => return Some(1),
        Beneath::File(f) => f,
    };
    let meta = file.metadata().ok()?;
    if !meta.is_file() || meta.len() > COUNT_LIMIT {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(COUNT_LIMIT + 1).read_to_end(&mut bytes).ok()?;
    if bytes.len() as u64 > COUNT_LIMIT || bytes.contains(&0) {
        return None;
    }
    let n = bytes.iter().filter(|&&b| b == b'\n').count();
    let last = !bytes.is_empty() && !bytes.ends_with(b"\n");
    Some((n + last as usize) as u32)
}

pub(crate) enum Beneath {
    File(std::fs::File),
    /// The last component is a symlink, with its target. It was not followed.
    Symlink(Vec<u8>),
}

/// Opens `rel` under the open folder `root` one component at a time, each
/// relative to the folder opened before it and none followed if it is a
/// symlink, so a folder swapped for a link meanwhile cannot lead outside
/// `root`. The file opens non-blocking: a FIFO that slipped in fails its
/// type check, not a read.
#[cfg(unix)]
pub(crate) fn open_beneath(root: &Root, rel: &str) -> Option<Beneath> {
    use std::{ffi::CString, os::fd::AsRawFd, os::unix::ffi::OsStrExt, os::fd::FromRawFd, path::Component};
    let parts: Vec<_> = Path::new(rel).components().collect();
    if parts.is_empty() || parts.iter().any(|c| !matches!(c, Component::Normal(_))) {
        return None;
    }
    // SAFETY: plain open(2) calls on NUL-terminated paths; every fd made here
    // is closed below or owned by the returned File. `root` stays open.
    unsafe {
        let mut fd = libc::fcntl(root.dir.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0);
        if fd < 0 {
            return None;
        }
        for (i, part) in parts.iter().enumerate() {
            let Ok(name) = CString::new(part.as_os_str().as_bytes()) else {
                libc::close(fd);
                return None;
            };
            let last = i + 1 == parts.len();
            if last {
                // Its type before any open: a link is not followed, and a
                // FIFO or device is never opened.
                let mut st: libc::stat = std::mem::zeroed();
                if libc::fstatat(fd, name.as_ptr(), &mut st, libc::AT_SYMLINK_NOFOLLOW) != 0 {
                    libc::close(fd);
                    return None;
                }
                match st.st_mode & libc::S_IFMT {
                    libc::S_IFREG => {}
                    libc::S_IFLNK => {
                        let mut target = vec![0u8; 4096];
                        let n = libc::readlinkat(fd, name.as_ptr(), target.as_mut_ptr().cast(), target.len());
                        libc::close(fd);
                        target.truncate(n.max(0) as usize);
                        return Some(Beneath::Symlink(target));
                    }
                    _ => {
                        libc::close(fd);
                        return None;
                    }
                }
            }
            // Swapped meanwhile: O_NOFOLLOW refuses a link, O_NONBLOCK keeps a
            // FIFO from blocking, and the caller checks the type again.
            let flags = libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | if last { libc::O_NONBLOCK } else { libc::O_DIRECTORY };
            let next = libc::openat(fd, name.as_ptr(), flags);
            libc::close(fd);
            if next < 0 {
                return None;
            }
            fd = next;
        }
        Some(Beneath::File(std::fs::File::from_raw_fd(fd)))
    }
}

#[cfg(not(unix))]
pub(crate) fn open_beneath(root: &Root, rel: &str) -> Option<Beneath> {
    let path = root.path.join(rel);
    let meta = std::fs::symlink_metadata(&path).ok()?;
    if meta.file_type().is_symlink() {
        let target = std::fs::read_link(&path).map(|t| t.to_string_lossy().into_owned().into_bytes()).unwrap_or_default();
        return Some(Beneath::Symlink(target));
    }
    meta.is_file().then(|| std::fs::File::open(path).ok().map(Beneath::File)).flatten()
}

/// Which of `paths`, relative to `root`, git ignores, as [`ignored`] finds
/// them but run in the held folder and bounded by `deadline`. Any failure, such as no repository, ignores nothing.
pub(crate) fn ignored_in(root: &Root, dir: &Path, paths: &[String], deadline: Deadline) -> HashSet<String> {
    let mut found = HashSet::new();
    // In batches, so no command line grows past the system's limit.
    for batch in paths.chunks(256) {
        let mut cmd = git_in(root, dir);
        // `-z` needs stdin, which a bounded run closes: one path per line.
        // A name with a control character, `"` or `\\` comes back quoted;
        // `unquote` gives the real name back.
        // `./` first, so a name that starts with `:` is not pathspec magic.
        cmd.args(["-c", "core.quotePath=false", "check-ignore", "--"])
            .args(batch.iter().map(|p| format!("./{p}")))
            .env("GIT_OPTIONAL_LOCKS", "0");
        // git exits 1 when nothing is ignored, and says nothing: no batch is lost to it.
        let Ok((out, _)) = bounded(cmd, PREFLIGHT_LIMIT * 4, deadline) else { continue };
        found.extend(out.split(|b| *b == b'\n').filter_map(|l| unquote(l).strip_prefix("./").map(str::to_string)));
    }
    found
}

/// A line of git's path output as the real name. Git wraps a name with a
/// control character, `"` or `\` in double quotes and escapes it like C
/// (`\t`, `\"`, `\\`, `\ooo` for a byte); any other line is the name.
pub(super) fn unquote(line: &[u8]) -> String {
    let Some(inner) = line.strip_prefix(b"\"").and_then(|l| l.strip_suffix(b"\"")) else {
        return String::from_utf8_lossy(line).into_owned();
    };
    let mut out = Vec::with_capacity(inner.len());
    let mut i = 0;
    while i < inner.len() {
        if inner[i] != b'\\' || i + 1 == inner.len() {
            out.push(inner[i]);
            i += 1;
            continue;
        }
        i += 1;
        let c = inner[i];
        i += 1;
        out.push(match c {
            b'a' => 7,
            b'b' => 8,
            b't' => b'\t',
            b'n' => b'\n',
            b'v' => 11,
            b'f' => 12,
            b'r' => b'\r',
            b'0'..=b'7' => {
                let mut v = u32::from(c - b'0');
                for _ in 0..2 {
                    match inner.get(i) {
                        Some(d @ b'0'..=b'7') => {
                            v = v * 8 + u32::from(d - b'0');
                            i += 1;
                        }
                        _ => break,
                    }
                }
                v as u8
            }
            other => other,
        });
    }
    String::from_utf8_lossy(&out).into_owned()
}
