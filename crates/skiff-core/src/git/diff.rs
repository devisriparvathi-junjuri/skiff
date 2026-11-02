use super::{branch_base, branch_base_with, git, git_bounded, open_beneath, Beneath, Deadline, Root};

use crate::shell::shell_quote;

use std::{
    path::Path,
    process::{Command, Stdio},
    time::Duration,
};

use anyhow::{anyhow, bail, Context, Result};

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
pub(super) const GIT_TIMEOUT: Duration = Duration::from_secs(20);
/// The most a git run before the diff itself may print, in bytes.
pub(super) const PREFLIGHT_LIMIT: usize = 64 * 1024;
/// The most `git status` may print for one [`changes_in`], in bytes.
pub(super) const STATUS_LIMIT: usize = 4 * 1024 * 1024;

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

pub(super) fn diff_within(root: &Root, dir: &Path, file: Option<&str>, deadline: Deadline) -> Result<Diff> {
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
pub(super) fn untracked_diff(root: &Root, rel: &str) -> Result<Diff> {
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
