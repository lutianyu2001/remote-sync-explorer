//! The settings page as the interface sees it: moving through the settings, changing the
//! one under the cursor, which is written to the user's file at once (see
//! [`crate::settings`]), and taking up what holds without a new start.

use crossterm::event::{KeyCode, KeyEvent};
use layeredcfg::Value;

use super::{App, Dialog, Life, edit_text};
use crate::config::Config;
use crate::hitmap::Spot;
use crate::keymap::{Action, Keymap};
use crate::settings::{Line, Row, lines};

/// Rows a page key moves by.
const PAGE: isize = 10;

#[derive(Debug, Clone, PartialEq)]
pub struct SettingsPage {
    pub rows: Vec<Row>,
    /// The setting under the cursor, by its number in `rows`.
    pub cursor: usize,
    /// The value being typed for it, and the cursor in that text.
    pub edit: Option<(String, usize)>,
    /// The list of its values opened under it, by the choice that list's cursor is on.
    pub menu: Option<usize>,
}

impl SettingsPage {
    /// Moves the cursor by `delta` settings, or the cursor of the list opened under one by
    /// as many choices; not while a value is typed.
    pub(super) fn step(&mut self, delta: isize) {
        let choices = self.row().map_or(0, |row| row.values().len());
        if let Some(at) = &mut self.menu {
            *at = at.saturating_add_signed(delta).min(choices.saturating_sub(1));
        } else if self.edit.is_none() {
            let last = self.rows.len().saturating_sub(1);
            self.cursor = self.cursor.saturating_add_signed(delta).min(last);
        }
    }

    /// Changes the setting under the cursor as Enter does: one that is on or off or one of
    /// a few opens the list of its values, the cursor on the one it has; another has its
    /// value typed.
    fn change(&mut self) {
        let Some(row) = self.row() else { return };
        let values = row.values();
        if values.is_empty() {
            let text = row.text();
            let cursor = text.chars().count();
            self.edit = Some((text, cursor));
        } else {
            self.menu = Some(values.iter().position(|v| *v == row.value).unwrap_or(0));
        }
    }

    /// A click on line `line` of the page: the cursor goes to its setting, which is
    /// changed as Enter changes it. With a list open under a setting, the click only
    /// closes it.
    pub(super) fn click(&mut self, line: usize) {
        if self.menu.take().is_some() {
            return;
        }
        let Some(Line::Row(row)) = lines(&self.rows).get(line).copied() else { return };
        if row == self.cursor && self.edit.is_some() {
            return;
        }
        self.cursor = row;
        self.edit = None;
        self.change();
    }

    fn row(&self) -> Option<&Row> {
        self.rows.get(self.cursor)
    }
}

/// An error with what caused it, as the command line shows one.
fn causes(error: &anyhow::Error) -> String {
    let causes: Vec<String> = error.chain().map(ToString::to_string).collect();
    causes.join(&tr!("cause-separator"))
}

impl App {
    /// The file the settings page writes.
    pub fn settings_file(&self) -> Option<&std::path::Path> {
        self.settings.file.as_deref()
    }

    pub(super) fn open_settings(&mut self) {
        match self.settings.rows() {
            Ok(rows) => {
                let page = SettingsPage { rows, cursor: 0, edit: None, menu: None };
                self.dialog = Some(Dialog::Settings(Box::new(page)));
            }
            Err(e) => self.fail(tr!("settings-unreadable", error = causes(&e))),
        }
    }

    /// A key for the settings page; whether the page stays open.
    pub(super) fn settings_key(&mut self, page: &mut SettingsPage, key: KeyEvent) -> bool {
        if let Some((text, cursor)) = &mut page.edit {
            match key.code {
                KeyCode::Esc => page.edit = None,
                KeyCode::Enter => {
                    let text = text.clone();
                    self.settings_typed(page, &text);
                }
                _ => edit_text(text, cursor, key.code, key.modifiers),
            }
            return true;
        }
        if let Some(at) = page.menu {
            match key.code {
                KeyCode::Esc => page.menu = None,
                KeyCode::Up | KeyCode::Char('k') => page.step(-1),
                KeyCode::Down | KeyCode::Char('j') => page.step(1),
                KeyCode::Home | KeyCode::PageUp => page.menu = Some(0),
                KeyCode::End | KeyCode::PageDown => page.step(isize::MAX),
                KeyCode::Enter | KeyCode::Char(' ') => self.settings_choose(page, at),
                _ => {}
            }
            return true;
        }
        if self.keymap.pane_action(&key) == Some(Action::Settings) {
            return false;
        }
        match key.code {
            KeyCode::Esc => return false,
            KeyCode::Up | KeyCode::Char('k') => page.step(-1),
            KeyCode::Down | KeyCode::Char('j') => page.step(1),
            KeyCode::PageUp => page.step(-PAGE),
            KeyCode::PageDown => page.step(PAGE),
            KeyCode::Home => page.cursor = 0,
            KeyCode::End => page.cursor = page.rows.len().saturating_sub(1),
            KeyCode::Enter => page.change(),
            // The next value at once, without the list.
            KeyCode::Char(' ') => {
                if !self.settings_next(page, false) {
                    page.change();
                }
            }
            KeyCode::Left | KeyCode::Right => {
                self.settings_next(page, key.code == KeyCode::Left);
            }
            KeyCode::Delete | KeyCode::Backspace if page.row().is_some_and(|row| row.in_file) => {
                self.settings_write(page, None);
            }
            _ => {}
        }
        true
    }

    /// A click on an arrow beside a setting's value, or on a choice of the list under it.
    pub(super) fn settings_spot(&mut self, page: &mut SettingsPage, spot: Spot) {
        match spot {
            Spot::Back(row) | Spot::Forward(row) => {
                (page.cursor, page.edit, page.menu) = (row, None, None);
                self.settings_next(page, matches!(spot, Spot::Back(_)));
            }
            Spot::Choice(at) => self.settings_choose(page, at),
        }
    }

    /// Gives the setting under the cursor the value after the one it has (the one before
    /// with `back`). Whether it is a setting with such values.
    fn settings_next(&mut self, page: &mut SettingsPage, back: bool) -> bool {
        let Some(value) = page.row().and_then(|row| row.next(back)) else { return false };
        self.settings_write(page, Some(&value));
        true
    }

    /// Takes choice `at` of the open list as the value of the setting under the cursor,
    /// and closes the list.
    fn settings_choose(&mut self, page: &mut SettingsPage, at: usize) {
        page.menu = None;
        if let Some(value) = page.row().and_then(|row| row.values().get(at).cloned()) {
            self.settings_write(page, Some(&value));
        }
    }

    /// Takes the typed `text` as the value of the setting under the cursor; a value that
    /// cannot be one is left to be corrected.
    fn settings_typed(&mut self, page: &mut SettingsPage, text: &str) {
        let Some(row) = page.row() else { return };
        match row.parse(text) {
            Ok(value) => {
                if self.settings_write(page, Some(&value)) {
                    page.edit = None;
                }
            }
            Err(e) => self.show(tr!("settings-failed", error = causes(&e)), Life::Long),
        }
    }

    /// Writes the setting under the cursor into the user's file (without a value, takes
    /// it out, for its default), and takes up the settings as they then are. Whether it
    /// was written.
    fn settings_write(&mut self, page: &mut SettingsPage, value: Option<&Value>) -> bool {
        let Some(row) = page.row() else { return false };
        let (key, live) = (row.key.clone(), row.live);
        match self.settings.write(&key, value) {
            Ok((config, keymap)) => {
                self.apply_settings(config, keymap);
                if let Ok(rows) = self.settings.rows() {
                    page.rows = rows;
                }
                let key = key.as_str();
                self.say(if live {
                    tr!("settings-saved", key = key)
                } else {
                    tr!("settings-saved-restart", key = key)
                });
                true
            }
            Err(e) => {
                self.show(tr!("settings-failed", error = causes(&e)), Life::Long);
                false
            }
        }
    }

    /// Takes up the settings that hold without a new start: the interface's, the keys,
    /// and how many jobs run at once. The mouse is turned on and off by the event loop,
    /// which reads `ui.mouse`.
    fn apply_settings(&mut self, config: Config, keymap: Keymap) {
        if config.ui.language != self.ui.language {
            crate::i18n::set(config.ui.language);
        }
        // Only when the setting changed: the key that shows hidden files for now stays
        // in charge otherwise.
        if config.ui.show_hidden != self.ui.show_hidden {
            self.show_hidden = config.ui.show_hidden;
            self.rebuild_all();
        }
        self.ui = config.ui;
        self.keymap = keymap;
        let effects = self.queue.set_max_concurrent(config.jobs.max_concurrent);
        self.run_effects(effects);
    }
}
