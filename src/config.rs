//! `~/.config/rse/config.toml`. Remotes stay in rclone.conf.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub rclone: RcloneConfig,
    pub ui: UiConfig,
    pub jobs: JobsConfig,
    /// Action name to key list, e.g. `copy = ["F5", "c"]`. Missing actions keep defaults.
    pub keys: BTreeMap<String, Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct RcloneConfig {
    /// rclone.conf path; empty means rclone's default location.
    pub config: String,
    pub transfers: u32,
    pub checkers: u32,
    /// Threads for listings and other short calls.
    pub browse_threads: usize,
    /// Reach sftp servers through the system ssh, sharing one login per server.
    pub system_ssh: bool,
}

impl Default for RcloneConfig {
    fn default() -> Self {
        Self {
            config: String::new(),
            transfers: 4,
            checkers: 8,
            browse_threads: 2,
            system_ssh: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum MultiSelectModifier {
    Ctrl,
    Alt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DropAction {
    /// Show the copy / move / cancel menu.
    Ask,
    /// Copy without asking (conflicts are still confirmed).
    Copy,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct UiConfig {
    pub left: String,
    /// Empty means the root of the first remote (or the home directory without remotes).
    pub right: String,
    pub show_hidden: bool,
    pub mouse: bool,
    pub multi_select_modifier: MultiSelectModifier,
    pub drop_action: DropAction,
    pub language: Language,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            left: "~".to_owned(),
            right: String::new(),
            show_hidden: false,
            mouse: true,
            multi_select_modifier: MultiSelectModifier::Ctrl,
            drop_action: DropAction::Ask,
            language: Language::Auto,
        }
    }
}

/// The interface language.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
pub enum Language {
    /// The system's (`LANG` and the like): Chinese for `zh*`, else English.
    #[serde(rename = "auto")]
    Auto,
    #[serde(rename = "en")]
    En,
    #[serde(rename = "zh-CN")]
    ZhCn,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct JobsConfig {
    pub max_concurrent: usize,
    pub poll_ms: u64,
}

impl Default for JobsConfig {
    fn default() -> Self {
        Self { max_concurrent: 2, poll_ms: 500 }
    }
}

/// The user's home directory.
pub fn home_dir() -> Option<PathBuf> {
    std::env::home_dir().filter(|p| p.is_absolute())
}

/// `$XDG_CACHE_HOME`, else `~/Library/Caches` on macOS and `~/.cache` elsewhere.
pub fn cache_dir() -> Option<PathBuf> {
    if let Some(p) =
        std::env::var_os("XDG_CACHE_HOME").map(PathBuf::from).filter(|p| p.is_absolute())
    {
        return Some(p);
    }
    let home = home_dir()?;
    Some(if cfg!(target_os = "macos") { home.join("Library/Caches") } else { home.join(".cache") })
}

/// `$XDG_CONFIG_HOME`, else `~/.config`.
fn config_base() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| home_dir().map(|h| h.join(".config")))
}

/// Moves what rse kept under its former name, rext, to its own: the config directory, the
/// cache directory and the remembered ssh passwords, each only where rse has none yet.
/// Returns what was moved, as (from, to).
pub fn migrate_from_rext() -> Vec<(PathBuf, PathBuf)> {
    let ssh = home_dir().map(|h| h.join(".ssh"));
    let pairs = [
        config_base().map(|b| (b.join("rext"), b.join("rse"))),
        cache_dir().map(|b| (b.join("rext"), b.join("rse"))),
        ssh.map(|d| (d.join("rext-passwords"), d.join("rse-passwords"))),
    ];
    pairs.into_iter().flatten().filter(|(from, to)| migrate(from, to)).collect()
}

/// Renames `from` to `to` when `from` exists and `to` does not; whether it did. Neither
/// may be a link, so a link planted at either cannot send the move elsewhere.
fn migrate(from: &Path, to: &Path) -> bool {
    let real = |p: &Path| fs::symlink_metadata(p).map(|m| !m.file_type().is_symlink());
    real(from).unwrap_or(false)
        && fs::symlink_metadata(to).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
        && fs::rename(from, to).is_ok()
}

impl Config {
    /// `$XDG_CONFIG_HOME/rse/config.toml`, else under `~/.config`.
    pub fn default_path() -> Option<PathBuf> {
        Some(config_base()?.join("rse").join("config.toml"))
    }

    /// Reads the file; a missing file gives the defaults.
    ///
    /// # Errors
    /// Fails when the file exists but cannot be read or parsed.
    pub fn load(path: &Path) -> Result<Self> {
        match fs::read_to_string(path) {
            Ok(text) => Self::parse(&text)
                .with_context(|| tr!("config-in", path = path.display().to_string())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => {
                Err(e).with_context(|| tr!("config-cannot-read", path = path.display().to_string()))
            }
        }
    }

    /// # Errors
    /// Fails on invalid TOML or unknown settings.
    pub fn parse(text: &str) -> Result<Self> {
        let config: Self = toml::from_str(text)?;
        anyhow::ensure!(config.jobs.max_concurrent >= 1, "jobs.max_concurrent must be at least 1");
        anyhow::ensure!(config.jobs.poll_ms >= 50, "jobs.poll_ms must be at least 50");
        Ok(config)
    }

    /// Writes the file atomically: a temporary file in the same directory, then a rename.
    ///
    /// # Errors
    /// Fails when the directory or file cannot be written.
    pub fn save(&self, path: &Path) -> Result<()> {
        let text = toml::to_string_pretty(self)?;
        write_atomic(path, text.as_bytes())
    }
}

/// Replaces `path` with `data` so that readers see either the old or the new content.
///
/// # Errors
/// Fails when the directory or file cannot be written.
pub fn write_atomic(path: &Path, data: &[u8]) -> Result<()> {
    let dir = path.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
    fs::create_dir_all(dir)
        .with_context(|| tr!("config-cannot-create", path = dir.display().to_string()))?;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = dir.join(format!(".{name}.tmp-{}", std::process::id()));
    let result = (|| -> Result<()> {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
        fs::rename(&tmp, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result.with_context(|| tr!("config-cannot-write", path = path.display().to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrate_moves_only_to_a_free_name() {
        let dir = tempfile::tempdir().unwrap();
        let (old, new) = (dir.path().join("rext"), dir.path().join("rse"));
        fs::create_dir(&old).unwrap();
        fs::write(old.join("config.toml"), "x").unwrap();
        assert!(migrate(&old, &new));
        assert_eq!(fs::read_to_string(new.join("config.toml")).unwrap(), "x");
        // Gone, so nothing to move; then rse's own is never replaced.
        assert!(!migrate(&old, &new));
        fs::create_dir(&old).unwrap();
        assert!(!migrate(&old, &new));
        assert!(old.exists());
    }

    #[test]
    fn migrate_leaves_links_alone() {
        let dir = tempfile::tempdir().unwrap();
        let (old, new) = (dir.path().join("rext"), dir.path().join("rse"));
        std::os::unix::fs::symlink(dir.path(), &old).unwrap();
        assert!(!migrate(&old, &new));
        std::os::unix::fs::symlink(dir.path(), &new).unwrap();
        fs::remove_file(&old).unwrap();
        fs::create_dir(&old).unwrap();
        assert!(!migrate(&old, &new));
    }

    #[test]
    fn missing_file_gives_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let c = Config::load(&dir.path().join("none.toml")).unwrap();
        assert_eq!(c, Config::default());
        assert_eq!(c.jobs.max_concurrent, 2);
        assert_eq!(c.ui.left, "~");
    }

    #[test]
    fn a_full_config_parses() {
        let c = Config::parse(
            r#"
[rclone]
config = ""
transfers = 4
checkers = 8

[ui]
left = "~"
right = ""
show_hidden = false
mouse = true
multi_select_modifier = "alt"
drop_action = "copy"
language = "zh-CN"

[jobs]
max_concurrent = 3
poll_ms = 500

[keys]
copy = ["F5", "c"]
move = ["F6", "m"]
"#,
        )
        .unwrap();
        assert_eq!(c.ui.multi_select_modifier, MultiSelectModifier::Alt);
        assert_eq!(c.ui.drop_action, DropAction::Copy);
        assert_eq!(c.ui.language, Language::ZhCn);
        assert_eq!(c.jobs.max_concurrent, 3);
        assert_eq!(c.keys["move"], ["F6", "m"]);
    }

    #[test]
    fn typos_and_bad_values_are_errors() {
        assert!(Config::parse("[ui]\nshow_hiden = true\n").is_err());
        assert!(Config::parse("[ui]\ndrop_action = \"move\"\n").is_err());
        assert!(Config::parse("[jobs]\nmax_concurrent = 0\n").is_err());
    }

    #[test]
    fn save_is_atomic_and_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub/config.toml");
        let mut c = Config::default();
        c.ui.show_hidden = true;
        c.keys.insert("copy".into(), vec!["F5".into()]);
        c.save(&path).unwrap();
        assert_eq!(Config::load(&path).unwrap(), c);
        let leftovers: Vec<_> = fs::read_dir(path.parent().unwrap()).unwrap().collect();
        assert_eq!(leftovers.len(), 1, "temporary file left behind");
    }
}
