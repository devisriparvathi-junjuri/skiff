//! `folders@1`: the picker lists folders only, resolves links, and makes
//! one folder from one name. Private temp folders only.
#![cfg(unix)]

use std::path::PathBuf;

use skiff_host::folders;
use skiff_host_api::{Error, FolderKind};

mod common;
use common::{repo, Guard};

#[test]
fn folders_lists_folders_only_and_create_takes_one_name() {
    let dir = PathBuf::from("/tmp").join(format!("shfo-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let _guard = Guard(dir.clone());
    let top = dir.to_string_lossy().into_owned();
    repo(&dir.join("api"));
    std::fs::create_dir_all(dir.join("notes")).unwrap();
    std::fs::create_dir_all(dir.join(".hidden")).unwrap();
    std::fs::write(dir.join("file.txt"), "x").unwrap();
    std::os::unix::fs::symlink(dir.join("api"), dir.join("link")).unwrap();
    std::os::unix::fs::symlink(dir.join("file.txt"), dir.join("filelink")).unwrap();

    let l = folders::list(&top, false, None).unwrap();
    let names: Vec<_> = l.entries.iter().map(|e| (e.name.as_str(), e.kind, e.repo.is_some())).collect();
    assert_eq!(names, [("api", FolderKind::Dir, true), ("link", FolderKind::Link, true), ("notes", FolderKind::Dir, false)]);
    assert!(l.entries[0].repo.as_ref().unwrap().branch.is_some());
    assert_eq!(folders::list(&top, true, None).unwrap().entries.len(), 4, "hidden folders on request");
    assert_eq!(folders::list(&top, false, Some("NOT")).unwrap().entries.len(), 1, "filter ignores case");
    // A link opens at its real path.
    let via = folders::list(&format!("{top}/link"), false, None).unwrap();
    assert_eq!(via.real, folders::list(&format!("{top}/api"), false, None).unwrap().real);
    assert!(matches!(folders::list("relative", false, None), Err(Error::Invalid { .. })));
    assert!(matches!(folders::list(&format!("{top}/gone"), false, None), Err(Error::NotFound { .. })));
    assert!(matches!(folders::list(&format!("{top}/file.txt"), false, None), Err(Error::NotFound { .. })));

    for bad in ["..", ".", "a/b", "", "a\0b", &"x".repeat(256)] {
        assert!(matches!(folders::create(&top, bad), Err(Error::Invalid { .. })), "{bad:?}");
    }
    assert!(!dir.join("a").exists());
    let made = folders::create(&top, "new one").unwrap();
    assert!(PathBuf::from(&made).is_dir());
    assert!(matches!(folders::create(&top, "new one"), Err(Error::Conflict { .. })));
    assert!(folders::home().unwrap().starts_with('/'));
}

#[test]
fn the_repo_scan_stops_at_its_bounds() {
    use folders::ScanBounds;
    use std::time::{Duration, Instant};
    let dir = PathBuf::from("/tmp").join(format!("shsc-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let _guard = Guard(dir.clone());
    // Level 4 is found, level 5 is past the depth.
    std::fs::create_dir_all(dir.join("a/b/c/deep4/.git")).unwrap();
    std::fs::write(dir.join("a/b/c/deep4/.git/HEAD"), "ref: refs/heads/main\n").unwrap();
    std::fs::create_dir_all(dir.join("a/b/c/d/deep5/.git")).unwrap();
    std::fs::write(dir.join("a/b/c/d/deep5/.git/HEAD"), "ref: refs/heads/main\n").unwrap();
    // Never entered: hidden folders, built files, and a repo's own folders.
    for skip in [".config/nvim", "node_modules/x", "target/x", "vendor/x", "w/.cache/x", "a/b/c/deep4/inner"] {
        std::fs::create_dir_all(dir.join(skip).join(".git")).unwrap();
        std::fs::write(dir.join(skip).join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
    }
    // 250 repositories at level 2.
    for i in 0..250 {
        let r = dir.join(format!("many/r{i:03}/.git"));
        std::fs::create_dir_all(&r).unwrap();
        std::fs::write(r.join("HEAD"), "ref: refs/heads/main\n").unwrap();
    }
    let (all, complete) = folders::scan(&dir, ScanBounds { depth: 4, time: Duration::from_secs(5), repos: 1000 });
    assert!(complete);
    let names: Vec<_> = all.iter().map(|r| r.path.strip_prefix(dir.to_str().unwrap()).unwrap()).filter(|p| !p.starts_with("/many")).collect();
    assert_eq!(names, ["/a/b/c/deep4"], "depth 4 only, nothing skipped is entered");
    assert_eq!(all.len(), 251);
    assert_eq!(all[0].branch.as_deref(), Some("main"));

    let (capped, complete) = folders::scan(&dir, ScanBounds::PLACES);
    assert_eq!((capped.len(), complete), (200, false), "at most 200 repositories");
    let started = Instant::now();
    let (_, complete) = folders::scan(&dir, ScanBounds { depth: 4, time: Duration::ZERO, repos: 200 });
    assert!(!complete && started.elapsed() < Duration::from_millis(500), "stops at its time bound");
}
