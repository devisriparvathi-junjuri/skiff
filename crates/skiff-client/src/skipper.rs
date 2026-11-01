//! The Skiff agent (Skipper): one agent session that coordinates the others.
//! The app and the `skiff skipper` command share this code, so they set up
//! the same folder and find the same session.
//!
//! The daemon marks the session as the coordinator and finds or starts it in
//! one guarded request ([`Client::coordinator`]). Nothing here guesses it from
//! the folder.
//!
//! The template and the `skiff-cli` skill are built in. The skill is copied
//! out next to `projects.toml`, and the folder links to that copy, so the
//! skill updates with Skiff.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use skiff_core::{
    config::{self, Config, ProjectConfig},
    project::AgentInfo,
    session::{Role, SessionInfo, SessionSpec, SessionState},
};

use crate::{Client, Coordinated, Coordinator};

/// The main Skipper's name until Settings has one. The app keeps the same default.
pub const DEFAULT_NAME: &str = config::ADMIRAL;

/// The folder's instructions. `{{name}}` becomes the agent's name.
const TEMPLATE: &str = include_str!("../agent-template/AGENTS.md.tmpl");

/// A project Skipper's `SKIPPER.md`: the project section only, as
/// [`TEMPLATE`] is the base of every Skipper. `{{project}}`, `{{path}}`,
/// `{{dir}}` and `{{folder}}` become the project, its folder, the
/// Skipper's folder and the Skiff agent's folder.
const PROJECT_TEMPLATE: &str = include_str!("../agent-template/SKIPPER-project.md.tmpl");

/// A fleet Skipper's `SKIPPER.md`: the fleet section only. `{{fleet}}`,
/// `{{dir}}` and `{{folder}}` become the fleet, the Skipper's folder and
/// the Skiff agent's folder. It names no project: the fleet Skipper reads
/// them from `skiff projects --json`, so the file never goes stale.
const FLEET_TEMPLATE: &str = include_str!("../agent-template/SKIPPER-fleet.md.tmpl");

/// The scope prefix of a fleet: a fleet Skipper's mark and its rows' owner
/// are `fleet:<fleet>`. A project's are its bare name.
pub const FLEET: &str = "fleet:";

/// The scope of the fleet `fleet`: `fleet:<fleet>`.
pub fn fleet_scope(fleet: &str) -> String {
    format!("{FLEET}{fleet}")
}

/// The fleet a scope names, or `None` for a project's.
pub fn scope_fleet(scope: &str) -> Option<&str> {
    scope.strip_prefix(FLEET)
}

/// The `skiff-cli` skill, file by file: (path, bytes, runnable).
/// `skill_files_match_the_repo` keeps this list in step with the folder.
const SKILL: &[(&str, &[u8], bool)] = &[
    ("SKILL.md", include_bytes!("../../../.agents/skills/skiff-cli/SKILL.md"), false),
    ("scripts/session_report.py", include_bytes!("../../../.agents/skills/skiff-cli/scripts/session_report.py"), true),
];

/// The marker setup leaves, so a later run knows the folder is the agent's.
/// Its first line is `1`. Each line after it is the hash of an instruction
/// file as Skiff last wrote it, then its path in the folder:
/// `<hash> projects/gesher/SKIPPER.md`. So Skiff tells a file it wrote from
/// one the person edited, with no second copy of the file.
pub(crate) const MARKER: &str = ".skiff-agent";

/// `AGENTS.md` as setup writes it for `name`.
pub(crate) fn template(name: &str) -> String {
    TEMPLATE.replace("{{name}}", name)
}

/// The configured main Skipper name, or Admiral.
pub fn name(cfg: &Config) -> String {
    cfg.skiff_agent.name()
}

/// A project Skipper's session name: the project, then the configured name
/// or "Skipper" ("gesher Skipper"). Unique per project, and never the grand
/// Skipper's, so `skiff send "gesher Skipper"` reaches one session.
pub fn project_name(cfg: &Config, project: &str) -> String {
    format!("{project} {}", cfg.skiff_agent.skipper_word())
}

/// A fleet Skipper's session name: the fleet, then the configured name or
/// "Skipper" ("Harbor Skipper").
pub fn fleet_name(cfg: &Config, fleet: &str) -> String {
    format!("{fleet} {}", cfg.skiff_agent.skipper_word())
}

/// The agent the Skiff agent starts: the one from Settings while it is
/// installed and on (a custom agent is on), else the first enabled one.
/// Settings shows the same.
pub fn choose<'a>(agents: &'a [AgentInfo], id: Option<&str>) -> Option<&'a AgentInfo> {
    agents
        .iter()
        .find(|a| Some(a.id.as_str()) == id && a.installed && (a.enabled || a.custom))
        .or_else(|| agents.iter().find(|a| a.enabled))
}

/// The line that continues the newest conversation in the folder, from each
/// CLI's help (2026-10-13). Only the Skiff agent's own folder runs it: it
/// holds no other conversation. A custom agent has none. Keep in step with
/// `CONTINUE` in apps/desktop/src/terminal/resume.ts, which Settings shows.
pub fn continue_line(agent: &str, cmd: &str) -> Option<String> {
    let cmd = cmd.trim();
    // A subcommand goes after the program and before the user's flags.
    let sub = |words: &str| match cmd.split_once(char::is_whitespace) {
        Some((program, flags)) => format!("{program} {words} {}", flags.trim()),
        None => format!("{cmd} {words}"),
    };
    match agent {
        "claude" | "opencode" | "grok" | "gemini" => Some(format!("{cmd} --continue")),
        "codex" => Some(sub("resume --last")),
        // `--continue` takes the newest session anywhere; `--in` keeps it to this folder.
        "hermes" => Some(format!("{cmd} --resume latest --in .")),
        _ => None,
    }
}

/// Where the skill is copied: next to `projects.toml`.
pub fn skill_home() -> Result<PathBuf> {
    let config = config::config_path();
    let dir = config.parent().context("find the config folder")?;
    Ok(dir.join("skills").join("skiff-cli"))
}

/// Replaces the copy of the skill when the built-in one differs. A new copy
/// is built beside the old one and renamed over it, so an agent that reads
/// the skill never sees half of it.
pub fn sync_skill() -> Result<PathBuf> {
    let dest = skill_home()?;
    if SKILL.iter().all(|(f, bytes, _)| std::fs::read(dest.join(f)).is_ok_and(|b| b == *bytes)) {
        return Ok(dest);
    }
    let parent = dest.parent().context("find the skills folder")?;
    std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    // Named by process, so two copies at once do not share a folder.
    let pid = std::process::id();
    let tmp = parent.join(format!(".skiff-cli.new.{pid}"));
    let old = parent.join(format!(".skiff-cli.old.{pid}"));
    let _ = std::fs::remove_dir_all(&tmp);
    for (f, bytes, run) in SKILL {
        let p = tmp.join(f);
        std::fs::create_dir_all(p.parent().unwrap()).with_context(|| format!("create {}", p.display()))?;
        std::fs::write(&p, bytes).with_context(|| format!("write {}", p.display()))?;
        #[cfg(unix)]
        if *run {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755))?;
        }
    }
    if dest.symlink_metadata().is_ok() {
        std::fs::rename(&dest, &old).with_context(|| format!("move {}", dest.display()))?;
    }
    std::fs::rename(&tmp, &dest).with_context(|| format!("replace {}", dest.display()))?;
    let _ = std::fs::remove_dir_all(&old);
    Ok(dest)
}

/// The agent's folder: [`default_folder`], or the override in projects.toml
/// when it passes [`check`]. Settings › Skippers › Folder changes it with
/// a copy, check and switch ([`crate::agent_folder`]), so the ledger, the
/// results and the instructions go with it. A socket other than the live
/// one never gets the live agent's folder ([`refuse_live_folder`]).
pub fn folder(cfg: &Config) -> Result<PathBuf> {
    let home = home()?;
    let dir = match cfg.skiff_agent.folder.clone() {
        Some(dir) => {
            check(&dir, &home, cfg)?;
            dir
        }
        None => default_folder()?,
    };
    refuse_live_folder(&dir)?;
    Ok(dir)
}

/// The image cache: `cache/` in the agent's [`folder`]. Makes nothing:
/// [`make_folder`] makes it. Every reader of the cache path calls this.
pub fn cache_dir(cfg: &Config) -> Result<PathBuf> {
    Ok(folder(cfg)?.join("cache"))
}

/// What the image and Disk use features say when [`make_folder`] fails.
pub fn no_folder(dir: &Path) -> String {
    format!("Skiff cannot make the folder {}.", dir.display())
}

/// Makes the agent's [`folder`] when it is missing, with the marker and
/// `cache/` (mode 0700). The app runs it at start, with Skipper on or off,
/// and each feature that writes in the folder runs it first. Writes no
/// Skipper file: [`setup`] adds those later. A folder that holds other
/// files is refused, as [`setup`] refuses it. Every failure is the one line
/// of [`no_folder`]; the log keeps the cause. Returns the folder.
pub fn make_folder(cfg: &Config) -> Result<PathBuf> {
    use std::os::unix::fs::DirBuilderExt;
    let dir = match folder(cfg) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("skiff: {e:#}");
            bail!(no_folder(&configured(cfg)?));
        }
    };
    let cache = dir.join("cache");
    if dir.join(MARKER).is_file() && cache.is_dir() {
        return Ok(dir);
    }
    if dir.is_dir() && !dir.join(MARKER).exists() && std::fs::read_dir(&dir).is_ok_and(|mut r| r.next().is_some()) {
        eprintln!("skiff: {} holds other files", dir.display());
        bail!(no_folder(&dir));
    }
    let made = std::fs::create_dir_all(&dir)
        .and_then(|_| std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&cache))
        .and_then(|_| match std::fs::OpenOptions::new().write(true).create_new(true).open(dir.join(MARKER)) {
            Ok(mut f) => std::io::Write::write_all(&mut f, b"1\n"),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
            Err(e) => Err(e),
        });
    if let Err(e) = made {
        eprintln!("skiff: make {}: {e}", dir.display());
        bail!(no_folder(&dir));
    }
    Ok(dir)
}

/// The folder projects.toml names, refused or not: the override, else
/// [`default_folder`]. A change of folder copies from it.
pub fn configured(cfg: &Config) -> Result<PathBuf> {
    match cfg.skiff_agent.folder.clone() {
        Some(dir) => Ok(dir),
        None => default_folder(),
    }
}

/// `~/skiff-agent`, or `skiff-agent` in the socket's private state (see
/// [`skiff_core::state`]).
pub fn default_folder() -> Result<PathBuf> {
    match skiff_core::state::private_dir() {
        Some(d) => Ok(d.join("skiff-agent")),
        None => Ok(home()?.join("skiff-agent")),
    }
}

/// Refuses the live Skiff agent's folder (`~/skiff-agent`, or the folder in
/// the person's projects.toml) to a process on any other socket, opted in
/// to real state or not. Two Skiff agents in one folder share one ledger,
/// and each takes the other's tasks for gone.
pub fn refuse_live_folder(dir: &Path) -> Result<()> {
    use skiff_core::state;
    let socket = state::socket();
    if state::is_live(&socket) {
        return Ok(());
    }
    let d = real(dir);
    if state::real_agent_folders().iter().any(|f| real(f) == d) {
        bail!(
            "{} is the folder of the live Skiff agent, on {}. This skiff uses {}, so it starts no Skiff agent there and follows none of its tasks. Use a folder of its own in [skiff_agent] folder of {}",
            dir.display(),
            state::live_socket().display(),
            socket.display(),
            config::config_path().display()
        );
    }
    Ok(())
}

/// The home folder, from `HOME`.
pub fn home() -> Result<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from).context("HOME is not set")
}

/// Refuses a folder whose instructions would reach other sessions: an agent
/// reads `CLAUDE.md` in every folder above its own, and `AGENTS.md` up to its
/// repository's root. So the folder must not be the home, the root, the config
/// folder, a project, a folder above or inside one, or a folder in a Git
/// repository. Nor may it be an agent's own home or a Hermes profile: Skiff
/// writes nothing in an agent's setup.
fn check(dir: &Path, home: &Path, cfg: &Config) -> Result<()> {
    let shown = dir.display();
    if !dir.is_absolute() {
        bail!("[skiff_agent] folder = \"{shown}\" is not a full path. Use one such as ~/skiff-agent");
    }
    if let Some(why) = refusal(&real(dir), home, cfg) {
        bail!("[skiff_agent] folder = \"{shown}\": {why}");
    }
    Ok(())
}

/// `p` made canonical: `.` and `..` gone and links resolved. For a path
/// that does not exist yet, the nearest existing parent is made canonical
/// and the rest added to it. `p` is absolute.
pub(crate) fn real(p: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            c => {
                out.push(c);
                // Each step resolves its link, so `..` after a link leaves the link's target.
                if let Ok(r) = out.canonicalize() {
                    out = r;
                }
            }
        }
    }
    out
}

/// The agents' own homes, from the home folder. Skiff never writes there.
const AGENT_HOMES: [(&str, &str); 5] =
    [(".claude", "claude"), (".codex", "codex"), (".gemini", "gemini"), (".config/opencode", "opencode"), (".grok", "grok")];

/// The files that make a folder a Hermes profile.
const HERMES_FILES: [&str; 5] = ["config.yaml", "SOUL.md", "state.db", "auth.json", "profile.yaml"];

/// Why `d` (absolute, links resolved) cannot be the agent's folder, in one
/// line, or `None`. Settings, `skiff skipper --move-to` and the override in
/// projects.toml share these rules. A dot-folder such as `~/.skiff-agent` is a
/// folder like any other: the agent homes match by exact path.
pub(crate) fn refusal(d: &Path, home: &Path, cfg: &Config) -> Option<String> {
    let home = real(home);
    if d.parent().is_none() {
        return Some("The root folder holds instructions for every session. Use a folder of its own.".into());
    }
    if d == home {
        return Some("Your home folder holds instructions for every session. Use a folder of its own.".into());
    }
    if let Some(c) = config::config_path().parent().map(real) {
        if d.starts_with(&c) {
            return Some("This is Skiff's config folder. Use a folder of its own.".into());
        }
    }
    let hermes = std::env::var_os("HERMES_HOME").map(PathBuf::from).into_iter().chain([home.join(".hermes")]);
    if hermes.map(|h| real(&h)).any(|h| d.starts_with(h)) || HERMES_FILES.iter().any(|f| d.join(f).exists()) {
        return Some("This is a Hermes profile. Choose another folder for Admiral.".into());
    }
    for (dir, agent) in AGENT_HOMES {
        if d.starts_with(real(&home.join(dir))) {
            return Some(format!("This is the home folder of {agent}. Skiff does not write in an agent's setup."));
        }
    }
    for p in &cfg.projects {
        let pr = real(&p.path);
        if d.starts_with(&pr) || pr.starts_with(d) {
            return Some(format!("This folder mixes with the project {}. Choose a folder outside any project.", p.name));
        }
    }
    if d.ancestors().any(|a| a.join(".git").symlink_metadata().is_ok()) {
        return Some("This folder is inside a Git repository. Choose a folder outside any project.".into());
    }
    if d.file_name().is_some_and(|n| n.to_string_lossy().contains(crate::agent_folder::STAGING)) {
        return Some("This is an unfinished copy. Skiff removes it at the next change.".into());
    }
    None
}

/// One edit from Settings › Skippers or Task types, on this Host's config:
/// the app's for This Mac, `skiff-host`'s for a remote Host
/// (`skipper_config@1`).
pub fn apply(edit: &skiff_host_api::SkipperEdit) -> Result<()> {
    use config::SkiffAgentKey as K;
    use skiff_host_api::SkipperEdit as E;
    match edit {
        E::Name(v) => config::set_skiff_agent(K::Name(v.as_deref())),
        E::SkipperName(v) => config::set_skiff_agent(K::SkipperName(v.as_deref())),
        E::Agent(v) => config::set_skiff_agent(K::Agent(v.as_deref())),
        E::Model(v) => config::set_skiff_agent(K::Model(v.as_deref())),
        E::ContinueOnRestore(on) => config::set_skiff_agent(K::ContinueOnRestore(*on)),
        E::ProjectAgent(v) => config::set_skiff_agent(K::ProjectAgent(v.as_deref())),
        E::ProjectModel(v) => config::set_skiff_agent(K::ProjectModel(v.as_deref())),
        E::AddTask(t) => config::add_task_type(&t.name, &t.agent, t.model.as_deref()),
        E::Task(t) => config::set_task_type(&t.name, &t.agent, t.model.as_deref()),
        E::RenameTask { from, to } => {
            // The file moves first: a name a kept file has stops the rename.
            let dir = folder(&config::load()?)?;
            skiff_core::tasks::rename_file(&dir, from, to)?;
            config::rename_task_type(from, to).inspect_err(|_| {
                let _ = skiff_core::tasks::rename_file(&dir, to, from);
            })
        }
        E::RemoveTask(name) => config::remove_task_type(name),
    }
}

/// Makes the agent's folder ready: writes the template parts it lacks, with
/// the name from Settings. Every open runs it before it starts a session.
/// Returns the folder.
pub fn ensure(cfg: &Config) -> Result<PathBuf> {
    setup(&folder(cfg)?, &name(cfg))
}

/// Writes the parts of the template that `dir` does not have yet. The folder
/// must be new, empty, or a Skiff folder (it has the marker: the app makes
/// one at start, or this setup made it before), so setup never mixes the
/// agent's files with other files. A second run keeps
/// the user's edits and puts back what is missing. Nothing is written outside
/// the folder and the skill's copy. A lock beside `projects.toml` keeps two
/// setups at once (the app and the CLI) apart.
///
/// ```text
/// AGENTS.md                    the instructions, with the agent's name
/// CLAUDE.md, GEMINI.md         links to AGENTS.md
/// .claude                      a link to .agents, for Claude's skills
/// .agents/skills/skiff-cli     a link to the copy of the skill
/// results/                     long child reports
/// cache/                       the image cache, mode 0700
/// ```
pub fn setup(dir: &Path, name: &str) -> Result<PathBuf> {
    let _lock = lock()?;
    setup_in(dir, name, &sync_skill()?)
}

/// [`setup`] for a caller that holds the lock. `skill` is the skill's copy.
pub(crate) fn setup_in(dir: &Path, name: &str, skill: &Path) -> Result<PathBuf> {
    if dir.metadata().is_ok_and(|m| !m.is_dir()) {
        bail!("{} is not a folder", dir.display());
    }
    if dir.is_dir() && !dir.join(MARKER).exists() && std::fs::read_dir(dir)?.next().is_some() {
        bail!("{} holds other files. Move them, or choose another folder in Settings › Skippers › Folder", dir.display());
    }

    std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    let dir = dir.canonicalize().with_context(|| format!("find {}", dir.display()))?;
    // A broken link counts as there: setup does not replace what the user made.
    let missing = |p: &Path| p.symlink_metadata().is_err();
    if missing(&dir.join(MARKER)) {
        write_new(&dir.join(MARKER), "1\n")?;
    }
    refresh(&dir, AGENTS_FILE, &template(name))?;
    for f in ["CLAUDE.md", "GEMINI.md"] {
        if missing(&dir.join(f)) {
            link(Path::new("AGENTS.md"), &dir.join(f))?;
        }
    }
    std::fs::create_dir_all(dir.join("results")).context("create results")?;
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir.join("cache")).context("create cache")?;
    }
    let skills = dir.join(".agents").join("skills");
    std::fs::create_dir_all(&skills).context("create .agents/skills")?;
    if missing(&skills.join("skiff-cli")) {
        link(skill, &skills.join("skiff-cli"))?;
    }
    if missing(&dir.join(".claude")) {
        link(Path::new(".agents"), &dir.join(".claude"))?;
    }
    Ok(dir)
}

/// The Skiff agent's instructions, the base of every Skipper.
pub const AGENTS_FILE: &str = "AGENTS.md";

/// The 16 hex digits of a 64-bit FNV-1a hash of `bytes`, as [`build_of`] makes.
fn hash(bytes: &[u8]) -> String {
    format!("{:016x}", bytes.iter().fold(0xcbf29ce484222325u64, |h, &b| (h ^ b as u64).wrapping_mul(0x100000001b3)))
}

/// The hashes in the marker of `dir`, by path in the folder.
fn written(dir: &Path) -> std::collections::BTreeMap<String, String> {
    let text = std::fs::read_to_string(dir.join(MARKER)).unwrap_or_default();
    text.lines().skip(1).filter_map(|l| l.split_once(' ')).map(|(h, f)| (f.to_string(), h.to_string())).collect()
}

/// Puts the hash of `rel` in the marker of `dir`, or takes it out with `None`.
fn record(dir: &Path, rel: &str, h: Option<&str>) -> Result<()> {
    let mut all = written(dir);
    match h {
        Some(h) => all.insert(rel.to_string(), h.to_string()),
        None => all.remove(rel),
    };
    let lines: String = all.iter().map(|(f, h)| format!("{h} {f}\n")).collect();
    config::write_atomic(&dir.join(MARKER), &format!("1\n{lines}"))
}

/// Moves the hash of `from` to `to` in the marker of `dir`, as a renamed
/// fleet moves its folder. The caller holds [`lock`].
pub(crate) fn move_record(dir: &Path, from: &str, to: &str) -> Result<()> {
    let Some(h) = written(dir).get(from).cloned() else { return Ok(()) };
    record(dir, from, None)?;
    record(dir, to, Some(&h))
}

/// Writes `text` to `rel` in `dir`, makes its folder, and keeps its hash.
fn write_file(dir: &Path, rel: &str, text: &str) -> Result<()> {
    let path = dir.join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    std::fs::write(&path, text).with_context(|| format!("write {}", path.display()))?;
    record(dir, rel, Some(&hash(text.as_bytes())))
}

/// At a Skipper's start: writes `rel` when it is missing, and again when its
/// status is template and the template changed. An edited file stays.
fn refresh(dir: &Path, rel: &str, text: &str) -> Result<()> {
    let path = dir.join(rel);
    // A broken link counts as there: setup does not replace what the user made.
    if path.symlink_metadata().is_err() {
        return write_file(dir, rel, text);
    }
    let Ok(now) = std::fs::read(&path) else { return Ok(()) };
    match written(dir).get(rel) {
        Some(h) if *h == hash(&now) && now != text.as_bytes() => write_file(dir, rel, text),
        // The same text with no hash: keep it as Skiff's, so the next template change reaches it.
        None if now == text.as_bytes() => record(dir, rel, Some(&hash(&now))),
        _ => Ok(()),
    }
}

/// The setup lock. It frees itself when the process ends. A change of
/// folder holds it from the first step to the last.
pub(crate) fn lock() -> Result<std::fs::File> {
    let config = config::config_path();
    let dir = config.parent().context("find the config folder")?;
    std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    let path = dir.join(".skiff-agent.lock");
    let f = std::fs::File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .with_context(|| format!("open {}", path.display()))?;
    f.lock().with_context(|| format!("lock {}", path.display()))?;
    Ok(f)
}

/// Writes a new file. Refuses one that is there, so setup never overwrites.
fn write_new(path: &Path, text: &str) -> Result<()> {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .with_context(|| format!("create {}", path.display()))?;
    f.write_all(text.as_bytes()).with_context(|| format!("write {}", path.display()))
}

#[cfg(unix)]
fn link(target: &Path, at: &Path) -> Result<()> {
    std::os::unix::fs::symlink(target, at).with_context(|| format!("link {}", at.display()))
}

#[cfg(not(unix))]
fn link(_target: &Path, _at: &Path) -> Result<()> {
    bail!("not supported on this platform")
}

/// The name of a project Skipper's file in its folder.
pub const SKIPPER_FILE: &str = "SKIPPER.md";

/// `SKIPPER.md` for the project `p`, whose Skipper's folder is `dir`, in
/// the Skiff agent's folder `folder`.
pub fn project_template(cfg: &Config, p: &ProjectConfig, dir: &Path, folder: &Path) -> String {
    PROJECT_TEMPLATE
        .replace("{{main}}", &name(cfg))
        .replace("{{project}}", &p.name)
        .replace("{{path}}", &p.path.display().to_string())
        .replace("{{dir}}", &dir.display().to_string())
        .replace("{{folder}}", &folder.display().to_string())
}

/// The configured project, by name.
pub fn project<'a>(cfg: &'a Config, name: &str) -> Result<&'a ProjectConfig> {
    cfg.projects.iter().find(|p| p.name == name).with_context(|| format!("no project {name:?} in {}", config::config_path().display()))
}

/// A project Skipper's folder: `projects/<project>` in the Skiff agent's
/// folder. Nothing goes into the project itself.
pub fn project_folder(cfg: &Config, project: &str) -> Result<PathBuf> {
    if project.is_empty() || project == "." || project == ".." || project.contains(['/', '\\']) {
        bail!("the project name {project:?} cannot name a folder");
    }
    Ok(folder(cfg)?.join("projects").join(project))
}

/// The agent a project Skipper starts, with its model: the project's
/// `skipper_agent` with the agent's default model, else `[skiff_agent]
/// project_agent`, else the Skipper's agent (see [`choose`]), each with
/// `project_model`. One that is not installed and on falls through to the
/// next.
pub fn project_agent<'a>(cfg: &Config, agents: &'a [AgentInfo], p: &ProjectConfig) -> Option<(&'a AgentInfo, Option<String>)> {
    let usable = |id: Option<&str>| agents.iter().find(|a| Some(a.id.as_str()) == id && a.installed && (a.enabled || a.custom));
    match usable(p.skipper_agent.as_deref()) {
        Some(a) => Some((a, None)),
        None => fleet_agent(cfg, agents),
    }
}

/// Makes a project Skipper's folder ready: the Skiff agent's folder first,
/// then `projects/<project>/` with `SKIPPER.md` and `results/`. Skiff
/// writes `SKIPPER.md` once, and again only while its status is template
/// (see [`refresh`]). Returns the folder. The caller holds [`lock`].
fn setup_project(cfg: &Config, p: &ProjectConfig) -> Result<PathBuf> {
    let top = setup_in(&folder(cfg)?, &name(cfg), &sync_skill()?)?;
    let dir = project_folder(cfg, &p.name)?;
    std::fs::create_dir_all(dir.join("results")).with_context(|| format!("create {}", dir.join("results").display()))?;
    refresh(&top, &Scope::Project(p.name.clone()).file(), &project_template(cfg, p, &dir, &top))?;
    Ok(dir)
}

/// The open projects of `fleet`, in config order.
pub fn fleet_projects<'a>(cfg: &'a Config, fleet: &str) -> Vec<&'a ProjectConfig> {
    cfg.projects.iter().filter(|p| !p.closed && p.fleet.as_deref() == Some(fleet)).collect()
}

/// A fleet Skipper's folder: `fleets/<fleet>` in the Skiff agent's folder.
pub fn fleet_folder(cfg: &Config, fleet: &str) -> Result<PathBuf> {
    if fleet.is_empty() || fleet == "." || fleet == ".." || fleet.contains(['/', '\\']) {
        bail!("the fleet name {fleet:?} cannot name a folder");
    }
    Ok(folder(cfg)?.join("fleets").join(fleet))
}

/// `SKIPPER.md` for the fleet `fleet`, whose Skipper's folder is `dir`, in
/// the Skiff agent's folder `folder`.
pub fn fleet_template(cfg: &Config, fleet: &str, dir: &Path, folder: &Path) -> String {
    FLEET_TEMPLATE
        .replace("{{main}}", &name(cfg))
        .replace("{{fleet}}", fleet)
        .replace("{{dir}}", &dir.display().to_string())
        .replace("{{folder}}", &folder.display().to_string())
}

/// The agent a fleet Skipper starts, with its model: `[skiff_agent]
/// project_agent`, else the Skipper's agent, with `project_model`. A
/// project's `skipper_agent` is for its project Skipper only.
pub fn fleet_agent<'a>(cfg: &Config, agents: &'a [AgentInfo]) -> Option<(&'a AgentInfo, Option<String>)> {
    let a = agents
        .iter()
        .find(|a| Some(a.id.as_str()) == cfg.skiff_agent.project_agent.as_deref() && a.installed && (a.enabled || a.custom))
        .or_else(|| choose(agents, cfg.skiff_agent.agent.as_deref()))?;
    Some((a, cfg.skiff_agent.project_model.clone()))
}

/// Makes a fleet Skipper's folder ready, with `results/` and `SKIPPER.md`.
/// Skiff writes `SKIPPER.md` once, and again only while its status is
/// template (see [`refresh`]). Returns the folder. The app and the watcher
/// call it when the fleet changes under a running fleet Skipper.
pub fn write_fleet(cfg: &Config, fleet: &str) -> Result<PathBuf> {
    let top = folder(cfg)?;
    let dir = fleet_folder(cfg, fleet)?;
    std::fs::create_dir_all(dir.join("results")).with_context(|| format!("create {}", dir.join("results").display()))?;
    refresh(&top, &Scope::Fleet(fleet.to_string()).file(), &fleet_template(cfg, fleet, &dir, &top))?;
    Ok(dir)
}

/// The first message a new project or fleet Skipper gets. It names both
/// files: an agent in a project folder does not read the Skiff agent's
/// `AGENTS.md` by itself.
pub fn first_message(cfg: &Config, dir: &Path) -> Result<String> {
    Ok(format!(
        "Read {}, then {}. {SKIPPER_FILE} wins where the two differ. Then do the First steps.",
        folder(cfg)?.join(AGENTS_FILE).display(),
        dir.join(SKIPPER_FILE).display()
    ))
}

/// One instruction file: `AGENTS.md` for the Skipper and the base of every
/// Skipper, or the `SKIPPER.md` of a fleet or a project.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", content = "name", rename_all = "snake_case")]
pub enum Scope {
    Skipper,
    Fleet(String),
    Project(String),
}

impl Scope {
    /// Its path in the Skiff agent's folder.
    pub fn file(&self) -> String {
        match self {
            Scope::Skipper => AGENTS_FILE.to_string(),
            Scope::Fleet(f) => format!("fleets/{f}/{SKIPPER_FILE}"),
            Scope::Project(p) => format!("projects/{p}/{SKIPPER_FILE}"),
        }
    }

    /// The text Skiff writes for it now.
    pub(crate) fn template(&self, cfg: &Config, top: &Path) -> Result<String> {
        Ok(match self {
            Scope::Skipper => template(&name(cfg)),
            Scope::Fleet(f) => {
                if !is_fleet(cfg, f) {
                    bail!("no fleet {f:?} in {}", config::config_path().display());
                }
                fleet_template(cfg, f, &fleet_folder(cfg, f)?, top)
            }
            Scope::Project(p) => project_template(cfg, project(cfg, p)?, &project_folder(cfg, p)?, top),
        })
    }
}

/// What an instruction file is now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Status {
    /// No file.
    None,
    /// The file is what Skiff last wrote.
    Template,
    /// The person changed it.
    Edited,
    /// The person changed it, and the template is newer than Skiff's last write.
    EditedTemplateChanged,
}

/// One row of Settings › Skippers › Instructions.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FileStatus {
    pub scope: Scope,
    /// Its path in the Skiff agent's folder.
    pub file: String,
    /// Its full path.
    pub path: PathBuf,
    pub status: Status,
    /// Unix time in milliseconds. `None` with no file.
    pub modified: Option<u64>,
    /// In bytes. `None` with no file.
    pub size: Option<u64>,
}

/// The status of the file of `scope`. Reads only.
pub fn file_status(cfg: &Config, scope: &Scope) -> Result<FileStatus> {
    let top = folder(cfg)?;
    status_in(&top, scope, &scope.template(cfg, &top)?)
}

fn status_in(top: &Path, scope: &Scope, text: &str) -> Result<FileStatus> {
    let file = scope.file();
    let path = top.join(&file);
    let (status, modified, size) = match std::fs::read(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (Status::None, None, None),
        Err(e) => return Err(e).with_context(|| format!("read {}", path.display())),
        Ok(now) => {
            let m = std::fs::metadata(&path)?;
            let at = m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_millis() as u64);
            // With no hash, Skiff did not write the file: it is the template only when it is the same.
            let status = match written(top).get(&file) {
                Some(h) if *h == hash(&now) => Status::Template,
                Some(h) if *h == hash(text.as_bytes()) => Status::Edited,
                None if now == text.as_bytes() => Status::Template,
                _ => Status::EditedTemplateChanged,
            };
            (status, at, Some(m.len()))
        }
    };
    Ok(FileStatus { scope: scope.clone(), file, path, status, modified, size })
}

/// Every instruction file, in tree order: the Skipper, each fleet, then
/// each open project, the projects in no fleet first.
pub fn instructions(cfg: &Config) -> Result<Vec<FileStatus>> {
    let top = folder(cfg)?;
    let open: Vec<&ProjectConfig> = cfg.projects.iter().filter(|p| !p.closed).collect();
    let mut fleets: Vec<&str> = Vec::new();
    for f in open.iter().filter_map(|p| p.fleet.as_deref()) {
        if !fleets.contains(&f) {
            fleets.push(f);
        }
    }
    let mut scopes = vec![Scope::Skipper];
    scopes.extend(fleets.iter().map(|f| Scope::Fleet(f.to_string())));
    scopes.extend(open.iter().filter(|p| p.fleet.is_none()).map(|p| Scope::Project(p.name.clone())));
    for f in &fleets {
        scopes.extend(open.iter().filter(|p| p.fleet.as_deref() == Some(f)).map(|p| Scope::Project(p.name.clone())));
    }
    scopes.iter().map(|s| status_in(&top, s, &s.template(cfg, &top)?)).collect()
}

/// Settings › Edit: the file's full path. A missing file is written from
/// the template first, with its folder.
pub fn edit_file(cfg: &Config, scope: &Scope) -> Result<PathBuf> {
    let _lock = lock()?;
    let top = setup_in(&folder(cfg)?, &name(cfg), &sync_skill()?)?;
    let file = scope.file();
    let text = scope.template(cfg, &top)?;
    if top.join(&file).symlink_metadata().is_err() {
        write_file(&top, &file, &text)?;
    }
    Ok(top.join(file))
}

/// Settings › Reset…: the template again. The old file goes to
/// `<name>.bak`, which replaces the last one.
pub fn reset_file(cfg: &Config, scope: &Scope) -> Result<FileStatus> {
    let _lock = lock()?;
    let top = setup_in(&folder(cfg)?, &name(cfg), &sync_skill()?)?;
    let file = scope.file();
    let text = scope.template(cfg, &top)?;
    let path = top.join(&file);
    if path.exists() {
        let bak = path.with_file_name(format!("{}.bak", path.file_name().unwrap_or_default().to_string_lossy()));
        std::fs::rename(&path, &bak).with_context(|| format!("move {} to {}", path.display(), bak.display()))?;
    }
    write_file(&top, &file, &text)?;
    status_in(&top, scope, &text)
}

/// The live project Skipper of `project` in `all`, if one runs.
pub fn find_project<'a>(all: &'a [SessionInfo], project: &str) -> Option<&'a SessionInfo> {
    all.iter().find(|s| s.skipper.as_deref() == Some(project) && s.state != SessionState::Done)
}

/// The live fleet Skipper of `fleet` in `all`, if one runs.
pub fn find_fleet<'a>(all: &'a [SessionInfo], fleet: &str) -> Option<&'a SessionInfo> {
    find_project(all, &fleet_scope(fleet))
}

/// The live project Skippers of the projects of `fleet`, in config order.
pub fn project_skippers_in<'a>(cfg: &Config, all: &'a [SessionInfo], fleet: &str) -> Vec<(String, &'a SessionInfo)> {
    fleet_projects(cfg, fleet).into_iter().filter_map(|p| find_project(all, &p.name).map(|s| (p.name.clone(), s))).collect()
}

/// The fleet of `project` when that fleet's Skipper runs.
pub fn commanding_fleet<'a>(cfg: &'a Config, all: &[SessionInfo], project: &str) -> Option<&'a str> {
    let fleet = cfg.projects.iter().find(|p| p.name == project)?.fleet.as_deref()?;
    find_fleet(all, fleet).is_some().then_some(fleet)
}

/// The fleet named `fleet` exists: an open project carries it.
pub fn is_fleet(cfg: &Config, fleet: &str) -> bool {
    !fleet_projects(cfg, fleet).is_empty()
}

/// A project or fleet Skipper the CLI found or started.
pub struct ProjectSkipper {
    pub session: SessionInfo,
    /// It started now: it needs [`first_message`].
    pub created: bool,
    /// Its folder.
    pub dir: PathBuf,
}

/// The first skiffd that marks a project Skipper (`SessionSpec::skipper`).
/// An older one starts a plain agent instead, so nothing is started on it.
pub const PROJECT_SKIPPER_SINCE: &str = "0.6.25";

/// skiffd `version` cannot mark a project Skipper.
pub fn too_old_for_project(version: &str) -> bool {
    skiff_core::compare_versions(version, PROJECT_SKIPPER_SINCE).is_lt()
}

/// Finds the live project Skipper of `project`, or starts one in the
/// project's folder. The setup lock keeps two starts apart, so a project
/// has at most one. Refuses before anything starts when skiffd is too old
/// to mark one.
pub async fn open_project(c: &Client, project_name: &str) -> Result<ProjectSkipper> {
    let cfg = config::load()?;
    let daemon = c.daemon_info().await?;
    if too_old_for_project(&daemon.version) {
        bail!("skiffd {} is older than this Skiff and cannot start a project Skipper. Update skiffd first", daemon.version);
    }
    let p = project(&cfg, project_name)?;
    let _lock = lock()?;
    let all = c.list_sessions().await?;
    if let Some(s) = find_project(&all, &p.name) {
        let want = self::project_name(&cfg, &p.name);
        let session = if s.name.as_deref() == Some(&want) { s.clone() } else { c.rename_session(&s.id, &want).await? };
        crate::fleet::restart_rows(&crate::ledger::path()?, &p.name, &session, &all)?;
        return Ok(ProjectSkipper { session, created: false, dir: setup_project(&cfg, p)? });
    }
    // One Skipper per project: a running fleet Skipper commands this one.
    if let Some(fleet) = commanding_fleet(&cfg, &all, &p.name) {
        bail!("{} is in {fleet}, which has a fleet Skipper. Run: skiff skipper --fleet {fleet}", p.name);
    }
    let dir = setup_project(&cfg, p)?;
    let agents = c.list_agents().await?;
    let Some((agent, model)) = project_agent(&cfg, &agents, p) else { bail!("No agent is on. Turn one on in Settings › Agents") };
    let mut s = spec(&with_model(agent, model.as_deref()), &self::project_name(&cfg, &p.name), p.path.clone());
    crate::cache::add_flags(&cfg, std::slice::from_ref(agent), &mut s);
    s.skipper = Some(p.name.clone());
    s.cols = 120;
    s.rows = 40;
    let info = c.create_session(s).await?;
    if info.skipper.as_deref() != Some(p.name.as_str()) {
        // Checked above by version; a daemon that still drops the mark
        // leaves nothing behind.
        let _ = c.kill(&info.id).await;
        bail!("skiffd did not mark the project Skipper. Nothing started. Update skiffd first");
    }
    crate::fleet::restart_rows(&crate::ledger::path()?, &p.name, &info, &all)?;
    Ok(ProjectSkipper { session: info, created: true, dir })
}

/// Runs `skiff skipper --project P --no-attach` with `cli` on `socket`: it
/// finds or starts the project Skipper, sends a new one its first message,
/// and starts the watcher. The app and `skiff-host` share it, so both start
/// one the same way. Returns the session id. It waits for a new agent's
/// input box, so it can take a while.
pub fn start_project(socket: &Path, cli: &Path, project: &str) -> Result<String> {
    let out = std::process::Command::new(cli)
        .arg("--socket")
        .arg(socket)
        .args(["skipper", "--project", project, "--no-attach"])
        // The caller is the person, not a pane.
        .env_remove("SKIFF_SESSION")
        .stdin(std::process::Stdio::null())
        .output()
        .with_context(|| format!("run {}", cli.display()))?;
    if !out.status.success() {
        let msg = String::from_utf8_lossy(&out.stderr);
        bail!("{}", msg.trim().trim_start_matches("skiff: ").trim_start_matches("Error: "));
    }
    Ok(String::from_utf8_lossy(&out.stdout).lines().last().unwrap_or_default().trim().to_string())
}

/// Runs `skiff skipper --fleet F --no-attach --yes` with `cli` on `socket`:
/// it finds or starts the fleet Skipper, takes the fleet's project Skippers
/// over (the person confirmed it first), sends a new one its first message,
/// and starts the watcher. The app and `skiff-host` share it. Returns the
/// session id.
pub fn start_fleet(socket: &Path, cli: &Path, fleet: &str) -> Result<String> {
    let out = std::process::Command::new(cli)
        .arg("--socket")
        .arg(socket)
        .args(["skipper", "--fleet", fleet, "--no-attach", "--yes"])
        // The caller is the person, not a pane.
        .env_remove("SKIFF_SESSION")
        .stdin(std::process::Stdio::null())
        .output()
        .with_context(|| format!("run {}", cli.display()))?;
    if !out.status.success() {
        let msg = String::from_utf8_lossy(&out.stderr);
        bail!("{}", msg.trim().trim_start_matches("skiff: ").trim_start_matches("Error: "));
    }
    Ok(String::from_utf8_lossy(&out.stdout).lines().last().unwrap_or_default().trim().to_string())
}

/// The first skiffd that retires a project Skipper (`Request::Retire`). A
/// fleet Skipper's takeover needs it, so an older one starts none.
pub const FLEET_SKIPPER_SINCE: &str = "0.6.28";

/// skiffd `version` cannot run a fleet Skipper's takeover.
pub fn too_old_for_fleet(version: &str) -> bool {
    skiff_core::compare_versions(version, FLEET_SKIPPER_SINCE).is_lt()
}

/// Finds the live fleet Skipper of `fleet`, or starts one in its folder.
/// It refuses before anything starts when skiffd is too old for a
/// takeover. It does not look at project Skippers in the fleet: the caller
/// takes them over first (see [`crate::fleet::takeover`]).
pub async fn open_fleet(c: &Client, fleet: &str) -> Result<ProjectSkipper> {
    let cfg = config::load()?;
    let daemon = c.daemon_info().await?;
    if too_old_for_fleet(&daemon.version) {
        bail!("skiffd {} is older than this Skiff and cannot start a fleet Skipper. Update skiffd first", daemon.version);
    }
    if !is_fleet(&cfg, fleet) {
        bail!("no fleet {fleet:?} in {}", config::config_path().display());
    }
    let _lock = lock()?;
    setup_in(&folder(&cfg)?, &name(&cfg), &sync_skill()?)?;
    let dir = write_fleet(&cfg, fleet)?;
    let all = c.list_sessions().await?;
    if let Some(s) = find_fleet(&all, fleet) {
        let want = fleet_name(&cfg, fleet);
        let session = if s.name.as_deref() == Some(&want) { s.clone() } else { c.rename_session(&s.id, &want).await? };
        crate::fleet::restart_rows(&crate::ledger::path()?, &fleet_scope(fleet), &session, &all)?;
        return Ok(ProjectSkipper { session, created: false, dir });
    }
    let agents = c.list_agents().await?;
    let Some((agent, model)) = fleet_agent(&cfg, &agents) else { bail!("No agent is on. Turn one on in Settings › Agents") };
    let mut s = spec(&with_model(agent, model.as_deref()), &fleet_name(&cfg, fleet), dir.clone());
    crate::cache::add_flags(&cfg, std::slice::from_ref(agent), &mut s);
    s.skipper = Some(fleet_scope(fleet));
    s.cols = 120;
    s.rows = 40;
    let info = c.create_session(s).await?;
    if info.skipper.as_deref() != Some(fleet_scope(fleet).as_str()) {
        bail!("skiffd is too old to mark a fleet Skipper. Run `skiff daemon reload`. Session {} runs as a plain agent", info.id);
    }
    crate::fleet::restart_rows(&crate::ledger::path()?, &fleet_scope(fleet), &info, &all)?;
    Ok(ProjectSkipper { session: info, created: true, dir })
}

/// What [`open`] does with the coordinator it finds.
pub enum Open {
    /// Find it, or start it. A bare shell gets the agent: its last
    /// conversation when Settings says to continue on restore, else a new one.
    Start,
    /// A restore brought it back as a shell: continue its last conversation,
    /// when Settings says so. Never starts a session or a new conversation.
    /// `expect` is the `input_gen` the app read, so a line the person
    /// typed first stays theirs.
    Restore { expect: u64 },
}

/// Finds the live Skiff agent session, or starts it, through the daemon's
/// guarded request. `None` only for [`Open::Restore`] with no coordinator.
pub async fn open(c: &Client, how: Open) -> Result<Option<Coordinated>> {
    let cfg = config::load()?;
    let a = &cfg.skiff_agent;
    // Before any request: another socket must not resume the live agent's conversation either.
    folder(&cfg)?;
    let agents = c.list_agents().await?;
    let agent = choose(&agents, a.agent.as_deref());
    // Each line ends with the image cache's read flag, as the + menu adds it.
    let resume = agent
        .filter(|_| a.continue_on_restore)
        .and_then(|g| continue_line(&g.id, &with_model(g, a.model.as_deref())).map(|l| l + &crate::cache::flags_typed(&cfg, g)));
    let ask = match how {
        Open::Restore { expect } => Coordinator { resume, expect: Some(expect), ..Default::default() },
        Open::Start => {
            let Some(agent) = agent else { bail!("No agent is on. Turn one on in Settings › Agents") };
            // The folder is made ready first: files the user deleted come back.
            let dir = ensure(&cfg)?;
            let command = with_model(agent, a.model.as_deref());
            let mut s = spec(&command, &name(&cfg), dir);
            crate::cache::add_flags(&cfg, std::slice::from_ref(agent), &mut s);
            let start = format!("{command}{}", crate::cache::flags_typed(&cfg, agent));
            Coordinator { spec: Some(s), start: Some(start), resume, expect: None }
        }
    };
    c.coordinator(ask).await
}

/// The agent's command with the model flag, as a task type adds it. An
/// agent that takes its model in its command, or no model, gives the plain
/// command: Settings shows "in the command" for it.
pub fn with_model(agent: &AgentInfo, model: Option<&str>) -> String {
    skiff_core::tasks::command(agent, model).unwrap_or_else(|_| agent.command.trim().to_string())
}

/// A new session that runs `command` in `dir`, as the app's + menu starts one.
fn spec(command: &str, name: &str, dir: PathBuf) -> SessionSpec {
    let mut words = command.split_whitespace().map(str::to_string);
    SessionSpec {
        label: name.to_string(),
        name: Some(name.to_string()),
        role: Role::Agent,
        cwd: Some(dir),
        command: words.next(),
        args: words.collect(),
        ..Default::default()
    }
}

/// Starts `skiff watch` for the agent's session, unless one runs for this
/// daemon. `cli` is the `skiff` binary to run. The watcher is its own
/// process, out of the caller's process group and session: it outlives the
/// app or the terminal that started it, connects again after a daemon
/// reload, and stops when the session ends. Its lock beside the socket keeps
/// it to one copy. True when it started one.
pub fn start_watch(socket: &Path, cli: &Path, session: &str) -> Result<bool> {
    let lock = watch_lock(socket);
    if let Ok(f) = std::fs::File::options().create(true).truncate(false).write(true).open(&lock) {
        if f.try_lock().is_err() {
            return Ok(false);
        }
    }
    let log = fresh_log(&socket.with_extension("watch.log"))?;
    let mut cmd = outliving(cli, &unit_for("skiff-watch", socket));
    cmd.args(["watch", session])
        .env("SKIFF_SOCKET", socket)
        .stdin(std::process::Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    detach(&mut cmd);
    let mut child = cmd.spawn().with_context(|| format!("start {}", cli.display()))?;
    // Reap it when it ends while the caller runs.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(true)
}

/// The watcher that holds the lock, as it writes itself into the lock file.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct WatchHolder {
    pub pid: u32,
    /// The daemon socket it watches.
    pub socket: String,
    /// Unix time in milliseconds.
    pub started_at: u64,
    /// The CLI version it runs. `None` from a watcher older than this field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Its executable, with links resolved. `None` from an older watcher.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exe: Option<String>,
    /// The build of its executable when it started ([`build_of`]). `None`
    /// from an older watcher: that counts as another build.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build: Option<String>,
}

/// The CLI version this crate builds with: the app's and the CLI's.
pub const CLI_VERSION: &str = env!("CARGO_PKG_VERSION");

/// The build id of the program file at `path` (`skiff`, `skiff-host`): 16
/// hex digits of a 64-bit FNV-1a hash over its bytes. Two builds of one
/// version differ by it, as a dev build and the next one do. Read again
/// only when the file's size or time changes. `None` when the file cannot
/// be read.
pub fn build_of(path: &Path) -> Option<String> {
    use std::io::Read;
    type Seen = (PathBuf, u64, Option<std::time::SystemTime>, String);
    static SEEN: std::sync::Mutex<Vec<Seen>> = std::sync::Mutex::new(Vec::new());
    let m = std::fs::metadata(path).ok()?;
    let (len, time) = (m.len(), m.modified().ok());
    if let Some((_, _, _, id)) = SEEN.lock().unwrap().iter().find(|(p, l, t, _)| p == path && *l == len && *t == time) {
        return Some(id.clone());
    }
    let mut f = std::fs::File::open(path).ok()?;
    let mut buf = vec![0u8; 1 << 20];
    let mut h = 0xcbf29ce484222325u64;
    loop {
        let n = f.read(&mut buf).ok()?;
        if n == 0 {
            break;
        }
        h = buf[..n].iter().fold(h, |h, &b| (h ^ b as u64).wrapping_mul(0x100000001b3));
    }
    let id = format!("{h:016x}");
    let mut seen = SEEN.lock().unwrap();
    seen.retain(|(p, ..)| p != path);
    seen.push((path.to_path_buf(), len, time, id.clone()));
    Some(id)
}

/// The watch lock beside the socket, as [`watch_status`] reads it.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct WatchLock {
    /// A process holds the lock: a watcher runs for this daemon, registered or not.
    pub held: bool,
    /// What the holder wrote. `None` when the lock is free, or when a watcher
    /// from before this record holds it.
    pub holder: Option<WatchHolder>,
}

/// The lock file of the watcher for `socket`.
pub fn watch_lock(socket: &Path) -> PathBuf {
    socket.with_extension("watch.lock")
}

/// Writes this process into the lock file that `f` holds, for the app's
/// stale-watcher check and Restart. The lock stays on the file.
pub fn write_holder(f: &mut std::fs::File, socket: &Path) -> Result<()> {
    use std::io::{Seek, Write};
    let exe = std::env::current_exe().ok().map(|e| e.canonicalize().unwrap_or(e));
    let me = WatchHolder {
        pid: std::process::id(),
        socket: socket.display().to_string(),
        started_at: skiff_core::session::now_ms(),
        version: Some(CLI_VERSION.to_string()),
        build: exe.as_deref().and_then(build_of),
        exe: exe.map(|e| e.display().to_string()),
    };
    f.set_len(0)?;
    f.seek(std::io::SeekFrom::Start(0))?;
    f.write_all(serde_json::to_string(&me)?.as_bytes())?;
    f.flush()?;
    Ok(())
}

/// Whether a watcher holds the lock for `socket`, and which one. Reads only:
/// a free lock is let go at once.
pub fn watch_status(socket: &Path) -> Result<WatchLock> {
    let path = watch_lock(socket);
    let Ok(f) = std::fs::File::options().read(true).write(true).open(&path) else {
        return Ok(WatchLock { held: false, holder: None });
    };
    match f.try_lock() {
        Ok(()) => Ok(WatchLock { held: false, holder: None }),
        Err(std::fs::TryLockError::WouldBlock) => {
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            Ok(WatchLock { held: true, holder: serde_json::from_str(text.trim()).ok() })
        }
        Err(std::fs::TryLockError::Error(e)) => Err(e).with_context(|| format!("lock {}", path.display())),
    }
}

/// How long [`restart_watch`] waits for a signalled watcher to let go of its lock.
pub const RESTART_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

/// True for the command line of `skiff watch`: a program named `skiff`
/// with `watch` as its first argument. The program is the first word, or
/// the first word after `--` (systemd-run on Linux).
pub fn is_watch_command(cmd: &str) -> bool {
    let words: Vec<&str> = cmd.split_whitespace().collect();
    let at = words.iter().position(|w| *w == "--").map_or(0, |i| i + 1);
    words.get(at).is_some_and(|w| Path::new(w).file_name().is_some_and(|n| n == "skiff")) && words.get(at + 1) == Some(&"watch")
}

/// The command line of a live process. `None` when it does not run.
#[cfg(unix)]
fn command_of(pid: u32) -> Option<String> {
    // SAFETY: signal 0 only checks that the process exists.
    if pid == 0 || unsafe { libc::kill(pid as libc::pid_t, 0) } != 0 {
        return None;
    }
    let out = std::process::Command::new("ps").args(["-o", "command=", "-p", &pid.to_string()]).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string()).filter(|c| !c.is_empty())
}

/// Why the lock holder is not ours to stop. `None`: it is a live `skiff
/// watch` for `socket`.
#[cfg(unix)]
fn not_stoppable(h: &WatchHolder, socket: &Path) -> Option<String> {
    if Path::new(&h.socket) != socket {
        return Some(format!("The watch lock names another socket ({}). Nothing was stopped.", h.socket));
    }
    match command_of(h.pid) {
        None => Some(format!("Process {} holds no watcher. Nothing was stopped.", h.pid)),
        Some(c) if !is_watch_command(&c) => Some(format!("Process {} is not skiff watch. Nothing was stopped.", h.pid)),
        Some(_) => None,
    }
}

/// Stops the watcher that holds the lock for `socket`, then starts one for
/// `session` as [`start_watch`] does. It signals only a live `skiff watch`
/// that wrote itself into the lock for this socket, with SIGTERM, and waits
/// up to [`RESTART_WAIT`] for the lock. It never sends SIGKILL, and it never
/// ends, types into or changes a session. True when it started a watcher.
#[cfg(unix)]
pub fn restart_watch(socket: &Path, cli: &Path, session: &str) -> Result<bool> {
    let st = watch_status(socket)?;
    if st.held {
        let Some(h) = st.holder else {
            bail!("A watcher holds the lock, but its pid is unknown. Stop it from a terminal, then restart.");
        };
        if let Some(why) = not_stoppable(&h, socket) {
            bail!(why);
        }
        // SAFETY: a plain syscall to a checked pid.
        if unsafe { libc::kill(h.pid as libc::pid_t, libc::SIGTERM) } != 0 {
            bail!("Cannot signal the watcher (pid {}): {}", h.pid, std::io::Error::last_os_error());
        }
        let until = std::time::Instant::now() + RESTART_WAIT;
        while watch_status(socket)?.held {
            if std::time::Instant::now() >= until {
                bail!("The watcher (pid {}) did not stop within 5 seconds. Nothing else changed.", h.pid);
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }
    start_watch(socket, cli, session)
}

/// What [`ensure_watch`] did.
#[derive(Debug, Clone, PartialEq)]
pub enum WatchOutcome {
    /// A watcher of this build, or of a newer version, holds the lock.
    Current,
    /// None ran. It started one.
    Started,
    /// It stopped an older or other-build watcher with SIGTERM and started one.
    Replaced { pid: u32 },
    /// An older watcher holds the lock and keeps running: it is not proven to
    /// be a `skiff watch` for this socket, or it did not stop. Why, for logs.
    Kept(String),
}

/// The watcher in `h` is not the build of `cli` at `version`: an older
/// version, a record from before the version, or the same version from
/// another build ([`build_of`]), a record from before the build included.
/// One computer runs one watcher per socket, and the app's build replaces
/// it. A newer version is never replaced. A `cli` that cannot be read
/// replaces nothing of its own version.
pub fn stale_holder(h: &WatchHolder, cli: &Path, version: &str) -> bool {
    let Some(v) = h.version.as_deref() else { return true };
    match skiff_core::compare_versions(v, version) {
        std::cmp::Ordering::Less => true,
        std::cmp::Ordering::Greater => false,
        std::cmp::Ordering::Equal => build_of(cli).is_some_and(|mine| h.build.as_deref() != Some(mine.as_str())),
    }
}

/// Makes one current watcher run for `session`, without the user. `cli` is
/// the bundled `skiff`, at `version`. A free lock gets a watcher. A stale
/// holder ([`stale_holder`]) is stopped and replaced when it is a live
/// `skiff watch` for `socket`: the pid its record names, or for a record
/// from before the pid, the one pid in `known`. `known` holds the watcher
/// pids skiffd registered for `socket`, from its peer credentials.
///
/// `tried` allows one attempt: it is set before the signal, and a set flag
/// stops any later signal, from this call or one that runs beside it. `None`
/// never signals. It sends SIGTERM only and waits up to [`RESTART_WAIT`]
/// for the lock; it never takes a held lock. A watcher it cannot prove, or
/// that does not stop, keeps running, and the outcome says why.
#[cfg(unix)]
pub fn ensure_watch(socket: &Path, cli: &Path, version: &str, session: &str, known: &[u32], tried: Option<&std::sync::atomic::AtomicBool>) -> Result<WatchOutcome> {
    let st = watch_status(socket)?;
    if !st.held {
        return Ok(if start_watch(socket, cli, session)? { WatchOutcome::Started } else { WatchOutcome::Current });
    }
    let pid = match &st.holder {
        Some(h) if !stale_holder(h, cli, version) => return Ok(WatchOutcome::Current),
        Some(h) => {
            if let Some(why) = not_stoppable(h, socket) {
                return Ok(WatchOutcome::Kept(why));
            }
            h.pid
        }
        None => {
            let mut live: Vec<u32> = known.iter().copied().filter(|p| command_of(*p).is_some_and(|c| is_watch_command(&c))).collect();
            live.sort();
            live.dedup();
            match live[..] {
                [pid] => pid,
                _ => return Ok(WatchOutcome::Kept("A watcher from an older Skiff holds the lock, and its process is not known. It keeps running.".into())),
            }
        }
    };
    // The attempt is taken before the signal, so a timeout keeps it taken.
    if tried.is_none_or(|t| t.swap(true, std::sync::atomic::Ordering::SeqCst)) {
        return Ok(WatchOutcome::Kept(format!("The watcher (pid {pid}) is older. A replacement was tried once this run, so it keeps running.")));
    }
    // SAFETY: a plain syscall to a checked pid.
    if unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) } != 0 {
        return Ok(WatchOutcome::Kept(format!("Cannot signal the watcher (pid {pid}): {}", std::io::Error::last_os_error())));
    }
    let until = std::time::Instant::now() + RESTART_WAIT;
    while watch_status(socket)?.held {
        if std::time::Instant::now() >= until {
            return Ok(WatchOutcome::Kept(format!("The watcher (pid {pid}) did not stop within 5 seconds. It keeps running.")));
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    start_watch(socket, cli, session)?;
    Ok(WatchOutcome::Replaced { pid })
}

/// A new, empty watch log. The last one moves to `<log>.1`, so the log of a
/// watcher that died is still there when the next one starts.
fn fresh_log(path: &Path) -> Result<std::fs::File> {
    if path.exists() {
        let mut old = path.as_os_str().to_owned();
        old.push(".1");
        std::fs::rename(path, &old).context("keep the last watch log")?;
    }
    std::fs::File::create(path).context("create the watch log")
}

/// A command for `program` that outlives the login that starts it, an SSH
/// login included. On Linux with a systemd user manager, it runs through
/// `systemd-run --user --scope --unit=<unit>`: the process leaves the
/// login's cgroup, so logind does not kill it at logout
/// (KillUserProcesses=yes). It gets DISPLAY, WAYLAND_DISPLAY,
/// DBUS_SESSION_BUS_ADDRESS and XDG_RUNTIME_DIR from the user manager, so
/// the sessions it starts see the desktop. A scope, not a service: a service
/// unit ends and kills the child when systemd-run returns. Else `program`
/// as is. Add the arguments to the result, then call [`detach`].
pub fn outliving(program: &Path, unit: &str) -> std::process::Command {
    #[cfg(target_os = "linux")]
    if let Some(env) = user_manager_env() {
        let mut cmd = std::process::Command::new("systemd-run");
        cmd.args(["--user", "--scope", "--quiet", "--collect"])
            .arg(format!("--unit={}", unit_name(unit)))
            .arg("--")
            .arg(program)
            .envs(env);
        return cmd;
    }
    let _ = unit;
    std::process::Command::new(program)
}

/// `unit-<socket name>-<8 hex digits of the socket path>`, in the
/// characters a unit name takes. The hash keeps two sockets with one file
/// name apart, such as a test's or a dev checkout's beside the live one:
/// a second `systemd-run` with a unit name in use fails to start.
pub fn unit_for(unit: &str, socket: &Path) -> String {
    let stem = socket.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let hash = socket.as_os_str().as_encoded_bytes().iter().fold(0xcbf29ce484222325u64, |h, &b| (h ^ b as u64).wrapping_mul(0x100000001b3));
    format!("{unit}-{stem}-{:08x}", hash as u32)
}

#[cfg(target_os = "linux")]
fn unit_name(unit: &str) -> String {
    unit.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' }).collect()
}

/// The desktop's variables from `systemctl --user show-environment`. `None`
/// when there is no user manager to ask.
#[cfg(target_os = "linux")]
fn user_manager_env() -> Option<Vec<(String, String)>> {
    const KEEP: [&str; 4] = ["DISPLAY", "WAYLAND_DISPLAY", "DBUS_SESSION_BUS_ADDRESS", "XDG_RUNTIME_DIR"];
    let mut cmd = std::process::Command::new("systemctl");
    cmd.args(["--user", "show-environment"]).stdin(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    // An SSH login can lack XDG_RUNTIME_DIR, and systemctl --user needs it.
    if std::env::var_os("XDG_RUNTIME_DIR").is_none() {
        let run = skiff_core::socket::runtime_dir();
        cmd.env("XDG_RUNTIME_DIR", run);
    }
    let out = cmd.output().ok().filter(|o| o.status.success())?;
    let text = String::from_utf8_lossy(&out.stdout);
    let env: Vec<(String, String)> = text
        .lines()
        .filter_map(|l| l.split_once('='))
        .filter(|(k, _)| KEEP.contains(k))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    // systemd-run reaches the manager through XDG_RUNTIME_DIR too.
    env.iter().any(|(k, _)| k == "XDG_RUNTIME_DIR").then_some(env)
}

/// Runs `cmd` in a session of its own, so a hangup of the terminal that
/// started it (an SSH logout) does not reach it. On Linux it also gets a
/// session keyring of its own, linked to the user keyring as pam_keyinit
/// does: the login's keyring is revoked at logout, and every program the
/// process starts would inherit the dead one.
pub fn detach(cmd: &mut std::process::Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: setsid and keyctl are plain syscalls and touch no memory.
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                #[cfg(target_os = "linux")]
                {
                    const JOIN_SESSION_KEYRING: libc::c_long = 1;
                    const LINK: libc::c_long = 8;
                    const USER_KEYRING: libc::c_long = -4;
                    const SESSION_KEYRING: libc::c_long = -3;
                    // A kernel without keys refuses both. That is no reason not to start.
                    if libc::syscall(libc::SYS_keyctl, JOIN_SESSION_KEYRING, 0 as libc::c_long) >= 0 {
                        libc::syscall(libc::SYS_keyctl, LINK, USER_KEYRING, SESSION_KEYRING);
                    }
                }
                Ok(())
            });
        }
    }
}

/// `[skiff_agent]` as Settings and the app's Skipper row read it, with the
/// folder's state and the command each task type starts. Reads the config
/// and the folder; changes nothing. The Host serves it (`skipper@1`).
pub fn settings(cfg: &Config) -> Result<skiff_host_api::HostSkipper> {
    use crate::agent_folder::{self, Kind};
    let (folder, mut folder_error) = match folder(cfg) {
        Ok(f) => (Some(f.display().to_string()), None),
        Err(e) => (None, Some(format!("{e:#}"))),
    };
    let configured = configured(cfg)?;
    let folder_status = match (&folder, agent_folder::kind(&configured)) {
        (None, _) => "refused",
        (_, Ok(Kind::Skiff)) => "ready",
        (_, Ok(Kind::New | Kind::Empty)) => {
            folder_error = Some(no_folder(&configured));
            "missing"
        }
        _ => "other",
    };
    let a = &cfg.skiff_agent;
    // The command lines come from this config, so the list builds the same ones skiffd would.
    let tasks = skiff_core::tasks::resolve(&a.tasks, &skiff_core::project::agents(), &configured)
        .into_iter()
        .map(|t| skiff_host_api::HostTaskType {
            name: t.name,
            agent: t.agent,
            model: t.model,
            file: t.file.display().to_string(),
            instructions: t.instructions,
            command: t.command,
            error: t.error,
        })
        .collect();
    let types = format!("{}/", skiff_core::tasks::tilde(&configured.join(skiff_core::tasks::TYPES)));
    Ok(skiff_host_api::HostSkipper {
        name: a.name.clone(),
        skipper_name: a.skipper_name.clone(),
        agent: a.agent.clone(),
        model: a.model.clone(),
        folder,
        folder_error,
        configured: configured.display().to_string(),
        folder_status: folder_status.into(),
        continue_on_restore: a.continue_on_restore,
        project_agent: a.project_agent.clone(),
        project_model: a.project_model.clone(),
        tasks,
        types,
        // A refused folder has no files to show.
        instructions: host_instructions(cfg).unwrap_or_default(),
    })
}

/// [`instructions`] as the Host serves them (`skipper@1`).
pub fn host_instructions(cfg: &Config) -> Result<Vec<skiff_host_api::HostInstruction>> {
    use skiff_host_api::{HostFileScope as H, HostInstruction};
    let status = |s: Status| match s {
        Status::None => "none",
        Status::Template => "template",
        Status::Edited => "edited",
        Status::EditedTemplateChanged => "edited-template-changed",
    };
    Ok(instructions(cfg)?
        .into_iter()
        .map(|f| HostInstruction {
            scope: match f.scope {
                Scope::Skipper => H::Skipper,
                Scope::Fleet(n) => H::Fleet(n),
                Scope::Project(n) => H::Project(n),
            },
            file: f.file,
            status: status(f.status).into(),
            modified: f.modified,
            size: f.size,
        })
        .collect())
}

/// The Skipper scope a Host request names. A task type's file is not a
/// Skipper's: it has no template.
pub fn scope_of(s: &skiff_host_api::HostFileScope) -> Result<Scope> {
    use skiff_host_api::HostFileScope as H;
    Ok(match s {
        H::Skipper => Scope::Skipper,
        H::Fleet(n) => Scope::Fleet(n.clone()),
        H::Project(n) => Scope::Project(n.clone()),
        H::Type(t) => bail!("The task type {t} has no template."),
    })
}

/// The path in a Skipper folder of the file `s` names, as its row shows it.
pub fn host_file(s: &skiff_host_api::HostFileScope) -> Result<String> {
    Ok(match s {
        skiff_host_api::HostFileScope::Type(t) => format!("types/{}.md", skiff_core::tasks::check_name(t)?),
        s => {
            let scope = scope_of(s)?;
            if let Scope::Fleet(n) | Scope::Project(n) = &scope {
                if n.is_empty() || n == "." || n == ".." || n.contains(['/', '\\']) {
                    bail!("the name {n:?} cannot name a folder");
                }
            }
            scope.file()
        }
    })
}

/// The largest instruction file `skipper_file@1` writes.
pub const FILE_MAX: usize = 1 << 20;

/// The full path of the file `s` names on this Host: a Skipper's, or a
/// task type's `types/<type>.md`.
fn host_path(cfg: &Config, s: &skiff_host_api::HostFileScope) -> Result<PathBuf> {
    match s {
        skiff_host_api::HostFileScope::Type(t) => Ok(folder(cfg)?.join("types").join(format!("{}.md", skiff_core::tasks::check_name(t)?))),
        s => {
            let scope = scope_of(s)?;
            scope.template(cfg, &folder(cfg)?)?;
            Ok(folder(cfg)?.join(scope.file()))
        }
    }
}

/// A file's revision: the hash of its bytes, or empty with no file.
fn revision(path: &Path) -> String {
    std::fs::read(path).map(|b| hash(&b)).unwrap_or_default()
}

/// Edit on this Host: the file `s` names, written first when it is
/// missing. A Skipper's comes from its template; a task type's is its
/// `# <type>` line.
pub fn edit_host_file(cfg: &Config, s: &skiff_host_api::HostFileScope) -> Result<PathBuf> {
    match s {
        skiff_host_api::HostFileScope::Type(t) => skiff_core::tasks::edit_file(&folder(cfg)?, t),
        s => edit_file(cfg, &scope_of(s)?),
    }
}

/// `skipper_file@1` read: the file and its revision. A missing file is
/// written first, as Edit does.
pub fn host_read(cfg: &Config, s: &skiff_host_api::HostFileScope) -> Result<skiff_host_api::HostSkipperFile> {
    let path = edit_host_file(cfg, s)?;
    let text = match std::fs::read_to_string(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        r => r.with_context(|| format!("read {}", path.display()))?,
    };
    Ok(skiff_host_api::HostSkipperFile { revision: revision(&path), text })
}

/// What a `skipper_file@1` write did.
pub enum Wrote {
    Done(skiff_host_api::HostSkipperFile),
    /// The file changed since the revision the client read.
    Changed,
}

/// `skipper_file@1` write: `text` replaces the file when it is still at
/// `rev`. A Skipper file keeps the hash of Skiff's last write, so the
/// person's text reads as edited.
pub fn host_write(cfg: &Config, s: &skiff_host_api::HostFileScope, text: &str, rev: &str) -> Result<Wrote> {
    if text.len() > FILE_MAX {
        bail!("The file is over 1 MiB.");
    }
    let _lock = lock()?;
    let path = host_path(cfg, s)?;
    if revision(&path) != rev {
        return Ok(Wrote::Changed);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    config::write_atomic(&path, text)?;
    Ok(Wrote::Done(skiff_host_api::HostSkipperFile { text: text.to_string(), revision: revision(&path) }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    /// The lock tests run one at a time: a process another test forks holds
    /// a copy of each open file until it execs, and with it a lock.
    static LOCKS: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn knows_the_watch_command() {
        assert!(is_watch_command("/Applications/Skiff.app/Contents/MacOS/skiff watch 4ac9e7a1"));
        assert!(is_watch_command("skiff watch abc"));
        assert!(is_watch_command("systemd-run --user --scope -- /usr/bin/skiff watch abc"));
        assert!(!is_watch_command("skiff ledger ls"));
        assert!(!is_watch_command("/usr/bin/skiffd"));
        assert!(!is_watch_command("vim skiff watch"));
        assert!(!is_watch_command(""));
    }

    /// A watcher that holds the lock reads as held, with what it wrote. A free
    /// lock reads as free, and the read does not keep it.
    #[test]
    fn reads_the_watch_lock_and_its_holder() {
        let _one = LOCKS.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("skiff-watch-lock-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("s.sock");
        assert_eq!(watch_status(&socket).unwrap(), WatchLock { held: false, holder: None });
        let mut f = std::fs::File::options().create(true).truncate(false).write(true).open(watch_lock(&socket)).unwrap();
        f.lock().unwrap();
        write_holder(&mut f, &socket).unwrap();
        let st = watch_status(&socket).unwrap();
        assert!(st.held);
        let h = st.holder.unwrap();
        assert_eq!((h.pid, h.socket.as_str()), (std::process::id(), socket.to_str().unwrap()));
        // A watcher from before the record: held, pid unknown.
        f.set_len(0).unwrap();
        assert_eq!(watch_status(&socket).unwrap(), WatchLock { held: true, holder: None });
        drop(f);
        assert!(!watch_status(&socket).unwrap().held);
        assert!(!watch_status(&socket).unwrap().held);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Restart signals nothing that is not a live `skiff watch` for this
    /// socket: here the holder is the test itself, and it keeps running.
    #[cfg(unix)]
    #[test]
    fn restart_stops_only_a_skiff_watch() {
        let _one = LOCKS.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("skiff-watch-restart-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("s.sock");
        let cli = dir.join("no-skiff");
        let mut f = std::fs::File::options().create(true).truncate(false).write(true).open(watch_lock(&socket)).unwrap();
        f.lock().unwrap();
        let err = restart_watch(&socket, &cli, "abc").unwrap_err().to_string();
        assert!(err.contains("pid is unknown"), "{err}");
        write_holder(&mut f, &socket).unwrap();
        let err = restart_watch(&socket, &cli, "abc").unwrap_err().to_string();
        assert!(err.contains("is not skiff watch"), "{err}");
        // Another socket's record is refused before any process check.
        let other = WatchHolder { pid: std::process::id(), socket: "/elsewhere.sock".into(), started_at: 0, version: None, exe: None, build: None };
        assert!(not_stoppable(&other, &socket).unwrap().contains("another socket"));
        assert!(watch_status(&socket).unwrap().held);
        drop(f);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A watcher that holds the lock and never registers is stopped with
    /// SIGTERM, and a new one starts. The stand-in shows as `skiff watch` in
    /// `ps`: python takes the lock, then execs `cat` named `skiff` on a FIFO
    /// named `watch`, which blocks. Skipped without python3 or mkfifo.
    #[cfg(unix)]
    #[test]
    fn restart_stops_a_stale_watcher_and_starts_one() {
        let _one = LOCKS.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("skiff-watch-stale-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("s.sock");
        if !std::process::Command::new("mkfifo").arg(dir.join("watch")).status().is_ok_and(|s| s.success()) {
            return;
        }
        let script = r#"
import fcntl, json, os, sys, time
lock, sock = sys.argv[1], sys.argv[2]
fd = os.open(lock, os.O_WRONLY | os.O_CREAT)
fcntl.flock(fd, fcntl.LOCK_EX)
os.ftruncate(fd, 0)
os.write(fd, json.dumps({"pid": os.getpid(), "socket": sock, "started_at": int(time.time() * 1000)}).encode())
os.set_inheritable(fd, True)
os.execv("/bin/cat", ["skiff", "watch"])
"#;
        let Ok(mut child) = std::process::Command::new("python3")
            .args(["-c", script])
            .arg(watch_lock(&socket))
            .arg(&socket)
            .current_dir(&dir)
            .spawn()
        else {
            return;
        };
        let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !watch_status(&socket).unwrap().held || !command_of(child.id()).is_some_and(|c| is_watch_command(&c)) {
            assert!(std::time::Instant::now() < until, "the stand-in watcher did not start");
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let st = watch_status(&socket).unwrap();
        assert_eq!(st.holder.map(|h| h.pid), Some(child.id()));
        // `true` stands in for the skiff that starts the new watcher.
        let started = restart_watch(&socket, Path::new("/usr/bin/true"), "abc").unwrap();
        assert!(started);
        let code = child.wait().unwrap();
        assert_eq!(std::os::unix::process::ExitStatusExt::signal(&code), Some(libc::SIGTERM));
        assert!(!watch_status(&socket).unwrap().held);
        assert!(socket.with_extension("watch.log").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Which holder is stale: no version (a watcher from before the field),
    /// an older one, or the same version from another build, or with no
    /// build. A newer version, or this build, is current, whatever
    /// executable path runs it. Old records still read.
    #[test]
    fn tells_a_stale_watcher_by_its_record() {
        let dir = std::env::temp_dir().join(format!("skiff-watch-build-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cli = dir.join("skiff");
        std::fs::write(&cli, "this build").unwrap();
        let mine = build_of(&cli).unwrap();
        let h = |version: Option<&str>, build: Option<&str>| WatchHolder {
            pid: 1,
            socket: "/s.sock".into(),
            started_at: 0,
            version: version.map(Into::into),
            exe: Some("/src/skiff/target/debug/skiff".into()),
            build: build.map(Into::into),
        };
        assert!(stale_holder(&h(None, None), &cli, "1.6.5"));
        assert!(stale_holder(&h(Some("1.6.4"), Some(&mine)), &cli, "1.6.5"));
        assert!(stale_holder(&h(Some("1.6.5"), Some("0123456789abcdef")), &cli, "1.6.5"));
        assert!(stale_holder(&h(Some("1.6.5"), None), &cli, "1.6.5"));
        assert!(!stale_holder(&h(Some("1.6.5"), Some(&mine)), &cli, "1.6.5"));
        assert!(!stale_holder(&h(Some("1.7.0"), Some("0123456789abcdef")), &cli, "1.6.5"));
        assert!(!stale_holder(&h(Some("1.6.5"), None), &dir.join("missing"), "1.6.5"));
        // A new build at the path is another build.
        std::fs::write(&cli, "the next build").unwrap();
        assert!(stale_holder(&h(Some("1.6.5"), Some(&mine)), &cli, "1.6.5"));
        let legacy: WatchHolder = serde_json::from_str(r#"{"pid":7,"socket":"/s.sock","started_at":3}"#).unwrap();
        assert_eq!((legacy.version, legacy.exe, legacy.build), (None, None, None));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A live `skiff watch` of this version from another build is stopped
    /// and replaced. One of this build is kept. Skipped without python3,
    /// which takes the lock.
    #[test]
    #[cfg(unix)]
    fn replaces_a_watcher_of_another_build_and_keeps_this_one() {
        let _one = LOCKS.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("skiff-watch-other-build-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let dir = dir.canonicalize().unwrap();
        let socket = dir.join("s.sock");
        // `skiff watch` to ps: a shell named skiff that reads the script `watch`.
        std::os::unix::fs::symlink("/bin/sh", dir.join("skiff")).unwrap();
        std::fs::write(dir.join("watch"), "while :; do sleep 0.1; done\n").unwrap();
        // `true` stands in for the app's skiff that starts the new watcher.
        let cli = Path::new("/usr/bin/true");
        let script = r#"
import fcntl, json, os, sys, time
lock, sock, exe, version, build = sys.argv[1:6]
fd = os.open(lock, os.O_WRONLY | os.O_CREAT)
fcntl.flock(fd, fcntl.LOCK_EX)
os.ftruncate(fd, 0)
os.write(fd, json.dumps({"pid": os.getpid(), "socket": sock, "started_at": int(time.time() * 1000), "version": version, "build": build}).encode())
os.set_inheritable(fd, True)
os.execv(exe, ["skiff", "watch"])
"#;
        let stand_in = |build: &str| {
            let child = std::process::Command::new("python3")
                .args(["-c", script])
                .arg(watch_lock(&socket))
                .arg(&socket)
                .arg(dir.join("skiff"))
                .args([CLI_VERSION, build])
                .current_dir(&dir)
                .spawn()
                .ok()?;
            let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while !watch_status(&socket).unwrap().held || !command_of(child.id()).is_some_and(|c| is_watch_command(&c)) {
                assert!(std::time::Instant::now() < until, "the stand-in watcher did not start");
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Some(child)
        };
        // This build: kept, and nothing is signalled.
        let Some(mut same) = stand_in(&build_of(cli).unwrap()) else { return };
        assert_eq!(ensure_watch(&socket, cli, CLI_VERSION, "abc", &[], Some(&AtomicBool::new(false))).unwrap(), WatchOutcome::Current);
        assert!(same.try_wait().unwrap().is_none());
        unsafe { libc::kill(same.id() as libc::pid_t, libc::SIGKILL) };
        let _ = same.wait();
        // Another build of the same version: stopped and replaced.
        let mut old = stand_in("0123456789abcdef").unwrap();
        let out = ensure_watch(&socket, cli, CLI_VERSION, "abc", &[], Some(&AtomicBool::new(false))).unwrap();
        assert_eq!(out, WatchOutcome::Replaced { pid: old.id() });
        let code = old.wait().unwrap();
        assert_eq!(std::os::unix::process::ExitStatusExt::signal(&code), Some(libc::SIGTERM));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A held lock with no record, and no registered `skiff watch` to name
    /// its process, stays as it is: nothing is signalled, nothing starts. The
    /// same with a known pid that is not a watcher, and with an older record
    /// whose pid is not `skiff watch`.
    #[test]
    #[cfg(unix)]
    fn keeps_a_watcher_it_cannot_prove() {
        let _one = LOCKS.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("skiff-watch-ensure-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("s.sock");
        let mut f = std::fs::File::options().create(true).truncate(false).write(true).open(watch_lock(&socket)).unwrap();
        f.lock().unwrap();
        let cli = Path::new("/usr/bin/true");
        for known in [&[][..], &[std::process::id()][..]] {
            let out = ensure_watch(&socket, cli, CLI_VERSION, "abc", known, Some(&AtomicBool::new(false))).unwrap();
            assert!(matches!(out, WatchOutcome::Kept(_)), "{out:?}");
        }
        // This process, as a holder from an older CLI: not a watcher, so it stays.
        write_holder(&mut f, &socket).unwrap();
        let old = WatchHolder { version: Some("0.0.1".into()), ..watch_status(&socket).unwrap().holder.unwrap() };
        f.set_len(0).unwrap();
        std::io::Seek::seek(&mut f, std::io::SeekFrom::Start(0)).unwrap();
        std::io::Write::write_all(&mut f, serde_json::to_string(&old).unwrap().as_bytes()).unwrap();
        let out = ensure_watch(&socket, cli, CLI_VERSION, "abc", &[], Some(&AtomicBool::new(false))).unwrap();
        assert!(matches!(&out, WatchOutcome::Kept(w) if w.contains("is not skiff watch")), "{out:?}");
        // This CLI's own record: current, and nothing changes.
        write_holder(&mut f, &socket).unwrap();
        let me = std::env::current_exe().unwrap();
        assert_eq!(ensure_watch(&socket, &me, CLI_VERSION, "abc", &[], Some(&AtomicBool::new(false))).unwrap(), WatchOutcome::Current);
        assert!(watch_status(&socket).unwrap().held);
        assert!(!socket.with_extension("watch.log").exists());
        drop(f);
        // A free lock gets a watcher.
        assert_eq!(ensure_watch(&socket, cli, CLI_VERSION, "abc", &[], Some(&AtomicBool::new(false))).unwrap(), WatchOutcome::Started);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A project Skipper needs a skiffd that marks it. The check is by
    /// version, before anything starts.
    #[test]
    fn an_older_skiffd_cannot_start_a_project_skipper() {
        assert!(too_old_for_project("0.6.24"));
        assert!(too_old_for_project("0.6.21"));
        assert!(!too_old_for_project(PROJECT_SKIPPER_SINCE));
        assert!(!too_old_for_project(skiff_core::VERSION), "this skiffd marks project Skippers");
        assert!(!too_old_for_project("0.7.0"));
    }

    /// A project Skipper is named after its project and the Skiff agent's
    /// name, so no two sessions share a name.
    #[test]
    fn a_project_skipper_takes_its_project_and_the_agents_name() {
        // The two stored names stay independent.
        let cfg = skiff_core::config::parse("[skiff_agent]\nname = \"Skipper\"\n").unwrap();
        assert_eq!(project_name(&cfg, "gesher"), "gesher Skipper");
        assert_eq!(name(&cfg), "Skipper");
        let cfg = skiff_core::config::parse("[skiff_agent]\nname = \"Captain\"\n").unwrap();
        assert_eq!(project_name(&cfg, "skiff"), "skiff Skipper");
        assert_eq!(name(&cfg), "Captain");
        let cfg = skiff_core::config::parse("[skiff_agent]\nname = \"Captain\"\nskipper_name = \"Mate\"\n").unwrap();
        assert_eq!(project_name(&cfg, "skiff"), "skiff Mate");
        let cfg = skiff_core::config::parse("").unwrap();
        assert_eq!(project_name(&cfg, "p"), "p Skipper");
        assert_eq!(name(&cfg), "Admiral");
    }

    /// Each socket gets a scope of its own, so a test daemon named
    /// skiffd.sock beside the live one does not fail to start its watcher.
    #[test]
    fn each_socket_gets_its_own_scope_unit() {
        let a = unit_for("skiff-watch", Path::new("/tmp/a/skiffd.sock"));
        let b = unit_for("skiff-watch", Path::new("/tmp/b/skiffd.sock"));
        assert!(a.starts_with("skiff-watch-skiffd-") && a.len() == "skiff-watch-skiffd-".len() + 8, "{a}");
        assert_ne!(a, b);
        assert_eq!(a, unit_for("skiff-watch", Path::new("/tmp/a/skiffd.sock")));
    }

    /// One attempt per flag. A stale watcher that ignores SIGTERM holds the
    /// lock past the wait: the outcome is Kept, and it keeps running. Two
    /// calls at once, as from a start and a restore, and every call after
    /// the timeout, share the flag: the watcher gets one SIGTERM in all, and
    /// no second watcher starts. Skipped without python3, which takes the lock.
    #[test]
    #[cfg(unix)]
    fn a_watcher_that_does_not_stop_gets_one_signal() {
        let _one = LOCKS.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("skiff-watch-once-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let dir = dir.canonicalize().unwrap();
        let socket = dir.join("s.sock");
        // `skiff watch` to ps: a shell named skiff that reads the script `watch`.
        std::os::unix::fs::symlink("/bin/sh", dir.join("skiff")).unwrap();
        std::fs::write(dir.join("watch"), "trap 'echo term >> count' TERM\nwhile :; do sleep 0.1; done\n").unwrap();
        let script = r#"
import fcntl, json, os, sys, time
lock, sock, exe = sys.argv[1], sys.argv[2], sys.argv[3]
fd = os.open(lock, os.O_WRONLY | os.O_CREAT)
fcntl.flock(fd, fcntl.LOCK_EX)
os.ftruncate(fd, 0)
os.write(fd, json.dumps({"pid": os.getpid(), "socket": sock, "started_at": int(time.time() * 1000)}).encode())
os.set_inheritable(fd, True)
os.execv(exe, ["skiff", "watch"])
"#;
        let Ok(mut child) = std::process::Command::new("python3")
            .args(["-c", script])
            .arg(watch_lock(&socket))
            .arg(&socket)
            .arg(dir.join("skiff"))
            .current_dir(&dir)
            .spawn()
        else {
            return;
        };
        let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !watch_status(&socket).unwrap().held || !command_of(child.id()).is_some_and(|c| is_watch_command(&c)) {
            assert!(std::time::Instant::now() < until, "the stand-in watcher did not start");
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let tried = AtomicBool::new(false);
        let cli = Path::new("/usr/bin/true");
        let both = std::thread::scope(|s| {
            let calls: Vec<_> = (0..2).map(|_| s.spawn(|| ensure_watch(&socket, cli, CLI_VERSION, "abc", &[], Some(&tried)).unwrap())).collect();
            calls.into_iter().map(|c| c.join().unwrap()).collect::<Vec<_>>()
        });
        let later = ensure_watch(&socket, cli, CLI_VERSION, "abc", &[], Some(&tried)).unwrap();
        // A trap runs between sleeps: give a late one time to show.
        std::thread::sleep(std::time::Duration::from_millis(300));
        let signals = std::fs::read_to_string(dir.join("count")).unwrap_or_default().lines().count();
        let running = child.try_wait().unwrap().is_none();
        let held = watch_status(&socket).unwrap().held;
        unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGKILL) };
        let _ = child.wait();
        let kept = |o: &WatchOutcome, what: &str| matches!(o, WatchOutcome::Kept(w) if w.contains(what));
        assert_eq!(both.iter().filter(|o| kept(o, "did not stop")).count(), 1, "{both:?}");
        assert_eq!(both.iter().filter(|o| kept(o, "tried once")).count(), 1, "{both:?}");
        assert!(kept(&later, "tried once"), "{later:?}");
        assert_eq!(signals, 1);
        assert!(running && held, "the watcher keeps running and holds the lock");
        assert!(!socket.with_extension("watch.log").exists(), "a second watcher started");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_new_watch_log_keeps_the_last_one() {
        let dir = std::env::temp_dir().join(format!("skiff-watch-log-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("s.watch.log");
        std::io::Write::write_all(&mut fresh_log(&log).unwrap(), b"first").unwrap();
        fresh_log(&log).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("s.watch.log.1")).unwrap(), "first");
        assert_eq!(std::fs::read_to_string(&log).unwrap(), "");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn writes_the_template_and_fills_in_only_what_is_missing() {
        let tmp = std::env::temp_dir().join(format!("skiff-agent-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::env::set_var("SKIFF_CONFIG", tmp.join("config/projects.toml"));
        let dir = setup(&tmp.join("agent"), "Skipper").unwrap();
        let agents = std::fs::read_to_string(dir.join("CLAUDE.md")).unwrap();
        assert!(agents.contains("You are Skipper, the Skiff agent."));
        assert_eq!(std::fs::read_to_string(dir.join("GEMINI.md")).unwrap(), agents);
        assert!(dir.join(".claude/skills/skiff-cli/SKILL.md").is_file());
        // The ledger is the CLI's ledger.db, made on first use.
        assert!(!dir.join("ledger.md").exists() && dir.join("results").is_dir());
        // A second run fills in what is missing and keeps the user's edits.
        std::fs::write(dir.join("AGENTS.md"), "mine").unwrap();
        std::fs::remove_dir(dir.join("results")).unwrap();
        std::fs::remove_file(dir.join("GEMINI.md")).unwrap();
        setup(&dir, "Skipper").unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("AGENTS.md")).unwrap(), "mine");
        assert_eq!(std::fs::read_to_string(dir.join("GEMINI.md")).unwrap(), "mine");
        assert!(dir.join("results").is_dir() && dir.join(MARKER).is_file());
        // A folder with other files and no marker is refused.
        let other = tmp.join("other");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(other.join("notes.md"), "x").unwrap();
        assert!(setup(&other, "Skipper").unwrap_err().to_string().contains("holds other files"));
        std::fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn refuses_a_folder_that_mixes_with_other_work() {
        let home = std::env::temp_dir().join(format!("skiff-agent-home-{}", std::process::id()));
        let project = home.join("code/app");
        std::fs::create_dir_all(&project).unwrap();
        let cfg = config::parse(&format!("[[project]]\nname = \"app\"\npath = \"{}\"\n", project.display())).unwrap();
        for bad in [home.clone(), PathBuf::from("/"), home.join("code"), project.clone(), project.join("sub"), PathBuf::from("rel/x")] {
            assert!(check(&bad, &home, &cfg).is_err(), "{}", bad.display());
        }
        assert!(check(&home.join("skiff-agent"), &home, &cfg).is_ok());
        assert!(check(&home.join(".skiff-agent"), &home, &cfg).is_ok());
        // Any other dot-folder is still a folder like any other.
        assert!(check(&home.join(".skiff"), &home, &cfg).is_ok());
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn skill_files_match_the_repo() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.agents/skills/skiff-cli");
        let mut found = Vec::new();
        let mut stack = vec![dir.clone()];
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(d).unwrap() {
                let p = e.unwrap().path();
                if p.is_dir() {
                    stack.push(p);
                } else {
                    found.push(p.strip_prefix(&dir).unwrap().to_string_lossy().into_owned());
                }
            }
        }
        found.sort();
        let mut listed: Vec<String> = SKILL.iter().map(|(f, _, _)| f.to_string()).collect();
        listed.sort();
        assert_eq!(found, listed, "add the new skill files to SKILL");
    }

    #[test]
    fn continue_lines() {
        assert_eq!(continue_line("claude", "claude --model x").as_deref(), Some("claude --model x --continue"));
        assert_eq!(continue_line("codex", "codex --yolo").as_deref(), Some("codex resume --last --yolo"));
        assert_eq!(continue_line("codex", "codex").as_deref(), Some("codex resume --last"));
        assert_eq!(continue_line("mine", "mine").as_deref(), None);
        // The person's bypass flag stays on resume.
        assert_eq!(
            continue_line("claude", "claude --dangerously-skip-permissions").as_deref(),
            Some("claude --dangerously-skip-permissions --continue")
        );
        assert_eq!(continue_line("gemini", "agy --dangerously-skip-permissions").as_deref(), Some("agy --dangerously-skip-permissions --continue"));
        // The built-in line resumes with the agent's own prompts.
        assert_eq!(continue_line("gemini", "agy").as_deref(), Some("agy --continue"));
    }

    #[test]
    fn launch_starts_the_persons_command() {
        let agent = |command: &str| AgentInfo {
            id: "codex".into(),
            command: command.into(),
            default_command: "codex".into(),
            custom: false,
            installed: true,
            enabled: true,
            used_by: None,
        };
        let s = spec(&with_model(&agent("codex"), None), "Skipper", PathBuf::from("/tmp"));
        assert_eq!((s.command.as_deref(), s.args.len()), (Some("codex"), 0));
        let s = spec(&with_model(&agent(" codex --yolo "), Some("gpt-5")), "Skipper", PathBuf::from("/tmp"));
        assert_eq!((s.command.as_deref(), s.args), (Some("codex"), vec!["--yolo".to_string(), "-m".into(), "gpt-5".into()]));
        // An agent with no model flag takes its model in its command.
        let mut gemini = agent("gemini");
        gemini.id = "gemini".into();
        assert_eq!(with_model(&gemini, Some("pro")), "gemini");
    }
}
