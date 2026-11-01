use anyhow::Result;
use super::persistence::edit_table;

/// Writes `[agents] enabled = [...]`, keeping the rest of the file.
pub fn set_enabled_agents(enabled: &[String]) -> Result<()> {
    let mut list = toml_edit::Array::new();
    for a in enabled {
        list.push(a.as_str());
    }
    edit_agents(|t| {
        t["enabled"] = toml_edit::value(list);
        Ok(())
    })
}

/// Sets `[agents.commands] <id> = "<command>"`. An empty command removes the
/// override, so the default applies again. An id Skiff does not know is a
/// custom agent: setting it adds the agent, and an empty command removes it.
/// Setting a removed default brings it back.
pub fn set_agent_command(id: &str, command: &str) -> Result<()> {
    let id = agent_name(id)?;
    let command = command.trim();
    edit_agents(|t| {
        if command.is_empty() {
            commands(t)?.remove(id);
            if !is_known(id) {
                enabled_list(t, |l| l.retain(|v| v.as_str() != Some(id)));
            }
            return Ok(());
        }
        let new = !is_known(id) && !commands(t)?.contains_key(id);
        commands(t)?[id] = toml_edit::value(command);
        unremove(t, id);
        // A new agent starts on.
        if new {
            enabled_list(t, |l| l.push(id));
        }
        Ok(())
    })
}

/// Takes an agent out of the list. A default goes to `[agents] removed`, so
/// `restore_agents` can bring it back. An agent that something starts
/// ([`crate::project::used_by`]) is refused.
pub fn remove_agent(id: &str) -> Result<()> {
    // Something starts it: it stays, and the line says what.
    if let Some(line) = crate::project::used_by(&super::load()?, id.trim()) {
        anyhow::bail!("{line}. {} stays.", id.trim());
    }
    edit_agents(|t| {
        drop_agent(t, id)?;
        Ok(())
    })
}

/// Gives an agent a new name. It keeps its command and its switch.
pub fn rename_agent(id: &str, name: &str) -> Result<()> {
    let name = agent_name(name)?;
    if name == id {
        return Ok(());
    }
    let list = crate::project::agents();
    let Some(cur) = list.iter().find(|a| a.id == id) else {
        anyhow::bail!("no agent named {id}");
    };
    if list.iter().any(|a| a.id == name) {
        anyhow::bail!("an agent named {name} exists");
    }
    let command = cur.command.clone();
    edit_agents(|t| {
        let was_on = drop_agent(t, id)?;
        commands(t)?[name] = toml_edit::value(command.as_str());
        unremove(t, name);
        if was_on {
            enabled_list(t, |l| l.push(name));
        }
        Ok(())
    })?;
    // Task types that start the agent follow it to its new name.
    if super::load()?.skiff_agent.tasks.0.iter().any(|(_, r)| r.agent == id) {
        edit_table("skiff_agent", |t| {
            for (_, row) in tasks(t)?.iter_mut() {
                if let Some(r) = row.as_value_mut().and_then(|v| v.as_inline_table_mut()) {
                    if r.get("agent").and_then(|v| v.as_str()) == Some(id) {
                        r.insert("agent", name.into());
                    }
                }
            }
            Ok(())
        })?;
    }
    Ok(())
}

/// Brings back every removed default.
pub fn restore_agents() -> Result<()> {
    edit_agents(|t| {
        t.remove("removed");
        Ok(())
    })
}

fn agent_name(id: &str) -> Result<&str> {
    let id = id.trim();
    if id.is_empty() {
        anyhow::bail!("an agent needs a name");
    }
    Ok(id)
}

fn is_known(id: &str) -> bool {
    crate::project::KNOWN_AGENTS.iter().any(|&(k, _)| k == id)
}

/// Removes the agent's command and switch. True when the enabled list had it.
fn drop_agent(t: &mut toml_edit::Table, id: &str) -> Result<bool> {
    commands(t)?.remove(id);
    let mut was_on = false;
    enabled_list(t, |l| {
        was_on = l.iter().any(|v| v.as_str() == Some(id));
        l.retain(|v| v.as_str() != Some(id));
    });
    if is_known(id) {
        let removed = t
            .entry("removed")
            .or_insert_with(|| toml_edit::value(toml_edit::Array::new()))
            .as_array_mut()
            .ok_or_else(|| anyhow::anyhow!("`agents.removed` is not a list"))?;
        if !removed.iter().any(|v| v.as_str() == Some(id)) {
            removed.push(id);
        }
    }
    Ok(was_on)
}

fn unremove(t: &mut toml_edit::Table, id: &str) {
    if let Some(l) = t.get_mut("removed").and_then(|e| e.as_array_mut()) {
        l.retain(|v| v.as_str() != Some(id));
    }
}

/// Edits `[agents] enabled` when it is set. Absent means every installed agent.
fn enabled_list(t: &mut toml_edit::Table, f: impl FnOnce(&mut toml_edit::Array)) {
    if let Some(l) = t.get_mut("enabled").and_then(|e| e.as_array_mut()) {
        f(l);
    }
}

fn commands(t: &mut toml_edit::Table) -> Result<&mut toml_edit::Table> {
    t.entry("commands")
        .or_insert_with(|| {
            let mut c = toml_edit::Table::new();
            c.set_implicit(false);
            toml_edit::Item::Table(c)
        })
        .as_table_mut()
        .ok_or_else(|| anyhow::anyhow!("`agents.commands` is not a table"))
}

/// Sets or removes `icon` on the named project. `None` removes the key, so
/// Sets `[appearance] theme`, or removes it for the default.
pub fn set_app_theme(theme: Option<&str>) -> Result<()> {
    super::client::edit_table("appearance", |t| {
        match theme {
            Some(v) => t["theme"] = toml_edit::value(v),
            None => {
                t.remove("theme");
            }
        }
        Ok(())
    })
}

/// Sets `[appearance] font_size`, or removes it for the default.
pub fn set_font_size(size: Option<u8>) -> Result<()> {
    super::client::edit_table("appearance", |t| {
        match size {
            Some(v) => t["font_size"] = toml_edit::value(i64::from(v)),
            None => {
                t.remove("font_size");
            }
        }
        Ok(())
    })
}

/// Sets `[appearance] mission_filter` to `running` or `all`. `all` is
/// written out: an absent key means `running`.
pub fn set_mission_filter(running: bool) -> Result<()> {
    super::client::edit_table("appearance", |t| {
        t["mission_filter"] = toml_edit::value(if running { "running" } else { "all" });
        Ok(())
    })
}

/// Sets `[appearance] language` to a code such as `es`, or removes it for
/// the system language.
pub fn set_language(language: Option<&str>) -> Result<()> {
    if let Some(v) = language.filter(|v| v.is_empty() || !v.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')) {
        anyhow::bail!("not a language code: {v}");
    }
    edit_table("appearance", |t| {
        match language {
            Some(v) => t["language"] = toml_edit::value(v),
            None => {
                t.remove("language");
            }
        }
        Ok(())
    })
}

/// The values `[appearance] menu_layout` takes.
pub const MENU_LAYOUTS: [&str; 2] = ["header-bar", "menu-bar"];

/// Sets `[appearance] menu_layout`, or removes it for the automatic choice.
pub fn set_menu_layout(layout: Option<&str>) -> Result<()> {
    if let Some(v) = layout.filter(|v| !MENU_LAYOUTS.contains(v)) {
        anyhow::bail!("unknown menu layout: {v}");
    }
    super::client::edit_table("appearance", |t| {
        match layout {
            Some(v) => t["menu_layout"] = toml_edit::value(v),
            None => {
                t.remove("menu_layout");
            }
        }
        Ok(())
    })
}

/// The keys `[open]` takes.
pub const OPEN_KEYS: [&str; 5] = ["diff", "text", "markdown", "html", "image"];

/// Sets whether selecting this file kind opens its command in the peek.
pub fn set_open_peek(key: &str, peek: bool) -> Result<()> {
    if !OPEN_KEYS.contains(&key) || key == "diff" || key == "image" {
        anyhow::bail!("unknown file kind: {key}");
    }
    super::client::edit_table("open", |t| {
        let mut kinds = t.get("peek").and_then(|v| v.as_array()).cloned().unwrap_or_default();
        kinds.retain(|v| v.as_str() != Some(key));
        if peek { kinds.push(key); }
        t["peek"] = toml_edit::value(kinds);
        Ok(())
    })
}

/// Sets one `[open]` command, or removes it for the default.
pub fn set_open(key: &str, command: Option<&str>) -> Result<()> {
    if !OPEN_KEYS.contains(&key) {
        anyhow::bail!("unknown [open] key: {key}");
    }
    super::client::edit_table("open", |t| {
        match command {
            Some(v) => t[key] = toml_edit::value(v),
            None => {
                t.remove(key);
            }
        }
        Ok(())
    })
}

/// One `[skiff_agent]` key. `None` removes it, so its default applies.
pub enum SkiffAgentKey<'a> {
    Name(Option<&'a str>),
    SkipperName(Option<&'a str>),
    Agent(Option<&'a str>),
    /// One word, as a task type's model.
    Model(Option<&'a str>),
    Folder(Option<&'a std::path::Path>),
    ContinueOnRestore(bool),
    ProjectAgent(Option<&'a str>),
    ProjectModel(Option<&'a str>),
    /// Auto flush's age in days. `0`: off.
    CacheDays(u32),
}

/// Sets one `[skiff_agent]` key, keeping the rest of the file. The folder is
/// written as the absolute path it is given.
pub fn set_skiff_agent(key: SkiffAgentKey) -> Result<()> {
    let text = |v: Option<&str>| v.map(str::trim).filter(|v| !v.is_empty()).map(str::to_owned);
    let model = |v: Option<&str>| crate::tasks::check_model(v).map(|m| m.map(toml_edit::value));
    let model = match &key {
        SkiffAgentKey::Model(v) | SkiffAgentKey::ProjectModel(v) => model(*v)?,
        _ => None,
    };
    edit_table("skiff_agent", |t| {
        let (k, v) = match key {
            SkiffAgentKey::Name(v) => ("name", text(v).map(toml_edit::value)),
            SkiffAgentKey::SkipperName(v) => ("skipper_name", text(v).map(toml_edit::value)),
            SkiffAgentKey::Agent(v) => ("agent", text(v).map(toml_edit::value)),
            // The full path, never `~` or a relative one: the folder is the same wherever Skiff starts.
            SkiffAgentKey::Folder(v) => ("folder", v.map(|p| toml_edit::value(p.to_string_lossy().into_owned()))),
            SkiffAgentKey::Model(_) => ("model", model),
            // Off is the default: the key goes.
            SkiffAgentKey::ContinueOnRestore(on) => ("continue_on_restore", on.then(|| toml_edit::value(true))),
            // None: the Skipper's own agent.
            SkiffAgentKey::ProjectAgent(v) => ("project_agent", text(v).map(toml_edit::value)),
            SkiffAgentKey::ProjectModel(_) => ("project_model", model),
            // The default: the key goes.
            SkiffAgentKey::CacheDays(n) => ("cache_days", (n != crate::config::CACHE_DAYS).then(|| toml_edit::value(i64::from(n)))),
        };
        match v {
            Some(v) => t[k] = v,
            None => {
                t.remove(k);
            }
        }
        Ok(())
    })
}

/// Adds `[skiff_agent.tasks] <name> = { agent, model }` at the end of the
/// table. Refuses a name that is there.
pub fn add_task_type(name: &str, agent: &str, model: Option<&str>) -> Result<()> {
    let name = crate::tasks::check_name(name)?;
    let row = task_row(agent, model)?;
    edit_table("skiff_agent", |t| {
        let tasks = tasks(t)?;
        if tasks.contains_key(name) {
            anyhow::bail!("A task type named {name} exists. Change it in its row.");
        }
        tasks.insert(name, toml_edit::value(row));
        Ok(())
    })
}

/// Changes the agent and model of a task type, in its place. An empty model
/// takes the key out: the agent's default. An agent that takes its model in
/// its command saves no model.
pub fn set_task_type(name: &str, agent: &str, model: Option<&str>) -> Result<()> {
    let row = task_row(agent, model)?;
    edit_table("skiff_agent", |t| {
        let tasks = tasks(t)?;
        if !tasks.contains_key(name) {
            anyhow::bail!("There is no task type {name}.");
        }
        tasks.insert(name, toml_edit::value(row));
        Ok(())
    })
}

/// Gives a task type a new name. It keeps its place, its agent and its model.
pub fn rename_task_type(name: &str, to: &str) -> Result<()> {
    let to = crate::tasks::check_name(to)?;
    if to == name {
        return Ok(());
    }
    edit_table("skiff_agent", |t| {
        let tasks = tasks(t)?;
        if !tasks.contains_key(name) {
            anyhow::bail!("There is no task type {name}.");
        }
        if tasks.contains_key(to) {
            anyhow::bail!("A task type named {to} exists. {name} keeps its name.");
        }
        // Take every row out and put them back in order, with the new name.
        let keys: Vec<String> = tasks.iter().map(|(k, _)| k.to_string()).collect();
        let rows: Vec<(String, toml_edit::Item)> = keys.into_iter().filter_map(|k| tasks.remove(&k).map(|v| (k, v))).collect();
        for (k, v) in rows {
            tasks.insert(if k == name { to } else { &k }, v);
        }
        Ok(())
    })
}

/// Takes a task type out. The last one takes the table out too.
pub fn remove_task_type(name: &str) -> Result<()> {
    edit_table("skiff_agent", |t| {
        tasks(t)?.remove(name);
        if t.get("tasks").and_then(|v| v.as_table()).is_some_and(|v| v.is_empty()) {
            t.remove("tasks");
        }
        Ok(())
    })
}

/// `{ agent = "claude", model = "opus" }` for an agent Skiff lists.
fn task_row(agent: &str, model: Option<&str>) -> Result<toml_edit::InlineTable> {
    let agent = agent.trim();
    let Some(a) = crate::project::agents().into_iter().find(|a| a.id == agent) else {
        anyhow::bail!("There is no agent {agent}. Add it in Settings › Agents.");
    };
    // An agent that takes its model in its command saves none, whatever came with it.
    let takes_model = !a.custom && crate::tasks::model_flag(&a.id).is_some();
    let model = if takes_model { crate::tasks::check_model(model)? } else { None };
    let mut row = toml_edit::InlineTable::new();
    row.insert("agent", agent.into());
    if let Some(m) = model {
        row.insert("model", m.into());
    }
    Ok(row)
}

/// `[skiff_agent.tasks]`, made on first use.
fn tasks(t: &mut toml_edit::Table) -> Result<&mut toml_edit::Table> {
    t.entry("tasks")
        .or_insert_with(|| {
            let mut c = toml_edit::Table::new();
            c.set_implicit(false);
            toml_edit::Item::Table(c)
        })
        .as_table_mut()
        .ok_or_else(|| anyhow::anyhow!("`skiff_agent.tasks` is not a table"))
}

fn edit_agents(f: impl FnOnce(&mut toml_edit::Table) -> Result<()>) -> Result<()> {
    edit_table("agents", f)
}
