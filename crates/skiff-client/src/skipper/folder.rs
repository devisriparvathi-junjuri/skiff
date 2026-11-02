//! Skipper folder, setup, instruction marker and skill helpers.

use super::*;

/// The folder's instructions. `{{name}}` becomes the agent's name.
const TEMPLATE: &str = include_str!("../../agent-template/AGENTS.md.tmpl");

/// The `skiff-cli` skill, file by file: (path, bytes, runnable).
/// `skill_files_match_the_repo` keeps this list in step with the folder.
const SKILL: &[(&str, &[u8], bool)] = &[
    ("SKILL.md", include_bytes!("../../../../.agents/skills/skiff-cli/SKILL.md"), false),
    ("scripts/session_report.py", include_bytes!("../../../../.agents/skills/skiff-cli/scripts/session_report.py"), true),
];

/// The marker setup leaves, so a later run knows the folder is the agent's.
/// Its first line is `1`. Each line after it is the hash of an instruction
/// file as Skiff last wrote it, then its path in the folder:
/// `<hash> projects/gesher/SKIPPER.md`. So Skiff tells a file it wrote from
/// one the person edited, with no second copy of the file.
pub(crate) const MARKER: &str = ".skiff-agent";

/// The agents' own homes, from the home folder. Skiff never writes there.
const AGENT_HOMES: [(&str, &str); 5] =
    [(".claude", "claude"), (".codex", "codex"), (".gemini", "gemini"), (".config/opencode", "opencode"), (".grok", "grok")];

/// The files that make a folder a Hermes profile.
const HERMES_FILES: [&str; 5] = ["config.yaml", "SOUL.md", "state.db", "auth.json", "profile.yaml"];

/// The Skiff agent's instructions, the base of every Skipper.
pub const AGENTS_FILE: &str = "AGENTS.md";

#[cfg(unix)]
fn link(target: &Path, at: &Path) -> Result<()> {
    std::os::unix::fs::symlink(target, at).with_context(|| format!("link {}", at.display()))
}

#[cfg(not(unix))]
fn link(_target: &Path, _at: &Path) -> Result<()> {
    bail!("not supported on this platform")
}


/// `AGENTS.md` as setup writes it for `name`.
pub(crate) fn template(name: &str) -> String {
    TEMPLATE.replace("{{name}}", name)
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

/// The 16 hex digits of a 64-bit FNV-1a hash of `bytes`, as [`build_of`] makes.
pub(super) fn hash(bytes: &[u8]) -> String {
    format!("{:016x}", bytes.iter().fold(0xcbf29ce484222325u64, |h, &b| (h ^ b as u64).wrapping_mul(0x100000001b3)))
}

/// The hashes in the marker of `dir`, by path in the folder.
pub(super) fn written(dir: &Path) -> std::collections::BTreeMap<String, String> {
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
pub(super) fn write_file(dir: &Path, rel: &str, text: &str) -> Result<()> {
    let path = dir.join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    std::fs::write(&path, text).with_context(|| format!("write {}", path.display()))?;
    record(dir, rel, Some(&hash(text.as_bytes())))
}

/// At a Skipper's start: writes `rel` when it is missing, and again when its
/// status is template and the template changed. An edited file stays.
pub(super) fn refresh(dir: &Path, rel: &str, text: &str) -> Result<()> {
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
