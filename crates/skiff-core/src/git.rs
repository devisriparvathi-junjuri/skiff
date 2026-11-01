//! Thin wrappers over the `git` binary. Blocking: async callers use
//! `spawn_blocking`.

use crate::shell::shell_quote;

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

use anyhow::{anyhow, bail, Context, Result};

use crate::project::Worktree;

/// Runs `git -C <dir> <args>` and returns stdout. A non-zero exit becomes an
/// error that carries git's stderr.
fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .context("run git")?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        if stderr.is_empty() {
            bail!("git {} failed: {}", args.join(" "), out.status);
        }
        bail!(stderr);
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Initializes an existing folder. A folder inside a repository stays as it is.
pub fn init_repository(dir: &Path) -> Result<()> {
    if toplevel(dir).is_ok() {
        return Ok(());
    }
    git(dir, &["init"]).map(drop)
}

pub fn list_worktrees(repo: &Path) -> Result<Vec<Worktree>> {
    // Bounded: the Host lists worktrees on every file and diff request.
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(repo);
    let (out, _) = git_run(cmd, &["worktree", "list", "--porcelain", "-z"], PREFLIGHT_LIMIT * 4, Deadline::after(GIT_TIMEOUT))?;
    Ok(parse_worktrees(&String::from_utf8_lossy(&out)))
}

/// Parses `git worktree list --porcelain -z`: NUL after every field, and an
/// empty field after every record. The first record is the main worktree.
pub fn parse_worktrees(out: &str) -> Vec<Worktree> {
    let mut list = Vec::new();
    let mut cur: Option<Worktree> = None;
    for field in out.split('\0') {
        if field.is_empty() {
            list.extend(cur.take());
            continue;
        }
        let (key, val) = field.split_once(' ').unwrap_or((field, ""));
        if key == "worktree" {
            list.extend(cur.take());
            cur = Some(Worktree {
                path: PathBuf::from(val),
                branch: None,
                head: String::new(),
                is_main: list.is_empty(),
                locked: false,
                prunable: false,
            });
            continue;
        }
        let Some(wt) = cur.as_mut() else { continue };
        match key {
            "HEAD" => wt.head = val.to_string(),
            "branch" => {
                wt.branch = Some(val.strip_prefix("refs/heads/").unwrap_or(val).to_string())
            }
            "locked" => wt.locked = true,
            "prunable" => wt.prunable = true,
            _ => {}
        }
    }
    list.extend(cur);
    list
}

pub fn add_worktree(repo: &Path, branch: &str, path: &Path, base: Option<&str>) -> Result<()> {
    let path = path.to_str().ok_or_else(|| anyhow!("path is not UTF-8"))?;
    // `--` keeps a base like `--force` from being read as an option.
    let mut args = vec!["worktree", "add", "-b", branch, "--", path];
    args.extend(base);
    git(repo, &args).map(drop)
}

/// Checks out the existing `branch` in a new worktree at `path`. A name
/// only a remote has becomes a local branch that tracks it, as git does.
pub fn checkout_worktree(repo: &Path, branch: &str, path: &Path) -> Result<()> {
    let path = path.to_str().ok_or_else(|| anyhow!("path is not UTF-8"))?;
    git(repo, &["worktree", "add", "--", path, branch]).map(drop)
}

/// One branch of a repository, local or a remote's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Branch {
    /// Short: `main`, or `origin/main` for a remote's.
    pub name: String,
    pub remote: bool,
    /// A worktree has it checked out. Always false for a remote's.
    pub worktree: bool,
}

/// At most this many branches, newest commit first.
pub const BRANCHES_MAX: usize = 1000;

/// The local and remote branches, newest commit first. A remote's `HEAD`
/// link is left out.
pub fn branches(repo: &Path) -> Result<Vec<Branch>> {
    let out = git(
        repo,
        &["for-each-ref", "--sort=-committerdate", "--format=%(refname)%00%(worktreepath)%00%(symref)", "refs/heads", "refs/remotes"],
    )?;
    Ok(out
        .lines()
        .filter_map(|l| {
            let mut f = l.split('\0');
            let (r, wt, sym) = (f.next()?, f.next().unwrap_or(""), f.next().unwrap_or(""));
            if let Some(name) = r.strip_prefix("refs/heads/") {
                Some(Branch { name: name.into(), remote: false, worktree: !wt.is_empty() })
            } else {
                let name = r.strip_prefix("refs/remotes/")?;
                (sym.is_empty() && !name.ends_with("/HEAD")).then(|| Branch { name: name.into(), remote: true, worktree: false })
            }
        })
        .take(BRANCHES_MAX)
        .collect())
}

/// The branch checked out in `dir`; `None` when HEAD is detached.
pub fn head_branch(dir: &Path) -> Option<String> {
    git(dir, &["symbolic-ref", "--quiet", "--short", "HEAD"]).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// Without `force`, git refuses a dirty worktree. A locked one is refused either way.
pub fn remove_worktree(repo: &Path, path: &Path, force: bool) -> Result<()> {
    let path = path.to_str().ok_or_else(|| anyhow!("path is not UTF-8"))?;
    let mut args = vec!["worktree", "remove"];
    if force {
        args.push("--force");
    }
    args.extend(["--", path]);
    git(repo, &args).map(drop)
}

/// The branch the main checkout normally holds: origin's HEAD, else a local
/// `main` or `master`. `None` when none of them exists.
pub fn default_branch(repo: &Path) -> Option<String> {
    if let Ok(r) = git(repo, &["symbolic-ref", "--quiet", "--short", "refs/remotes/origin/HEAD"]) {
        if let Some(b) = r.trim().strip_prefix("origin/") {
            return Some(b.to_string());
        }
    }
    ["main", "master"]
        .into_iter()
        .find(|b| git(repo, &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{b}")]).is_ok())
        .map(String::from)
}

/// Whether `dir` is in a git repository. `Ok(false)` only when git runs and
/// says it is not. A missing git or any other failure is an error, so a
/// repository never passes for a plain folder.
pub fn in_repository(dir: &Path) -> Result<bool> {
    // Git's own words, not a translation: the check reads them.
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "--show-toplevel"])
        .env("LC_ALL", "C")
        .env("LANGUAGE", "C")
        .output()
        .context("run git")?;
    classify_repository(out.status.success(), &String::from_utf8_lossy(&out.stderr))
}

/// What `git rev-parse --show-toplevel` says about the folder: in a
/// repository, not in one, or a failure that answers neither.
fn classify_repository(ok: bool, stderr: &str) -> Result<bool> {
    if ok {
        return Ok(true);
    }
    if stderr.contains("not a git repository") {
        return Ok(false);
    }
    let stderr = stderr.trim();
    if stderr.is_empty() {
        bail!("git rev-parse failed");
    }
    bail!(stderr.to_string())
}

/// The top-level directory of the repository that contains `dir`.
pub fn toplevel(dir: &Path) -> Result<PathBuf> {
    let out = git(dir, &["rev-parse", "--show-toplevel"])
        .map_err(|_| anyhow!("{} is not inside a git repository", dir.display()))?;
    Ok(PathBuf::from(out.trim()))
}

pub fn is_dirty(dir: &Path) -> Result<bool> {
    Ok(!git(dir, &["status", "--porcelain"])?.trim().is_empty())
}

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

/// Where a branch worktree's changes count from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Base {
    /// The project's default branch.
    pub branch: String,
    /// The commit where HEAD left it.
    pub sha: String,
}

/// The fork point of the worktree at `dir`: where HEAD left the project's
/// default branch. `None` on the default branch itself, detached, with no
/// default branch, or with no commit in common; the changes then count
/// from HEAD.
pub fn branch_base(dir: &Path) -> Option<Base> {
    branch_base_with(|args| git(dir, args))
}

/// [`branch_base`] through `run`, so a bounded read shares its deadline.
fn branch_base_with(run: impl Fn(&[&str]) -> Result<String>) -> Option<Base> {
    let branch = run(&["symbolic-ref", "--quiet", "--short", "HEAD"]).ok()?;
    let branch = branch.trim();
    let default = run(&["symbolic-ref", "--quiet", "--short", "refs/remotes/origin/HEAD"])
        .ok()
        .and_then(|r| r.trim().strip_prefix("origin/").map(String::from))
        .or_else(|| ["main", "master"].into_iter().find(|b| run(&["rev-parse", "--verify", "--quiet", &format!("refs/heads/{b}")]).is_ok()).map(String::from))?;
    if branch == default {
        return None;
    }
    // The local branch, else origin's: a clone may track main without a local one.
    let sha = [format!("refs/heads/{default}"), format!("refs/remotes/origin/{default}")]
        .iter()
        .find_map(|r| run(&["merge-base", r, "HEAD"]).ok())?;
    Some(Base { branch: default, sha: sha.trim().to_string() })
}

/// Untracked files larger than this show no line count.
const COUNT_LIMIT: u64 = 1 << 20;

/// The worktree folder held open, so a count reads the folder that was
/// checked even if its path is swapped for another folder or a link.
pub struct Root {
    pub(crate) dir: std::fs::File,
    #[cfg(not(unix))]
    pub(crate) path: PathBuf,
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

/// The diff command when `[open] diff` sets none.
pub const DEFAULT_DIFF: &str = "git diff --color=always";

/// A changed file's diff against the worktree's base, or the whole
/// worktree's, printed by `command` in the worktree at `width` columns. The
/// base is the branch's fork point, else HEAD: see [`branch_base`].
/// `{target}` in the command becomes what to compare; without it, the target
/// goes at the end. An untracked file compares with an empty file. The
/// whole-worktree view leaves untracked files out.
pub fn diff_text(dir: &Path, file: Option<&str>, command: Option<&str>, width: u16) -> Result<String> {
    let script = diff_script(dir, file, command);
    let out = Command::new("sh")
        .args(["-c", &script])
        .current_dir(dir)
        // Printed to a pipe, tools drop color and guess the width. Tell them both.
        .env("COLUMNS", width.to_string())
        .env("DFT_COLOR", "always")
        .env("DFT_WIDTH", width.to_string())
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", "core.quotePath")
        .env("GIT_CONFIG_VALUE_0", "false")
        .stdin(Stdio::null())
        .output()
        .context("run the diff command")?;
    // Diff tools exit 1 when the files differ.
    if !out.status.success() && out.status.code() != Some(1) {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        bail!("{script} failed: {}", if err.is_empty() { out.status.to_string() } else { err });
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The shell script that shows a changed file's diff against the worktree's
/// base, or the whole worktree's, with `command`, else `git diff`. The base
/// is the branch's fork point, else HEAD: see [`branch_base`]. `{target}` in
/// the command becomes what to compare; without it, the target goes at the
/// end. An untracked file compares with an empty file.
pub fn diff_script(dir: &Path, file: Option<&str>, command: Option<&str>) -> String {
    let base = branch_base(dir).map(|b| b.sha);
    let against = base.as_deref().unwrap_or("HEAD");
    // Known to git in the index, or at the base: a file a commit deleted is still a diff.
    let known = |f: &str| {
        git(dir, &["ls-files", "--error-unmatch", "--", f]).is_ok()
            || base.as_ref().is_some_and(|b| git(dir, &["cat-file", "-e", &format!("{b}:{f}")]).is_ok())
    };
    let target = match file {
        None => against.to_string(),
        Some(f) if known(f) => format!("{against} -- {}", shell_quote(f)),
        Some(f) => format!("--no-index -- /dev/null {}", shell_quote(f)),
    };
    let command = command.map(str::trim).filter(|c| !c.is_empty()).unwrap_or(DEFAULT_DIFF);
    if command.contains("{target}") {
        command.replace("{target}", &target)
    } else {
        format!("{command} {target}")
    }
}

/// The most diff text [`diff_in`] answers with, in bytes.
pub const DIFF_LIMIT: usize = 512 * 1024;
/// How long every git run for one [`diff_in`] or [`changes_in`] may take
/// together before it is killed.
const GIT_TIMEOUT: Duration = Duration::from_secs(20);
/// The most a git run before the diff itself may print, in bytes.
const PREFLIGHT_LIMIT: usize = 64 * 1024;
/// The most `git status` may print for one [`changes_in`], in bytes.
const STATUS_LIMIT: usize = 4 * 1024 * 1024;

/// A diff as plain text, for a viewer: never colored, never from an
/// external diff tool or a textconv filter, never over [`DIFF_LIMIT`].
#[derive(serde::Serialize, Debug, Clone, PartialEq, Eq, Default)]
pub struct Diff {
    /// Unified diff text. Empty: no change.
    pub text: String,
    /// The diff is longer than the limit: `text` is its start, cut at a line end.
    pub truncated: bool,
    /// The file is binary: `text` holds git's note only.
    pub binary: bool,
    /// The file is untracked: it is compared with an empty file.
    pub untracked: bool,
}

/// `rel` is a path below a worktree: relative, with no `..`, `.` or root.
pub fn is_relative_file(rel: &str) -> bool {
    use std::path::Component;
    let parts: Vec<_> = Path::new(rel).components().collect();
    !parts.is_empty() && parts.iter().all(|c| matches!(c, Component::Normal(_)))
}

/// One changed file's diff against the worktree's base, or the whole
/// worktree's, as [`changes_in`] reads it: the branch's fork point, else
/// HEAD. Git runs in `root` itself, `dir` must name `root` before and after,
/// and an untracked file is read under `root` itself, never followed if it
/// is a link, never opened if it is not a regular file. The whole-worktree
/// diff leaves untracked files out, as `git diff` does. Every git run
/// together takes at most [`GIT_TIMEOUT`].
pub fn diff_in(root: &Root, dir: &Path, file: Option<&str>) -> Result<Diff> {
    diff_within(root, dir, file, Deadline::after(GIT_TIMEOUT))
}

fn diff_within(root: &Root, dir: &Path, file: Option<&str>, deadline: Deadline) -> Result<Diff> {
    root.check(dir)?;
    let git = |args: &[&str], limit| git_bounded(root, dir, args, limit, deadline);
    // The fork point shares the deadline: its runs are small.
    let base = branch_base_with(|args| preflight(git, args)).map(|b| b.sha);
    let diff = match file {
        None => tracked_diff(git, None, base.as_deref())?,
        Some(f) => {
            if !is_relative_file(f) {
                bail!("{f} is not a file path in this worktree");
            }
            // Literal: a `*` or `:` in a name is not a pattern.
            let spec = format!(":(literal){f}");
            let status = preflight(git, &["status", "--porcelain=v1", "-z", "--no-renames", "--untracked-files=all", "--", &spec])?;
            match status.split('\0').find(|r| r.len() > 3 && &r[3..] == f) {
                // Clean against HEAD. On a branch, a commit may still have changed it.
                None if base.is_some() => tracked_diff(git, Some(&spec), base.as_deref())?,
                None => Diff::default(),
                Some(r) if r.starts_with("??") => untracked_diff(root, f)?,
                Some(_) => tracked_diff(git, Some(&spec), base.as_deref())?,
            }
        }
    };
    // Swapped while git ran: its output may be another folder's.
    root.check(dir)?;
    Ok(diff)
}

/// A git run before the diff, as text: more than [`PREFLIGHT_LIMIT`] is an error.
fn preflight(git: impl Fn(&[&str], usize) -> Result<(Vec<u8>, bool)>, args: &[&str]) -> Result<String> {
    let (out, cut) = git(args, PREFLIGHT_LIMIT)?;
    if cut {
        bail!("git {} printed more than {} KiB", args[0], PREFLIGHT_LIMIT / 1024);
    }
    Ok(String::from_utf8_lossy(&out).into_owned())
}

/// The diff of the working tree against `base`, the branch's fork point,
/// else against HEAD.
fn tracked_diff(git: impl Fn(&[&str], usize) -> Result<(Vec<u8>, bool)>, spec: Option<&str>, base: Option<&str>) -> Result<Diff> {
    let base = match base {
        Some(b) => b.to_string(),
        // No commit yet: everything is compared with the empty tree.
        None => match preflight(&git, &["rev-parse", "--verify", "--quiet", "HEAD^{commit}"]) {
            Ok(head) => head,
            Err(_) => preflight(&git, &["hash-object", "-t", "tree", "/dev/null"])?,
        },
    };
    let base = base.trim();
    let mut args = vec!["-c", "core.quotePath=false", "diff", "--no-color", "--no-ext-diff", "--no-textconv", "--no-renames", base];
    if let Some(spec) = spec {
        args.extend(["--", spec]);
    }
    let (out, truncated) = git(&args, DIFF_LIMIT)?;
    let (text, truncated) = cap(out, truncated, DIFF_LIMIT);
    // Content lines start with ` `, `+` or `-`: this line is git's own.
    let binary = spec.is_some() && text.lines().any(|l| l.starts_with("Binary files "));
    Ok(Diff { text, truncated, binary, untracked: false })
}

/// A new file's diff, as `git diff --no-index /dev/null <file>` shows it,
/// read under `root` with the same rules as its line count.
fn untracked_diff(root: &Root, rel: &str) -> Result<Diff> {
    use std::io::Read;
    let not_shown = || anyhow!("{rel} is not a regular file or a link, so it is not shown");
    let (mode, body, truncated) = match open_beneath(root, rel).ok_or_else(not_shown)? {
        Beneath::Symlink(target) => ("120000", target, false),
        Beneath::File(file) => {
            let meta = file.metadata()?;
            if !meta.is_file() {
                return Err(not_shown());
            }
            #[cfg(unix)]
            let exec = std::os::unix::fs::PermissionsExt::mode(&meta.permissions()) & 0o111 != 0;
            #[cfg(not(unix))]
            let exec = false;
            let mut bytes = Vec::new();
            file.take(DIFF_LIMIT as u64 + 1).read_to_end(&mut bytes)?;
            let truncated = bytes.len() > DIFF_LIMIT;
            bytes.truncate(DIFF_LIMIT);
            (if exec { "100755" } else { "100644" }, bytes, truncated)
        }
    };
    let mut text = format!("diff --git a/{rel} b/{rel}\nnew file mode {mode}\n");
    // git's test: a NUL in the first 8000 bytes.
    if body[..body.len().min(8000)].contains(&0) {
        text += &format!("Binary files /dev/null and b/{rel} differ\n");
        return Ok(Diff { text, truncated: false, binary: true, untracked: true });
    }
    let body = String::from_utf8_lossy(&body);
    let lines: Vec<&str> = body.split_inclusive('\n').collect();
    if !lines.is_empty() {
        let count = if lines.len() == 1 { String::new() } else { format!(",{}", lines.len()) };
        text += &format!("--- /dev/null\n+++ b/{rel}\n@@ -0,0 +1{count} @@\n");
        for line in &lines {
            text.push('+');
            text.push_str(line);
        }
        if !body.ends_with('\n') && !truncated {
            text += "\n\\ No newline at end of file\n";
        }
    }
    let (text, cut) = cap(text.into_bytes(), truncated, DIFF_LIMIT);
    Ok(Diff { text, truncated: cut, binary: false, untracked: true })
}

/// At most `limit` bytes of UTF-8 text, counted after an invalid byte
/// becomes U+FFFD (three bytes). Cut text ends at its last whole line.
pub(crate) fn cap(out: Vec<u8>, truncated: bool, limit: usize) -> (String, bool) {
    let mut text = String::from_utf8_lossy(&out).into_owned();
    let truncated = truncated || text.len() > limit;
    if truncated {
        // A newline is one byte, so the cut is on a character boundary.
        let head = &text.as_bytes()[..text.len().min(limit)];
        let end = head.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
        text.truncate(end);
    }
    (text, truncated)
}

/// `git`, started in the folder `root` holds open rather than at its path:
/// a path swapped for another folder, even for a moment, is never read.
/// `dir` names `root`, for a system that cannot start a process in an open folder.
fn git_in(root: &Root, dir: &Path) -> Command {
    let mut cmd = Command::new("git");
    #[cfg(unix)]
    {
        use std::os::{fd::AsRawFd, unix::process::CommandExt};
        let _ = dir;
        let fd = root.dir.as_raw_fd();
        // SAFETY: fchdir(2) is async-signal-safe. The fd is `root`'s, open
        // while the caller borrows it, and the child inherits it until exec.
        unsafe {
            cmd.pre_exec(move || if libc::fchdir(fd) == 0 { Ok(()) } else { Err(std::io::Error::last_os_error()) });
        }
    }
    #[cfg(not(unix))]
    {
        let _ = root;
        cmd.arg("-C").arg(dir);
    }
    cmd
}

/// When a run, or several that share it, must be done.
#[derive(Clone, Copy)]
pub(crate) struct Deadline {
    at: Instant,
    after: Duration,
}

impl Deadline {
    pub(crate) fn after(after: Duration) -> Deadline {
        Deadline { at: Instant::now() + after, after }
    }
}

/// Runs `git <args>` in `root` with [`bounded`].
fn git_bounded(root: &Root, dir: &Path, args: &[&str], limit: usize, deadline: Deadline) -> Result<(Vec<u8>, bool)> {
    git_run(git_in(root, dir), args, limit, deadline)
}

/// Runs the git command `cmd` with `args` under [`bounded`], as a read:
/// git takes no optional lock and writes no index. A failure carries git's
/// own words.
fn git_run(mut cmd: Command, args: &[&str], limit: usize, deadline: Deadline) -> Result<(Vec<u8>, bool)> {
    cmd.args(args).env("GIT_OPTIONAL_LOCKS", "0");
    bounded(cmd, limit, deadline).map_err(|e| match e.downcast_ref::<Failed>() {
        Some(Failed(err)) if err.is_empty() => anyhow!("git {} failed", args.join(" ")),
        Some(Failed(err)) => anyhow!("{err}"),
        None => e,
    })
}

/// A command that exited with an error, and the start of what it printed there.
#[derive(Debug)]
struct Failed(String);
impl std::fmt::Display for Failed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for Failed {}

/// How long a command's pipes are still read after it exits: a helper it
/// started may hold them open, and never close them.
const DRAIN_GRACE: Duration = Duration::from_millis(200);
/// The start of a command's errors that is kept.
const ERROR_LIMIT: usize = 4096;

/// Runs `cmd` and keeps at most `limit` bytes of its output, and the start
/// of its errors. More output, or a run past `deadline`, kills it. True: the
/// output was cut. Its pipes are read here, never past the deadline, nor
/// past [`DRAIN_GRACE`] after it exits; then the process group it leads,
/// any helper it started included, is killed.
#[cfg(unix)]
fn bounded(mut cmd: Command, limit: usize, deadline: Deadline) -> Result<(Vec<u8>, bool)> {
    use std::{
        io::Read,
        os::{fd::AsRawFd, fd::OwnedFd, unix::process::CommandExt},
    };
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .context("start the command")?;
    let pid = child.id() as libc::pid_t;
    let mut pipes: [Option<std::fs::File>; 2] = [
        child.stdout.take().map(|p| OwnedFd::from(p).into()),
        child.stderr.take().map(|p| OwnedFd::from(p).into()),
    ];
    for p in pipes.iter().flatten() {
        // SAFETY: fcntl(2) on an fd this function owns.
        unsafe {
            let flags = libc::fcntl(p.as_raw_fd(), libc::F_GETFL);
            libc::fcntl(p.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK);
        }
    }
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let (mut truncated, mut timed_out) = (false, false);
    let mut exited: Option<Instant> = None;
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        if exited.is_none() && has_exited(pid) {
            exited = Some(Instant::now());
        }
        let open: Vec<usize> = (0..2).filter(|&i| pipes[i].is_some()).collect();
        if open.is_empty() && exited.is_some() {
            break;
        }
        let end = exited.map_or(deadline.at, |t| deadline.at.min(t + DRAIN_GRACE));
        let now = Instant::now();
        if now >= end {
            timed_out = exited.is_none();
            break;
        }
        // Woken now and then to see whether the command exited.
        let wait = (end - now).min(Duration::from_millis(20));
        let mut fds: Vec<libc::pollfd> = open
            .iter()
            .map(|&i| libc::pollfd { fd: pipes[i].as_ref().unwrap().as_raw_fd(), events: libc::POLLIN, revents: 0 })
            .collect();
        // SAFETY: `fds` holds `fds.len()` entries for fds open here.
        if unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, wait.as_millis().max(1) as libc::c_int) } < 0 {
            let e = std::io::Error::last_os_error();
            if e.kind() != std::io::ErrorKind::Interrupted {
                return Err(e).context("wait for the command's output");
            }
            continue;
        }
        for (fd, &i) in fds.iter().zip(&open) {
            if fd.revents == 0 {
                continue;
            }
            match pipes[i].as_mut().unwrap().read(&mut buf) {
                Ok(0) => pipes[i] = None,
                Ok(n) => {
                    // Read to the end, so the command never waits on a full pipe; the start is kept.
                    let (kept, most) = if i == 0 { (&mut out, limit + 1) } else { (&mut err, ERROR_LIMIT) };
                    let room = most.saturating_sub(kept.len());
                    kept.extend_from_slice(&buf[..n.min(room)]);
                }
                Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted) => {}
                Err(_) => pipes[i] = None,
            }
        }
        if out.len() > limit {
            truncated = true;
            out.truncate(limit);
            break;
        }
    }
    drop(pipes);
    // The leader is not reaped yet, so the group id is still its own and the
    // kill reaches no other process. A helper that left the group is cut off
    // by the closed pipes instead.
    // SAFETY: kill(2) of this command's own process group.
    unsafe { libc::kill(-pid, libc::SIGKILL) };
    let status = child.wait().context("wait for the command")?;
    let err = String::from_utf8_lossy(&err).trim().to_string();
    if timed_out {
        bail!("git took longer than {}s", deadline.after.as_secs_f32());
    }
    if !truncated && !status.success() {
        return Err(Failed(err).into());
    }
    Ok((out, truncated))
}

/// The child `pid` exited. It is left unreaped.
#[cfg(unix)]
fn has_exited(pid: libc::pid_t) -> bool {
    // SAFETY: waitid(2) with WNOWAIT only reads the child's state.
    unsafe {
        let mut info: libc::siginfo_t = std::mem::zeroed();
        let r = libc::waitid(libc::P_PID, pid as libc::id_t, &mut info, libc::WEXITED | libc::WNOHANG | libc::WNOWAIT);
        // With WNOHANG and no exit yet, the pid stays zero.
        r == 0 && info.si_pid() != 0
    }
}

/// Without process groups: the output is cut at `limit`, and the time is not bounded.
#[cfg(not(unix))]
fn bounded(mut cmd: Command, limit: usize, _deadline: Deadline) -> Result<(Vec<u8>, bool)> {
    let o = cmd.stdin(Stdio::null()).output().context("start the command")?;
    if !o.status.success() {
        return Err(Failed(String::from_utf8_lossy(&o.stderr).trim().to_string()).into());
    }
    let mut out = o.stdout;
    let truncated = out.len() > limit;
    out.truncate(limit);
    Ok((out, truncated))
}

/// Lines in a new file at `rel` under the worktree `root`, as `git diff`
/// would count them. It never leaves the worktree and never blocks:
/// - a symlink is one line, its target, as git shows it; it is not followed
/// - a path through a symlinked folder, `..` or an absolute path: `None`
/// - a FIFO, device, socket or folder: `None`, never opened for reading
/// - a binary file, or one over [`COUNT_LIMIT`] bytes: `None`, and no
///   more than the limit is read even if the file grows meanwhile
fn count_lines(root: &Root, rel: &str) -> Option<u32> {
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
fn unquote(line: &[u8]) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_quoted_names_come_back_real() {
        assert_eq!(unquote(b"./plain.txt"), "./plain.txt");
        assert_eq!(unquote(br#""./a\tb\"c\\d""#), "./a\tb\"c\\d");
        assert_eq!(unquote(br#""./\303\251""#), "./é");
    }

    #[test]
    fn only_git_saying_not_a_repository_makes_a_plain_folder() {
        assert!(classify_repository(true, "").unwrap());
        let not = "fatal: not a git repository (or any of the parent directories): .git\n";
        assert!(!classify_repository(false, not).unwrap());
        // Any other failure is an error: the folder may well be a repository.
        let unsafe_dir = "fatal: detected dubious ownership in repository at '/srv/repo'\n";
        assert!(classify_repository(false, unsafe_dir).unwrap_err().to_string().contains("dubious ownership"));
        assert!(classify_repository(false, "").is_err());
    }

    #[test]
    fn init_repository_keeps_files_and_supports_an_unborn_branch() {
        let root = std::env::temp_dir().join(format!("skiff-init-{}", std::process::id()));
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("notes.txt"), "keep me\n").unwrap();
        init_repository(&root).unwrap();
        assert_eq!(toplevel(&root).unwrap(), root.canonicalize().unwrap());
        assert_eq!(std::fs::read_to_string(root.join("notes.txt")).unwrap(), "keep me\n");
        assert_eq!(git(&root, &["status", "--porcelain"]).unwrap().trim(), "?? notes.txt");
        assert!(git(&root, &["rev-parse", "--verify", "HEAD"]).is_err());
        let worktrees = list_worktrees(&root).unwrap();
        assert_eq!(worktrees.len(), 1);
        assert!(worktrees[0].is_main);
        assert!(worktrees[0].branch.is_some());
        init_repository(&root.join("src")).unwrap();
        assert!(!root.join("src/.git").exists());
        assert!(init_repository(&root.join("missing")).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn changes_counts_lines_per_file() {
        let root = std::env::temp_dir().join(format!("skiff-changes-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let run = |args: &[&str]| assert!(Command::new("git").arg("-C").arg(&root).args(args).status().unwrap().success());
        run(&["init", "-q"]);
        run(&["config", "user.email", "t@t"]);
        run(&["config", "user.name", "t"]);
        std::fs::write(root.join("a.txt"), "1\n2\n3\n").unwrap();
        std::fs::write(root.join("gone.txt"), "x\n").unwrap();
        run(&["add", "."]);
        run(&["commit", "-qm", "init"]);
        std::fs::write(root.join("a.txt"), "1\ntwo\n3\n4\n").unwrap();
        std::fs::remove_file(root.join("gone.txt")).unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/new.rs"), "fn main() {}\nlast").unwrap();
        std::fs::write(root.join("blob.bin"), [0u8, 1, 2]).unwrap();

        let got = changes(&root).unwrap();
        std::fs::remove_dir_all(&root).unwrap();
        let c = |path: &str, status, added, removed| Change { path: path.into(), status, added, removed, committed: false };
        assert_eq!(got.base, None);
        assert_eq!(
            got.files,
            [
                c("a.txt", 'M', Some(2), Some(1)),
                c("blob.bin", 'A', None, Some(0)),
                c("gone.txt", 'D', Some(0), Some(1)),
                c("src/new.rs", 'A', Some(2), Some(0)),
            ]
        );
    }

    #[test]
    fn a_branch_counts_its_commits_from_the_fork_point() {
        let root = std::env::temp_dir().join(format!("skiff-branch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let run = |args: &[&str]| assert!(Command::new("git").arg("-C").arg(&root).args(args).status().unwrap().success());
        run(&["init", "-q", "-b", "main"]);
        run(&["config", "user.email", "t@t"]);
        run(&["config", "user.name", "t"]);
        std::fs::write(root.join("a.txt"), "1\n2\n3\n").unwrap();
        std::fs::write(root.join("gone.txt"), "x\n").unwrap();
        std::fs::write(root.join("same.txt"), "s\n").unwrap();
        run(&["add", "."]);
        run(&["commit", "-qm", "init"]);
        run(&["checkout", "-qb", "feat/x"]);
        std::fs::write(root.join("a.txt"), "1\ntwo\n3\n").unwrap();
        std::fs::write(root.join("b.txt"), "b\n").unwrap();
        std::fs::remove_file(root.join("gone.txt")).unwrap();
        run(&["add", "-A"]);
        run(&["commit", "-qm", "work"]);
        // Main moves on: its own edit must not show on the branch.
        run(&["checkout", "-q", "main"]);
        std::fs::write(root.join("same.txt"), "s\nmain\n").unwrap();
        run(&["commit", "-qam", "main moves"]);
        run(&["checkout", "-q", "feat/x"]);
        // A committed file edited again is live, and a new file is untracked.
        std::fs::write(root.join("b.txt"), "b\nmore\n").unwrap();
        std::fs::write(root.join("c.txt"), "c\n").unwrap();

        let got = changes(&root).unwrap();
        let base = branch_base(&root).unwrap();
        let script = diff_script(&root, Some("gone.txt"), Some("git diff"));
        let held = Root::open(&root).unwrap();
        // A file only a commit changed, one a commit deleted, and the whole branch.
        let a = diff_in(&held, &root, Some("a.txt")).unwrap();
        let gone = diff_in(&held, &root, Some("gone.txt")).unwrap();
        let same = diff_in(&held, &root, Some("same.txt")).unwrap();
        let all = diff_in(&held, &root, None).unwrap();
        // Main has no b.txt: its edit goes back before the checkout.
        run(&["checkout", "-q", "--", "b.txt"]);
        run(&["checkout", "-q", "main"]);
        let on_main = changes(&root).unwrap();
        std::fs::remove_dir_all(&root).unwrap();
        let c = |path: &str, status, added, removed, committed| Change { path: path.into(), status, added, removed, committed };
        assert_eq!(got.base.as_deref(), Some("main"));
        assert_eq!(
            got.files,
            [
                c("a.txt", 'M', Some(1), Some(1), true),
                c("b.txt", 'A', Some(2), Some(0), false),
                c("c.txt", 'A', Some(1), Some(0), false),
                c("gone.txt", 'D', Some(0), Some(1), true),
            ]
        );
        assert_eq!(base.branch, "main");
        assert_eq!(script, format!("git diff {} -- {}", base.sha, shell_quote("gone.txt")));
        assert!(a.text.contains("-2\n+two\n") && !a.untracked, "{a:?}");
        assert!(gone.text.contains("deleted file mode") && gone.text.contains("-x\n"), "{gone:?}");
        assert_eq!(same, Diff::default());
        assert!(all.text.contains("a/a.txt") && all.text.contains("a/gone.txt") && all.text.contains("b/b.txt") && !all.text.contains("same.txt") && !all.text.contains("c.txt"), "{}", all.text);
        // The default branch keeps its own view: uncommitted files only.
        assert_eq!(on_main.base, None);
        assert_eq!(on_main.files, [c("c.txt", 'A', Some(1), Some(0), false)]);
    }

    /// Review M4 P2: an untracked link to a FIFO outside the worktree once
    /// blocked the count and read the outside data. Now a link counts as git
    /// counts it (one line, its target), and nothing outside, no FIFO and no
    /// more than the limit is ever read.
    #[cfg(unix)]
    #[test]
    fn untracked_links_fifos_and_large_files_are_never_followed_or_read() {
        use std::os::unix::fs::symlink;
        let base = std::env::temp_dir().join(format!("skiff-count-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        struct Gone(PathBuf);
        impl Drop for Gone {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _gone = Gone(base.clone());
        let (root, outside) = (base.join("repo"), base.join("outside"));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let run = |args: &[&str]| assert!(Command::new("git").arg("-C").arg(&root).args(args).status().unwrap().success());
        run(&["init", "-q"]);
        std::fs::write(outside.join("secret.txt"), "1\n2\n3\n").unwrap();
        let mkfifo = |p: &Path| {
            let c = std::ffi::CString::new(p.as_os_str().as_encoded_bytes()).unwrap();
            // SAFETY: a NUL-terminated path.
            assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        };
        mkfifo(&outside.join("fifo"));
        mkfifo(&root.join("pipe"));
        symlink(outside.join("secret.txt"), root.join("link-out")).unwrap();
        symlink(outside.join("fifo"), root.join("link-fifo")).unwrap();
        symlink(&outside, root.join("dir-link")).unwrap();
        std::fs::write(root.join("big.txt"), "x\n".repeat((COUNT_LIMIT as usize / 2) + 1)).unwrap();
        std::fs::write(root.join("small.txt"), "a\nb").unwrap();

        // Off this thread: a regression would block on the FIFO.
        let (tx, rx) = std::sync::mpsc::channel();
        let dir = root.clone();
        std::thread::spawn(move || tx.send(changes(&dir)).unwrap());
        let got = rx.recv_timeout(std::time::Duration::from_secs(10)).expect("changes blocked").unwrap();
        let count = |p: &str| got.files.iter().find(|c| c.path == p).map(|c| c.added);
        // git's own count for a new link is one line: its target.
        let out = Command::new("git").arg("-C").arg(&root).args(["diff", "--no-index", "--numstat", "--", "/dev/null", "link-out"]).output().unwrap();
        let git_count = String::from_utf8_lossy(&out.stdout);
        assert!(git_count.starts_with("1\t0"), "{git_count}");
        assert_eq!(count("link-out"), Some(Some(1)));
        assert_eq!(count("link-fifo"), Some(Some(1)));
        assert_eq!(count("dir-link"), Some(Some(1)));
        assert_eq!(count("big.txt"), Some(None));
        assert_eq!(count("small.txt"), Some(Some(2)));
        // Directly: a FIFO, a path through a link, `..` and absolute paths.
        for rel in ["pipe", "dir-link/secret.txt", "../outside/secret.txt", "/etc/hosts", ""] {
            assert_eq!(count_lines(&Root::open(&root).unwrap(), rel), None, "{rel}");
        }
    }

    /// Review M4 P2: the folder held open is the one counted. A path that
    /// no longer names it, swapped for a link or another folder, is refused.
    #[cfg(unix)]
    #[test]
    fn a_root_whose_path_names_another_folder_is_refused() {
        let base = std::env::temp_dir().join(format!("skiff-root-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let (root, other) = (base.join("repo"), base.join("other"));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        let held = Root::open(&root).unwrap();
        let first = held.check(&root).is_ok();
        std::fs::rename(&root, base.join("moved")).unwrap();
        std::os::unix::fs::symlink(&other, &root).unwrap();
        let swapped = changes_in(&held, &root).map_err(|e| e.to_string());
        let other_folder = changes_in(&held, &other).map_err(|e| e.to_string());
        let file = Root::open(&base.join("moved/missing"));
        std::fs::remove_dir_all(&base).unwrap();
        assert!(first);
        assert!(swapped.is_err_and(|e| e.contains("replaced")));
        assert!(other_folder.is_err_and(|e| e.contains("replaced")));
        assert!(file.is_err());
    }

    /// M4 diff text for a viewer: git's own text for a tracked file, a new
    /// file read under the held root, explicit binary, untracked and cut
    /// states, and nothing read outside the worktree or through a FIFO.
    #[cfg(unix)]
    #[test]
    fn diff_text_is_bounded_plain_and_stays_in_the_worktree() {
        use std::os::unix::fs::symlink;
        let base = std::env::temp_dir().join(format!("skiff-difftext-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        struct Gone(PathBuf);
        impl Drop for Gone {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _gone = Gone(base.clone());
        let (root, outside) = (base.join("repo"), base.join("outside"));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let run = |args: &[&str]| assert!(Command::new("git").arg("-C").arg(&root).args(args).status().unwrap().success());
        run(&["init", "-q"]);
        // No commit yet: an added file is compared with the empty tree.
        std::fs::write(root.join("a.txt"), "1\n2\n").unwrap();
        run(&["add", "a.txt"]);
        let held = Root::open(&root).unwrap();
        let unborn = diff_in(&held, &root, Some("a.txt")).unwrap();
        assert!(unborn.text.contains("+1\n+2\n") && !unborn.untracked, "{unborn:?}");
        run(&["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "init"]);

        // A tracked change: exactly git's own plain text.
        std::fs::write(root.join("a.txt"), "1\ntwo\n").unwrap();
        let got = diff_in(&held, &root, Some("a.txt")).unwrap();
        let git_text = git(&root, &["diff", "--no-color", "--no-ext-diff", "HEAD", "--", "a.txt"]).unwrap();
        assert_eq!(got, Diff { text: git_text, ..Diff::default() });
        assert!(!got.text.contains('\x1b'));
        // Unchanged, and a name that is a pattern elsewhere: nothing, literally.
        std::fs::write(root.join("b.txt"), "b\n").unwrap();
        run(&["add", "b.txt"]);
        run(&["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "b"]);
        assert_eq!(diff_in(&held, &root, Some("b.txt")).unwrap(), Diff::default());
        assert_eq!(diff_in(&held, &root, Some("*.txt")).unwrap(), Diff::default());

        // Untracked: compared with an empty file, read under the root.
        std::fs::write(root.join("new.txt"), "x\ny").unwrap();
        let new = diff_in(&held, &root, Some("new.txt")).unwrap();
        assert!(new.untracked && !new.binary && !new.truncated, "{new:?}");
        assert!(new.text.ends_with("@@ -0,0 +1,2 @@\n+x\n+y\n\\ No newline at end of file\n"), "{}", new.text);
        // A link shows its target and is never followed.
        std::fs::write(outside.join("secret.txt"), "secret\n").unwrap();
        symlink(outside.join("secret.txt"), root.join("link")).unwrap();
        let link = diff_in(&held, &root, Some("link")).unwrap();
        assert!(link.text.contains("new file mode 120000") && link.text.contains(&format!("+{}", outside.join("secret.txt").display())));
        assert!(!link.text.contains("+secret"), "{}", link.text);
        // A FIFO never blocks the diff and is never opened for reading.
        let fifo = std::ffi::CString::new(root.join("pipe").as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: a NUL-terminated path.
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        let (tx, rx) = std::sync::mpsc::channel();
        let (dir, r2) = (root.clone(), Root::open(&root).unwrap());
        std::thread::spawn(move || tx.send(diff_in(&r2, &dir, Some("pipe")).map_err(|e| e.to_string())).unwrap());
        let pipe = rx.recv_timeout(Duration::from_secs(10)).expect("diff blocked on a FIFO");
        // git lists no FIFO as untracked: no change, and nothing opened.
        assert_eq!(pipe, Ok(Diff::default()));
        let direct = untracked_diff(&held, "pipe").map_err(|e| e.to_string());
        assert!(direct.is_err_and(|e| e.contains("not a regular file")));
        std::fs::remove_file(root.join("pipe")).unwrap();
        // Binary: git's note only.
        std::fs::write(root.join("blob.bin"), [0u8, 1, 2, 3]).unwrap();
        let blob = diff_in(&held, &root, Some("blob.bin")).unwrap();
        assert!(blob.binary && blob.untracked && blob.text.contains("Binary files /dev/null and b/blob.bin differ"));
        std::fs::write(root.join("a.txt"), [0u8, 9, 9]).unwrap();
        let tracked_bin = diff_in(&held, &root, Some("a.txt")).unwrap();
        assert!(tracked_bin.binary && !tracked_bin.untracked, "{tracked_bin:?}");

        // Over the limit, tracked or untracked: cut at a line end, and said so.
        let big = "0123456789abcdef\n".repeat(DIFF_LIMIT / 8);
        std::fs::write(root.join("a.txt"), &big).unwrap();
        std::fs::write(root.join("big.txt"), &big).unwrap();
        for f in ["a.txt", "big.txt"] {
            let d = diff_in(&held, &root, Some(f)).unwrap();
            assert!(d.truncated && d.text.len() <= DIFF_LIMIT && d.text.ends_with('\n'), "{f}: {}", d.text.len());
        }
        let all = diff_in(&held, &root, None).unwrap();
        assert!(all.truncated && all.text.len() <= DIFF_LIMIT && !all.text.contains("big.txt"));

        // Not a path below the worktree.
        for bad in ["../outside/secret.txt", "/etc/hosts", "", "./a.txt", "link/../a.txt"] {
            assert!(diff_in(&held, &root, Some(bad)).is_err(), "{bad}");
        }
        // The root swapped for a link: refused, never the other folder's.
        std::fs::rename(&root, base.join("moved")).unwrap();
        symlink(&outside, &root).unwrap();
        assert!(diff_in(&held, &root, Some("new.txt")).is_err_and(|e| e.to_string().contains("replaced")));
    }

    /// The diff's subprocess: a run past its time is killed, and output past
    /// the limit is cut and the process killed, not read to the end.
    #[cfg(unix)]
    #[test]
    fn a_diff_process_is_bounded_in_time_and_output() {
        let sh = |script: &str| {
            let mut c = Command::new("sh");
            c.args(["-c", script]);
            c
        };
        let within = Deadline::after;
        let started = std::time::Instant::now();
        let slow = bounded(sh("sleep 30"), 10, within(Duration::from_millis(300)));
        assert!(slow.is_err_and(|e| e.to_string().contains("longer than")));
        assert!(started.elapsed() < Duration::from_secs(10));
        let started = std::time::Instant::now();
        let (out, cut) = bounded(sh("yes"), 4096, within(Duration::from_secs(20))).unwrap();
        assert!(cut && out.len() == 4096 && started.elapsed() < Duration::from_secs(10));
        let failed = bounded(sh("echo why >&2; exit 3"), 10, within(Duration::from_secs(20)));
        assert!(failed.is_err_and(|e| e.to_string() == "why"));
        assert_eq!(bounded(sh("printf ok"), 10, within(Duration::from_secs(20))).unwrap(), (b"ok".to_vec(), false));
    }

    /// Review M4 P2: a helper the command starts may hold its stdout or
    /// stderr open after it exits, or for ever. The pipes are read only
    /// until the deadline, or briefly after the command exits, and the
    /// helper is killed with the command's process group.
    #[cfg(unix)]
    #[test]
    fn a_helper_holding_the_pipes_never_holds_the_run() {
        let base = std::env::temp_dir().join(format!("skiff-helper-{}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        let pidfile = |n: &str| base.join(n);
        let sh = |script: String| {
            let mut c = Command::new("sh");
            c.args(["-c", &script]);
            c
        };
        let alive = |f: &Path| {
            let pid: i32 = std::fs::read_to_string(f).unwrap().trim().parse().unwrap();
            // SAFETY: signal 0 only asks whether the process exists.
            unsafe { libc::kill(pid, 0) == 0 }
        };
        let quick = |c, d| {
            let started = Instant::now();
            let r = bounded(c, 1 << 20, Deadline::after(d));
            (r, started.elapsed())
        };
        // Exits at once, a helper keeps stdout: git's output, without waiting for the helper.
        let (r, took) = quick(sh(format!("sleep 30 & echo $! > {}; echo out", pidfile("out").display())), Duration::from_secs(20));
        assert_eq!(r.unwrap(), (b"out\n".to_vec(), false));
        assert!(took < Duration::from_secs(3), "{took:?}");
        // A helper on stderr only.
        let (r, took) = quick(sh(format!("sleep 30 >/dev/null & echo $! > {}; echo out", pidfile("err").display())), Duration::from_secs(20));
        assert_eq!(r.unwrap(), (b"out\n".to_vec(), false));
        assert!(took < Duration::from_secs(3), "{took:?}");
        // Never exits, and its helper holds both: the deadline, not the helper.
        let (r, took) = quick(sh(format!("sleep 30 & echo $! > {}; sleep 30", pidfile("both").display())), Duration::from_millis(500));
        assert!(r.is_err_and(|e| e.to_string().contains("longer than")));
        assert!(took < Duration::from_secs(3), "{took:?}");
        // No helper is left running.
        std::thread::sleep(Duration::from_millis(100));
        let left: Vec<_> = ["out", "err", "both"].into_iter().filter(|n| alive(&pidfile(n))).collect();
        std::fs::remove_dir_all(&base).unwrap();
        assert!(left.is_empty(), "still running: {left:?}");
    }

    /// A private repository at `base/name` with one commit of `a.txt`.
    #[cfg(unix)]
    fn committed(base: &Path, name: &str, a: &str) -> PathBuf {
        let root = base.join(name);
        std::fs::create_dir_all(&root).unwrap();
        let run = |args: &[&str]| assert!(Command::new("git").arg("-C").arg(&root).args(args).status().unwrap().success());
        run(&["init", "-q"]);
        std::fs::write(root.join("a.txt"), a).unwrap();
        run(&["add", "a.txt"]);
        run(&["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false", "commit", "-qm", "init"]);
        root
    }

    /// An executable script at `path`.
    #[cfg(unix)]
    fn script(path: &Path, body: &str) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(path, format!("#!/bin/sh\n{body}")).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// Review M4 P2: git's status, revision and base runs before the diff
    /// share its deadline. An fsmonitor hook that stalls the status no
    /// longer stalls the request, and is killed with git.
    #[cfg(unix)]
    #[test]
    fn a_stalled_status_before_the_diff_is_bounded() {
        let base = std::env::temp_dir().join(format!("skiff-preflight-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let root = committed(&base, "repo", "1\n");
        let hook = base.join("hook");
        script(&hook, &format!("echo $$ > {}\nsleep 30\n", base.join("hook.pid").display()));
        assert!(Command::new("git").arg("-C").arg(&root).args(["config", "core.fsmonitor"]).arg(&hook).status().unwrap().success());
        std::fs::write(root.join("a.txt"), "2\n").unwrap();
        let held = Root::open(&root).unwrap();
        let started = Instant::now();
        let got = diff_within(&held, &root, Some("a.txt"), Deadline::after(Duration::from_secs(1)));
        let took = started.elapsed();
        std::thread::sleep(Duration::from_millis(100));
        let hook_pid: Option<i32> = std::fs::read_to_string(base.join("hook.pid")).ok().and_then(|p| p.trim().parse().ok());
        // SAFETY: signal 0 only asks whether the process exists.
        let hook_alive = hook_pid.is_some_and(|p| unsafe { libc::kill(p, 0) == 0 });
        std::fs::remove_dir_all(&base).unwrap();
        assert!(hook_pid.is_some(), "the hook never ran");
        assert!(got.is_err_and(|e| e.to_string().contains("longer than")));
        assert!(took < Duration::from_secs(4), "{took:?}");
        assert!(!hook_alive);
    }

    /// Review M4 P2: the worktree's path swapped for another repository
    /// between git runs, and swapped back before the last check. Git runs
    /// in the folder held open, so the other repository is never read. The
    /// swap is made by an fsmonitor hook of the worktree's own status run,
    /// and undone by one of the other repository's.
    #[cfg(unix)]
    #[test]
    fn git_runs_in_the_held_root_even_while_its_path_names_another() {
        let base = std::env::temp_dir().join(format!("skiff-anchor-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let root = committed(&base, "repo", "inside\n");
        let other = committed(&base, "other", "inside\n");
        std::fs::write(other.join("a.txt"), "OUTSIDE_PRIVATE_CONTENT\n").unwrap();
        std::fs::write(root.join("a.txt"), "changed\n").unwrap();
        let (moved, done) = (base.join("moved"), base.join("swapped"));
        let set = |repo: &Path, hook: &Path| assert!(Command::new("git").arg("-C").arg(repo).args(["config", "core.fsmonitor"]).arg(hook).status().unwrap().success());
        let (swap, restore) = (base.join("swap"), base.join("restore"));
        script(&swap, &format!(
            "[ -e {done} ] && exit 1\ntouch {done}\nmv {root} {moved}\nln -s {other} {root}\nexit 1\n",
            done = done.display(), root = root.display(), moved = moved.display(), other = other.display(),
        ));
        script(&restore, &format!(
            "[ -L {root} ] && rm {root} && mv {moved} {root}\nexit 1\n",
            root = root.display(), moved = moved.display(),
        ));
        set(&root, &swap);
        set(&other, &restore);
        let held = Root::open(&root).unwrap();
        let got = diff_in(&held, &root, Some("a.txt")).map_err(|e| e.to_string());
        let swapped = done.exists();
        // Whichever way it ended, the fixture is put back before it is removed.
        if root.is_symlink() {
            std::fs::remove_file(&root).unwrap();
            std::fs::rename(&moved, &root).unwrap();
        }
        std::fs::remove_dir_all(&base).unwrap();
        assert!(swapped, "the hook never swapped the path");
        let text = format!("{got:?}");
        assert!(!text.contains("OUTSIDE_PRIVATE_CONTENT"), "{text}");
        // The held folder's own diff, or a refusal: never the other's.
        assert!(got.as_ref().map_or_else(|e| e.contains("replaced"), |d| d.text.contains("+changed")), "{text}");
    }

    /// Review M4 P3: invalid UTF-8 becomes U+FFFD, three bytes for one, so
    /// the cap is on the text that is answered, and a cut says so.
    #[cfg(unix)]
    #[test]
    fn the_diff_limit_holds_after_invalid_utf8_is_replaced() {
        let base = std::env::temp_dir().join(format!("skiff-utf8cap-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let root = committed(&base, "repo", "1\n");
        let mut bytes = vec![0xffu8; 400_000];
        bytes.push(b'\n');
        std::fs::write(root.join("a.txt"), &bytes).unwrap();
        std::fs::write(root.join("new.txt"), &bytes).unwrap();
        let held = Root::open(&root).unwrap();
        let got: Vec<_> = ["a.txt", "new.txt"].iter().map(|f| diff_in(&held, &root, Some(f)).unwrap()).collect();
        std::fs::remove_dir_all(&base).unwrap();
        for d in got {
            assert!(d.truncated && !d.binary && d.text.len() <= DIFF_LIMIT, "{}: {}", d.truncated, d.text.len());
            assert!(d.text.is_empty() || d.text.ends_with('\n'));
        }
        // Under the limit, valid text is untouched.
        assert_eq!(cap("é\n".into(), false, DIFF_LIMIT), ("é\n".to_string(), false));
    }

    #[test]
    fn diff_command_gets_the_target() {
        let root = std::env::temp_dir().join(format!("skiff-diffcmd-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let run = |args: &[&str]| assert!(Command::new("git").arg("-C").arg(&root).args(args).status().unwrap().success());
        run(&["init", "-q"]);
        run(&["config", "user.email", "t@t"]);
        run(&["config", "user.name", "t"]);
        std::fs::write(root.join("a b.txt"), "1\n").unwrap();
        run(&["add", "."]);
        run(&["commit", "-qm", "init"]);
        std::fs::write(root.join("a b.txt"), "2\n").unwrap();
        std::fs::write(root.join("new.txt"), "n\n").unwrap();

        let plain = diff_text(&root, Some("a b.txt"), Some("git diff"), 80).unwrap();
        let untracked = diff_text(&root, Some("new.txt"), None, 80).unwrap();
        let placed = diff_text(&root, Some("a b.txt"), Some("git diff {target} | wc -l"), 80).unwrap();
        let failed = diff_text(&root, None, Some("false-command-that-is-missing"), 80);
        std::fs::remove_dir_all(&root).unwrap();
        assert!(plain.contains("-1") && plain.contains("+2"), "{plain}");
        assert!(untracked.contains("+++ b/new.txt"), "{untracked}");
        assert!(placed.trim().parse::<u32>().unwrap() > 3, "{placed}");
        assert!(failed.is_err());
    }

    #[test]
    fn parses_porcelain_z() {
        let out = "worktree /code/app\0HEAD aaa\0branch refs/heads/main\0\0\
                   worktree /code/app-worktrees/feat-x\0HEAD bbb\0branch refs/heads/feat/x\0locked\0\0\
                   worktree /tmp/gone\0HEAD ccc\0detached\0prunable gitdir file points to non-existent location\0\0";
        let wts = parse_worktrees(out);
        assert_eq!(wts.len(), 3);
        assert!(wts[0].is_main && !wts[1].is_main && !wts[2].is_main);
        assert_eq!(wts[0].branch.as_deref(), Some("main"));
        assert_eq!(wts[1].path, PathBuf::from("/code/app-worktrees/feat-x"));
        assert_eq!(wts[1].branch.as_deref(), Some("feat/x"));
        assert!(wts[1].locked && !wts[1].prunable);
        assert_eq!(wts[2].head, "ccc");
        assert_eq!(wts[2].branch, None);
        assert!(wts[2].prunable);
    }
}
