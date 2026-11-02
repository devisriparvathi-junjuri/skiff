//! Known Hosts and which one this window shows: client-owned `hosts.toml`
//! beside `projects.toml`. "This Mac" is the local Host and needs no entry.
//! A remote Host has a local id that never changes, a shown name, an SSH
//! destination (an alias from ssh config, a hostname or an address), and an
//! optional user and port. The skiffd socket on it is not stored: each Host
//! has one, `<runtime dir>/skiff/skiffd.sock` there, and the probe finds it
//! on each connect (and in Settings › Hosts › Check). No two Hosts
//! have the same destination, user and port. Each remote Host keeps the Skiff identity it first
//! reported (`host_id`): a different one is refused on every later
//! connection, after a new selection or app start too, until the person
//! accepts it. Each remote Host has its own clipboard opt-in for OSC 52
//! writes from its terminals, given to that identity only, and it counts
//! only while this window's live connection answers as that identity.
//!
//! Every change to the file is one transaction for all windows and app
//! runs: [`update`] holds an exclusive `flock` from the read to the rename.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use skiff_client::host::{
    keys,
    local::HostSelection,
    ssh::{SshOptions, SshTarget},
};
use skiff_host_api::HostId;
use tauri::{Manager, State};

use crate::connection::{blocking, err, App};

pub(crate) use skiff_client::host::known::{forward_dir, load, path, Hosts, KnownHost};

mod form;
mod list;
mod provision;
mod store;

use form::*;
use list::*;
use provision::*;
use store::*;
pub(crate) use form::HostForm;
pub(crate) use list::{note, note_login, watch, HostList};
pub(crate) use provision::{found_socket, update_on_connect, version_warning, Arrival};
pub(crate) use store::{pin_first, remote, selected};

#[tauri::command]
pub(crate) async fn list_hosts(app: State<'_, App>) -> Result<HostList, String> {
    let hosts = blocking(|| forget_removed(&path())).await?;
    let selection = app.selection();
    let identity = app.live_identity().or_else(|| selection.pinned());
    let selected = selection.ssh_target().and_then(|t| hosts.find_login(t)).map(|h| h.id.clone());
    let removed = selection.ssh_target().filter(|t| hosts.find_login(t).is_none()).map(|t| t.destination.clone());
    Ok(HostList { removed, ..host_list(hosts, selected, identity.as_ref()) })
}

/// An OSC 52 write from the shown Host may reach this Mac's clipboard now:
/// asked on every write, against the identity this window's live connection
/// accepted and the file as it is now. This Mac: always.
#[tauri::command]
pub(crate) async fn host_clipboard(app: State<'_, App>) -> Result<bool, String> {
    let Some(target) = app.selection().ssh_target().cloned() else {
        return Ok(true);
    };
    let Some(live) = app.live_identity() else { return Ok(false) };
    blocking(move || clipboard_allowed(&path(), &target, &live)).await
}

/// Adds a remote Host and answers its id. It does not connect.
#[tauri::command]
pub(crate) async fn add_host(form: HostForm) -> Result<String, String> {
    blocking(move || {
        let (name, target) = form.parse()?;
        update(&path(), |hosts| {
            check_unique(hosts, &name, &target, None)?;
            let id = new_id(hosts)?;
            hosts.hosts.push(KnownHost {
                id: id.clone(),
                name,
                destination: target.destination,
                user: target.user,
                port: target.port,
                host_id: None,
                clipboard_host: None,
                auto_update: true,
            });
            Ok(id)
        })
    })
    .await
}

/// Changes a Host's name, server, user or port. Its id and its
/// pin stay: if another machine answers there now, the pin catches it. The
/// shown Host keeps its connection, so only its name can change.
#[tauri::command]
pub(crate) async fn edit_host(app: State<'_, App>, id: String, form: HostForm) -> Result<(), String> {
    let shown = shown_id(&app);
    blocking(move || {
        let (name, target) = form.parse()?;
        update(&path(), |hosts| {
            check_unique(hosts, &name, &target, Some(&id))?;
            let h = known(hosts, &id)?;
            // Another login: the next connect or check finds its socket.
            let moved = !h.logs_in_as(&target);
            anyhow::ensure!(!(moved && shown.as_deref() == Some(id.as_str())), "This window shows {}. Pick another Host first, then change its server.", h.name);
            if moved {
                forget_check(&id);
                memo(&id, |m| m.socket = None);
            }
            h.name = name;
            h.destination = target.destination;
            h.user = target.user;
            h.port = target.port;
            Ok(())
        })
    })
    .await
}

/// Changes only the shown name.
#[tauri::command]
pub(crate) async fn rename_host(id: String, name: String) -> Result<(), String> {
    blocking(move || {
        update(&path(), |hosts| {
            let name = name.trim().to_string();
            anyhow::ensure!(!name.is_empty() && !name.chars().any(|c| c.is_control()), "Type a name on one line.");
            let me = known(hosts, &id)?.target();
            check_unique(hosts, &name, &me, Some(&id))?;
            known(hosts, &id)?.name = name;
            Ok(())
        })
    })
    .await
}

/// Forgets a remote Host. The shown Host cannot go: pick another first.
/// `uninstall`: first stop Skiff's services on the server and delete the
/// managed install. If that fails, the Host stays in the list.
#[tauri::command]
pub(crate) async fn remove_host(app: State<'_, App>, id: String, uninstall: Option<bool>) -> Result<(), String> {
    if shown_id(&app).as_deref() == Some(id.as_str()) {
        return Err("This window shows that Host. Pick another Host first.".into());
    }
    if uninstall == Some(true) {
        let h = host_by_id(&id).await?;
        skiff_client::host::install::uninstall(&h.target(), &SshOptions::default(), &h.name).await?;
        MEMO.lock().unwrap().remove(&id);
    }
    blocking(move || {
        update(&path(), |hosts| {
            hosts.hosts.retain(|h| h.id != id);
            if hosts.selected.as_deref() == Some(id.as_str()) {
                hosts.selected = None;
            }
            Ok(())
        })
    })
    .await
}

/// For the Remove confirm: whether Skiff installed Skiff on the Host, and
/// which sessions an uninstall ends. Read only.
#[tauri::command]
pub(crate) async fn uninstall_preview(id: String) -> Result<skiff_client::host::install::Preview, String> {
    let h = host_by_id(&id).await?;
    skiff_client::host::install::preview(&h.target(), &SshOptions::default()).await
}

/// The aliases in this computer's ssh config, for the Server field.
#[tauri::command]
pub(crate) async fn ssh_aliases() -> Result<Vec<skiff_client::host::ssh_config::Alias>, String> {
    blocking(|| {
        let home = std::env::var_os("HOME").map(PathBuf::from).ok_or_else(|| anyhow::anyhow!("no home folder"))?;
        Ok(skiff_client::host::ssh_config::aliases(&home))
    })
    .await
}

/// What `ssh -G` makes of the form's server, user and port. Local only.
#[tauri::command]
pub(crate) async fn resolve_ssh(destination: String, user: String, port: String) -> Result<skiff_client::host::ssh_config::Resolved, String> {
    blocking(move || {
        let form = HostForm { name: String::new(), destination, user, port };
        let (_, target) = form.parse()?;
        skiff_client::host::ssh_config::resolve(&target, &SshOptions::default()).map_err(anyhow::Error::msg)
    })
    .await
}

/// The check list of one Host, read only on the Host. Its outcome is noted
/// for the list and the menu, and the socket it found is kept for the run.
#[tauri::command]
pub(crate) async fn check_host(id: String) -> Result<skiff_client::host::check::Report, String> {
    let h = host_by_id(&id).await?;
    let input = skiff_client::host::check::CheckInput {
        target: h.target(),
        name: h.name.clone(),
        pin: h.host_id.clone().and_then(|p| HostId::try_from(p).ok()),
        forward_dir: forward_dir(&h.id),
        home: home().ok(),
        agent: true,
        installed: memo(&id, |m| m.installed.clone()),
        version: VERSION.into(),
        build: own_build(None),
        linger_refused: memo(&id, |m| m.linger_refused),
        update_failed: memo(&id, |m| m.update_failed.clone()),
    };
    let report = skiff_client::host::check::check(&input, &SshOptions::default()).await;
    note(&id, report.ok);
    if report.platform.is_some() {
        memo(&id, |m| {
            m.skiff = report.installed.clone();
            m.other_build = report.other_build;
        });
    }
    if let Some(found) = report.socket.clone() {
        memo(&id, |m| m.socket = Some(found));
    }
    Ok(report)
}

async fn host_by_id(id: &str) -> Result<KnownHost, String> {
    let id = id.to_string();
    blocking(move || load(&path())?.find(&id).cloned().ok_or_else(|| anyhow::anyhow!("That Host is not in the list any more."))).await
}

/// This computer's home folder.
fn home() -> Result<PathBuf, String> {
    std::env::var_os("HOME").map(PathBuf::from).ok_or_else(|| "This computer gives Skiff no home folder.".into())
}

/// Host keys learned into a temp file, waiting for the person's confirm, by
/// token. The page never sends a key: only the token of one Rust scanned.
static SCANNED: std::sync::Mutex<std::collections::BTreeMap<String, (String, keys::Scanned)>> = std::sync::Mutex::new(std::collections::BTreeMap::new());

/// The host key that answers for a Host now, for the trust dialog.
#[derive(Serialize)]
pub(crate) struct HostKeyView {
    token: String,
    fingerprints: Vec<keys::Fingerprint>,
    /// What known_hosts holds for that host now. Not empty: the key changed.
    known: Vec<keys::Fingerprint>,
    /// The known_hosts file a trust writes, with `~` for home.
    file: String,
}

/// Learns the host key that answers for a Host into a temp file. Nothing
/// the person owns changes until they confirm with [`host_key_trust`].
#[tauri::command]
pub(crate) async fn host_key_scan(id: String) -> Result<HostKeyView, String> {
    let h = host_by_id(&id).await?;
    let home = home()?;
    let scanned = keys::scan_host_key(&h.target(), &SshOptions::default(), &home).await?;
    let token = new_id(&Hosts::default()).map_err(err)?;
    let file = match scanned.file.strip_prefix(&home) {
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => scanned.file.display().to_string(),
    };
    let view = HostKeyView { token: token.clone(), fingerprints: scanned.fingerprints.clone(), known: scanned.known.clone(), file };
    SCANNED.lock().unwrap().insert(token, (id, scanned));
    Ok(view)
}

/// The person confirmed the fingerprint: its line goes into known_hosts.
/// `replace`: the old keys of that host go first, as they confirmed twice.
#[tauri::command]
pub(crate) async fn host_key_trust(token: String, replace: bool) -> Result<(), String> {
    let Some((id, scanned)) = SCANNED.lock().unwrap().remove(&token) else {
        return Err("Skiff no longer holds that key. Check again.".into());
    };
    if !scanned.known.is_empty() && !replace {
        return Err("The host key changed. Review it first.".into());
    }
    keys::trust_host_key(&scanned, replace).await?;
    forget_check(&id);
    Ok(())
}

/// The key to install, or why there is none to install yet.
async fn ready_key(h: &KnownHost) -> Result<String, String> {
    match keys::local_key(&h.target(), &SshOptions::default(), &home()?, true).await? {
        keys::LocalKey::Ready(line) => Ok(line),
        keys::LocalKey::Locked(_) => Err("Your SSH key is locked. Unlock it first.".into()),
        keys::LocalKey::None => Err("This computer has no SSH key. Make one first.".into()),
    }
}

/// The Skiff version this app installs on a server: its own.
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// What Install Skiff… did, for the check list's note.
#[derive(Serialize)]
pub(crate) struct InstallView {
    #[serde(flatten)]
    outcome: skiff_client::host::install::Outcome,
    /// `release`, or `this build` (a dev build with no matching release).
    source: &'static str,
    version: &'static str,
}

/// Installs Skiff on a Linux or macOS server in the account's home, and starts its
/// skiffd. Only where no skiffd runs that Skiff did not install. The bundle
/// is the signed release bundle for this app's version; a dev build with no
/// such release sends its own binaries.
#[tauri::command]
pub(crate) async fn install_skiff(handle: tauri::AppHandle, id: String) -> Result<InstallView, String> {
    let step = steps(handle, &id);
    cancellable(&id, install_run(&id, &step)).await
}

/// The install or update that runs per Host id, so Cancel can stop it.
static RUNNING: std::sync::Mutex<std::collections::BTreeMap<String, std::sync::Arc<tokio::sync::Notify>>> =
    std::sync::Mutex::new(std::collections::BTreeMap::new());

/// Cancel in the Install or Update dialog.
#[tauri::command]
pub(crate) fn cancel_install(id: String) {
    if let Some(stop) = RUNNING.lock().unwrap().get(&id) {
        stop.notify_one();
    }
}

/// What an update did, for the check list's note and the red dialog.
#[derive(Serialize)]
pub(crate) struct UpdateView {
    #[serde(flatten)]
    updated: skiff_client::host::install::Updated,
    source: &'static str,
    version: &'static str,
}

/// Update Skiff… in the check list, after the person confirms it.
/// `reinstall`: from the banner of a Host whose Skiff lacks a part this app
/// needs, so this app's build goes in even when its build id is there.
#[tauri::command]
pub(crate) async fn update_skiff(handle: tauri::AppHandle, id: String, end_sessions: bool, reinstall: Option<bool>) -> Result<UpdateView, String> {
    let step = steps(handle, &id);
    let h = host_by_id(&id).await?;
    cancellable(&id, update_managed(&h, end_sessions, reinstall.unwrap_or(false), &step)).await
}

/// Turns the automatic update of one Host on or off.
#[tauri::command]
pub(crate) async fn set_host_auto_update(id: String, on: bool) -> Result<(), String> {
    blocking(move || {
        update(&path(), |hosts| {
            known(hosts, &id)?.auto_update = on;
            Ok(())
        })
    })
    .await
}

/// Starts the managed skiffd and skiff-host on a server, each if it does
/// not run. Never one Skiff did not install.
#[tauri::command]
pub(crate) async fn start_skiff(id: String) -> Result<skiff_client::host::install::Outcome, String> {
    let h = host_by_id(&id).await?;
    let p = probe_now(&h).await?;
    if p.managed.is_none() {
        return Err(format!("Skiff did not install Skiff on {}, so it does not start it.", h.name));
    }
    let out = skiff_client::host::install::start(&h.target(), &SshOptions::default()).await?;
    if out.linger == "refused" {
        memo(&id, |m| m.linger_refused = true);
    }
    if !out.running {
        return Err(format!("skiffd on {} stops right after it starts.", h.name));
    }
    if !out.host_running {
        return Err(format!("skiff-host on {} stops right after it starts. Sessions work.", h.name));
    }
    Ok(out)
}

/// Turns on lingering for the account on a server, so its user services
/// start at boot and stay after logout. False: the server refused it, and
/// the check list says so in plain words.
#[tauri::command]
pub(crate) async fn turn_on_linger(id: String) -> Result<bool, String> {
    let h = host_by_id(&id).await?;
    let on = skiff_client::host::install::linger(&h.target(), &SshOptions::default()).await?;
    memo(&id, |m| m.linger_refused = !on);
    Ok(on)
}

/// Makes `~/.ssh/id_ed25519` with no passphrase, when it is not there.
#[tauri::command]
pub(crate) async fn make_key() -> Result<(), String> {
    let user = std::env::var("USER").unwrap_or_default();
    keys::make_key(&home()?, &format!("{user}@{} (Skiff)", local_hostname())).await.map(drop)
}

/// Gives the locked key ssh would use for this Host to the agent.
#[tauri::command]
pub(crate) async fn unlock_key(id: String, passphrase: String) -> Result<(), String> {
    let h = host_by_id(&id).await?;
    match keys::local_key(&h.target(), &SshOptions::default(), &home()?, true).await? {
        keys::LocalKey::Locked(key) => keys::unlock_key(&key, passphrase).await,
        _ => Ok(()),
    }
}

/// This computer's public key for this Host, for Copy public key.
#[tauri::command]
pub(crate) async fn public_key(id: String) -> Result<String, String> {
    ready_key(&host_by_id(&id).await?).await
}

/// Adds this computer's key to the Host's `authorized_keys`, once, after one
/// password entry. The password goes to ssh through askpass only.
#[tauri::command]
pub(crate) async fn install_key(id: String, password: String) -> Result<keys::Installed, String> {
    let h = host_by_id(&id).await?;
    let key = ready_key(&h).await?;
    match keys::install_key(&h.target(), &SshOptions::default(), keys::BY_PASSWORD, &key, password).await {
        Ok(done) => {
            memo(&id, |m| m.installed = Some(done.clone()));
            Ok(done)
        }
        Err(keys::StepError::Refused) => Err(format!("The password did not work. Nothing changed on {}.", h.name)),
        Err(keys::StepError::Other(e)) => Err(e),
    }
}

/// Puts the Host's foreign-owned `authorized_keys` aside and writes a new
/// one with this computer's key, after one password entry.
#[tauri::command]
pub(crate) async fn fix_permissions(id: String, password: String) -> Result<(), String> {
    let h = host_by_id(&id).await?;
    let key = ready_key(&h).await?;
    match keys::fix_key_file(&h.target(), &SshOptions::default(), keys::BY_PASSWORD, &key, password).await {
        Ok(()) => {
            memo(&id, |m| m.installed = None);
            Ok(())
        }
        Err(keys::StepError::Refused) => Err(format!("The password did not work. Nothing changed on {}.", h.name)),
        Err(keys::StepError::Other(e)) => Err(e),
    }
}

/// Shows another Host in this window: the old connection, its streams and
/// any held input go first, so nothing typed for one Host reaches another.
/// The page reloads after it. `None`: this Mac.
#[tauri::command]
pub(crate) async fn select_host(handle: tauri::AppHandle, app: State<'_, App>, id: Option<String>) -> Result<(), String> {
    let selection = blocking(move || {
        update(&path(), |hosts| {
            if let Some(id) = &id {
                let h = hosts.find(id).ok_or_else(|| anyhow::anyhow!("That Host is not in the list any more."))?;
                remote(h).map_err(anyhow::Error::msg)?;
            }
            hosts.selected = id;
            Ok(selected(&hosts, crate::connection::local_selection))
        })
    })
    .await?;
    crate::connection::replace_selection(&app, selection).await;
    // A launch on a remote Host skipped this Mac's Skipper setup.
    if !app.remote() {
        let handle = handle.clone();
        std::thread::spawn(move || crate::skiff_agent::sync(&handle.state::<App>()));
    }
    Ok(())
}

/// Allows or stops OSC 52 clipboard writes from a remote Host's terminals.
/// Allowed for the identity this window's live connection accepted, which
/// must be the one the Host must report, so a Host that answers with another
/// identity later has no permission.
#[tauri::command]
pub(crate) async fn set_host_clipboard(app: State<'_, App>, id: String, allow: bool) -> Result<(), String> {
    let shown = shown_id(&app).as_deref() == Some(id.as_str());
    let live = app.live_identity().filter(|_| shown);
    blocking(move || set_clipboard(&path(), &id, live.as_ref(), allow)).await
}

/// The person accepts the new identity the shown Host reported. Only the one
/// Rust saw and refused can be accepted, not one the page names. The page
/// reloads after it.
#[tauri::command]
pub(crate) async fn accept_host_identity(app: State<'_, App>, id: String) -> Result<(), String> {
    let changed = app.changed.lock().unwrap().clone();
    if shown_id(&app).as_deref() != Some(id.as_str()) {
        return Err("This window shows another Host now.".into());
    }
    let Some((_, received)) = changed.filter(|(d, _)| app.selection().ssh_target().is_some_and(|t| t.destination == *d)) else {
        return Err("This Host reported no new identity to accept.".into());
    };
    let selection = blocking(move || {
        let hosts = accept_identity(&path(), &id, &received)?;
        Ok(selected(&hosts, crate::connection::local_selection))
    })
    .await?;
    crate::connection::replace_selection(&app, selection).await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_host_dtos_keep_their_json_contracts() {
        let form: HostForm = serde_json::from_value(serde_json::json!({
            "destination": "devbox"
        }))
        .unwrap();
        assert_eq!(form.name, "");
        assert_eq!(form.user, "");
        assert_eq!(form.port, "");

        let list = host_list(
            Hosts { selected: Some("h_devbox".into()), hosts: vec![KnownHost::new("devbox".into())] },
            Some("h_devbox".into()),
            None,
        );
        let json = serde_json::to_value(list).unwrap();
        assert_eq!(json["selected"], "h_devbox");
        assert_eq!(json["clipboard"], false);
        assert!(json["hosts"][0].get("clipboard").is_none(), "per-Host clipboard permission is not page data");
        assert!(json["hosts"][0].get("auto_update").is_some());
    }

    fn dev() -> SshTarget {
        SshTarget::new("devbox", "/s.sock")
    }

    #[test]
    fn hosts_round_trip_and_an_unknown_selection_is_this_mac() {
        let dir = std::env::temp_dir().join(format!("skiff-hosts-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let file = dir.join("hosts.toml");
        assert_eq!(load(&file).unwrap(), Hosts::default());
        let hosts = Hosts {
            selected: Some("h_devbox".into()),
            hosts: vec![KnownHost::new("devbox".into())],
        };
        save(&file, &hosts).unwrap();
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(text.contains("selected = \"h_devbox\"") && text.contains("[[host]]"), "{text}");
        assert_eq!(load(&file).unwrap(), hosts);
        let local = || HostSelection::new("/l.sock".into(), "/id".into());
        let picked = selected(&hosts, local);
        assert_eq!(picked.ssh_target().map(|t| t.destination.as_str()), Some("devbox"));
        assert!(picked.process_control().is_none());
        // Room under the forwards' directory for `<pid>-<n>/skiffd.sock`.
        assert!(picked.socket().as_os_str().len() + 32 < 104, "{}", picked.socket().display());
        assert_eq!(picked.pinned(), None);
        let gone = Hosts { selected: Some("h_gone".into()), ..hosts.clone() };
        assert!(selected(&gone, local).is_local());
        let bad = Hosts { hosts: vec![KnownHost { destination: "-oProxyCommand=x".into(), ..hosts.hosts[0].clone() }], selected: Some("h_devbox".into()) };
        assert!(selected(&bad, local).is_local(), "an invalid entry never becomes an endpoint");
        // A selection whose Host was removed by hand is cleared on the next read.
        assert_eq!(forget_removed(&file).unwrap(), hosts);
        save(&file, &gone).unwrap();
        assert_eq!(forget_removed(&file).unwrap(), Hosts { selected: None, ..hosts.clone() });
        assert_eq!(load(&file).unwrap().selected, None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_form_keeps_id_name_user_and_port_and_refuses_duplicates() {
        let form = |name: &str, d: &str, user: &str, port: &str| HostForm { name: name.into(), destination: d.into(), user: user.into(), port: port.into() };
        let (name, t) = form("", "jose@devbox.lan", "", "").parse().unwrap();
        assert_eq!((name.as_str(), t.user, t.port), ("devbox.lan", None, None));
        let (name, t) = form(" Dev ", "devbox", "me", "2222").parse().unwrap();
        assert_eq!((name.as_str(), t.user.as_deref(), t.port), ("Dev", Some("me"), Some(2222)));
        for (f, want) in [
            (form("", "", "", ""), "Pick a server"),
            (form("", "-oProxyCommand=x", "", ""), "Type one name"),
            (form("", "dev box", "", ""), "Type one name"),
            (form("", "devbox", "", "0"), "Type a port"),
            (form("", "devbox", "", "70000"), "Type a port"),
            (form("", "devbox", "-l", ""), "Type one user"),
        ] {
            let e = f.parse().unwrap_err().to_string();
            assert!(e.starts_with(want), "{e}");
        }
        let mut h = KnownHost::new("devbox".into());
        h.user = Some("me".into());
        h.port = Some(2222);
        let hosts = Hosts { selected: Some(h.id.clone()), hosts: vec![h.clone()] };
        let text = toml::to_string(&hosts).unwrap();
        assert!(text.contains("id = \"h_devbox\"") && text.contains("user = \"me\"") && text.contains("port = 2222"), "{text}");
        assert_eq!(toml::from_str::<Hosts>(&text).unwrap(), hosts);
        // The same login is one Host, whatever its name. Another user is another Host.
        assert!(check_unique(&hosts, "x", &h.target(), None).unwrap_err().to_string().contains("in the list already"));
        assert!(check_unique(&hosts, "x", &h.target(), Some("h_devbox")).is_ok());
        assert!(check_unique(&hosts, "DEVBOX", &SshTarget::new("devbox", "/s.sock"), None).unwrap_err().to_string().contains("this name"));
        assert!(check_unique(&hosts, "other", &SshTarget::new("devbox", "/s.sock"), None).is_ok());
        // No socket is stored: one Host has one skiffd, and the probe finds
        // its socket. A line from an unreleased build is dropped on the next write.
        let old = "[[host]]\nid = \"h_p0p\"\nname = \"p0p-deb\"\ndestination = \"p0p-deb\"\nsocket = \"/run/user/1000/skiff/skiffd.sock\"\n";
        let text = toml::to_string(&toml::from_str::<Hosts>(old).unwrap()).unwrap();
        assert!(!text.contains("socket"), "{text}");
        // A file without an id is not a shape any release wrote: it does not load.
        assert!(toml::from_str::<Hosts>("[[host]]\ndestination = \"d\"\nsocket = \"/s\"\n").is_err());
    }

    /// Review P1: accept A, then B at the same endpoint. A new selection from
    /// the file, as after picking another Host or restarting, still refuses B
    /// and A's clipboard permission does not pass to B.
    #[test]
    fn the_first_identity_is_pinned_and_only_the_person_changes_it() {
        let dir = std::env::temp_dir().join(format!("skiff-hosts-pin-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let file = dir.join("hosts.toml");
        let id = |s: &str| HostId::try_from(s.to_string()).unwrap();
        save(&file, &Hosts { selected: Some("h_devbox".into()), hosts: vec![KnownHost::new("devbox".into())] }).unwrap();
        pin_first(&file, &dev(), &id("A")).unwrap();
        pin_first(&file, &dev(), &id("A")).unwrap();
        let changed = pin_first(&file, &dev(), &id("B")).unwrap_err();
        assert!(matches!(changed.downcast_ref(), Some(skiff_host_api::Error::IdentityChanged { .. })), "{changed:#}");
        let hosts = load(&file).unwrap();
        assert_eq!(hosts.hosts[0].host_id.as_deref(), Some("A"));
        let local = || HostSelection::new("/l.sock".into(), "/id".into());
        assert_eq!(selected(&hosts, local).pinned(), Some(id("A")));

        // Clipboard needs a live identity, and is given to that one only.
        assert!(set_clipboard(&file, "h_devbox", None, true).is_err());
        set_clipboard(&file, "h_devbox", Some(&id("A")), true).unwrap();
        assert!(load(&file).unwrap().hosts[0].clipboard_for(&id("A")));
        let accepted = accept_identity(&file, "h_devbox", &id("B")).unwrap();
        assert_eq!(accepted.hosts[0].host_id.as_deref(), Some("B"));
        assert!(!accepted.hosts[0].clipboard_for(&id("B")), "the old identity's permission is gone");
        assert_eq!(selected(&load(&file).unwrap(), local).pinned(), Some(id("B")));
        // A file whose permission names another identity allows nothing.
        let foreign = KnownHost { host_id: Some("B".into()), clipboard_host: Some("A".into()), ..KnownHost::new("x".into()) };
        assert!(!foreign.clipboard_for(&id("A")) && !foreign.clipboard_for(&id("B")));
        // The old bool key from before pinning is ignored, not trusted.
        std::fs::write(&file, "[[host]]\nid = \"h_devbox\"\nname = \"devbox\"\ndestination = \"devbox\"\nsocket = \"/s.sock\"\nclipboard = true\n").unwrap();
        assert!(!load(&file).unwrap().hosts[0].clipboard_for(&id("A")));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Residual P2: window 1 stays connected to B with no permission; window
    /// 2, another app run, accepts C at the same destination and allows C.
    /// Window 1's page reloads (its live B connection survives): neither the
    /// list nor a write allows B, and window 1 cannot grant B or C.
    #[test]
    fn clipboard_follows_the_live_identity_not_another_windows_grant() {
        let dir = std::env::temp_dir().join(format!("skiff-hosts-clip-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let file = dir.join("hosts.toml");
        let id = |s: &str| HostId::try_from(s.to_string()).unwrap();
        update(&file, |h| {
            h.selected = Some("h_devbox".into());
            h.hosts.push(KnownHost::new("devbox".into()));
            Ok(())
        })
        .unwrap();
        pin_first(&file, &dev(), &id("B")).unwrap();
        let live_b = id("B");
        // Window 2.
        accept_identity(&file, "h_devbox", &id("C")).unwrap();
        set_clipboard(&file, "h_devbox", Some(&id("C")), true).unwrap();
        // Window 1 after its page reloads: its live identity is still B.
        let list = host_list(load(&file).unwrap(), Some("h_devbox".into()), Some(&live_b));
        assert!(!list.clipboard, "C's grant allowed B's writes in the list");
        assert!(!clipboard_allowed(&file, &dev(), &live_b).unwrap(), "C's grant allowed a write from B");
        assert!(set_clipboard(&file, "h_devbox", Some(&live_b), true).is_err(), "window 1 granted while connected to B");
        assert_eq!(load(&file).unwrap().hosts[0].clipboard_host.as_deref(), Some("C"), "window 1 changed C's grant");
        // Window 2, live on C, may write.
        assert!(host_list(load(&file).unwrap(), Some("h_devbox".into()), Some(&id("C"))).clipboard);
        assert!(clipboard_allowed(&file, &dev(), &id("C")).unwrap());
        // The page's list carries no per-Host permission to fall back on.
        let json = serde_json::to_value(host_list(load(&file).unwrap(), Some("h_devbox".into()), Some(&live_b))).unwrap();
        assert!(json["hosts"][0].get("clipboard").is_none(), "{json}");
        // No identity yet: nothing allowed.
        assert!(!host_list(load(&file).unwrap(), Some("h_devbox".into()), None).clipboard);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// One store step in a child process of this test binary, released by
    /// the parent's `go` file at the same time as the others. Run only by
    /// the tests below.
    #[test]
    #[ignore]
    fn store_child() {
        let Ok(spec) = std::env::var("SKIFF_HOSTS_CHILD") else { return };
        let [file, dir, role]: [&str; 3] = spec.splitn(3, '|').collect::<Vec<_>>().try_into().unwrap();
        let (file, dir) = (Path::new(file), Path::new(dir));
        let id = |s: &str| HostId::try_from(s.to_string()).unwrap();
        std::fs::write(dir.join(format!("ready-{role}")), "").unwrap();
        while !dir.join("go").exists() {
            std::thread::sleep(std::time::Duration::from_micros(200));
        }
        let result = match role.split_once(':').unwrap() {
            ("pin", h) => pin_first(file, &dev(), &id(h)).map(drop),
            ("accept", h) => accept_identity(file, "h_devbox", &id(h)).map(drop),
            ("add", d) => update(file, |hosts| {
                hosts.hosts.push(KnownHost::new(d.into()));
                Ok(())
            }),
            _ => unreachable!(),
        };
        let out = match result {
            Ok(()) => "ok".to_string(),
            Err(e) if matches!(e.downcast_ref(), Some(skiff_host_api::Error::IdentityChanged { .. })) => "changed".into(),
            Err(e) => format!("error {e:#}"),
        };
        std::fs::write(dir.join(format!("out-{role}")), out).unwrap();
    }

    /// Runs `roles` in separate processes against one file, all released at
    /// once, and returns what each reported.
    fn race(name: &str, trial: usize, roles: &[&str]) -> (PathBuf, Vec<String>) {
        let dir = std::env::temp_dir().join(format!("skiff-hosts-race-{name}-{}-{trial}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("hosts.toml");
        update(&file, |h| {
            h.hosts.push(KnownHost::new("devbox".into()));
            Ok(())
        })
        .unwrap();
        let exe = std::env::current_exe().unwrap();
        let children: Vec<_> = roles
            .iter()
            .map(|role| {
                std::process::Command::new(&exe)
                    .args(["--exact", "hosts::tests::store_child", "--ignored", "--test-threads=1"])
                    .env("SKIFF_HOSTS_CHILD", format!("{}|{}|{role}", file.display(), dir.display()))
                    .stdout(std::process::Stdio::null())
                    .spawn()
                    .unwrap()
            })
            .collect();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while !roles.iter().all(|r| dir.join(format!("ready-{r}")).exists()) {
            assert!(std::time::Instant::now() < deadline, "children never got ready");
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        std::fs::write(dir.join("go"), "").unwrap();
        for mut c in children {
            assert!(c.wait().unwrap().success());
        }
        let outs = roles.iter().map(|r| std::fs::read_to_string(dir.join(format!("out-{r}"))).unwrap()).collect();
        (dir, outs)
    }

    /// Residual P1: two app runs pin different first identities at once.
    /// Exactly one wins, the other is refused as a changed identity, and the
    /// file holds the winner's pin.
    #[test]
    fn a_first_pin_is_one_compare_and_set_across_processes() {
        for trial in 0..12 {
            let (dir, outs) = race("pin", trial, &["pin:A", "pin:B"]);
            let pin = load(&dir.join("hosts.toml")).unwrap().hosts[0].host_id.clone().unwrap();
            let winner = if pin == "A" { 0 } else { 1 };
            assert_eq!(outs[winner], "ok", "trial {trial}: {outs:?}, pin {pin}");
            assert_eq!(outs[1 - winner], "changed", "trial {trial}: both stored a first pin: {outs:?}, pin {pin}");
            std::fs::remove_dir_all(&dir).unwrap();
        }
    }

    /// Residual P1: a first pin and the person's accepted identity at once,
    /// beside other writers. The accepted identity is never overwritten by
    /// a first pin, and no writer's change is lost.
    #[test]
    fn identity_acceptance_and_other_writers_lose_no_update_across_processes() {
        for trial in 0..8 {
            let (dir, outs) = race("accept", trial, &["pin:A", "accept:C", "add:x", "add:y"]);
            let hosts = load(&dir.join("hosts.toml")).unwrap();
            assert_eq!(hosts.find("h_devbox").unwrap().host_id.as_deref(), Some("C"), "trial {trial}: {outs:?}");
            assert!(outs[0] == "ok" || outs[0] == "changed", "trial {trial}: {outs:?}");
            assert!(outs[1..].iter().all(|o| o == "ok"), "trial {trial}: {outs:?}");
            assert!(hosts.find("h_x").is_some() && hosts.find("h_y").is_some(), "trial {trial}: an added Host was lost");
            std::fs::remove_dir_all(&dir).unwrap();
        }
    }
}
