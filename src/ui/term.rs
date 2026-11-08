//! The terminal under the panes: its shell's screen in a window across the full width.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};

use super::panes::SPINNER;
use crate::app::{App, Focus, TermState};
use crate::keymap::Action;

/// The terminal's share of the panes' room, and what the panes keep at least.
const SHARE: (u16, u16) = (2, 5);
const MIN: u16 = 6;
const PANES_MIN: u16 = 8;

/// The terminal's height, border included, out of the panes' `room`; 0 when there is
/// not enough room for it.
pub fn height(room: u16) -> u16 {
    let h = (room * SHARE.0 / SHARE.1).max(MIN).min(room.saturating_sub(PANES_MIN));
    if h < 3 { 0 } else { h }
}

/// Draws the terminal's window over `area`. Returns its screen's area, and where the text
/// cursor goes while the terminal has the keys.
pub fn draw(buf: &mut Buffer, area: Rect, app: &App) -> (Rect, Option<(u16, u16)>) {
    let Some(term) = &app.terminal else { return (Rect::default(), None) };
    let focused = app.focus == Focus::Terminal && app.dialog.is_none();
    let border = if focused { super::FOCUSED } else { Style::new() };
    let name = tr!("window-terminal");
    let note = match &term.state {
        TermState::Running(s) => s.dir().shown(),
        TermState::Opening(_) => String::new(),
    };
    let inner = super::window(
        buf,
        area,
        super::named(app, Action::Terminal, &name).note(&note).border_style(border),
    );
    if inner.is_empty() {
        return (inner, None);
    }
    let spinner = SPINNER[app.spinner % SPINNER.len()];
    let dim = Style::new().add_modifier(Modifier::DIM);
    let session = match &term.state {
        TermState::Running(s) if s.shown() => s,
        TermState::Running(_) => {
            let text = tr!("terminal-starting", spinner = spinner);
            buf.set_stringn(inner.x + 1, inner.y, text, usize::from(inner.width - 1), dim);
            return (inner, None);
        }
        TermState::Opening(_) => {
            let text = tr!("terminal-connecting", spinner = spinner);
            buf.set_stringn(inner.x + 1, inner.y, text, usize::from(inner.width - 1), dim);
            return (inner, None);
        }
    };
    let screen = session.screen();
    let (rows, cols) = screen.size();
    for row in 0..rows.get().min(inner.height) {
        for col in 0..cols.get().min(inner.width) {
            let Some(cell) = screen.cell(row, col) else { continue };
            if cell.is_wide_continuation() {
                continue;
            }
            let at = &mut buf[(inner.x + col, inner.y + row)];
            at.set_symbol(if cell.has_contents() { cell.contents() } else { " " });
            at.set_style(style(cell));
        }
    }
    let (row, col) = screen.cursor_position();
    let cursor = (focused && session.scroll == 0 && !screen.hide_cursor())
        .then(|| (inner.x + col, inner.y + row))
        .filter(|&(x, y)| x < inner.x + inner.width && y < inner.y + inner.height);
    (inner, cursor)
}

fn style(cell: &vt100::Cell) -> Style {
    let mut style = Style::new().fg(color(cell.fgcolor())).bg(color(cell.bgcolor()));
    for (on, m) in [
        (cell.bold(), Modifier::BOLD),
        (cell.dim(), Modifier::DIM),
        (cell.italic(), Modifier::ITALIC),
        (cell.underline(), Modifier::UNDERLINED),
        (cell.inverse(), Modifier::REVERSED),
    ] {
        if on {
            style = style.add_modifier(m);
        }
    }
    style
}

fn color(c: vt100::Color) -> Color {
    match c {
        vt100::Color::Default => Color::Reset,
        vt100::Color::Idx(i) => Color::Indexed(i),
        vt100::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}
