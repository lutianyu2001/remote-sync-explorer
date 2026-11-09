//! The settings, from the highest layer that has each (layeredcfg): the defaults in
//! `defaults.toml`, the user's `~/.config/rse/config.toml`, the environment's `RSE_*`
//! variables, and `--set KEY=VALUE` on the command line. Remotes stay in rclone.conf.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use layeredcfg::{Environment, Options, Source};
use serde::{Deserialize, Serialize};

/// What the settings' names in the environment begin with: `RSE_UI_MOUSE` is `ui.mouse`.
pub const ENV_PREFIX: &str = "RSE_";

/// The defaults, which say which settings there are and the type of each; `[keys]` is
/// added from the key map.
const DEFAULTS: &str = include_str!("defaults.toml");

/// The defaults with every action's default keys as `[keys]`.
fn defaults() -> String {
    let mut text = DEFAULTS.to_owned();
    text.push_str("\n[keys]\n");
    for (name, keys) in crate::keymap::default_keys() {
        let keys: Vec<String> =
            keys.iter().map(|k| toml::Value::String((*k).to_owned()).to_string()).collect();
        let _ = writeln!(text, "{name} = [{}]", keys.join(", "));
    }
    text
}

/// A setting's value as TOML, for serde to read into `Config`.
fn to_toml(value: layeredcfg::Value) -> toml::Value {
    match value {
        layeredcfg::Value::Bool(b) => toml::Value::Boolean(b),
        layeredcfg::Value::Int(i) => toml::Value::Integer(i),
        layeredcfg::Value::Float(d) => toml::Value::Float(d),
        layeredcfg::Value::Str(s) => toml::Value::String(s),
        layeredcfg::Value::List(items) => {
            toml::Value::Array(items.into_iter().map(toml::Value::String).collect())
        }
    }
}

/// `section.name = value` pairs as nested tables.
fn tables(values: &[(String, toml::Value)]) -> toml::Value {
    let mut root = toml::Table::new();
    for (key, value) in values {
        let (section, name) = key.split_once('.').unwrap_or(("", key));
        let table = root.entry(section).or_insert_with(|| toml::Value::Table(toml::Table::new()));
        if let toml::Value::Table(table) = table {
            table.insert(name.to_owned(), value.clone());
        }
    }
    toml::Value::Table(root)
}

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
    /// What carries out copies and moves.
    pub engine: EngineChoice,
    /// Tell files apart by checksum instead of size and modification time.
    pub checksum: bool,
}

impl Default for JobsConfig {
    fn default() -> Self {
        Self { max_concurrent: 2, poll_ms: 500, engine: EngineChoice::Auto, checksum: false }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum EngineChoice {
    /// rsync between the local disk and a server reached through the system ssh that has
    /// it, rclone for everything else.
    Auto,
    /// rclone always.
    Rclone,
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

    /// The settings from every layer: the defaults, `file` (none when it is not there), the
    /// process's environment, and `overrides`, each `KEY=VALUE`.
    ///
    /// # Errors
    /// Fails when the file cannot be read, a setting is unknown, or a value is no value of
    /// its setting, saying where it was set.
    pub fn load(file: Option<&Path>, overrides: &[String]) -> Result<Self> {
        Self::layered(file, overrides, Environment::process())
    }

    /// As [`Config::load`], with the environment given.
    ///
    /// # Errors
    /// As [`Config::load`].
    pub fn layered(
        file: Option<&Path>,
        overrides: &[String],
        environment: Environment,
    ) -> Result<Self> {
        let options = Options {
            prefix: ENV_PREFIX.to_owned(),
            user_files: file.map(Path::to_path_buf).into_iter().collect(),
            // No env-style files: the user's settings are in the TOML file.
            disabled: [Source::File].into(),
            environment,
            ..Options::default()
        };
        let mut layers = layeredcfg::Config::parse(&defaults(), options, "defaults")?;
        for given in overrides {
            let (key, value) = given.split_once('=').ok_or_else(|| {
                anyhow!("{}", tr!("config-set-needs-equals", arg = given.as_str()))
            })?;
            layers.set(key.trim(), value.trim()).with_context(|| tr!("config-on-command-line"))?;
        }
        Self::from_layers(&layers, file.unwrap_or(Path::new("config.toml")))
    }

    /// The settings of TOML text written as the user's file is, over the defaults, with no
    /// other layer (tests, fuzzing).
    ///
    /// # Errors
    /// As [`Config::load`].
    pub fn parse(text: &str) -> Result<Self> {
        let options =
            Options { disabled: [Source::File, Source::Env].into(), ..Options::default() };
        let mut layers = layeredcfg::Config::parse(&defaults(), options, "defaults")?;
        let file = Path::new("config.toml");
        layers.set_user_text(text, file)?;
        Self::from_layers(&layers, file)
    }

    /// The settings as `Config`, each value checked; a mistake says which layer has it.
    fn from_layers(layers: &layeredcfg::Config, file: &Path) -> Result<Self> {
        let origin = |key: &str| match layers.source(key) {
            Ok(Source::Env) => tr!("config-in-variable", variable = layers.variable(key)),
            Ok(Source::Override) => tr!("config-on-command-line"),
            _ => tr!("config-in", path = file.display().to_string()),
        };
        let mut values = Vec::new();
        for key in layers.keys() {
            let value = layers.get(key).with_context(|| origin(key))?;
            values.push((key.clone(), to_toml(value)));
        }
        let config = match tables(&values).try_into::<Self>() {
            Ok(config) => config,
            Err(error) => {
                // The setting at fault: the one that alone among the defaults does not read.
                let defaults: Vec<_> = layers
                    .keys()
                    .iter()
                    .filter_map(|k| {
                        Some((k.clone(), to_toml(layers.default_value(k).ok()?.clone())))
                    })
                    .collect();
                let culprit = values.iter().find(|(key, value)| {
                    let mut one = defaults.clone();
                    one.iter_mut().filter(|(k, _)| k == key).for_each(|(_, v)| v.clone_from(value));
                    tables(&one).try_into::<Self>().is_err()
                });
                let error = anyhow!(error);
                return Err(match culprit {
                    Some((key, _)) => error.context(origin(key)),
                    None => error,
                });
            }
        };
        let at_least = |key: &str, value: u64, least: u64| {
            anyhow::ensure!(value >= least, "{key} must be at least {least}");
            Ok(())
        };
        let max_concurrent = u64::try_from(config.jobs.max_concurrent).unwrap_or(u64::MAX);
        at_least("jobs.max_concurrent", max_concurrent, 1)
            .with_context(|| origin("jobs.max_concurrent"))?;
        at_least("jobs.poll_ms", config.jobs.poll_ms, 50)
            .with_context(|| origin("jobs.poll_ms"))?;
        Ok(config)
    }
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

    /// The settings with `file` and an environment of the test's own.
    fn layered(file: Option<&Path>, env: &[(&str, &str)], overrides: &[&str]) -> Result<Config> {
        let env: BTreeMap<String, String> =
            env.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect();
        let environment =
            Environment::new(move |name| env.get(name.to_str()?).map(|v| v.clone().into()));
        let overrides: Vec<String> = overrides.iter().map(|o| (*o).to_owned()).collect();
        Config::layered(file, &overrides, environment)
    }

    fn message(error: &anyhow::Error) -> String {
        error.chain().map(ToString::to_string).collect::<Vec<_>>().join(": ")
    }

    #[test]
    fn the_defaults_file_has_the_defaults_and_every_actions_keys() {
        let dir = tempfile::tempdir().unwrap();
        let c = layered(Some(&dir.path().join("none.toml")), &[], &[]).unwrap();
        assert_eq!(Config { keys: BTreeMap::new(), ..c.clone() }, Config::default());
        assert_eq!(c.jobs.max_concurrent, 2);
        assert_eq!(c.ui.left, "~");
        let keys: BTreeMap<_, _> = crate::keymap::default_keys()
            .map(|(name, keys)| (name.to_owned(), keys.iter().map(|k| (*k).to_owned()).collect()))
            .collect();
        assert_eq!(c.keys, keys);
        assert!(crate::keymap::Keymap::new(&c.keys).is_ok());
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
engine = "rclone"
checksum = true

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
        assert_eq!((c.jobs.engine, c.jobs.checksum), (EngineChoice::Rclone, true));
        assert_eq!(c.keys["move"], ["F6", "m"]);
        assert_eq!(c.keys["quit"], ["q"]); // not written: the default
    }

    #[test]
    fn typos_and_bad_values_are_errors_that_say_where_they_are() {
        let error = Config::parse("[ui]\nshow_hiden = true\n").unwrap_err();
        assert_eq!(message(&error), "config.toml:2:1: unknown setting: ui.show_hiden");
        let error = Config::parse("[ui]\ndrop_action = \"move\"\n").unwrap_err();
        assert!(message(&error).starts_with("in config.toml: unknown variant `move`"), "{error:#}");
        let error = Config::parse("[jobs]\nmax_concurrent = 0\n").unwrap_err();
        assert_eq!(message(&error), "in config.toml: jobs.max_concurrent must be at least 1");
        assert!(Config::parse("[keys]\nnope = [\"x\"]\n").is_err());
        assert!(Config::parse("[rclone]\ntransfers = -1\n").is_err());
        assert!(
            Config::parse("[keys]\ncopy = [\"NoSuchKey\"]\n")
                .is_ok_and(|c| { crate::keymap::Keymap::new(&c.keys).is_err() })
        );
    }

    #[test]
    fn the_environment_is_above_the_file_and_the_command_line_above_both() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("config.toml");
        fs::write(&file, "[jobs]\nmax_concurrent = 3\n[ui]\nmouse = false\n").unwrap();
        let c = layered(Some(&file), &[], &[]).unwrap();
        assert_eq!((c.jobs.max_concurrent, c.ui.mouse), (3, false));
        let env = [("RSE_JOBS_MAX_CONCURRENT", "4"), ("RSE_KEYS_COPY", "F5, y")];
        let c = layered(Some(&file), &env, &[]).unwrap();
        assert_eq!((c.jobs.max_concurrent, c.ui.mouse), (4, false));
        assert_eq!(c.keys["copy"], ["F5", "y"]);
        let c =
            layered(Some(&file), &env, &["jobs.max_concurrent=5", "ui.language = zh-CN"]).unwrap();
        assert_eq!((c.jobs.max_concurrent, c.ui.language), (5, Language::ZhCn));
        // Env-style files are no layer here, and a missing file is none.
        let c = layered(Some(&dir.path().join("none.toml")), &[], &[]).unwrap();
        assert_eq!(c.jobs.max_concurrent, 2);
    }

    #[test]
    fn a_mistake_in_the_environment_or_on_the_command_line_says_so() {
        let error = layered(None, &[("RSE_UI_MOUSE", "maybe")], &[]).unwrap_err();
        let text = message(&error);
        assert!(text.starts_with("in the environment variable RSE_UI_MOUSE: "), "{text}");
        let error = layered(None, &[("RSE_UI_DROP_ACTION", "move")], &[]).unwrap_err();
        assert!(message(&error).starts_with("in the environment variable RSE_UI_DROP_ACTION: "));
        let error = layered(None, &[], &["jobs.poll_ms=10"]).unwrap_err();
        assert_eq!(
            message(&error),
            "on the command line (--set): jobs.poll_ms must be at least 50"
        );
        let error = layered(None, &[], &["ui.mouse"]).unwrap_err();
        assert_eq!(message(&error), "--set takes KEY=VALUE, not ui.mouse");
        let error = layered(None, &[], &["ui.nope=1"]).unwrap_err();
        assert_eq!(message(&error), "on the command line (--set): unknown setting: ui.nope");
    }
}
