//! The transfer queue below the panes.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};

use super::panes::SPINNER;
use super::text::{
    ellipsize_middle, fit, fit_right, format_eta, human_size, shown, take_width, width,
};
use crate::app::{App, Focus, cancellable, job_label};
use crate::jobs::{Job, JobState};
use crate::keymap::Action;

const BAR_W: usize = 12;

/// Rows the transfer window needs, borders and the header row included: no jobs when
/// empty, 3 jobs normally, more while focused.
pub fn height(app: &App, screen_height: u16) -> u16 {
    let jobs = if app.focus == Focus::Queue {
        // One more row for the selected job's current file.
        let want = u16::try_from(app.queue.visible().len() + 1).unwrap_or(u16::MAX);
        want.clamp(3, (screen_height / 2).saturating_sub(3).max(3))
    } else if app.queue.visible().is_empty() {
        0
    } else {
        3
    };
    jobs + 3
}

/// Draws the transfer window, labelled (`[5] Transfers`) in its top right corner: counts
/// on the first row, then one job per row: the jobs of the endpoints
/// the panes show.
pub fn draw(buf: &mut Buffer, area: Rect, app: &App) -> Vec<(u16, usize, u16)> {
    let focused = app.focus == Focus::Queue;
    let border = if focused { super::FOCUSED } else { Style::new() };
    let name = tr!("window-transfers");
    let window = super::named(app, Action::FocusQueue, &name).border_style(border);
    let area = super::window(buf, area, window);
    if area.height == 0 {
        return Vec::new();
    }
    let w = usize::from(area.width);
    draw_header(buf, area, app, focused);

    let jobs = app.queue.visible();
    let rows = usize::from(area.height - 1);
    if rows == 0 || jobs.is_empty() {
        return Vec::new();
    }
    // While focused, the selected job gets a second line with the current file.
    let detail = focused && jobs.get(app.queue_cursor).is_some_and(|j| j.current_file().is_some());
    let job_rows = if detail { rows.saturating_sub(1).max(1) } else { rows };
    let anchor = if focused {
        app.queue_cursor
    } else {
        jobs.iter().position(|j| !j.state.is_finished()).unwrap_or(0)
    };
    let start = if focused {
        anchor.saturating_sub(job_rows - 1)
    } else {
        anchor.min(jobs.len().saturating_sub(job_rows))
    };

    let mut hits = Vec::new();
    let mut y = area.y + 1;
    for (index, &job) in jobs.iter().enumerate().skip(start).take(job_rows) {
        if y >= area.y + area.height {
            break;
        }
        let selected = focused && index == app.queue_cursor;
        let cancel_x = draw_job(buf, Rect { y, height: 1, ..area }, app, job, selected);
        hits.push((y, index, cancel_x));
        y += 1;
        if selected && detail && y < area.y + area.height {
            let t = &job.stats.transferring[0];
            let line = format!(
                "     ↳ {}  {} / {}",
                shown(&t.name),
                human_size(t.bytes),
                human_size(t.size)
            );
            buf.set_stringn(
                area.x,
                y,
                ellipsize_middle(&line, w),
                w,
                Style::new().add_modifier(Modifier::DIM),
            );
            y += 1;
        }
    }
    hits
}

fn draw_header(buf: &mut Buffer, area: Rect, app: &App, focused: bool) {
    let w = usize::from(area.width);
    let jobs = app.queue.visible();
    let count = |f: fn(&JobState) -> bool| jobs.iter().filter(|j| f(&j.state)).count();
    let done = count(|s| *s == JobState::Done);
    let failed = count(|s| matches!(s, JobState::Failed(_)));
    let mut text = String::new();
    if jobs.is_empty() && app.queue.elsewhere() == 0 {
        text.push_str(&tr!("queue-idle"));
    } else {
        let mut parts = vec![
            tr!("queue-running", count = count(JobState::is_active)),
            tr!("queue-queued", count = count(|s| *s == JobState::Queued)),
        ];
        if done > 0 {
            parts.push(tr!("queue-done", count = done));
        }
        if failed > 0 {
            parts.push(tr!("queue-failed", count = failed));
        }
        let elsewhere = app.queue.elsewhere();
        if elsewhere > 0 {
            parts.push(tr!("queue-elsewhere", count = elsewhere));
        }
        text.push_str(&parts.join(" · "));
    }
    if focused {
        text.push_str("   ");
        text.push_str(&tr!("queue-keys"));
    }
    // `2 running · 1 queued`.
    let w = w.saturating_sub(1);
    let text = take_width(&text, w).to_owned();
    buf.set_stringn(area.x + 1, area.y, &text, w, Style::new().add_modifier(Modifier::BOLD));
}

/// Draws one job row and returns the column of its cancel mark.
fn draw_job(buf: &mut Buffer, area: Rect, app: &App, job: &Job, selected: bool) -> u16 {
    let w = usize::from(area.width);
    let right = match &job.state {
        JobState::Queued => tr!("job-queued"),
        JobState::Checking => tr!("job-checking", spinner = SPINNER[app.spinner % SPINNER.len()]),
        JobState::Running | JobState::Cancelling | JobState::Pausing => running_text(app, job),
        JobState::Done => tr!("job-done"),
        JobState::Failed(e) => tr!("job-failed", error = e.as_str()),
        JobState::Cancelled => tr!("job-cancelled"),
        JobState::Skipped => tr!("job-skipped"),
    };
    let right_w = match job.state {
        JobState::Failed(_) => (w / 2).min(width(&right)),
        _ => width(&right),
    };
    let right = take_width(&right, right_w);
    let marker = if selected { " > " } else { "   " };
    let label_w = w.saturating_sub(3 + right_w + 2 + 2);
    let line = format!(
        "{marker}{}  {}{}",
        fit(&ellipsize_middle(&job_label(job), label_w), label_w),
        fit_right(right, right_w),
        if cancellable(&job.state) { " ✕" } else { "  " }
    );
    let style = match job.state {
        JobState::Failed(_) => Style::new().fg(Color::Red),
        JobState::Done | JobState::Cancelled | JobState::Skipped => {
            Style::new().add_modifier(Modifier::DIM)
        }
        _ => Style::new(),
    };
    let style = if selected { style.add_modifier(Modifier::REVERSED) } else { style };
    buf.set_stringn(area.x, area.y, &line, w, style);
    area.x + area.width.saturating_sub(1)
}

fn running_text(app: &App, job: &Job) -> String {
    if matches!(job.state, JobState::Cancelling | JobState::Pausing) {
        return tr!("job-stopping");
    }
    let Some(f) = job.fraction() else {
        return tr!("job-starting", spinner = SPINNER[app.spinner % SPINNER.len()]);
    };
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)]
    let filled = ((f * BAR_W as f64).round() as usize).min(BAR_W);
    let bar = format!("{}{}", "█".repeat(filled), "░".repeat(BAR_W - filled));
    #[allow(clippy::cast_possible_truncation)]
    let speed = format!("{}/s", human_size(job.stats.speed as i64));
    let eta = job.stats.eta.map(format_eta).unwrap_or_default();
    format!("{bar} {:>4} {:>10} {:>6}", format!("{:.0}%", f * 100.0), speed, eta)
}
