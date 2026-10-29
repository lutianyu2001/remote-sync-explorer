//! Snapshot tests of whole screens at 80×24 and 160×30.

use crossterm::event::KeyCode;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use serde_json::Value;

use crate::app::Dialog;
use crate::config::UiConfig;
use crate::event::Entry;
use crate::jobs::{JobState, OpKind, Transfer};
use crate::location::Location;
use crate::rclone::api::{Stats, Transferring};
use crate::testutil::{Harness, dir, fake_fs, file};

fn harness(w: u16, h: u16, left: &str, left_entries: Vec<Value>) -> Harness {
    let (backend, _) = fake_fs(
        &[
            (left, left_entries),
            (
                "nas:backup/2026",
                vec![dir("photos"), file("report-final-v2.pdf", 2_411_724), file("notes.md", 4198)],
            ),
        ],
        &[],
    );
    let loc = Location::parse(left, std::path::Path::new("/"), std::path::Path::new("/"))
        .unwrap_or_else(|e| panic!("{e}"));
    Harness::with(backend, loc, Location::new("nas:", "backup/2026"), UiConfig::default(), w, h)
}

fn projects() -> Vec<Value> {
    vec![
        dir("rext"),
        file("report-final-v3.pdf", 2_516_582),
        file("huge.bin", 1_610_612_736),
        file("very-long-file-name-that-needs-shortening.tar.gz", 12_582_912),
    ]
}

fn with_jobs(h: &mut Harness) {
    let src = Location::new("/", "home/sky/projects");
    let dst = Location::new("nas:", "backup/2026");
    // Chosen to overwrite, so the first starts without a check of its target.
    let t = |name: &str| Transfer {
        kind: OpKind::Copy,
        src_dir: src.clone(),
        name: name.to_owned(),
        is_dir: false,
        dst_dir: Some(dst.clone()),
        size: None,
        overwrite: true,
    };
    h.app.queue.jobs.clear();
    // Build the queue state directly; the fake backend would finish jobs at once.
    let mut q = crate::jobs::Queue::new(1);
    q.push(vec![t("huge.bin"), t("report-final-v3.pdf")]);
    q.jobs[0].stats = Stats {
        bytes: 773_094_113,
        total_bytes: 1_610_612_736,
        speed: 54_525_952.0,
        eta: Some(16.0),
        transferring: vec![Transferring {
            name: "huge.bin".into(),
            bytes: 773_094_113,
            size: 1_610_612_736,
        }],
        ..Stats::default()
    };
    h.app.queue = q;
    h.app.panes[0].marks.insert(Entry::file("report-final-v3.pdf", 0).mark());
    h.app.panes[0].marks.insert(Entry::file("huge.bin", 0).mark());
}

#[test]
fn main_screen_80x24() {
    let mut h = harness(80, 24, "/home/sky/projects", projects());
    with_jobs(&mut h);
    insta::assert_snapshot!(h.render());
}

#[test]
fn pane_names_follow_a_swap() {
    let mut h = harness(80, 24, "/home/sky/projects", projects());
    h.press_mod(KeyCode::Char('u'), crossterm::event::KeyModifiers::CONTROL);
    let screen = h.render().to_owned();
    let row = screen.lines().nth(3).unwrap_or_default();
    let (remote, local) = (row.find("[2] Remote"), row.find("[4] Local"));
    assert!(remote.is_some() && local.is_some(), "{screen}");
}

#[test]
fn main_screen_in_chinese() {
    crate::i18n::set_for_this_thread(crate::config::Language::ZhCn);
    let mut h = harness(80, 24, "/home/sky/projects", projects());
    with_jobs(&mut h);
    let screen = h.render().to_owned();
    assert!(screen.contains("[1] 地址") && screen.contains("[5] 传输"), "{screen}");
    insta::assert_snapshot!(screen);
}

#[test]
fn delete_confirmation_and_help_in_chinese() {
    crate::i18n::set_for_this_thread(crate::config::Language::ZhCn);
    let mut h = harness(80, 24, "/home/sky/projects", projects());
    h.app.panes[0].marks.insert(Entry::dir("rext").mark());
    h.app.panes[0].marks.insert(Entry::file("huge.bin", 0).mark());
    h.press(KeyCode::F(8));
    insta::assert_snapshot!("delete_in_chinese", h.render());
    h.press(KeyCode::Esc);
    h.press(KeyCode::Char('?'));
    insta::assert_snapshot!("help_in_chinese", h.render());
}

#[test]
fn overwrite_mode_row_in_chinese() {
    crate::i18n::set_for_this_thread(crate::config::Language::ZhCn);
    let mut h = harness(80, 24, "/home/sky/projects", projects());
    h.press(KeyCode::Char('O'));
    let screen = h.render().to_owned();
    let top = screen.lines().next().unwrap_or_default();
    assert_eq!(top, " 覆写模式  已有的文件会被直接覆盖，不再询问；按 O 关闭", "{screen}");
    // The panes move down a row.
    assert!(screen.lines().nth(1).is_some_and(|l| l.starts_with("╭──────╮")), "{screen}");
}

#[test]
fn main_screen_160x30_shows_times() {
    let mut h = harness(160, 30, "/home/sky/projects", projects());
    with_jobs(&mut h);
    let screen = h.render().to_owned();
    assert!(screen.contains("2026-10-20 08:15"), "{screen}");
    insta::assert_snapshot!(screen);
}

#[test]
fn narrow_panes_hide_the_time_column() {
    let mut h = harness(80, 24, "/home/sky/projects", projects());
    assert!(!h.render().contains("2026-10-20"));
    let mut wide = harness(100, 24, "/home/sky/projects", projects());
    assert!(wide.render().contains("2026-10-20 08:15"));
}

#[test]
fn long_path_keeps_name_column_and_ellipsizes_title() {
    let deep = "/home/sky/a-really-long-directory-name/another-quite-long-directory/photos/2026";
    let mut h = harness(80, 24, deep, projects());
    let screen = h.render().to_owned();
    let bar = screen.lines().nth(1).unwrap_or_default();
    assert!(bar.starts_with("│ Local ││ « 2026 "), "{bar}");
    // The name column still shows names, not just sizes.
    assert!(screen.contains("report-final-v3.pdf"), "{screen}");
    insta::assert_snapshot!(screen);
}

#[test]
fn empty_queue_shows_idle() {
    let mut h = harness(80, 24, "/home/sky/projects", projects());
    let screen = h.render().to_owned();
    let lines: Vec<&str> = screen.lines().collect();
    let n = lines.len();
    assert!(lines[n - 4].starts_with("╭─") && lines[n - 4].contains("[5] Transfers"));
    assert!(lines[n - 3].starts_with("│ idle "), "{screen}");
    assert!(lines[n - 2].starts_with("╰─"), "{screen}");
    insta::assert_snapshot!(screen);
}

#[test]
fn focused_queue_expands_with_current_file() {
    let mut h = harness(80, 24, "/home/sky/projects", projects());
    with_jobs(&mut h);
    h.app.focus = crate::app::Focus::Queue;
    let screen = h.render().to_owned();
    assert!(screen.contains("↳ huge.bin"), "{screen}");
    insta::assert_snapshot!(screen);
}

#[test]
fn failed_and_done_jobs() {
    let mut h = harness(80, 24, "/home/sky/projects", projects());
    with_jobs(&mut h);
    h.app.queue.jobs[0].state = JobState::Done;
    h.app.queue.jobs[1].state = JobState::Failed("permission denied".into());
    let screen = h.render().to_owned();
    assert!(screen.contains("failed: permission denied"), "{screen}");
    assert!(screen.contains("1 done · 1 failed"), "{screen}");
}

#[test]
fn delete_confirmation_dialog() {
    let mut h = harness(80, 24, "/home/sky/projects", projects());
    h.app.panes[0].marks.insert(Entry::dir("rext").mark());
    h.app.panes[0].marks.insert(Entry::file("huge.bin", 0).mark());
    h.press(KeyCode::F(8));
    assert!(matches!(h.app.dialog, Some(Dialog::Confirm { focus: 1, .. })));
    insta::assert_snapshot!(h.render());
}

#[test]
fn long_remote_filter_scrolls_inside_the_picker() {
    let mut h = harness(80, 24, "/home/sky/projects", projects());
    h.press(KeyCode::Char('g'));
    h.type_text(&"x".repeat(60));
    let area = Rect::new(0, 0, 80, 24);
    let (map, cursor) = crate::ui::draw_buffer(&mut Buffer::empty(area), area, &h.app);
    let (picker, (x, y)) = (map.dialog.unwrap(), cursor.unwrap());
    assert!(x > picker.x && x + 1 < picker.right() && y == picker.y + 1, "{x},{y} in {picker:?}");
    assert!(h.render().contains("│ <xxxxxxxxxx"), "{}", h.screen);
}

#[test]
fn wide_file_names_line_up() {
    let mut h = harness(
        80,
        24,
        "/home/sky/projects",
        vec![file("报告-最终版本-第三稿-很长的名字.pdf", 1), file("ascii.txt", 2)],
    );
    let screen = h.render().to_owned();
    let sizes: Vec<usize> = screen
        .lines()
        .filter(|l| l.contains(".pdf") || l.contains("ascii.txt"))
        .map(|l| crate::ui::text::width(l.split_once('│').map_or(l, |(left, _)| left)))
        .collect();
    assert!(sizes.len() >= 2, "{screen}");
    assert!(sizes.iter().all(|w| *w == sizes[0]), "size column misaligned:\n{screen}");
}

#[test]
fn tiny_terminal_does_not_panic() {
    for (w, hgt) in [(1, 1), (9, 3), (12, 6), (30, 8), (40, 10)] {
        let mut h = harness(w, hgt, "/home/sky/projects", projects());
        with_jobs(&mut h);
        h.press(KeyCode::F(8));
        h.render();
        h.app.queue.overwrite = true;
        h.render();
    }
}
