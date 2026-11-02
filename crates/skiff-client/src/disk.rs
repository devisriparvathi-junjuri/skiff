//! Settings › Disk use: the Skiff agent's folder as a tree, the one rule
//! that says what a flush may remove, and the flush.
//!
//! The rule judges each file. A file in a work folder (a ledger row's
//! `work_dir`) takes its folder's verdict: the folder is the task, and it
//! goes as one unit. A file outside the work folders stands alone.
//!
//! - Protected always: Skiff's own files, pinned images, files Skiff cannot
//!   read, and a git checkout that is dirty, ahead, has no upstream, or that
//!   git cannot read, with the work folder that holds it.
//! - In use: anything in a live pane's folder; a work folder whose row is
//!   not done, whose pane is working, or in which a file changed in the last
//!   [`HOLD_WORK`] seconds.
//! - Held: a file outside the work folders that changed in the last
//!   [`HOLD_LOOSE`] seconds.
//! - Anything else is stale, and a flush may remove it.
//!
//! Without the session list or the ledger, nothing is stale: the rule needs
//! both. The walk never follows a link: a link is a file of 0 bytes, and a
//! flush removes the link only.

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
};

#[cfg(test)]
use std::{
    process::Command,
    time::UNIX_EPOCH,
};

use skiff_core::session::{SessionInfo, SessionState};

use crate::ledger;

mod checkout;
mod model;

use checkout::{checkout, git, mtime, now_secs, plain_repo, Checkout, Repo};
#[cfg(test)]
use checkout::plain_config;
use model::{FOLD_FILES, KEEP};
pub use model::{words, Expected, Flushed, Kind, Node, Usage, Verdict, Work, HOLD_LOOSE, HOLD_WORK};

/// A walk of `abs`, never through a link. Counts what it cannot read.
fn walk(abs: &Path, rel: &str, bad: &mut u64) -> Node {
    let name = abs.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let mut n = Node {
        name: name.clone(),
        path: rel.to_string(),
        bytes: 0,
        files: 0,
        mtime: 0,
        state: None,
        children: Vec::new(),
        dir: false,
        frame: false,
        work: None,
        checkout: None,
        abs: abs.to_path_buf(),
    };
    let meta = match std::fs::symlink_metadata(abs) {
        Ok(m) => m,
        Err(_) => {
            *bad += 1;
            n.state = Some(Verdict::new(Kind::Prot, "unreadable"));
            n.files = 1;
            return n;
        }
    };
    n.mtime = mtime(&meta);
    if !meta.is_dir() {
        n.files = 1;
        n.bytes = if meta.file_type().is_symlink() { 0 } else { meta.len() };
        return n;
    }
    n.dir = true;
    n.name.push('/');
    let rd = match std::fs::read_dir(abs) {
        Ok(rd) => rd,
        Err(_) => {
            // A folder Skiff cannot list: one protected row.
            *bad += 1;
            n.state = Some(Verdict::new(Kind::Prot, "unreadable"));
            n.files = 1;
            return n;
        }
    };
    let mut kids: Vec<PathBuf> = Vec::new();
    for e in rd {
        match e {
            Ok(e) => kids.push(e.path()),
            Err(_) => *bad += 1,
        }
    }
    kids.sort();
    if kids.iter().any(|k| k.file_name().is_some_and(|f| f == ".git")) && !rel.is_empty() {
        n.checkout = Some(checkout(abs));
    }
    let own = n.mtime;
    for k in kids {
        let kname = k.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
        let krel = if rel.is_empty() { kname } else { format!("{rel}/{kname}") };
        let c = walk(&k, &krel, bad);
        n.bytes += c.bytes;
        n.files += c.files;
        n.children.push(c);
    }
    n.mtime = n.children.iter().filter(|c| c.files > 0).map(|c| c.mtime).max().unwrap_or(own);
    n
}

/// Skiff's own folders: the agent folder, `cache/`, `types/`, the results
/// folders and the Skippers' folders above them.
fn is_frame(rel: &str) -> bool {
    let parts: Vec<&str> = if rel.is_empty() { Vec::new() } else { rel.split('/').collect() };
    matches!(
        parts.as_slice(),
        [] | ["cache"] | ["results"] | ["types"] | ["projects"] | ["fleets"] | ["projects" | "fleets", _] | ["projects" | "fleets", _, "results"]
    )
}

/// A Skipper's own folder: its pane's cwd holds every result, so it
/// protects nothing.
fn is_home(rel: &str) -> bool {
    let parts: Vec<&str> = if rel.is_empty() { Vec::new() } else { rel.split('/').collect() };
    matches!(parts.as_slice(), [] | ["projects" | "fleets", _])
}

/// Skiff's own files: [`KEEP`] and `types/` at the top of the agent
/// folder, and [`KEEP`] in a Skipper's folder. A file of that name deeper
/// down, such as a repository's `AGENTS.md`, is not Skiff's.
fn kept_name(rel: &str) -> bool {
    let parts: Vec<&str> = rel.split('/').collect();
    match parts.as_slice() {
        [first, ..] if *first == "types" || KEEP.contains(first) => true,
        ["projects" | "fleets", _, name, ..] => KEEP.contains(name),
        _ => false,
    }
}

/// A work folder's row and what the ledger and the panes say of it.
struct Unit {
    work: Work,
    /// The verdict from the row and its pane, before the files and the
    /// protections: `None` when the row is done and its pane is not working.
    live: Option<Verdict>,
}

/// What the rule reads besides the files.
struct Signals {
    now: i64,
    units: HashMap<PathBuf, Unit>,
    /// Live panes' folders inside the agent folder, but not a Skipper's own.
    cwds: Vec<(PathBuf, String)>,
    pinned: HashSet<PathBuf>,
    /// The result files of rows that are not done, by their exact path: a
    /// row from before work folders, or one with `--result` elsewhere.
    results: HashMap<PathBuf, Verdict>,
    cache: PathBuf,
    /// The session list and the ledger were read: without them nothing is stale.
    sure: Option<&'static str>,
}

/// A row is not done: a task or hand-off that is not `done`, or a plan with
/// a task that is not done, or with no task.
fn not_done(r: &ledger::Row, rows: &[ledger::Row]) -> bool {
    if r.is_plan() {
        let kids: Vec<&ledger::Row> = rows.iter().filter(|k| k.parent.as_deref() == Some(r.uid.as_str())).collect();
        kids.is_empty() || kids.iter().any(|k| k.state != "done")
    } else {
        r.state != "done"
    }
}

fn title_of(r: &ledger::Row) -> String {
    r.title.clone().filter(|t| !t.trim().is_empty()).unwrap_or_else(|| r.task.lines().next().unwrap_or("").to_string())
}

fn open_why(r: &ledger::Row) -> &'static str {
    if r.is_plan() {
        "plan-open"
    } else if r.state == "sent" {
        "working"
    } else {
        "open"
    }
}

/// The result files of the rows that are not done. The ledger names each
/// path, so the match is exact.
fn open_results(rows: &[ledger::Row]) -> HashMap<PathBuf, Verdict> {
    rows.iter()
        .filter(|r| not_done(r, rows))
        .filter_map(|r| {
            let p = Path::new(r.result_path.as_deref()?).canonicalize().ok()?;
            Some((p, Verdict::new(Kind::Use, open_why(r)).label(title_of(r))))
        })
        .collect()
}

/// The work folders of the ledger's rows, each with its live verdict.
fn units(rows: &[ledger::Row], sessions: Option<&[SessionInfo]>, socket: &Path) -> HashMap<PathBuf, Unit> {
    let mut out = HashMap::new();
    for r in rows {
        let Some(dir) = r.work_dir.as_deref().and_then(|d| Path::new(d).canonicalize().ok()) else { continue };
        let title = title_of(r);
        let live = if not_done(r, rows) {
            Some(Verdict::new(Kind::Use, open_why(r)).label(title.clone()))
        } else if r.socket_other_than(socket).is_some() {
            // Its pane is on another skiffd: it cannot be checked from here.
            Some(Verdict::new(Kind::Use, "pane-working").label("another skiffd"))
        } else {
            let pane = r.pane_id.as_deref().and_then(|p| sessions.and_then(|all| all.iter().find(|s| s.id == p)));
            pane.filter(|s| matches!(s.state, SessionState::Working | SessionState::Waiting))
                .map(|s| Verdict::new(Kind::Use, "pane-working").label(pane_name(s)))
        };
        out.insert(dir, Unit { work: Work { uid: r.uid.clone(), kind: r.kind.clone(), title }, live });
    }
    out
}

/// The name a person gave the pane, else its label.
fn pane_name(s: &SessionInfo) -> String {
    s.name.clone().unwrap_or_else(|| s.label.clone())
}

/// The verdict of a checkout that is not clean.
fn checkout_hold(c: Checkout) -> Option<Verdict> {
    match c {
        Checkout::Clean { .. } => None,
        Checkout::Dirty => Some(Verdict::new(Kind::Prot, "dirty")),
        Checkout::Ahead => Some(Verdict::new(Kind::Prot, "ahead")),
        Checkout::NoUpstream => Some(Verdict::new(Kind::Prot, "no-upstream")),
        Checkout::Unknown => Some(Verdict::new(Kind::Prot, "checkout")),
    }
}

/// Every file under `n`, with the nested work folders left out when `own`.
fn files_of<'a>(n: &'a Node, own: bool, units: &HashMap<PathBuf, Unit>, out: &mut Vec<&'a Node>) {
    for c in &n.children {
        if own && c.dir && units.contains_key(&c.abs) {
            continue;
        }
        if c.dir && c.state.is_none() {
            files_of(c, own, units, out);
        } else {
            out.push(c);
        }
    }
}

/// The checkouts under `n`, nested work folders left out.
fn checkouts_of(n: &Node, units: &HashMap<PathBuf, Unit>, out: &mut Vec<Checkout>) {
    if let Some(c) = n.checkout {
        out.push(c);
    }
    for c in &n.children {
        if c.dir && !units.contains_key(&c.abs) {
            checkouts_of(c, units, out);
        }
    }
}

fn cwd_in<'a>(sig: &'a Signals, abs: &Path) -> Option<&'a str> {
    sig.cwds.iter().find(|(c, _)| c.starts_with(abs)).map(|(_, n)| n.as_str())
}

/// The verdict of a work folder: the protections of its own files, then the
/// row and its pane, then the last change.
fn judge_unit(n: &Node, sig: &Signals) -> Verdict {
    let u = &sig.units[&n.abs];
    let mut own = Vec::new();
    files_of(n, true, &sig.units, &mut own);
    if let Some(v) = own.iter().filter_map(|f| f.state.as_ref()).find(|v| v.why == "unreadable") {
        return v.clone().label(u.work.title.clone());
    }
    let mut cos = Vec::new();
    checkouts_of(n, &sig.units, &mut cos);
    if let Some(v) = cos.into_iter().find_map(checkout_hold) {
        return v.label(u.work.title.clone());
    }
    if let Some(name) = cwd_in(sig, &n.abs) {
        return Verdict::new(Kind::Use, "pane").label(name);
    }
    if let Some(v) = &u.live {
        return v.clone();
    }
    let newest = own.iter().map(|f| f.mtime).max();
    if let Some(t) = newest.filter(|t| sig.now - t < HOLD_WORK) {
        return Verdict { changed: Some(t), ..Verdict::new(Kind::Use, "changed").label(u.work.title.clone()) };
    }
    Verdict::new(Kind::Stale, "done").label(u.work.title.clone())
}

/// The verdict of a file outside the work folders.
fn judge_loose(f: &Node, sig: &Signals) -> Verdict {
    if let Some(v) = &f.state {
        return v.clone();
    }
    if kept_name(&f.path) || crate::cache::is_index_file(&f.abs) && f.abs.starts_with(&sig.cache) {
        return Verdict::new(Kind::Prot, "skiff-file");
    }
    if sig.pinned.contains(&f.abs) {
        return Verdict::new(Kind::Prot, "pinned");
    }
    if let Some(v) = sig.results.get(&f.abs) {
        return v.clone();
    }
    if sig.now - f.mtime < HOLD_LOOSE {
        return Verdict { changed: Some(f.mtime), ..Verdict::new(Kind::Held, "changed") };
    }
    Verdict::new(Kind::Stale, if f.abs.starts_with(&sig.cache) { "image" } else { "no-folder" })
}

/// Gives every file under `n` its verdict. `over`: the verdict a work
/// folder, a checkout or a pane's folder above gave its files.
fn judge(n: &mut Node, sig: &Signals, over: Option<&Verdict>) {
    n.frame = n.dir && is_frame(&n.path);
    let mut here = over.cloned();
    if n.dir && n.state.is_none() {
        if here.is_none() && !is_home(&n.path) {
            if let Some((_, name)) = sig.cwds.iter().find(|(c, _)| *c == n.abs) {
                here = Some(Verdict::new(Kind::Use, "pane").label(name.clone()));
            }
        }
        if let Some(u) = sig.units.get(&n.abs) {
            n.work = Some(u.work.clone());
            // A pane's folder above keeps the work folder in use too.
            if here.as_ref().is_none_or(|v| v.kind != Kind::Use || v.why != "pane") {
                here = Some(judge_unit(n, sig));
            }
        } else if let (Some(c), None) = (n.checkout, &here) {
            let mut all = Vec::new();
            files_of(n, false, &sig.units, &mut all);
            let newest = all.iter().map(|f| f.mtime).max().unwrap_or(n.mtime);
            here = Some(match checkout_hold(c) {
                Some(v) => v,
                None if cwd_in(sig, &n.abs).is_some() => Verdict::new(Kind::Use, "pane").label(cwd_in(sig, &n.abs).unwrap_or_default()),
                None if sig.now - newest < HOLD_LOOSE => Verdict { changed: Some(newest), ..Verdict::new(Kind::Held, "changed") },
                None => Verdict::new(Kind::Stale, "clean-checkout"),
            });
        }
        // A work folder's own files take its verdict; a nested work folder judges its own.
        let pass = here.clone();
        for c in &mut n.children {
            let sub = if c.dir && sig.units.contains_key(&c.abs) && pass.as_ref().is_none_or(|v| v.why != "pane") { None } else { pass.as_ref() };
            judge(c, sig, sub);
        }
        if n.children.is_empty() {
            n.state = Some(here.unwrap_or_else(|| judge_loose(n, sig)));
        }
        return;
    }
    let v = match (&n.state, here) {
        // Unreadable stays protected under any verdict.
        (Some(v), _) => v.clone(),
        (None, Some(v)) => v,
        (None, None) => judge_loose(n, sig),
    };
    n.state = Some(v);
}

/// Without the session list or the ledger, nothing is stale.
fn unsure(n: &mut Node, why: &'static str) {
    if let Some(v) = &mut n.state {
        if v.kind == Kind::Stale {
            *v = Verdict::new(Kind::Prot, why);
        }
    }
    for c in &mut n.children {
        unsure(c, why);
    }
}

/// Folds a folder whose files all have one verdict and that has many files.
fn fold(n: &mut Node) {
    for c in &mut n.children {
        fold(c);
    }
    if !n.dir || n.frame || n.work.is_some() || n.files <= FOLD_FILES || n.children.is_empty() {
        return;
    }
    let mut vs = Vec::new();
    collect_states(n, &mut vs);
    if vs.windows(2).all(|w| w[0] == w[1]) {
        n.state = vs.into_iter().next();
        n.children.clear();
    }
}

fn collect_states(n: &Node, out: &mut Vec<Verdict>) {
    if n.children.is_empty() {
        if let Some(v) = &n.state {
            out.push(v.clone());
        }
    }
    for c in &n.children {
        collect_states(c, out);
    }
}

/// Counts and judges `folder`. `cache`: the image cache. `ledger`: the
/// ledger file. `sessions`: the daemon's list, `None` when it did not answer.
pub fn usage(folder: &Path, cache: &Path, ledger_file: &Path, sessions: Option<&[SessionInfo]>, socket: &Path) -> Usage {
    let mut u = raw(folder, cache, ledger_file, sessions, socket, now_secs());
    fold(&mut u.tree);
    u
}

/// [`usage`] at `now`, unfolded: the flush finds every path in it.
fn raw(folder: &Path, cache: &Path, ledger_file: &Path, sessions: Option<&[SessionInfo]>, socket: &Path, now: i64) -> Usage {
    let root = folder.canonicalize().unwrap_or_else(|_| folder.to_path_buf());
    let (rows, ledger_error) = if ledger_file.exists() {
        match ledger::open_read(ledger_file).and_then(|db| ledger::rows(&db, "", None)) {
            Ok(r) => (r, None),
            Err(e) => (Vec::new(), Some(format!("{e:#}"))),
        }
    } else {
        (Vec::new(), None)
    };
    let cwds = sessions
        .unwrap_or_default()
        .iter()
        .filter(|s| s.state != SessionState::Done)
        .filter_map(|s| s.cwd.canonicalize().ok().map(|c| (c, pane_name(s))))
        .filter(|(c, _): &(PathBuf, String)| c.strip_prefix(&root).is_ok_and(|rel| !is_home(&rel.to_string_lossy())))
        .collect();
    let sig = Signals {
        now,
        units: units(&rows, sessions, socket),
        cwds,
        pinned: crate::cache::kept(cache),
        results: open_results(&rows),
        cache: cache.canonicalize().unwrap_or_else(|_| cache.to_path_buf()),
        sure: if sessions.is_none() {
            Some("no-daemon")
        } else if ledger_error.is_some() {
            Some("no-ledger")
        } else {
            None
        },
    };
    let mut bad = 0;
    let mut tree = walk(&root, "", &mut bad);
    tree.name = String::new();
    judge(&mut tree, &sig, None);
    if let Some(why) = sig.sure {
        unsure(&mut tree, why);
    }
    Usage {
        folder: root,
        tree,
        now,
        daemon: sessions.is_some(),
        ledger_error,
        unreadable: bad,
        hold_work: HOLD_WORK,
        hold_loose: HOLD_LOOSE,
    }
}

fn find<'a>(n: &'a Node, path: &str) -> Option<&'a Node> {
    if n.path == path {
        return Some(n);
    }
    n.children.iter().filter(|c| path == c.path || path.starts_with(&format!("{}/", c.path))).find_map(|c| find(c, path))
}

/// Every file under `n` is stale, and none of Skiff's folders is in it.
fn all_stale(n: &Node) -> bool {
    if n.frame {
        return false;
    }
    if n.children.is_empty() {
        // An empty folder goes with its stale parent.
        return n.state.as_ref().is_some_and(Verdict::stale);
    }
    n.children.iter().all(all_stale)
}

fn expected_of(n: &Node) -> Expected {
    Expected { path: n.path.clone(), bytes: n.bytes, files: n.files, mtime: n.mtime }
}

/// What a flush of `n` removes: each largest part whose files are all stale.
/// The page does the same in `diskTree.ts`.
fn plan(n: &Node, out: &mut Vec<Expected>) {
    if !n.path.is_empty() && all_stale(n) {
        out.push(expected_of(n));
        return;
    }
    for c in &n.children {
        plan(c, out);
    }
}

/// Removes the clean linked worktrees under `n` with git, first: a plain
/// delete would leave the main repository's record of it.
fn remove_worktrees(n: &Node) -> Result<(), String> {
    if let Some(Checkout::Clean { linked: true }) = n.checkout {
        // The check again, just before the remove: it reads the config again too.
        if checkout(&n.abs) != (Checkout::Clean { linked: true }) {
            return Err(format!("{} changed", n.path));
        }
        let r = plain_repo(&n.abs).ok_or_else(|| format!("{}: not a plain git checkout", n.path))?;
        let main = r.common.parent().ok_or_else(|| format!("{}: no main repository", n.path))?.to_path_buf();
        let main = Repo { work: main, gitdir: r.common.clone(), common: r.common.clone(), linked: false };
        let out = git(&main).args(["worktree", "remove"]).arg(&n.abs).output().map_err(|e| e.to_string())?;
        if !out.status.success() {
            return Err(format!("git worktree remove {}: {}", n.path, String::from_utf8_lossy(&out.stderr).trim()));
        }
        return Ok(());
    }
    for c in &n.children {
        remove_worktrees(c)?;
    }
    Ok(())
}

/// The folder that holds `p` is still the real folder the walk saw: no
/// part of the path became a link since. The walk never follows a link, so
/// each path it gives has a real folder above it.
fn same_parent(p: &Path) -> bool {
    p.parent().is_some_and(|d| d.canonicalize().is_ok_and(|c| c == d))
}

/// Removes the files of `n` that did not change since `newest`, one at a
/// time, then the folders that are now empty, deepest first.
fn remove_files(n: &Node, newest: i64, out: &mut Flushed) {
    if n.dir {
        for c in &n.children {
            remove_files(c, newest, out);
        }
        if same_parent(&n.abs) {
            let _ = std::fs::remove_dir(&n.abs);
        }
        return;
    }
    if !same_parent(&n.abs) {
        out.kept += 1;
        return;
    }
    let Ok(m) = std::fs::symlink_metadata(&n.abs) else { return };
    if m.is_dir() || mtime(&m) > newest {
        out.kept += 1;
        return;
    }
    let bytes = if m.file_type().is_symlink() { 0 } else { m.len() };
    if std::fs::remove_file(&n.abs).is_ok() {
        out.files += 1;
        out.bytes += bytes;
    }
}

/// Flushes `items`, each as the count showed it. Counts again first: an item
/// goes only when it is still all stale and has the same size, file count
/// and newest change. Each file is checked again just before its remove.
/// The rows whose work folders went get `flushed_at`.
pub fn flush(folder: &Path, cache: &Path, ledger_file: &Path, sessions: Option<&[SessionInfo]>, socket: &Path, items: &[Expected]) -> Flushed {
    flush_at(folder, cache, ledger_file, sessions, socket, items, now_secs())
}

fn flush_at(folder: &Path, cache: &Path, ledger_file: &Path, sessions: Option<&[SessionInfo]>, socket: &Path, items: &[Expected], now: i64) -> Flushed {
    let fresh = raw(folder, cache, ledger_file, sessions, socket, now);
    let mut out = Flushed::default();
    let mut gone = Vec::new();
    for e in items {
        let Some(n) = find(&fresh.tree, &e.path).filter(|n| !n.path.is_empty() && all_stale(n) && expected_of(n) == *e) else {
            out.kept += 1;
            continue;
        };
        if let Err(err) = remove_worktrees(n) {
            out.kept += 1;
            out.errors.push(err);
            continue;
        }
        remove_files(n, e.mtime, &mut out);
        collect_units(n, &mut gone);
    }
    let gone: Vec<String> = gone.into_iter().filter(|(_, abs)| !abs.exists()).map(|(uid, _)| uid).collect();
    out.rows = stamp(ledger_file, &gone);
    out
}

fn collect_units(n: &Node, out: &mut Vec<(String, PathBuf)>) {
    if let Some(w) = &n.work {
        out.push((w.uid.clone(), n.abs.clone()));
    }
    for c in &n.children {
        collect_units(c, out);
    }
}

/// Stamps `flushed_at` on the rows `uids`. A ledger without the column (an
/// older `skiff`) is left as it is.
fn stamp(ledger_file: &Path, uids: &[String]) -> u64 {
    if uids.is_empty() {
        return 0;
    }
    let Ok(db) = ledger::open_write(ledger_file) else { return 0 };
    let mut n = 0;
    for uid in uids {
        n += db.execute("UPDATE tasks SET flushed_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now') WHERE uid = ?1", [uid]).unwrap_or(0) as u64;
    }
    n
}

/// Auto flush: the stale parts of the folder whose last change is older
/// than `days` days, by the same rule. `0` does nothing.
pub fn flush_old(folder: &Path, cache: &Path, ledger_file: &Path, sessions: Option<&[SessionInfo]>, socket: &Path, days: u32) -> Flushed {
    flush_old_at(folder, cache, ledger_file, sessions, socket, days, now_secs())
}

fn flush_old_at(folder: &Path, cache: &Path, ledger_file: &Path, sessions: Option<&[SessionInfo]>, socket: &Path, days: u32, now: i64) -> Flushed {
    if days == 0 {
        return Flushed::default();
    }
    let u = raw(folder, cache, ledger_file, sessions, socket, now);
    let mut items = Vec::new();
    plan(&u.tree, &mut items);
    items.retain(|e| now - e.mtime > i64::from(days) * 86_400);
    flush_at(folder, cache, ledger_file, sessions, socket, &items, now)
}

/// For `skiff ledger prune --flush` and `rm --flush`: flushes each work
/// folder in `dirs` that the rule finds all stale, before its row goes.
/// Answers what went, and each folder that stays with the reason.
pub fn flush_work(folder: &Path, cache: &Path, ledger_file: &Path, sessions: Option<&[SessionInfo]>, socket: &Path, dirs: &[PathBuf]) -> (Flushed, Vec<(PathBuf, String)>) {
    let now = now_secs();
    let u = raw(folder, cache, ledger_file, sessions, socket, now);
    let mut items = Vec::new();
    let mut stays = Vec::new();
    for d in dirs {
        let Some(rel) = d.canonicalize().ok().and_then(|a| a.strip_prefix(&u.folder).ok().map(|r| r.to_string_lossy().replace('\\', "/"))) else {
            continue;
        };
        match find(&u.tree, &rel) {
            Some(n) if all_stale(n) => items.push(expected_of(n)),
            Some(n) => stays.push((d.clone(), why_kept(n))),
            None => {}
        }
    }
    (flush_at(folder, cache, ledger_file, sessions, socket, &items, now), stays)
}

/// The first reason a part of `n` stays, in words.
fn why_kept(n: &Node) -> String {
    let mut vs = Vec::new();
    collect_states(n, &mut vs);
    let v = vs.into_iter().find(|v| !v.stale());
    match v {
        None => "it is empty".into(),
        Some(v) => words(&v),
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use std::fs::File;

    const NOW: i64 = 2_000_000_000;
    const SOCK: &str = "/tmp/skiff-disk-test.sock";

    fn write(p: &Path, text: &str, age: i64) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
        age_of(p, age);
    }

    fn age_of(p: &Path, age: i64) {
        let t = UNIX_EPOCH + std::time::Duration::from_secs((NOW - age) as u64);
        File::options().write(true).open(p).unwrap().set_modified(t).unwrap();
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("skiff-disk-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.canonicalize().unwrap()
    }

    /// A test row: uid, kind, parent, state, pane, work folder.
    type TestRow<'a> = (&'a str, &'a str, Option<&'a str>, &'a str, Option<&'a str>, &'a Path);

    /// A ledger with the version 11 columns.
    fn ledger(dir: &Path, rows: &[TestRow]) -> PathBuf {
        let file = dir.join("ledger.db");
        let db = Connection::open(&file).unwrap();
        db.execute_batch(
            "CREATE TABLE tasks (id INTEGER PRIMARY KEY, uid TEXT, kind TEXT, parent TEXT, pane_id TEXT, pane_gen INTEGER, project TEXT, task TEXT, sent_at TEXT,
               state TEXT, result_path TEXT, ref TEXT, updated_at TEXT, type TEXT, socket TEXT, reported_at TEXT, title TEXT, closed_at TEXT, gone_at TEXT,
               owner TEXT NOT NULL DEFAULT 'grand', reopened_at TEXT, work_dir TEXT, flushed_at TEXT);",
        )
        .unwrap();
        for (uid, kind, parent, st, pane, wd) in rows {
            std::fs::create_dir_all(wd).unwrap();
            db.execute(
                "INSERT INTO tasks (uid, kind, parent, pane_id, project, task, sent_at, state, updated_at, title, socket, work_dir)
                 VALUES (?1, ?2, ?3, ?4, '', 't', 'x', ?5, 'x', ?6, ?7, ?8)",
                rusqlite::params![uid, kind, parent, pane, st, format!("Title {uid}"), SOCK, wd.to_string_lossy()],
            )
            .unwrap();
        }
        file
    }

    fn pane(id: &str, state: &str, cwd: &Path) -> SessionInfo {
        serde_json::from_value(serde_json::json!({
            "id": id, "label": id, "role": "agent", "cwd": cwd, "command": "claude", "args": [],
            "state": state, "cols": 80, "rows": 24, "pid": 1, "exit_code": null, "started_at": 1,
        }))
        .unwrap()
    }

    fn at(dir: &Path, l: &Path, s: &[SessionInfo]) -> Usage {
        raw(dir, &dir.join("cache"), l, Some(s), Path::new(SOCK), NOW)
    }

    fn state_of(u: &Usage, path: &str) -> (Kind, &'static str) {
        let n = find(&u.tree, path).unwrap_or_else(|| panic!("no {path}"));
        let mut vs = Vec::new();
        collect_states(n, &mut vs);
        let v = &vs[0];
        assert!(vs.iter().all(|x| x == v), "{path}: mixed {vs:?}");
        (v.kind, v.why)
    }

    #[test]
    // SPLIT-SCAFFOLD
    fn disk_tree_json_keeps_public_values_and_omits_empty_fields() {
        assert_eq!(serde_json::to_value([Kind::Use, Kind::Held, Kind::Prot, Kind::Stale]).unwrap(), serde_json::json!(["use", "held", "prot", "stale"]));
        let empty = Node {
            name: "empty".into(),
            path: "empty".into(),
            bytes: 0,
            files: 0,
            mtime: NOW,
            state: None,
            children: vec![],
            dir: false,
            frame: false,
            work: None,
            checkout: None,
            abs: PathBuf::new(),
        };
        assert_eq!(
            serde_json::to_value(&empty).unwrap(),
            serde_json::json!({"name": "empty", "path": "empty", "bytes": 0, "files": 0, "mtime": NOW}),
        );
        let full = Node {
            state: Some(Verdict { kind: Kind::Stale, why: "done", label: "Task".into(), changed: Some(NOW) }),
            children: vec![empty],
            dir: true,
            frame: true,
            work: Some(Work { uid: "a1".into(), kind: "task".into(), title: "Task".into() }),
            ..Node {
                name: "full/".into(),
                path: "full".into(),
                bytes: 1,
                files: 1,
                mtime: NOW,
                state: None,
                children: vec![],
                dir: false,
                frame: false,
                work: None,
                checkout: None,
                abs: PathBuf::new(),
            }
        };
        assert_eq!(
            serde_json::to_value(&full).unwrap(),
            serde_json::json!({
                "name": "full/", "path": "full", "bytes": 1, "files": 1, "mtime": NOW,
                "state": {"kind": "stale", "why": "done", "label": "Task", "changed": NOW},
                "children": [{"name": "empty", "path": "empty", "bytes": 0, "files": 0, "mtime": NOW}],
                "dir": true, "frame": true, "work": {"uid": "a1", "kind": "task", "title": "Task"},
            }),
        );
    }

    #[test]
    // SPLIT-SCAFFOLD
    fn a_requested_non_stale_expected_cannot_delete_its_file() {
        let dir = scratch("requested-held");
        let ledger_file = ledger(&dir, &[]);
        let file = dir.join("results/fresh.md");
        write(&file, "fresh", 60);
        let usage = at(&dir, &ledger_file, &[]);
        assert_eq!(state_of(&usage, "results/fresh.md"), (Kind::Held, "changed"));
        let expected = expected_of(find(&usage.tree, "results/fresh.md").unwrap());
        let out = flush_at(&dir, &dir.join("cache"), &ledger_file, Some(&[]), Path::new(SOCK), &[expected], NOW);
        assert_eq!((out.files, out.kept), (0, 1));
        assert!(file.exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_done_work_folder_is_stale_after_15_minutes_and_goes_as_one() {
        let dir = scratch("done");
        let r = dir.join("results");
        let (p, a, b) = (r.join("p1-plan"), r.join("p1-plan/a1-task"), r.join("p1-plan/b1-task"));
        let l = ledger(&dir, &[("p1", "plan", None, "sent", None, &p), ("a1", "task", Some("p1"), "done", Some("pa"), &a), ("b1", "task", Some("p1"), "sent", Some("pb"), &b)]);
        write(&a.join("result.md"), "a", 3600);
        write(&a.join("shots/one.png"), "png", 3600);
        write(&b.join("result.md"), "b", 3600);
        let s = [pane("pa", "idle", Path::new("/")), pane("pb", "working", Path::new("/"))];
        let u = at(&dir, &l, &s);
        assert_eq!(state_of(&u, "results/p1-plan/a1-task"), (Kind::Stale, "done"));
        assert_eq!(state_of(&u, "results/p1-plan/b1-task"), (Kind::Use, "working"));
        // The plan folder is mixed: only the done task goes.
        let mut items = Vec::new();
        plan(&find(&u.tree, "results/p1-plan").unwrap().clone(), &mut items);
        assert_eq!(items.iter().map(|e| e.path.as_str()).collect::<Vec<_>>(), ["results/p1-plan/a1-task"]);
        // A change 5 minutes ago holds the folder.
        age_of(&a.join("shots/one.png"), 300);
        assert_eq!(state_of(&at(&dir, &l, &s), "results/p1-plan/a1-task"), (Kind::Use, "changed"));
        age_of(&a.join("shots/one.png"), 3600);
        // Its pane working again holds it.
        let busy = [pane("pa", "working", Path::new("/")), pane("pb", "working", Path::new("/"))];
        assert_eq!(state_of(&at(&dir, &l, &busy), "results/p1-plan/a1-task"), (Kind::Use, "pane-working"));
        // The flush removes the folder, file by file, and stamps the row.
        let out = flush_at(&dir, &dir.join("cache"), &l, Some(&s), Path::new(SOCK), &items, NOW);
        assert_eq!((out.files, out.rows, out.kept), (2, 1, 0));
        assert!(!a.exists() && b.join("result.md").exists() && p.exists());
        let flushed: Option<String> = Connection::open(&l).unwrap().query_row("SELECT flushed_at FROM tasks WHERE uid = 'a1'", [], |r| r.get(0)).unwrap();
        assert!(flushed.is_some());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn outside_the_work_folders_a_file_is_held_for_a_day() {
        let dir = scratch("loose");
        let l = ledger(&dir, &[]);
        write(&dir.join("results/2026-10-01-old.md"), "old", 2 * 86_400);
        write(&dir.join("results/new-notes/n.md"), "new", 3600);
        write(&dir.join("scratch/thing/x.bin"), "x", 3 * 86_400);
        let u = at(&dir, &l, &[]);
        assert_eq!(state_of(&u, "results/2026-10-01-old.md"), (Kind::Stale, "no-folder"));
        assert_eq!(state_of(&u, "results/new-notes"), (Kind::Held, "changed"));
        assert_eq!(state_of(&u, "scratch"), (Kind::Stale, "no-folder"));
        let mut items = Vec::new();
        plan(&u.tree, &mut items);
        let paths: Vec<_> = items.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(paths, ["results/2026-10-01-old.md", "scratch"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_result_file_of_an_older_row_that_is_not_done_stays_past_a_day() {
        let dir = scratch("older-row");
        let l = ledger(&dir, &[]);
        let (open, done) = (dir.join("results/2026-10-01-ab12cd34-long.md"), dir.join("results/2026-10-01-cd34ef56-over.md"));
        write(&open, "wip", 3 * 86_400);
        write(&done, "r", 3 * 86_400);
        let db = Connection::open(&l).unwrap();
        for (uid, st, p) in [("ab12cd34", "sent", &open), ("cd34ef56", "done", &done)] {
            db.execute(
                "INSERT INTO tasks (uid, kind, project, task, sent_at, state, updated_at, title, result_path) VALUES (?1, 'task', '', 't', 'x', ?2, 'x', 'Long', ?3)",
                rusqlite::params![uid, st, p.to_string_lossy()],
            )
            .unwrap();
        }
        let u = at(&dir, &l, &[]);
        assert_eq!(state_of(&u, "results/2026-10-01-ab12cd34-long.md"), (Kind::Use, "working"));
        assert_eq!(state_of(&u, "results/2026-10-01-cd34ef56-over.md"), (Kind::Stale, "no-folder"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn skiff_files_pinned_images_and_unreadable_folders_are_protected() {
        let dir = scratch("prot");
        let l = ledger(&dir, &[]);
        write(&dir.join("AGENTS.md"), "a", 9 * 86_400);
        write(&dir.join("types/review.md"), "t", 9 * 86_400);
        write(&dir.join("projects/web/SKIPPER.md"), "s", 9 * 86_400);
        write(&dir.join("cache/aaaa.png"), "png", 9 * 86_400);
        write(&dir.join("cache/bbbb.png"), "png", 9 * 86_400);
        crate::cache::pin(&dir.join("cache"), &dir.join("cache/aaaa.png"), true).unwrap();
        age_of(&dir.join("cache/images.json"), 9 * 86_400);
        write(&dir.join("results/locked/x.md"), "x", 9 * 86_400);
        let locked = dir.join("results/locked");
        std::fs::set_permissions(&locked, std::os::unix::fs::PermissionsExt::from_mode(0o000)).unwrap();
        let u = at(&dir, &l, &[]);
        std::fs::set_permissions(&locked, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        assert_eq!(state_of(&u, "AGENTS.md"), (Kind::Prot, "skiff-file"));
        assert_eq!(state_of(&u, "ledger.db"), (Kind::Prot, "skiff-file"));
        assert_eq!(state_of(&u, "types"), (Kind::Prot, "skiff-file"));
        assert_eq!(state_of(&u, "projects/web/SKIPPER.md"), (Kind::Prot, "skiff-file"));
        assert_eq!(state_of(&u, "cache/aaaa.png"), (Kind::Prot, "pinned"));
        assert_eq!(state_of(&u, "cache/images.json"), (Kind::Prot, "skiff-file"));
        assert_eq!(state_of(&u, "cache/bbbb.png"), (Kind::Stale, "image"));
        assert_eq!(state_of(&u, "results/locked"), (Kind::Prot, "unreadable"));
        assert!(u.unreadable > 0);
        let mut items = Vec::new();
        plan(&u.tree, &mut items);
        assert_eq!(items.iter().map(|e| e.path.as_str()).collect::<Vec<_>>(), ["cache/bbbb.png"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_live_panes_folder_holds_everything_in_it_but_a_skippers_folder_does_not() {
        let dir = scratch("cwd");
        let r = dir.join("projects/web/results");
        let a = r.join("a1-task");
        let l = ledger(&dir, &[("a1", "task", None, "done", Some("pa"), &a)]);
        write(&a.join("worktree/src/main.rs"), "fn", 7200);
        write(&dir.join("scratch/tool/x"), "x", 3 * 86_400);
        let skipper = pane("sk", "working", &dir.join("projects/web"));
        let admiral = pane("ad", "working", &dir);
        let u = at(&dir, &l, &[skipper.clone(), admiral.clone()]);
        assert_eq!(state_of(&u, "projects/web/results/a1-task"), (Kind::Stale, "done"));
        let inside = pane("in", "idle", &a.join("worktree"));
        let u = at(&dir, &l, &[skipper.clone(), inside]);
        assert_eq!(state_of(&u, "projects/web/results/a1-task"), (Kind::Use, "pane"));
        let tool = pane("tl", "idle", &dir.join("scratch/tool"));
        assert_eq!(state_of(&at(&dir, &l, &[tool]), "scratch/tool"), (Kind::Use, "pane"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn without_the_daemon_or_the_ledger_nothing_is_stale() {
        let dir = scratch("unsure");
        let l = ledger(&dir, &[]);
        write(&dir.join("results/old.md"), "x", 9 * 86_400);
        let u = raw(&dir, &dir.join("cache"), &l, None, Path::new(SOCK), NOW);
        assert_eq!(state_of(&u, "results/old.md"), (Kind::Prot, "no-daemon"));
        std::fs::write(&l, "not a database").unwrap();
        let u = at(&dir, &l, &[]);
        assert!(u.ledger_error.is_some());
        assert_eq!(state_of(&u, "results/old.md"), (Kind::Prot, "no-ledger"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_file_that_changes_after_the_count_keeps_its_item() {
        let dir = scratch("race");
        let a = dir.join("results/a1-task");
        let l = ledger(&dir, &[("a1", "task", None, "done", None, &a)]);
        write(&a.join("result.md"), "a", 3600);
        write(&a.join("log.txt"), "log", 3600);
        let u = at(&dir, &l, &[]);
        let mut items = Vec::new();
        plan(&u.tree, &mut items);
        assert_eq!(items.len(), 1);
        // The agent writes again after the dialog: the item stays whole.
        write(&a.join("log.txt"), "more log", 1200);
        let out = flush_at(&dir, &dir.join("cache"), &l, Some(&[]), Path::new(SOCK), &items, NOW);
        assert_eq!((out.files, out.kept), (0, 1));
        assert!(a.join("result.md").exists());
        // A file newer than the count is kept even when the item matches.
        let u = at(&dir, &l, &[]);
        let mut items = Vec::new();
        plan(&u.tree, &mut items);
        let mut out = Flushed::default();
        let n = find(&u.tree, "results/a1-task").unwrap();
        write(&a.join("late.md"), "late", 0);
        let late = Node { children: vec![], ..find(&at(&dir, &l, &[]).tree, "results/a1-task/late.md").unwrap().clone() };
        remove_files(&late, items[0].mtime, &mut out);
        assert_eq!(out.kept, 1);
        assert!(a.join("late.md").exists() && n.abs.exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_link_is_removed_never_followed() {
        let dir = scratch("link");
        let outside = scratch("link-target");
        write(&outside.join("keep.md"), "keep", 0);
        let l = ledger(&dir, &[]);
        std::fs::create_dir_all(dir.join("results/old")).unwrap();
        std::os::unix::fs::symlink(&outside, dir.join("results/old/to-outside")).unwrap();
        std::fs::write(dir.join("results/old/x.md"), "x").unwrap();
        // The link's own change time and the file's are now: count two days later.
        let u = raw(&dir, &dir.join("cache"), &l, Some(&[]), Path::new(SOCK), now_secs() + 2 * 86_400);
        assert_eq!(find(&u.tree, "results/old/to-outside").unwrap().bytes, 0);
        let mut items = Vec::new();
        plan(&u.tree, &mut items);
        let out = flush_at(&dir, &dir.join("cache"), &l, Some(&[]), Path::new(SOCK), &items, now_secs() + 2 * 86_400);
        assert_eq!(out.files, 2);
        assert!(!dir.join("results/old").exists() && outside.join("keep.md").exists());
        std::fs::remove_dir_all(&dir).unwrap();
        std::fs::remove_dir_all(&outside).unwrap();
    }

    #[test]
    fn a_checkout_whose_config_can_run_a_program_is_never_asked_and_stays() {
        let dir = scratch("filter");
        let a = dir.join("results/a1-task");
        let l = ledger(&dir, &[("a1", "task", None, "done", None, &a)]);
        let repo = a.join("copy");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q"]);
        let marker = dir.join("ran");
        let cfg = repo.join(".git/config");
        let mut text = std::fs::read_to_string(&cfg).unwrap();
        text.push_str(&format!("[filter \"x\"]\n\tclean = touch {}\n", marker.display()));
        std::fs::write(&cfg, text).unwrap();
        assert!(!plain_config(&cfg));
        // git parses: a key on the header's line, an include or a promisor is not plain.
        let file = |name: &str, text: &str| {
            let f = dir.join(name);
            std::fs::write(&f, text).unwrap();
            f
        };
        assert!(plain_config(&file("ok", "[core]\n\trepositoryformatversion = 0\n[remote \"origin\"]\n\turl = x\n\tfetch = +refs/heads/*:refs/remotes/origin/*\n")));
        assert!(!plain_config(&file("same-line", "[core] fsmonitor = run\n")));
        assert!(!plain_config(&file("include", "[include]\n\tpath = other\n")));
        assert!(!plain_config(&file("promisor", "[remote \"o\"]\n\tpromisor = true\n")));
        assert!(!plain_config(&file("broken", "[core\n")));
        assert_eq!(checkout(&repo), Checkout::Unknown);
        let u = raw(&dir, &dir.join("cache"), &l, Some(&[]), Path::new(SOCK), now_secs() + 3600);
        assert_eq!(state_of(&u, "results/a1-task"), (Kind::Prot, "checkout"));
        assert!(!marker.exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_folder_swapped_for_a_link_after_the_count_is_not_followed() {
        let dir = scratch("swap");
        let outside = scratch("swap-target");
        write(&outside.join("x.md"), "keep", 0);
        let l = ledger(&dir, &[]);
        write(&dir.join("results/old/x.md"), "x", 0);
        let u = raw(&dir, &dir.join("cache"), &l, Some(&[]), Path::new(SOCK), now_secs() + 2 * 86_400);
        let n = find(&u.tree, "results/old").unwrap().clone();
        // Another process swaps the folder for a link to elsewhere.
        std::fs::remove_dir_all(dir.join("results/old")).unwrap();
        std::os::unix::fs::symlink(&outside, dir.join("results/old")).unwrap();
        let mut out = Flushed::default();
        remove_files(&n, i64::MAX, &mut out);
        assert_eq!((out.files, out.kept), (0, 1));
        assert!(outside.join("x.md").exists());
        std::fs::remove_dir_all(&dir).unwrap();
        std::fs::remove_dir_all(&outside).unwrap();
    }

    fn git(dir: &Path, args: &[&str]) {
        let ok = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "init.defaultBranch=main"])
            .args(args)
            .output()
            .unwrap();
        assert!(ok.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&ok.stderr));
    }

    #[test]
    fn a_worktree_is_protected_unless_clean_and_pushed_and_goes_with_git() {
        let dir = scratch("wt");
        let repo = scratch("wt-repo");
        let remote = scratch("wt-remote");
        git(&remote, &["init", "--bare", "-q"]);
        git(&repo, &["init", "-q"]);
        std::fs::write(repo.join("a.txt"), "a").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-qm", "a"]);
        git(&repo, &["remote", "add", "origin", remote.to_str().unwrap()]);
        git(&repo, &["push", "-q", "-u", "origin", "main"]);
        let a = dir.join("results/a1-task");
        let l = ledger(&dir, &[("a1", "task", None, "done", None, &a)]);
        let wt = a.join("worktree");
        git(&repo, &["worktree", "add", "-q", "-b", "fix", wt.to_str().unwrap()]);
        let judge_now = || state_of(&raw(&dir, &dir.join("cache"), &l, Some(&[]), Path::new(SOCK), now_secs() + 3600), "results/a1-task");
        // A new branch with no upstream.
        assert_eq!(judge_now(), (Kind::Prot, "no-upstream"));
        git(&wt, &["push", "-q", "-u", "origin", "fix"]);
        std::fs::write(wt.join("b.txt"), "b").unwrap();
        assert_eq!(judge_now(), (Kind::Prot, "dirty"));
        git(&wt, &["add", "."]);
        git(&wt, &["commit", "-qm", "b"]);
        assert_eq!(judge_now(), (Kind::Prot, "ahead"));
        git(&wt, &["push", "-q"]);
        assert_eq!(judge_now(), (Kind::Stale, "done"));
        // The flush removes it with git: the main repository forgets it.
        let u = raw(&dir, &dir.join("cache"), &l, Some(&[]), Path::new(SOCK), now_secs() + 3600);
        let mut items = Vec::new();
        plan(&u.tree, &mut items);
        let out = flush_at(&dir, &dir.join("cache"), &l, Some(&[]), Path::new(SOCK), &items, now_secs() + 3600);
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert!(!a.exists());
        let list = Command::new("git").arg("-C").arg(&repo).args(["worktree", "list"]).output().unwrap();
        assert!(!String::from_utf8_lossy(&list.stdout).contains("a1-task"));
        for d in [&dir, &repo, &remote] {
            std::fs::remove_dir_all(d).unwrap();
        }
    }

    #[test]
    fn auto_flush_takes_only_stale_parts_older_than_its_days() {
        let dir = scratch("auto");
        let (a, b) = (dir.join("results/a1-task"), dir.join("results/b1-task"));
        let l = ledger(&dir, &[("a1", "task", None, "done", None, &a), ("b1", "task", None, "done", None, &b)]);
        write(&a.join("result.md"), "a", 40 * 86_400);
        write(&b.join("result.md"), "b", 2 * 86_400);
        write(&dir.join("results/notes.md"), "n", 40 * 86_400);
        let out = flush_old_at(&dir, &dir.join("cache"), &l, Some(&[]), Path::new(SOCK), 30, NOW);
        assert_eq!(out.files, 2);
        assert!(!a.exists() && b.join("result.md").exists() && !dir.join("results/notes.md").exists());
        assert_eq!(flush_old_at(&dir, &dir.join("cache"), &l, Some(&[]), Path::new(SOCK), 0, NOW), Flushed::default());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_big_folder_with_one_verdict_folds_into_one_row() {
        let dir = scratch("fold");
        let a = dir.join("results/a1-task");
        let l = ledger(&dir, &[("a1", "task", None, "done", None, &a)]);
        for i in 0..60 {
            write(&a.join(format!("target/debug/f{i}")), "x", 3600);
        }
        let mut u = at(&dir, &l, &[]);
        fold(&mut u.tree);
        let t = find(&u.tree, "results/a1-task/target").unwrap();
        assert!(t.children.is_empty() && t.files == 60 && t.state.as_ref().unwrap().stale());
        // The work folder itself stays open to look into.
        assert!(!find(&u.tree, "results/a1-task").unwrap().children.is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
