use super::{
    checkout::{now_secs, Checkout},
    model::{Kind, Node, Usage, Verdict, Work, HOLD_LOOSE, HOLD_WORK},
    tree::{fold, is_frame, is_home, kept_name, walk},
};

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
};

use skiff_core::session::{SessionInfo, SessionState};

use crate::ledger;

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

/// Counts and judges `folder`. `cache`: the image cache. `ledger`: the
/// ledger file. `sessions`: the daemon's list, `None` when it did not answer.
pub fn usage(folder: &Path, cache: &Path, ledger_file: &Path, sessions: Option<&[SessionInfo]>, socket: &Path) -> Usage {
    let mut u = raw(folder, cache, ledger_file, sessions, socket, now_secs());
    fold(&mut u.tree);
    u
}

/// [`usage`] at `now`, unfolded: the flush finds every path in it.
pub(super) fn raw(folder: &Path, cache: &Path, ledger_file: &Path, sessions: Option<&[SessionInfo]>, socket: &Path, now: i64) -> Usage {
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
