//! Local copies of a Host's files, for Open with (M5). A remote Host's path
//! is never opened here: the app fetches the file's bytes (bounded by the
//! Host, `files@1` with the copy purpose) and writes them to a copy it
//! manages, and only the copy goes to an app on this Mac.
//!
//! - One private folder per account (`$TMPDIR/skiff-copies-<uid>`, `0700`,
//!   owned by this account and not a link, else refused), one folder per app
//!   process in it, one folder per copy, so two files with one name never meet.
//! - A copy is read-only (`0400`) and never executable. On macOS it carries
//!   the quarantine flag, as a download does. Types that launch or install
//!   something when opened are refused.
//! - At most [`KEEP`] copies per process: the oldest go. The process's folder
//!   goes when the app quits, and the folders of processes that no longer
//!   run go when the app starts. Every removal starts from the account's
//!   folder held open after the same checks as a copy's, and goes down one
//!   entry at a time relative to it, never through a link: a folder Skiff
//!   refuses to copy into is never cleaned either.
use std::{
    ffi::{CStr, CString},
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use anyhow::{bail, Context, Result};

/// Copies kept per app process.
const KEEP: usize = 32;

/// File types that run, install or redirect something when opened: never
/// handed to an app from a Host.
const REFUSED: &[&str] = &[
    "app", "command", "tool", "terminal", "workflow", "action", "scpt", "scptd", "applescript", "osascript",
    "pkg", "mpkg", "dmg", "jar", "webloc", "inetloc", "fileloc", "url", "desktop", "prefpane", "kext",
    "plugin", "bundle", "shortcut", "service", "mobileconfig", "appimage", "deb", "rpm", "run", "sh", "bash",
    "zsh", "csh", "ksh", "fish",
];

static NEXT: AtomicU64 = AtomicU64::new(0);

/// The account's folder for copies.
pub(crate) fn base() -> PathBuf {
    // SAFETY: geteuid takes no arguments and cannot fail.
    std::env::temp_dir().join(format!("skiff-copies-{}", unsafe { libc::geteuid() }))
}

/// `base`, made if missing, and checked: a folder of this account, not a
/// link, that no one else can open.
fn private(base: &Path) -> Result<()> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
    match std::fs::DirBuilder::new().mode(0o700).create(base) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e).with_context(|| format!("create {}", base.display())),
    }
    let meta = std::fs::symlink_metadata(base)?;
    // SAFETY: as in `base`.
    let me = unsafe { libc::geteuid() };
    if !meta.is_dir() || meta.uid() != me || meta.permissions().mode() & 0o077 != 0 {
        bail!("{} is not a private folder of this account, so Skiff makes no copy there", base.display());
    }
    Ok(())
}

/// A name for the copy from the Host file's own: its last component, with
/// no path separator or control character, at most 120 bytes, its
/// extension kept. Refused for a type that runs something when opened.
pub(crate) fn copy_name(host_file: &str) -> Result<String> {
    let last = host_file.rsplit('/').next().unwrap_or_default();
    let mut name: String = last.chars().map(|c| if c.is_control() || c == '\\' { '_' } else { c }).collect();
    if name.is_empty() || name == "." || name == ".." {
        name = "file".into();
    }
    let ext = Path::new(&name).extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    if REFUSED.contains(&ext.as_str()) {
        bail!("Skiff does not open a copy of a .{ext} file: opening it could run or install something on this computer.");
    }
    if name.len() > 120 {
        let tail = if ext.is_empty() || ext.len() > 16 { String::new() } else { format!(".{ext}") };
        let mut cut = 120 - tail.len();
        while !name.is_char_boundary(cut) {
            cut -= 1;
        }
        name = format!("{}{tail}", &name[..cut]);
    }
    Ok(name)
}

/// Writes `bytes` as a new read-only copy named for `host_file`, under
/// `base`'s folder for this process. Its path, to hand to an app.
pub(crate) fn save_in(base: &Path, host_file: &str, bytes: &[u8]) -> Result<PathBuf> {
    use std::{io::Write, os::unix::fs::{DirBuilderExt, OpenOptionsExt}};
    let name = copy_name(host_file)?;
    private(base)?;
    let mine = base.join(std::process::id().to_string());
    match std::fs::DirBuilder::new().mode(0o700).create(&mine) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e).context("make the copy folder"),
    }
    if !std::fs::symlink_metadata(&mine)?.is_dir() {
        bail!("{} is not a folder", mine.display());
    }
    if let Some(held) = held(base) {
        prune(&held, KEEP - 1);
    }
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let folder = mine.join(format!("{stamp:032}-{n}"));
    std::fs::DirBuilder::new().mode(0o700).create(&folder).context("make the copy folder")?;
    let path = folder.join(&name);
    // A new file only: never through a link, never over a file.
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&path)
        .with_context(|| format!("write {}", path.display()))?;
    file.write_all(bytes)?;
    // Flagged while it is still writable, then made read-only.
    quarantine(&file);
    file.set_permissions(std::os::unix::fs::PermissionsExt::from_mode(0o400))?;
    file.sync_all()?;
    Ok(path)
}

/// [`save_in`] the account's folder.
pub(crate) fn save(host_file: &str, bytes: &[u8]) -> Result<PathBuf> {
    save_in(&base(), host_file, bytes)
}

/// `base` held open, if it is a private folder of this account and not a
/// link: what [`private`] checks, without making it. The fd checked is the
/// one every removal starts from.
fn held(base: &Path) -> Option<OwnedFd> {
    let path = CString::new(base.as_os_str().as_bytes()).ok()?;
    // SAFETY: open(2) on a NUL-terminated path; the fd is owned at once.
    let fd = unsafe { libc::open(path.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC) };
    if fd < 0 {
        return None;
    }
    // SAFETY: a fresh fd this function owns.
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    owned(&fd, true).then_some(fd)
}

/// The open fd is a folder of this account; `private`: that no one else can open.
fn owned(fd: &OwnedFd, private: bool) -> bool {
    // SAFETY: fstat(2) on an open fd into a zeroed stat; geteuid as in `base`.
    unsafe {
        let mut st: libc::stat = std::mem::zeroed();
        libc::fstat(fd.as_raw_fd(), &mut st) == 0
            && st.st_mode & libc::S_IFMT == libc::S_IFDIR
            && st.st_uid == libc::geteuid()
            && (!private || st.st_mode & 0o077 == 0)
    }
}

/// The entry `name` of the open folder `dir`, opened as a folder without
/// following it: `None` for a link or anything but a folder.
fn open_dir_at(dir: &OwnedFd, name: &CStr) -> Option<OwnedFd> {
    // SAFETY: openat(2) relative to an open fd; the fd is owned at once.
    let fd = unsafe { libc::openat(dir.as_raw_fd(), name.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC) };
    // SAFETY: a fresh fd this function owns.
    (fd >= 0).then(|| unsafe { OwnedFd::from_raw_fd(fd) })
}

/// The names in the open folder `dir`, without `.` and `..`.
fn names(dir: &OwnedFd) -> Vec<CString> {
    let mut found = Vec::new();
    // SAFETY: fdopendir(3) takes a dup of `dir`, closed with the stream.
    unsafe {
        let fd = libc::fcntl(dir.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0);
        if fd < 0 {
            return found;
        }
        let stream = libc::fdopendir(fd);
        if stream.is_null() {
            libc::close(fd);
            return found;
        }
        // A dup shares the offset: read from the start.
        libc::rewinddir(stream);
        loop {
            let e = libc::readdir(stream);
            if e.is_null() {
                break;
            }
            let name = CStr::from_ptr((*e).d_name.as_ptr());
            if !matches!(name.to_bytes(), b"." | b"..") {
                found.push(name.to_owned());
            }
        }
        libc::closedir(stream);
    }
    found
}

/// Removes the entry `name` of the open folder `dir`, and everything in it
/// if it is a folder, each relative to the folder held before it. A link is
/// removed itself, never followed.
fn remove_at(dir: &OwnedFd, name: &CStr) -> std::io::Result<()> {
    // SAFETY: fstatat(2) and unlinkat(2) relative to an open fd, on a NUL-terminated name.
    unsafe {
        let mut st: libc::stat = std::mem::zeroed();
        if libc::fstatat(dir.as_raw_fd(), name.as_ptr(), &mut st, libc::AT_SYMLINK_NOFOLLOW) != 0 {
            return Err(std::io::Error::last_os_error());
        }
        if st.st_mode & libc::S_IFMT == libc::S_IFDIR {
            let Some(inner) = open_dir_at(dir, name) else { return Err(std::io::Error::last_os_error()) };
            for n in names(&inner) {
                remove_at(&inner, &n)?;
            }
            if libc::unlinkat(dir.as_raw_fd(), name.as_ptr(), libc::AT_REMOVEDIR) != 0 {
                return Err(std::io::Error::last_os_error());
            }
        } else if libc::unlinkat(dir.as_raw_fd(), name.as_ptr(), 0) != 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

/// This process's folder in the held `base`: a name it owns there.
fn mine_name() -> CString {
    CString::new(std::process::id().to_string()).expect("a number has no NUL")
}

/// Keeps the newest `keep` copy folders of this process's folder in `base`.
fn prune(base: &OwnedFd, keep: usize) {
    let Some(mine) = open_dir_at(base, &mine_name()) else { return };
    if !owned(&mine, false) {
        return;
    }
    let mut folders = names(&mine);
    if folders.len() <= keep {
        return;
    }
    // Named by time first: the name order is the age order.
    folders.sort();
    for old in &folders[..folders.len() - keep] {
        let _ = remove_at(&mine, old);
    }
}

/// The app quits: this process's copies go. Nothing goes unless `base` is
/// a private folder of this account and not a link, and this process's
/// folder in it is a folder of this account, not a link.
pub(crate) fn cleanup_in(base: &Path) {
    let Some(base) = held(base) else { return };
    let name = mine_name();
    if open_dir_at(&base, &name).is_some_and(|mine| owned(&mine, false)) {
        let _ = remove_at(&base, &name);
    }
}

pub(crate) fn cleanup() {
    cleanup_in(&base());
}

/// The app starts: copies of app processes that no longer run go. Only
/// folders named by a process id, inside a private folder of this account.
pub(crate) fn sweep_in(base: &Path) {
    let Some(base) = held(base) else { return };
    for name in names(&base) {
        let Some(pid) = name.to_str().ok().and_then(|n| n.parse::<libc::pid_t>().ok()) else { continue };
        // SAFETY: kill(2) with signal 0 only checks that the process exists.
        let alive = pid > 0 && (unsafe { libc::kill(pid, 0) } == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM));
        if !alive && open_dir_at(&base, &name).is_some_and(|d| owned(&d, false)) {
            let _ = remove_at(&base, &name);
        }
    }
}

pub(crate) fn sweep() {
    sweep_in(&base());
}

/// Marks the copy as downloaded, so macOS checks it before it runs anything.
#[cfg(target_os = "macos")]
fn quarantine(file: &std::fs::File) {
    use std::os::fd::AsRawFd;
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let value = format!("0081;{secs:x};Skiff;");
    // SAFETY: fsetxattr(2) on an open fd, a NUL-terminated name and a buffer of the given length.
    unsafe {
        libc::fsetxattr(file.as_raw_fd(), c"com.apple.quarantine".as_ptr(), value.as_ptr().cast(), value.len(), 0, 0);
    }
}

#[cfg(not(target_os = "macos"))]
fn quarantine(_: &std::fs::File) {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// Removes a test's folder, also when an assertion fails.
    struct Gone(PathBuf);
    impl Drop for Gone {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("skiff-copies-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn names_keep_the_type_and_refuse_what_runs() {
        assert_eq!(copy_name("src/main.rs").unwrap(), "main.rs");
        assert_eq!(copy_name("a\nb\\c.md").unwrap(), "a_b_c.md");
        assert_eq!(copy_name("..").unwrap(), "file");
        assert_eq!(copy_name(".env").unwrap(), ".env");
        let long = copy_name(&format!("{}.png", "é".repeat(100))).unwrap();
        assert!(long.len() <= 120 && long.ends_with(".png"), "{long}");
        for bad in ["x.command", "Y.APP", "z.webloc", "run.sh", "a.desktop", "i.pkg"] {
            assert!(copy_name(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn copies_are_private_read_only_bounded_and_cleaned_up() {
        let base = scratch("save");
        let _gone = Gone(base.clone());
        let a = save_in(&base, "docs/readme.md", b"hello").unwrap();
        let b = save_in(&base, "other/readme.md", b"world").unwrap();
        assert_ne!(a, b, "two files with one name never meet");
        assert_eq!(a.file_name().unwrap(), "readme.md");
        assert_eq!(std::fs::read(&a).unwrap(), b"hello");
        assert_eq!(std::fs::metadata(&a).unwrap().permissions().mode() & 0o777, 0o400);
        assert_eq!(std::fs::metadata(&base).unwrap().permissions().mode() & 0o777, 0o700);
        #[cfg(target_os = "macos")]
        assert!(std::process::Command::new("xattr").arg("-p").arg("com.apple.quarantine").arg(&a).output().unwrap().status.success());
        // Bounded: the oldest go.
        for i in 0..KEEP + 3 {
            save_in(&base, &format!("f{i}.txt"), b"x").unwrap();
        }
        let mine = base.join(std::process::id().to_string());
        assert_eq!(std::fs::read_dir(&mine).unwrap().count(), KEEP);
        assert!(!a.exists());
        // A dead process's copies go at start; a live one's stay.
        let dead = base.join("999999");
        std::fs::create_dir_all(dead.join("x")).unwrap();
        std::fs::create_dir_all(base.join("not-a-pid")).unwrap();
        sweep_in(&base);
        assert!(!dead.exists() && mine.exists() && base.join("not-a-pid").exists());
        cleanup_in(&base);
        assert!(!mine.exists());
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn a_shared_or_linked_folder_is_refused() {
        let base = scratch("open");
        std::fs::create_dir_all(&base).unwrap();
        std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o777)).unwrap();
        let open = save_in(&base, "a.txt", b"x");
        std::fs::remove_dir_all(&base).unwrap();
        let target = scratch("target");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::os::unix::fs::symlink(&target, &base).unwrap();
        let linked = save_in(&base, "a.txt", b"x");
        let written = std::fs::read_dir(&target).unwrap().count();
        std::fs::remove_file(&base).unwrap();
        std::fs::remove_dir_all(&target).unwrap();
        assert!(open.is_err() && linked.is_err());
        assert_eq!(written, 0);
    }

    /// The review's case: a base that save refuses, then the exit cleanup and
    /// the start sweep through that same base. Neither removes anything.
    #[test]
    fn cleanup_and_sweep_never_remove_through_a_refused_folder() {
        let pid = std::process::id().to_string();
        let fill = |d: &Path| {
            for n in [pid.as_str(), "999999"] {
                std::fs::create_dir_all(d.join(n)).unwrap();
                std::fs::write(d.join(n).join("keep.txt"), "keep").unwrap();
            }
        };
        let kept = |d: &Path| [pid.as_str(), "999999"].map(|n| d.join(n).join("keep.txt").exists());

        // A link to another private folder of this account.
        let target = scratch("foreign");
        let _target = Gone(target.clone());
        std::fs::create_dir_all(&target).unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o700)).unwrap();
        fill(&target);
        let linked = scratch("linked");
        let _linked = Gone(linked.clone());
        std::os::unix::fs::symlink(&target, &linked).unwrap();
        assert!(save_in(&linked, "a.txt", b"x").is_err());
        cleanup_in(&linked);
        sweep_in(&linked);
        assert_eq!(kept(&target), [true, true], "nothing removed through a linked base");
        std::fs::remove_file(&linked).unwrap();

        // A folder others can open.
        let shared = scratch("shared");
        let _shared = Gone(shared.clone());
        std::fs::create_dir_all(&shared).unwrap();
        fill(&shared);
        std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(save_in(&shared, "a.txt", b"x").is_err());
        cleanup_in(&shared);
        sweep_in(&shared);
        assert_eq!(kept(&shared), [true, true], "nothing removed in a shared base");

        // A private base whose process folders are links out: the links
        // are not followed, so what they name stays.
        let base = scratch("inner-links");
        let _base = Gone(base.clone());
        std::fs::create_dir_all(&base).unwrap();
        std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o700)).unwrap();
        let outside = scratch("inner-outside");
        let _outside = Gone(outside.clone());
        fill(&outside);
        for n in [pid.as_str(), "999999"] {
            std::os::unix::fs::symlink(outside.join(n), base.join(n)).unwrap();
        }
        cleanup_in(&base);
        sweep_in(&base);
        assert_eq!(kept(&outside), [true, true], "process folders that are links are not followed");

        // Missing: nothing made, nothing removed.
        let missing = scratch("missing");
        cleanup_in(&missing);
        sweep_in(&missing);
        assert!(!missing.exists());
    }

    #[test]
    fn removal_goes_down_without_following_links() {
        let base = scratch("deep");
        let _base = Gone(base.clone());
        let outside = scratch("deep-outside");
        let _outside = Gone(outside.clone());
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("keep.txt"), "keep").unwrap();
        let copy = save_in(&base, "a.txt", b"x").unwrap();
        // A link inside a copy folder: the link goes, its target stays.
        std::os::unix::fs::symlink(&outside, copy.parent().unwrap().join("out")).unwrap();
        cleanup_in(&base);
        assert!(!base.join(std::process::id().to_string()).exists());
        assert!(outside.join("keep.txt").exists());
    }
}
