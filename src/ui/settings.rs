//! The settings page: every setting under its section's heading, the cursor's setting
//! explained under the list.

use layeredcfg::Value;
use lazyui_ratatui::{Choice, Dropdown, Field};
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::Widget;

use super::dialogs::{Drawn, centered, frame};
use super::panes::wrap;
use super::text::{ellipsize_middle, fit, shown, take_width, width};
use crate::app::{App, SettingsPage, visible_offset};
use crate::hitmap::Spot;
use crate::settings::{Above, Line, Row, lines};

/// The lines under the list: a rule, what the setting is (two lines), its notes, the keys.
const FOOT: u16 = 5;

/// The widest a setting's name is shown; the rest of a longer one is under the list.
const NAME: usize = 26;

/// The room a value keeps beside the names of the file, which give way first.
const VALUE: usize = 16;

/// The cells before a value, which the list of its values opened under it takes for its
/// border and its marks.
const GAP: usize = 4;

fn message(id: &str) -> String {
    crate::i18n::text(id, std::iter::empty())
}

/// A value as the file writes it, an empty text as `""`.
fn written(value: &Value) -> String {
    match value {
        Value::Str(s) if s.is_empty() => "\"\"".to_owned(),
        value => shown(&crate::settings::text(value)).into_owned(),
    }
}

/// A value by its name, where it has one.
fn value_name(row: &Row, value: &Value) -> String {
    row.value_id(value).map_or_else(|| written(value), |id| message(&id))
}

/// Draws the value of setting `index` from `at`, in `w` cells. One that is on or off or
/// one of a few is named between the arrows that Left and Right are, in the middle of the
/// `name_w` cells that hold the longest such name of the page, so that the arrows of all
/// settings stand in two columns; as the file writes it after that. The arrows take clicks.
fn draw_value(
    buf: &mut Buffer,
    (index, row): (usize, &Row),
    at: (u16, u16),
    (w, name_w): (usize, Option<usize>),
    style: Style,
) -> Vec<(Rect, Spot)> {
    let (x, y) = at;
    let room = |end: u16| w.saturating_sub(usize::from(end - x));
    // What the file sets stands out from the defaults.
    let set = if row.in_file { style.add_modifier(Modifier::BOLD) } else { style };
    let mut spots = Vec::new();
    if let Some(id) = row.value_id(&row.value) {
        let (name, file) = (message(&id), written(&row.value));
        let area = Rect::new(x, y, u16::try_from(w).unwrap_or(u16::MAX), 1);
        let mut choice =
            Choice::new(&name).style(set).note_style(style.add_modifier(Modifier::DIM));
        if let Some(cells) = name_w {
            choice = choice.width(cells);
        }
        // What is above the file for this run comes before how the file writes the value.
        if row.above.is_none() {
            choice = choice.note(&file);
        }
        if let Some((back, forward)) = choice.arrows(area) {
            spots.push((back, Spot::Back(index)));
            spots.push((forward, Spot::Forward(index)));
            if row.above.is_some() {
                let end = forward.right();
                buf.set_stringn(end, y, tail(row), room(end), style.fg(Color::Yellow));
            }
        }
        choice.render(area, buf);
    } else {
        // A typed value stands where the named ones do: past the left arrow's column, and
        // in the middle of their cells when it fits there.
        let text = written(&row.value);
        let before = name_w.map_or(0, |cells| cells.saturating_sub(width(&text)) / 2);
        let from = x + u16::try_from(2 + before).unwrap_or(0);
        let (end, _) = buf.set_stringn(from, y, text, room(from), set);
        if let Some((_, effective)) = &row.above {
            let now = format!("  → {}", value_name(row, effective));
            buf.set_stringn(end, y, now, room(end), style.fg(Color::Yellow));
        }
    }
    spots
}

/// What follows the arrows of a value: what holds for this run where that is above the
/// file, otherwise the value as the file writes it.
fn tail(row: &Row) -> String {
    match &row.above {
        Some((_, effective)) => format!("  → {}", value_name(row, effective)),
        None => format!("  {}", written(&row.value)),
    }
}

/// The cells between the arrows of every value of `rows`, where a value has `w` cells: the
/// longest name any of them can take, so that no arrow moves when a value changes; fewer
/// where that would push what follows the arrows out, but never fewer than the names now
/// shown need. None where even that pushes it out: each value then keeps its arrows close.
fn choice_width(rows: &[Row], w: usize) -> Option<usize> {
    let rows = || rows.iter().filter(|row| row.value_id(&row.value).is_some());
    let widths = |row: &Row| -> Vec<usize> {
        row.values().iter().map(|value| width(&value_name(row, value))).collect()
    };
    let all = rows().flat_map(widths).max().unwrap_or(0);
    let now = rows().map(|row| width(&value_name(row, &row.value))).max().unwrap_or(0);
    let after = rows().map(|row| width(&tail(row))).max().unwrap_or(0);
    let room = w.checked_sub(4 + after).filter(|&room| room >= now)?;
    Some(all.min(room))
}

/// Draws the list of the values of `row` joined to its value at `at`, inside `within`: the
/// list's border on the setting's row holds the value, an arrow down where the left arrow
/// was and none on the right, its name and what the file writes over those of the choices.
/// Each choice takes clicks.
fn draw_menu(
    buf: &mut Buffer,
    row: &Row,
    cursor: usize,
    at: (u16, u16),
    within: Rect,
) -> Vec<(Rect, Spot)> {
    let values = row.values();
    let texts: Vec<(String, String)> =
        values.iter().map(|value| (value_name(row, value), written(value))).collect();
    let items: Vec<(&str, &str)> =
        texts.iter().map(|(name, written)| (name.as_str(), written.as_str())).collect();
    let (name, file) = (value_name(row, &row.value), written(&row.value));
    let mut list = Dropdown::new(&items).cursor(cursor).head(&name, &file, at.1);
    if let Some(chosen) = values.iter().position(|value| *value == row.value) {
        list = list.chosen(chosen);
    }
    // The list's arrow goes where the left arrow was.
    let area = list.area(Rect::new(at.0, at.1, 1, 1), within);
    let inside = Rect::new(area.x + 1, area.y, area.width.saturating_sub(2), 1);
    let spots = (area.y..area.bottom())
        .filter_map(|y| {
            let choice = list.item_at(area, Position::new(inside.x, y))?;
            Some((Rect { y, ..inside }, Spot::Choice(choice)))
        })
        .collect();
    list.render(area, buf);
    spots
}

/// `s` in `w` columns, its end left out when it is longer.
fn cut(s: &str, w: usize) -> String {
    if width(s) <= w {
        return fit(s, w);
    }
    fit(&format!("{}…", take_width(s, w.saturating_sub(1))), w)
}

/// What is to be known of the setting besides its value: its default when the file has
/// another, that it waits for the next start, and what is above the file for this run.
fn notes(row: &Row) -> (String, Style) {
    let mut parts = Vec::new();
    if let Some((above, value)) = &row.above {
        let value = value_name(row, value);
        parts.push(match above {
            Above::Variable(variable) => {
                tr!("settings-above-variable", variable = variable.as_str(), value = value)
            }
            Above::CommandLine => tr!("settings-above-command-line", value = value),
        });
    }
    if row.in_file && row.value != row.default {
        parts.push(tr!("settings-default", value = value_name(row, &row.default)));
    }
    if !row.live {
        parts.push(tr!("settings-restart"));
    }
    let style = if row.above.is_some() {
        Style::new().fg(Color::Yellow)
    } else {
        Style::new().add_modifier(Modifier::DIM)
    };
    (parts.join(" · "), style)
}

pub fn draw(buf: &mut Buffer, screen: Rect, app: &App, page: &SettingsPage) -> Drawn {
    let width_all = screen.width.saturating_sub(4).clamp(20, 110).min(screen.width);
    let height = screen.height.saturating_sub(2).max(3).min(screen.height);
    let rect = centered(screen, width_all, height);
    let mut title = tr!("settings-title");
    if let Some(path) = app.settings_file() {
        let room = usize::from(width_all).saturating_sub(width(&title) + 9);
        let path = path.display().to_string();
        if room >= 8 {
            title = format!("{title} · {}", ellipsize_middle(&shown(&path), room));
        }
    }
    let inner = frame(buf, rect, &title);
    let inner_w = usize::from(inner.width.saturating_sub(2));
    let foot = if inner.height > FOOT + 2 { FOOT } else { 0 };
    let list = Rect { height: inner.height - foot, ..inner };

    let all = lines(&page.rows);
    let rows = usize::from(list.height);
    let at = all.iter().position(|l| *l == Line::Row(page.cursor)).unwrap_or(0);
    let offset = visible_offset(0, at, rows, all.len());
    let names: Vec<String> = page.rows.iter().map(|r| message(&r.name_id())).collect();
    let name_w = names.iter().map(|n| width(n)).max().unwrap_or(0).min(NAME);
    // The names the file uses stand beside the friendly ones while there is room.
    let key_w = page.rows.iter().map(|r| width(r.name())).max().unwrap_or(0);
    let key_w = if inner_w >= 2 + name_w + 2 + key_w + GAP + VALUE { key_w } else { 0 };
    let left = list.x + 1;
    let key_x = left + u16::try_from(2 + name_w + 2).unwrap_or(0);
    let value_x =
        if key_w > 0 { key_x + u16::try_from(key_w + GAP).unwrap_or(0) } else { key_x + 2 };
    let value_w = usize::from((list.x + list.width).saturating_sub(value_x + 1));
    let choice_w = choice_width(&page.rows, value_w);
    let (mut cursor, mut menu, mut spots) = (None, None, Vec::new());
    for (i, line) in all.iter().enumerate().skip(offset).take(rows) {
        let y = list.y + u16::try_from(i - offset).unwrap_or(0);
        match line {
            Line::Blank => {}
            Line::Section(name) => {
                let bold = Style::new().add_modifier(Modifier::BOLD);
                let heading = message(&crate::settings::section_id(name));
                let (end, _) = buf.set_stringn(left, y, heading, inner_w, bold);
                let room = inner_w.saturating_sub(usize::from(end - left));
                let dim = Style::new().add_modifier(Modifier::DIM);
                buf.set_stringn(end, y, format!("  [{name}]"), room, dim);
            }
            Line::Row(index) => {
                let Some(row) = page.rows.get(*index) else { continue };
                let under_cursor = *index == page.cursor;
                let edit = page.edit.as_ref().filter(|_| under_cursor);
                let style = if under_cursor && edit.is_none() && page.menu.is_none() {
                    Style::new().add_modifier(Modifier::REVERSED)
                } else {
                    Style::new()
                };
                let dim = style.add_modifier(Modifier::DIM);
                let name = names.get(*index).map_or("", String::as_str);
                let name = format!("  {}", cut(name, name_w));
                buf.set_stringn(left, y, fit(&name, inner_w), inner_w, style);
                if key_w > 0 {
                    buf.set_stringn(key_x, y, row.name(), key_w, dim);
                }
                if let Some((text, at)) = edit {
                    // Where the value stood: in the middle of the cells between the
                    // columns of the arrows. A text too long for them takes the cells up to
                    // the end, and scrolls in those.
                    let cells = width(text) + 1;
                    let field_w = choice_w.unwrap_or(0).max(cells).min(value_w.saturating_sub(2));
                    let before = field_w.saturating_sub(cells.saturating_sub(1)) / 2;
                    let underline = Style::new().add_modifier(Modifier::UNDERLINED);
                    let all = Rect::new(value_x + 2, y, u16::try_from(field_w).unwrap_or(0), 1);
                    buf.set_style(all, underline);
                    let before = u16::try_from(before).unwrap_or(0);
                    let field = Rect { x: all.x + before, width: all.width - before, ..all };
                    let p = Field::new(text, *at).style(underline).render(field, buf);
                    cursor = Some((p.x, p.y));
                    continue;
                }
                if under_cursor && page.menu.is_some() {
                    menu = Some((row, (value_x, y)));
                } else {
                    let at = (value_x, y);
                    spots.extend(draw_value(buf, (*index, row), at, (value_w, choice_w), style));
                }
            }
        }
    }

    if foot > 0 {
        let under = Rect { y: list.y + list.height, height: foot, ..inner };
        draw_foot(buf, under, page);
    }
    // Over the lines under the list too.
    if let Some(((row, at), choice)) = menu.zip(page.menu) {
        spots.extend(draw_menu(buf, row, choice, at, inner));
    }
    Drawn { rect: Some(rect), list: Some((list, offset)), cursor, spots, ..Drawn::default() }
}

/// The lines under the list: what the cursor's setting is, its notes, and the keys.
fn draw_foot(buf: &mut Buffer, inner: Rect, page: &SettingsPage) {
    let dim = Style::new().add_modifier(Modifier::DIM);
    let (left, top) = (inner.x + 1, inner.y);
    let inner_w = usize::from(inner.width.saturating_sub(2));
    buf.set_stringn(inner.x, top, "─".repeat(usize::from(inner.width)), inner.width.into(), dim);
    if let Some(row) = page.rows.get(page.cursor) {
        let what = format!("{}: {}", row.key, message(&row.description_id()));
        for (line, y) in wrap(&what, inner_w).iter().zip(top + 1..top + 3) {
            buf.set_stringn(left, y, line, inner_w, Style::new());
        }
        let (notes, style) = notes(row);
        buf.set_stringn(left, top + 3, ellipsize_middle(&notes, inner_w), inner_w, style);
    }
    let keys = if page.edit.is_some() {
        tr!("settings-keys-typing")
    } else if page.menu.is_some() {
        tr!("settings-keys-choosing")
    } else {
        tr!("settings-keys")
    };
    buf.set_stringn(left, top + 4, keys, inner_w, dim);
}
