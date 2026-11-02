//! Project and fleet Skipper setup and instructions.

use super::*;

/// A project Skipper's `SKIPPER.md`: the project section only, as
/// [`TEMPLATE`] is the base of every Skipper. `{{project}}`, `{{path}}`,
/// `{{dir}}` and `{{folder}}` become the project, its folder, the
/// Skipper's folder and the Skiff agent's folder.
const PROJECT_TEMPLATE: &str = include_str!("../../agent-template/SKIPPER-project.md.tmpl");

/// A fleet Skipper's `SKIPPER.md`: the fleet section only. `{{fleet}}`,
/// `{{dir}}` and `{{folder}}` become the fleet, the Skipper's folder and
/// the Skiff agent's folder. It names no project: the fleet Skipper reads
/// them from `skiff projects --json`, so the file never goes stale.
const FLEET_TEMPLATE: &str = include_str!("../../agent-template/SKIPPER-fleet.md.tmpl");

/// The name of a project Skipper's file in its folder.
pub const SKIPPER_FILE: &str = "SKIPPER.md";

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
pub(super) fn setup_project(cfg: &Config, p: &ProjectConfig) -> Result<PathBuf> {
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

#[cfg(test)]
mod tests {
    use super::super::host_file;
    use super::*;

    // SPLIT-SCAFFOLD
    #[test]
    fn scopes_keep_their_files_templates_and_host_paths() {
        let cfg = config::parse(
            "[skiff_agent]\nfolder = \"/tmp/skiff-agent\"\n\n[[project]]\nname = \"api\"\npath = \"/tmp/api\"\nfleet = \"Harbor\"\n",
        )
        .unwrap();
        let scopes = [
            (Scope::Skipper, skiff_host_api::HostFileScope::Skipper, "AGENTS.md"),
            (Scope::Fleet("Harbor".into()), skiff_host_api::HostFileScope::Fleet("Harbor".into()), "fleets/Harbor/SKIPPER.md"),
            (Scope::Project("api".into()), skiff_host_api::HostFileScope::Project("api".into()), "projects/api/SKIPPER.md"),
        ];
        for (scope, host, file) in scopes {
            assert_eq!(scope.file(), file);
            assert_eq!(host_file(&host).unwrap(), file);
            assert!(!scope.template(&cfg, Path::new("/tmp/skiff-agent")).unwrap().is_empty());
        }
    }
}
