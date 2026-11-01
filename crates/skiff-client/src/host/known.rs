//! The known Hosts in `hosts.toml`, beside `projects.toml`, as the app and
//! the CLI read them. The app owns every write (see its `hosts.rs`); this is
//! the file's format and what a reader needs to reach a remote Host.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use skiff_host_api::HostId;

use super::{
    local::HostSelection,
    ssh::{SshOptions, SshTarget},
};

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct KnownHost {
    /// Local only. Rename and edit keep it; `selected` names it.
    pub id: String,
    /// Shown in the Host menu, Settings and the window title.
    pub name: String,
    /// What `ssh` takes: an alias from ssh config, `host` or `user@host`.
    pub destination: String,
    /// Sent as `-l` only when the person set it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    /// Sent as `-p` only when the person set it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// The Host id skiffd reported on the first accepted connection. Only
    /// the person changes it, by accepting a new identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_id: Option<String>,
    /// The Host id whose OSC 52 clipboard writes reach this Mac. It counts
    /// only while it is `host_id`: a new identity has no permission.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clipboard_host: Option<String>,
    /// Update Skiff on this server automatically when this app is newer.
    /// On by default; only an install Skiff made is updated.
    #[serde(default = "yes", skip_serializing_if = "is_yes")]
    pub auto_update: bool,
}

fn yes() -> bool {
    true
}

fn is_yes(b: &bool) -> bool {
    *b
}

impl KnownHost {
    /// A new entry for `destination`, named after it.
    pub fn new(destination: String) -> Self {
        Self { id: format!("h_{destination}"), name: destination.clone(), destination, user: None, port: None, host_id: None, clipboard_host: None, auto_update: true }
    }

    /// The login, with the skiffd socket on the Host when one is known.
    /// Empty: the first connect probes for it.
    pub fn target_with(&self, socket: Option<String>) -> SshTarget {
        SshTarget { destination: self.destination.clone(), user: self.user.clone(), port: self.port, socket: socket.unwrap_or_default() }
    }

    /// The same login: destination, user and port.
    pub fn logs_in_as(&self, t: &SshTarget) -> bool {
        self.destination == t.destination && self.user == t.user && self.port == t.port
    }

    /// Clipboard writes from a connection that answers as `live` are
    /// allowed: it is the identity this Host must have, and the one the
    /// person allowed. Another window's newer pin or grant does not count.
    pub fn clipboard_for(&self, live: &HostId) -> bool {
        self.host_id.as_deref() == Some(live.as_str()) && self.clipboard_host.as_deref() == Some(live.as_str())
    }

    /// This Host through `ssh`, pinned to the identity it first reported.
    pub fn selection(&self, socket: Option<String>) -> Result<HostSelection, String> {
        let pin = self.host_id.clone().map(HostId::try_from).transpose()?;
        HostSelection::ssh(self.target_with(socket), SshOptions::default(), forward_dir(&self.id), pin)
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq)]
pub struct Hosts {
    /// The id of the Host this window shows. None: this Mac.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected: Option<String>,
    #[serde(default, rename = "host")]
    pub hosts: Vec<KnownHost>,
}

impl Hosts {
    pub fn find(&self, id: &str) -> Option<&KnownHost> {
        self.hosts.iter().find(|h| h.id == id)
    }

    /// The Host a selection's target logs in to.
    pub fn find_login(&self, t: &SshTarget) -> Option<&KnownHost> {
        self.hosts.iter().find(|h| h.logs_in_as(t))
    }

    /// The Host the person names: its shown name, its id or its destination.
    pub fn named(&self, name: &str) -> Option<&KnownHost> {
        self.hosts
            .iter()
            .find(|h| h.name == name)
            .or_else(|| self.find(name))
            .or_else(|| self.hosts.iter().find(|h| h.destination == name))
    }
}

pub fn path() -> PathBuf {
    skiff_core::config::config_path().with_file_name("hosts.toml")
}

pub fn load(path: &Path) -> anyhow::Result<Hosts> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(toml::from_str(&text)?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Hosts::default()),
        Err(e) => Err(e.into()),
    }
}

/// The private directory for this Host's forwards. Each forward makes its
/// own directory inside it. Short, as a socket path must be.
pub fn forward_dir(id: &str) -> PathBuf {
    let hash = id.bytes().fold(0xcbf29ce484222325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100000001b3));
    // SAFETY: geteuid takes no arguments and cannot fail.
    std::env::temp_dir().join(format!("skiff-{}-{:08x}", unsafe { libc::geteuid() }, hash as u32))
}
