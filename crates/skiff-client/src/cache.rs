//! The image cache: images dropped or pasted on a pane are copied here, and
//! the pane gets the copy's path: `<agent folder>/cache/<hash>.<ext>`. The
//! cache does not need the Skiff agent to be on. Other files keep their own
//! path.
//!
//! The first bytes decide what is an image, not the extension. The name is
//! the first 16 hex characters of the SHA-256 of the bytes, so the same
//! image is one file. A repeat sets the mtime to now and writes nothing.
//!
//! The folder is the Images library, newest first. `images.json` beside the
//! images keeps each one's own name, the session that last got it, and its
//! pin. Flushes skip pinned images and the index.

use std::{
    collections::HashSet,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use skiff_core::{
    config::Config,
    project::AgentInfo,
    session::{Role, SessionSpec},
    shell::quote_word,
    tasks,
};

/// An image larger than this keeps its own path.
pub const MAX_BYTES: u64 = 50 * 1024 * 1024;

/// What `showError` says for an image over [`MAX_BYTES`].
pub const TOO_LARGE: &str = "The image is larger than 50 MB. Skiff typed its path and did not copy it.";

/// The cache folder. It may not exist yet: [`ensure`] makes it.
pub fn dir(cfg: &Config) -> Result<PathBuf> {
    crate::skipper::cache_dir(cfg)
}

/// Makes the agent folder and its cache when they are missing, and returns
/// the cache. See [`crate::skipper::make_folder`].
pub fn ensure(cfg: &Config) -> Result<PathBuf> {
    Ok(crate::skipper::make_folder(cfg)?.join("cache"))
}

/// The cache extension of an image, from its first bytes. SVG also needs the
/// `.svg` extension. `None`: not an image.
pub fn image_ext(head: &[u8], file_ext: Option<&str>) -> Option<&'static str> {
    let at = |i: usize, sig: &[u8]| head.get(i..i + sig.len()) == Some(sig);
    if at(0, b"\x89PNG\r\n\x1a\n") {
        return Some("png");
    }
    if at(0, b"\xff\xd8\xff") {
        return Some("jpg");
    }
    if at(0, b"GIF87a") || at(0, b"GIF89a") {
        return Some("gif");
    }
    if at(0, b"RIFF") && at(8, b"WEBP") {
        return Some("webp");
    }
    if at(0, b"BM") && head.len() >= 26 {
        return Some("bmp");
    }
    if at(0, b"II*\0") || at(0, b"MM\0*") {
        return Some("tif");
    }
    if at(4, b"ftyp") {
        return match head.get(8..12) {
            Some(b"avif" | b"avis") => Some("avif"),
            Some(b"heic" | b"heix" | b"hevc" | b"hevx" | b"heim" | b"heis" | b"hevm" | b"hevs" | b"mif1" | b"msf1") => Some("heic"),
            _ => None,
        };
    }
    if file_ext.is_some_and(|e| e.eq_ignore_ascii_case("svg")) {
        let text = String::from_utf8_lossy(head);
        let t = text.trim_start_matches('\u{feff}').trim_start();
        if (t.starts_with("<svg") || t.starts_with("<?xml")) && t.contains("<svg") {
            return Some("svg");
        }
    }
    None
}

/// Puts `bytes` in the cache as `<hash>.<ext>`. An existing copy gets a new
/// mtime, so an image still in use stays. A new one is written beside its
/// name and renamed into place, so a crash leaves no half file.
pub fn store(dir: &Path, bytes: &[u8], ext: &str) -> Result<PathBuf> {
    write_once(&dir.join(format!("{}.{ext}", hash16(bytes))), bytes)
}

/// The first 16 hex characters of the bytes' SHA-256.
fn hash16(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().take(8).map(|b| format!("{b:02x}")).collect()
}

/// A file a client sent for a pane on this Host (a drop or a paste on a
/// remote pane): an image as [`store`] keeps it, any other file as
/// `cache/files/<hash>/<name>`, so it keeps its name. `name` is one file
/// name; anything else is refused.
pub fn put(cfg: &Config, name: &str, bytes: &[u8]) -> Result<PathBuf> {
    let name = name.trim();
    if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\0']) {
        return Err(skiff_core::config::Refused::Invalid(format!("{name:?} is not a file name")).into());
    }
    let dir = ensure(cfg)?;
    let ext = Path::new(name).extension().and_then(|e| e.to_str());
    if let Some(ext) = image_ext(bytes, ext) {
        return store(&dir, bytes, ext);
    }
    let sub = dir.join("files").join(hash16(bytes));
    std::fs::create_dir_all(&sub).with_context(|| format!("create {}", sub.display()))?;
    write_once(&sub.join(name), bytes)
}

/// Writes `path` beside its name and renames it into place. An existing
/// file gets a new mtime instead.
fn write_once(path: &Path, bytes: &[u8]) -> Result<PathBuf> {
    let path = path.to_path_buf();
    let dir = path.parent().unwrap_or(Path::new("."));
    if path.is_file() {
        let f = std::fs::File::options().write(true).open(&path).with_context(|| format!("open {}", path.display()))?;
        f.set_modified(std::time::SystemTime::now()).with_context(|| format!("touch {}", path.display()))?;
        return Ok(path);
    }
    let tmp = dir.join(format!("{}.tmp-{}", path.file_name().unwrap_or_default().to_string_lossy(), std::process::id()));
    let write = || -> std::io::Result<()> {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        std::fs::rename(&tmp, &path)
    };
    if let Err(e) = write() {
        let _ = std::fs::remove_file(&tmp);
        return Err(e).with_context(|| format!("write {}", path.display()));
    }
    Ok(path)
}

/// An image Skiff put in the cache for a drop or a paste.
#[derive(Debug, Clone, PartialEq)]
pub struct Stored {
    pub path: PathBuf,
    /// The dropped file's own name. `None`: a paste.
    pub name: Option<String>,
}

/// What Skiff types for dropped or pasted files.
#[derive(Debug, Default, PartialEq)]
pub struct Typed {
    /// Each file's word, quoted, in order.
    pub words: Vec<String>,
    /// The images, in order.
    pub images: Vec<Stored>,
    /// An image was over [`MAX_BYTES`]: it kept its own path.
    pub too_large: bool,
}

/// The words for `paths`, in order: an image's cache copy, else the path as
/// it is. A failed copy fails the whole drop, so nothing is typed.
pub fn files(dir: &Path, paths: &[PathBuf]) -> Result<Typed> {
    let mut out = Typed::default();
    for p in paths {
        let word = |p: &Path| quote_word(&p.to_string_lossy());
        let Some((len, ext)) = image_of(p)? else {
            out.words.push(word(p));
            continue;
        };
        if len > MAX_BYTES {
            out.too_large = true;
            out.words.push(word(p));
            continue;
        }
        let bytes = std::fs::read(p).with_context(|| format!("read {}", p.display()))?;
        let stored = store(dir, &bytes, ext)?;
        out.words.push(word(&stored));
        out.images.push(Stored { path: stored, name: p.file_name().map(|n| n.to_string_lossy().into_owned()) });
    }
    Ok(out)
}

/// A file's length and cache extension when it is an image. A folder, or a
/// file Skiff cannot read, is not.
fn image_of(p: &Path) -> Result<Option<(u64, &'static str)>> {
    let Ok(m) = std::fs::metadata(p) else { return Ok(None) };
    if !m.is_file() {
        return Ok(None);
    }
    let Ok(f) = std::fs::File::open(p) else { return Ok(None) };
    let mut head = Vec::with_capacity(4096);
    if f.take(4096).read_to_end(&mut head).is_err() {
        return Ok(None);
    }
    let file_ext = p.extension().and_then(|e| e.to_str());
    Ok(image_ext(&head, file_ext).map(|e| (m.len(), e)))
}

/// The library's index, beside the images.
pub const INDEX: &str = "images.json";

/// One image of the library, as `images.json` keeps it.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Entry {
    /// The cache file's name.
    pub file: String,
    /// The dropped file's own name. `None`: a paste, or a file with no entry.
    #[serde(default)]
    pub name: Option<String>,
    /// The session that last got the image, with its title and project as
    /// its pane showed them then.
    #[serde(default)]
    pub session: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub project: Option<String>,
    /// Unix ms of the last drop, paste or Insert. A file with no entry: its mtime.
    pub at: u64,
    #[serde(default)]
    pub pinned: bool,
}

/// The session that gets an image, as its pane shows it.
#[derive(Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Who {
    pub session: String,
    pub title: String,
    pub project: Option<String>,
}

/// An image of the library: its path and its entry.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Image {
    pub path: String,
    #[serde(flatten)]
    pub entry: Entry,
    /// The file's length in bytes.
    pub size: u64,
}

/// One writer of `images.json` at a time in this process.
static INDEX_LOCK: Mutex<()> = Mutex::new(());

/// The file whose lock keeps writers in other processes (a second app, a
/// dev build) from writing over each other's entries.
const LOCK_FILE: &str = ".images.lock";

/// Holds both locks while an index change reads and writes the file.
struct Locked {
    _file: Option<std::fs::File>,
    _here: std::sync::MutexGuard<'static, ()>,
}

/// `images.json` or its lock file: the library's own files, not images.
pub fn is_index_file(p: &Path) -> bool {
    p.file_name().is_some_and(|n| n == INDEX || n == LOCK_FILE)
}

fn lock(dir: &Path) -> Locked {
    let here = INDEX_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let file = std::fs::File::options().create(true).truncate(false).write(true).open(dir.join(LOCK_FILE)).ok();
    // Without the lock file the change still runs, as before.
    let file = file.filter(|f| f.lock().is_ok());
    Locked { _file: file, _here: here }
}

/// A cache image's file name: no index, no temp file.
fn is_image_name(name: &str) -> bool {
    const EXTS: [&str; 9] = ["png", "jpg", "gif", "webp", "bmp", "tif", "avif", "heic", "svg"];
    !name.starts_with('.') && !name.contains(".tmp-") && name.rsplit_once('.').is_some_and(|(_, e)| EXTS.contains(&e))
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

fn read_index(dir: &Path) -> Vec<Entry> {
    std::fs::read_to_string(dir.join(INDEX)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

/// Writes the entries whose file is still there.
fn write_index(dir: &Path, entries: Vec<Entry>) -> Result<()> {
    let kept: Vec<Entry> = entries.into_iter().filter(|e| dir.join(&e.file).is_file()).collect();
    skiff_core::config::write_atomic(&dir.join(INDEX), &serde_json::to_string(&kept)?)
}

/// The cache file's name, when `file` is an image directly in `dir`.
fn name_in(dir: &Path, file: &Path) -> Result<String> {
    let name = file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let inside = file.parent().is_some_and(|p| p == dir || p.canonicalize().ok() == dir.canonicalize().ok());
    if !inside || !is_image_name(&name) {
        anyhow::bail!("{} is not in the Images folder", file.display());
    }
    Ok(name)
}

/// Every image in `dir`, newest first. A file with no entry shows with its
/// mtime; an entry with no file is left out.
pub fn list(dir: &Path) -> Vec<Image> {
    let index = read_index(dir);
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out: Vec<Image> = rd
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter_map(|e| {
            let file = e.file_name().to_string_lossy().into_owned();
            if !is_image_name(&file) {
                return None;
            }
            let meta = e.metadata().ok();
            let entry = index.iter().find(|x| x.file == file).cloned().unwrap_or_else(|| {
                let at = meta.as_ref().and_then(|m| m.modified().ok()).and_then(|t| t.duration_since(UNIX_EPOCH).ok());
                Entry { file: file.clone(), at: at.map_or(0, |d| d.as_millis() as u64), ..Default::default() }
            });
            Some(Image { path: e.path().to_string_lossy().into_owned(), entry, size: meta.map_or(0, |m| m.len()) })
        })
        .collect();
    out.sort_by(|a, b| b.entry.at.cmp(&a.entry.at).then_with(|| a.entry.file.cmp(&b.entry.file)));
    out
}

/// A drop, a paste or an Insert of `file` into `who`'s pane: the image goes to
/// the top, with its session and a new mtime. `name`: the dropped file's own
/// name; `None` keeps the one it has.
pub fn record(dir: &Path, file: &Path, name: Option<&str>, who: Option<&Who>) -> Result<()> {
    let key = name_in(dir, file)?;
    let _lock = lock(dir);
    let mut index = read_index(dir);
    let i = index.iter().position(|e| e.file == key).unwrap_or_else(|| {
        index.push(Entry { file: key.clone(), ..Default::default() });
        index.len() - 1
    });
    let e = &mut index[i];
    if name.is_some() {
        e.name = name.map(str::to_string);
    }
    if let Some(w) = who {
        e.session = Some(w.session.clone());
        e.title = Some(w.title.clone());
        e.project = w.project.clone();
    }
    e.at = now_ms();
    if let Ok(f) = std::fs::File::options().write(true).open(file) {
        let _ = f.set_modified(SystemTime::now());
    }
    write_index(dir, index)
}

/// Pins or unpins `file`. A flush never removes a pinned image.
pub fn pin(dir: &Path, file: &Path, on: bool) -> Result<()> {
    let key = name_in(dir, file)?;
    let _lock = lock(dir);
    let mut index = read_index(dir);
    match index.iter_mut().find(|e| e.file == key) {
        Some(e) => e.pinned = on,
        None => {
            let at = std::fs::metadata(file).and_then(|m| m.modified()).ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok());
            index.push(Entry { file: key, at: at.map_or_else(now_ms, |d| d.as_millis() as u64), pinned: on, ..Default::default() });
        }
    }
    write_index(dir, index)
}

/// Deletes `file` and its entry, pinned or not.
pub fn delete(dir: &Path, file: &Path) -> Result<()> {
    let key = name_in(dir, file)?;
    let _lock = lock(dir);
    match std::fs::remove_file(file) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e).with_context(|| format!("delete {}", file.display())),
        _ => {}
    }
    let index = read_index(dir).into_iter().filter(|e| e.file != key).collect();
    write_index(dir, index)
}

/// The cache paths a flush keeps: the pinned images and the index.
pub fn kept(dir: &Path) -> HashSet<PathBuf> {
    let root = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    read_index(dir).into_iter().filter(|e| e.pinned).map(|e| root.join(e.file)).chain([root.join(INDEX), root.join(LOCK_FILE)]).collect()
}

/// The flags for the agent's start line, as argv words. See [`tasks::cache_flags`].
pub fn flags(cfg: &Config, agent: &AgentInfo) -> Vec<String> {
    dir(cfg).ok().and_then(|d| tasks::cache_flags(agent, &d)).unwrap_or_default()
}

/// [`flags`] as text to type after a start line in a shell.
pub fn flags_typed(cfg: &Config, agent: &AgentInfo) -> String {
    dir(cfg).map(|d| tasks::cache_flags_typed(agent, &d)).unwrap_or_default()
}

/// Adds the cache flags to an agent session that runs one of `agents` as
/// its command, as the + menu, `skiff new` and Skipper start one.
pub fn add_flags(cfg: &Config, agents: &[AgentInfo], spec: &mut SessionSpec) {
    if spec.role != Role::Agent || spec.peek {
        return;
    }
    let Some(program) = spec.command.as_deref() else { return };
    let started = |a: &&AgentInfo| {
        let mut words = a.command.split_whitespace();
        words.next() == Some(program) && words.zip(spec.args.iter()).all(|(w, arg)| w == arg)
    };
    if let Some(a) = agents.iter().find(started) {
        let more = flags(cfg, a);
        spec.args.extend(more);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_put_keeps_images_by_hash_and_files_by_name() {
        let root = std::env::temp_dir().join(format!("skiff-put-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let cfg = Config { skiff_agent: skiff_core::config::SkiffAgent { folder: Some(root.clone()), ..Default::default() }, ..Default::default() };
        // A folder outside home is refused: the put falls back to the /tmp cache, which is fine here.
        let png = put(&cfg, "shot.png", b"\x89PNG\r\n\x1a\nbody").unwrap();
        let note = put(&cfg, "note.txt", b"pelican").unwrap();
        let again = put(&cfg, "note.txt", b"pelican").unwrap();
        let bad = put(&cfg, "../x", b"x").is_err() && put(&cfg, "", b"x").is_err();
        let note_text = std::fs::read(&note).unwrap();
        let _ = std::fs::remove_file(&png);
        let _ = std::fs::remove_dir_all(note.parent().unwrap());
        std::fs::remove_dir_all(&root).unwrap();
        assert!(png.extension().unwrap() == "png" && png.file_name().unwrap().len() == 20, "{png:?}");
        assert_eq!(note.file_name().unwrap(), "note.txt");
        assert_eq!(note.parent().unwrap().parent().unwrap().file_name().unwrap(), "files");
        assert_eq!(note, again);
        assert_eq!(note_text, b"pelican");
        assert!(bad);
    }

    #[test]
    fn the_first_bytes_decide() {
        assert_eq!(image_ext(b"\x89PNG\r\n\x1a\n....", Some("txt")), Some("png"));
        assert_eq!(image_ext(b"\xff\xd8\xff\xe0", None), Some("jpg"));
        assert_eq!(image_ext(b"GIF89a..", None), Some("gif"));
        assert_eq!(image_ext(b"RIFF\0\0\0\0WEBPVP8 ", None), Some("webp"));
        assert_eq!(image_ext(b"\0\0\0\x1cftypavif", None), Some("avif"));
        assert_eq!(image_ext(b"\0\0\0\x18ftypheic", None), Some("heic"));
        assert_eq!(image_ext(b"\0\0\0\x18ftypisom", None), None);
        assert_eq!(image_ext(b"II*\0", None), Some("tif"));
        assert_eq!(image_ext(b"<?xml version=\"1.0\"?><svg>", Some("SVG")), Some("svg"));
        assert_eq!(image_ext(b"<svg xmlns=", Some("txt")), None);
        assert_eq!(image_ext(b"<?xml version=\"1.0\"?><html>", Some("svg")), None);
        assert_eq!(image_ext(b"hello", Some("png")), None);
    }

    #[test]
    fn drops_copy_images_once_and_keep_other_paths() {
        let root = std::env::temp_dir().join(format!("skiff-cache-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let cache = root.join("cache");
        std::fs::create_dir_all(&cache).unwrap();
        let png = root.join("shot one.png");
        std::fs::write(&png, b"\x89PNG\r\n\x1a\nbody").unwrap();
        let note = root.join("note.txt");
        std::fs::write(&note, b"pelican").unwrap();
        let t = files(&cache, &[png.clone(), note.clone(), root.clone(), png.clone()]).unwrap();
        let copy = std::fs::read_dir(&cache).unwrap().map(|e| e.unwrap().path()).collect::<Vec<_>>();
        let named = copy.len() == 1 && copy[0].file_name().unwrap().to_string_lossy().len() == 16 + 4;
        std::fs::remove_dir_all(&root).unwrap();
        assert!(named, "{copy:?}");
        assert_eq!(t.images.len(), 2);
        assert_eq!(t.images[0], t.images[1]);
        assert_eq!(t.images[0].name.as_deref(), Some("shot one.png"));
        assert_eq!(t.words[0], quote_word(&t.images[0].path.to_string_lossy()));
        assert_eq!(t.words[1], note.to_string_lossy());
        assert_eq!(t.words[2], root.to_string_lossy());
        assert!(!t.too_large);
        assert_eq!(quote_word("/a/shot one.png"), "'/a/shot one.png'");
        assert_eq!(quote_word("/a/it's"), r"'/a/it'\''s'");
    }

    #[test]
    fn the_library_lists_records_pins_and_deletes() {
        let dir = std::env::temp_dir().join(format!("skiff-library-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let a = store(&dir, b"\x89PNG\r\n\x1a\na", "png").unwrap();
        let b = store(&dir, b"\x89PNG\r\n\x1a\nb", "png").unwrap();
        std::fs::write(dir.join("notes.txt"), "x").unwrap();
        // No entry yet: the cache name and the mtime.
        assert_eq!(list(&dir).len(), 2);
        assert_eq!(list(&dir)[0].size, 9);
        assert!(list(&dir).iter().all(|i| i.entry.session.is_none()));
        let who = Who { session: "s1".into(), title: "Header clips".into(), project: Some("skiff".into()) };
        record(&dir, &a, Some("header.png"), Some(&who)).unwrap();
        let top = &list(&dir)[0];
        assert_eq!((top.path.as_str(), top.entry.name.as_deref(), top.entry.title.as_deref()), (a.to_str().unwrap(), Some("header.png"), Some("Header clips")));
        // Insert keeps the name and moves the image to the top.
        std::thread::sleep(std::time::Duration::from_millis(2));
        record(&dir, &b, None, Some(&who)).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
        record(&dir, &a, None, None).unwrap();
        assert_eq!(list(&dir)[0].entry.name.as_deref(), Some("header.png"));
        pin(&dir, &b, true).unwrap();
        assert!(kept(&dir).contains(&dir.canonicalize().unwrap().join(b.file_name().unwrap())));
        // Disk use removes the image that is not pinned (see crate::disk).
        std::fs::remove_file(&a).unwrap();
        assert!(dir.join(INDEX).is_file());
        // The entry of a gone file drops on the next write.
        delete(&dir, &b).unwrap();
        assert!(list(&dir).is_empty() && read_index(&dir).is_empty());
        assert!(pin(&dir, &dir.join("notes.txt"), true).is_err());
        assert!(delete(&dir, Path::new("/etc/x.png")).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
