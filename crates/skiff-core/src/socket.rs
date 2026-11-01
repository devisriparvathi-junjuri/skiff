use std::path::PathBuf;

/// The one socket name. Every build, debug or release, of the app, the CLI
/// and skiffd uses it: one computer runs one skiffd, and the newer build
/// updates it in place.
pub const SOCKET_NAME: &str = "skiffd.sock";

/// Where `skiffd` listens. `SKIFF_SOCKET` overrides, for tests and for an
/// agent's throwaway daemon. Otherwise `<runtime dir>/skiff/skiffd.sock`
/// (see [`runtime_dir`]), the same in every build.
pub fn socket_path() -> PathBuf {
    socket_path_from(std::env::var_os("SKIFF_SOCKET").as_deref(), &runtime_dir())
}

/// [`socket_path`] for a given `SKIFF_SOCKET` value and runtime folder.
pub fn socket_path_from(env: Option<&std::ffi::OsStr>, runtime: &std::path::Path) -> PathBuf {
    match env.filter(|p| !p.is_empty()) {
        Some(p) => PathBuf::from(p),
        None => runtime.join("skiff").join(SOCKET_NAME),
    }
}

/// Where a Host's data-plane service (`skiff-host`) listens, beside the
/// skiffd socket it serves with: `x.sock` gives `x-host.sock`. Pure path
/// arithmetic, so it also names a remote Host's socket without touching
/// this machine.
pub fn data_socket(session: &std::path::Path) -> PathBuf {
    let stem = session.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "skiffd".into());
    session.with_file_name(format!("{stem}-host.sock"))
}

/// The user's runtime folder, the same from the desktop and from an SSH
/// login, where the environment can differ. On Linux, `$XDG_RUNTIME_DIR`,
/// else `/run/user/<uid>` when it exists. On macOS, the per-user temp folder
/// the system names, which is the desktop's `$TMPDIR`. Else the temp dir.
pub fn runtime_dir() -> PathBuf {
    if let Some(d) = dirs::runtime_dir() {
        return d;
    }
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::MetadataExt;
        if let Ok(m) = std::fs::metadata("/proc/self") {
            let d = PathBuf::from(format!("/run/user/{}", m.uid()));
            if d.is_dir() {
                return d;
            }
        }
    }
    #[cfg(target_os = "macos")]
    if let Some(d) = darwin_user_temp() {
        return d;
    }
    std::env::temp_dir()
}

/// `confstr(_CS_DARWIN_USER_TEMP_DIR)`: what launchd sets `$TMPDIR` to for
/// the desktop. An SSH login can lack `$TMPDIR`.
#[cfg(target_os = "macos")]
fn darwin_user_temp() -> Option<PathBuf> {
    let mut buf = vec![0u8; 1024];
    // SAFETY: confstr writes at most `buf.len()` bytes, NUL included.
    let n = unsafe { libc::confstr(libc::_CS_DARWIN_USER_TEMP_DIR, buf.as_mut_ptr().cast(), buf.len()) };
    if n == 0 || n > buf.len() {
        return None;
    }
    buf.truncate(n - 1);
    String::from_utf8(buf).ok().map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// A debug build resolves the same socket as a release build: there is
    /// one skiffd per computer. Only `SKIFF_SOCKET` names another one.
    #[test]
    fn every_build_resolves_skiffd_sock() {
        let run = Path::new("/run/user/1000");
        assert_eq!(socket_path_from(None, run), Path::new("/run/user/1000/skiff/skiffd.sock"));
        assert_eq!(socket_path_from(Some(std::ffi::OsStr::new("")), run), Path::new("/run/user/1000/skiff/skiffd.sock"));
        assert_eq!(socket_path_from(Some(std::ffi::OsStr::new("/tmp/t/x.sock")), run), Path::new("/tmp/t/x.sock"));
        assert!(socket_path().file_name().is_some_and(|n| n == SOCKET_NAME) || std::env::var_os("SKIFF_SOCKET").is_some());
        assert!(crate::state::is_live(&socket_path_from(None, &runtime_dir())));
    }
}
