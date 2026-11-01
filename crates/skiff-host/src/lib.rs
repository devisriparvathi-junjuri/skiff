//! `skiff-host`: the Host's data-plane service (M4). It speaks the Host
//! envelope on its own socket beside skiffd's and answers projects, Git
//! status, diff text, worktree files (M5), project backgrounds, and
//! Skipper's ledger, results and settings (M6) from the Host's own config,
//! disk and git, the account's folders for the picker (`folders@1`), and is
//! the writer of the Host's `[[project]]` tables, `[agents]`, `[skiff_agent]`
//! and project worktrees, the starter of Skipper's session there and the
//! keeper of its watcher ([`skipper`]), through
//! the same `skiff-core` code the app ran before. It holds no PTY and no session:
//! restarting it never touches skiffd or a terminal (#62, M1f topology).
//!
//! Identity: the same durable `host_id` store skiffd reads, beside the
//! selected `projects.toml`, so both endpoints of one Host agree. Its
//! `instance_id` is its own and changes on every start.
pub mod folders;
pub mod skipper;

use anyhow::{bail, Context};
use std::{
    collections::BTreeMap,
    path::Path,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};

use skiff_core::{config::{self, ProjectConfig}, files, git, project, session::SessionInfo};
use skiff_host_api::{
    Capability, ChangeStatus, EntryKind, Envelope, Error, FileContent, Hello, Host, HostChange, HostDiff, HostEntry,
    HostFile, HostBranch, HostBranches, HostFolder, HostId, HostImage, HostLedgerRow, HostListing, HostProject, HostWorktree, InstanceId, Limits, Message,
    ProjectEdit, ReadPurpose, Request, Response, API_MAJOR,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{UnixListener, UnixStream},
    sync::{mpsc, OwnedSemaphorePermit, Semaphore},
    task::JoinSet,
};

/// The largest frame either side sends, newline included.
pub const MAX_FRAME: usize = 8 * 1024 * 1024;
/// Requests one connection may have admitted at once, until each reply is
/// written. A client that sends more waits: the service reads no further.
pub const MAX_IN_FLIGHT: usize = 16;

/// What this service offers.
pub fn capabilities() -> Vec<Capability> {
    vec![
        Capability::projects(),
        Capability::git_status(),
        Capability::git_diff(),
        Capability::project_config(),
        Capability::worktrees(),
        Capability::files(),
        Capability::project_assets(),
        Capability::folders(),
        Capability::ledger_read(),
        Capability::skipper_settings(),
        Capability::skipper_config(),
        Capability::skipper_file(),
        Capability::skipper_run(),
        Capability::skipper_run_fleets(),
        Capability::agent_config(),
        Capability::drops(),
    ]
}

/// This run's Hello: the Host's durable id from the store beside the
/// selected `projects.toml`, and a new instance id. Without an id the
/// service does not start: a data plane that cannot say which Host it is
/// must never be paired with one.
pub fn hello() -> anyhow::Result<Hello> {
    let host_id = skiff_core::host_identity::load(&skiff_core::host_identity::path())?;
    Ok(Hello {
        host: Host {
            host_id: HostId::try_from(host_id).map_err(anyhow::Error::msg)?,
            os: std::env::consts::OS.into(),
            account: skiff_core::host_identity::account(),
        },
        instance_id: InstanceId::try_from(uuid::Uuid::new_v4().to_string()).map_err(anyhow::Error::msg)?,
        service_versions: BTreeMap::from([("skiff-host".into(), version())]),
        api_major: API_MAJOR,
        capabilities: capabilities(),
        limits: Limits { max_frame_bytes: MAX_FRAME, max_in_flight: MAX_IN_FLIGHT },
    })
}

/// This service's version, with its build as SemVer build metadata
/// (`0.6.26+<build id>`): the app replaces a service of its own version
/// from another build.
fn version() -> String {
    match std::env::current_exe().ok().as_deref().and_then(skiff_client::skipper::build_of) {
        Some(build) => format!("{}+{build}", skiff_core::VERSION),
        None => skiff_core::VERSION.into(),
    }
}

/// The socket file this service bound. Only it is ever removed:
/// [`BoundSocket::release`] checks that the path still names it.
pub struct BoundSocket {
    path: std::path::PathBuf,
    id: (u64, u64),
}

/// Binds the service socket at `path`. Only a folder it creates gets locked
/// down (`0700`); the socket is `0600`. An entry already at `path` is:
/// - a live socket: another service, so this one does not start
/// - a stale socket of this account: a crash's leftover, removed
/// - anything else (a file, a link, a folder, another account's socket):
///   refused and left as it is
pub async fn bind(path: &Path) -> anyhow::Result<(UnixListener, BoundSocket)> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
    if let Some(dir) = path.parent().filter(|d| !d.exists()) {
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    match std::fs::symlink_metadata(path) {
        Ok(found) => {
            if !found.file_type().is_socket() {
                bail!("{} exists and is not a socket; skiff-host leaves it as it is", path.display());
            }
            // SAFETY: geteuid takes no arguments and cannot fail.
            if found.uid() != unsafe { libc::geteuid() } {
                bail!("{} belongs to another account; skiff-host leaves it as it is", path.display());
            }
            if UnixStream::connect(path).await.is_ok() {
                bail!("skiff-host is already running at {}", path.display());
            }
            // Still the same stale socket: nothing replaced it meanwhile.
            let again = std::fs::symlink_metadata(path)?;
            if (again.dev(), again.ino()) != (found.dev(), found.ino()) {
                bail!("{} changed while skiff-host checked it", path.display());
            }
            std::fs::remove_file(path)?;
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e).with_context(|| format!("check {}", path.display())),
    }
    let listener = UnixListener::bind(path).with_context(|| format!("bind {}", path.display()))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    let meta = std::fs::symlink_metadata(path)?;
    Ok((listener, BoundSocket { path: path.to_path_buf(), id: (meta.dev(), meta.ino()) }))
}

impl BoundSocket {
    /// Removes the socket file only if the path still names the socket this
    /// service bound: never a newer service's socket, nor anything else.
    pub fn release(self) {
        use std::os::unix::fs::{FileTypeExt, MetadataExt};
        if let Ok(m) = std::fs::symlink_metadata(&self.path) {
            // The inode alone is not proof: ext4 gives a newer socket at this
            // path the freed number. This service no longer listens, so a
            // listener that answers is another one's.
            if m.file_type().is_socket()
                && (m.dev(), m.ino()) == self.id
                && std::os::unix::net::UnixStream::connect(&self.path).is_err()
            {
                let _ = std::fs::remove_file(&self.path);
            }
        }
    }
}

/// Accepts connections until the listener fails. Each one is served on its
/// own; dropping this future closes them all, as a stopped service does.
pub async fn serve(listener: UnixListener, hello: Arc<Hello>) -> std::io::Result<()> {
    let mut connections = tokio::task::JoinSet::new();
    // Skipper's watcher is this service's to keep, as long as it runs.
    connections.spawn(skipper::supervise());
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (stream, _) = accepted?;
                connections.spawn(handle(stream, hello.clone()));
            }
            Some(_) = connections.join_next() => {}
        }
    }
}

/// Requests admitted and not yet written back or abandoned, over every
/// connection of this process. For diagnostics and tests.
pub fn admitted() -> usize {
    ADMITTED.load(Ordering::Relaxed)
}
static ADMITTED: AtomicUsize = AtomicUsize::new(0);

/// Set when the app started this service for This Mac. Then a ledger result
/// may also sit anywhere under the person's home folder. A service reached
/// over SSH keeps the ledger folder only.
static THIS_MAC: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Says whether this service is This Mac's own. Off by default.
pub fn set_this_mac(on: bool) {
    THIS_MAC.store(on, Ordering::Relaxed);
}

/// One admitted request, from its frame until its reply is written. Its
/// permit bounds what a connection holds: work, encoded replies and the
/// reply being written, together.
struct Admission(#[allow(dead_code)] OwnedSemaphorePermit);
impl Admission {
    fn new(permit: OwnedSemaphorePermit) -> Self {
        ADMITTED.fetch_add(1, Ordering::Relaxed);
        Self(permit)
    }
}
impl Drop for Admission {
    fn drop(&mut self) {
        ADMITTED.fetch_sub(1, Ordering::Relaxed);
    }
}

/// One client: newline-delimited envelopes in, replies out, in any order.
///
/// At most [`MAX_IN_FLIGHT`] requests are admitted at once, and one stays
/// admitted until its reply is written. A client that stops reading stalls
/// at that bound: no more frames are read, so nothing more is computed or
/// queued for it. The work belongs to the connection: when the client is
/// gone, or a frame is unreadable or oversize, what is left is dropped.
/// Nothing is retried.
pub async fn handle(stream: UnixStream, hello: Arc<Hello>) {
    let (rd, mut wr) = stream.into_split();
    let slots = Arc::new(Semaphore::new(MAX_IN_FLIGHT));
    // Never full: every queued reply holds one of the permits.
    let (tx, mut rx) = mpsc::channel::<(Vec<u8>, Admission)>(MAX_IN_FLIGHT);
    let writer = async move {
        while let Some((line, admission)) = rx.recv().await {
            if wr.write_all(&line).await.is_err() {
                return;
            }
            drop(admission);
        }
    };
    let reader = async move {
        let mut work = JoinSet::new();
        let mut reader = BufReader::new(rd);
        loop {
            // Finished requests leave the set; a read is never interrupted.
            while work.try_join_next().is_some() {}
            let mut line = Vec::new();
            let read = (&mut reader).take(MAX_FRAME as u64 + 1).read_until(b'\n', &mut line).await;
            match read {
                // The client is done sending: finish what it asked for.
                Ok(0) => break,
                Err(_) => return,
                Ok(_) if line.len() > MAX_FRAME || line.last() != Some(&b'\n') => return,
                Ok(_) => {}
            }
            let Ok(Envelope { id, request }) = serde_json::from_slice::<Envelope>(&line) else {
                return;
            };
            let Ok(permit) = slots.clone().acquire_owned().await else { return };
            let admission = Admission::new(permit);
            let (tx, hello) = (tx.clone(), hello.clone());
            work.spawn(async move {
                let result = answer(request, &hello).await;
                let _ = tx.send((encode(id, result), admission)).await;
            });
        }
        while work.join_next().await.is_some() {}
    };
    tokio::pin!(writer);
    tokio::select! {
        // Every request answered and queued: the writer drains the queue.
        _ = reader => writer.await,
        // The client is gone: the reader and its work are dropped here.
        _ = &mut writer => {}
    }
}

fn encode(id: u64, result: Result<Response, Error>) -> Vec<u8> {
    let mut line = serde_json::to_vec(&Message::Reply { id, result }).unwrap_or_default();
    if line.len() >= MAX_FRAME {
        line = serde_json::to_vec(&Message::Reply {
            id,
            result: Err(Error::OperationFailed { message: "the reply is larger than a frame".into() }),
        })
        .unwrap_or_default();
    }
    line.push(b'\n');
    line
}

/// One request. A family this service does not offer is refused before it
/// runs, as the Hello says.
pub async fn answer(request: Request, hello: &Hello) -> Result<Response, Error> {
    let Some(capability) = request.capability() else {
        return Ok(Response::Hello { hello: hello.clone() });
    };
    hello.require(&capability)?;
    // The Host's own sessions, read before a worktree goes: never the client's word.
    let sessions = match &request {
        Request::WorktreeRemove { .. } => live_sessions().await?,
        _ => Vec::new(),
    };
    // The ledger's buckets need them too. Without them, a sent task counts as
    // worked on, as in `skiff ledger ls`.
    let ledger_sessions = match &request {
        Request::LedgerRows => live_sessions().await.ok(),
        _ => None,
    };
    if matches!(request, Request::FolderPlaces) {
        note_in_use().await;
    }
    if matches!(
        request,
        Request::SkipperEdit { .. }
            | Request::SkipperFileRead { .. }
            | Request::SkipperFileWrite { .. }
            | Request::SkipperFileReset { .. }
            | Request::SkipperFolderPreview { .. }
            | Request::SkipperFolderChange { .. }
            | Request::SkipperOpen { .. }
            | Request::SkipperWatch { .. }
            | Request::SkipperProject { .. }
            | Request::SkipperFleet { .. }
            | Request::FleetTakeoverPreview { .. }
            | Request::FleetTakeover { .. }
            | Request::FleetRename { .. }
            | Request::AgentConfig
            | Request::AgentEdit { .. }
    ) {
        return skipper::answer(request).await;
    }
    blocking(move || match request {
        Request::ProjectList => {
            let (rows, revision) = project::list_with_revision().map_err(failed)?;
            Ok(Response::Projects { projects: rows.into_iter().map(host_project).collect(), revision: Some(revision) })
        }
        Request::ProjectGet { name } => {
            let p = project::find(&name).map_err(refused)?;
            Ok(Response::Project { project: Box::new(host_project(project::Project::from_config(&p))) })
        }
        Request::GitChanges { worktree } => {
            // Held open before the check, so a folder swapped in after it
            // is refused rather than read.
            let root = git::Root::open(Path::new(&worktree));
            let worktree = known_worktree(&worktree)?;
            let read = git::changes_in(&root.map_err(failed)?, &worktree).map_err(failed)?;
            Ok(Response::Changes { changes: read.files.into_iter().map(host_change).collect(), base: read.base })
        }
        Request::GitDiff { worktree, file } => {
            // As for GitChanges: the folder held open before the check is
            // the one git and the untracked read must still name.
            let root = git::Root::open(Path::new(&worktree));
            let worktree = known_worktree(&worktree)?;
            if let Some(f) = file.as_deref().filter(|f| !git::is_relative_file(f)) {
                return Err(Error::NotFound { message: format!("{f} is not a file path in this worktree") });
            }
            let diff = git::diff_in(&root.map_err(failed)?, &worktree, file.as_deref()).map_err(failed)?;
            Ok(Response::Diff { diff: host_diff(diff) })
        }
        Request::FileList { worktree, dir, follow_links } => {
            // As for GitChanges: held before the check, named after it.
            let root = git::Root::open(Path::new(&worktree));
            let worktree = known_worktree(&worktree)?;
            let listing = files::list_with(&root.map_err(failed)?, &worktree, &dir, follow_links).map_err(unread)?;
            Ok(Response::Files { listing: host_listing(listing) })
        }
        Request::FileRead { worktree, file, purpose } => {
            let root = git::Root::open(Path::new(&worktree));
            let worktree = known_worktree(&worktree)?;
            let purpose = match purpose {
                ReadPurpose::Preview => files::Purpose::Preview,
                ReadPurpose::Copy => files::Purpose::Copy,
            };
            let read = files::read_in(&root.map_err(failed)?, &worktree, &file, purpose).map_err(unread)?;
            Ok(Response::File { file: Box::new(host_file(read)) })
        }
        Request::ProjectBackground { project } => {
            let p = project::find(&project).map_err(refused)?;
            let configured = p.background.as_deref().filter(|b| !b.trim().is_empty())
                .ok_or_else(|| Error::NotFound { message: format!("{project} has no background image on this Host") })?;
            // Opened again here, never through a link: not the path the row named.
            let (mime, bytes) = files::read_background(&p.path, configured).map_err(unread)?;
            Ok(Response::Image { image: HostImage { mime, bytes } })
        }
        Request::FolderHome => Ok(Response::Path { path: folders::home()? }),
        Request::FolderList { path, hidden, filter } => Ok(Response::Folders { listing: folders::list(&path, hidden, filter.as_deref())? }),
        Request::FolderPlaces => Ok(Response::Places { places: folders::places()? }),
        Request::FolderCreate { parent, name } => Ok(Response::Path { path: folders::create(&parent, &name)? }),
        Request::DropPut { name, bytes } => {
            let cfg = config::load().map_err(failed)?;
            let path = skiff_client::cache::put(&cfg, &name, &bytes).map_err(refused)?;
            Ok(Response::Path { path: path.to_string_lossy().into_owned() })
        }
        Request::LedgerRows => Ok(Response::Ledger { rows: ledger_rows(ledger_sessions.as_deref())? }),
        Request::LedgerStamp => {
            let file = skiff_client::ledger::path().map_err(failed)?;
            Ok(Response::LedgerStamp { stamp: skiff_client::ledger::stamp(&file) })
        }
        Request::LedgerResult { uid } => Ok(Response::File { file: Box::new(host_file(ledger_result(&uid)?)) }),
        Request::SkipperSettings => {
            let cfg = config::load().map_err(failed)?;
            Ok(Response::Skipper { settings: Box::new(skiff_client::skipper::settings(&cfg).map_err(failed)?) })
        }
        Request::ProjectInspect { path } => Ok(Response::Folder { folder: host_folder(config::inspect_folder(&host_folder_path(&path)?)) }),
        Request::ProjectInit { path } => {
            let path = host_folder_path(&path)?;
            if !path.is_dir() {
                return Err(Error::NotFound { message: "This folder is no longer on this Host.".into() });
            }
            git::init_repository(&path).map_err(failed)?;
            Ok(Response::Folder { folder: host_folder(config::inspect_folder(&path)) })
        }
        Request::ProjectAdd { path, revision, name, short, color } => {
            let path = host_folder_path(&path)?;
            let (added, revision) =
                config::edit_projects(&revision, || config::add_project(&path, name, short, color, None, Vec::new(), None)).map_err(refused)?;
            folders::recent::note([(added.path.to_string_lossy().into_owned(), folders::now_ms())]);
            Ok(Response::ProjectAdded {
                project: Box::new(host_project(project::Project::from_config(&added))),
                revision,
            })
        }
        Request::ProjectEdit { project, revision, edit } => {
            let p = project.as_str();
            let ((), revision) = config::edit_projects(&revision, || match &edit {
                ProjectEdit::Short { short } => config::set_project_short(p, short),
                ProjectEdit::Fleet { fleet } => config::set_project_fleet(p, fleet),
                ProjectEdit::Color { color } => config::set_project_color(p, color),
                ProjectEdit::Theme { theme } => config::set_project_theme(p, theme.as_deref()),
                ProjectEdit::Changes { on } => config::set_project_changes(p, *on),
                ProjectEdit::Files { on } => config::set_project_files(p, *on),
                ProjectEdit::BackgroundOpacity { percent } => config::set_project_background_opacity(p, *percent),
                ProjectEdit::Closed { closed } => config::set_project_closed(p, *closed),
            })
            .map_err(refused)?;
            Ok(Response::Revision { revision })
        }
        Request::ProjectArrange { revision, order, fleets } => {
            let fleets: Vec<(String, String)> = fleets.into_iter().map(|f| (f.project, f.fleet)).collect();
            let ((), revision) = config::edit_projects(&revision, || config::arrange_projects(&order, &fleets)).map_err(refused)?;
            Ok(Response::Revision { revision })
        }
        Request::ProjectRemove { project, revision } => {
            let ((), revision) = config::edit_projects(&revision, || config::remove_project(&project)).map_err(refused)?;
            Ok(Response::Revision { revision })
        }
        Request::WorktreeAdd { project, branch, base, checkout } => {
            let w = project::add_worktree(&project, &branch, base.as_deref(), checkout).map_err(refused)?;
            Ok(Response::Worktree { worktree: host_worktree(w) })
        }
        Request::BranchList { project } => {
            let (head, list) = project::branches(&project).map_err(refused)?;
            let branches = list.into_iter().map(|b| HostBranch { name: b.name, remote: b.remote, worktree: b.worktree }).collect();
            Ok(Response::Branches { branches: HostBranches { head, branches } })
        }
        Request::WorktreeRemove { project, path, force } => {
            project::remove_worktree(&project, Path::new(&path), force, &sessions).map_err(refused)?;
            Ok(Response::Ok)
        }
        _ => Err(Error::UnsupportedCapability { capability }),
    })
    .await
}

async fn blocking(f: impl FnOnce() -> Result<Response, Error> + Send + 'static) -> Result<Response, Error> {
    tokio::task::spawn_blocking(f).await.unwrap_or_else(|e| Err(failed(e)))
}

pub(crate) fn failed(e: impl std::fmt::Display) -> Error {
    Error::OperationFailed { message: format!("{e:#}") }
}

/// A refused edit as its typed error: a stale revision or a name or folder
/// already taken is `conflict`, a bad value `invalid`, an unknown project
/// `not_found`. Anything else failed while it ran.
pub(crate) fn refused(e: anyhow::Error) -> Error {
    let message = format!("{e:#}");
    if e.downcast_ref::<config::Stale>().is_some() {
        return Error::Conflict { message };
    }
    match (e.downcast_ref::<config::Refused>(), e.downcast_ref::<project::Keep>()) {
        (Some(config::Refused::Invalid(_)), _) | (_, Some(project::Keep::Main)) => Error::Invalid { message },
        (Some(config::Refused::NoProject(_)), _) | (_, Some(project::Keep::NotFound(_))) => Error::NotFound { message },
        (Some(config::Refused::Taken(_)), _) | (_, Some(project::Keep::Live(_) | project::Keep::Dirty)) => {
            Error::Conflict { message }
        }
        _ => failed(e),
    }
}

/// A refused file read as its typed error; anything else failed while it ran.
fn unread(e: anyhow::Error) -> Error {
    let message = format!("{e:#}");
    match e.downcast_ref::<files::Refused>() {
        Some(files::Refused::Missing(_)) => Error::NotFound { message },
        Some(files::Refused::Invalid(_)) => Error::Invalid { message },
        None => failed(e),
    }
}

/// The Ledger panel's rows from this Host's `ledger.db`, read-only. A pane
/// is `elsewhere` when its socket is not this Host's skiffd; a result
/// exists only when [`ledger_result`] would serve it. Each row carries its
/// work's bucket from the sessions `all`, as `skiff ledger ls` gives it.
fn ledger_rows(all: Option<&[SessionInfo]>) -> Result<Vec<HostLedgerRow>, Error> {
    let file = skiff_client::ledger::path().map_err(failed)?;
    let mut rows = skiff_client::ledger::panel(&file).map_err(ledger_unread)?;
    skiff_client::ledger::mark_live(&skiff_core::state::socket(), all, &mut rows);
    Ok(rows
        .into_iter()
        .map(|r| {
            let elsewhere = r.elsewhere;
            let bucket = r.bucket.unwrap_or(skiff_client::ledger::OPEN).to_string();
            let result_exists = r.result_path.as_deref().is_some_and(|p| result_file(&file, p).is_ok());
            HostLedgerRow {
                id: r.id,
                uid: r.uid,
                kind: r.kind,
                owner: r.owner,
                parent: r.parent,
                pane_id: r.pane_id,
                pane_gen: r.pane_gen,
                project: r.project,
                task: r.task,
                sent_at: r.sent_at,
                title: r.title,
                state: r.state,
                updated_at: r.updated_at,
                task_type: r.task_type,
                reference: r.reference,
                reported_at: r.reported_at,
                closed_at: r.closed_at,
                gone_at: r.gone_at,
                bucket,
                elsewhere,
                result_exists,
                flushed_at: r.flushed_at,
            }
        })
        .collect())
}

/// A ledger older than the readers take is `invalid`: any `skiff ledger`
/// command on the Host updates it. Anything else failed while it ran.
fn ledger_unread(e: anyhow::Error) -> Error {
    let message = format!("{e:#}");
    if message.contains("skiff ledger") {
        return Error::Invalid { message };
    }
    failed(e)
}

/// The result of the row `uid`, read on this Host as a preview. Only the
/// path the row records is read, never one the client names, and only inside
/// the ledger's own folder: see [`result_file`].
fn ledger_result(uid: &str) -> Result<files::FileRead, Error> {
    let file = skiff_client::ledger::path().map_err(failed)?;
    let none = || Error::NotFound { message: format!("row {uid} has no result this Host can show") };
    let row = skiff_client::ledger::by_uid(&file, uid).map_err(ledger_unread)?.ok_or_else(none)?;
    let path = row.result_path.ok_or_else(none)?;
    let (root, folder, rel) = result_file(&file, &path)?;
    files::read_in(&root, &folder, &rel, files::Purpose::Preview).map_err(unread)
}

/// Where a result may be read: below the folder that holds `ledger`, where
/// `skiff ledger` puts results, as [`files::hold_beneath`] allows. On This
/// Mac it may also be anywhere under the person's home folder: a Skipper
/// records a repo path when its pane cannot write outside its repo. Answers
/// the held folder, its path and the result relative to it. A ledger with
/// no folder of its own serves no result: never the root in its place.
fn result_file(ledger: &Path, result: &str) -> Result<(git::Root, std::path::PathBuf, String), Error> {
    let folder = ledger
        .parent()
        .filter(|f| f.is_absolute() && f.parent().is_some())
        .ok_or_else(|| Error::NotFound { message: format!("{} has no folder to hold results", ledger.display()) })?
        .to_path_buf();
    let beneath = files::hold_beneath(&folder, Path::new(result));
    let (root, folder, rel) = match beneath {
        Ok((root, rel)) => (root, folder, rel),
        Err(e) => {
            let home = dirs::home_dir().filter(|h| THIS_MAC.load(Ordering::Relaxed) && h.is_absolute() && h.parent().is_some());
            let Some(home) = home else { return Err(unread(e)) };
            let (root, rel) = files::hold_beneath(&home, Path::new(result)).map_err(unread)?;
            (root, home, rel)
        }
    };
    Ok((root, folder, rel))
}

/// A folder the client names on this Host: absolute, or under its home as
/// `~/`. Never resolved against this service's working folder.
fn host_folder_path(path: &str) -> Result<std::path::PathBuf, Error> {
    let p = Path::new(path.trim());
    if !(p.is_absolute() || path.trim().starts_with("~/")) || path.chars().any(char::is_control) {
        return Err(Error::Invalid { message: format!("{path} is not a full folder path on this Host") });
    }
    Ok(p.to_path_buf())
}

/// How long the paired skiffd has to list its sessions.
pub(crate) const SESSIONS_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// The sessions of the skiffd this service is paired with, on this Host.
/// Without them nothing is removed: a worktree a session runs in must stay.
pub async fn live_sessions() -> Result<Vec<SessionInfo>, Error> {
    let socket = skiff_core::state::socket();
    let read = async {
        let client = skiff_client::Client::connect(&socket).await?;
        client.list_sessions().await
    };
    match tokio::time::timeout(SESSIONS_TIMEOUT, read).await {
        Ok(Ok(sessions)) => Ok(sessions),
        _ => Err(Error::OperationFailed {
            message: "Skiff cannot read this Host's sessions, so the worktree stays.".into(),
        }),
    }
}

/// Adds to Recent the folders this Host's sessions, groups and open
/// projects use. Best effort: without skiffd, Recent stays as it was.
async fn note_in_use() {
    let socket = skiff_core::state::socket();
    let read = async {
        let client = skiff_client::Client::connect(&socket).await?;
        anyhow::Ok((client.list_sessions().await?, client.list_groups().await?))
    };
    let mut seen: Vec<(String, u64)> = Vec::new();
    if let Ok(Ok((sessions, groups))) = tokio::time::timeout(SESSIONS_TIMEOUT, read).await {
        seen.extend(sessions.into_iter().map(|s| (s.cwd.to_string_lossy().into_owned(), s.started_at)));
        seen.extend(groups.into_iter().filter_map(|g| g.cwd).map(|c| (c, 0)));
    }
    let projects = tokio::task::spawn_blocking(config).await.ok().and_then(Result::ok).unwrap_or_default();
    seen.extend(projects.into_iter().filter(|p| !p.closed).map(|p| (p.path.to_string_lossy().into_owned(), 0)));
    let _ = tokio::task::spawn_blocking(move || folders::recent::note(seen)).await;
}

fn config() -> Result<Vec<ProjectConfig>, Error> {
    Ok(skiff_core::config::load().map_err(failed)?.projects)
}

/// `path` as a worktree of one of the Host's open projects, or `not_found`.
/// The service runs git in no other folder, whatever a client names.
fn known_worktree(path: &str) -> Result<std::path::PathBuf, Error> {
    let wanted = Path::new(path);
    for p in config()?.into_iter().filter(|p| !p.closed && p.path.is_dir()) {
        let Ok(worktrees) = git::list_worktrees(&p.path) else { continue };
        if let Some(w) = worktrees.into_iter().find(|w| project::same_path(&w.path, wanted)) {
            return Ok(w.path);
        }
    }
    Err(Error::NotFound { message: format!("{path} is not a worktree of a project on this Host") })
}

/// The app's project row, field for field. Paths stay the Host's own.
pub fn host_project(p: project::Project) -> HostProject {
    HostProject {
        name: p.name,
        short: p.short,
        path: p.path.to_string_lossy().into_owned(),
        color: p.color,
        agents: p.agents,
        icon: p.icon,
        background: p.background,
        background_opacity: p.background_opacity,
        worktrees: p.worktrees.into_iter().map(host_worktree).collect(),
        kind: Some(match p.kind {
            project::ProjectKind::Git => "git",
            project::ProjectKind::Folder => "folder",
        }.into()),
        default_branch: p.default_branch,
        error: p.error,
        closed: p.closed,
        fleet: p.fleet,
        changes: p.changes,
        theme: p.theme,
        files: p.files,
    }
}

pub fn host_worktree(w: project::Worktree) -> HostWorktree {
    HostWorktree {
        path: w.path.to_string_lossy().into_owned(),
        branch: w.branch,
        head: w.head,
        is_main: w.is_main,
        locked: w.locked,
        prunable: w.prunable,
    }
}

/// The folder as the Host would add it. Its icon is bytes; the path of the
/// file it came from stays on the Host.
pub fn host_folder(f: config::FolderInfo) -> HostFolder {
    HostFolder {
        root: f.root.map(|r| r.to_string_lossy().into_owned()),
        name: f.name,
        short: f.short,
        color: f.color,
        branch: f.branch,
        worktrees: f.worktrees,
        icon: f.icon,
        error: f.error,
    }
}

pub fn host_diff(d: git::Diff) -> HostDiff {
    HostDiff { text: d.text, truncated: d.truncated, binary: d.binary, untracked: d.untracked }
}

pub fn host_change(c: git::Change) -> HostChange {
    HostChange {
        path: c.path,
        status: match c.status {
            'A' => ChangeStatus::A,
            'D' => ChangeStatus::D,
            'U' => ChangeStatus::U,
            _ => ChangeStatus::M,
        },
        added: c.added,
        removed: c.removed,
        committed: c.committed,
    }
}

pub fn host_listing(l: files::Listing) -> HostListing {
    HostListing {
        entries: l
            .entries
            .into_iter()
            .map(|e| HostEntry {
                name: e.name,
                kind: match e.kind {
                    files::Kind::Dir => EntryKind::Dir,
                    files::Kind::File => EntryKind::File,
                    files::Kind::Link => EntryKind::Link,
                    files::Kind::Other => EntryKind::Other,
                },
            })
            .collect(),
        truncated: l.truncated,
    }
}

pub fn host_file(f: files::FileRead) -> HostFile {
    HostFile {
        content: match f.content {
            files::Content::Text => FileContent::Text,
            files::Content::Image => FileContent::Image,
            files::Content::Binary => FileContent::Binary,
            files::Content::Link => FileContent::Link,
        },
        size: f.size,
        mime: f.mime,
        text: f.text,
        bytes: f.bytes,
        truncated: f.truncated,
        target: f.target,
    }
}
