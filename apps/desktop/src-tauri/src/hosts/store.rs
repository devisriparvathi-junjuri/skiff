//! The hosts.toml store: locked updates, ids, pins and per-Host flags.

use super::*;

/// Changes the file as one step for every window and app run. An exclusive
/// `flock` on `hosts.toml.lock`, beside it, is held from the read to the
/// rename, so no change is lost to another, and a check such as "no pin yet"
/// still holds when the change is written. Nothing is written when `change`
/// fails or changes nothing. Every write of the file goes through here.
pub(super) fn update<T>(path: &Path, change: impl FnOnce(&mut Hosts) -> anyhow::Result<T>) -> anyhow::Result<T> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    let lock = std::fs::File::options().create(true).truncate(false).write(true).open(path.with_extension("toml.lock"))?;
    // Released when `lock` closes, or when the process ends.
    lock.lock()?;
    let before = load(path)?;
    let mut hosts = before.clone();
    let out = change(&mut hosts)?;
    if hosts != before {
        save(path, &hosts)?;
    }
    Ok(out)
}

/// A complete file or none: written beside, then renamed over. Only under
/// [`update`]'s lock.
pub(super) fn save(path: &Path, hosts: &Hosts) -> anyhow::Result<()> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    let temp = path.with_extension(format!("toml.{}.tmp", std::process::id()));
    std::fs::write(&temp, toml::to_string(hosts)?)?;
    std::fs::rename(&temp, path)?;
    Ok(())
}


/// What this app run knows on top of `hosts.toml`: the skiffd socket the last
/// probe found there.
pub(crate) trait Known {
    fn target(&self) -> SshTarget;
}

impl Known for KnownHost {
    /// The login, with the socket the last probe of this app run found
    /// there, or empty before one: the first connect probes.
    fn target(&self) -> SshTarget {
        self.target_with(memo(&self.id, |m| m.socket.clone()))
    }
}

/// The selection the file names. A selected Host that is not in the list
/// is this Mac: the file never points at an unknown endpoint.
pub(crate) fn selected(hosts: &Hosts, local: impl FnOnce() -> HostSelection) -> HostSelection {
    match hosts.selected.as_deref().and_then(|d| hosts.find(d)) {
        Some(h) => remote(h).unwrap_or_else(|_| local()),
        None => local(),
    }
}

pub(crate) fn remote(h: &KnownHost) -> Result<HostSelection, String> {
    h.selection(memo(&h.id, |m| m.socket.clone()))
}

pub(super) fn known<'a>(hosts: &'a mut Hosts, id: &str) -> anyhow::Result<&'a mut KnownHost> {
    hosts.hosts.iter_mut().find(|h| h.id == id).ok_or_else(|| anyhow::anyhow!("That Host is not in the list any more."))
}

/// The Host a selection's target logs in to, for a change made from a
/// live connection.
pub(super) fn known_login<'a>(hosts: &'a mut Hosts, t: &SshTarget) -> anyhow::Result<&'a mut KnownHost> {
    hosts.hosts.iter_mut().find(|h| h.logs_in_as(t)).ok_or_else(|| anyhow::anyhow!("{} is not a known Host", t.destination))
}

/// A new local id: `h_` and 8 hex digits, not in the list yet.
pub(super) fn new_id(hosts: &Hosts) -> anyhow::Result<String> {
    use std::io::Read;
    let mut f = std::fs::File::open("/dev/urandom")?;
    loop {
        let mut b = [0u8; 4];
        f.read_exact(&mut b)?;
        let id = format!("h_{:08x}", u32::from_be_bytes(b));
        if hosts.find(&id).is_none() {
            return Ok(id);
        }
    }
}

/// Stores the identity a Host reported on its first accepted connection: a
/// compare-and-set under [`update`]'s lock. The file decides: when another
/// window or run stored a different one first, this one is refused as a
/// changed identity. Ok only once the pin is on disk.
pub(crate) fn pin_first(file: &Path, target: &SshTarget, host_id: &HostId) -> anyhow::Result<()> {
    update(file, |hosts| {
        let h = known_login(hosts, target)?;
        match &h.host_id {
            Some(pin) if pin == host_id.as_str() => Ok(()),
            Some(pin) => Err(skiff_host_api::Error::IdentityChanged {
                expected: HostId::try_from(pin.clone()).map_err(anyhow::Error::msg)?,
                received: host_id.clone(),
            }
            .into()),
            None => {
                h.host_id = Some(host_id.as_str().to_string());
                h.clipboard_host = None;
                Ok(())
            }
        }
    })
}

/// Makes `host_id` the identity Host `id` must report, as the person
/// accepted it. The clipboard permission of the old identity goes.
pub(crate) fn accept_identity(file: &Path, id: &str, host_id: &HostId) -> anyhow::Result<Hosts> {
    update(file, |hosts| {
        let h = known(hosts, id)?;
        h.host_id = Some(host_id.as_str().to_string());
        h.clipboard_host = None;
        Ok(hosts.clone())
    })
}

/// Allows or stops OSC 52 writes from `destination` for `live`, the identity
/// the asking window's connection accepted. Allowing needs that identity to
/// be the one the file pins now: a window still connected to an identity
/// another window replaced cannot grant the new one, nor its own.
pub(super) fn set_clipboard(file: &Path, id: &str, live: Option<&HostId>, allow: bool) -> anyhow::Result<()> {
    update(file, |hosts| {
        let h = known(hosts, id)?;
        let destination = h.name.clone();
        h.clipboard_host = match live {
            _ if !allow => None,
            Some(id) if h.host_id.as_deref() == Some(id.as_str()) => Some(id.as_str().to_string()),
            Some(_) => anyhow::bail!("Skiff stored another identity for {destination} since this window connected. Pick the Host again, then allow clipboard writes."),
            None => anyhow::bail!("Skiff is not connected to {destination}. Connect once, then allow clipboard writes."),
        };
        Ok(())
    })
}

/// Clipboard writes from the shown Host are allowed for a connection that
/// answers as `live`, as the file says now.
pub(super) fn clipboard_allowed(file: &Path, target: &SshTarget, live: &HostId) -> anyhow::Result<bool> {
    Ok(load(file)?.find_login(target).is_some_and(|h| h.clipboard_for(live)))
}
