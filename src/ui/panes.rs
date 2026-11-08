//! The two file panes.

use std::fmt::Write as _;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};

use lazyui_ratatui::{Field, Window};
use unicode_segmentation::UnicodeSegmentation as _;

use super::text::{
    ellipsize_middle, ellipsize_name, fit, fit_right, format_time, human_size, shown, width,
};
use crate::app::{App, Dialog, Drag, Focus, InputPurpose, Pane, PaneStatus, Row, SortKey};
use crate::event::PaneId;
use crate::hitmap::PaneArea;
use crate::keymap::Action;
use crate::location::Location;

const NAME_MIN: usize = 12;
const SIZE_W: usize = 9;
const TIME_W: usize = 16;
/// Panes narrower than this hide the modification time.
const TIME_MIN_PANE: u16 = 48;
/// The button box right of the address bar: reload, or go once the address is edited.
const REFRESH: &str = "↻";
const GO: &str = ">";
const BUTTON_W: u16 = 5;
/// The address window keeps at least this width before the boxes beside it are dropped.
const BAR_MIN: u16 = 20;

/// The arrow after the header of the column the pane is sorted by.
const ASCENDING: &str = "▲";
const DESCENDING: &str = "▼";

/// The protocol box's text.
const TAG: Style = Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD);

pub const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Draws a pane: a row of the protocol box, the address bar (`[1] Address`) and its
/// button box, the window Go to edits the address in (while it does), then the entries
/// (`[2] Local`, with `[2 marked]` in the bottom right corner) under a row of column
/// headers, which sort the entries. Windows are labelled in
/// their top right corner; the boxes beside the address bar have no key, as they can't
/// be focused.
pub fn draw(buf: &mut Buffer, area: Rect, app: &App, id: PaneId) -> PaneArea {
    let pane = &app.panes[id];
    let edit =
        match &app.dialog {
            Some(Dialog::Input {
                purpose: purpose @ InputPurpose::Goto { scheme }, text, ..
            }) if id == app.active => Some((purpose, *scheme, text.as_str())),
            _ => None,
        };
    let editing = edit.is_some();
    let focused = id == app.active && app.focus == Focus::Panes && !editing;
    let drop = match &app.drag {
        Drag::Dragging { target: Some(t), .. } if t.pane == id => Some(t.valid),
        _ => None,
    };
    let border = match drop {
        Some(valid) => Style::new().fg(if valid { Color::Green } else { Color::Red }),
        None if focused => super::FOCUSED,
        None => Style::new(),
    };
    let (bar_action, action) = if id == 0 {
        (Action::FocusLeftAddress, Action::FocusLeft)
    } else {
        (Action::FocusRightAddress, Action::FocusRight)
    };
    // Named after what the pane shows, so the names follow a swap or a Go to.
    let name = if pane.loc.is_local() { tr!("window-local") } else { tr!("window-remote") };
    let top = Rect { height: 3.min(area.height), ..area };
    // While editing, the entries move down to make room for the edit window.
    let edit_h = if editing && area.height >= 9 { 3 } else { 0 };
    let edit_box = Rect { y: area.y + top.height, height: edit_h, ..area };
    let used = top.height + edit_h;
    let frame = Rect { y: area.y + used, height: area.height - used, ..area };
    let tag = match edit {
        Some((purpose, _, text)) => input_tag(app, purpose, text),
        None => tag_text(app, &pane.loc),
    };
    let (tag_box, bar, button) = address_row(top, tag.as_deref());

    let selected = editing || (id == app.active && app.focus == Focus::Address);
    let bar_border = if selected { super::FOCUSED } else { Style::new() };
    if let Some(tag) = &tag
        && !tag_box.is_empty()
    {
        let inner = super::window(buf, tag_box, Window::new().border_style(bar_border));
        buf.set_stringn(inner.x + 1, inner.y, tag, width(tag), TAG);
    }
    let address = tr!("window-address");
    let title =
        super::window(buf, bar, super::named(app, bar_action, &address).border_style(bar_border));
    if !button.is_empty() {
        let go = edit.is_some_and(|(_, scheme, text)| app.address_edited(scheme, text));
        let inner = super::window(buf, button, Window::new().border_style(bar_border));
        let bold = Style::new().add_modifier(Modifier::BOLD);
        buf.set_stringn(inner.x + 1, inner.y, if go { GO } else { REFRESH }, 1, bold);
    }
    let edit = if edit_h > 0 {
        let dim = Style::new().add_modifier(Modifier::DIM);
        let revising = tr!("address-revising");
        buf.set_stringn(title.x + 1, title.y, revising, usize::from(title.width), dim);
        let inner = super::window(buf, edit_box, Window::new().border_style(super::FOCUSED));
        // An arrow from the address bar to the window that edits it.
        lazyui_ratatui::arrow_down(buf, title.x + 1, bar, edit_box, super::FOCUSED);
        inner
    } else {
        // Too short for an edit window: Go to edits the address bar itself.
        draw_title(buf, title, app, pane);
        title
    };
    let marked =
        if pane.marks.is_empty() { String::new() } else { tr!("marked", count = pane.marks.len()) };
    let list = super::window(
        buf,
        frame,
        super::named(app, action, &name).note(&marked).border_style(border),
    );

    let columns = Columns::new(list.width);
    // The headers take the first row when one is left for the entries.
    let (columns_at, list) = if list.height >= 2 {
        let header = Rect { height: 1, ..list };
        let rects = draw_header(buf, header, pane, columns);
        (rects, Rect { y: list.y + 1, height: list.height - 1, ..list })
    } else {
        (<[Rect; 3]>::default(), list)
    };
    let offset = pane.first_shown(usize::from(list.height));
    draw_list(buf, list, app, id, offset, columns);

    PaneArea {
        frame,
        bar,
        title,
        button,
        edit,
        list,
        columns: columns_at,
        offset,
        rows: pane.view.len(),
    }
}

/// The widths of the list's columns: a mark, the name, and the size and time when the
/// pane is wide enough for them.
#[derive(Clone, Copy)]
struct Columns {
    name: usize,
    size: bool,
    time: bool,
}

impl Columns {
    fn new(width: u16) -> Self {
        let w = usize::from(width);
        let time = width >= TIME_MIN_PANE;
        let size = w >= 1 + NAME_MIN + 1 + SIZE_W;
        let fixed = 1 + if size { 1 + SIZE_W } else { 0 } + if time { 1 + TIME_W } else { 0 };
        Self { name: w.saturating_sub(fixed).max(1), size, time }
    }
}

/// The column headers, the one the pane is sorted by in bold with an arrow for the
/// order. Returns where the name, size and time headers are, to click on.
fn draw_header(buf: &mut Buffer, area: Rect, pane: &Pane, columns: Columns) -> [Rect; 3] {
    let mut rects = <[Rect; 3]>::default();
    let mut x = area.x;
    let end = area.x + area.width;
    for (i, key) in [SortKey::Name, SortKey::Size, SortKey::Time].into_iter().enumerate() {
        let (w, shown) = match key {
            // The mark column goes with the name.
            SortKey::Name => (1 + columns.name, true),
            SortKey::Size => (1 + SIZE_W, columns.size),
            SortKey::Time => (1 + TIME_W, columns.time),
        };
        if !shown {
            continue;
        }
        let sorted = pane.sort == key;
        let mut label = match key {
            SortKey::Name => tr!("column-name"),
            SortKey::Size => tr!("column-size"),
            SortKey::Time => tr!("column-time"),
        };
        if sorted {
            label.push(' ');
            label.push_str(if pane.ascending { ASCENDING } else { DESCENDING });
        }
        let inner = w - 1;
        let text = match key {
            SortKey::Size => fit_right(&label, inner),
            _ => fit(&label, inner),
        };
        let style = if sorted {
            Style::new().add_modifier(Modifier::BOLD)
        } else {
            Style::new().add_modifier(Modifier::DIM)
        };
        let w16 = u16::try_from(w).unwrap_or(u16::MAX).min(end.saturating_sub(x));
        buf.set_stringn(x + 1, area.y, text, inner, style);
        rects[i] = Rect { x, width: w16, ..area };
        x = x.saturating_add(w16);
    }
    rects
}

/// Splits the address row into the protocol box, the address window and the button box.
/// A box is left out (an empty rect) when the address window would get too narrow.
fn address_row(row: Rect, tag: Option<&str>) -> (Rect, Rect, Rect) {
    let mut bar = row;
    let mut button = Rect::default();
    let mut tag_box = Rect::default();
    if bar.width >= BUTTON_W + BAR_MIN {
        bar.width -= BUTTON_W;
        button = Rect { x: bar.x + bar.width, width: BUTTON_W, ..row };
    }
    if let Some(tag) = tag {
        let w = u16::try_from(width(tag) + 4).unwrap_or(u16::MAX);
        if bar.width >= w + BAR_MIN {
            tag_box = Rect { width: w, ..row };
            bar.x += w;
            bar.width -= w;
        }
    }
    (tag_box, bar, button)
}

/// What the protocol box shows for a location: `sftp://`, or `Local` on the local disk.
fn tag_text(app: &App, loc: &Location) -> Option<String> {
    match app.protocol(loc) {
        Some(p) => Some(format!("{p}://")),
        None => loc.is_local().then(|| tr!("tag-local")),
    }
}

/// What the protocol box shows while Go to edits the address: the tag, or the protocol
/// the typed address leads to.
fn input_tag(app: &App, purpose: &InputPurpose, text: &str) -> Option<String> {
    match app.input_location(purpose, text) {
        Some(loc) => tag_text(app, &loc),
        None => app.input_protocol(purpose, text).map(|p| format!("{p}://")),
    }
}

/// The address bar: clickable path, filter and sort notes.
fn draw_title(buf: &mut Buffer, area: Rect, app: &App, pane: &Pane) {
    let style = Style::new().add_modifier(Modifier::BOLD);

    let mut extra = String::new();
    if !pane.filter.is_empty() {
        let _ = write!(extra, " [/{}]", pane.filter);
    }
    if pane.status == PaneStatus::Loading {
        extra.push(' ');
        extra.push_str(SPINNER[app.spinner % SPINNER.len()]);
    }
    // One cell of padding on each side.
    let w = usize::from(area.width).saturating_sub(1);
    let extra = super::text::take_width(&extra, w.saturating_sub(NAME_MIN)).to_owned();
    let room = w.saturating_sub(width(&extra) + 1);

    let base = area.x + 1;
    buf.set_stringn(base, area.y, breadcrumb(&pane.loc, room), room, style);
    let ex = area.x + u16::try_from(w.saturating_sub(width(&extra))).unwrap_or(0);
    buf.set_stringn(ex, area.y, &extra, width(&extra), style);
}

/// The active pane's address bar while Go to edits it. The protocol box and the button
/// box beside it follow the text. Returns the cursor position.
pub fn draw_address_edit(
    buf: &mut Buffer,
    title: Rect,
    purpose: &InputPurpose,
    text: &str,
    cursor: usize,
) -> (u16, u16) {
    let InputPurpose::Goto { scheme } = purpose else { return (title.x, title.y) };
    buf.set_stringn(
        title.x,
        title.y,
        " ".repeat(usize::from(title.width)),
        usize::MAX,
        Style::new(),
    );
    let field = Rect::new(title.x + 1, title.y, title.width.saturating_sub(2), 1);
    let underline = Style::new().add_modifier(Modifier::UNDERLINED);
    let at = Field::new(text, cursor).style(underline).render(field, buf);
    if text.is_empty() && scheme.is_none() {
        let hint = tr!("address-hint");
        let dim = underline.add_modifier(Modifier::DIM);
        buf.set_stringn(field.x, field.y, &hint, usize::from(field.width), dim);
    }
    (at.x, at.y)
}

/// The title text for a location within `room` cells. Too long: only the current
/// directory after `«`, as Windows Explorer's address bar does.
pub fn breadcrumb(loc: &Location, room: usize) -> String {
    let segs = loc.segments();
    let mut text = String::new();
    for (i, (label, _)) in segs.iter().enumerate() {
        // Roots end in `/` or `:` (`/`, `nas:`), or not (`box`, `me@box/~`).
        if i > 0 && !text.ends_with(['/', ':']) {
            text.push('/');
        }
        text.push_str(&shown(label));
    }
    if width(&text) <= room {
        return text;
    }
    match segs.last() {
        Some((label, _)) if segs.len() > 1 && room > 3 => {
            format!("« {}", ellipsize_middle(&shown(label), room - 2))
        }
        _ => ellipsize_middle(&loc.address().1, room),
    }
}

fn draw_list(buf: &mut Buffer, area: Rect, app: &App, id: PaneId, offset: usize, cols: Columns) {
    let pane = &app.panes[id];
    let w = usize::from(area.width);
    if area.height == 0 || w == 0 {
        return;
    }
    let dim = Style::new().add_modifier(Modifier::DIM);
    match &pane.status {
        PaneStatus::Error(msg) => {
            let style = Style::new().fg(Color::Red);
            for (i, line) in
                wrap(&tr!("pane-error", error = shown(msg)), w.saturating_sub(2)).iter().enumerate()
            {
                if i >= usize::from(area.height) {
                    break;
                }
                buf.set_stringn(
                    area.x + 1,
                    area.y + u16::try_from(i).unwrap_or(0),
                    line,
                    w - 1,
                    style,
                );
            }
            return;
        }
        PaneStatus::Loading if pane.view.is_empty() => {
            let s = tr!("pane-loading", spinner = SPINNER[app.spinner % SPINNER.len()]);
            buf.set_stringn(area.x + 1, area.y, s, w - 1, dim);
            return;
        }
        _ => {}
    }

    let Columns { name: name_w, size: show_size, time: show_time } = cols;

    let focused = id == app.active && app.focus == Focus::Panes && app.dialog.is_none();
    let hover_row = match &app.drag {
        Drag::Dragging { target: Some(t), .. } if t.pane == id => t.row.map(|r| (r, t.valid)),
        _ => None,
    };
    let dragging = matches!(app.drag, Drag::Dragging { .. });

    for (screen_row, row) in (offset..pane.view.len()).take(usize::from(area.height)).enumerate() {
        let y = area.y + u16::try_from(screen_row).unwrap_or(0);
        let (name, size, time, is_dir) = match pane.view[row] {
            Row::Parent => ("..".to_owned(), String::new(), String::new(), true),
            Row::Entry(i) => {
                let e = &pane.entries[i];
                let name = shown(&e.name);
                let name = if e.is_dir { format!("{name}/") } else { name.into_owned() };
                let size = if e.is_dir { tr!("dir-size") } else { human_size(e.size) };
                let time =
                    e.mtime.as_ref().map(|t| format_time(t, app.display_tz)).unwrap_or_default();
                (name, size, time, e.is_dir)
            }
        };
        let marked = pane.is_marked(row);
        let mut line = String::with_capacity(w);
        line.push(if marked { '*' } else { ' ' });
        line.push_str(&fit(&ellipsize_name(&name, name_w), name_w));
        if show_size {
            line.push(' ');
            line.push_str(&fit_right(&size, SIZE_W));
        }
        if show_time {
            line.push(' ');
            line.push_str(&fit(&time, TIME_W));
        }

        let mut style = Style::new();
        if is_dir {
            style = style.fg(Color::Blue).add_modifier(Modifier::BOLD);
        }
        if marked {
            style = style.fg(Color::Yellow).add_modifier(Modifier::BOLD);
        }
        if row == pane.cursor && !dragging {
            style = if focused {
                style.add_modifier(Modifier::REVERSED)
            } else {
                style.add_modifier(Modifier::UNDERLINED)
            };
        }
        if let Some((r, valid)) = hover_row
            && r == row
        {
            style = style.fg(Color::Black).bg(if valid { Color::Green } else { Color::Red });
        }
        buf.set_stringn(area.x, y, &line, w, style);
    }

    if pane.view.iter().all(|r| *r == Row::Parent) && pane.status == PaneStatus::Ready {
        let y = area.y + u16::try_from(pane.view.len()).unwrap_or(0);
        if y < area.y + area.height {
            let msg =
                if pane.entries.is_empty() { tr!("pane-empty") } else { tr!("pane-no-match") };
            buf.set_stringn(area.x + 1, y, msg, w - 1, dim);
        }
    }
}

/// Greedy word wrap to `w` cells; long words are cut.
pub fn wrap(text: &str, w: usize) -> Vec<String> {
    let w = w.max(1);
    let mut lines = Vec::new();
    for para in text.split('\n') {
        // Keep a leading indent on the first line of each paragraph.
        let body = para.trim_start_matches(' ');
        let mut line = " ".repeat((para.len() - body.len()).min(w.saturating_sub(1)));
        for word in body.split(' ') {
            let mut word = word.to_owned();
            loop {
                let sep = usize::from(!line.trim_start_matches(' ').is_empty());
                if width(&line) + sep + width(&word) <= w {
                    if sep == 1 {
                        line.push(' ');
                    }
                    line.push_str(&word);
                    break;
                }
                if !line.trim_start_matches(' ').is_empty() {
                    lines.push(std::mem::take(&mut line));
                    continue;
                }
                // A character wider than `w` goes on a line of its own, or this never ends.
                let mut head = super::text::take_width(&word, w).to_owned();
                if head.is_empty() {
                    word.graphemes(true).next().unwrap_or_default().clone_into(&mut head);
                }
                word = word.split_off(head.len());
                if word.is_empty() {
                    line = head;
                    break;
                }
                lines.push(head);
            }
        }
        lines.push(line);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn breadcrumb_joins_the_levels() {
        assert_eq!(breadcrumb(&Location::new("nas:", "backup/2026"), 80), "nas:backup/2026");
        assert_eq!(breadcrumb(&Location::new("/", "home/sky"), 80), "/home/sky");
    }

    #[test]
    fn long_breadcrumb_shows_only_the_current_directory() {
        let loc = Location::new("nas:", "a-long-dir/another-long-dir/photos/2026");
        assert_eq!(breadcrumb(&loc, 20), "« 2026");
        let loc = Location::new("/", "home/a-very-long-directory-name");
        assert_eq!(breadcrumb(&loc, 12), "« a-ver…name");
    }

    #[test]
    fn wrap_breaks_on_words_and_cuts_long_ones() {
        assert_eq!(wrap("aa bb cc", 5), ["aa bb", "cc"]);
        assert_eq!(wrap("abcdefgh", 3), ["abc", "def", "gh"]);
        assert_eq!(wrap("  ab cd", 5), ["  ab", "cd"]);
        assert_eq!(wrap("文件", 1), ["文", "件"]);
        assert_eq!(wrap("\r ab", 1), ["\r", "a", "b"], "only spaces are indent");
    }
}
