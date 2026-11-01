//! Open with: which command opens a diff or a file, and where. Edits `[open]`
//! in client.toml, as the Settings dialog does, and reads it from there.

use anyhow::{Result, bail};
use skiff_core::config::{self, OPEN_KEYS};

use crate::{OpenWithCmd, print};

#[derive(serde::Serialize)]
struct Row {
    key: &'static str,
    /// The command set, or `None` for the default.
    command: Option<String>,
    /// What selecting the file does: `peek` runs the command, `app` shows
    /// an Open button. Settings › Open with › OPENS IN. A diff is always `peek`.
    opens: &'static str,
}

fn rows() -> Result<Vec<Row>> {
    // client.toml, or projects.toml until the first client setting copies it.
    let o = config::client::load()?.open;
    Ok(OPEN_KEYS
        .into_iter()
        .map(|key| {
            let cmd = match key {
                "diff" => &o.diff,
                "text" => &o.text,
                "markdown" => &o.markdown,
                "image" => &o.image,
                _ => &o.html,
            };
            let command = cmd.as_deref().map(str::trim).filter(|c| !c.is_empty()).map(String::from);
            let opens = if key == "image" { "app" } else if key == "diff" || o.peek.iter().any(|k| k == key) { "peek" } else { "app" };
            Row { key, command, opens }
        })
        .collect())
}

/// Lists each row with its command and where it opens.
pub fn ls(json: bool) -> Result<()> {
    let rows = rows()?;
    if json {
        return print::json(&rows);
    }
    let table: Vec<Vec<String>> = rows
        .iter()
        .map(|r| {
            let command = match (&r.command, r.key) {
                (Some(c), _) => c.clone(),
                (None, "diff") => format!("{} (default)", skiff_core::git::DEFAULT_DIFF),
                (None, _) => "(system default app)".to_string(),
            };
            vec![r.key.to_string(), command, r.opens.to_string()]
        })
        .collect();
    print::table(&["KEY", "COMMAND", "OPENS IN"], &table);
    Ok(())
}

pub fn run(cmd: OpenWithCmd) -> Result<()> {
    match cmd {
        OpenWithCmd::Ls { json } => ls(json),
        OpenWithCmd::Set { key, command, peek, app } => {
            let key = key.name();
            if (peek || app) && key == "diff" {
                bail!("diff always opens in the peek");
            }
            if let Some(command) = command {
                let c = command.trim();
                if c.is_empty() {
                    bail!("the command is empty; use `skiff open-with clear {key}`");
                }
                config::set_open(key, Some(c))?;
            }
            if peek || app {
                config::set_open_peek(key, peek)?;
            }
            Ok(())
        }
        OpenWithCmd::Clear { key } => config::set_open(key.name(), None),
    }
}
