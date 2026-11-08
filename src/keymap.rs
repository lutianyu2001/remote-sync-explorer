//! Key bindings: every action has default keys that `[keys]` in the config can replace.

use std::collections::{BTreeMap, HashMap};
use std::fmt;

use anyhow::{Result, bail};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Action {
    SwitchPane,
    Up,
    Down,
    PageUp,
    PageDown,
    Home,
    End,
    Enter,
    Parent,
    Mark,
    MarkAll,
    Filter,
    Sort,
    /// Sort the other way round: Z to A, smallest or oldest first.
    SortReverse,
    ToggleHidden,
    Goto,
    Remote,
    Swap,
    Refresh,
    Copy,
    Move,
    Rename,
    Mkdir,
    Delete,
    /// Turn overwrite mode on or off.
    OverwriteMode,
    Queue,
    ErrorLog,
    Help,
    Quit,
    QueueCancel,
    QueueRetry,
    QueueClear,
    /// Focus a window: its key is shown in the window's top right corner.
    FocusLeftAddress,
    FocusLeft,
    FocusRightAddress,
    FocusRight,
    FocusQueue,
}

/// Where an action applies: in the panes or in the transfer queue (or both).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scope {
    Panes,
    Queue,
    Both,
}

struct Binding {
    action: Action,
    name: &'static str,
    keys: &'static [&'static str],
    scope: Scope,
}

const BINDINGS: &[Binding] = &[
    Binding {
        action: Action::SwitchPane,
        name: "switch_pane",
        keys: &["Tab"],
        scope: Scope::Panes,
    },
    Binding { action: Action::Up, name: "up", keys: &["Up", "k"], scope: Scope::Both },
    Binding { action: Action::Down, name: "down", keys: &["Down", "j"], scope: Scope::Both },
    Binding { action: Action::PageUp, name: "page_up", keys: &["PgUp"], scope: Scope::Both },
    Binding { action: Action::PageDown, name: "page_down", keys: &["PgDn"], scope: Scope::Both },
    Binding { action: Action::Home, name: "home", keys: &["Home"], scope: Scope::Both },
    Binding { action: Action::End, name: "end", keys: &["End"], scope: Scope::Both },
    Binding { action: Action::Enter, name: "enter", keys: &["Enter", "l"], scope: Scope::Panes },
    Binding {
        action: Action::Parent,
        name: "parent",
        keys: &["Backspace", "h"],
        scope: Scope::Panes,
    },
    Binding { action: Action::Mark, name: "mark", keys: &["Space", "Insert"], scope: Scope::Panes },
    Binding { action: Action::MarkAll, name: "mark_all", keys: &["a"], scope: Scope::Panes },
    Binding { action: Action::Filter, name: "filter", keys: &["/"], scope: Scope::Panes },
    Binding { action: Action::Sort, name: "sort", keys: &["s"], scope: Scope::Panes },
    Binding {
        action: Action::SortReverse,
        name: "sort_reverse",
        keys: &["S"],
        scope: Scope::Panes,
    },
    Binding {
        action: Action::ToggleHidden,
        name: "toggle_hidden",
        keys: &["."],
        scope: Scope::Panes,
    },
    Binding { action: Action::Goto, name: "goto", keys: &[":"], scope: Scope::Panes },
    Binding { action: Action::Remote, name: "remote", keys: &["F10", "g"], scope: Scope::Panes },
    Binding { action: Action::Swap, name: "swap", keys: &["Ctrl+U"], scope: Scope::Panes },
    Binding { action: Action::Refresh, name: "refresh", keys: &["Ctrl+R"], scope: Scope::Panes },
    Binding { action: Action::Copy, name: "copy", keys: &["F5", "c"], scope: Scope::Panes },
    Binding { action: Action::Move, name: "move", keys: &["F6", "m"], scope: Scope::Panes },
    Binding { action: Action::Rename, name: "rename", keys: &["F2", "r"], scope: Scope::Panes },
    Binding { action: Action::Mkdir, name: "mkdir", keys: &["F7", "n"], scope: Scope::Panes },
    Binding {
        action: Action::Delete,
        name: "delete",
        keys: &["F8", "Delete"],
        scope: Scope::Panes,
    },
    Binding {
        action: Action::OverwriteMode,
        name: "overwrite_mode",
        keys: &["O"],
        scope: Scope::Both,
    },
    Binding { action: Action::Queue, name: "queue", keys: &["Ctrl+T"], scope: Scope::Both },
    Binding {
        action: Action::FocusLeftAddress,
        name: "focus_left_address",
        keys: &["1"],
        scope: Scope::Both,
    },
    Binding { action: Action::FocusLeft, name: "focus_left", keys: &["2"], scope: Scope::Both },
    Binding {
        action: Action::FocusRightAddress,
        name: "focus_right_address",
        keys: &["3"],
        scope: Scope::Both,
    },
    Binding { action: Action::FocusRight, name: "focus_right", keys: &["4"], scope: Scope::Both },
    Binding { action: Action::FocusQueue, name: "focus_queue", keys: &["5"], scope: Scope::Both },
    Binding { action: Action::ErrorLog, name: "error_log", keys: &["E"], scope: Scope::Both },
    Binding { action: Action::Help, name: "help", keys: &["?"], scope: Scope::Both },
    Binding { action: Action::Quit, name: "quit", keys: &["q"], scope: Scope::Both },
    Binding {
        action: Action::QueueCancel,
        name: "queue_cancel",
        keys: &["x"],
        scope: Scope::Queue,
    },
    Binding { action: Action::QueueRetry, name: "queue_retry", keys: &["r"], scope: Scope::Queue },
    Binding { action: Action::QueueClear, name: "queue_clear", keys: &["c"], scope: Scope::Queue },
];

/// A key with its relevant modifiers (Ctrl, Alt).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Key {
    pub code: KeyCode,
    pub mods: KeyModifiers,
}

impl Key {
    pub fn from_event(ev: &KeyEvent) -> Self {
        let mut mods = ev.modifiers & (KeyModifiers::CONTROL | KeyModifiers::ALT);
        let code = match ev.code {
            // Terminals report Ctrl+U as either 'u' or 'U' with Ctrl.
            KeyCode::Char(c) if mods.contains(KeyModifiers::CONTROL) => {
                KeyCode::Char(c.to_ascii_lowercase())
            }
            KeyCode::BackTab => {
                mods |= KeyModifiers::SHIFT;
                KeyCode::Tab
            }
            c => c,
        };
        Self { code, mods }
    }

    /// Parses `F5`, `c`, `E`, `Ctrl+U`, `Alt+x`, `Space`, `PgDn`, ...
    pub fn parse(s: &str) -> Result<Self> {
        let mut mods = KeyModifiers::NONE;
        let mut rest = s;
        loop {
            if let Some(r) = strip_prefix_ci(rest, "ctrl+") {
                mods |= KeyModifiers::CONTROL;
                rest = r;
            } else if let Some(r) = strip_prefix_ci(rest, "alt+") {
                mods |= KeyModifiers::ALT;
                rest = r;
            } else {
                break;
            }
        }
        let mut chars = rest.chars();
        let code = match (chars.next(), chars.next()) {
            (Some(c), None) if mods.contains(KeyModifiers::CONTROL) => {
                KeyCode::Char(c.to_ascii_lowercase())
            }
            (Some(c), None) => KeyCode::Char(c),
            _ => match rest.to_ascii_lowercase().as_str() {
                "tab" => KeyCode::Tab,
                "enter" | "return" => KeyCode::Enter,
                "esc" | "escape" => KeyCode::Esc,
                "backspace" => KeyCode::Backspace,
                "space" => KeyCode::Char(' '),
                "insert" | "ins" => KeyCode::Insert,
                "delete" | "del" => KeyCode::Delete,
                "up" => KeyCode::Up,
                "down" => KeyCode::Down,
                "left" => KeyCode::Left,
                "right" => KeyCode::Right,
                "pgup" | "pageup" => KeyCode::PageUp,
                "pgdn" | "pagedown" => KeyCode::PageDown,
                "home" => KeyCode::Home,
                "end" => KeyCode::End,
                f if f.starts_with('f')
                    && f[1..].parse::<u8>().is_ok_and(|n| (1..=24).contains(&n)) =>
                {
                    KeyCode::F(f[1..].parse()?)
                }
                _ => bail!("{}", tr!("keys-unknown-key", key = s)),
            },
        };
        Ok(Self { code, mods })
    }
}

fn strip_prefix_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    let (head, rest) = s.split_at_checked(prefix.len())?;
    (!rest.is_empty() && head.eq_ignore_ascii_case(prefix)).then_some(rest)
}

#[derive(Debug, Clone)]
pub struct Keymap {
    panes: HashMap<Key, Action>,
    queue: HashMap<Key, Action>,
    labels: HashMap<Action, String>,
}

impl Default for Keymap {
    fn default() -> Self {
        // The built-in table is valid; an empty override cannot fail.
        Self::new(&BTreeMap::new()).unwrap_or_else(|_| unreachable!())
    }
}

impl Keymap {
    /// Builds the keymap from the defaults plus the config's `[keys]` overrides.
    ///
    /// # Errors
    /// Fails on unknown action names, unparsable keys, or a key bound to two actions.
    pub fn new(overrides: &BTreeMap<String, Vec<String>>) -> Result<Self> {
        for name in overrides.keys() {
            if !BINDINGS.iter().any(|b| b.name == name) {
                bail!("{}", tr!("keys-unknown-action", name = name.as_str()));
            }
        }
        let mut map = Self { panes: HashMap::new(), queue: HashMap::new(), labels: HashMap::new() };
        for b in BINDINGS {
            let keys: Vec<String> = match overrides.get(b.name) {
                Some(keys) => keys.clone(),
                None => b.keys.iter().map(|k| (*k).to_owned()).collect(),
            };
            if let Some(first) = keys.first() {
                map.labels.insert(b.action, first.clone());
            }
            for k in &keys {
                let key = Key::parse(k).map_err(|e| {
                    anyhow::anyhow!("{}", tr!("keys-bad-key", name = b.name, error = e.to_string()))
                })?;
                if matches!(b.scope, Scope::Panes | Scope::Both) {
                    insert_unique(&mut map.panes, key, b, k)?;
                }
                if matches!(b.scope, Scope::Queue | Scope::Both) {
                    insert_unique(&mut map.queue, key, b, k)?;
                }
            }
        }
        Ok(map)
    }

    pub fn pane_action(&self, ev: &KeyEvent) -> Option<Action> {
        self.panes.get(&Key::from_event(ev)).copied()
    }

    pub fn queue_action(&self, ev: &KeyEvent) -> Option<Action> {
        self.queue.get(&Key::from_event(ev)).copied()
    }

    /// The first key bound to `action`, as written in the config (`F5`).
    pub fn label(&self, action: Action) -> &str {
        self.labels.get(&action).map_or("", String::as_str)
    }

    /// (keys, description) for the help screen.
    pub fn help(&self) -> Vec<(String, String)> {
        BINDINGS
            .iter()
            .map(|b| {
                let source = if b.scope == Scope::Queue { &self.queue } else { &self.panes };
                let mut keys: Vec<String> = source
                    .iter()
                    .filter(|(_, a)| **a == b.action)
                    .map(|(k, _)| KeyLabel(*k).to_string())
                    .collect();
                keys.sort();
                keys.dedup();
                (keys.join(" / "), crate::i18n::text(&help_id(b.name), std::iter::empty()))
            })
            .collect()
    }
}

/// The Fluent message describing the action named `name` (`key-copy`).
fn help_id(name: &str) -> String {
    format!("key-{name}")
}

/// The Fluent messages describing the actions, for checking they all exist.
#[cfg(test)]
pub fn help_ids() -> impl Iterator<Item = String> {
    BINDINGS.iter().map(|b| help_id(b.name))
}

fn insert_unique(
    map: &mut HashMap<Key, Action>,
    key: Key,
    b: &Binding,
    written: &str,
) -> Result<()> {
    if let Some(other) = map.insert(key, b.action)
        && other != b.action
    {
        {
            let other_name = BINDINGS.iter().find(|x| x.action == other).map_or("?", |x| x.name);
            bail!(
                "{}",
                tr!("keys-bound-twice", key = written, first = other_name, second = b.name)
            );
        }
    }
    Ok(())
}

/// Displays a key the way the config spells it.
struct KeyLabel(Key);

impl fmt::Display for KeyLabel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Key { code, mods } = self.0;
        if mods.contains(KeyModifiers::CONTROL) {
            f.write_str("Ctrl+")?;
        }
        if mods.contains(KeyModifiers::ALT) {
            f.write_str("Alt+")?;
        }
        match code {
            KeyCode::Char(' ') => f.write_str("Space"),
            KeyCode::Char(c) if mods.contains(KeyModifiers::CONTROL) => {
                write!(f, "{}", c.to_ascii_uppercase())
            }
            KeyCode::Char(c) => write!(f, "{c}"),
            KeyCode::F(n) => write!(f, "F{n}"),
            KeyCode::PageUp => f.write_str("PgUp"),
            KeyCode::PageDown => f.write_str("PgDn"),
            other => write!(f, "{other:?}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    #[test]
    fn default_keys_map_to_their_actions() {
        let k = Keymap::default();
        assert_eq!(k.pane_action(&ev(KeyCode::F(5), KeyModifiers::NONE)), Some(Action::Copy));
        assert_eq!(k.pane_action(&ev(KeyCode::Char('c'), KeyModifiers::NONE)), Some(Action::Copy));
        assert_eq!(
            k.pane_action(&ev(KeyCode::Char('u'), KeyModifiers::CONTROL)),
            Some(Action::Swap)
        );
        assert_eq!(
            k.pane_action(&ev(KeyCode::Char('U'), KeyModifiers::CONTROL)),
            Some(Action::Swap)
        );
        assert_eq!(
            k.pane_action(&ev(KeyCode::Char('E'), KeyModifiers::SHIFT)),
            Some(Action::ErrorLog)
        );
        assert_eq!(k.pane_action(&ev(KeyCode::Char('?'), KeyModifiers::SHIFT)), Some(Action::Help));
        assert_eq!(k.pane_action(&ev(KeyCode::Char(' '), KeyModifiers::NONE)), Some(Action::Mark));
        assert_eq!(k.pane_action(&ev(KeyCode::Delete, KeyModifiers::NONE)), Some(Action::Delete));
        assert_eq!(
            k.pane_action(&ev(KeyCode::Char('r'), KeyModifiers::NONE)),
            Some(Action::Rename)
        );
        // The same key means something else inside the queue.
        assert_eq!(
            k.queue_action(&ev(KeyCode::Char('r'), KeyModifiers::NONE)),
            Some(Action::QueueRetry)
        );
        assert_eq!(
            k.queue_action(&ev(KeyCode::Char('x'), KeyModifiers::NONE)),
            Some(Action::QueueCancel)
        );
        assert_eq!(k.queue_action(&ev(KeyCode::Down, KeyModifiers::NONE)), Some(Action::Down));
        assert_eq!(k.pane_action(&ev(KeyCode::Char('x'), KeyModifiers::NONE)), None);
        // A capital, so that it is not turned on by a slip.
        assert_eq!(
            k.queue_action(&ev(KeyCode::Char('O'), KeyModifiers::SHIFT)),
            Some(Action::OverwriteMode)
        );
        assert_eq!(k.pane_action(&ev(KeyCode::Char('o'), KeyModifiers::NONE)), None);
        assert_eq!(k.label(Action::Copy), "F5");
    }

    #[test]
    fn overrides_replace_defaults() {
        let mut o = BTreeMap::new();
        o.insert("copy".to_owned(), vec!["F9".to_owned(), "Alt+c".to_owned()]);
        let k = Keymap::new(&o).unwrap();
        assert_eq!(k.pane_action(&ev(KeyCode::F(9), KeyModifiers::NONE)), Some(Action::Copy));
        assert_eq!(k.pane_action(&ev(KeyCode::Char('c'), KeyModifiers::ALT)), Some(Action::Copy));
        assert_eq!(k.pane_action(&ev(KeyCode::F(5), KeyModifiers::NONE)), None);
        assert_eq!(k.label(Action::Copy), "F9");
    }

    #[test]
    fn bad_overrides_are_rejected() {
        let one = |name: &str, key: &str| {
            let mut o = BTreeMap::new();
            o.insert(name.to_owned(), vec![key.to_owned()]);
            Keymap::new(&o)
        };
        assert!(one("copyy", "F5").is_err());
        assert!(one("copy", "F99").is_err());
        assert!(one("copy", "Hyper+x").is_err());
        let err = one("copy", "m").unwrap_err().to_string();
        assert!(err.contains("`m` is bound to both"), "{err}");
    }

    #[test]
    fn parses_key_names() {
        assert_eq!(
            Key::parse("ctrl+t").unwrap(),
            Key { code: KeyCode::Char('t'), mods: KeyModifiers::CONTROL }
        );
        assert_eq!(Key::parse("PgDn").unwrap().code, KeyCode::PageDown);
        assert_eq!(Key::parse("+").unwrap().code, KeyCode::Char('+'));
        assert_eq!(Key::parse("F12").unwrap().code, KeyCode::F(12));
        assert!(Key::parse("ctr日").is_err(), "no byte slicing inside a character");
    }

    #[test]
    fn help_lists_every_action() {
        let help = Keymap::default().help();
        assert_eq!(help.len(), BINDINGS.len());
        assert!(help.iter().any(|(k, d)| k == "F5 / c" && d.starts_with("Copy")));
    }
}

/// Proofs over every input up to a size, checked by `cargo kani` (see SECURITY.md).
#[cfg(kani)]
mod proofs {
    use super::*;

    /// Any text of up to 7 bytes: never sliced inside a character, and what is left is the
    /// end of it.
    #[kani::proof]
    #[kani::unwind(9)]
    fn prefixes_are_cut_at_character_boundaries() {
        let bytes: [u8; 7] = kani::any();
        let len: usize = kani::any_where(|&n| n <= 7);
        let Ok(text) = std::str::from_utf8(&bytes[..len]) else { return };
        if let Some(rest) = strip_prefix_ci(text, "alt+") {
            assert!(text.ends_with(rest) && rest.len() + 4 == text.len());
        }
    }
}
