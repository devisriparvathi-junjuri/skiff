//! Puts the bundled `skiff` CLI on the user's PATH, so the CLI ships and
//! updates with the app. The .deb and .rpm install it to /usr/bin, so they
//! need nothing here. The AppImage and the macOS app keep it inside the
//! bundle: the app copies or links it out on every start.
//!
//! The command location the app manages is the `skiff` it placed before, or
//! a `skiff` link of a checkout there: a release app takes it back at start,
//! so the command updates with the app. It replaces the link, never what the
//! link points at, and never a file of the user's.
//!
//! The Linux .deb and .rpm apps do the same for a checkout's link in
//! ~/.local/bin: they point it at /usr/bin/skiff again.
//!
//! The Linux dev build (`pnpm tauri dev`) builds the CLI beside the app
//! binary and links ~/.local/bin/skiff to it on every start, so the `skiff`
//! on PATH is the one built from this tree. It replaces only a link or a
//! skiff CLI there. A macOS dev build leaves a release app's link alone.

use std::path::{Path, PathBuf};

/// The CLI next to the app binary, if this build ships one.
fn bundled() -> Option<PathBuf> {
    let cli = std::env::current_exe().ok()?.with_file_name("skiff");
    cli.is_file().then_some(cli)
}

/// Runs at app start, off the main thread. Never prompts. A failure leaves
/// everything as it was and is only logged. It links this Mac's CLI for the
/// local Host's sessions, so only a local endpoint may: `_local` proves it.
pub fn sync(_local: &skiff_client::host::local::ProcessControl) {
    if let Err(e) = imp::sync() {
        eprintln!("skiff CLI not updated: {e:#}");
    }
}

/// The palette and menu command. On macOS it asks for an admin password
/// when /usr/local/bin is not writable. Returns where the CLI is.
#[tauri::command]
pub async fn install_cli() -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(|| imp::install().map(|p| p.display().to_string()))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| format!("{e:#}"))
}

/// A `skiff` that a Skiff bundle placed: a link into a Skiff.app, or a copy
/// that answers `--version` as the skiff CLI. Anything else belongs to the
/// user and stays.
#[allow(dead_code)]
fn ours(dest: &Path) -> bool {
    if let Ok(target) = std::fs::read_link(dest) {
        return into_app(&target);
    }
    std::process::Command::new(dest)
        .arg("--version")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).starts_with("skiff "))
        .unwrap_or(false)
}

/// A link target inside a macOS app bundle.
#[allow(dead_code)]
fn into_app(target: &Path) -> bool {
    target.to_string_lossy().ends_with(".app/Contents/MacOS/skiff")
}

/// A link named `skiff` to a program named `skiff`: the app's, or one from a
/// checkout (`target/debug/skiff`), whether it still resolves or not.
#[allow(dead_code)]
fn skiff_link(dest: &Path) -> bool {
    std::fs::read_link(dest).is_ok_and(|t| t.file_name().is_some_and(|n| n == "skiff"))
}

/// The app may replace `dest`: a skiff link or a copy of ours. A dev build
/// leaves a link into an app bundle alone.
#[allow(dead_code)]
fn replaceable(dest: &Path, dev: bool) -> bool {
    match std::fs::read_link(dest) {
        Ok(t) => skiff_link(dest) && !(dev && into_app(&t)),
        Err(_) => ours(dest),
    }
}

#[allow(dead_code)]
fn links_to(dest: &Path, src: &Path) -> bool {
    std::fs::read_link(dest).is_ok_and(|t| t == src)
}

/// Points `dest` at `src`: a new link beside it, renamed over it. `skiff`
/// never goes missing, and the old link's target stays as it was.
#[cfg(unix)]
#[allow(dead_code)]
fn relink(dest: &Path, src: &Path) -> anyhow::Result<()> {
    use anyhow::Context;
    let dir = dest.parent().context("no folder")?;
    let tmp = dir.join(format!(".skiff.link.{}", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    std::os::unix::fs::symlink(src, &tmp).with_context(|| format!("link {}", tmp.display()))?;
    if let Err(e) = std::fs::rename(&tmp, dest) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e).with_context(|| format!("replace {}", dest.display()));
    }
    Ok(())
}

/// What [`reconcile`] did.
#[derive(Debug, PartialEq)]
#[allow(dead_code)]
pub(crate) enum Linked {
    /// Each managed `skiff` already pointed at the bundle.
    Current(Vec<PathBuf>),
    /// It pointed these at the bundle.
    Updated(Vec<PathBuf>),
    /// There was none, and it made this one.
    Added(PathBuf),
    /// There was none, and no folder took a link without a password.
    Nowhere,
}

/// Makes each `skiff` the app manages in `dirs` point at `src`. When there
/// is none, it adds one in the first folder that takes it. A `skiff` it may
/// not replace stays, and does not count as managed. An error names each
/// managed `skiff` it could not update; the others are updated.
#[cfg(unix)]
#[allow(dead_code)]
pub(crate) fn reconcile(dirs: &[&Path], src: &Path, dev: bool) -> anyhow::Result<Linked> {
    let (mut current, mut stale) = (Vec::new(), Vec::new());
    for dir in dirs {
        let dest = dir.join("skiff");
        if dest.symlink_metadata().is_err() {
            continue;
        }
        if links_to(&dest, src) {
            current.push(dest);
        } else if replaceable(&dest, dev) {
            stale.push(dest);
        }
    }
    if current.is_empty() && stale.is_empty() {
        for dir in dirs {
            let dest = dir.join("skiff");
            if dest.symlink_metadata().is_err() && relink(&dest, src).is_ok() {
                return Ok(Linked::Added(dest));
            }
        }
        return Ok(Linked::Nowhere);
    }
    if stale.is_empty() {
        return Ok(Linked::Current(current));
    }
    let mut failed = Vec::new();
    let mut updated = Vec::new();
    for dest in stale {
        match relink(&dest, src) {
            Ok(()) => updated.push(dest),
            Err(e) => failed.push(format!("{e:#}")),
        }
    }
    if !failed.is_empty() {
        anyhow::bail!("{}", failed.join("; "));
    }
    Ok(Linked::Updated(updated))
}

#[cfg(target_os = "linux")]
mod imp {
    use super::{bundled, links_to, relink, replaceable};
    use anyhow::{bail, Context};
    use std::path::{Path, PathBuf};

    /// The AppImage's mount point changes on every start, so it copies the
    /// CLI to ~/.local/bin, which most distros put on PATH. A dev build
    /// links it there instead, so it follows each rebuild. A .deb or .rpm
    /// app takes back a checkout's link there.
    pub fn sync() -> anyhow::Result<()> {
        if std::env::var_os("APPIMAGE").is_none() {
            let home = std::env::var_os("HOME").context("HOME is not set")?;
            let dir = PathBuf::from(home).join(".local/bin");
            if cfg!(debug_assertions) {
                let src = bundled().context("this dev build has no skiff CLI beside the app")?;
                dev_link(&src, &dir)?;
            } else if let Some(src) = bundled() {
                if let Some(dest) = take_back(&src, &dir)? {
                    eprintln!("skiff CLI: {} now points at {}", dest.display(), src.display());
                }
            }
            return Ok(());
        }
        install().map(|_| ())
    }

    /// Points `dir/skiff` at `src` when it is a link into a checkout
    /// (`target/debug/skiff` or `target/release/skiff`), whether that still
    /// resolves or not. Replaces only the link. Anything else stays, and no
    /// link is added: the package puts `src` on PATH already.
    pub(super) fn take_back(src: &Path, dir: &Path) -> anyhow::Result<Option<PathBuf>> {
        let dest = dir.join("skiff");
        let Ok(target) = std::fs::read_link(&dest) else { return Ok(None) };
        let checkout = target.ends_with("target/debug/skiff") || target.ends_with("target/release/skiff");
        if !checkout || links_to(&dest, src) {
            return Ok(None);
        }
        relink(&dest, src)?;
        Ok(Some(dest))
    }

    /// Links `dir/skiff` to `src`. Replaces only a link or a skiff CLI: a
    /// file of the user's stays.
    pub(super) fn dev_link(src: &Path, dir: &Path) -> anyhow::Result<PathBuf> {
        let dest = dir.join("skiff");
        if std::fs::read_link(&dest).is_ok_and(|t| t == src) {
            return Ok(dest);
        }
        if let Ok(meta) = dest.symlink_metadata() {
            if !meta.file_type().is_symlink() && !replaceable(&dest, true) {
                bail!("{} is not the skiff CLI. Left as it is", dest.display());
            }
        }
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
        // Link beside it and rename, so `skiff` never goes missing.
        let tmp = dir.join(".skiff.link");
        let _ = std::fs::remove_file(&tmp);
        std::os::unix::fs::symlink(src, &tmp).with_context(|| format!("link {}", tmp.display()))?;
        std::fs::rename(&tmp, &dest).with_context(|| format!("replace {}", dest.display()))?;
        Ok(dest)
    }

    pub fn install() -> anyhow::Result<PathBuf> {
        if std::env::var_os("APPIMAGE").is_none() {
            if let Some(cli) = bundled() {
                return Ok(cli);
            }
            bail!("this build has no skiff CLI");
        }
        let src = bundled().context("the AppImage has no skiff CLI")?;
        let home = std::env::var_os("HOME").context("HOME is not set")?;
        let dir = PathBuf::from(home).join(".local/bin");
        let dest = dir.join("skiff");
        let new = std::fs::read(&src).with_context(|| format!("read {}", src.display()))?;
        if dest.symlink_metadata().is_ok() {
            if std::fs::read(&dest).is_ok_and(|old| old == new) {
                return Ok(dest);
            }
            // A link of a checkout counts: the copy replaces the link, not its target.
            if !replaceable(&dest, false) {
                bail!("{} is not the skiff CLI. Remove it, then try again", dest.display());
            }
        }
        std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
        // Write beside it and rename, so a running `skiff` never sees half a file.
        let tmp = dir.join(".skiff.new");
        std::fs::write(&tmp, &new).with_context(|| format!("write {}", tmp.display()))?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
        std::fs::rename(&tmp, &dest).with_context(|| format!("replace {}", dest.display()))?;
        Ok(dest)
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use super::{bundled, links_to, reconcile, relink, replaceable, Linked};
    use anyhow::{bail, Context};
    use std::path::{Path, PathBuf};

    /// /usr/local/bin is on the default macOS PATH (/etc/paths).
    /// /opt/homebrew/bin is on PATH for Homebrew users on Apple silicon.
    const DIRS: [&str; 2] = ["/usr/local/bin", "/opt/homebrew/bin"];

    fn source() -> anyhow::Result<PathBuf> {
        let src = bundled().context("this build has no skiff CLI")?;
        let s = src.to_string_lossy();
        // A link into the disk image or a translocated copy breaks later.
        if crate::updater::in_disk_image(&s) {
            bail!("move Skiff to Applications first, then try again");
        }
        Ok(src)
    }

    /// Points the managed `skiff` at this bundle: one an older app placed,
    /// or a checkout's link there. Adds one where it can without a password
    /// when there is none. Never prompts.
    pub fn sync() -> anyhow::Result<()> {
        let Ok(src) = source() else { return Ok(()) };
        let dirs = DIRS.map(Path::new);
        if let Linked::Updated(at) = reconcile(&dirs, &src, cfg!(debug_assertions))? {
            for dest in at {
                eprintln!("skiff CLI: {} now points at {}", dest.display(), src.display());
            }
        }
        Ok(())
    }

    fn try_link(dir: &Path, src: &Path) -> anyhow::Result<PathBuf> {
        let dest = dir.join("skiff");
        if dest.symlink_metadata().is_ok() && !replaceable(&dest, cfg!(debug_assertions)) {
            bail!("{} is not the skiff CLI", dest.display());
        }
        relink(&dest, src)?;
        Ok(dest)
    }

    pub fn install() -> anyhow::Result<PathBuf> {
        let src = source()?;
        for dir in DIRS {
            let dest = Path::new(dir).join("skiff");
            if links_to(&dest, &src) {
                return Ok(dest);
            }
        }
        for dir in DIRS {
            if let Ok(dest) = try_link(Path::new(dir), &src) {
                return Ok(dest);
            }
        }
        let dest = Path::new(DIRS[0]).join("skiff");
        if dest.symlink_metadata().is_ok() && !replaceable(&dest, false) {
            bail!("{} is not the skiff CLI. Remove it, then try again", dest.display());
        }
        let quote = |p: &Path| format!("'{}'", p.display().to_string().replace('\'', r"'\''"));
        let sh = format!("mkdir -p {} && ln -sf {} {}", DIRS[0], quote(&src), quote(&dest));
        let script = format!(
            "do shell script \"{}\" with prompt \"Skiff wants to add the skiff command to {}.\" with administrator privileges",
            sh.replace('\\', "\\\\").replace('"', "\\\""),
            DIRS[0],
        );
        let out = std::process::Command::new("osascript").arg("-e").arg(&script).output()?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            if err.contains("-128") {
                bail!("cancelled");
            }
            bail!("osascript: {}", err.trim());
        }
        Ok(dest)
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod imp {
    use std::path::PathBuf;
    pub fn sync() -> anyhow::Result<()> {
        Ok(())
    }
    pub fn install() -> anyhow::Result<PathBuf> {
        anyhow::bail!("not supported on this platform")
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::imp::{dev_link, take_back};
    use std::os::unix::fs::symlink;

    #[test]
    fn dev_link_replaces_a_link_and_keeps_a_file_of_the_users() {
        let tmp = std::env::temp_dir().join(format!("skiff-cli-link-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let bin = tmp.join("home/.local/bin");
        let src = tmp.join("target/debug/skiff");
        std::fs::create_dir_all(src.parent().unwrap()).unwrap();
        std::fs::write(&src, "").unwrap();
        // No file yet: the link is made, folder and all.
        let dest = dev_link(&src, &bin).unwrap();
        assert_eq!(std::fs::read_link(&dest).unwrap(), src);
        // A link elsewhere is replaced.
        std::fs::remove_file(&dest).unwrap();
        std::os::unix::fs::symlink("/nowhere/skiff", &dest).unwrap();
        dev_link(&src, &bin).unwrap();
        assert_eq!(std::fs::read_link(&dest).unwrap(), src);
        // A file that is not the skiff CLI stays.
        std::fs::remove_file(&dest).unwrap();
        std::fs::write(&dest, "#!/bin/sh\necho mine\n").unwrap();
        assert!(dev_link(&src, &bin).is_err());
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "#!/bin/sh\necho mine\n");
        std::fs::remove_dir_all(&tmp).unwrap();
    }

    /// A .deb app points a checkout's link at its own CLI again. The
    /// checkout binary stays; a file of the user's, a link to another
    /// program, or no `skiff` at all stays as it is.
    #[test]
    fn a_release_app_takes_back_a_checkout_link() {
        let tmp = std::env::temp_dir().join(format!("skiff-cli-take-back-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let bin = tmp.join("home/.local/bin");
        let src = tmp.join("usr/bin/skiff");
        let checkout = tmp.join("checkout/target/debug/skiff");
        for d in [bin.as_path(), src.parent().unwrap(), checkout.parent().unwrap()] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::write(&src, "release").unwrap();
        std::fs::write(&checkout, "checkout").unwrap();
        let dest = bin.join("skiff");
        // No skiff: none is added.
        assert_eq!(take_back(&src, &bin).unwrap(), None);
        assert!(dest.symlink_metadata().is_err());
        // A checkout's link points at the app's CLI again.
        symlink(&checkout, &dest).unwrap();
        assert_eq!(take_back(&src, &bin).unwrap(), Some(dest.clone()));
        assert_eq!(std::fs::read_link(&dest).unwrap(), src);
        assert_eq!(std::fs::read_to_string(&checkout).unwrap(), "checkout");
        // Again: nothing to do.
        assert_eq!(take_back(&src, &bin).unwrap(), None);
        // A dangling release-build link of a removed checkout counts too.
        std::fs::remove_file(&dest).unwrap();
        symlink("/nowhere/target/release/skiff", &dest).unwrap();
        assert_eq!(take_back(&src, &bin).unwrap(), Some(dest.clone()));
        assert_eq!(std::fs::read_link(&dest).unwrap(), src);
        // A link to another program stays.
        std::fs::remove_file(&dest).unwrap();
        symlink("/usr/bin/true", &dest).unwrap();
        assert_eq!(take_back(&src, &bin).unwrap(), None);
        assert_eq!(std::fs::read_link(&dest).unwrap(), std::path::Path::new("/usr/bin/true"));
        // A file of the user's stays.
        std::fs::remove_file(&dest).unwrap();
        std::fs::write(&dest, "#!/bin/sh\necho mine\n").unwrap();
        assert_eq!(take_back(&src, &bin).unwrap(), None);
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "#!/bin/sh\necho mine\n");
        std::fs::remove_dir_all(&tmp).unwrap();
    }
}

#[cfg(all(test, unix))]
mod reconcile_tests {
    use super::{reconcile, Linked};
    use std::os::unix::fs::{symlink, PermissionsExt};
    use std::path::{Path, PathBuf};

    fn fixture(name: &str) -> PathBuf {
        let tmp = std::env::temp_dir().join(format!("skiff-cli-reconcile-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        for d in ["local", "brew", "Skiff.app/Contents/MacOS", "checkout/target/debug"] {
            std::fs::create_dir_all(tmp.join(d)).unwrap();
        }
        std::fs::write(tmp.join("Skiff.app/Contents/MacOS/skiff"), "release").unwrap();
        std::fs::write(tmp.join("checkout/target/debug/skiff"), "checkout").unwrap();
        tmp
    }

    /// A checkout's link at the managed place points at the bundle again. The
    /// link is replaced; the checkout binary it pointed at stays as it was. No
    /// second `skiff` appears in the other folder.
    #[test]
    fn a_checkout_link_points_at_the_bundle_again() {
        let tmp = fixture("checkout");
        let (local, brew) = (tmp.join("local"), tmp.join("brew"));
        let src = tmp.join("Skiff.app/Contents/MacOS/skiff");
        let checkout = tmp.join("checkout/target/debug/skiff");
        symlink(&checkout, local.join("skiff")).unwrap();
        let dirs = [local.as_path(), brew.as_path()];
        assert_eq!(reconcile(&dirs, &src, false).unwrap(), Linked::Updated(vec![local.join("skiff")]));
        assert_eq!(std::fs::read_link(local.join("skiff")).unwrap(), src);
        assert_eq!(std::fs::read_to_string(&checkout).unwrap(), "checkout");
        assert!(brew.join("skiff").symlink_metadata().is_err());
        // Again: nothing to do.
        assert_eq!(reconcile(&dirs, &src, false).unwrap(), Linked::Current(vec![local.join("skiff")]));
        // A dangling link of a removed checkout counts too.
        std::fs::remove_file(local.join("skiff")).unwrap();
        symlink("/nowhere/target/debug/skiff", local.join("skiff")).unwrap();
        assert!(matches!(reconcile(&dirs, &src, false).unwrap(), Linked::Updated(_)));
        std::fs::remove_dir_all(&tmp).unwrap();
    }

    /// A file of the user's, or a link to another program, stays, and does
    /// not count as the managed place. With none managed, the first folder
    /// that takes a link gets one.
    #[test]
    fn leaves_what_is_not_skiff() {
        let tmp = fixture("user");
        let (local, brew) = (tmp.join("local"), tmp.join("brew"));
        let src = tmp.join("Skiff.app/Contents/MacOS/skiff");
        std::fs::write(local.join("skiff"), "#!/bin/sh\necho mine\n").unwrap();
        std::fs::set_permissions(local.join("skiff"), std::fs::Permissions::from_mode(0o755)).unwrap();
        let dirs = [local.as_path(), brew.as_path()];
        assert_eq!(reconcile(&dirs, &src, false).unwrap(), Linked::Added(brew.join("skiff")));
        assert_eq!(std::fs::read_to_string(local.join("skiff")).unwrap(), "#!/bin/sh\necho mine\n");
        std::fs::remove_file(brew.join("skiff")).unwrap();
        symlink("/usr/bin/true", brew.join("skiff-other")).unwrap();
        std::fs::rename(brew.join("skiff-other"), brew.join("skiff")).unwrap();
        // A link named skiff to another program is not ours either.
        assert_eq!(reconcile(&dirs, &src, false).unwrap(), Linked::Nowhere);
        assert_eq!(std::fs::read_link(brew.join("skiff")).unwrap(), Path::new("/usr/bin/true"));
        std::fs::remove_dir_all(&tmp).unwrap();
    }

    /// A dev build leaves a release app's link alone: it keeps its own
    /// socket and state, and the installed command stays the release one.
    #[test]
    fn a_dev_build_leaves_the_release_link() {
        let tmp = fixture("dev");
        let local = tmp.join("local");
        let release = tmp.join("Skiff.app/Contents/MacOS/skiff");
        let checkout = tmp.join("checkout/target/debug/skiff");
        symlink(&release, local.join("skiff")).unwrap();
        let dirs = [local.as_path()];
        assert!(matches!(reconcile(&dirs, &checkout, true).unwrap(), Linked::Nowhere));
        assert_eq!(std::fs::read_link(local.join("skiff")).unwrap(), release);
        std::fs::remove_dir_all(&tmp).unwrap();
    }

    /// A managed place the app cannot write is reported, not skipped, and
    /// the link and its target stay as they were. Root ignores the mode.
    #[test]
    fn a_denied_write_is_reported_and_changes_nothing() {
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let tmp = fixture("denied");
        let (local, brew) = (tmp.join("local"), tmp.join("brew"));
        let src = tmp.join("Skiff.app/Contents/MacOS/skiff");
        let checkout = tmp.join("checkout/target/debug/skiff");
        symlink(&checkout, local.join("skiff")).unwrap();
        std::fs::set_permissions(&local, std::fs::Permissions::from_mode(0o555)).unwrap();
        let dirs = [local.as_path(), brew.as_path()];
        let err = reconcile(&dirs, &src, false).unwrap_err().to_string();
        std::fs::set_permissions(&local, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(err.contains("skiff"), "{err}");
        assert_eq!(std::fs::read_link(local.join("skiff")).unwrap(), checkout);
        // No second command elsewhere.
        assert!(brew.join("skiff").symlink_metadata().is_err());
        assert_eq!(std::fs::read_dir(&local).unwrap().count(), 1);
        std::fs::remove_dir_all(&tmp).unwrap();
    }
}
