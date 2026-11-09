//! Confirmation, conflict, input, remote picker, drop menu, quit, help and error log.

use lazyui_ratatui::Field;
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, BorderType, Clear, Widget};

use super::panes::wrap;
use super::text::{ellipsize_middle, ellipsize_name, fit, format_time, human_size, shown, width};
use crate::app::{App, Dialog, InputPurpose, Plan, job_label};
use crate::event::{Entry, JobId};
use crate::jobs::{JobState, OpKind, same_name};
use crate::keymap::Action;

/// What a drawn dialog occupies.
#[derive(Default)]
pub struct Drawn {
    pub rect: Option<Rect>,
    pub buttons: Vec<Rect>,
    pub list: Option<(Rect, usize)>,
    pub cursor: Option<(u16, u16)>,
    pub scroll_max: usize,
}

const MAX_LISTED: usize = 5;

pub fn draw(buf: &mut Buffer, screen: Rect, app: &App, dialog: &Dialog) -> Drawn {
    match dialog {
        Dialog::Confirm { plan, focus } => {
            let (title, lines) = confirm_text(plan, app.queue.overwrite);
            boxed(buf, screen, &title, &lines, &dialog.buttons(), Some(*focus))
        }
        Dialog::Conflict { plan, conflicts, index, focus, .. } => {
            let existing = &conflicts[*index];
            let dst = plan.dst_dir.as_ref().map(ToString::to_string).unwrap_or_default();
            let new = plan.items.iter().find(|i| same_name(&i.name, &existing.name));
            let lines = conflict_text(app, existing, new, &dst, (*index, conflicts.len()));
            boxed(buf, screen, &tr!("conflict-title"), &lines, &dialog.buttons(), Some(*focus))
        }
        Dialog::JobConflict { conflicts, index, focus } => {
            let lines = job_conflict_text(app, conflicts, *index);
            boxed(buf, screen, &tr!("conflict-title"), &lines, &dialog.buttons(), Some(*focus))
        }
        Dialog::Quit { jobs, focus } => {
            let lines = interrupted_text(app, &tr!("quit-note"), jobs);
            boxed(buf, screen, &tr!("quit-title"), &lines, &dialog.buttons(), Some(*focus))
        }
        Dialog::Leave { jobs, focus, .. } => {
            let lines = interrupted_text(app, &tr!("leave-note"), jobs);
            boxed(buf, screen, &tr!("leave-title"), &lines, &dialog.buttons(), Some(*focus))
        }
        Dialog::Input { purpose, text, cursor } => input(buf, screen, app, purpose, text, *cursor),
        Dialog::Drop { plan, focus, at } => drop_menu(buf, screen, plan, *focus, *at),
        Dialog::Remotes { filter, cursor } => remotes(buf, screen, app, filter, *cursor),
        Dialog::Help { scroll } => {
            let lines = help_text(app);
            scrolled(buf, screen, &tr!("help-title"), &lines, *scroll)
        }
        Dialog::Errors { scroll } => {
            let lines: Vec<(String, Style)> = if app.errors.is_empty() {
                vec![plain(tr!("errors-none"))]
            } else {
                app.errors
                    .iter()
                    .map(|e| {
                        (format!("{}  {}", e.time, shown(&e.message)), Style::new().fg(Color::Red))
                    })
                    .collect()
            };
            scrolled(buf, screen, &tr!("errors-title"), &lines, *scroll)
        }
        Dialog::SshConfirm { question, focus } => {
            let lines: Vec<_> = question.lines().map(|l| plain(l.to_owned())).collect();
            boxed(buf, screen, &tr!("ssh-title"), &lines, &dialog.buttons(), Some(*focus))
        }
        Dialog::Filter => Drawn::default(),
    }
}

/// `intro`, then each of `jobs` as it is now: the file it would send again, or that it
/// has finished and nothing is lost.
fn interrupted_text(app: &App, intro: &str, jobs: &[JobId]) -> Vec<(String, Style)> {
    let mut lines = vec![plain(intro.to_owned())];
    for job in jobs.iter().filter_map(|id| app.queue.jobs.iter().find(|j| j.id == *id)) {
        let label = job_label(job);
        if job.state.is_finished() || job.state == JobState::Queued {
            let line = tr!("interrupted-done", job = label);
            lines.push((format!("  {line}"), Style::new().add_modifier(Modifier::DIM)));
            continue;
        }
        lines.push(plain(format!("  {}", tr!("interrupted-running", job = label))));
        for t in &job.stats.transferring {
            let (done, size) = (human_size(t.bytes), human_size(t.size));
            let file = shown(&t.name).into_owned();
            lines.push(note(&format!(
                "    {}",
                tr!("interrupted-file", file = file, done = done, size = size)
            )));
        }
    }
    lines
}

fn plain(s: String) -> (String, Style) {
    (s, Style::new())
}

fn note(s: &str) -> (String, Style) {
    (s.to_owned(), Style::new().fg(Color::Yellow))
}

fn count(n: usize) -> String {
    tr!("items", count = n)
}

fn summary(plan: &Plan) -> String {
    let mut parts = Vec::new();
    let files = plan.items.len() - plan.dir_count();
    if files > 0 {
        parts.push(tr!("summary-files", size = human_size(plan.file_bytes()), count = files));
    }
    let dirs = plan.dir_count();
    if dirs > 0 {
        parts.push(tr!("summary-dirs", count = dirs));
    }
    let details = parts.join(&tr!("list-separator"));
    tr!("summary", items = count(plan.items.len()), details = details)
}

fn names(plan: &Plan) -> Vec<(String, Style)> {
    let mut out: Vec<(String, Style)> = plan
        .items
        .iter()
        .take(MAX_LISTED)
        .map(|e| plain(format!("  {}{}", shown(&e.name), if e.is_dir { "/" } else { "" })))
        .collect();
    if plan.items.len() > MAX_LISTED {
        out.push(plain(format!("  {}", tr!("and-more", count = plan.items.len() - MAX_LISTED))));
    }
    out
}

/// `overwrite`: overwrite mode is on, which a copy or move says.
fn confirm_text(plan: &Plan, overwrite: bool) -> (String, Vec<(String, Style)>) {
    let mut lines = Vec::new();
    let title = plan.kind.title();
    lines.push(plain(format!("{title} {}", summary(plan))));
    match &plan.dst_dir {
        Some(dst) => {
            lines.push(plain(tr!("confirm-from", dir = shown(&plan.src_dir.to_string()))));
            lines.push(plain(tr!("confirm-to", dir = shown(&dst.to_string()))));
        }
        None => lines.push(plain(tr!("confirm-in", dir = shown(&plan.src_dir.to_string())))),
    }
    lines.push(plain(String::new()));
    lines.extend(names(plan));
    let has_dirs = plan.dir_count() > 0;
    match plan.kind {
        OpKind::Copy | OpKind::Move if has_dirs => {
            lines.push(plain(String::new()));
            lines.push(note(&tr!("note-merge")));
        }
        _ => {}
    }
    if plan.kind == OpKind::Move && plan.crosses_remotes() {
        lines.push(plain(String::new()));
        lines.push(note(&tr!("note-cross-remote-move")));
    }
    if overwrite && plan.dst_dir.is_some() {
        lines.push(plain(String::new()));
        lines.push((
            tr!("note-overwrite-mode"),
            Style::new().fg(Color::Red).add_modifier(Modifier::BOLD),
        ));
    }
    if plan.kind == OpKind::Delete {
        lines.push(plain(String::new()));
        if has_dirs {
            lines.push(note(&tr!("note-delete-dirs")));
        }
        lines.push(note(&tr!("note-no-undo")));
    }
    (tr!("confirm-title", op = title), lines)
}

fn describe(app: &App, e: &Entry) -> String {
    let size = if e.is_dir { tr!("entry-dir") } else { human_size(e.size) };
    match &e.mtime {
        Some(t) => tr!("entry-modified", size = size, time = format_time(t, app.display_tz)),
        None => size,
    }
}

/// `existing` in `dst`, against `new`, the entry that would replace it; then the
/// conflict's number (from 0) out of `count`.
fn conflict_text(
    app: &App,
    existing: &Entry,
    new: Option<&Entry>,
    dst: &str,
    (index, count): (usize, usize),
) -> Vec<(String, Style)> {
    let (name, dst) = (shown(&existing.name).into_owned(), shown(dst).into_owned());
    let mut lines = vec![
        plain(tr!("conflict-exists", name = name, dir = dst)),
        plain(String::new()),
        plain(tr!("conflict-existing", entry = describe(app, existing))),
    ];
    if let Some(new) = new {
        lines.push(plain(tr!("conflict-new", entry = describe(app, new))));
        if new.is_dir && existing.is_dir {
            lines.push(plain(String::new()));
            lines.push(note(&tr!("note-dirs-merged")));
        }
    }
    lines.push(plain(String::new()));
    lines.push((
        tr!("conflict-progress", index = index + 1, count = count),
        Style::new().add_modifier(Modifier::DIM),
    ));
    lines
}

/// A conflict found as a queued job was about to start: which job, then as for an
/// operation being queued.
fn job_conflict_text(
    app: &App,
    conflicts: &[(JobId, Entry)],
    index: usize,
) -> Vec<(String, Style)> {
    let (id, existing) = &conflicts[index];
    let Some(job) = app.queue.jobs.iter().find(|j| j.id == *id) else { return Vec::new() };
    let t = &job.transfer;
    let new =
        Entry { name: t.name.clone(), is_dir: t.is_dir, size: t.size.unwrap_or(-1), mtime: None };
    let dst = t.dst_dir.as_ref().map(ToString::to_string).unwrap_or_default();
    let mut lines = vec![plain(tr!("conflict-job", job = job_label(job)))];
    lines.extend(conflict_text(app, existing, Some(&new), &dst, (index, conflicts.len())));
    // What Cancel leaves, above the conflict's number.
    let dim = Style::new().add_modifier(Modifier::DIM);
    let note = tr!("conflict-job-note", count = conflicts.len() - index);
    lines.insert(lines.len() - 1, (note, dim));
    lines
}

fn help_text(app: &App) -> Vec<(String, Style)> {
    let bold = Style::new().add_modifier(Modifier::BOLD);
    let key = |action| app.keymap.label(action);
    let mut lines = vec![
        (tr!("help-start"), bold),
        plain(format!("  {}", tr!("help-start-panes", switch = key(Action::SwitchPane)))),
        plain(format!(
            "  {}",
            tr!("help-start-open", goto = key(Action::Goto), remote = key(Action::Remote))
        )),
        plain(format!(
            "  {}",
            tr!(
                "help-start-copy",
                mark = key(Action::Mark),
                copy = key(Action::Copy),
                move = key(Action::Move)
            )
        )),
        plain(format!("  {}", tr!("help-start-more"))),
        plain(String::new()),
        (tr!("help-keys"), bold),
    ];
    for (keys, what) in app.keymap.help() {
        lines.push(plain(format!("  {}  {what}", fit(&keys, 22))));
    }
    lines.push(plain(String::new()));
    lines.push((tr!("help-mouse"), bold));
    let modifier = match app.ui.multi_select_modifier {
        crate::config::MultiSelectModifier::Ctrl => "Ctrl",
        crate::config::MultiSelectModifier::Alt => "Alt",
    };
    for (what, effect) in [
        (tr!("mouse-click"), tr!("mouse-click-does")),
        (tr!("mouse-blank"), tr!("mouse-blank-does")),
        (tr!("mouse-mark", modifier = modifier), tr!("mouse-mark-does")),
        (tr!("mouse-double"), tr!("mouse-double-does")),
        (tr!("mouse-wheel"), tr!("mouse-wheel-does")),
        (tr!("mouse-address"), tr!("mouse-address-does")),
        (tr!("mouse-button"), tr!("mouse-button-does")),
        (tr!("mouse-drag"), tr!("mouse-drag-does")),
        (tr!("mouse-cancel"), tr!("mouse-cancel-does")),
    ] {
        lines.push(plain(format!("  {}  {effect}", fit(&what, 22))));
    }
    lines.push(plain(String::new()));
    lines.push(plain(tr!("help-shift")));
    lines
}

/// A centered dialog with wrapped text and a row of buttons.
fn boxed(
    buf: &mut Buffer,
    screen: Rect,
    title: &str,
    lines: &[(String, Style)],
    buttons: &[String],
    focus: Option<usize>,
) -> Drawn {
    let w = screen.width.saturating_sub(4).clamp(20, 76).min(screen.width);
    let inner_w = usize::from(w.saturating_sub(4));
    let wrapped: Vec<(String, Style)> = lines
        .iter()
        .flat_map(|(l, s)| wrap(l, inner_w).into_iter().map(move |x| (x, *s)))
        .collect();
    let want = u16::try_from(wrapped.len() + 4).unwrap_or(u16::MAX);
    let h = want.min(screen.height);
    let rect = centered(screen, w, h);
    let inner = frame(buf, rect, title);
    let body_h = usize::from(inner.height.saturating_sub(2));
    for (i, (line, style)) in wrapped.iter().take(body_h).enumerate() {
        buf.set_stringn(
            inner.x + 1,
            inner.y + u16::try_from(i).unwrap_or(0),
            line,
            inner_w,
            *style,
        );
    }
    let buttons = button_row(buf, inner, buttons, focus);
    Drawn { rect: Some(rect), buttons, ..Drawn::default() }
}

fn button_row(buf: &mut Buffer, inner: Rect, labels: &[String], focus: Option<usize>) -> Vec<Rect> {
    if inner.height == 0 {
        return Vec::new();
    }
    let y = inner.y + inner.height - 1;
    let texts: Vec<String> = labels.iter().map(|l| format!("[ {l} ]")).collect();
    let total: usize =
        texts.iter().map(|t| width(t)).sum::<usize>() + 2 * texts.len().saturating_sub(1);
    let mut x =
        inner.x + u16::try_from(usize::from(inner.width).saturating_sub(total) / 2).unwrap_or(0);
    let mut rects = Vec::new();
    for (i, t) in texts.iter().enumerate() {
        let tw = u16::try_from(width(t)).unwrap_or(0);
        let style = if Some(i) == focus {
            Style::new().add_modifier(Modifier::REVERSED | Modifier::BOLD)
        } else {
            Style::new()
        };
        buf.set_stringn(
            x,
            y,
            t,
            usize::from(inner.x + inner.width).saturating_sub(usize::from(x)),
            style,
        );
        rects.push(Rect::new(x, y, tw, 1).intersection(inner));
        x = x.saturating_add(tw + 2);
    }
    rects
}

fn centered(screen: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(screen.width);
    let h = h.min(screen.height);
    Rect::new(screen.x + (screen.width - w) / 2, screen.y + (screen.height - h) / 2, w, h)
}

/// Clears `rect`, draws a border with a title, and returns the inside.
fn frame(buf: &mut Buffer, rect: Rect, title: &str) -> Rect {
    Clear.render(rect, buf);
    let block = Block::bordered().border_type(BorderType::Rounded).title(format!(" {title} "));
    let inner = block.inner(rect);
    block.render(rect, buf);
    inner
}

fn input(
    buf: &mut Buffer,
    screen: Rect,
    app: &App,
    purpose: &InputPurpose,
    text: &str,
    cursor: usize,
) -> Drawn {
    let here = app.panes[app.active].loc.to_string();
    let (title, prompt) = match purpose {
        InputPurpose::Mkdir => (tr!("mkdir-title"), tr!("mkdir-prompt", dir = here)),
        InputPurpose::Rename(old) => {
            (tr!("rename-title"), tr!("rename-prompt", name = shown(&old.name)))
        }
        // Edited under the pane's address bar instead (`panes::draw_address_edit`).
        InputPurpose::Goto { .. } => (String::new(), String::new()),
        InputPurpose::Login(a) => (
            tr!("login-title"),
            tr!(
                "login-prompt",
                address = format!("{}://{}", a.endpoint.scheme.name(), a.endpoint.authority())
            ),
        ),
        InputPurpose::SshPrompt { question, .. } => {
            (tr!("ssh-title"), shown(question).into_owned())
        }
    };
    let remember = match purpose {
        InputPurpose::SshPrompt { remember, .. } => *remember,
        _ => None,
    };
    let masked;
    let text = if let InputPurpose::Login(_) | InputPurpose::SshPrompt { .. } = purpose {
        masked = "•".repeat(text.chars().count());
        masked.as_str()
    } else {
        text
    };
    let w = screen.width.saturating_sub(4).clamp(20, 76).min(screen.width);
    let rect = centered(screen, w, if remember.is_some() { 7 } else { 6 });
    let inner = frame(buf, rect, &title);
    let inner_w = usize::from(inner.width.saturating_sub(2));
    buf.set_stringn(
        inner.x + 1,
        inner.y,
        ellipsize_middle(&prompt, inner_w),
        inner_w,
        Style::new(),
    );
    let field = Rect::new(inner.x + 1, inner.y + 1, u16::try_from(inner_w).unwrap_or(0), 1);
    let underline = Style::new().add_modifier(Modifier::UNDERLINED);
    let mut at = Position::new(field.x, field.y);
    if inner.height > 1 {
        at = Field::new(text, cursor).style(underline).render(field, buf);
    }
    let mut buttons = button_row(buf, inner, &[tr!("button-ok"), tr!("button-cancel")], None);
    if let Some(on) = remember
        && inner.height > 3
    {
        let label = format!("[{}] {}", if on { "x" } else { " " }, tr!("ssh-remember"));
        let hint = format!("  {}", tr!("ssh-remember-key"));
        let (x, y) = (inner.x + 1, inner.y + 2);
        buf.set_stringn(x, y, &label, inner_w, Style::new());
        let label_w = width(&label).min(inner_w);
        buf.set_stringn(
            x + u16::try_from(label_w).unwrap_or(0),
            y,
            &hint,
            inner_w - label_w,
            Style::new().add_modifier(Modifier::DIM),
        );
        // Clicking the box or its label toggles it (`app::REMEMBER`).
        buttons.push(Rect::new(x, y, u16::try_from(label_w).unwrap_or(0), 1));
    }
    if let InputPurpose::SshPrompt { notice: Some(notice), .. } = purpose {
        notice_window(buf, screen, rect, notice);
    }
    Drawn { rect: Some(rect), buttons, cursor: Some((at.x, at.y)), ..Drawn::default() }
}

/// A window right under `above` saying why the user is asked, in as many lines as fit
/// on the screen.
fn notice_window(buf: &mut Buffer, screen: Rect, above: Rect, text: &str) {
    let top = above.y + above.height;
    let room = (screen.y + screen.height).saturating_sub(top);
    let lines = wrap(&shown(text), usize::from(above.width.saturating_sub(4)));
    let h = u16::try_from(lines.len() + 2).unwrap_or(u16::MAX).min(room);
    if h < 3 {
        return;
    }
    let rect = Rect::new(above.x, top, above.width, h);
    let inner = frame(buf, rect, &tr!("ssh-notice-title"));
    let inner_w = usize::from(inner.width.saturating_sub(2));
    for (y, line) in (inner.y..inner.y + inner.height).zip(&lines) {
        buf.set_stringn(inner.x + 1, y, line, inner_w, Style::new().fg(Color::Yellow));
    }
}

fn drop_menu(buf: &mut Buffer, screen: Rect, plan: &Plan, focus: usize, at: (u16, u16)) -> Drawn {
    let dst = plan.dst_dir.as_ref().map(ToString::to_string).unwrap_or_default();
    let title = format!("{} → {}", count(plan.items.len()), ellipsize_name(&dst, 30));
    let labels = crate::app::drop_buttons();
    let menu_w = u16::try_from(width(&title).max(14) + 4).unwrap_or(20).min(screen.width);
    let menu_h = 5u16.min(screen.height);
    let x = at.0.min(screen.x + screen.width - menu_w);
    let y = at.1.min(screen.y + screen.height - menu_h);
    let rect = Rect::new(x, y, menu_w, menu_h);
    let inner = frame(buf, rect, &title);
    let mut buttons = Vec::new();
    for (i, label) in labels.iter().enumerate() {
        let row = inner.y + u16::try_from(i).unwrap_or(0);
        if row >= inner.y + inner.height {
            break;
        }
        let r = Rect::new(inner.x, row, inner.width, 1);
        let style = if i == focus {
            Style::new().add_modifier(Modifier::REVERSED | Modifier::BOLD)
        } else {
            Style::new()
        };
        buf.set_stringn(
            r.x,
            r.y,
            fit(&format!(" {label}"), usize::from(r.width)),
            usize::from(r.width),
            style,
        );
        buttons.push(r);
    }
    Drawn { rect: Some(rect), buttons, ..Drawn::default() }
}

fn remotes(buf: &mut Buffer, screen: Rect, app: &App, filter: &str, cursor: usize) -> Drawn {
    let choices = app.remote_choices(filter);
    let w = 50u16.min(screen.width);
    // With no remote in rclone.conf, how to get one, and how to open a server without.
    let note = if app.remotes.is_empty() {
        let how = tr!("remotes-how", key = app.keymap.label(Action::Goto));
        super::panes::wrap(&how, usize::from(w.saturating_sub(4)))
    } else {
        Vec::new()
    };
    let note_h = if note.is_empty() { 0 } else { note.len() + 1 };
    let want = u16::try_from(choices.len() + 4 + note_h).unwrap_or(u16::MAX).max(6);
    let h = want.min(screen.height.saturating_sub(2).max(6)).min(screen.height);
    let rect = centered(screen, w, h);
    let inner = frame(buf, rect, &tr!("remotes-title"));
    let inner_w = usize::from(inner.width.saturating_sub(2));
    // The cursor stays at the end of the filter; a filter too long for the line scrolls.
    let text = tr!("remotes-filter", filter = filter);
    let field = Rect::new(inner.x + 1, inner.y, u16::try_from(inner_w).unwrap_or(0), 1);
    let at = Field::new(&text, text.chars().count()).render(field, buf);
    let note_h = u16::try_from(note_h).unwrap_or(0).min(inner.height.saturating_sub(3));
    let list =
        Rect::new(inner.x, inner.y + 2, inner.width, inner.height.saturating_sub(2 + note_h));
    let dim = Style::new().add_modifier(Modifier::DIM);
    for (i, line) in note.iter().take(usize::from(note_h.saturating_sub(1))).enumerate() {
        let y = list.y + list.height + 1 + u16::try_from(i).unwrap_or(0);
        buf.set_stringn(list.x + 1, y, line, inner_w, dim);
    }
    let rows = usize::from(list.height);
    let cursor = cursor.min(choices.len().saturating_sub(1));
    let offset = crate::app::visible_offset(0, cursor, rows, choices.len());
    for (i, (label, _)) in choices.iter().enumerate().skip(offset).take(rows) {
        let y = list.y + u16::try_from(i - offset).unwrap_or(0);
        let style =
            if i == cursor { Style::new().add_modifier(Modifier::REVERSED) } else { Style::new() };
        buf.set_stringn(list.x + 1, y, fit(label, inner_w), inner_w, style);
    }
    if choices.is_empty() {
        buf.set_stringn(
            list.x + 1,
            list.y,
            tr!("remotes-none"),
            inner_w,
            Style::new().add_modifier(Modifier::DIM),
        );
    }
    Drawn {
        rect: Some(rect),
        list: Some((list, offset)),
        cursor: Some((at.x, at.y)),
        ..Drawn::default()
    }
}

fn scrolled(
    buf: &mut Buffer,
    screen: Rect,
    title: &str,
    lines: &[(String, Style)],
    scroll: usize,
) -> Drawn {
    let w = screen.width.saturating_sub(4).clamp(20, 90).min(screen.width);
    let h = screen.height.saturating_sub(2).max(3).min(screen.height);
    let rect = centered(screen, w, h);
    let inner = frame(buf, rect, title);
    let inner_w = usize::from(inner.width.saturating_sub(2));
    let wrapped: Vec<(String, Style)> = lines
        .iter()
        .flat_map(|(l, s)| wrap(l, inner_w).into_iter().map(move |x| (x, *s)))
        .collect();
    let rows = usize::from(inner.height);
    let scroll_max = wrapped.len().saturating_sub(rows);
    let scroll = scroll.min(scroll_max);
    for (i, (line, style)) in wrapped.iter().skip(scroll).take(rows).enumerate() {
        buf.set_stringn(
            inner.x + 1,
            inner.y + u16::try_from(i).unwrap_or(0),
            line,
            inner_w,
            *style,
        );
    }
    Drawn { rect: Some(rect), scroll_max, ..Drawn::default() }
}
