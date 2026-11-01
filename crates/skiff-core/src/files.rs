//! A worktree's files, for the sidebar's file tree and its preview (M5).
//! The tree only lists and reads: no edits, no watching. Blocking.
//!
//! Every read happens under the worktree folder held open ([`git::Root`]):
//! the path must name that folder before and after, each component below it
//! opens relative to the one before with `O_NOFOLLOW`, and a file's type is
//! checked before it opens. A symlink is listed and shown as a link, never
//! followed; a FIFO, device or socket is listed and never opened. Every read
//! is bounded.

use std::{io::Read, path::Path, time::Duration};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::git::{self, Beneath, Root};

/// At most this many entries of one folder are listed, counted after what
/// git ignores is left out.
pub const LIST_LIMIT: usize = 2000;
/// At most this many entries of one folder are read from disk to find them:
/// a folder of ignored files cannot hide the ones listed after them.
pub const SCAN_LIMIT: usize = 20 * LIST_LIMIT;
/// Entries asked of git at a time, so a listing that is full stops asking.
const IGNORE_BATCH: usize = 512;
/// At most this much text of a file is shown.
pub const TEXT_LIMIT: usize = 512 * 1024;
/// The largest image shown, file copied or background served: its bytes fit
/// one Host frame.
pub const BYTES_LIMIT: u64 = 5 * 1024 * 1024;
/// How long git may take to say which entries of one folder it ignores.
const IGNORE_TIMEOUT: Duration = Duration::from_secs(10);

/// A read the Host refuses, by kind, so the service can type it.
#[derive(Debug)]
pub enum Refused {
    /// No such folder or file in the worktree, or one that is never opened.
    Missing(String),
    /// A file the purpose does not take: too large, or a link to copy.
    Invalid(String),
}
impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing(m) | Self::Invalid(m) => f.write_str(m),
        }
    }
}
impl std::error::Error for Refused {}

macro_rules! missing { ($($t:tt)*) => { anyhow::Error::from(Refused::Missing(format!($($t)*))) } }
macro_rules! invalid { ($($t:tt)*) => { anyhow::Error::from(Refused::Invalid(format!($($t)*))) } }

/// What an entry is, as it is on disk: a link is not followed.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Dir,
    File,
    Link,
    /// A FIFO, device or socket: listed, never opened.
    Other,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub kind: Kind,
}

/// One folder's entries, folders first, without `.git` and what git ignores.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default)]
pub struct Listing {
    pub entries: Vec<Entry>,
    /// The folder has more than [`LIST_LIMIT`] entries git does not ignore,
    /// or more than [`SCAN_LIMIT`] in all: the rest are left out.
    pub truncated: bool,
}

/// `rel` names the worktree itself (empty) or a path below it.
fn below(rel: &str) -> Result<()> {
    if rel.is_empty() || git::is_relative_file(rel) {
        Ok(())
    } else {
        Err(missing!("{rel} is not a path in this worktree"))
    }
}

/// The folder `rel` under `root` (empty: `root` itself), listed. `dir` must
/// name `root` before and after. Outside a repository nothing is ignored.
pub fn list_in(root: &Root, dir: &Path, rel: &str) -> Result<Listing> {
    list_with(root, dir, rel, false)
}

/// [`list_in`], and with `follow` a link is followed: a link to a folder is
/// listed as a folder and can be listed. For the Host that is this Mac only;
/// the client never asks for it on a remote Host.
pub fn list_with(root: &Root, dir: &Path, rel: &str, follow: bool) -> Result<Listing> {
    below(rel)?;
    root.check(dir)?;
    let (read, mut truncated, linked) = read_folder(root, rel, follow)?;
    let deadline = git::Deadline::after(IGNORE_TIMEOUT);
    let mut entries = Vec::new();
    // In batches, after git's answer: an ignored entry takes no place in the listing.
    for batch in read.chunks(IGNORE_BATCH) {
        let asked: Vec<String> = batch
            .iter()
            .map(|e| {
                let path = if rel.is_empty() { e.name.clone() } else { format!("{rel}/{}", e.name) };
                // A followed link is no folder to git: `path/` would fail the batch.
                if e.kind == Kind::Dir && !linked.contains(&e.name) { format!("{path}/") } else { path }
            })
            .collect();
        let ignored = git::ignored_in(root, dir, &asked, deadline);
        for (e, path) in batch.iter().zip(&asked) {
            if ignored.contains(path) || ignored.contains(path.trim_end_matches('/')) {
                continue;
            }
            if entries.len() == LIST_LIMIT {
                truncated = true;
                break;
            }
            entries.push(e.clone());
        }
        if entries.len() == LIST_LIMIT && truncated {
            break;
        }
    }
    entries.sort_by(|a, b| {
        (b.kind == Kind::Dir)
            .cmp(&(a.kind == Kind::Dir))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    root.check(dir)?;
    Ok(Listing { entries, truncated })
}

/// The entries of `rel`, opened under `root` one component at a time, each
/// typed without following it. Names that are not UTF-8 are left out.
#[cfg(unix)]
fn read_folder(root: &Root, rel: &str, follow: bool) -> Result<(Vec<Entry>, bool, std::collections::HashSet<String>)> {
    use std::{ffi::{CStr, CString}, os::fd::AsRawFd, os::unix::ffi::OsStrExt};
    let gone = || missing!("{} is not a folder in this worktree", if rel.is_empty() { "." } else { rel });
    // SAFETY: open(2), readdir(3) and fstatat(2) on fds made and closed here;
    // `root` stays open while it is borrowed.
    unsafe {
        let mut fd = libc::fcntl(root.dir.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0);
        if fd < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        for part in Path::new(rel).components() {
            let name = CString::new(part.as_os_str().as_bytes()).map_err(|_| gone())?;
            // A link, or a FIFO swapped in, fails here: never followed (unless
            // `follow`), never waited on.
            let nofollow = if follow { 0 } else { libc::O_NOFOLLOW };
            let next = libc::openat(fd, name.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY | nofollow | libc::O_NONBLOCK | libc::O_CLOEXEC);
            libc::close(fd);
            if next < 0 {
                return Err(gone());
            }
            fd = next;
        }
        let stream = libc::fdopendir(fd);
        if stream.is_null() {
            libc::close(fd);
            return Err(gone());
        }
        let mut entries = Vec::new();
        let mut linked = std::collections::HashSet::new();
        let mut truncated = false;
        loop {
            let e = libc::readdir(stream);
            if e.is_null() {
                break;
            }
            let raw = CStr::from_ptr((*e).d_name.as_ptr());
            let Ok(name) = raw.to_str() else { continue };
            if matches!(name, "." | ".." | ".git") {
                continue;
            }
            if entries.len() == SCAN_LIMIT {
                truncated = true;
                break;
            }
            let mut st: libc::stat = std::mem::zeroed();
            if libc::fstatat(libc::dirfd(stream), raw.as_ptr(), &mut st, libc::AT_SYMLINK_NOFOLLOW) != 0 {
                continue;
            }
            let mut mode = st.st_mode & libc::S_IFMT;
            // A link to a folder is a folder when links are followed.
            if follow && mode == libc::S_IFLNK {
                let mut target: libc::stat = std::mem::zeroed();
                if libc::fstatat(libc::dirfd(stream), raw.as_ptr(), &mut target, 0) == 0 && target.st_mode & libc::S_IFMT == libc::S_IFDIR {
                    mode = libc::S_IFDIR;
                    linked.insert(name.to_string());
                }
            }
            let kind = match mode {
                libc::S_IFDIR => Kind::Dir,
                libc::S_IFREG => Kind::File,
                libc::S_IFLNK => Kind::Link,
                _ => Kind::Other,
            };
            entries.push(Entry { name: name.to_string(), kind });
        }
        libc::closedir(stream);
        Ok((entries, truncated, linked))
    }
}

#[cfg(not(unix))]
fn read_folder(root: &Root, rel: &str, follow: bool) -> Result<(Vec<Entry>, bool, std::collections::HashSet<String>)> {
    let mut entries = Vec::new();
    let mut truncated = false;
    for e in std::fs::read_dir(root.path.join(rel))? {
        let e = e?;
        let Ok(name) = e.file_name().into_string() else { continue };
        if name == ".git" {
            continue;
        }
        if entries.len() == SCAN_LIMIT {
            truncated = true;
            break;
        }
        let t = e.file_type()?;
        let kind = if t.is_symlink() { Kind::Link } else if t.is_dir() { Kind::Dir } else if t.is_file() { Kind::File } else { Kind::Other };
        entries.push(Entry { name, kind });
    }
    Ok((entries, truncated, Default::default()))
}

/// Why a file is read: to show it, or to hand a copy to an app on the client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Purpose {
    /// Text up to [`TEXT_LIMIT`], an image up to [`BYTES_LIMIT`], else no bytes.
    Preview,
    /// Every byte of a regular file up to [`BYTES_LIMIT`], else refused.
    Copy,
}

/// What a file holds, as far as it was read.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Content {
    Text,
    Image,
    Binary,
    /// A symlink: `target` holds where it points. It was not followed.
    Link,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRead {
    pub content: Content,
    /// The file's size on disk; for a link, its target's length.
    pub size: u64,
    /// An image's type, from its first bytes: never from its name.
    pub mime: Option<String>,
    /// The text shown (Preview of a text file), lossy UTF-8.
    pub text: Option<String>,
    /// The bytes: an image to show, or the whole file for a copy.
    pub bytes: Option<Vec<u8>>,
    /// Text cut at [`TEXT_LIMIT`], or an image over [`BYTES_LIMIT`] not sent.
    pub truncated: bool,
    pub target: Option<String>,
}

/// The file `rel` under `root`, read for `purpose`. `dir` must name `root`
/// before and after. A link is reported, not followed; anything but a
/// regular file or a link is refused without being opened.
pub fn read_in(root: &Root, dir: &Path, rel: &str, purpose: Purpose) -> Result<FileRead> {
    if !git::is_relative_file(rel) {
        return Err(missing!("{rel} is not a file path in this worktree"));
    }
    root.check(dir)?;
    let not_shown = || missing!("{rel} is not a regular file or a link, so it is not shown");
    let read = match git::open_beneath(root, rel).ok_or_else(not_shown)? {
        Beneath::Symlink(target) => {
            if purpose == Purpose::Copy {
                return Err(invalid!("{rel} is a link. Skiff does not follow it, so there is nothing to copy."));
            }
            FileRead {
                content: Content::Link,
                size: target.len() as u64,
                mime: None,
                text: None,
                bytes: None,
                truncated: false,
                target: Some(String::from_utf8_lossy(&target).into_owned()),
            }
        }
        Beneath::File(file) => {
            let meta = file.metadata()?;
            if !meta.is_file() {
                return Err(not_shown());
            }
            read_file(file, meta.len(), rel, purpose)?
        }
    };
    root.check(dir)?;
    Ok(read)
}

fn read_file(file: std::fs::File, size: u64, rel: &str, purpose: Purpose) -> Result<FileRead> {
    let mut file = file;
    if purpose == Purpose::Copy && size > BYTES_LIMIT {
        return Err(invalid!("{rel} is {}. Skiff copies files up to {}.", human(size), human(BYTES_LIMIT)));
    }
    // The start decides what the file is, as git decides: a NUL in it is binary.
    let mut head = Vec::new();
    (&mut file).take(8000).read_to_end(&mut head)?;
    let mime = image_type(&head);
    let content = match mime {
        Some(_) => Content::Image,
        None if head.contains(&0) => Content::Binary,
        None => Content::Text,
    };
    let mut read = FileRead { content, size, mime: mime.map(str::to_string), text: None, bytes: None, truncated: false, target: None };
    let rest = |file: &mut std::fs::File, head: Vec<u8>, limit: u64| -> Result<(Vec<u8>, bool)> {
        let mut bytes = head;
        // One byte past the limit tells a file that grew meanwhile.
        file.take((limit + 1).saturating_sub(bytes.len() as u64)).read_to_end(&mut bytes)?;
        let over = bytes.len() as u64 > limit;
        bytes.truncate(limit as usize);
        Ok((bytes, over))
    };
    match (purpose, content) {
        (Purpose::Copy, _) => {
            let (bytes, over) = rest(&mut file, head, BYTES_LIMIT)?;
            if over {
                return Err(invalid!("{rel} grew past {} while it was read.", human(BYTES_LIMIT)));
            }
            read.bytes = Some(bytes);
        }
        (Purpose::Preview, Content::Image) if size <= BYTES_LIMIT => {
            let (bytes, over) = rest(&mut file, head, BYTES_LIMIT)?;
            read.truncated = over;
            read.bytes = (!over).then_some(bytes);
        }
        (Purpose::Preview, Content::Image) => read.truncated = true,
        (Purpose::Preview, Content::Text) => {
            let (bytes, over) = rest(&mut file, head, TEXT_LIMIT as u64)?;
            let (text, cut) = crate::git::cap(bytes, over, TEXT_LIMIT);
            read.text = Some(text);
            read.truncated = cut;
        }
        (Purpose::Preview, _) => {}
    }
    Ok(read)
}

/// The image types a client may draw, by their first bytes. SVG is text:
/// it can hold scripts, so it shows as text.
pub fn image_type(head: &[u8]) -> Option<&'static str> {
    if head.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if head.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("image/jpeg")
    } else if head.starts_with(b"GIF87a") || head.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if head.len() >= 12 && &head[..4] == b"RIFF" && &head[8..12] == b"WEBP" {
        Some("image/webp")
    } else if head.starts_with(b"BM") && head.len() >= 14 {
        Some("image/bmp")
    } else if head.starts_with(&[0, 0, 1, 0]) && head.len() >= 6 && head[4..6] != [0, 0] {
        Some("image/x-icon")
    } else {
        None
    }
}

/// A project's configured background, opened as [`read_in`] opens a file:
/// a relative name below the project folder, an absolute one from `/`, one
/// component at a time and none followed if it is a link, and only a regular
/// file, typed before it opens. A link anywhere in the name is refused.
pub fn open_background(project: &Path, configured: &str) -> Result<std::fs::File> {
    let configured = configured.trim();
    let named = Path::new(configured);
    let (from, rel) = match named.strip_prefix("/") {
        Ok(rel) => (Path::new("/"), rel),
        Err(_) => (project, named),
    };
    let not_regular = || missing!("{configured} is not a regular file, so it is not a background");
    let rel = rel.to_str().filter(|r| !r.is_empty()).ok_or_else(not_regular)?;
    let root = Root::open(from).map_err(|_| not_regular())?;
    match git::open_beneath(&root, rel) {
        Some(Beneath::File(file)) if file.metadata().is_ok_and(|m| m.is_file()) => Ok(file),
        Some(Beneath::Symlink(_)) => Err(invalid!("{configured} is a link, or goes through one. Skiff does not follow it.")),
        _ => Err(not_regular()),
    }
}

/// The project's configured background, read: see [`open_background`] and [`read_image`].
pub fn read_background(project: &Path, configured: &str) -> Result<(String, Vec<u8>)> {
    read_image(open_background(project, configured)?)
}

/// `named`, a full path below `folder`, as a path relative to the held
/// folder: a regular file reached component by component without following
/// a link. Outside `folder`, `..`, through a link or anything but a regular
/// file is [`Refused::Missing`]. A path that names `folder` by its resolved
/// name is taken too.
pub fn hold_beneath(folder: &Path, named: &Path) -> Result<(Root, String)> {
    let outside = || missing!("{} is not a file in {}", named.display(), folder.display());
    if !named.is_absolute() {
        return Err(outside());
    }
    let rel = named
        .strip_prefix(folder)
        .ok()
        .map(Path::to_path_buf)
        .or_else(|| named.strip_prefix(folder.canonicalize().ok()?).ok().map(Path::to_path_buf))
        .ok_or_else(outside)?;
    let rel = rel.to_str().filter(|r| git::is_relative_file(r)).ok_or_else(outside)?.to_string();
    let root = Root::open(folder).map_err(|_| outside())?;
    match git::open_beneath(&root, &rel) {
        Some(Beneath::File(f)) if f.metadata().is_ok_and(|m| m.is_file()) => Ok((root, rel)),
        _ => Err(outside()),
    }
}

/// An image file the Host's config names, opened without following a link:
/// a regular file of at most [`BYTES_LIMIT`] whose first bytes are an
/// image. Its type and bytes.
pub fn read_image(file: std::fs::File) -> Result<(String, Vec<u8>)> {
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(invalid!("The image is not a regular file."));
    }
    if meta.len() > BYTES_LIMIT {
        return Err(invalid!("The image is {}. Skiff shows images up to {}.", human(meta.len()), human(BYTES_LIMIT)));
    }
    let mut bytes = Vec::new();
    file.take(BYTES_LIMIT + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > BYTES_LIMIT {
        return Err(invalid!("The image grew past {} while it was read.", human(BYTES_LIMIT)));
    }
    let mime = image_type(&bytes).ok_or_else(|| invalid!("The file is not a PNG, JPEG, GIF, WebP, BMP or ICO image."))?;
    Ok((mime.to_string(), bytes))
}

/// A size for a message: KiB or MiB.
pub fn human(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{} KiB", bytes.div_ceil(1024))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, process::Command};

    fn fixture(tag: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!("skiff-files-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn names(l: &Listing) -> Vec<(String, Kind)> {
        l.entries.iter().map(|e| (e.name.clone(), e.kind)).collect()
    }

    #[test]
    fn lists_folders_first_without_ignored_and_never_follows_links() {
        let root = fixture("list");
        let outside = fixture("list-outside");
        fs::create_dir_all(root.join("src/deep")).unwrap();
        fs::create_dir_all(root.join("target/debug")).unwrap();
        fs::write(root.join(".gitignore"), "target/\n*.log\n").unwrap();
        fs::write(root.join("README.md"), "").unwrap();
        fs::write(root.join("build.log"), "").unwrap();
        // Git quotes this name; it is still ignored, so it stays out.
        fs::write(root.join("tab\there.log"), "").unwrap();
        fs::write(root.join("b.txt"), "").unwrap();
        fs::write(root.join("src/deep/:(glob)*"), "").unwrap();
        fs::write(root.join(":(glob)x.log"), "").unwrap();
        fs::write(outside.join("secret"), "x").unwrap();
        std::os::unix::fs::symlink(&outside, root.join("out")).unwrap();
        assert!(Command::new("mkfifo").arg(root.join("pipe")).status().unwrap().success());
        assert!(Command::new("git").arg("init").arg("-q").arg(&root).status().unwrap().success());

        let held = Root::open(&root).unwrap();
        let top = list_in(&held, &root, "").unwrap();
        let deep = list_in(&held, &root, "src/deep").unwrap();
        // A link is a link: listing through it is refused, as is leaving the root.
        let through = list_in(&held, &root, "out");
        let up = list_in(&held, &root, "../x");
        let file = list_in(&held, &root, "b.txt");
        // This Mac asks to follow: the link lists as a folder and opens.
        // (A held root is read once: a fresh one for each listing.)
        let followed = list_with(&Root::open(&root).unwrap(), &root, "", true).unwrap();
        assert!(followed.entries.iter().any(|e| e.name == "out" && e.kind == Kind::Dir));
        assert_eq!(list_with(&Root::open(&root).unwrap(), &root, "out", true).unwrap().entries[0].name, "secret");
        fs::remove_dir_all(&root).unwrap();
        fs::remove_dir_all(&outside).unwrap();
        assert_eq!(
            names(&top),
            [
                ("src".into(), Kind::Dir),
                (".gitignore".into(), Kind::File),
                ("b.txt".into(), Kind::File),
                ("out".into(), Kind::Link),
                ("pipe".into(), Kind::Other),
                ("README.md".into(), Kind::File),
            ]
        );
        assert!(!top.truncated);
        assert_eq!(names(&deep), [(":(glob)*".into(), Kind::File)]);
        assert!(through.is_err() && up.is_err() && file.is_err());
    }

    /// The review's case: more ignored entries than the listing holds, then
    /// a file git does not ignore. It is listed; the ignored take no place.
    #[test]
    fn ignored_entries_take_no_place_in_the_listing() {
        let root = fixture("ignored-many");
        fs::create_dir_all(root.join("sub")).unwrap();
        for i in 0..LIST_LIMIT + 5 {
            fs::write(root.join(format!("sub/f{i}")), "").unwrap();
        }
        assert!(Command::new("git").arg("init").arg("-q").arg(&root).status().unwrap().success());
        fs::write(root.join("sub/.gitignore"), "").unwrap();
        // Every name but the one the disk reads last is ignored: before, the
        // cap was spent on the ignored ones before it was reached.
        let keep = fs::read_dir(root.join("sub")).unwrap().flatten().filter_map(|e| e.file_name().into_string().ok()).filter(|n| n.starts_with('f')).last().unwrap();
        fs::write(root.join("sub/.gitignore"), format!("*\n!{keep}\n")).unwrap();
        let held = Root::open(&root).unwrap();
        let listing = list_in(&held, &root, "sub").unwrap();
        fs::remove_dir_all(&root).unwrap();
        assert_eq!(names(&listing), [(keep, Kind::File)]);
        assert!(!listing.truncated, "every entry was read");
    }

    #[test]
    fn a_large_folder_is_cut_and_a_swapped_root_is_refused() {
        let root = fixture("many");
        for i in 0..LIST_LIMIT + 5 {
            fs::write(root.join(format!("f{i}")), "").unwrap();
        }
        let held = Root::open(&root).unwrap();
        let listing = list_in(&held, &root, "").unwrap();
        assert_eq!(listing.entries.len(), LIST_LIMIT);
        assert!(listing.truncated);
        // The path now names another folder: refused, never read.
        let moved = root.with_extension("moved");
        fs::rename(&root, &moved).unwrap();
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("other"), "other folder").unwrap();
        let list = list_in(&held, &root, "");
        let read = read_in(&held, &root, "other", Purpose::Preview);
        fs::remove_dir_all(&root).unwrap();
        fs::remove_dir_all(&moved).unwrap();
        assert!(list.unwrap_err().to_string().contains("replaced"));
        assert!(read.unwrap_err().to_string().contains("replaced"));
    }

    #[test]
    fn reads_are_typed_bounded_and_stay_in_the_worktree() {
        let root = fixture("read");
        let outside = fixture("read-outside");
        fs::create_dir_all(root.join("sub")).unwrap();
        fs::write(root.join("a.txt"), "one\ntwo\n").unwrap();
        fs::write(root.join("sub/b.rs"), "fn b() {}\n").unwrap();
        let png = [b"\x89PNG\r\n\x1a\n".as_slice(), &[0u8; 32]].concat();
        fs::write(root.join("i.png"), &png).unwrap();
        fs::write(root.join("fake.png"), "not an image").unwrap();
        fs::write(root.join("bin"), [1u8, 0, 2, 3]).unwrap();
        let long = "x".repeat(1000) + "\n";
        fs::write(root.join("long.txt"), long.repeat(TEXT_LIMIT / long.len() + 10)).unwrap();
        let big = fs::File::create(root.join("big.bin")).unwrap();
        big.set_len(BYTES_LIMIT + 1).unwrap();
        let mut big_png = png.clone();
        big_png.resize(BYTES_LIMIT as usize + 1, 0);
        fs::write(root.join("big.png"), &big_png).unwrap();
        fs::write(outside.join("secret"), "secret").unwrap();
        std::os::unix::fs::symlink(outside.join("secret"), root.join("link")).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("dirlink")).unwrap();
        assert!(Command::new("mkfifo").arg(root.join("pipe")).status().unwrap().success());
        let held = Root::open(&root).unwrap();
        let read = |rel: &str, p| read_in(&held, &root, rel, p);

        let text = read("a.txt", Purpose::Preview).unwrap();
        assert_eq!((text.content, text.text.as_deref(), text.truncated, text.size), (Content::Text, Some("one\ntwo\n"), false, 8));
        assert_eq!(read("sub/b.rs", Purpose::Preview).unwrap().text.as_deref(), Some("fn b() {}\n"));
        let image = read("i.png", Purpose::Preview).unwrap();
        assert_eq!((image.content, image.mime.as_deref(), image.bytes.as_deref()), (Content::Image, Some("image/png"), Some(png.as_slice())));
        // The name says png, the bytes say text.
        assert_eq!(read("fake.png", Purpose::Preview).unwrap().content, Content::Text);
        let bin = read("bin", Purpose::Preview).unwrap();
        assert_eq!((bin.content, bin.text, bin.bytes), (Content::Binary, None, None));
        let long = read("long.txt", Purpose::Preview).unwrap();
        assert!(long.truncated && long.text.as_ref().unwrap().len() <= TEXT_LIMIT && long.text.unwrap().ends_with('\n'));
        let large_image = read("big.png", Purpose::Preview).unwrap();
        assert_eq!((large_image.content, large_image.truncated, large_image.bytes), (Content::Image, true, None));
        assert_eq!(read("big.bin", Purpose::Preview).unwrap().content, Content::Binary);
        let link = read("link", Purpose::Preview).unwrap();
        assert_eq!((link.content, link.target.unwrap()), (Content::Link, outside.join("secret").display().to_string()));
        // Copies: the whole file, bounded; never a link, a FIFO or too much.
        assert_eq!(read("bin", Purpose::Copy).unwrap().bytes.unwrap(), [1u8, 0, 2, 3]);
        assert_eq!(read("long.txt", Purpose::Copy).unwrap().bytes.unwrap().len(), fs::metadata(root.join("long.txt")).unwrap().len() as usize);
        assert!(read("big.bin", Purpose::Copy).unwrap_err().to_string().contains("Skiff copies files up to 5.0 MiB"));
        assert!(read("link", Purpose::Copy).is_err());
        // Never followed, never opened, never outside.
        let started = std::time::Instant::now();
        for rel in ["pipe", "dirlink/secret", "../read-outside/secret", "/etc/hosts", "sub", ".", ""] {
            assert!(read(rel, Purpose::Preview).is_err(), "{rel}");
            assert!(read(rel, Purpose::Copy).is_err(), "{rel}");
        }
        assert!(started.elapsed() < std::time::Duration::from_secs(2), "a FIFO blocked");
        fs::remove_dir_all(&root).unwrap();
        fs::remove_dir_all(&outside).unwrap();
    }

    #[test]
    fn a_configured_image_is_checked_by_its_bytes_and_size() {
        let root = fixture("image");
        let png = [b"\x89PNG\r\n\x1a\n".as_slice(), &[0u8; 8]].concat();
        fs::write(root.join("bg.jpg"), &png).unwrap();
        fs::write(root.join("bg.png"), "<svg onload=x>").unwrap();
        fs::File::create(root.join("huge.png")).unwrap().set_len(BYTES_LIMIT + 1).unwrap();
        assert!(Command::new("mkfifo").arg(root.join("pipe.png")).status().unwrap().success());
        let ok = read_background(&root, "bg.jpg").unwrap();
        let errors = ["bg.png", "huge.png", "pipe.png", "missing.png", "", "."].map(|n| read_background(&root, n).is_err());
        fs::remove_dir_all(&root).unwrap();
        assert_eq!(ok, ("image/png".to_string(), png));
        assert_eq!(errors, [true; 6]);
    }

    /// The review's case: a background that is a link, or goes through one,
    /// to an image outside is never followed. An absolute regular image is.
    #[test]
    fn a_configured_image_is_never_read_through_a_link() {
        // Canonical, so the absolute name has no link of the system's in it.
        let root = fixture("bg-links").canonicalize().unwrap();
        let outside = fixture("bg-outside").canonicalize().unwrap();
        let png = [b"\x89PNG\r\n\x1a\n".as_slice(), &[9u8; 8]].concat();
        fs::write(outside.join("bg.png"), &png).unwrap();
        fs::create_dir_all(root.join("art")).unwrap();
        fs::write(root.join("art/own.png"), &png).unwrap();
        std::os::unix::fs::symlink(outside.join("bg.png"), root.join("bg-link.png")).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("dirlink")).unwrap();
        std::os::unix::fs::symlink(&root, outside.join("back")).unwrap();
        let direct = read_background(&root, "bg-link.png");
        let through = read_background(&root, "dirlink/bg.png");
        let up = read_background(&root, "../bg-outside/bg.png");
        let absolute = read_background(&root, &outside.join("bg.png").display().to_string());
        let absolute_link = read_background(&root, &root.join("bg-link.png").display().to_string());
        let absolute_through = read_background(&root, &outside.join("back/art/own.png").display().to_string());
        let inside = read_background(&root, "art/own.png");
        fs::remove_dir_all(&root).unwrap();
        fs::remove_dir_all(&outside).unwrap();
        assert!(direct.unwrap_err().to_string().contains("is a link"));
        assert!(through.is_err() && up.is_err() && absolute_link.is_err() && absolute_through.is_err());
        assert_eq!(absolute.unwrap().1, png, "an absolute regular image is still served");
        assert_eq!(inside.unwrap().1, png);
    }
}
