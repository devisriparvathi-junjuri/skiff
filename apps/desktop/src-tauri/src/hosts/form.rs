//! Host form parsing and uniqueness.

use super::*;

/// What the Add and Edit form sends. Empty name, user or port: the name
/// comes from the destination, user and port from ssh config. The probe
/// finds the socket.
#[derive(Deserialize, Debug, Clone)]
pub(crate) struct HostForm {
    #[serde(default)]
    pub(super) name: String,
    pub(super) destination: String,
    #[serde(default)]
    pub(super) user: String,
    #[serde(default)]
    pub(super) port: String,
}

impl HostForm {
    /// The checked values: name, target. The target has no socket.
    /// Copy follows Settings › Hosts.
    pub(super) fn parse(&self) -> anyhow::Result<(String, SshTarget)> {
        let destination = self.destination.trim().to_string();
        anyhow::ensure!(!destination.is_empty(), "Pick a server or type its address.");
        let user = Some(self.user.trim().to_string()).filter(|u| !u.is_empty());
        let port = match self.port.trim() {
            "" => None,
            p => Some(p.parse::<u16>().ok().filter(|&p| p > 0).ok_or_else(|| anyhow::anyhow!("Type a port from 1 to 65535."))?),
        };
        let target = SshTarget { destination, user, port, socket: String::new() };
        target.validate_login().map_err(anyhow::Error::msg)?;
        let name = match self.name.trim() {
            "" => default_name(&target.destination),
            n => n.to_string(),
        };
        anyhow::ensure!(!name.chars().any(|c| c.is_control()), "Type a name on one line.");
        Ok((name, target))
    }
}

/// The alias, or the host part of `user@host`.
fn default_name(destination: &str) -> String {
    destination.rsplit('@').next().unwrap_or(destination).to_string()
}

/// Refuses a name or a login that another Host has. `except`: the Host
/// being edited.
pub(super) fn check_unique(hosts: &Hosts, name: &str, target: &SshTarget, except: Option<&str>) -> anyhow::Result<()> {
    let others = || hosts.hosts.iter().filter(|h| Some(h.id.as_str()) != except);
    if let Some(h) = others().find(|h| h.logs_in_as(target)) {
        anyhow::bail!("{} is in the list already.", h.name);
    }
    anyhow::ensure!(!others().any(|h| h.name.eq_ignore_ascii_case(name)), "Another Host has this name.");
    Ok(())
}

