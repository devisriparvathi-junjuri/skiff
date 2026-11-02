//! The Host list the page shows: per-run probe memo, states and the file watch.

use super::*;

/// A known Host as the page sees it.
#[derive(Serialize)]
pub(super) struct HostView {
    id: String,
    name: String,
    destination: String,
    user: Option<String>,
    port: Option<u16>,
    auto_update: bool,
    /// From the last check or connection in this app run: `ok`,
    /// `attention`, or `unknown` before any.
    state: &'static str,
    /// The Skiff version the managed install runs, from the last probe in
    /// this app run, as the Skiff row compares it. None: not known.
    skiff: Option<String>,
    /// That version is older than this app's, or another build of it:
    /// Update Skiff… is offered.
    update: bool,
}

/// What this app run learned about one Host. Nothing here is stored.
#[derive(Default)]
pub(super) struct Memo {
    /// The last check or connection passed. None: no check yet, or a step
    /// since then changed the server or the keys, so the list says so.
    pub(super) ok: Option<bool>,
    /// What the last key install found on the server, for the check that
    /// follows it (Fix permissions).
    pub(super) installed: Option<keys::Installed>,
    /// The last Turn on (linger) was refused: it needs an admin.
    pub(super) linger_refused: bool,
    /// Why the last update did not finish, for the Skiff row.
    pub(super) update_failed: Option<String>,
    /// The managed install is this app's version in another build.
    pub(super) other_build: bool,
    /// What the connect learned about the install, and when it looked. A
    /// Host that could not be probed is tried again after [`ARRIVAL_RETRY`].
    pub(super) arrival: Option<(Option<Arrival>, std::time::Instant)>,
    /// The managed install's Skiff version from the last probe.
    pub(super) skiff: Option<String>,
    /// skiffd's socket there, from the last probe: `<runtime dir>/skiff/skiffd.sock`
    /// on that Host. The forward goes there. Never stored in `hosts.toml`.
    pub(super) socket: Option<String>,
}

pub(super) static MEMO: std::sync::Mutex<std::collections::BTreeMap<String, Memo>> = std::sync::Mutex::new(std::collections::BTreeMap::new());

/// Reads or changes what this app run knows about Host `id`.
pub(super) fn memo<T>(id: &str, f: impl FnOnce(&mut Memo) -> T) -> T {
    f(MEMO.lock().unwrap().entry(id.to_string()).or_default())
}

/// Notes how the last check or connection of a Host went, for the list and
/// the menu.
pub(crate) fn note(id: &str, ok: bool) {
    memo(id, |m| m.ok = Some(ok));
}

/// A step changed the server or the keys: the next list says "Not checked".
pub(super) fn forget_check(id: &str) {
    memo(id, |m| m.ok = None);
}

/// As [`note`], for the Host a live selection's target logs in to.
pub(crate) fn note_login(target: &SshTarget, ok: bool) {
    if let Some(h) = load(&path()).ok().and_then(|hosts| hosts.find_login(target).cloned()) {
        note(&h.id, ok);
    }
}

pub(super) fn state(h: &KnownHost) -> &'static str {
    match memo(&h.id, |m| m.ok) {
        Some(true) => "ok",
        Some(false) => "attention",
        None => "unknown",
    }
}

/// What the page shows and offers: the selected Host, and the known ones.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct HostList {
    /// The shown Host's id. None: this Mac.
    pub(super) selected: Option<String>,
    /// This computer's hostname, for the local Host's label. Empty if the
    /// system gives none.
    local_name: String,
    pub(super) hosts: Vec<HostView>,
    /// The shown remote Host's clipboard permission, for the menu: given to
    /// the identity this window accepted. Each write asks `host_clipboard`.
    pub(super) clipboard: bool,
    /// This window shows a remote Host that is not in the list any more:
    /// its destination. The page then picks this computer.
    pub(super) removed: Option<String>,
}

/// `identity`: the one this window accepted, live or pinned.
pub(super) fn host_list(hosts: Hosts, selected: Option<String>, identity: Option<&HostId>) -> HostList {
    let clipboard = match (&selected, identity) {
        (Some(d), Some(id)) => hosts.find(d).is_some_and(|h| h.clipboard_for(id)),
        _ => false,
    };
    let hosts = hosts
        .hosts
        .into_iter()
        .map(|h| {
            let skiff = memo(&h.id, |m| m.skiff.clone());
            let update = skiff.as_deref().is_some_and(|v| skiff_client::host::check::newer(VERSION, v)) || memo(&h.id, |m| m.other_build);
            HostView { state: state(&h), id: h.id, name: h.name, destination: h.destination, user: h.user, port: h.port, auto_update: h.auto_update, skiff, update }
        })
        .collect();
    HostList { selected, local_name: local_hostname(), hosts, clipboard, removed: None }
}

/// The short hostname (`thr0w-deb`, not `thr0w-deb.local`), or empty.
pub(super) fn local_hostname() -> String {
    let mut buf = [0u8; 256];
    // SAFETY: the buffer and its length are valid; gethostname writes at most that.
    if unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) } != 0 {
        return String::new();
    }
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    let name = String::from_utf8_lossy(&buf[..end]);
    name.split('.').next().unwrap_or_default().trim().to_string()
}

/// The file, with a selection that names a Host no longer in the list
/// cleared: an edit by hand that removed it does not leave the next app
/// run pointing at it.
pub(super) fn forget_removed(path: &Path) -> anyhow::Result<Hosts> {
    let hosts = load(path)?;
    if hosts.selected.as_deref().is_none_or(|d| hosts.find(d).is_some()) {
        return Ok(hosts);
    }
    update(path, |hosts| {
        if hosts.selected.as_deref().is_some_and(|d| hosts.find(d).is_none()) {
            hosts.selected = None;
        }
        Ok(hosts.clone())
    })
}

/// Tells the page when `hosts.toml` changes, by this app or by hand, so
/// the Host menu and Settings › Hosts follow it, and a shown Host that was
/// removed gives way to this computer.
pub(crate) fn watch(app: tauri::AppHandle) {
    use tauri::Emitter;
    std::thread::spawn(move || {
        // The file is small: its text is the stamp.
        let stamp = || std::fs::read_to_string(path()).ok();
        let mut last = stamp();
        loop {
            std::thread::sleep(std::time::Duration::from_millis(1500));
            let now = stamp();
            if now != last {
                last = now;
                let _ = app.emit("skiff:hosts-changed", ());
            }
        }
    });
}

/// The shown Host's id, as the file names it now. None: this Mac.
pub(super) fn shown_id(app: &App) -> Option<String> {
    let t = app.selection().ssh_target()?.clone();
    load(&path()).ok()?.find_login(&t).map(|h| h.id.clone())
}
