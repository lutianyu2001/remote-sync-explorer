//! Drawing. `draw` renders the whole screen from `App` and returns the `HitMap` that
//! mouse handling uses for the next events.

mod dialogs;
pub(crate) mod panes;
mod queue;
mod term;
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

/// The key hints, in their order on the row. A row too narrow for all of them leaves out
/// the last of [`HINT_PRIORITY`] first.
const HINTS: [(Action, fn() -> String); 9] = [
    (Action::Rename, || tr!("hint-rename")),
    (Action::Copy, || tr!("hint-copy")),
    (Action::Move, || tr!("hint-move")),
    (Action::Mkdir, || tr!("hint-mkdir")),
    (Action::Delete, || tr!("hint-delete")),
    (Action::Remote, || tr!("hint-remote")),
    (Action::Filter, || tr!("hint-filter")),
    (Action::Help, || tr!("hint-help")),
    (Action::Quit, || tr!("hint-quit")),
];

/// The hints from the one kept longest to the one left out first: help, where all the
/// others are, last of all.
const HINT_PRIORITY: [Action; 9] = [
    Action::Help,
    Action::Copy,
    Action::Move,
    Action::Rename,
    Action::Delete,
    Action::Remote,
    Action::Filter,
    Action::Mkdir,
    Action::Quit,
];

/// Draws the frame; returns the hit map, and where to put the hidden cursor ([`park_cursor`]),
/// `None` while the text cursor is shown (in a text field, or in the terminal).
pub fn draw(frame: &mut Frame, app: &App) -> (HitMap, Option<Position>) {
    let area = frame.area();
    let (map, cursor) = draw_buffer(frame.buffer_mut(), area, app);
    if let Some((x, y)) = cursor {
        frame.set_cursor_position(Position::new(x, y));
        return (map, None);
    }
    let at = park(&map, area);
    (map, Some(at))
}

/// Puts the hidden cursor at `at`, the message line's free space, while no text is typed:
/// an input method shows what it is composing (a key pressed with a Chinese one on) where
/// the cursor is, which would otherwise be wherever the last frame's drawing ended, over
/// the key hints or an entry. The message line is there for it, as vim's command line is.
///
/// # Errors
/// Fails when the terminal cannot be written to.
pub fn park_cursor<B: ratatui::backend::Backend>(
    terminal: &mut ratatui::Terminal<B>,
    at: Option<Position>,
) -> Result<(), B::Error> {
    match at {
        Some(at) => terminal.set_cursor_position(at),
        None => Ok(()),
    }
}

/// Where the hidden cursor goes: after the message on the bottom row, or at its start.
fn park(map: &HitMap, area: Rect) -> Position {
    let y = area.y + area.height.saturating_sub(1);
    let end = area.x + area.width.saturating_sub(1);
    Position::new(map.message_end.min(end).max(area.x), y)
}

/// Draws into a buffer; returns the hit map and where the text cursor should be.
pub fn draw_buffer(buf: &mut Buffer, area: Rect, app: &App) -> (HitMap, Option<(u16, u16)>) {
    let mut map = HitMap::default();
    let overwrite = app.queue.overwrite;
    if area.width < 10 || area.height < 5 + u16::from(overwrite) {
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

    // Two panes side by side (address bar over entries), the transfer window under them, then
    // the key hints, and last the message line: messages, and room for an input method.
    let message_y = area.y + area.height - 1;
    let hints_y = message_y - 1;
    let mut queue_h = queue::height(app, area.height).min(area.height.saturating_sub(4));
    if queue_h < 3 {
        queue_h = 0;
    }
    let queue_y = hints_y - queue_h;
    let mut panes_h = queue_y - area.y;
    // The terminal takes the bottom of the panes' room.
    let term_h = if app.terminal.is_some() { term::height(panes_h) } else { 0 };
    panes_h -= term_h;

    let left_w = area.width / 2;
    let left = Rect::new(area.x, area.y, left_w, panes_h);
    let right = Rect::new(area.x + left_w, area.y, area.width - left_w, panes_h);
    map.panes[0] = panes::draw(buf, left, app, 0);
    map.panes[1] = panes::draw(buf, right, app, 1);
    let mut cursor = None;
    if term_h > 0 {
        map.terminal = Rect::new(area.x, area.y + panes_h, area.width, term_h);
        let (screen, at) = term::draw(buf, map.terminal, app);
        map.terminal_screen = screen;
        cursor = at;
    }

    map.queue = Rect::new(area.x, queue_y, area.width, queue_h);
    if queue_h > 0 {
        map.queue_rows = queue::draw(buf, map.queue, app);
    }
    map.hints = draw_hints(buf, Rect::new(area.x, hints_y, area.width, 1), app);
    map.message_end = draw_message(buf, Rect::new(area.x, message_y, area.width, 1), app);

    // A dialog puts the cursor where it is typed in, or hides it.
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
            // Above the key hints and the message line, which stay readable.
            let above = Rect { height: area.height - 2, ..area };
            let drawn = dialogs::draw(buf, above, app, dialog);
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

/// The key hints, and on the right the engine of copies between the panes (`rsync`),
/// which the hints make room for.
fn draw_hints(buf: &mut Buffer, area: Rect, app: &App) -> Vec<(Rect, Action)> {
    let mut x = area.x + 1;
    let mut end = area.x + area.width;
    if let Some(engine) = app.pane_engine() {
        // In reverse video, a box of its own that stands out from the hints.
        let name = format!(" {} ", engine.name());
        let w = u16::try_from(width(&name)).unwrap_or(u16::MAX);
        if w < area.width / 2 {
            end -= w;
            let style = Style::new().add_modifier(Modifier::REVERSED | Modifier::BOLD);
            buf.set_string(end, area.y, &name, style);
            end = end.saturating_sub(1);
        }
    }
    let hints: Vec<(Action, &str, String, u16)> = HINTS
        .iter()
        .filter_map(|(action, label)| {
            let key = app.keymap.label(*action);
            let label = label();
            let w = u16::try_from(width(key) + 1 + width(&label)).unwrap_or(u16::MAX);
            (!key.is_empty()).then_some((*action, key, label, w))
        })
        .collect();
    // Those that fit, by priority, each with the gap of two after it.
    let room = end.saturating_sub(x);
    let mut used = 0u16;
    let mut kept = Vec::new();
    for action in HINT_PRIORITY {
        if let Some(&(_, _, _, w)) = hints.iter().find(|h| h.0 == action) {
            let more = if kept.is_empty() { w } else { w.saturating_add(2) };
            if used.saturating_add(more) <= room {
                used += more;
                kept.push(action);
            }
        }
    }
    let mut out = Vec::new();
    for (action, key, label, w) in hints.iter().filter(|h| kept.contains(&h.0)) {
        buf.set_string(x, area.y, key, Style::new().add_modifier(Modifier::BOLD));
        buf.set_string(x + u16::try_from(width(key)).unwrap_or(0) + 1, area.y, label, Style::new());
        out.push((Rect::new(x, area.y, *w, 1), *action));
        x += w + 2;
    }
    out
}

/// The message line: the last message (`Docs already exists`); returns the column after it.
fn draw_message(buf: &mut Buffer, area: Rect, app: &App) -> u16 {
    let Some(status) = &app.status else { return area.x };
    let w = usize::from(area.width).saturating_sub(2);
    let s = text::ellipsize_middle(&text::shown(status), w);
    let (end, _) = buf.set_stringn(area.x + 1, area.y, &s, w, Style::new().fg(Color::Yellow));
    end + 1
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

/// The filter being typed, on the active pane's own line under its entries.
fn draw_filter(buf: &mut Buffer, map: &HitMap, app: &App) -> Option<(u16, u16)> {
    // The pane's own line, or its last entry in a pane too short for that line.
    let area = &map.panes[app.active];
    let list = if area.footer.height > 0 { area.footer } else { area.list };
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
