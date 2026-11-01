//! `client.toml`: what the person at this computer sets, apart from the
//! Host's `projects.toml` (V2 epic §5). Keys, appearance and Open with apply
//! to every Host this app shows; projects, fleets and worktrees belong to the
//! Host, and only its `skiff-host` writes them.
//!
//! Migration (`schema = 2`) copies, never moves: the client tables of
//! `projects.toml` are copied into a new `client.toml` on the first client
//! setting written, and `projects.toml` is never written or deleted. v1 reads
//! `projects.toml` as before and keeps the values it had at the copy.
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

use super::persistence::{config_path, edit_lock_for, edit_table_in};
use super::types::{Appearance, Keys, Open};

/// The version `client.toml` is written in.
pub const SCHEMA: i64 = 2;

/// The `projects.toml` tables the client owns, in the order they are copied.
pub const CLIENT_TABLES: [&str; 3] = ["keys", "appearance", "open"];

/// The first lines of a new `client.toml`.
const HEADER: &str = "# Skiff's settings for this computer: keys, appearance and Open with.\n\
# Copied from projects.toml, which keeps its own copy for Skiff 1.\n";

#[derive(Deserialize, Debug, Clone, Default, PartialEq)]
pub struct ClientConfig {
    #[serde(default)]
    pub schema: i64,
    #[serde(default)]
    pub keys: Keys,
    #[serde(default)]
    pub appearance: Appearance,
    #[serde(default)]
    pub open: Open,
}

/// `client.toml` beside the `projects.toml` this process uses: a private
/// state's beside its private copy, never the person's.
pub fn path() -> PathBuf {
    config_path().with_file_name("client.toml")
}

/// What a migration did, or would do.
#[derive(Debug, Clone, PartialEq)]
pub enum Migration {
    /// `client.toml` is at schema 2: nothing to do.
    Current,
    /// `client.toml` is written from these tables of `projects.toml`
    /// (possibly none), with this text.
    Copy { tables: Vec<String>, text: String },
}

/// What [`migrate`] would do with these files. Writes nothing.
pub fn plan(projects: &Path, client: &Path) -> Result<Migration> {
    if client.exists() {
        read_client(client)?;
        return Ok(Migration::Current);
    }
    let text = match std::fs::read_to_string(projects) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e).with_context(|| format!("read {}", projects.display())),
    };
    // A file Skiff cannot read is not copied from.
    super::parse(&text).with_context(|| format!("parse {}", projects.display()))?;
    let source: toml_edit::DocumentMut = text.parse().with_context(|| format!("parse {}", projects.display()))?;
    let mut doc = toml_edit::DocumentMut::new();
    doc["schema"] = toml_edit::value(SCHEMA);
    let mut tables = Vec::new();
    for name in CLIENT_TABLES {
        if let Some(item) = source.get(name) {
            let mut item = item.clone();
            if let Some(t) = item.as_table_mut() {
                t.set_position(None);
            }
            doc[name] = item;
            tables.push(name.to_string());
        }
    }
    Ok(Migration::Copy { tables, text: format!("{HEADER}{doc}") })
}

/// Makes `client.toml` from `projects.toml` once. Again it changes nothing.
/// `projects.toml` is only read. An existing `client.toml` is never
/// replaced: one of another schema is refused as it is.
pub fn migrate(projects: &Path, client: &Path) -> Result<Migration> {
    let _edit = edit_lock_for(client)?;
    let step = plan(projects, client)?;
    if let Migration::Copy { text, .. } = &step {
        if let Some(dir) = client.parent() {
            std::fs::create_dir_all(dir)?;
        }
        // A link, not a rename: it fails rather than replace a file that
        // another writer put there meanwhile.
        let name = client.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let tmp = client.with_file_name(format!(".{name}.{}.tmp", std::process::id()));
        let linked = std::fs::write(&tmp, text)
            .with_context(|| format!("write {}", tmp.display()))
            .and_then(|_| std::fs::hard_link(&tmp, client).with_context(|| format!("create {}", client.display())));
        // Gone on every path: a half-written temp file never stays.
        let _ = std::fs::remove_file(&tmp);
        linked?;
    }
    Ok(step)
}

fn read_client(client: &Path) -> Result<ClientConfig> {
    let text = std::fs::read_to_string(client).with_context(|| format!("read {}", client.display()))?;
    let cfg: ClientConfig = toml::from_str(&text).with_context(|| format!("parse {}", client.display()))?;
    match cfg.schema {
        SCHEMA => Ok(cfg),
        s if s > SCHEMA => anyhow::bail!("{} is from a newer Skiff (schema {s}); Skiff leaves it as it is", client.display()),
        s => anyhow::bail!("{} has schema {s}, not {SCHEMA}; Skiff leaves it as it is", client.display()),
    }
}

/// This computer's client settings: `client.toml`, or before the first
/// client setting is written, the same tables of `projects.toml`.
pub fn load() -> Result<ClientConfig> {
    load_from(&config_path(), &path())
}

pub fn load_from(projects: &Path, client: &Path) -> Result<ClientConfig> {
    if client.exists() {
        return read_client(client);
    }
    let cfg = super::load_from(projects)?;
    Ok(ClientConfig { schema: 0, keys: cfg.keys, appearance: cfg.appearance, open: cfg.open })
}

/// Edits one table of `client.toml`, made first from `projects.toml`.
pub(super) fn edit_table(name: &str, f: impl FnOnce(&mut toml_edit::Table) -> Result<()>) -> Result<()> {
    let client = path();
    let _edit = edit_lock_for(&client)?;
    migrate(&config_path(), &client)?;
    edit_table_in(&client, name, f)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Shapes of `projects.toml` from Skiff 1.4 to 1.6: each release added
    /// keys, and every one of them is read the same after a migration.
    const V1: [(&str, &str); 3] = [
        ("v1.4", include_str!("../../tests/fixtures/v1/projects-1.4.toml")),
        ("v1.5", include_str!("../../tests/fixtures/v1/projects-1.5.toml")),
        ("v1.6", include_str!("../../tests/fixtures/v1/projects-1.6.toml")),
    ];

    struct Dir(PathBuf);
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn dir(tag: &str) -> Dir {
        let d = std::env::temp_dir().join(format!("skiff-migrate-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        Dir(d)
    }

    #[test]
    fn v1_files_copy_their_client_tables_and_stay_as_they_were() {
        for (version, text) in V1 {
            let d = dir(version);
            let (projects, client) = (d.0.join("projects.toml"), d.0.join("client.toml"));
            std::fs::write(&projects, text).unwrap();
            let before = super::super::parse(text).unwrap();

            // Dry run: what it would write, and nothing written.
            let planned = plan(&projects, &client).unwrap();
            let Migration::Copy { tables, text: copy } = &planned else { panic!("{version}: {planned:?}") };
            assert!(!client.exists(), "{version}: a plan writes nothing");
            assert_eq!(tables, &["keys", "appearance", "open"].map(String::from), "{version}");

            assert_eq!(migrate(&projects, &client).unwrap(), planned, "{version}");
            assert_eq!(&std::fs::read_to_string(&client).unwrap(), copy, "{version}");
            // The original: the same bytes, so v1 reads what it read before.
            assert_eq!(std::fs::read_to_string(&projects).unwrap(), text, "{version}");
            assert_eq!(super::super::load_from(&projects).unwrap(), before, "{version}");
            // The copy: schema 2 and the same client values.
            let got = load_from(&projects, &client).unwrap();
            assert_eq!(got.schema, SCHEMA);
            assert_eq!((got.keys, got.appearance, got.open), (before.keys.clone(), before.appearance.clone(), before.open.clone()), "{version}");
            // No Host key in the client file.
            let copied: toml::Table = toml::from_str(copy).unwrap();
            assert!(copied.keys().all(|k| k == "schema" || CLIENT_TABLES.contains(&k.as_str())), "{version}: {copied:?}");

            // Again: nothing to do, nothing changed.
            assert_eq!(plan(&projects, &client).unwrap(), Migration::Current);
            assert_eq!(migrate(&projects, &client).unwrap(), Migration::Current);
            assert_eq!(&std::fs::read_to_string(&client).unwrap(), copy);
            assert_eq!(std::fs::read_to_string(&projects).unwrap(), text);
        }
    }

    #[test]
    fn a_client_file_is_never_replaced_and_a_bad_source_is_not_copied() {
        let d = dir("refuse");
        let (projects, client) = (d.0.join("projects.toml"), d.0.join("client.toml"));
        std::fs::write(&projects, V1[2].1).unwrap();

        // A client file edited after the copy keeps its edits.
        std::fs::write(&client, "schema = 2\n[appearance]\nfont_size = 17\n").unwrap();
        assert_eq!(migrate(&projects, &client).unwrap(), Migration::Current);
        assert_eq!(load_from(&projects, &client).unwrap().appearance.font_size, Some(17));

        // A newer or unknown schema is refused and left as it is.
        for text in ["schema = 3\n", "[keys]\npalette = \"ctrl+p\"\n"] {
            std::fs::write(&client, text).unwrap();
            assert!(migrate(&projects, &client).is_err(), "{text}");
            assert!(load_from(&projects, &client).is_err(), "{text}");
            assert_eq!(std::fs::read_to_string(&client).unwrap(), text);
        }

        // An unreadable projects.toml: nothing is written.
        std::fs::remove_file(&client).unwrap();
        std::fs::write(&projects, "[[project]\nname = ").unwrap();
        assert!(plan(&projects, &client).is_err());
        assert!(migrate(&projects, &client).is_err());
        assert!(!client.exists());

        // No projects.toml: a client file with the schema only.
        std::fs::remove_file(&projects).unwrap();
        let Migration::Copy { tables, .. } = migrate(&projects, &client).unwrap() else { panic!() };
        assert!(tables.is_empty());
        assert_eq!(load_from(&projects, &client).unwrap(), ClientConfig { schema: SCHEMA, ..Default::default() });
        assert!(!projects.exists(), "the migration never makes a projects.toml");
    }

    #[test]
    fn before_the_copy_the_client_reads_projects_toml() {
        let d = dir("compat");
        let (projects, client) = (d.0.join("projects.toml"), d.0.join("client.toml"));
        std::fs::write(&projects, V1[1].1).unwrap();
        let got = load_from(&projects, &client).unwrap();
        assert_eq!(got.schema, 0);
        assert_eq!(got.appearance, super::super::parse(V1[1].1).unwrap().appearance);
        assert!(!client.exists(), "reading writes nothing");
    }
}
