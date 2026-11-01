//! The fixes of the check list's first rows: trust a host key, make, unlock
//! and install this computer's SSH key, and fix the key file on a server.
//! Each runs only after the person confirms it in Settings › Hosts.
//!
//! - The session link never changes: it keeps `BatchMode=yes` and
//!   `StrictHostKeyChecking=yes`. Each step here is its own short ssh run.
//! - A new host key is learned into a private temp file
//!   (`StrictHostKeyChecking=accept-new` there only). Nothing reaches the
//!   person's known_hosts before they confirm the fingerprint.
//! - A password or passphrase reaches ssh or ssh-add through [`Askpass`]
//!   only: a one-shot FIFO in a private folder. It is never written to a
//!   file, an argument, the environment or a log.
//! - What runs on the server is the fixed server script
//!   ([`install::SERVER`], modes `install-key` and `fix-key`). The one
//!   value it takes, the public key line, is checked against a strict
//!   pattern first and single-quoted.

use std::{
    ffi::OsString,
    io::Write,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use serde::Serialize;
use tokio::{io::AsyncWriteExt, process::Command};

use super::{
    install::SERVER,
    ssh::{quote, temp_dir, SshOptions, SshTarget},
    ssh_config,
};

/// How long an interactive step may take, login included.
const STEP_TIMEOUT: Duration = Duration::from_secs(30);

/// A secret for one ssh or ssh-add run. ssh runs the helper script, which
/// reads the secret once from a FIFO; the secret lives only in this
/// process's memory and the pipe. Dropping it removes the folder.
pub struct Askpass {
    dir: PathBuf,
    script: PathBuf,
    done: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl Askpass {
    pub fn new(secret: String) -> std::io::Result<Self> {
        let dir = temp_dir("askpass")?;
        let fifo = dir.join("secret");
        let c = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).map_err(std::io::Error::other)?;
        // SAFETY: a valid C path; mkfifo makes one node in our private folder.
        if unsafe { libc::mkfifo(c.as_ptr(), 0o600) } != 0 {
            let e = std::io::Error::last_os_error();
            let _ = std::fs::remove_dir_all(&dir);
            return Err(e);
        }
        let script = dir.join("askpass");
        let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o700).open(&script)?;
        // The path is our own temp folder: no quote in it.
        writeln!(f, "#!/bin/sh\nexec cat '{}'", fifo.display())?;
        drop(f);
        // The first reader gets the secret. A second prompt, such as a
        // wrong password, gets an empty answer at once, so ssh stops.
        let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop = done.clone();
        std::thread::spawn(move || {
            let mut secret = secret.into_bytes();
            let mut first = true;
            while let Ok(mut pipe) = std::fs::OpenOptions::new().write(true).open(&fifo) {
                if stop.load(std::sync::atomic::Ordering::SeqCst) {
                    break;
                }
                if first {
                    let _ = pipe.write_all(&secret);
                    let _ = pipe.write_all(b"\n");
                    secret.fill(0);
                    first = false;
                }
            }
            secret.fill(0);
        });
        Ok(Self { dir, script, done })
    }

    /// ssh and ssh-add ask the helper, never a terminal (OpenSSH 8.4+).
    fn apply(&self, cmd: &mut Command) {
        cmd.env("SSH_ASKPASS", &self.script).env("SSH_ASKPASS_REQUIRE", "force").env("DISPLAY", std::env::var_os("DISPLAY").unwrap_or_else(|| ":0".into()));
    }
}

impl Drop for Askpass {
    fn drop(&mut self) {
        // The writer waits for a reader: open the read end, so it ends.
        self.done.store(true, std::sync::atomic::Ordering::SeqCst);
        let fifo = self.dir.join("secret");
        let _ = std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NONBLOCK).open(&fifo);
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// One host key, as `ssh-keygen -l` names it.
#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct Fingerprint {
    /// `ED25519`, `RSA`, `ECDSA`.
    pub kind: String,
    /// `SHA256:…`
    pub sha256: String,
}

/// A host key that answers now, learned into a temp file only.
#[derive(Debug, Clone)]
pub struct Scanned {
    /// The exact lines ssh wrote, with its host name, port, alias and hashing.
    pub lines: Vec<String>,
    pub fingerprints: Vec<Fingerprint>,
    /// The keys known_hosts holds now for this host, if any.
    pub known: Vec<Fingerprint>,
    /// The known_hosts file a trust writes to: the first one ssh config names.
    pub file: PathBuf,
    /// The host key name in known_hosts: `host`, `[host]:port` or the alias.
    pub name: String,
}

fn ssh_cmd(options: &SshOptions) -> Command {
    let mut cmd = Command::new(&options.program);
    if let Some(config) = &options.config {
        cmd.arg("-F").arg(config);
    }
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    cmd
}

/// `~/x` against `home`.
fn expand(path: &str, home: &Path) -> PathBuf {
    match path.strip_prefix("~/") {
        Some(rest) => home.join(rest),
        None => PathBuf::from(path),
    }
}

/// Learns the host key that answers for `target` into a private temp file,
/// and reads what the person's known_hosts holds for that host now. It
/// writes nothing the person owns.
pub async fn scan_host_key(target: &SshTarget, options: &SshOptions, home: &Path) -> Result<Scanned, String> {
    let g = ssh_config::resolve_full(target, options)?;
    let tmp = temp_dir("scan").map_err(|e| e.to_string())?;
    let file = tmp.join("known_hosts");
    let mut cmd = ssh_cmd(options);
    cmd.args(["-o", "BatchMode=yes", "-o", "StrictHostKeyChecking=accept-new", "-o", "UpdateHostKeys=no", "-o", "GlobalKnownHostsFile=/dev/null"])
        .arg("-o")
        .arg(format!("UserKnownHostsFile={}", file.display()))
        .args(["-o", "PreferredAuthentications=none", "-o", "ConnectTimeout=15", "-o", "ControlMaster=no", "-o", "ControlPath=none", "-o", "ForwardAgent=no", "-o", "RequestTTY=no", "-T"])
        .args(target.login_args())
        .arg("--")
        .arg(&target.destination)
        .arg("true");
    // Login fails here on purpose: the key line is all this needs.
    let _ = tokio::time::timeout(STEP_TIMEOUT, cmd.output()).await;
    let text = std::fs::read_to_string(&file).unwrap_or_default();
    let lines: Vec<String> = text.lines().filter(|l| !l.trim().is_empty()).map(String::from).collect();
    let fingerprints = fingerprints_of(&file).await;
    let _ = std::fs::remove_dir_all(&tmp);
    if lines.is_empty() || fingerprints.is_empty() {
        return Err(format!("{} sent no host key. Check again.", target.destination));
    }
    let known_file = g.known_hosts.first().map(|f| expand(f, home)).unwrap_or_else(|| home.join(".ssh/known_hosts"));
    let name = g.key_name();
    let known = known_fingerprints(&g.known_hosts.iter().map(|f| expand(f, home)).collect::<Vec<_>>(), &name).await;
    Ok(Scanned { lines, fingerprints, known, file: known_file, name })
}

/// Fingerprints of each key in a known_hosts file.
async fn fingerprints_of(file: &Path) -> Vec<Fingerprint> {
    let Ok(out) = Command::new("ssh-keygen").arg("-l").arg("-f").arg(file).stdin(Stdio::null()).output().await else { return Vec::new() };
    parse_fingerprints(&String::from_utf8_lossy(&out.stdout))
}

/// What the known_hosts files hold for `name` now.
async fn known_fingerprints(files: &[PathBuf], name: &str) -> Vec<Fingerprint> {
    let mut out = Vec::new();
    for f in files.iter().filter(|f| f.is_file()) {
        if let Ok(o) = Command::new("ssh-keygen").args(["-l", "-F", name, "-f"]).arg(f).stdin(Stdio::null()).output().await {
            out.extend(parse_fingerprints(&String::from_utf8_lossy(&o.stdout)));
        }
    }
    out
}

/// `256 SHA256:abc host (ED25519)` lines; `# Host … found` lines skipped.
pub fn parse_fingerprints(text: &str) -> Vec<Fingerprint> {
    text.lines()
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| {
            let words: Vec<&str> = l.split_whitespace().collect();
            let at = words.iter().position(|w| w.starts_with("SHA256:"))?;
            // `-l -f`: `bits SHA256:… host (TYPE)`. `-l -F`: `host TYPE SHA256:…`.
            let kind = match l.rsplit_once('(').and_then(|(_, k)| k.trim().strip_suffix(')')) {
                Some(k) => k.to_string(),
                None => words.get(at.checked_sub(1)?)?.to_string(),
            };
            Some(Fingerprint { kind, sha256: words[at].to_string() })
        })
        .collect()
}

/// Adds the scanned key to the person's known_hosts, after they confirmed
/// its fingerprint. `replace`: the old keys for this host go first, as the
/// person confirmed twice. A line that is there already is not added again.
pub async fn trust_host_key(s: &Scanned, replace: bool) -> Result<(), String> {
    if let Some(dir) = s.file.parent() {
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir).map_err(|e| format!("Skiff cannot make {}: {e}", dir.display()))?;
    }
    if replace && s.file.is_file() {
        let out = Command::new("ssh-keygen").args(["-R", &s.name, "-f"]).arg(&s.file).stdin(Stdio::null()).output().await.map_err(|e| e.to_string())?;
        if !out.status.success() {
            return Err("Skiff could not remove the old key. Nothing changed.".into());
        }
    }
    let have = std::fs::read_to_string(&s.file).unwrap_or_default();
    let mut f = std::fs::OpenOptions::new().create(true).append(true).mode(0o600).open(&s.file).map_err(|e| format!("Skiff cannot write {}: {e}", s.file.display()))?;
    let mut text = String::new();
    if !have.is_empty() && !have.ends_with('\n') {
        text.push('\n');
    }
    for line in s.lines.iter().filter(|l| !have.lines().any(|h| h == l.as_str())) {
        text.push_str(line);
        text.push('\n');
    }
    f.write_all(text.as_bytes()).map_err(|e| e.to_string())
}

/// This computer's keys, as ssh would use them for a target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalKey {
    /// No identity file exists, and the agent holds no key.
    None,
    /// A key file that needs its passphrase, and the agent does not hold it.
    Locked(PathBuf),
    /// A key ssh can use: the public line to install.
    Ready(String),
}

/// Which key ssh would offer for `target`, from `ssh -G` and, with
/// `agent`, the SSH agent.
pub async fn local_key(target: &SshTarget, options: &SshOptions, home: &Path, agent: bool) -> Result<LocalKey, String> {
    let g = ssh_config::resolve_full(target, options)?;
    let agent = if agent { agent_keys().await } else { Vec::new() };
    let files: Vec<PathBuf> = g.identity_files.iter().map(|f| expand(f, home)).filter(|f| f.is_file()).collect();
    let mut locked = None;
    for f in &files {
        let public = std::fs::read_to_string(f.with_extension(ext_pub(f))).ok().and_then(|t| public_line(&t));
        let held = public.as_ref().is_some_and(|p| agent.iter().any(|a| same_key(a, p)));
        let open = Command::new("ssh-keygen").args(["-y", "-P", "", "-f"]).arg(f).stdin(Stdio::null()).stderr(Stdio::null()).output().await.ok();
        let derived = open.filter(|o| o.status.success()).and_then(|o| public_line(&String::from_utf8_lossy(&o.stdout)));
        match (derived, held) {
            (Some(line), _) => return Ok(LocalKey::Ready(public.unwrap_or(line))),
            (None, true) => return Ok(LocalKey::Ready(public.unwrap())),
            (None, false) if locked.is_none() => locked = Some(f.clone()),
            _ => {}
        }
    }
    if let Some(a) = agent.first() {
        return Ok(LocalKey::Ready(a.clone()));
    }
    Ok(locked.map(LocalKey::Locked).unwrap_or(LocalKey::None))
}

/// `id_ed25519` → `id_ed25519.pub`, keeping any extension the name has.
fn ext_pub(f: &Path) -> String {
    match f.extension() {
        Some(e) => format!("{}.pub", e.to_string_lossy()),
        None => "pub".into(),
    }
}

fn same_key(a: &str, b: &str) -> bool {
    a.split_whitespace().take(2).eq(b.split_whitespace().take(2))
}

/// The public lines the agent holds. Empty without an agent.
async fn agent_keys() -> Vec<String> {
    let Ok(out) = Command::new("ssh-add").arg("-L").stdin(Stdio::null()).stderr(Stdio::null()).output().await else { return Vec::new() };
    if !out.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&out.stdout).lines().filter_map(public_line).collect()
}

/// One public key line Skiff accepts to install: a known type, base64, and
/// a comment of plain characters. Anything else is refused, never escaped.
pub fn public_line(text: &str) -> Option<String> {
    let line = text.lines().next()?.trim();
    let mut parts = line.splitn(3, ' ');
    let kind = parts.next()?;
    let body = parts.next()?;
    let comment = parts.next().unwrap_or("").trim();
    let known = ["ssh-ed25519", "ssh-rsa", "ecdsa-sha2-nistp256", "ecdsa-sha2-nistp384", "ecdsa-sha2-nistp521", "sk-ssh-ed25519@openssh.com", "sk-ecdsa-sha2-nistp256@openssh.com"];
    let base64 = !body.is_empty() && body.bytes().all(|b| b.is_ascii_alphanumeric() || b"+/=".contains(&b));
    let plain = comment.bytes().all(|b| b.is_ascii_alphanumeric() || b" @._()-+:".contains(&b));
    (known.contains(&kind) && base64 && plain && comment.len() <= 200).then(|| if comment.is_empty() { format!("{kind} {body}") } else { format!("{kind} {body} {comment}") })
}

/// Makes `<home>/.ssh/id_ed25519` with no passphrase, only when it is not
/// there. Answers its public line.
pub async fn make_key(home: &Path, comment: &str) -> Result<String, String> {
    let ssh = home.join(".ssh");
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&ssh).map_err(|e| e.to_string())?;
    let key = ssh.join("id_ed25519");
    if key.exists() {
        return Err("This computer has a key at ~/.ssh/id_ed25519 already. Skiff did not change it.".into());
    }
    let comment: String = comment.chars().filter(|c| c.is_ascii_alphanumeric() || " @._()-".contains(*c)).collect();
    let out = Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-C", &comment, "-f"])
        .arg(&key)
        .stdin(Stdio::null())
        .output()
        .await
        .map_err(|e| format!("Skiff cannot run ssh-keygen: {e}"))?;
    if !out.status.success() {
        return Err("ssh-keygen could not make the key.".into());
    }
    std::fs::read_to_string(key.with_extension("pub")).ok().and_then(|t| public_line(&t)).ok_or_else(|| "The new key has no public line.".into())
}

/// Gives a locked key to the SSH agent with its passphrase, until logout.
/// macOS keeps it in the Keychain too.
pub async fn unlock_key(key: &Path, passphrase: String) -> Result<(), String> {
    if std::env::var_os("SSH_AUTH_SOCK").is_none() {
        return Err("No SSH agent runs for this session. Skiff cannot keep the key unlocked.".into());
    }
    let askpass = Askpass::new(passphrase).map_err(|e| e.to_string())?;
    let mut cmd = Command::new("ssh-add");
    if cfg!(target_os = "macos") {
        cmd.arg("--apple-use-keychain");
    }
    cmd.arg(key).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).kill_on_drop(true);
    askpass.apply(&mut cmd);
    let ok = tokio::time::timeout(STEP_TIMEOUT, cmd.status()).await.map_err(|_| "ssh-add did not finish.".to_string())?.map_err(|e| e.to_string())?.success();
    if ok { Ok(()) } else { Err("The passphrase did not work. The key stays locked.".into()) }
}

/// What the key install found on the server.
#[derive(Serialize, Debug, Clone, PartialEq, Eq, Default)]
pub struct Installed {
    /// The line was added. False: it was there already.
    pub added: bool,
    /// `~/.ssh` belongs to the account.
    pub ssh_dir_own: bool,
    /// `authorized_keys` belongs to the account.
    pub file_own: bool,
}

/// Why an interactive step failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepError {
    /// The server refused the password or passphrase. Nothing changed.
    Refused,
    Other(String),
}

impl std::fmt::Display for StepError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused => write!(f, "refused"),
            Self::Other(s) => write!(f, "{s}"),
        }
    }
}

/// How the one interactive login answers. The product uses a password;
/// the loopback test, which cannot check passwords, a locked key.
pub const BY_PASSWORD: &str = "keyboard-interactive,password";

/// The args of one interactive run of the server script's `mode`: the host
/// key must be known already, one prompt, no agent, no shared master, no TTY.
fn interactive_args(target: &SshTarget, prefer: &str, mode: &str, key: &str) -> Vec<OsString> {
    let mut args: Vec<OsString> = Vec::new();
    for value in [
        "BatchMode=no",
        "StrictHostKeyChecking=yes",
        "UpdateHostKeys=no",
        &format!("PreferredAuthentications={prefer}"),
        "NumberOfPasswordPrompts=1",
        "ConnectTimeout=15",
        "ControlMaster=no",
        "ControlPath=none",
        "ForwardAgent=no",
        "ForwardX11=no",
        "PermitLocalCommand=no",
        "RequestTTY=no",
    ] {
        args.push("-o".into());
        args.push(value.into());
    }
    args.push("-T".into());
    args.extend(target.login_args());
    args.push("--".into());
    args.push(target.destination.clone().into());
    args.push(format!("sh -s -- {mode} {}", quote(key)).into());
    args
}

/// Runs the server script's `mode` with one secret, through askpass.
async fn run_interactive(target: &SshTarget, options: &SshOptions, prefer: &str, mode: &str, key: &str, secret: String) -> Result<String, StepError> {
    let key = public_line(key).ok_or_else(|| StepError::Other("This computer's public key has a form Skiff does not send.".into()))?;
    let askpass = Askpass::new(secret).map_err(|e| StepError::Other(e.to_string()))?;
    let mut cmd = ssh_cmd(options);
    cmd.args(interactive_args(target, prefer, mode, &key)).stdin(Stdio::piped());
    askpass.apply(&mut cmd);
    // No terminal: ssh must ask the helper.
    // SAFETY: setsid in the child before exec only detaches it from our TTY.
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let mut child = cmd.spawn().map_err(|e| StepError::Other(format!("Skiff cannot run ssh: {e}")))?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(SERVER.as_bytes()).await;
    }
    let out = tokio::time::timeout(STEP_TIMEOUT, child.wait_with_output()).await.map_err(|_| StepError::Other("The server did not answer in time. Nothing changed.".into()))?.map_err(|e| StepError::Other(e.to_string()))?;
    let stderr = String::from_utf8_lossy(&out.stderr);
    if out.status.code() == Some(255) {
        if stderr.contains("Permission denied") || stderr.contains("Too many authentication failures") {
            return Err(StepError::Refused);
        }
        return Err(StepError::Other("ssh stopped before it logged in. Nothing changed.".into()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Adds `key` to the server's `~/.ssh/authorized_keys`, once, after one
/// password entry. `prefer`: [`BY_PASSWORD`] outside tests.
pub async fn install_key(target: &SshTarget, options: &SshOptions, prefer: &str, key: &str, secret: String) -> Result<Installed, StepError> {
    let out = run_interactive(target, options, prefer, "install-key", key, secret).await?;
    let has = |k: &str| out.lines().any(|l| l.trim() == k);
    if !has("skiff-install-key=done") {
        return Err(StepError::Other("The server did not finish the key install. Check again.".into()));
    }
    Ok(Installed { added: has("added=yes"), ssh_dir_own: has("ssh_dir_own=yes"), file_own: has("file_own=yes") })
}

/// Puts a foreign-owned `authorized_keys` aside, as `authorized_keys.skiff-<time>`,
/// and writes a new one with `key`. The other keys in it stop working there.
pub async fn fix_key_file(target: &SshTarget, options: &SshOptions, prefer: &str, key: &str, secret: String) -> Result<(), StepError> {
    let out = run_interactive(target, options, prefer, "fix-key", key, secret).await?;
    if out.lines().any(|l| l.trim() == "skiff-fix-key=done") {
        Ok(())
    } else if out.lines().any(|l| l.trim() == "skiff-fix-key=not-own") {
        Err(StepError::Other("You do not own your ~/.ssh folder there. Skiff did not change it.".into()))
    } else {
        Err(StepError::Other("The server did not finish. Check again.".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_plain_public_key_line_is_sent() {
        let k = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIB1x jose@thr0w-deb (Skiff)";
        assert_eq!(public_line(k).as_deref(), Some(k));
        assert_eq!(public_line("ssh-ed25519 AAAA\n").as_deref(), Some("ssh-ed25519 AAAA"));
        for bad in ["ssh-ed25519 AAAA'; rm -rf ~", "ssh-ed25519 AA$(x)", "command=\"x\" ssh-ed25519 AAAA", "ssh-dss AAAA", "ssh-ed25519 AAAA a'b", ""] {
            assert_eq!(public_line(bad), None, "{bad}");
        }
    }

    #[test]
    fn fingerprints_parse_from_ssh_keygen() {
        let got = parse_fingerprints("# Host x found: line 1\n256 SHA256:LhhfmtmogMSY/x |1|abc= (ED25519)\n3072 SHA256:zz [h]:22 (RSA)\n");
        assert_eq!(got, [Fingerprint { kind: "ED25519".into(), sha256: "SHA256:LhhfmtmogMSY/x".into() }, Fingerprint { kind: "RSA".into(), sha256: "SHA256:zz".into() }]);
        let found = parse_fingerprints("# Host [127.0.0.1]:2222 found: line 1 \n[127.0.0.1]:2222 ED25519 SHA256:zvXx\n");
        assert_eq!(found, [Fingerprint { kind: "ED25519".into(), sha256: "SHA256:zvXx".into() }]);
    }

    #[test]
    fn askpass_gives_the_secret_once_and_leaves_no_file() {
        let a = Askpass::new("s3cret pass".into()).unwrap();
        let dir = a.dir.clone();
        // Another test thread's fork can hold the new file for a moment.
        let out = (0..50)
            .find_map(|_| match std::process::Command::new(&a.script).output() {
                Err(e) if e.raw_os_error() == Some(libc::ETXTBSY) => {
                    std::thread::sleep(Duration::from_millis(20));
                    None
                }
                r => Some(r.unwrap()),
            })
            .unwrap();
        assert_eq!(out.stdout, b"s3cret pass\n");
        // A second prompt gets nothing, at once.
        let again = std::process::Command::new(&a.script).output().unwrap();
        assert_eq!(again.stdout, b"");
        let names: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name()).collect();
        assert_eq!(names.len(), 2, "the script and the FIFO only: {names:?}");
        assert!(!std::fs::read_to_string(&a.script).unwrap().contains("s3cret"));
        drop(a);
        assert!(!dir.exists());
        // An unread secret does not hold the folder or a thread open.
        let b = Askpass::new("never read".into()).unwrap();
        let dir = b.dir.clone();
        drop(b);
        assert!(!dir.exists());
    }

    #[tokio::test]
    async fn the_local_key_is_none_locked_or_ready() {
        let home = temp_dir("keys-test").unwrap();
        let ssh = home.join(".ssh");
        std::fs::create_dir_all(&ssh).unwrap();
        let config = home.join("config");
        std::fs::write(&config, format!("Host t\n  HostName 127.0.0.1\n  IdentityFile {}/.ssh/id_ed25519\n  IdentitiesOnly yes\n", home.display())).unwrap();
        let opts = SshOptions { program: "/usr/bin/ssh".into(), config: Some(config) };
        let t = SshTarget::new("t", "");
        // With no agent: the test must not see the person's agent keys.
        assert_eq!(local_key(&t, &opts, &home, false).await.unwrap(), LocalKey::None);
        let line = make_key(&home, "me@here (Skiff)").await.unwrap();
        assert!(line.starts_with("ssh-ed25519 ") && line.ends_with("me@here (Skiff)"), "{line}");
        assert_eq!(local_key(&t, &opts, &home, false).await.unwrap(), LocalKey::Ready(line));
        assert!(make_key(&home, "x").await.is_err(), "an existing key is never replaced");
        // A key with a passphrase is locked.
        let locked = ssh.join("id_ed25519");
        std::fs::remove_file(&locked).unwrap();
        std::fs::remove_file(locked.with_extension("pub")).unwrap();
        let ok = std::process::Command::new("ssh-keygen").args(["-q", "-t", "ed25519", "-N", "pass phrase", "-f"]).arg(&locked).status().unwrap();
        assert!(ok.success());
        assert_eq!(local_key(&t, &opts, &home, false).await.unwrap(), LocalKey::Locked(locked));
        std::fs::remove_dir_all(&home).unwrap();
    }
}
