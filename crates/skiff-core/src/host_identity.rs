//! The Host's durable id: an opaque random UUID in `local-host-id` beside
//! `projects.toml`. skiffd reads it and reports it in `pong`, so identity
//! belongs to the Host that serves it. The app reads it itself only for an
//! older skiffd that reports none. Never derived from a hostname, socket,
//! peer credentials or pid.

use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

use anyhow::{bail, Context};

/// Where this Host keeps its id, beside the selected `projects.toml`, so
/// private test and debug state keep their own.
pub fn path() -> PathBuf {
    crate::config::config_path().with_file_name("local-host-id")
}

/// The id in `path`, made on first use. Publishes complete bytes with a
/// no-clobber hard link: processes that start at once agree on one id, and
/// a crash cannot leave a partial one. A corrupt, public, linked or foreign
/// file is an error, never replaced.
pub fn load(path: &Path) -> anyhow::Result<String> {
    match read(path) {
        Ok(id) => return Ok(id),
        Err(e)
            if e.downcast_ref::<std::io::Error>()
                .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) => {}
        Err(e) => return Err(e),
    }
    let parent = path.parent().context("local Host identity has no parent")?;
    // A bare SKIFF_CONFIG filename yields an empty parent, which is valid
    // for path joins but not for opening the directory to sync publication.
    let parent = if parent.as_os_str().is_empty() { Path::new(".") } else { parent };
    fs::create_dir_all(parent)?;
    let id = uuid::Uuid::new_v4();
    let temp = parent.join(format!(".local-host-id-{id}.tmp"));
    let mut file = OpenOptions::new().write(true).create_new(true).mode(0o600).open(&temp)?;
    let result = (|| {
        writeln!(file, "{id}")?;
        file.sync_all()?;
        match fs::hard_link(&temp, path) {
            Ok(()) => fs::File::open(parent)?.sync_all()?,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e.into()),
        }
        read(path)
    })();
    let _ = fs::remove_file(temp);
    result
}

fn read(path: &Path) -> anyhow::Result<String> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .with_context(|| format!("read local Host identity {}", path.display()))?;
    let meta = file.metadata()?;
    // SAFETY: geteuid takes no arguments and cannot fail.
    if !meta.is_file() || meta.len() > 128 || meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o077 != 0 {
        bail!("local Host identity must be a private regular file owned by this account");
    }
    let mut text = String::new();
    file.take(128).read_to_string(&mut text)?;
    let id = uuid::Uuid::parse_str(text.trim()).context("invalid local Host identity; refusing to replace it")?;
    Ok(id.to_string())
}

/// This process's account, by number: the same on every path to the Host.
pub fn account() -> String {
    // SAFETY: as above.
    format!("uid:{}", unsafe { libc::geteuid() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_private_id_survives_and_a_bad_file_is_kept() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("skiff-host-id-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let file = dir.join("config/local-host-id");
        let ids: Vec<_> = std::thread::scope(|s| {
            let all: Vec<_> = (0..8).map(|_| s.spawn(|| load(&file).unwrap())).collect();
            all.into_iter().map(|t| t.join().unwrap()).collect()
        });
        assert!(ids.iter().all(|id| id == &ids[0]));
        assert_eq!(load(&file).unwrap(), ids[0]);
        assert_eq!(fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600);
        fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(load(&file).is_err());
        fs::write(&file, "invalid").unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(load(&file).is_err());
        assert_eq!(fs::read_to_string(&file).unwrap(), "invalid");
        fs::remove_dir_all(&dir).unwrap();
    }
}
