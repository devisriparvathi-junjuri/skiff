//! Installing and updating Skiff on a Host: probes, bundles, steps and the update on connect.

use super::*;

/// The probe, for a step that changes the server: it must see the state now.
pub(super) async fn probe_now(h: &KnownHost) -> Result<skiff_client::host::check::Probe, String> {
    use skiff_client::host::check;
    match check::run_probe(&h.target(), &SshOptions::default()).await? {
        check::ProbeRun::Probed(p, _) => {
            let other = other_build(&p);
            memo(&h.id, |m| {
                m.skiff = p.managed_version().map(str::to_string);
                m.other_build = other;
                m.socket = Some(p.socket.clone());
            });
            Ok(p)
        }
        _ => Err(format!("Skiff cannot read the state of {}. Check again.", h.name)),
    }
}

pub(super) async fn install_run(id: &str, step: skiff_client::host::install::Steps<'_>) -> Result<InstallView, String> {
    use skiff_client::host::install::{self, Progress};
    let id = id.to_string();
    let h = host_by_id(&id).await?;
    step(Progress::Step { n: 1, text: format!("Check {}", h.name) });
    let p = probe_now(&h).await?;
    if p.socket_live && p.managed.is_none() {
        return Err(format!("{} runs its own Skiff. Skiff does not replace it.", h.name));
    }
    if p.managed.is_none() && !p.app.is_empty() {
        return Err(format!("The Skiff app is on {}. Skiff installs no second one.", h.name));
    }
    // An install over a managed one would move the version without the
    // reload handover: that is an update.
    if p.managed.is_some() {
        return Err(format!("Skiff is installed on {} already. Check again.", h.name));
    }
    let (Some(os), Some(arch)) = (p.bundle_os(), p.bundle_arch()) else {
        return Err(format!("Skiff runs on Linux and macOS, on x86_64 and ARM64. {} runs {} on {}.", h.name, p.os, p.arch));
    };
    step(Progress::Line { text: format!("{} on {}, {} MB free in your home folder", p.os, p.arch, p.free_kb / 1024) });
    step(Progress::Step { n: 2, text: format!("Get Skiff {VERSION} for {os} {arch}") });
    let bundle = bundle_for(os, arch).await?;
    let outcome = install::install(&h.target(), &SshOptions::default(), &bundle, VERSION, p.free_kb, &h.name, step).await?;
    memo(&id, |m| {
        m.linger_refused = outcome.linger == "refused";
        m.ok = None;
        // The next connect reads the version again, for the badge.
        m.arrival = None;
    });
    Ok(InstallView { outcome, source: bundle.source, version: VERSION })
}

/// The page shows each step of an install or update on this event, as
/// `{ id, kind, ... }` ([`skiff_client::host::install::Progress`]), while
/// the dialog waits.
pub(super) const HOST_STEP: &str = "skiff:host-step";

/// Sends each step of the Host `id` to the page.
pub(super) fn steps(handle: tauri::AppHandle, id: &str) -> impl Fn(skiff_client::host::install::Progress) + Send + Sync {
    use tauri::Emitter;
    let id = id.to_string();
    move |p| {
        let mut v = serde_json::to_value(&p).unwrap_or_default();
        v["id"] = id.clone().into();
        let _ = handle.emit(HOST_STEP, v);
    }
}

/// Runs `work` until it ends or Cancel stops it. Stopping drops it, which
/// ends its ssh. The page offers Cancel only until the switch to the new
/// version (step 5): before it, nothing on the server has changed but an
/// upload that the next run replaces.
pub(super) async fn cancellable<T>(id: &str, work: impl std::future::Future<Output = Result<T, String>>) -> Result<T, String> {
    let stop = std::sync::Arc::new(tokio::sync::Notify::new());
    RUNNING.lock().unwrap().insert(id.to_string(), stop.clone());
    let out = tokio::select! {
        out = work => out,
        _ = stop.notified() => Err("Cancelled. Skiff on the server did not change.".into()),
    };
    RUNNING.lock().unwrap().remove(id);
    out
}

/// The folder of this app's executable, where a dev build's own skiffd,
/// skiff-host and skiff are.
fn beside_exe() -> Result<std::path::PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    Ok(exe.parent().ok_or("This build has no folder.")?.to_path_buf())
}

/// A dev build sends its own binaries to a server with this computer's OS
/// and arch, `platform` (None: this computer's). This build's id for them,
/// or None: the server gets the release bundle of this version.
pub(super) fn own_build(platform: Option<(&str, &str)>) -> Option<String> {
    let (os, arch) = platform.unwrap_or((std::env::consts::OS, std::env::consts::ARCH));
    if !cfg!(debug_assertions) || os != std::env::consts::OS || arch != std::env::consts::ARCH {
        return None;
    }
    skiff_client::host::install::dev_build(&beside_exe().ok()?).ok()
}

/// The managed install on the server of `p` is this app's version in
/// another build than the one this app sends.
pub(super) fn other_build(p: &skiff_client::host::check::Probe) -> bool {
    let mine = own_build(p.bundle_os().zip(p.bundle_arch()));
    p.managed_version() == Some(VERSION) && !p.managed_build().is_some_and(|b| skiff_client::host::install::same_build(mine.as_deref(), b))
}

/// The bundle of this app's build for a server's `os` and `arch`: a dev
/// build's own binaries for a server with this computer's OS and arch, else
/// the signed release bundle of this app's version. A dev build with no
/// such release tries its own binaries on a server with this computer's OS.
async fn bundle_for(os: &str, arch: &str) -> Result<skiff_client::host::install::Bundle, String> {
    use skiff_client::host::install;
    if own_build(Some((os, arch))).is_some() {
        return install::dev_bundle(VERSION, arch, &beside_exe()?).await;
    }
    match install::download(VERSION, os, arch).await {
        Ok(b) => Ok(b),
        Err(e) if cfg!(debug_assertions) && os == std::env::consts::OS => install::dev_bundle(VERSION, arch, &beside_exe()?).await.map_err(|dev| format!("{e} {dev}")),
        Err(e) => Err(e),
    }
}

/// Moves the managed install on a server up to this app's version.
/// skiffd reloads onto it and keeps its sessions. When it cannot, nothing
/// changes and the answer names the sessions an update would end, unless
/// `end_sessions`: the person saw them in the red dialog and agreed. The
/// same version in another build moves to this app's build; `reinstall`
/// installs this app's build even when it is there. Never a Skiff that
/// Skiff did not install, and never down.
pub(super) async fn update_managed(h: &KnownHost, end_sessions: bool, reinstall: bool, step: skiff_client::host::install::Steps<'_>) -> Result<UpdateView, String> {
    use skiff_client::host::{check, install};
    use install::Progress;
    step(Progress::Step { n: 1, text: format!("Check {}", h.name) });
    let p = probe_now(h).await?;
    let Some(have) = p.managed_version() else {
        return Err(format!("Skiff did not install Skiff on {}, so it does not update it.", h.name));
    };
    if have == VERSION && !reinstall && !other_build(&p) {
        return Ok(UpdateView { updated: install::Updated::Same, source: "", version: VERSION });
    }
    if check::newer(have, VERSION) {
        return Ok(UpdateView { updated: install::Updated::Newer { from: have.to_string() }, source: "", version: VERSION });
    }
    let (Some(os), Some(arch)) = (p.bundle_os(), p.bundle_arch()) else {
        return Err(format!("Skiff runs on Linux and macOS, on x86_64 and ARM64. {} runs {} on {}.", h.name, p.os, p.arch));
    };
    step(Progress::Step { n: 2, text: format!("Get Skiff {VERSION} for {os} {arch}") });
    let bundle = bundle_for(os, arch).await?;
    let result = install::update(&h.target(), &SshOptions::default(), &bundle, VERSION, p.free_kb, &h.name, end_sessions, reinstall, step).await;
    let failed = match &result {
        Err(e) => Some(e.clone()),
        Ok(install::Updated::NoHandover { from, sessions }) => Some(no_handover(&h.name, from, sessions)),
        Ok(_) => None,
    };
    memo(&h.id, |m| {
        m.update_failed = failed;
        m.ok = None;
        m.arrival = None;
    });
    Ok(UpdateView { updated: result?, source: bundle.source, version: VERSION })
}

/// The Skiff row's words when skiffd cannot hand its sessions over.
fn no_handover(_name: &str, from: &str, sessions: &[String]) -> String {
    let n = sessions.len();
    format!("Update available: Skiff {from} to {VERSION}. It ends {n} {}.", if n == 1 { "session" } else { "sessions" })
}

/// What the app learned about a Host's install as it connected.
#[derive(Clone, Debug, Default)]
pub(crate) struct Arrival {
    pub(crate) name: String,
    /// Skiff installed the Skiff there.
    pub(crate) managed: bool,
    /// The version this connect updated from.
    pub(super) updated_from: Option<String>,
    /// The Skiff version of the managed install, as this app counts
    /// versions: what the check list's Skiff row compares. Never skiffd's
    /// own crate version. None: not managed, or not known.
    pub(crate) version: Option<String>,
}

/// The socket the last probe of this app run found on the Host `target`
/// logs in to, for the selection's forward.
pub(crate) fn found_socket(target: &SshTarget) -> Option<String> {
    let h = load(&path()).ok()?.find_login(target).cloned()?;
    memo(&h.id, |m| m.socket.clone())
}

/// How long after a failed probe on connect the next connect probes again.
pub(super) const ARRIVAL_RETRY: std::time::Duration = std::time::Duration::from_secs(300);

/// Before the session forward opens: a managed install older than this app,
/// or this version in another build, is updated, once per app run, if the Host allows it and its sessions
/// survive. No prompt: a reload keeps every session. An update that would
/// end sessions is left to the person, in the check list.
pub(crate) async fn update_on_connect(target: &SshTarget) -> Option<Arrival> {
    use skiff_client::host::install::Updated;
    let t = target.clone();
    let h = blocking(move || Ok(load(&path())?.find_login(&t).cloned())).await.ok().flatten()?;
    if let Some((arrival, at)) = memo(&h.id, |m| m.arrival.clone()) {
        if arrival.is_some() || at.elapsed() < ARRIVAL_RETRY {
            return arrival;
        }
    }
    let mut arrival = None;
    if let Ok(p) = probe_now(&h).await {
        let mut a = Arrival { name: h.name.clone(), managed: p.managed.is_some(), updated_from: None, version: p.managed_version().map(str::to_string) };
        let older = p.managed_version().is_some_and(|have| skiff_client::host::check::newer(VERSION, have)) || other_build(&p);
        if older && h.auto_update {
            match update_managed(&h, false, false, &|_| {}).await {
                Ok(UpdateView { updated: Updated::Done { from, .. }, .. }) => {
                    a.updated_from = Some(from);
                    a.version = Some(VERSION.to_string());
                }
                Ok(_) => {}
                Err(e) => eprintln!("update on connect to {}: {e}", h.name),
            }
        }
        arrival = Some(a);
    }
    memo(&h.id, |m| m.arrival = Some((arrival.clone(), std::time::Instant::now())));
    arrival
}

/// The banner for a Host whose Skiff differs from this app's. None: they
/// work together as they are. `version`: the managed install's Skiff
/// version, compared as the check list's Skiff row compares it; None for
/// its own install, which is judged by protocol only. `managed`: Skiff
/// installed it, so Settings › Hosts can update it; a Skiff from its own
/// install is never changed.
pub(crate) fn version_warning(name: &str, version: Option<&str>, protocol: u32, managed: bool) -> Option<(&'static str, String)> {
    use skiff_client::host::check::newer;
    let here = skiff_core::PROTOCOL;
    if let Some(v) = version.filter(|v| newer(v, VERSION)) {
        return Some(("remote", format!("{name} runs Skiff {v}. This computer runs {VERSION}. Update Skiff on this computer.")));
    }
    let older = version.filter(|v| newer(VERSION, v));
    match (managed, older, protocol == here) {
        (true, Some(v), false) => Some(("update", format!(
            "{name} runs Skiff {v}, older than this computer's {VERSION}. You can see and type in its sessions. Update Skiff on {name} to start and end sessions here."
        ))),
        (true, Some(v), true) => Some(("update", format!("Update available: Skiff {v} to {VERSION}."))),
        // Managed, version not read: the protocol still says it is older.
        (true, None, false) if protocol < here => Some(("update", format!(
            "{name} runs an older Skiff. You can see and type in its sessions. Update Skiff on {name} to start and end sessions here."
        ))),
        (false, _, false) if protocol < here => Some(("remote", format!(
            "{name} runs an older Skiff from its own install. You can see and type in its sessions. Update Skiff on {name} to start and end sessions here."
        ))),
        // Never down: a newer Skiff there waits for this computer.
        (_, _, false) if protocol > here => Some(("remote", format!("{name} runs a newer Skiff. This computer runs {VERSION}. Update Skiff on this computer."))),
        (_, _, false) => Some(("remote", format!("Skiff on {name} speaks protocol {protocol}, and this app speaks {here}. Parts of the app fail."))),
        _ => None,
    }
}
