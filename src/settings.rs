//! The settings page: what it lists (every setting with the value the user's file gives
//! it, or its default), and writing one setting into that file, leaving the rest of the
//! file, comments included, as the user wrote it.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Write as _};
use std::ops::Range;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use layeredcfg::{Environment, Type, Value};
use toml::de::DeTable;

use crate::config::{Config, to_toml};
use crate::keymap::Keymap;

/// The settings with a fixed set of values, in the order the page goes through them.
const CHOICES: &[(&str, &[&str])] = &[
    ("ui.multi_select_modifier", &["ctrl", "alt"]),
    ("ui.drop_action", &["ask", "copy"]),
    ("ui.language", &["auto", "en", "zh-CN"]),
    ("jobs.engine", &["auto", "rclone"]),
];

/// The settings a change of which holds at once; `[keys]` too. The others are read when
/// rse starts.
const LIVE: &[&str] = &[
    "ui.show_hidden",
    "ui.mouse",
    "ui.multi_select_modifier",
    "ui.drop_action",
    "ui.language",
    "jobs.max_concurrent",
    "jobs.retries",
    "jobs.retry_delay",
    "jobs.retry_backoff",
    "jobs.chunked_retry",
    "jobs.chunk_size",
];

/// How a setting is changed on the page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Bool,
    /// One of these.
    Choice(&'static [&'static str]),
    Number,
    Text,
    /// Key names, written with spaces between them.
    Keys,
}

/// A layer above the user's file, whose value holds for this run whatever the file says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Above {
    /// The environment variable of this name.
    Variable(String),
    /// `--set`.
    CommandLine,
}

/// One setting on the page.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    /// `ui.mouse`.
    pub key: String,
    pub kind: Kind,
    /// What the user's file sets it to, else its default.
    pub value: Value,
    pub default: Value,
    /// Whether the user's file sets it.
    pub in_file: bool,
    /// The layer above the file that sets it for this run, with its value.
    pub above: Option<(Above, Value)>,
    /// Whether a change holds at once, not only from the next start.
    pub live: bool,
}

impl Row {
    /// `ui` of `ui.mouse`.
    pub fn section(&self) -> &str {
        self.key.split_once('.').map_or("", |(section, _)| section)
    }

    /// `mouse` of `ui.mouse`.
    pub fn name(&self) -> &str {
        self.key.split_once('.').map_or(self.key.as_str(), |(_, name)| name)
    }

    /// The Fluent message that says what the setting is: an action's help text for its
    /// keys.
    pub fn description_id(&self) -> String {
        match self.kind {
            Kind::Keys => format!("key-{}", self.name()),
            _ => format!("setting-{}-{}", self.section(), self.name()),
        }
    }

    /// The Fluent message that names the setting; an action's help text names its keys.
    pub fn name_id(&self) -> String {
        match self.kind {
            Kind::Keys => format!("key-{}", self.name()),
            _ => format!("setting-{}-{}-name", self.section(), self.name()),
        }
    }

    /// The Fluent message that names `value`, for a setting that is on or off or one of
    /// a few.
    pub fn value_id(&self, value: &Value) -> Option<String> {
        match (self.kind, value) {
            (Kind::Bool, Value::Bool(on)) => {
                Some(if *on { "settings-on" } else { "settings-off" }.to_owned())
            }
            (Kind::Choice(choices), value) => {
                let value = text(value);
                choices
                    .contains(&value.as_str())
                    .then(|| format!("choice-{}-{}-{value}", self.section(), self.name()))
            }
            _ => None,
        }
    }

    /// The value as the page edits it.
    pub fn text(&self) -> String {
        text(&self.value)
    }

    /// The value typed as `text` on the page.
    ///
    /// # Errors
    /// Fails for a number that is none.
    pub fn parse(&self, text: &str) -> Result<Value> {
        Ok(match self.kind {
            Kind::Number => Value::Int(
                text.trim()
                    .parse()
                    .map_err(|_| anyhow!("{}", tr!("settings-not-a-number", text = text)))?,
            ),
            Kind::Keys => Value::List(text.split_whitespace().map(str::to_owned).collect()),
            Kind::Bool => Value::Bool(text.trim() == "true"),
            Kind::Choice(_) | Kind::Text => Value::Str(text.to_owned()),
        })
    }

    /// The values of a setting that is on or off or one of a few, in the order its list
    /// shows them; none for what is typed.
    pub fn values(&self) -> Vec<Value> {
        match self.kind {
            Kind::Bool => vec![Value::Bool(true), Value::Bool(false)],
            Kind::Choice(choices) => {
                choices.iter().map(|choice| Value::Str((*choice).to_owned())).collect()
            }
            _ => Vec::new(),
        }
    }

    /// The value after this one, going round: the other one of two, the next choice
    /// (the one before with `back`). None for what is typed.
    pub fn next(&self, back: bool) -> Option<Value> {
        match (&self.kind, &self.value) {
            (Kind::Bool, Value::Bool(on)) => Some(Value::Bool(!on)),
            (Kind::Choice(choices), Value::Str(now)) => {
                let n = choices.len();
                let at = choices.iter().position(|c| c == now);
                let next = match (at, back) {
                    (Some(at), false) => (at + 1) % n,
                    (Some(at), true) => (at + n - 1) % n,
                    (None, _) => 0,
                };
                Some(Value::Str(choices.get(next)?.to_string()))
            }
            _ => None,
        }
    }
}

/// A value as the page shows it: a list with spaces between its items.
pub fn text(value: &Value) -> String {
    match value {
        Value::Bool(on) => on.to_string(),
        Value::List(items) => items.join(" "),
        other => other.to_string(),
    }
}

/// The Fluent message that names a section of the file.
pub fn section_id(section: &str) -> String {
    format!("settings-section-{section}")
}

/// The Fluent messages naming and describing the settings, their sections and their
/// values, for checking they all exist.
#[cfg(test)]
pub fn message_ids() -> Vec<String> {
    let mut ids = vec!["settings-on".to_owned(), "settings-off".to_owned()];
    for row in Source::none().rows().unwrap_or_default() {
        ids.extend([row.description_id(), row.name_id(), section_id(row.section())]);
        if let Kind::Choice(choices) = row.kind {
            let named = |choice: &&str| row.value_id(&Value::Str((*choice).to_owned()));
            ids.extend(choices.iter().filter_map(named));
        }
    }
    ids
}

/// A line of the page: a section's heading, or the setting of this number.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Line<'a> {
    Section(&'a str),
    Row(usize),
    Blank,
}

/// The page's lines: each section's heading above its settings, a blank line between
/// sections.
pub fn lines(rows: &[Row]) -> Vec<Line<'_>> {
    let mut out = Vec::new();
    let mut section = None;
    for (index, row) in rows.iter().enumerate() {
        if section != Some(row.section()) {
            if section.is_some() {
                out.push(Line::Blank);
            }
            section = Some(row.section());
            out.push(Line::Section(row.section()));
        }
        out.push(Line::Row(index));
    }
    out
}

/// Where the settings of this run are from: the user's file, which the page writes, and
/// the layers above it.
#[derive(Clone)]
pub struct Source {
    /// The user's file; `None` when there is no place for one.
    pub file: Option<PathBuf>,
    /// `--set KEY=VALUE`, as given.
    pub overrides: Vec<String>,
    pub environment: Environment,
}

impl Source {
    /// No file, no environment, nothing on the command line: the defaults alone.
    pub fn none() -> Self {
        Self { file: None, overrides: Vec::new(), environment: Environment::new(|_| None) }
    }

    fn layers(&self) -> Result<layeredcfg::Config> {
        Config::layers(self.file.as_deref(), &self.overrides, self.environment.clone())
    }

    /// Every setting as the page lists it, the file read anew.
    ///
    /// # Errors
    /// Fails when the file cannot be read or is no valid settings file.
    pub fn rows(&self) -> Result<Vec<Row>> {
        let layers = self.layers()?;
        let user = layers.user_values();
        let mut rows = Vec::new();
        for key in layers.keys() {
            let default = layers.default_value(key)?.clone();
            let choices = CHOICES.iter().find(|(k, _)| k == key).map(|(_, choices)| *choices);
            let kind = match (layers.type_of(key)?, choices) {
                (Type::Bool, _) => Kind::Bool,
                (Type::Int | Type::Float, _) => Kind::Number,
                (Type::List, _) => Kind::Keys,
                (Type::String, Some(choices)) => Kind::Choice(choices),
                (Type::String, None) => Kind::Text,
            };
            let above = match layers.source(key)? {
                layeredcfg::Source::Env => Some(Above::Variable(layers.variable(key))),
                layeredcfg::Source::Override => Some(Above::CommandLine),
                _ => None,
            };
            let above = match above {
                Some(above) => Some((above, layers.get(key)?)),
                None => None,
            };
            rows.push(Row {
                kind,
                value: user.get(key).cloned().unwrap_or_else(|| default.clone()),
                default,
                in_file: user.contains_key(key),
                above,
                live: LIVE.contains(&key.as_str()) || kind == Kind::Keys,
                key: key.clone(),
            });
        }
        Ok(rows)
    }

    /// Writes `key = value` into the user's file, or with no value takes the setting out
    /// of it, which leaves the default. Returns the settings as they then are, from
    /// every layer, with their key map.
    ///
    /// # Errors
    /// Fails, with the file as it was, when there is no file to write, it cannot be read
    /// or written, the value is no value of the setting, or the file is written in a way
    /// the setting cannot be put into.
    pub fn write(&self, key: &str, value: Option<&Value>) -> Result<(Config, Keymap)> {
        let path = self.file.as_deref().ok_or_else(|| anyhow!("{}", tr!("settings-no-file")))?;
        let old = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(e).context(path.display().to_string()),
        };
        let mut layers = self.layers()?;
        let mut expected = layers.user_values();
        let new = if let Some(value) = value {
            expected.insert(key.to_owned(), value.clone());
            set(&old, key, &to_toml(value.clone()).to_string())?
        } else {
            expected.remove(key);
            remove(&old, key)?
        };
        // The file as it would be: valid on its own, and with the other layers.
        let alone = Config::parse(&new)?;
        Keymap::new(&alone.keys)?;
        layers.set_user_text(&new, path)?;
        if !same(&layers.user_values(), &expected) {
            bail!("{}", tr!("settings-by-hand", key = key, path = path.display().to_string()));
        }
        let config = Config::from_layers(&layers, path)?;
        let keymap = Keymap::new(&config.keys)?;
        save(path, &new).with_context(|| path.display().to_string())?;
        Ok((config, keymap))
    }
}

/// Whether two files' settings are the same; a number is the same whichever way a file
/// writes it.
fn same(a: &BTreeMap<String, Value>, b: &BTreeMap<String, Value>) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|((ak, av), (bk, bv))| ak == bk && text(av) == text(bv))
}

/// Where `key` (`ui.mouse`) is written in `text`: its name, and its value.
fn find(text: &str, key: &str) -> Result<Option<(Range<usize>, Range<usize>)>> {
    let (section, name) = key.split_once('.').unwrap_or(("", key));
    let root = DeTable::parse(text).map_err(|e| anyhow!("{}", e.message()))?;
    let Some(table) = root.get_ref().get(section).and_then(|t| t.get_ref().as_table()) else {
        return Ok(None);
    };
    Ok(table.get_key_value(name).map(|(name, value)| (name.span(), value.span())))
}

/// The table a header line names: `ui` for `[ui]  # comment`.
fn header(line: &str) -> Option<&str> {
    let rest = line.trim_start().strip_prefix('[')?;
    let (name, after) = rest.split_once(']')?;
    let after = after.trim_start();
    (!name.starts_with('[') && (after.is_empty() || after.starts_with('#'))).then(|| name.trim())
}

/// `text` with `key` set to `value`, written as TOML: in place where the file has the
/// setting, else as a new line at the end of its section, else in a new section at the
/// end of the file.
fn set(text: &str, key: &str, value: &str) -> Result<String> {
    if let Some((_, at)) = find(text, key)? {
        return Ok(format!("{}{value}{}", &text[..at.start], &text[at.end..]));
    }
    let (section, name) = key.split_once('.').unwrap_or(("", key));
    let line = format!("{name} = {value}\n");
    // The end of the section's last line that sets something, or of its header.
    let mut end = None;
    let mut inside = false;
    let mut offset = 0;
    for l in text.split_inclusive('\n') {
        offset += l.len();
        let content = l.trim();
        if let Some(name) = header(l) {
            inside = name == section;
            if inside {
                end = Some(offset);
            }
        } else if inside && !content.is_empty() && !content.starts_with('#') {
            end = Some(offset);
        }
    }
    if let Some(end) = end {
        let gap = if text[..end].ends_with('\n') { "" } else { "\n" };
        return Ok(format!("{}{gap}{line}{}", &text[..end], &text[end..]));
    }
    let gap = match text {
        "" => "",
        t if t.ends_with("\n\n") => "",
        t if t.ends_with('\n') => "\n",
        _ => "\n\n",
    };
    Ok(format!("{text}{gap}[{section}]\n{line}"))
}

/// `text` without the line that sets `key`; as it is where the file does not set it.
fn remove(text: &str, key: &str) -> Result<String> {
    let Some((name, value)) = find(text, key)? else { return Ok(text.to_owned()) };
    let start = text[..name.start].rfind('\n').map_or(0, |i| i + 1);
    let end = text[value.end..].find('\n').map_or(text.len(), |i| value.end + i + 1);
    let after = text[value.end..end].trim();
    // Alone on its lines, but for a comment after it.
    if !text[start..name.start].trim().is_empty() || !(after.is_empty() || after.starts_with('#')) {
        bail!("{}", tr!("settings-by-hand", key = key, path = "config.toml"));
    }
    Ok(format!("{}{}", &text[..start], &text[end..]))
}

/// Replaces the file at `path` with `text` in one step, keeping its permissions; a
/// symbolic link stays, and what it points to is written.
fn save(path: &Path, text: &str) -> io::Result<()> {
    let path = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let dir = path.parent().ok_or_else(|| io::Error::other("no directory"))?;
    fs::create_dir_all(dir)?;
    let mode = match fs::metadata(&path) {
        Ok(meta) => std::os::unix::fs::PermissionsExt::mode(&meta.permissions()) & 0o7777,
        Err(_) => 0o644,
    };
    let mut name = path.file_name().unwrap_or_default().to_owned();
    name.push(format!(".{}", std::process::id()));
    let tmp = dir.join(name);
    let _ = fs::remove_file(&tmp);
    let written = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(&tmp)
        .and_then(|mut f| f.write_all(text.as_bytes()).and_then(|()| f.sync_all()));
    match written.and_then(|()| fs::rename(&tmp, &path)) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(file: &Path, env: &[(&str, &str)], overrides: &[&str]) -> Source {
        let env: BTreeMap<String, String> =
            env.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect();
        Source {
            file: Some(file.to_path_buf()),
            overrides: overrides.iter().map(|o| (*o).to_owned()).collect(),
            environment: Environment::new(move |name| {
                env.get(name.to_str()?).map(|v| v.clone().into())
            }),
        }
    }

    fn row<'a>(rows: &'a [Row], key: &str) -> &'a Row {
        rows.iter().find(|r| r.key == key).unwrap_or_else(|| panic!("no {key}"))
    }

    #[test]
    fn a_value_is_replaced_where_it_is_and_the_rest_stays() {
        let text = "# mine\n[ui]\nmouse = true   # the mouse\nleft = \"~/a # b\"\n\n[jobs]\n";
        assert_eq!(
            set(text, "ui.mouse", "false").unwrap(),
            "# mine\n[ui]\nmouse = false   # the mouse\nleft = \"~/a # b\"\n\n[jobs]\n"
        );
        assert_eq!(
            set(text, "ui.left", "\"/srv\"").unwrap(),
            "# mine\n[ui]\nmouse = true   # the mouse\nleft = \"/srv\"\n\n[jobs]\n"
        );
        // Written the other ways TOML allows.
        assert_eq!(set("ui.mouse = true\n", "ui.mouse", "false").unwrap(), "ui.mouse = false\n");
        assert_eq!(
            set("ui = { mouse = true }\n", "ui.mouse", "false").unwrap(),
            "ui = { mouse = false }\n"
        );
        assert_eq!(
            set("[keys]\ncopy = [\n  \"F5\",\n  \"c\",\n]\n", "keys.copy", "[\"y\"]").unwrap(),
            "[keys]\ncopy = [\"y\"]\n"
        );
    }

    #[test]
    fn a_new_setting_goes_to_the_end_of_its_section_or_into_a_new_one() {
        let text = "[ui]\nmouse = true\n\n# transfers\n[jobs]  # how many\n";
        assert_eq!(
            set(text, "ui.show_hidden", "true").unwrap(),
            "[ui]\nmouse = true\nshow_hidden = true\n\n# transfers\n[jobs]  # how many\n"
        );
        assert_eq!(
            set(text, "jobs.poll_ms", "250").unwrap(),
            "[ui]\nmouse = true\n\n# transfers\n[jobs]  # how many\npoll_ms = 250\n"
        );
        assert_eq!(
            set(text, "rclone.transfers", "8").unwrap(),
            format!("{text}\n[rclone]\ntransfers = 8\n")
        );
        assert_eq!(set("", "ui.mouse", "false").unwrap(), "[ui]\nmouse = false\n");
        assert_eq!(set("[ui]", "ui.mouse", "false").unwrap(), "[ui]\nmouse = false\n");
        assert_eq!(set("# mine", "ui.mouse", "false").unwrap(), "# mine\n\n[ui]\nmouse = false\n");
    }

    #[test]
    fn a_setting_is_taken_out_with_its_line() {
        let text = "[ui]\nmouse = false  # no mouse\nleft = \"~\"\n[keys]\ncopy = [\n  \"y\",\n]\n";
        assert_eq!(
            remove(text, "ui.mouse").unwrap(),
            "[ui]\nleft = \"~\"\n[keys]\ncopy = [\n  \"y\",\n]\n"
        );
        assert_eq!(
            remove(text, "keys.copy").unwrap(),
            "[ui]\nmouse = false  # no mouse\nleft = \"~\"\n[keys]\n"
        );
        assert_eq!(remove(text, "ui.right").unwrap(), text);
        assert!(remove("ui = { mouse = true }\n", "ui.mouse").is_err());
    }

    #[test]
    fn the_page_lists_every_setting_with_the_files_value_and_what_is_above_it() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("config.toml");
        fs::write(&file, "[ui]\nmouse = false\n[keys]\ncopy = [\"F5\", \"y\"]\n").unwrap();
        let source =
            source(&file, &[("RSE_UI_MOUSE", "true")], &["jobs.max_concurrent=5", "ui.left=/x"]);
        let rows = source.rows().unwrap();
        let mouse = row(&rows, "ui.mouse");
        assert_eq!(
            (mouse.kind, &mouse.value, mouse.in_file),
            (Kind::Bool, &Value::Bool(false), true)
        );
        assert_eq!(
            mouse.above,
            Some((Above::Variable("RSE_UI_MOUSE".to_owned()), Value::Bool(true)))
        );
        assert!(mouse.live);
        let jobs = row(&rows, "jobs.max_concurrent");
        assert_eq!((jobs.kind, &jobs.value, jobs.in_file), (Kind::Number, &Value::Int(2), false));
        assert_eq!(jobs.above, Some((Above::CommandLine, Value::Int(5))));
        let copy = row(&rows, "keys.copy");
        assert_eq!((copy.kind, copy.text().as_str(), copy.live), (Kind::Keys, "F5 y", true));
        assert_eq!(copy.description_id(), "key-copy");
        let language = row(&rows, "ui.language");
        assert_eq!(language.next(false), Some(Value::Str("en".to_owned())));
        assert_eq!(language.next(true), Some(Value::Str("zh-CN".to_owned())));
        assert_eq!(language.description_id(), "setting-ui-language");
        assert!(!row(&rows, "rclone.transfers").live);
        // In the file's order, a heading above each section.
        let lines = lines(&rows);
        assert_eq!(lines[0], Line::Section("rclone"));
        assert_eq!(lines[1], Line::Row(0));
        assert!(lines.contains(&Line::Section("keys")) && lines.contains(&Line::Blank));
    }

    #[test]
    fn writing_keeps_the_file_and_returns_the_settings_of_every_layer() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("rse/config.toml");
        let source = source(&file, &[("RSE_UI_MOUSE", "false")], &[]);
        // No file yet: it is made, with its directory.
        let (config, _) = source.write("jobs.max_concurrent", Some(&Value::Int(3))).unwrap();
        assert_eq!((config.jobs.max_concurrent, config.ui.mouse), (3, false));
        assert_eq!(fs::read_to_string(&file).unwrap(), "[jobs]\nmax_concurrent = 3\n");
        let keys = Value::List(vec!["F5".to_owned(), "y".to_owned()]);
        let (config, keymap) = source.write("keys.copy", Some(&keys)).unwrap();
        assert_eq!(config.keys["copy"], ["F5", "y"]);
        assert_eq!(keymap.label(crate::keymap::Action::Copy), "F5");
        let (config, _) = source.write("jobs.max_concurrent", None).unwrap();
        assert_eq!(config.jobs.max_concurrent, 2);
        assert_eq!(
            fs::read_to_string(&file).unwrap(),
            "[jobs]\n\n[keys]\ncopy = [\"F5\", \"y\"]\n"
        );
    }

    #[test]
    fn a_value_that_is_none_of_its_setting_is_not_written() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("config.toml");
        let text = "[jobs]\npoll_ms = 500\n";
        fs::write(&file, text).unwrap();
        let source = source(&file, &[], &[]);
        assert!(source.write("jobs.poll_ms", Some(&Value::Int(10))).is_err());
        assert!(source.write("jobs.poll_ms", Some(&Value::Int(-1))).is_err());
        assert!(source.write("ui.language", Some(&Value::Str("fr".to_owned()))).is_err());
        // A key another action has, and a key that is none.
        let taken = Value::List(vec!["m".to_owned()]);
        assert!(source.write("keys.copy", Some(&taken)).is_err());
        let unknown = Value::List(vec!["NoSuchKey".to_owned()]);
        assert!(source.write("keys.copy", Some(&unknown)).is_err());
        assert_eq!(fs::read_to_string(&file).unwrap(), text);
        assert!(Source::none().write("ui.mouse", Some(&Value::Bool(false))).is_err());
    }

    #[test]
    fn a_link_stays_a_link_and_the_file_keeps_its_permissions() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let (real, link) = (dir.path().join("real.toml"), dir.path().join("config.toml"));
        fs::write(&real, "[ui]\n").unwrap();
        fs::set_permissions(&real, fs::Permissions::from_mode(0o600)).unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();
        source(&link, &[], &[]).write("ui.mouse", Some(&Value::Bool(false))).unwrap();
        assert!(fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
        assert_eq!(fs::read_to_string(&real).unwrap(), "[ui]\nmouse = false\n");
        assert_eq!(fs::metadata(&real).unwrap().permissions().mode() & 0o777, 0o600);
    }
}
