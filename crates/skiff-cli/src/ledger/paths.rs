use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rusqlite::Connection;

use super::{Row, GRAND};

/// A uid no row ever had: 8 hex characters with at least one letter.
pub(super) fn new_uid(db: &Connection) -> Result<String> {
    loop {
        let uid = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
        if uid.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        if db.execute("INSERT OR IGNORE INTO uids (uid) VALUES (?1)", [&uid])? == 1 {
            return Ok(uid);
        }
    }
}

/// Lowercase ASCII words joined by `-`, at most 40 characters. Other
/// characters split words. Empty text gives `task`.
fn slug(text: &str) -> String {
    let mut out = String::new();
    for w in text.split(|c: char| !c.is_ascii_alphanumeric()).filter(|w| !w.is_empty()) {
        let w = w.to_ascii_lowercase();
        let sep = usize::from(!out.is_empty());
        if out.len() + sep + w.len() > 40 {
            if out.is_empty() {
                out = w[..40].to_string();
            }
            break;
        }
        if sep == 1 {
            out.push('-');
        }
        out.push_str(&w);
    }
    if out.is_empty() {
        "task".into()
    } else {
        out
    }
}

/// A row's work folder, in `results`: the folder next to the ledger, or a
/// project or fleet Skipper's (see [`results_dir`]). Everything for the task
/// goes in it. The name is `<uid>-<slug>`, from the title, else the task
/// text, and never changes:
///
/// - a plan, a hand-off, or a task with no plan: `<results>/<uid>-<slug>/`;
/// - a task in a plan or under a hand-off: `<uid>-<slug>/` in that row's
///   folder (see [`container_folder`]).
///
/// Nothing is made on disk.
pub(super) fn work_folder(results: &Path, uid: &str, label: &str, container: Option<&Row>) -> PathBuf {
    let name = format!("{uid}-{}", slug(label));
    match container {
        Some(p) => container_folder(results, p).join(name),
        None => results.join(name),
    }
}

/// The result in a work folder: `plan.md` for a plan, else `result.md`.
pub(super) fn result_in(folder: &Path, plan: bool) -> PathBuf {
    folder.join(if plan { "plan.md" } else { "result.md" })
}

/// The title, else the task text: what a work folder's slug comes from.
pub(super) fn label<'a>(title: Option<&'a str>, task: &'a str) -> &'a str {
    title.map(str::trim).filter(|t| !t.is_empty()).unwrap_or(task)
}

/// The folder a task under `p` goes in: `p`'s work folder, or for a row from
/// before work folders, the plan folder it had (see [`plan_folder`]).
pub(super) fn container_folder(results: &Path, p: &Row) -> PathBuf {
    match &p.work_dir {
        Some(d) => PathBuf::from(d),
        None => plan_folder(results, p),
    }
}

/// Makes a work folder. The ledger records it only when this works.
pub(super) fn make_work_folder(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("make the work folder {}", dir.display()))
}

/// The results folder of `owner`'s rows: `results/` next to the ledger for
/// the grand Skipper, `projects/<project>/results/` for a project Skipper,
/// `fleets/<fleet>/results/` for a fleet Skipper.
pub fn results_dir(ledger: &Path, owner: &str) -> PathBuf {
    let dir = ledger.parent().unwrap_or(Path::new("."));
    match (owner, skiff_client::skipper::scope_fleet(owner)) {
        (GRAND, _) => dir.join("results"),
        (_, Some(fleet)) => dir.join("fleets").join(fleet).join("results"),
        _ => dir.join("projects").join(owner).join("results"),
    }
}

/// For a row from before work folders: the folder of the plan's `plan.md`,
/// or the folder of a hand-off's tasks. A plan whose result is elsewhere
/// gets the folder it would have had.
pub(super) fn plan_folder(results: &Path, p: &Row) -> PathBuf {
    if let Some(dir) = p.result_path.as_deref().map(Path::new).filter(|r| r.file_name() == Some("plan.md".as_ref())).and_then(Path::parent) {
        return dir.to_path_buf();
    }
    results.join(format!("{}-{}-{}", p.sent_at.get(..10).unwrap_or(&p.sent_at), p.uid, slug(&p.task)))
}
