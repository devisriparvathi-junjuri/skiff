//! The flush: the expected plan, the removal of stale items, and the automatic and work-folder flushes.

use super::{
    checkout::{checkout, git, mtime, now_secs, plain_repo, Checkout, Repo},
    model::{words, Expected, Flushed, Node, Verdict},
    policy::raw,
    tree::{collect_states, find},
};

use std::path::{Path, PathBuf};

use skiff_core::session::SessionInfo;

use crate::ledger;

#[cfg(test)]
use super::{
    checkout::plain_config,
    model::Kind,
    tree::fold,
};
#[cfg(test)]
use super::model::Usage;

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
    use std::{fs::File, process::Command, time::UNIX_EPOCH};

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
