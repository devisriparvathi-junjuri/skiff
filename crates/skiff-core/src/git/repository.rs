//! Repository, worktree and branch operations, each a plain `git -C <dir>` call.

use super::{git_run, Deadline, GIT_TIMEOUT, PREFLIGHT_LIMIT};

use std::{
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{anyhow, bail, Context, Result};

use crate::project::Worktree;

/// Runs `git -C <dir> <args>` and returns stdout. A non-zero exit becomes an
/// error that carries git's stderr.
pub(super) fn git(dir: &Path, args: &[&str]) -> Result<String> {
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
pub(super) fn classify_repository(ok: bool, stderr: &str) -> Result<bool> {
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
pub(super) fn branch_base_with(run: impl Fn(&[&str]) -> Result<String>) -> Option<Base> {
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
