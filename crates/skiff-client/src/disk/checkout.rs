//! Git checkouts in the tree: their repository, found by hand, and their state, asked of a git that runs no program.

use std::{
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

/// The state of a git checkout in the tree.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Checkout {
    Clean { linked: bool },
    Dirty,
    Ahead,
    NoUpstream,
    /// git failed, or its answer was not understood.
    Unknown,
}

pub(super) fn now_secs() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

pub(super) fn mtime(m: &std::fs::Metadata) -> i64 {
    m.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// git for a checkout Skiff did not make, pinned to its repository: no
/// search for another one, no lock, no lazy fetch, and none of the
/// checkout's settings that run a program (an fsmonitor or a hook).
pub(super) fn git(r: &Repo) -> Command {
    let mut c = Command::new("git");
    c.args(["-c", "core.fsmonitor=false", "-c", "core.hooksPath=/dev/null"])
        .arg(format!("--git-dir={}", r.gitdir.display()))
        .arg(format!("--work-tree={}", r.work.display()))
        .current_dir(&r.work)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_NO_LAZY_FETCH", "1")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE");
    c
}

/// A checkout's repository, found by hand, never by a search.
pub(super) struct Repo {
    pub(super) work: PathBuf,
    /// The checkout's git folder: `.git`, or a linked worktree's.
    pub(super) gitdir: PathBuf,
    /// The main repository's git folder.
    pub(super) common: PathBuf,
    /// A linked worktree: it goes with `git worktree remove`.
    pub(super) linked: bool,
}

/// The repository of the checkout at `dir`. `None` when `.git` is not a
/// git folder with a HEAD, or a `gitdir:` file that names one.
fn repo(dir: &Path) -> Option<Repo> {
    let dotgit = dir.join(".git");
    let meta = std::fs::symlink_metadata(&dotgit).ok()?;
    let (gitdir, linked) = if meta.is_dir() {
        (dotgit, false)
    } else if meta.is_file() {
        let text = std::fs::read_to_string(&dotgit).ok()?;
        let g = PathBuf::from(text.trim().strip_prefix("gitdir:")?.trim());
        (if g.is_absolute() { g } else { dir.join(g) }, true)
    } else {
        return None;
    };
    let common = match std::fs::read_to_string(gitdir.join("commondir")) {
        Ok(c) => {
            let c = PathBuf::from(c.trim());
            if c.is_absolute() { c } else { gitdir.join(c) }
        }
        Err(_) => gitdir.clone(),
    };
    if !gitdir.join("HEAD").is_file() || !common.join("config").is_file() {
        return None;
    }
    Some(Repo { work: dir.to_path_buf(), gitdir, common, linked })
}

/// The config sections and keys a plain checkout has. Anything else, such
/// as a filter, an include, an fsmonitor or a promisor remote, could make
/// git run a program or reach out: Skiff then does not run git there.
fn plain_key(section: &str, key: &str) -> bool {
    match section {
        "core" => matches!(
            key,
            "repositoryformatversion" | "filemode" | "bare" | "logallrefupdates" | "ignorecase" | "precomposeunicode" | "symlinks" | "autocrlf" | "eol" | "safecrlf"
        ),
        "remote" => matches!(key, "url" | "pushurl" | "fetch" | "push" | "tagopt" | "mirror" | "prune"),
        "branch" => matches!(key, "remote" | "merge" | "rebase" | "pushremote" | "description"),
        "user" => matches!(key, "name" | "email" | "signingkey"),
        "extensions" => matches!(key, "worktreeconfig" | "objectformat"),
        "lfs" => matches!(key, "repositoryformatversion" | "access" | "locksverify"),
        _ => false,
    }
}
/// Every key of the config file `f` is on [`plain_key`]'s list. git parses
/// the file (it runs nothing, and follows no include); a file git cannot
/// read is not plain.
pub(super) fn plain_config(f: &Path) -> bool {
    let out = Command::new("git")
        .args(["config", "--no-includes", "--null", "--list", "--file"])
        .arg(f)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("GIT_DIR")
        .output();
    let Some(out) = out.ok().filter(|o| o.status.success()) else { return false };
    plain_keys(&String::from_utf8_lossy(&out.stdout))
}

/// `git config --null --list` output: each entry is the key, a line break,
/// and the value. `section.key` or `section.subsection.key`.
fn plain_keys(list: &str) -> bool {
    list.split('\0').filter(|e| !e.is_empty()).all(|e| {
        let full = e.split('\n').next().unwrap_or("").to_ascii_lowercase();
        let (section, key) = (full.split('.').next().unwrap_or(""), full.rsplit('.').next().unwrap_or(""));
        full.contains('.') && plain_key(section, key)
    })
}

/// The repository of `dir` when its own config is plain; else `None`.
pub(super) fn plain_repo(dir: &Path) -> Option<Repo> {
    let r = repo(dir)?;
    let mut files = vec![r.common.join("config")];
    for w in [r.gitdir.join("config.worktree"), r.common.join("config.worktree")] {
        if w.exists() {
            files.push(w);
        }
    }
    if !files.iter().all(|f| plain_config(f)) {
        return None;
    }
    Some(r)
}

/// The state of the checkout at `dir`, from `git status`.
pub(super) fn checkout(dir: &Path) -> Checkout {
    let Some(r) = plain_repo(dir) else { return Checkout::Unknown };
    let linked = r.linked;
    let out = git(&r).args(["status", "--porcelain=v2", "--branch", "--ignore-submodules=all"]).output();
    let Some(out) = out.ok().filter(|o| o.status.success()) else { return Checkout::Unknown };
    let text = String::from_utf8_lossy(&out.stdout);
    let mut upstream = false;
    let mut ahead = 0;
    for line in text.lines() {
        if let Some(ab) = line.strip_prefix("# branch.ab ") {
            upstream = true;
            ahead = ab.split_whitespace().next().and_then(|a| a.trim_start_matches('+').parse::<i64>().ok()).unwrap_or(1);
        } else if line.starts_with("# branch.upstream ") {
            upstream = true;
        } else if !line.starts_with('#') && !line.is_empty() {
            return Checkout::Dirty;
        }
    }
    match (upstream, ahead) {
        (false, _) => Checkout::NoUpstream,
        (true, 0) => Checkout::Clean { linked },
        _ => Checkout::Ahead,
    }
}
