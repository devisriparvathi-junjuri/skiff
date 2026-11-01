//! The check list of one remote Host, read only. One strict, batch ssh run
//! sends the server script in its read-only `probe` mode
//! ([`install::SERVER`]) and answers the first rows: can this computer
//! reach the server, does it know the host key, can it log in without a
//! prompt. The probe then says where skiffd's socket is, the one socket of
//! that account (`<runtime dir>/skiff/skiffd.sock`), and whether skiffd
//! answers there. A real `-L` forward and a `ping` over it answer the next
//! rows, and a Hello to skiff-host beside it the projects row. Nothing here
//! writes anywhere: the caller keeps the socket the probe found for the run.
//!
//! The lines follow the design rules: plain causes, the Host's name, never
//! ssh's own output and never a command to run. A failed row names at most
//! one fix ([`Fix`]); `keys` does the work after the person confirms.

use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use skiff_host_api::HostId;

use super::{
    install::SERVER,
    keys::{self, Installed, LocalKey},
    local::HostSelection,
    ssh::{self, BatchRun, SshError, SshOptions, SshTarget, CONNECT_TIMEOUT},
};

/// How long the probe's ssh run may take, login included.
const PROBE_TIMEOUT: Duration = Duration::from_secs(20);

/// What the probe printed.
#[derive(Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct Probe {
    pub os: String,
    pub arch: String,
    pub runtime_dir: String,
    /// The one socket of that account: `<runtime dir>/skiff/skiffd.sock`.
    pub socket: String,
    /// A socket is there.
    pub socket_live: bool,
    /// `skiff status --json` there, when a CLI and a socket were found.
    pub status: Option<Status>,
    /// The install Skiff manages (`~/.local/share/skiff/server/current`).
    #[serde(default)]
    pub managed: Option<Managed>,
    /// The Skiff app's own skiffd there (a .deb, or Skiff.app), or empty.
    /// It runs while the app runs: Skiff installs no second one beside it.
    #[serde(default)]
    pub app: String,
    /// A systemd user manager answers.
    #[serde(default)]
    pub systemd: bool,
    /// The skiffd unit is enabled; on macOS, its LaunchAgent file is there.
    #[serde(default)]
    pub unit_enabled: bool,
    /// The unit is active; on macOS, launchd has the agent loaded.
    #[serde(default)]
    pub unit_active: bool,
    /// skiff-host's user unit is enabled.
    #[serde(default)]
    pub host_unit_enabled: bool,
    /// `loginctl … -p Linger`: `yes`, `no`, or empty without systemd.
    #[serde(default)]
    pub linger: String,
    /// The fallback start's skiffd runs (its pid file).
    #[serde(default)]
    pub fallback: bool,
    /// Free space in the account's home, in KiB.
    #[serde(default)]
    pub free_kb: u64,
}

impl Probe {
    /// `linux` or `macos`, as bundle names write it. None: not supported.
    pub fn bundle_os(&self) -> Option<&'static str> {
        match self.os.as_str() {
            "Linux" => Some("linux"),
            "Darwin" => Some("macos"),
            _ => None,
        }
    }
    /// `x86_64` or `aarch64`. None: not supported.
    pub fn bundle_arch(&self) -> Option<&'static str> {
        match self.arch.as_str() {
            "x86_64" | "amd64" => Some("x86_64"),
            "aarch64" | "arm64" => Some("aarch64"),
            _ => None,
        }
    }
    /// The Skiff version the managed install runs, as this app counts
    /// versions: the bundle its skiffd runs from, else the `current` link.
    /// After an update that stopped before its reload the two differ, and
    /// what runs is what the next update moves from. Never skiffd's own
    /// crate version: that one counts apart.
    pub fn managed_version(&self) -> Option<&str> {
        let m = self.managed.as_ref()?;
        Some(if m.running.is_empty() { &m.version } else { &m.running })
    }
    /// The build id of the bundle [`Self::managed_version`] names. Empty:
    /// a dev bundle from before build ids, or not known.
    pub fn managed_build(&self) -> Option<&str> {
        let m = self.managed.as_ref()?;
        Some(if m.running.is_empty() { &m.build } else { &m.running_build })
    }
}

#[derive(Deserialize, Serialize, Debug, Clone, PartialEq, Eq, Default)]
pub struct Managed {
    /// The Skiff bundle `current` names. Compared with this app's version.
    pub version: String,
    pub protocol: u32,
    /// The bundle the managed skiffd runs from, when the server can tell.
    #[serde(default)]
    pub running: String,
    /// The build id of the bundle `current` names ([`super::bundle::Manifest::build`]).
    #[serde(default)]
    pub build: String,
    /// The build id of the bundle skiffd runs from, with `running`.
    #[serde(default)]
    pub running_build: String,
}

/// `skiff status --json`, as much as the check reads.
#[derive(Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct Status {
    pub skiff: String,
    pub skiffd: Option<StatusDaemon>,
}

#[derive(Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct StatusDaemon {
    pub version: String,
    pub protocol: u32,
}

/// One row of the check list.
#[derive(Serialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Row {
    Reach,
    HostKey,
    Login,
    Supported,
    Skiff,
    Sessions,
    Forwarding,
    Identity,
    Projects,
    KeepRunning,
}

impl Row {
    pub const ALL: [Row; 10] = [Row::Reach, Row::HostKey, Row::Login, Row::Supported, Row::Skiff, Row::Sessions, Row::Forwarding, Row::Identity, Row::Projects, Row::KeepRunning];

    pub fn title(self) -> &'static str {
        match self {
            Row::Reach => "Reach the server",
            Row::HostKey => "Host key",
            Row::Login => "Log in without a prompt",
            Row::Supported => "Server supported",
            Row::Skiff => "Skiff on the server",
            Row::Sessions => "Sessions service (skiffd)",
            Row::Forwarding => "Socket forwarding",
            Row::Identity => "Skiff identity and version",
            Row::Projects => "Projects service (skiff-host)",
            Row::KeepRunning => "Keeps running after logout",
        }
    }
}

#[derive(Serialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Passed,
    Failed,
    Skipped,
    /// A Host can be used with it: the row says what is missing.
    Warning,
}

/// The one button a failed row offers.
#[derive(Serialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Fix {
    TrustKey,
    ReviewKey,
    MakeKey,
    UnlockKey,
    InstallKey,
    CopyPublicKey,
    FixPermissions,
    InstallSkiff,
    StartSkiff,
    TurnOnLinger,
    UpdateSkiff,
}

#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub row: Row,
    pub title: &'static str,
    pub state: State,
    pub text: String,
    pub fix: Option<Fix>,
}

/// The whole list. `socket`: the live socket the probe found, for this run.
#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub rows: Vec<Line>,
    pub ok: bool,
    pub socket: Option<String>,
    /// The Host id skiffd reported, when it answered.
    pub host_id: Option<String>,
    /// The server's OS and arch as bundle names write them, once probed.
    pub platform: Option<(String, String)>,
    /// The Skiff version this app installs.
    pub version: String,
    /// The Skiff version the managed install runs, as the Skiff row
    /// compares it. None: not managed, or not probed.
    #[serde(default)]
    pub installed: Option<String>,
    /// The managed install is this version in another build: Update
    /// Skiff… installs this app's build.
    #[serde(default)]
    pub other_build: bool,
}

/// What a check needs to know about the Host.
pub struct CheckInput {
    /// The login. Its `socket` is ignored: the probe finds it.
    pub target: SshTarget,
    /// The Host's shown name, for the lines.
    pub name: String,
    /// The identity this Host must report, once pinned.
    pub pin: Option<HostId>,
    /// A private folder for the check's own forward.
    pub forward_dir: PathBuf,
    /// This computer's home, to tell a missing or locked key apart. None:
    /// no key diagnosis.
    pub home: Option<PathBuf>,
    /// Ask the SSH agent for its keys.
    pub agent: bool,
    /// What the last key install in this app run found on the server.
    pub installed: Option<Installed>,
    /// The Skiff version this app installs.
    pub version: String,
    /// The build this app installs: a dev build's id, or None for the
    /// release bundle ([`super::install::same_build`]).
    pub build: Option<String>,
    /// The last Turn on in this app run was refused: linger needs an admin.
    pub linger_refused: bool,
    /// Why the last update in this app run did not finish, for the Skiff row.
    pub update_failed: Option<String>,
}

/// The probe's remote command. It is fixed: nothing from this app goes to it.
fn probe_command() -> String {
    "sh -s -- probe".to_string()
}

/// What the probe's ssh run gave.
#[allow(clippy::large_enum_variant, reason = "one per probe run, never in a collection")]
pub enum ProbeRun {
    Probed(Probe, Duration),
    /// ssh stopped before the probe printed: its stderr, kept for the cause.
    Ssh(String),
    TimedOut,
    /// The probe ran but printed no object.
    Garbled,
}

/// Runs the probe over one strict, batch ssh run.
pub async fn run_probe(target: &SshTarget, options: &SshOptions) -> Result<ProbeRun, String> {
    let started = Instant::now();
    let command = probe_command();
    Ok(match ssh::run_batch(target, options, command, SERVER.as_bytes(), PROBE_TIMEOUT).await? {
        BatchRun::Ssh(stderr) => ProbeRun::Ssh(stderr),
        BatchRun::TimedOut => ProbeRun::TimedOut,
        BatchRun::Done(text) => match text.lines().rev().find_map(|l| serde_json::from_str::<Probe>(l.trim()).ok()) {
            Some(p) => ProbeRun::Probed(p, started.elapsed()),
            None => ProbeRun::Garbled,
        },
    })
}

/// A socket path that may go to the probe and to `-L`.
pub fn valid_socket(s: &str) -> bool {
    s.starts_with('/') && !s.contains(':') && !s.chars().any(|c| c.is_control())
}

/// Runs the check list. Rows after the first failure are skipped.
pub async fn check(input: &CheckInput, options: &SshOptions) -> Report {
    let mut list = List { version: input.version.clone(), ..Default::default() };
    let name = input.name.as_str();
    let server = input.target.destination.rsplit('@').next().unwrap_or(&input.target.destination).to_string();
    let probe = match run_probe(&input.target, options).await {
        Err(e) => {
            list.fail(Row::Reach, e);
            return list.finish(None, None);
        }
        Ok(ProbeRun::TimedOut) => {
            list.fail(Row::Reach, format!("{server} did not answer in {CONNECT_TIMEOUT} s. Check the network."));
            return list.finish(None, None);
        }
        Ok(ProbeRun::Ssh(stderr)) => {
            let local = match &input.home {
                Some(home) if denied(&stderr) => keys::local_key(&input.target, options, home, input.agent).await.ok(),
                _ => None,
            };
            login_failure(&mut list, &stderr, name, &server, local.as_ref(), input.installed.as_ref());
            return list.finish(None, None);
        }
        Ok(ProbeRun::Garbled) => {
            list.pass(Row::Reach, "Answers.".into());
            list.pass(Row::HostKey, "Trusted.".into());
            list.pass(Row::Login, "With your key.".into());
            list.fail(Row::Sessions, format!("Skiff cannot read {name}. Its login shell prints extra text."));
            return list.finish(None, None);
        }
        Ok(ProbeRun::Probed(p, took)) => {
            list.pass(Row::Reach, format!("Answers in {} ms.", took.as_millis()));
            list.pass(Row::HostKey, "Trusted.".into());
            list.pass(Row::Login, "With your key.".into());
            p
        }
    };
    let socket = probe.socket.clone();
    let found = probe.socket_live.then(|| socket.clone());
    if !server_rows(&mut list, &probe, name, &input.version, input.build.as_deref(), input.update_failed.as_deref()) {
        return list.finish(found, None);
    }
    let user = input.target.user.clone().or_else(|| input.target.destination.split_once('@').map(|(u, _)| u.to_string()));
    let keep = keep_running(&probe, name, user.as_deref().unwrap_or("your account"), input.linger_refused);

    // The real forward and a ping over it, with no pin: this check stores nothing.
    let target = SshTarget { socket: socket.clone(), ..input.target.clone() };
    let selection = match HostSelection::ssh(target, options.clone(), input.forward_dir.clone(), None) {
        Ok(s) => s,
        Err(e) => {
            list.fail(Row::Forwarding, e);
            return list.finish(found, None);
        }
    };
    let info = async {
        let mut c = selection.connect().await?;
        tokio::time::timeout(Duration::from_secs(15), c.daemon_info()).await.map_err(|_| anyhow::anyhow!("no answer"))?
    };
    let info = match info.await {
        Ok(info) => info,
        Err(e) => {
            // The probe saw a live socket, so a refused channel is forwarding.
            let text = match e.downcast_ref::<SshError>() {
                Some(SshError::NoSkiffd { .. } | SshError::ForwardingRefused(_)) => forwarding_off(name),
                _ => "skiffd did not answer through ssh.".into(),
            };
            list.fail(Row::Forwarding, text);
            return list.finish(found, None);
        }
    };
    list.pass(Row::Forwarding, "Works.".into());
    let Some(identity) = info.identity else {
        list.fail(Row::Identity, format!("Too old. Update Skiff on {name}."));
        return list.finish(found, None);
    };
    let host_id = identity.host_id.clone();
    if input.pin.as_ref().is_some_and(|p| p.as_str() != host_id) {
        list.fail(
            Row::Identity,
            format!("{name} answers as another Skiff install than before. Skiff stopped. Accept only if you know why, such as a reinstall of that server."),
        );
        return list.finish(found, Some(host_id));
    }
    let here = skiff_core::PROTOCOL;
    match info.protocol {
        p if p < here => list.fail(
            Row::Identity,
            format!("Older protocol. Update Skiff on {name} to start and end sessions."),
        ),
        p if p > here => list.fail(Row::Identity, "Newer protocol. Update Skiff on this computer.".into()),
        _ => {
            let same = if input.pin.is_some() { " Same install as before." } else { "" };
            list.pass(Row::Identity, format!("Protocol {}.{same}", info.protocol));
            let answer = match skiff_host_api::HostId::try_from(host_id.clone()) {
                Ok(id) => selection.connect_data(&id).await.map(|plane| plane.connection().hello().service_versions.get("skiff-host").map(|v| format!(" {}", super::data::release(v))).unwrap_or_default()),
                Err(e) => Err(anyhow::anyhow!(e)),
            };
            list.rows.push(projects(answer, probe.managed.is_some()));
            list.rows.push(keep);
        }
    }
    list.finish(found, Some(host_id))
}

/// Rows 4 to 6 from the probe: the server's OS and arch, the install, and
/// whether its skiffd runs. False: a row failed, and the rest are skipped.
/// A skiffd that Skiff did not install is used as it is, never replaced.
fn server_rows(list: &mut List, p: &Probe, name: &str, version: &str, build: Option<&str>, update_failed: Option<&str>) -> bool {
    let (Some(os), Some(arch)) = (p.bundle_os(), p.bundle_arch()) else {
        list.fail(Row::Supported, format!("{name} runs {} on {}. Skiff runs on Linux and macOS, on x86_64 and ARM64.", p.os, p.arch));
        return false;
    };
    list.platform = Some((os.into(), arch.into()));
    list.installed = p.managed_version().map(str::to_string);
    let same = p.managed_build().is_some_and(|b| super::install::same_build(build, b));
    list.other_build = p.managed_version() == Some(version) && !same;
    list.pass(Row::Supported, format!("{} on {}.", p.os, p.arch));
    match (p.managed_version(), p.socket_live) {
        (Some(have), _) if have == version && same => list.pass(Row::Skiff, format!("Skiff {version}.")),
        // Never down: a newer server waits for this computer.
        (Some(have), _) if newer(have, version) => {
            list.warn(Row::Skiff, format!("Skiff {have} is newer than this computer's {version}. Update Skiff here."), None)
        }
        (Some(_), _) if update_failed.is_some() => list.warn(Row::Skiff, update_failed.unwrap_or_default().to_string(), Some(Fix::UpdateSkiff)),
        (Some(have), _) if have == version => list.warn(Row::Skiff, format!("Skiff {have}, another build. Update to this app's build."), Some(Fix::UpdateSkiff)),
        (Some(have), _) => list.warn(Row::Skiff, format!("Update available: Skiff {have} to {version}."), Some(Fix::UpdateSkiff)),
        (None, true) => list.pass(Row::Skiff, "Its own install. Skiff uses it as it is.".into()),
        // The Skiff app there, closed: its skiffd runs with the app, and a
        // second install would fight it for the socket.
        (None, false) if !p.app.is_empty() => {
            list.fail(Row::Skiff, format!("The Skiff app is on {name}. Open it there, then check again."));
            return false;
        }
        (None, false) => {
            list.fix(Row::Skiff, "Not installed.".into(), Fix::InstallSkiff);
            return false;
        }
    }
    match (p.socket_live, p.status.as_ref().map(|s| s.skiffd.as_ref())) {
        (true, Some(Some(d))) => list.pass(Row::Sessions, format!("skiffd {} answers.", d.version)),
        (true, None) => list.pass(Row::Sessions, "Running.".into()),
        // Only the install Skiff made gets a Start button.
        _ if p.managed.is_some() => {
            list.fix(Row::Sessions, "Not running.".into(), Fix::StartSkiff);
            return false;
        }
        _ => {
            list.fail(Row::Sessions, "Not running.".into());
            return false;
        }
    }
    true
}

/// Row 9, from skiff-host's Hello beside skiffd: ` <version>`, or why not.
/// A warning, never a failure: sessions work without it. Only the install
/// Skiff made gets a Start button.
fn projects(answer: anyhow::Result<String>, managed: bool) -> Line {
    let line = |state, text: String, fix| Line { row: Row::Projects, title: Row::Projects.title(), state, text, fix };
    match answer {
        Ok(v) => line(State::Passed, format!("skiff-host{v} answers."), None),
        Err(e) if matches!(e.downcast_ref::<skiff_host_api::Error>(), Some(skiff_host_api::Error::IdentityChanged { .. })) => line(
            State::Warning,
            "Answers as another install than skiffd. Projects do not show.".into(),
            None,
        ),
        Err(_) => line(
            State::Warning,
            "Not running. Sessions work. Projects do not show.".into(),
            managed.then_some(Fix::StartSkiff),
        ),
    }
}

/// Version `a` is newer than `b`, by major.minor.patch. A part that is not
/// a number counts as 0.
pub fn newer(a: &str, b: &str) -> bool {
    let parts = |v: &str| -> Vec<u64> { v.split(['.', '+', '-']).take(3).map(|p| p.parse().unwrap_or(0)).collect() };
    let (a, b) = (parts(a), parts(b));
    (0..3).map(|i| (a.get(i).copied().unwrap_or(0), b.get(i).copied().unwrap_or(0))).find(|(x, y)| x != y).is_some_and(|(x, y)| x > y)
}

/// Row 10. A warning, never a failure: the Host works while you are logged in.
fn keep_running(p: &Probe, name: &str, user: &str, refused: bool) -> Line {
    let line = |state, text: String, fix| Line { row: Row::KeepRunning, title: Row::KeepRunning.title(), state, text, fix };
    if p.managed.is_none() {
        return line(State::Passed, "Its own install decides.".into(), None);
    }
    if p.bundle_os() == Some("macos") {
        // LaunchAgents for skiffd and skiff-host: they start at login. A Mac
        // that nobody logs in to after a restart gets Start on the next check.
        return match p.unit_enabled && p.host_unit_enabled {
            true => line(State::Passed, "After a restart, it starts when you log in there.".into(), None),
            false => line(State::Warning, "Does not start again after a restart.".into(), Some(Fix::StartSkiff)),
        };
    }
    match (p.systemd, p.unit_enabled && p.host_unit_enabled, p.linger.as_str()) {
        (true, true, "yes") => line(State::Passed, "Starts again after a restart.".into(), None),
        (true, false, _) => line(State::Warning, "Does not start again after a restart.".into(), Some(Fix::StartSkiff)),
        (true, true, _) if refused => line(
            State::Warning,
            format!("{name} does not allow lingering. Ask its admin to turn it on for {user}."),
            None,
        ),
        (true, true, _) => line(State::Warning, "Stops when you log out.".into(), Some(Fix::TurnOnLinger)),
        (false, ..) => line(State::Warning, "Runs until the server restarts.".into(), None),
    }
}

fn forwarding_off(name: &str) -> String {
    format!("{name} blocks socket forwarding. Its admin must allow AllowStreamLocalForwarding in sshd.")
}

fn denied(stderr: &str) -> bool {
    stderr.contains("Permission denied") || stderr.contains("Too many authentication failures")
}

/// The first rows, from what ssh said when it stopped, this computer's key
/// (`local`), and what a key install found on the server.
fn login_failure(list: &mut List, stderr: &str, name: &str, server: &str, local: Option<&LocalKey>, installed: Option<&Installed>) {
    let has = |m: &str| stderr.contains(m);
    if has("Could not resolve hostname") || has("nodename nor servname") || has("Name or service not known") {
        return list.fail(Row::Reach, format!("This computer cannot find {server}. Check the name."));
    }
    if has("Connection timed out") || has("Operation timed out") {
        return list.fail(Row::Reach, format!("{server} did not answer within {CONNECT_TIMEOUT} s. Check again, or check the network."));
    }
    if ["Connection refused", "No route to host", "Network is unreachable", "Connection closed by", "Connection reset"].iter().any(|m| has(m)) && !has("Permission denied") {
        return list.fail(Row::Reach, format!("{server} does not answer. Check that it is on."));
    }
    list.pass(Row::Reach, "Answers.".into());
    let target = SshTarget::new(name, "/");
    match super::ssh::classify(stderr, &target) {
        Some(SshError::HostKeyChanged(_)) => {
            return list.fix(Row::HostKey, "The host key changed. Skiff does not connect.".into(), Fix::ReviewKey);
        }
        Some(SshError::HostKeyUnknown(_)) => {
            return list.fix(Row::HostKey, "Not known yet.".into(), Fix::TrustKey);
        }
        _ => {}
    }
    list.pass(Row::HostKey, "Trusted.".into());
    if denied(stderr) {
        // `Permission denied (publickey).`: the server allows keys only.
        let methods = stderr.split("Permission denied (").nth(1).and_then(|r| r.split(')').next()).unwrap_or_default();
        let keys_only = methods.split(',').all(|m| m.trim() == "publickey") && !methods.is_empty();
        return match (local, installed) {
            (Some(LocalKey::None), _) => list.fix(Row::Login, "This computer has no SSH key.".into(), Fix::MakeKey),
            (Some(LocalKey::Locked(_)), _) => list.fix(Row::Login, "Your SSH key is locked.".into(), Fix::UnlockKey),
            // The key is in the file, and the server still refuses it.
            (_, Some(i)) if !i.file_own && i.ssh_dir_own => list.fix(
                Row::Login,
                format!("{name} cannot read your key file. Another account owns it."),
                Fix::FixPermissions,
            ),
            (_, Some(i)) if !i.ssh_dir_own => list.fail(
                Row::Login,
                format!("You do not own ~/.ssh on {name}. Ask its admin to fix it."),
            ),
            _ if keys_only => list.fix(
                Row::Login,
                format!("{name} takes known keys only. Ask its admin to add this computer's key."),
                Fix::CopyPublicKey,
            ),
            _ => list.fix(Row::Login, "This computer's key is not on the server yet.".into(), Fix::InstallKey),
        };
    }
    list.fail(Row::Login, "ssh stopped for an unknown reason.".into());
}

#[derive(Default)]
struct List {
    rows: Vec<Line>,
    version: String,
    platform: Option<(String, String)>,
    installed: Option<String>,
    other_build: bool,
}

impl List {
    fn pass(&mut self, row: Row, text: String) {
        self.rows.push(Line { row, title: row.title(), state: State::Passed, text, fix: None });
    }
    fn fail(&mut self, row: Row, text: String) {
        self.rows.push(Line { row, title: row.title(), state: State::Failed, text, fix: None });
    }
    fn fix(&mut self, row: Row, text: String, fix: Fix) {
        self.rows.push(Line { row, title: row.title(), state: State::Failed, text, fix: Some(fix) });
    }
    fn warn(&mut self, row: Row, text: String, fix: Option<Fix>) {
        self.rows.push(Line { row, title: row.title(), state: State::Warning, text, fix });
    }
    fn finish(mut self, socket: Option<String>, host_id: Option<String>) -> Report {
        let ok = self.rows.len() == Row::ALL.len() && self.rows.iter().all(|r| matches!(r.state, State::Passed | State::Warning));
        for row in Row::ALL.into_iter().skip(self.rows.len()) {
            self.rows.push(Line { row, title: row.title(), state: State::Skipped, text: "Waits for the row above.".into(), fix: None });
        }
        Report { rows: self.rows, ok, socket, host_id, platform: self.platform, version: self.version, installed: self.installed, other_build: self.other_build }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(stderr: &str) -> Vec<(Row, State, String)> {
        with(stderr, None, None).into_iter().map(|l| (l.row, l.state, l.text)).collect()
    }

    fn with(stderr: &str, local: Option<&LocalKey>, installed: Option<&Installed>) -> Vec<Line> {
        let mut list = List::default();
        login_failure(&mut list, stderr, "devbox", "devbox.lan", local, installed);
        list.finish(None, None).rows
    }

    fn fix(stderr: &str, local: Option<&LocalKey>, installed: Option<&Installed>) -> (String, Option<Fix>) {
        let f = with(stderr, local, installed).into_iter().find(|l| l.state == State::Failed).unwrap();
        (f.text, f.fix)
    }

    fn probe() -> Probe {
        Probe {
            os: "Linux".into(),
            arch: "x86_64".into(),
            runtime_dir: "/run/user/1000".into(),
            socket: "/run/user/1000/skiff/skiffd.sock".into(),
            socket_live: false,
            status: None,
            managed: None,
            app: String::new(),
            systemd: true,
            unit_enabled: false,
            unit_active: false,
            host_unit_enabled: false,
            linger: "no".into(),
            fallback: false,
            free_kb: 1 << 20,
        }
    }

    fn server(p: &Probe) -> (bool, Vec<(Row, State, Option<Fix>, String)>) {
        let mut list = List::default();
        let go = server_rows(&mut list, p, "devbox", "2.0.0", None, None);
        (go, list.rows.into_iter().map(|l| (l.row, l.state, l.fix, l.text)).collect())
    }

    #[test]
    fn the_server_rows_install_start_or_leave_alone() {
        // Nothing there: Install.
        let (go, rows) = server(&probe());
        assert!(!go);
        assert_eq!((rows[1].0, rows[1].2), (Row::Skiff, Some(Fix::InstallSkiff)));
        // Another Skiff runs there: used as it is, never an Install.
        let other = Probe { socket_live: true, status: Some(Status { skiff: "1.6.6".into(), skiffd: Some(StatusDaemon { version: "0.6.24".into(), protocol: 27 }) }), ..probe() };
        let (go, rows) = server(&other);
        assert!(go && rows.iter().all(|r| r.2.is_none()), "{rows:?}");
        assert!(rows[1].3.contains("own install"));
        // Ours, stopped: Start. Ours, older: Update. Ours, newer: never down.
        let ours = Probe { managed: Some(Managed { version: "2.0.0".into(), protocol: 27, running: String::new(), build: "e8368d7".into(), running_build: "e8368d7".into() }), ..probe() };
        let (go, rows) = server(&ours);
        assert!(!go);
        assert_eq!((rows[2].0, rows[2].2), (Row::Sessions, Some(Fix::StartSkiff)));
        // The same version from an earlier dev build: Update, to this
        // app's build. A dev app's own build differs from a release.
        let dev = Probe { managed: Some(Managed { version: "2.0.0".into(), protocol: 27, build: "dev".into(), ..Default::default() }), socket_live: true, ..probe() };
        let (_, rows) = server(&dev);
        assert_eq!((rows[1].1, rows[1].2), (State::Warning, Some(Fix::UpdateSkiff)), "{rows:?}");
        assert!(rows[1].3.contains("another build"));
        assert!(super::super::install::same_build(Some("dev-1"), "dev-1") && !super::super::install::same_build(Some("dev-1"), "dev-2") && !super::super::install::same_build(Some("dev-1"), "e8368d7"));
        assert!(super::super::install::same_build(None, "e8368d7") && !super::super::install::same_build(None, "dev") && !super::super::install::same_build(None, "dev-1") && !super::super::install::same_build(None, ""));
        let older = Probe { managed: Some(Managed { version: "1.9.0".into(), protocol: 27, running: String::new(), build: "e8368d7".into(), running_build: "e8368d7".into() }), socket_live: true, ..probe() };
        let (go, rows) = server(&older);
        assert!(go);
        assert_eq!((rows[1].1, rows[1].2), (State::Warning, Some(Fix::UpdateSkiff)));
        assert!(rows[1].3.starts_with("Update available:"));
        let newer_one = Probe { managed: Some(Managed { version: "2.10.0".into(), protocol: 27, running: String::new(), build: "e8368d7".into(), running_build: "e8368d7".into() }), socket_live: true, ..probe() };
        let (_, rows) = server(&newer_one);
        assert_eq!((rows[1].1, rows[1].2), (State::Warning, None));
        assert!(rows[1].3.contains("Update Skiff here"));
        // An update that stopped after it moved `current`: the skiffd that
        // runs is still the old bundle's, and the row goes by it.
        let stopped = Probe { managed: Some(Managed { version: "2.0.0".into(), protocol: 27, running: "1.9.0".into(), build: "e8368d7".into(), running_build: "e8368d7".into() }), ..ours.clone() };
        assert_eq!(stopped.managed_version(), Some("1.9.0"));
        let (_, rows) = server(&Probe { socket_live: true, ..stopped });
        assert_eq!((rows[1].1, rows[1].2), (State::Warning, Some(Fix::UpdateSkiff)), "{rows:?}");
        // Not a Skiff platform: the row says what it found.
        let (go, rows) = server(&Probe { os: "FreeBSD".into(), arch: "amd64".into(), ..probe() });
        assert!(!go && rows[0].3.contains("FreeBSD on amd64"));
        // A Mac server: Install, the same as Linux.
        let (_, rows) = server(&Probe { os: "Darwin".into(), arch: "arm64".into(), systemd: false, ..probe() });
        assert_eq!((rows[1].1, rows[1].2), (State::Failed, Some(Fix::InstallSkiff)));
        // The Skiff app there, closed: no Install, only where to go.
        let (go, rows) = server(&Probe { app: "/Applications/Skiff.app/Contents/MacOS/skiffd".into(), ..probe() });
        assert!(!go && rows[1].1 == State::Failed && rows[1].2.is_none() && rows[1].3.contains("Open it there"), "{rows:?}");
    }

    #[test]
    fn versions_compare_by_number() {
        assert!(newer("2.10.0", "2.9.9") && newer("2.0.1", "2.0.0") && newer("3", "2.9"));
        assert!(!newer("2.0.0", "2.0.0") && !newer("2.0.0-rc.1", "2.0.0") && !newer("1.6.6", "2.0.0"));
    }

    #[test]
    fn keeping_it_running_is_a_warning_with_one_fix() {
        let ours = Probe { managed: Some(Managed { version: "2.0.0".into(), protocol: 27, running: String::new(), build: "e8368d7".into(), running_build: "e8368d7".into() }), unit_enabled: true, host_unit_enabled: true, ..probe() };
        let k = |p: &Probe, refused| { let l = keep_running(p, "devbox", "jose", refused); (l.state, l.fix) };
        assert_eq!(k(&ours, false), (State::Warning, Some(Fix::TurnOnLinger)));
        assert_eq!(k(&ours, true), (State::Warning, None), "refused: plain words only");
        assert_eq!(k(&Probe { linger: "yes".into(), ..ours.clone() }, false), (State::Passed, None));
        assert_eq!(k(&Probe { systemd: false, ..ours.clone() }, false), (State::Warning, None));
        assert_eq!(k(&probe(), false), (State::Passed, None), "not ours: not our business");
        // An install from before skiff-host had its unit: Start adds it.
        assert_eq!(k(&Probe { host_unit_enabled: false, ..ours.clone() }, false), (State::Warning, Some(Fix::StartSkiff)));
        // macOS: the LaunchAgent files decide; linger means nothing there.
        let mac = Probe { os: "Darwin".into(), arch: "arm64".into(), systemd: false, linger: String::new(), ..ours.clone() };
        assert_eq!(k(&mac, false), (State::Passed, None));
        assert_eq!(k(&Probe { unit_enabled: false, ..mac.clone() }, false), (State::Warning, Some(Fix::StartSkiff)));
        assert_eq!(k(&Probe { host_unit_enabled: false, ..mac }, false), (State::Warning, Some(Fix::StartSkiff)));
        // A warning still lets the Host be used.
        let mut list = List::default();
        for row in Row::ALL.into_iter().take(9) {
            list.pass(row, String::new());
        }
        list.rows.push(keep_running(&ours, "devbox", "jose", false));
        assert!(list.finish(None, None).ok);
    }

    #[test]
    fn the_projects_row_is_a_warning_with_start_for_our_install_only() {
        let p = |answer, managed| { let l = projects(answer, managed); (l.state, l.fix, l.text) };
        assert_eq!(p(Ok(" 2.0.0".into()), true), (State::Passed, None, "skiff-host 2.0.0 answers.".into()));
        let down = || Err(anyhow::anyhow!(super::super::data::DataError::NotRunning));
        let (state, fix, text) = p(down(), true);
        assert_eq!((state, fix), (State::Warning, Some(Fix::StartSkiff)));
        assert!(text.contains("Sessions work."), "{text}");
        assert_eq!(p(down(), false).1, None, "not ours: no Start");
        let other = skiff_host_api::Error::IdentityChanged { expected: "a".to_string().try_into().unwrap(), received: "b".to_string().try_into().unwrap() };
        assert_eq!(p(Err(other.into()), true).1, None, "another install: no Start");
    }

    #[test]
    fn a_refused_login_names_the_key_case_and_its_one_fix() {
        let pw = "me@devbox.lan: Permission denied (publickey,password).\n";
        let keys = "me@devbox.lan: Permission denied (publickey).\n";
        let ready = LocalKey::Ready("ssh-ed25519 AAAA".into());
        assert_eq!(fix(pw, Some(&LocalKey::None), None), ("This computer has no SSH key.".into(), Some(Fix::MakeKey)));
        assert_eq!(fix(pw, Some(&LocalKey::Locked("/k".into())), None).1, Some(Fix::UnlockKey));
        assert_eq!(fix(pw, Some(&ready), None).1, Some(Fix::InstallKey));
        assert_eq!(fix(keys, Some(&ready), None).1, Some(Fix::CopyPublicKey));
        let foreign = Installed { added: true, ssh_dir_own: true, file_own: false };
        let (text, f) = fix(pw, Some(&ready), Some(&foreign));
        assert_eq!(f, Some(Fix::FixPermissions));
        assert!(text.contains("cannot read your key file"), "{text}");
        let lost = Installed { added: false, ssh_dir_own: false, file_own: false };
        assert_eq!(fix(pw, Some(&ready), Some(&lost)).1, None, "no fix when ~/.ssh is not the account's");
        // A good install says nothing: the next refusal is plain again.
        let good = Installed { added: true, ssh_dir_own: true, file_own: true };
        assert_eq!(fix(pw, Some(&ready), Some(&good)).1, Some(Fix::InstallKey));
        let unknown = "No ED25519 host key is known for devbox.lan and you have requested strict checking.\nHost key verification failed.\n";
        assert_eq!(fix(unknown, None, None).1, Some(Fix::TrustKey));
        assert_eq!(fix("@ WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED! @\n", None, None).1, Some(Fix::ReviewKey));
    }

    fn failed(stderr: &str) -> (Row, String) {
        let rows = lines(stderr);
        let f = rows.iter().find(|r| r.1 == State::Failed).expect("a failed row");
        // Every row after it is skipped, and nothing repeats ssh's output.
        let at = rows.iter().position(|r| r.1 == State::Failed).unwrap();
        assert!(rows[at + 1..].iter().all(|r| r.1 == State::Skipped), "{rows:?}");
        assert!(!f.2.contains("ssh:") && !f.2.contains(".ssh/"), "{}", f.2);
        (f.0, f.2.clone())
    }

    #[test]
    fn each_ssh_failure_names_one_row_and_one_plain_cause() {
        let cases = [
            ("ssh: Could not resolve hostname devbox.lann: Name or service not known\n", Row::Reach, "cannot find devbox.lan"),
            ("ssh: connect to host 10.0.4.21 port 22: Connection timed out\n", Row::Reach, "in 15 s"),
            ("ssh: connect to host 10.0.4.21 port 22: No route to host\n", Row::Reach, "does not answer"),
            ("@@@\n@ WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED! @\nHost key verification failed.\n", Row::HostKey, "host key changed"),
            ("No ED25519 host key is known for devbox.lan and you have requested strict checking.\nHost key verification failed.\n", Row::HostKey, "Not known yet"),
            ("me@devbox.lan: Permission denied (publickey).\n", Row::Login, "known keys only"),
            ("me@devbox.lan: Permission denied (publickey,password).\n", Row::Login, "key is not on the server"),
            ("kex_exchange_identification: read: Connection reset by peer\n", Row::Reach, "does not answer"),
            ("something new\n", Row::Login, "unknown reason"),
        ];
        for (stderr, row, words) in cases {
            let (got, text) = failed(stderr);
            assert_eq!(got, row, "{stderr}");
            assert!(text.contains(words), "{stderr}: {text}");
        }
    }

    #[test]
    fn the_probe_command_is_fixed_and_quotes_its_values() {
        let t = SshTarget { user: Some("me".into()), port: Some(2222), ..SshTarget::new("devbox", "") };
        let command = probe_command();
        let args: Vec<String> = ssh::batch_args(&t, &SshOptions { program: "ssh".into(), config: None }, command)
            .into_iter()
            .map(|a| a.into_string().unwrap())
            .collect();
        for want in ["BatchMode=yes", "StrictHostKeyChecking=yes", "ControlPath=none", "ForwardAgent=no"] {
            assert!(args.iter().any(|a| a == want), "{want}: {args:?}");
        }
        assert!(!args.iter().any(|a| a.contains("accept-new") || a.contains("StrictHostKeyChecking=no")));
        let n = args.len();
        assert_eq!(&args[n - 7..], ["-l", "me", "-p", "2222", "--", "devbox", "sh -s -- probe"]);
    }

    /// The probe as the server runs it: here, with `sh`, no ssh. It looks
    /// at the one socket, `<runtime dir>/skiff/skiffd.sock`, whatever runs
    /// on this machine.
    #[test]
    fn the_probe_prints_one_object_and_finds_the_socket() {
        let dir = std::env::temp_dir().join(format!("skiff-probe-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("skiff")).unwrap();
        let run = || {
            let mut child = std::process::Command::new("sh")
                .args(["-s", "--", "probe"])
                .env("XDG_RUNTIME_DIR", &dir)
                .env("HOME", &dir)
                .env("PATH", "/usr/bin:/bin")
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            std::io::Write::write_all(&mut child.stdin.take().unwrap(), SERVER.as_bytes()).unwrap();
            let out = child.wait_with_output().unwrap();
            serde_json::from_slice::<Probe>(&out.stdout).unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&out.stdout)))
        };
        let sock = dir.join("skiff/skiffd.sock");
        let none = run();
        assert_eq!((none.runtime_dir.as_str(), none.socket.as_str(), none.socket_live), (&*dir.to_string_lossy(), &*sock.to_string_lossy(), false), "{none:?}");
        // Another name in the folder is not Skiff's socket.
        let _other = std::os::unix::net::UnixListener::bind(dir.join("skiff/skiffd-x.sock")).unwrap();
        assert!(!run().socket_live);
        let _listener = std::os::unix::net::UnixListener::bind(&sock).unwrap();
        let p = run();
        assert_eq!(p.socket, sock.to_string_lossy());
        assert!(p.socket_live && p.status.is_none(), "{p:?}");
        // The Skiff app's skiffd, when this machine has one (a .deb here).
        assert!(p.app.is_empty() || std::path::Path::new(&p.app).is_file(), "{p:?}");
        assert!(!p.os.is_empty() && !p.arch.is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
