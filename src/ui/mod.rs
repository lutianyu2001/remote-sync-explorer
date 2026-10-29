//! Drawing. `draw` renders the whole screen from `App` and returns the `HitMap` that
//! mouse handling uses for the next events.

mod dialogs;
pub(crate) mod panes;
mod queue;
pub mod text;

use lazyui_ratatui::Window;
use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::Widget;

use crate::app::{App, Dialog, Drag, InputPurpose};
use crate::hitmap::HitMap;
use crate::keymap::Action;
use text::width;

const HINTS: [(Action, fn() -> String); 8] = [
    (Action::Copy, || tr!("hint-copy")),
    (Action::Move, || tr!("hint-move")),
    (Action::Mkdir, || tr!("hint-mkdir")),
    (Action::Delete, || tr!("hint-delete")),
    (Action::Remote, || tr!("hint-remote")),
    (Action::Filter, || tr!("hint-filter")),
    (Action::Help, || tr!("hint-help")),
    (Action::Quit, || tr!("hint-quit")),
];

pub fn draw(frame: &mut Frame, app: &App) -> HitMap {
    let area = frame.area();
    let (map, cursor) = draw_buffer(frame.buffer_mut(), area, app);
    if let Some((x, y)) = cursor {
        frame.set_cursor_position(Position::new(x, y));
    }
    map
}

/// Draws into a buffer; returns the hit map and where the text cursor should be.
pub fn draw_buffer(buf: &mut Buffer, area: Rect, app: &App) -> (HitMap, Option<(u16, u16)>) {
    let mut map = HitMap::default();
    let overwrite = app.queue.overwrite;
    if area.width < 10 || area.height < 4 + u16::from(overwrite) {
        buf.set_stringn(area.x, area.y, tr!("too-small"), usize::from(area.width), Style::new());
        return (map, None);
    }
    // Overwrite mode takes the top row, above everything else, dialogs included.
    let area = if overwrite {
        draw_overwrite_mode(buf, Rect { height: 1, ..area }, app);
        Rect { y: area.y + 1, height: area.height - 1, ..area }
    } else {
        area
    };

    // Two panes side by side (address bar over entries), the transfer window under them, key hints last.
    let hints_y = area.y + area.height - 1;
    let mut queue_h = queue::height(app, area.height).min(area.height.saturating_sub(4));
    if queue_h < 3 {
        queue_h = 0;
    }
    let queue_y = hints_y - queue_h;
    let panes_h = queue_y - area.y;

    let left_w = area.width / 2;
    let left = Rect::new(area.x, area.y, left_w, panes_h);
    let right = Rect::new(area.x + left_w, area.y, area.width - left_w, panes_h);
    map.panes[0] = panes::draw(buf, left, app, 0);
    map.panes[1] = panes::draw(buf, right, app, 1);

    map.queue = Rect::new(area.x, queue_y, area.width, queue_h);
    if queue_h > 0 {
        map.queue_rows = queue::draw(buf, map.queue, app);
    }
    map.hints = draw_hints(buf, Rect::new(area.x, hints_y, area.width, 1), app);

    let mut cursor = None;
    if let Some(dialog) = &app.dialog {
        if *dialog == Dialog::Filter {
            cursor = draw_filter(buf, &map, app);
        } else if let Dialog::Input {
            purpose: purpose @ InputPurpose::Goto { .. },
            text,
            cursor: at,
        } = dialog
        {
            // Go to edits the active pane's address in the window under its address bar.
            // Clicks on the boxes beside it don't cancel; its button goes.
            let p = &map.panes[app.active];
            cursor = Some(panes::draw_address_edit(buf, p.edit, purpose, text, *at));
            let bottom = p.frame.y;
            map.dialog = Some(Rect {
                x: p.frame.x,
                width: p.frame.width,
                height: bottom - p.bar.y,
                ..p.bar
            });
            map.buttons = Some(p.button).filter(|b| !b.is_empty()).into_iter().collect();
        } else {
            let drawn = dialogs::draw(buf, area, app, dialog);
            map.dialog = drawn.rect;
            map.buttons = drawn.buttons;
            map.dialog_list = drawn.list;
            map.scroll_max = drawn.scroll_max;
            cursor = drawn.cursor;
        }
    }
    draw_drag(buf, area, app);
    (map, cursor)
}

/// Border of the window that has the keyboard.
const FOCUSED: Style = Style::new().fg(Color::Green).add_modifier(Modifier::BOLD);

/// A window named `name` after the key of `action`, which focuses it: `[1] Address`.
fn named<'a>(app: &'a App, action: Action, name: &'a str) -> Window<'a> {
    Window::new().name(app.keymap.label(action), name)
}

/// Draws `window` over `area`; returns its inside.
fn window(buf: &mut Buffer, area: Rect, window: Window) -> Rect {
    let inner = window.inner(area);
    window.render(area, buf);
    inner
}

fn draw_hints(buf: &mut Buffer, area: Rect, app: &App) -> Vec<(Rect, Action)> {
    let mut x = area.x + 1;
    let end = area.x + area.width;
    let mut out = Vec::new();
    for (action, label) in HINTS {
        let label = &label();
        let key = app.keymap.label(action);
        if key.is_empty() {
            continue;
        }
        let w = u16::try_from(width(key) + 1 + width(label)).unwrap_or(u16::MAX);
        if x + w > end {
            break;
        }
        buf.set_string(x, area.y, key, Style::new().add_modifier(Modifier::BOLD));
        buf.set_string(x + u16::try_from(width(key)).unwrap_or(0) + 1, area.y, label, Style::new());
        out.push((Rect::new(x, area.y, w, 1), action));
        x += w + 2;
    }
    out
}

/// A row in white on red: overwrite mode, what it does and the key that turns it off.
fn draw_overwrite_mode(buf: &mut Buffer, area: Rect, app: &App) {
    let style = Style::new().fg(Color::White).bg(Color::Red);
    buf.set_style(area, style);
    let w = usize::from(area.width);
    let label = format!(" {} ", tr!("overwrite-mode"));
    let (x, _) = buf.set_stringn(area.x, area.y, &label, w, style.add_modifier(Modifier::BOLD));
    let key = app.keymap.label(Action::OverwriteMode);
    let note = format!(" {}", tr!("overwrite-mode-note", key = key));
    buf.set_stringn(x, area.y, &note, w.saturating_sub(usize::from(x - area.x)), style);
}

/// The filter being typed, on the active pane's last row.
fn draw_filter(buf: &mut Buffer, map: &HitMap, app: &App) -> Option<(u16, u16)> {
    let list = map.panes[app.active].list;
    if list.height == 0 {
        return None;
    }
    let y = list.y + list.height - 1;
    let text = format!("/{}", app.panes[app.active].filter);
    let style = Style::new().fg(Color::Black).bg(Color::Yellow);
    buf.set_style(Rect::new(list.x, y, list.width, 1), style);
    let shown = text::take_width_end(&text, usize::from(list.width.saturating_sub(1)));
    buf.set_stringn(list.x, y, shown, usize::from(list.width), style);
    Some((list.x + u16::try_from(width(shown)).unwrap_or(0), y))
}

/// "3 items" next to the pointer while dragging, with a no-entry sign over invalid targets.
fn draw_drag(buf: &mut Buffer, area: Rect, app: &App) {
    let Drag::Dragging { items, at, target, .. } = &app.drag else { return };
    let n = items.len();
    let valid = target.as_ref().is_some_and(|t| t.valid);
    let label = format!(" {}{} ", if valid { "" } else { "⊘ " }, tr!("items", count = n));
    let w = u16::try_from(width(&label)).unwrap_or(0);
    let x = (at.0 + 2).min((area.x + area.width).saturating_sub(w));
    let y = if at.1 + 1 < area.y + area.height { at.1 + 1 } else { at.1.saturating_sub(1) };
    let style = if valid {
        Style::new().fg(Color::Black).bg(Color::Cyan)
    } else {
        Style::new().fg(Color::White).bg(Color::Red)
    };
    buf.set_stringn(x, y, &label, usize::from(w), style);
}

#[cfg(test)]
mod tests;
