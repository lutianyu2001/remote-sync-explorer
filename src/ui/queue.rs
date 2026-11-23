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

/// Draws the transfer window, labelled (`[5] Transfers`) in its top left corner: counts
/// on the first row, then one job per row: the jobs of the endpoints
/// the panes show.
pub fn draw(buf: &mut Buffer, area: Rect, app: &App) -> Vec<(u16, usize, u16)> {
    let focused = app.focus == Focus::Queue;
    // Red while a job of the endpoints shown has failed, so that it is not missed.
    let failed = app.queue.visible().iter().any(|j| matches!(j.state, JobState::Failed(_)));
    let border = match (focused, failed) {
        (true, _) => super::FOCUSED,
        (false, true) => FAILED.add_modifier(Modifier::BOLD),
        (false, false) => Style::new(),
    };
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
    // Unfocused, from the newest job under way, waiting for a retry or failed: the jobs
    // below it are older, and those above only queued.
    let anchor = if focused {
        app.queue_cursor
    } else {
        jobs.iter().position(|j| noteworthy(&j.state)).unwrap_or(0)
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

/// Failed jobs, and the count of them.
const FAILED: Style = Style::new().fg(Color::Red);
/// Jobs waiting to be tried again.
const RETRYING: Style = Style::new().fg(Color::Yellow);

/// A job the transfer window shows first when it is not focused.
fn noteworthy(state: &JobState) -> bool {
    state.is_active() || matches!(state, JobState::Failed(_) | JobState::Retrying { .. })
}

fn draw_header(buf: &mut Buffer, area: Rect, app: &App, focused: bool) {
    let jobs = app.queue.visible();
    let count = |f: fn(&JobState) -> bool| jobs.iter().filter(|j| f(&j.state)).count();
    let done = count(|s| *s == JobState::Done);
    let failed = count(|s| matches!(s, JobState::Failed(_)));
    let retrying = count(|s| matches!(s, JobState::Retrying { .. }));
    let bold = Style::new().add_modifier(Modifier::BOLD);
    let mut parts = Vec::new();
    if jobs.is_empty() && app.queue.elsewhere() == 0 {
        parts.push((tr!("queue-idle"), bold));
    } else {
        parts.push((tr!("queue-running", count = count(JobState::is_active)), bold));
        parts.push((tr!("queue-queued", count = count(|s| *s == JobState::Queued)), bold));
        if retrying > 0 {
            parts.push((tr!("queue-retrying", count = retrying), RETRYING.patch(bold)));
        }
        if done > 0 {
            parts.push((tr!("queue-done", count = done), bold));
        }
        if failed > 0 {
            parts.push((tr!("queue-failed", count = failed), FAILED.patch(bold)));
        }
        let elsewhere = app.queue.elsewhere();
        if elsewhere > 0 {
            parts.push((tr!("queue-elsewhere", count = elsewhere), bold));
        }
    }
    // `2 running · 1 queued`, each part in its own style.
    let mut x = area.x + 1;
    let end = area.x + area.width;
    for (i, (text, style)) in parts.iter().enumerate() {
        let text = if i == 0 { text.clone() } else { format!(" · {text}") };
        x = put(buf, x, end, area.y, &text, *style);
    }
    if focused {
        put(buf, x, end, area.y, &format!("   {}", tr!("queue-keys")), bold);
    }
}

/// Writes `text` at `x` on row `y`, as much of it as fits before `end`; returns where
/// it ends.
fn put(buf: &mut Buffer, x: u16, end: u16, y: u16, text: &str, style: Style) -> u16 {
    let room = usize::from(end.saturating_sub(x));
    let text = take_width(text, room);
    buf.set_stringn(x, y, text, room, style);
    x + u16::try_from(width(text)).unwrap_or(0)
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
        JobState::Retrying { error, left, .. } => tr!(
            "job-retrying-in",
            attempt = job.attempts,
            of = app.queue.retry_policy().retries,
            seconds = left.as_secs() + u64::from(left.subsec_nanos() > 0),
            error = error.as_str()
        ),
    };
    let right_w = match job.state {
        JobState::Failed(_) | JobState::Retrying { .. } => (w / 2).min(width(&right)),
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
        JobState::Failed(_) => FAILED.add_modifier(Modifier::BOLD),
        JobState::Retrying { .. } => RETRYING,
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
