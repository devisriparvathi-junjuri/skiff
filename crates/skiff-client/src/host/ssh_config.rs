//! What this machine's ssh config says, read without the network: the
//! aliases in `~/.ssh/config` (and the files it includes), and what `ssh -G`
//! resolves for one target. Settings › Hosts lists the first and shows the
//! second under the Server field.

use std::path::{Path, PathBuf};

use serde::Serialize;

use super::ssh::{SshOptions, SshTarget};

/// One `Host` name from ssh config, with what its own block says, if anything.
#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct Alias {
    pub name: String,
    /// `user@hostname` as the block writes it, or empty.
    pub goes_to: String,
}

/// What `ssh -G` resolves for a target.
#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub hostname: String,
    pub user: String,
    pub port: u16,
    /// The first jump host, or empty.
    pub jump: String,
}

/// The aliases in `<home>/.ssh/config` and the files it includes, in file
/// order, once each. Patterns (`*`, `?`, `!`) are not aliases.
pub fn aliases(home: &Path) -> Vec<Alias> {
    let ssh = home.join(".ssh");
    let mut out: Vec<Alias> = Vec::new();
    let mut seen = Vec::new();
    read_config(&ssh.join("config"), &ssh, &mut out, &mut seen, 0);
    out
}

fn read_config(file: &Path, ssh: &Path, out: &mut Vec<Alias>, seen: &mut Vec<PathBuf>, depth: u32) {
    if depth > 8 || seen.iter().any(|f| f == file) {
        return;
    }
    seen.push(file.to_path_buf());
    let Ok(text) = std::fs::read_to_string(file) else { return };
    // The aliases of the open `Host` block, by index into `out`.
    let mut open: Vec<usize> = Vec::new();
    let (mut user, mut hostname) = (String::new(), String::new());
    let close = |out: &mut Vec<Alias>, open: &mut Vec<usize>, user: &mut String, hostname: &mut String| {
        if !hostname.is_empty() {
            let goes = if user.is_empty() { hostname.clone() } else { format!("{user}@{hostname}") };
            for &i in open.iter() {
                if out[i].goes_to.is_empty() {
                    out[i].goes_to = goes.clone();
                }
            }
        }
        open.clear();
        user.clear();
        hostname.clear();
    };
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, value) = match line.split_once(|c: char| c.is_whitespace() || c == '=') {
            Some((k, v)) => (k.to_ascii_lowercase(), v.trim_start_matches(|c: char| c.is_whitespace() || c == '=').trim()),
            None => continue,
        };
        match key.as_str() {
            "host" => {
                close(out, &mut open, &mut user, &mut hostname);
                for name in value.split_whitespace().map(|n| n.trim_matches('"')) {
                    if name.contains(['*', '?', '!']) || name.is_empty() {
                        continue;
                    }
                    match out.iter().position(|a| a.name == name) {
                        Some(i) => open.push(i),
                        None => {
                            out.push(Alias { name: name.to_string(), goes_to: String::new() });
                            open.push(out.len() - 1);
                        }
                    }
                }
            }
            "match" => close(out, &mut open, &mut user, &mut hostname),
            "hostname" if !open.is_empty() && hostname.is_empty() => hostname = value.to_string(),
            "user" if !open.is_empty() && user.is_empty() => user = value.to_string(),
            "include" => {
                for pattern in value.split_whitespace() {
                    for f in include_files(pattern, ssh) {
                        read_config(&f, ssh, out, seen, depth + 1);
                    }
                }
            }
            _ => {}
        }
    }
    close(out, &mut open, &mut user, &mut hostname);
}

/// The files an `Include` names: relative to `~/.ssh`, `~/` expanded, and
/// `*` or `?` in the last part only.
fn include_files(pattern: &str, ssh: &Path) -> Vec<PathBuf> {
    let path = match pattern.strip_prefix("~/") {
        Some(rest) => ssh.parent().unwrap_or(ssh).join(rest),
        None if pattern.starts_with('/') => PathBuf::from(pattern),
        None => ssh.join(pattern),
    };
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    if !name.contains(['*', '?']) {
        return vec![path];
    }
    let Some(dir) = path.parent() else { return Vec::new() };
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|r| r.flatten().map(|e| e.path()).filter(|p| p.file_name().is_some_and(|n| glob(&name, &n.to_string_lossy()))).collect())
        .unwrap_or_default();
    files.sort();
    files
}

/// `*` and `?` only, as ssh config uses them.
fn glob(pattern: &str, name: &str) -> bool {
    fn at(p: &[char], n: &[char]) -> bool {
        match p.split_first() {
            None => n.is_empty(),
            Some(('*', rest)) => (0..=n.len()).any(|i| at(rest, &n[i..])),
            Some(('?', rest)) => !n.is_empty() && at(rest, &n[1..]),
            Some((c, rest)) => n.first() == Some(c) && at(rest, &n[1..]),
        }
    }
    at(&pattern.chars().collect::<Vec<_>>(), &name.chars().collect::<Vec<_>>())
}

/// What `ssh -G` resolves for `target`, as Settings shows it: local only,
/// no network. The error is plain words, never ssh's own output.
pub fn resolve(target: &SshTarget, options: &SshOptions) -> Result<Resolved, String> {
    resolve_full(target, options).map(|f| f.shown())
}

/// The lines of `ssh -G` that Settings and the key steps need.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Full {
    pub hostname: String,
    pub user: String,
    pub port: u16,
    pub host_key_alias: String,
    /// `ProxyJump` as written, or empty (`none` reads as empty).
    pub proxy_jump: String,
    /// `UserKnownHostsFile`, in order, `~` not expanded.
    pub known_hosts: Vec<String>,
    /// `IdentityFile`, in order, `~` not expanded.
    pub identity_files: Vec<String>,
}

impl Full {
    /// The host's name in known_hosts: the alias, `host`, or `[host]:port`.
    pub fn key_name(&self) -> String {
        if !self.host_key_alias.is_empty() {
            self.host_key_alias.clone()
        } else if self.port == 22 {
            self.hostname.clone()
        } else {
            format!("[{}]:{}", self.hostname, self.port)
        }
    }

    /// The lines Settings shows, with the first jump host's name alone.
    pub fn shown(&self) -> Resolved {
        let first = self.proxy_jump.split(',').next().unwrap_or_default();
        let host = first.rsplit('@').next().unwrap_or(first);
        let jump = host.split(':').next().unwrap_or(host).to_string();
        Resolved { hostname: self.hostname.clone(), user: self.user.clone(), port: self.port, jump }
    }
}

/// `ssh -G` for `target`: local only, no network. The error is plain
/// words, never ssh's own output.
pub fn resolve_full(target: &SshTarget, options: &SshOptions) -> Result<Full, String> {
    target.validate_login()?;
    let mut cmd = std::process::Command::new(&options.program);
    if let Some(config) = &options.config {
        cmd.arg("-F").arg(config);
    }
    let out = cmd
        .arg("-G")
        .args(target.login_args())
        .arg("--")
        .arg(&target.destination)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("Skiff cannot run ssh: {e}"))?;
    if !out.status.success() {
        return Err("ssh cannot read its config for this server.".into());
    }
    Ok(parse_full(&String::from_utf8_lossy(&out.stdout)))
}

pub fn parse_full(text: &str) -> Full {
    let mut f = Full { port: 22, ..Default::default() };
    for line in text.lines() {
        let Some((key, value)) = line.split_once(' ') else { continue };
        match key {
            "hostname" => f.hostname = value.to_string(),
            "user" => f.user = value.to_string(),
            "port" => f.port = value.parse().unwrap_or(22),
            "hostkeyalias" => f.host_key_alias = value.to_string(),
            "proxyjump" if value != "none" => f.proxy_jump = value.to_string(),
            "userknownhostsfile" => f.known_hosts = value.split_whitespace().map(String::from).collect(),
            "identityfile" => f.identity_files.push(value.to_string()),
            _ => {}
        }
    }
    f
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aliases_come_from_config_and_includes_and_skip_patterns() {
        let home = std::env::temp_dir().join(format!("skiff-sshcfg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(home.join(".ssh/conf.d")).unwrap();
        std::fs::write(
            home.join(".ssh/config"),
            "Include conf.d/*.conf\n\nHost devbox build-01\n  HostName devbox.lan\n  User jose\n\nHost *.internal !bad\n  User x\n\nHost pi\n  Hostname=10.0.0.9\nMatch all\n  User y\n",
        )
        .unwrap();
        std::fs::write(home.join(".ssh/conf.d/work.conf"), "Host staging\n  HostName staging.example.net\n").unwrap();
        let got = aliases(&home);
        let names: Vec<_> = got.iter().map(|a| (a.name.as_str(), a.goes_to.as_str())).collect();
        assert_eq!(
            names,
            [("staging", "staging.example.net"), ("devbox", "jose@devbox.lan"), ("build-01", "jose@devbox.lan"), ("pi", "10.0.0.9")]
        );
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn ssh_g_gives_the_resolve_line() {
        let r = parse_full("user jose\nhostname 10.0.0.5\nport 2222\nproxyjump me@bastion:22,other\n").shown();
        assert_eq!(r, Resolved { hostname: "10.0.0.5".into(), user: "jose".into(), port: 2222, jump: "bastion".into() });
        assert_eq!(parse_full("hostname x\nproxyjump none\n").shown().jump, "");
        let f = parse_full("hostname 10.0.0.5\nport 2222\nuserknownhostsfile ~/.ssh/known_hosts ~/.ssh/known_hosts2\nidentityfile ~/.ssh/id_rsa\nidentityfile ~/.ssh/id_ed25519\n");
        assert_eq!(f.key_name(), "[10.0.0.5]:2222");
        assert_eq!(f.known_hosts, ["~/.ssh/known_hosts", "~/.ssh/known_hosts2"]);
        assert_eq!(f.identity_files, ["~/.ssh/id_rsa", "~/.ssh/id_ed25519"]);
        assert_eq!(parse_full("hostname h\nport 22\nhostkeyalias box\n").key_name(), "box");
    }
}
