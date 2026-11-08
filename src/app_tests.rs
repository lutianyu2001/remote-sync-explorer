use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEventKind};
use ratatui::style::Color;
use serde_json::{Value, json};

use super::*;
use crate::config::{DropAction, UiConfig};
use crate::endpoint::Scheme;
use crate::jobs::Engine;
use crate::rclone::FakeBackend;
use crate::testutil::{Harness, Tree, dir, fake_fs, file};

fn home() -> Location {
    Location::new("/", "home/sky")
}

fn nas() -> Location {
    Location::new("nas:", "backup")
}

/// Left: /home/sky with two dirs, two files and a dot file. Right: nas:backup.
fn standard() -> Harness {
    let (backend, _) = fake_fs(
        &[
            (
                "/home/sky",
                vec![
                    file("b.txt", 2048),
                    dir("projects"),
                    file(".bashrc", 10),
                    dir("Docs"),
                    file("a.pdf", 3_000_000),
                ],
            ),
            ("/home/sky/projects", vec![file("main.rs", 100), dir("rse")]),
            ("/home/sky/Docs", vec![]),
            ("nas:backup", vec![file("a.pdf", 1), dir("photos")]),
            ("nas:backup/photos", vec![]),
            ("/home", vec![dir("sky")]),
            ("/", vec![dir("home")]),
            ("s3:", vec![dir("bucket")]),
        ],
        &[],
    );
    Harness::new(backend, home(), nas())
}

fn names(h: &Harness, pane: usize) -> Vec<String> {
    let p = &h.app.panes[pane];
    (0..p.view.len()).map(|r| p.entry(r).map_or("..".to_owned(), |e| e.name.clone())).collect()
}

fn cursor_name(h: &Harness, pane: usize) -> String {
    let p = &h.app.panes[pane];
    p.entry(p.cursor).map_or("..".to_owned(), |e| e.name.clone())
}

fn select(h: &mut Harness, name: &str) {
    let p = &h.app.panes[h.app.active];
    let row = (0..p.view.len())
        .find(|&r| p.entry(r).is_some_and(|e| e.name == name))
        .unwrap_or_else(|| panic!("{name} not listed"));
    h.app.panes[h.app.active].cursor = row;
    h.render();
}

fn last_call(h: &Harness, method: &str) -> Value {
    h.calls(method).last().cloned().unwrap_or_else(|| panic!("no call to {method}"))
}

// ----- browsing ------------------------------------------------------------------

#[test]
fn initial_listing_sorts_dirs_first_and_hides_dot_files() {
    let h = standard();
    assert_eq!(h.app.panes[0].status, PaneStatus::Ready);
    assert_eq!(names(&h, 0), ["..", "Docs", "projects", "a.pdf", "b.txt"]);
    assert_eq!(names(&h, 1), ["..", "photos", "a.pdf"]);
}

#[test]
fn enter_and_parent_restore_the_cursor() {
    let mut h = standard();
    select(&mut h, "projects");
    h.press(KeyCode::Enter);
    assert_eq!(h.app.panes[0].loc, home().join("projects"));
    assert_eq!(names(&h, 0), ["..", "rse", "main.rs"]);
    h.press(KeyCode::Backspace);
    assert_eq!(h.app.panes[0].loc, home());
    assert_eq!(cursor_name(&h, 0), "projects");
    // `..` with Enter also goes up.
    h.press(KeyCode::Home);
    h.press(KeyCode::Enter);
    assert_eq!(h.app.panes[0].loc, Location::new("/", "home"));
    assert_eq!(cursor_name(&h, 0), "sky");
}

#[test]
fn letter_aliases_navigate_too() {
    let mut h = standard();
    h.press(KeyCode::Char('j'));
    h.press(KeyCode::Char('j'));
    assert_eq!(cursor_name(&h, 0), "projects");
    h.press(KeyCode::Char('l'));
    assert_eq!(h.app.panes[0].loc, home().join("projects"));
    h.press(KeyCode::Char('h'));
    assert_eq!(h.app.panes[0].loc, home());
    h.press(KeyCode::End);
    assert_eq!(cursor_name(&h, 0), "b.txt");
    h.press(KeyCode::Char('k'));
    assert_eq!(cursor_name(&h, 0), "a.pdf");
}

#[test]
fn stale_listing_does_not_overwrite_a_newer_one() {
    let (backend, _) = fake_fs(
        &[
            ("/home/sky", vec![dir("slow"), dir("fast")]),
            ("/home/sky/slow", vec![file("old.txt", 1)]),
            ("/home/sky/fast", vec![file("new.txt", 1)]),
            ("nas:backup", vec![]),
        ],
        &[("/home/sky/slow", Duration::from_millis(400))],
    );
    let mut h = Harness::new(backend, home(), nas());
    select(&mut h, "slow");
    // Enter "slow", then immediately go back and into "fast" while "slow" is still listing.
    let now = Instant::now();
    h.app.handle(Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)), now);
    h.app.handle(Event::Key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE)), now);
    h.wait_until(|a| a.panes[0].status == PaneStatus::Ready);
    select(&mut h, "fast");
    h.press(KeyCode::Enter);
    std::thread::sleep(Duration::from_millis(500));
    h.settle();
    assert_eq!(h.app.panes[0].loc, home().join("fast"));
    assert_eq!(names(&h, 0), ["..", "new.txt"]);
}

#[test]
fn listing_error_is_shown_in_the_pane_and_logged() {
    let mut h = standard();
    h.press(KeyCode::Char(':'));
    h.press_mod(KeyCode::Char('u'), KeyModifiers::CONTROL);
    h.type_text("/nope");
    h.press(KeyCode::Enter);
    assert_eq!(h.app.panes[0].status, PaneStatus::Error("directory not found".into()));
    assert!(h.screen.contains("error: directory not found"), "{}", h.screen);
    assert_eq!(h.app.errors.len(), 1);
    h.press(KeyCode::Char('E'));
    assert!(h.screen.contains("listing /nope: directory not found"), "{}", h.screen);
}

/// A server could list names such as `..` or `a/../b`: they are left out, saying so. Names
/// with characters that would not show, or would reorder the text, show them as marks.
#[test]
fn names_that_lead_out_of_a_directory_are_left_out() {
    let spoof = "invoice-\u{202e}fdp.exe";
    let (backend, _) = fake_fs(
        &[
            ("/home/sky", vec![file("ok.txt", 1), file("..", 1), dir("a/../b"), file(spoof, 1)]),
            ("nas:backup", vec![]),
        ],
        &[],
    );
    let h = Harness::new(backend, home(), nas());
    assert_eq!(names(&h, 0), ["..", spoof, "ok.txt"]);
    let status = h.app.status.clone().unwrap_or_default();
    assert!(status.contains("left out 2 entries"), "{status}");
    assert!(h.screen.contains("invoice-\u{fffd}fdp.exe"), "{}", h.screen);
}

#[test]
fn ssh_errors_get_a_login_hint() {
    assert!(with_hint("ssh: exit status 255").contains("non-interactive login"));
    assert!(
        with_hint("NewFs: couldn't initialise SFTP: error receiving version packet from server: unexpected EOF")
            .contains("non-interactive login")
    );
    assert_eq!(with_hint("directory not found"), "directory not found");
}

#[test]
fn marks_toggle_and_mark_all() {
    let mut h = standard();
    select(&mut h, "a.pdf");
    h.press(KeyCode::Char(' '));
    assert_eq!(cursor_name(&h, 0), "b.txt");
    h.press(KeyCode::Insert);
    assert_eq!(h.app.panes[0].marks.len(), 2);
    let sel: Vec<_> = h.app.panes[0].selection().into_iter().map(|e| e.name).collect();
    assert_eq!(sel, ["b.txt", "a.pdf"]);
    h.press(KeyCode::Char('a'));
    assert_eq!(h.app.panes[0].marks.len(), 4);
    h.press(KeyCode::Char('a'));
    assert!(h.app.panes[0].marks.is_empty());
    // `..` cannot be marked.
    h.press(KeyCode::Home);
    h.press(KeyCode::Char(' '));
    assert!(h.app.panes[0].marks.is_empty());
}

/// S3 can list a file `x` next to a directory `x`: marking the file leaves the directory
/// out of what a delete would remove.
#[test]
fn a_file_and_a_directory_of_one_name_are_marked_apart() {
    let mut p = Pane::new(Location::new("s3:", "bucket"));
    p.entries = vec![Entry::dir("x"), Entry::file("x", 1)];
    p.rebuild(true);
    p.marks.insert(Entry::file("x", 1).mark());
    p.rebuild(true);
    assert_eq!(p.selection(), [Entry::file("x", 1)]);
    let marked: Vec<bool> = (0..p.view.len()).map(|r| p.is_marked(r)).collect();
    assert_eq!(marked, [false, false, true], "`..`, the directory, the file");
}

/// The cursor stays on the one of a file and a directory of one name it was on, across a
/// reload, and renaming renames that one.
#[test]
fn a_file_beside_a_directory_of_its_name_keeps_the_cursor() {
    let (backend, _) =
        fake_fs(&[("/home/sky", vec![dir("x"), file("x", 1)]), ("nas:backup", vec![])], &[]);
    let mut h = Harness::new(backend, home(), nas());
    h.app.panes[0].cursor = 2;
    h.render();
    h.press_mod(KeyCode::Char('r'), KeyModifiers::CONTROL);
    h.settle();
    let current = h.app.panes[0].current().cloned();
    assert!(current.is_some_and(|e| e.name == "x" && !e.is_dir), "the file, not the directory");
    h.press(KeyCode::F(2));
    h.type_text("y");
    h.press(KeyCode::Enter);
    h.wait_until(|a| !a.has_pending_requests());
    assert_eq!(last_call(&h, "operations/movefile")["dstRemote"], "xy");
}

#[test]
fn sort_filter_and_hidden_files() {
    let mut h = standard();
    h.press(KeyCode::Char('s'));
    assert_eq!(names(&h, 0), ["..", "Docs", "projects", "a.pdf", "b.txt"]);
    assert_eq!(h.app.panes[0].sort, SortKey::Size);
    h.press(KeyCode::Char('s'));
    assert_eq!(h.app.panes[0].sort, SortKey::Time);
    h.press(KeyCode::Char('s'));
    assert_eq!(h.app.panes[0].sort, SortKey::Name);

    h.press(KeyCode::Char('.'));
    assert!(names(&h, 0).contains(&".bashrc".to_owned()));
    h.press(KeyCode::Char('.'));

    h.press(KeyCode::Char('/'));
    h.type_text("PDF");
    assert_eq!(names(&h, 0), ["..", "a.pdf"]);
    h.press(KeyCode::Enter);
    assert!(h.screen.contains("[/PDF]"), "{}", h.screen);
    assert_eq!(h.app.dialog, None);
    h.press(KeyCode::Char('/'));
    h.press(KeyCode::Esc);
    assert_eq!(names(&h, 0).len(), 5);
}

#[test]
fn size_sort_puts_largest_files_first() {
    let mut h = standard();
    h.press(KeyCode::Char('s'));
    let files: Vec<_> = names(&h, 0).into_iter().skip(3).collect();
    assert_eq!(files, ["a.pdf", "b.txt"]);
    h.press(KeyCode::Char('.'));
    assert_eq!(names(&h, 0).last().map(String::as_str), Some(".bashrc"));
}

#[test]
fn goto_accepts_remote_paths() {
    let mut h = standard();
    h.press(KeyCode::Char(':'));
    assert!(matches!(&h.app.dialog, Some(Dialog::Input { text, .. }) if text == "/home/sky"));
    h.press_mod(KeyCode::Char('u'), KeyModifiers::CONTROL);
    h.type_text("s3:");
    h.press(KeyCode::Enter);
    assert_eq!(h.app.panes[0].loc, Location::new("s3:", ""));
    assert_eq!(names(&h, 0), ["bucket"]);
}

// ----- servers by address --------------------------------------------------------

fn goto(h: &mut Harness, address: &str) {
    h.press(KeyCode::Char(':'));
    h.press_mod(KeyCode::Char('u'), KeyModifiers::CONTROL);
    h.type_text(address);
    h.press(KeyCode::Enter);
}

fn goto_field(h: &Harness) -> (Option<Scheme>, String) {
    match &h.app.dialog {
        Some(Dialog::Input { purpose: InputPurpose::Goto { scheme }, text, .. }) => {
            (*scheme, text.clone())
        }
        other => panic!("not in Go to: {other:?}"),
    }
}

fn asks_password(h: &Harness) -> bool {
    matches!(&h.app.dialog, Some(Dialog::Input { purpose: InputPurpose::Login(_), .. }))
}

#[test]
fn protocol_tags_in_titles_and_go_to() {
    let (backend, _) = fake_fs(
        &[
            ("/home/sky", vec![]),
            ("nas:backup", vec![]),
            (":sftp,host=box,user=me:/srv", vec![file("x", 1)]),
        ],
        &[],
    );
    let mut h = Harness::new(backend, home(), nas());
    assert!(h.screen.contains("│ smb:// ││ nas:backup"), "{}", h.screen);
    assert!(h.screen.contains("│ Local ││ /home/sky"), "{}", h.screen);

    h.press(KeyCode::Char(':'));
    h.press_mod(KeyCode::Char('u'), KeyModifiers::CONTROL);
    h.type_text("sftp://me@box/srv");
    assert_eq!(goto_field(&h), (Some(Scheme::Sftp), "me@box/srv".to_owned()));
    h.press(KeyCode::Enter);
    assert_eq!(h.app.panes[0].loc.fs_string(), ":sftp,host=box,user=me:/srv");
    assert_eq!(names(&h, 0), ["..", "x"]);
    assert!(h.screen.contains("│ sftp:// ││ me@box/srv"), "{}", h.screen);

    // Go to shows the tag again; Backspace at the start removes it, keeping the text.
    h.press(KeyCode::Char(':'));
    assert_eq!(goto_field(&h), (Some(Scheme::Sftp), "me@box/srv".to_owned()));
    assert!(h.screen.contains("│ sftp:// ││ [Revising]"), "{}", h.screen);
    assert!(h.screen.lines().nth(4).is_some_and(|l| l.starts_with("│ me@box/srv ")));
    h.press(KeyCode::Home);
    h.press(KeyCode::Backspace);
    assert_eq!(goto_field(&h), (None, "me@box/srv".to_owned()));
    // Typing another protocol replaces the tag.
    h.press(KeyCode::Home);
    h.type_text("ftps://");
    assert_eq!(goto_field(&h), (Some(Scheme::Ftps), "me@box/srv".to_owned()));
    h.press_mod(KeyCode::Char('u'), KeyModifiers::CONTROL);
    assert_eq!(goto_field(&h), (None, String::new()));
}

#[test]
fn login_asks_once_per_server_and_again_after_a_failure() {
    let smb = ":smb,host=nas,user=me,pass=OB(pw):share";
    let (backend, _) =
        fake_fs(&[("/home/sky", vec![]), ("nas:backup", vec![]), (smb, vec![dir("d")])], &[]);
    let mut h = Harness::new(backend, home(), nas());
    goto(&mut h, "smb://me@nas/share");
    assert!(asks_password(&h));
    assert!(h.screen.contains("Password for smb://me@nas"), "{}", h.screen);
    h.type_text("pw");
    assert!(h.screen.contains("••") && !h.screen.contains("pw"), "{}", h.screen);
    h.press(KeyCode::Enter);
    assert_eq!(h.calls("core/obscure"), [json!({"clear": "pw"})]);
    assert_eq!(h.app.panes[0].loc.fs_string(), smb);
    assert_eq!(names(&h, 0), ["..", "d"]);
    assert!(h.screen.contains("│ smb:// ││ me@nas/share"), "{}", h.screen);

    // Known now: no question. This listing fails, which forgets the password.
    goto(&mut h, "smb://me@nas/other");
    assert!(h.app.dialog.is_none());
    assert_eq!(h.app.panes[0].loc.fs, ":smb,host=nas,user=me,pass=OB(pw):");
    assert!(matches!(h.app.panes[0].status, PaneStatus::Error(_)));
    goto(&mut h, "smb://me@nas/share");
    assert!(asks_password(&h));

    // A password in the address is used as it is.
    h.press(KeyCode::Esc);
    goto(&mut h, "webdav://me:s3cret@dav/files");
    assert!(h.app.dialog.is_none());
    assert_eq!(
        h.app.panes[0].loc.fs_string(),
        ":webdav,url='http://dav',user=me,pass=OB(s3cret):files"
    );
    assert!(!h.screen.contains("s3cret"), "{}", h.screen);
}

#[test]
fn address_bar_is_edited_below_it_with_one_refresh_or_go_button() {
    let mut h = standard();
    let bar_line = |h: &Harness| h.screen.lines().nth(1).unwrap_or_default().to_owned();
    let edit_line = |h: &Harness| h.screen.lines().nth(4).unwrap_or_default().to_owned();
    assert!(bar_line(&h).contains("││ ↻ ││"), "{}", h.screen);
    assert!(bar_line(&h).ends_with("││ ↻ │"), "{}", h.screen);

    // The button reloads its own pane, which becomes active.
    let listed = h.calls("operations/list").len();
    let button = h.app.hitmap.panes[1].button;
    h.click(button.x + 2, button.y + 1);
    assert_eq!(h.app.active, 1);
    assert_eq!(h.calls("operations/list").len(), listed + 1);
    assert_eq!(last_call(&h, "operations/list")["fs"], "nas:backup");

    // Clicking the address bar edits the address in a window that opens under it;
    // unchanged, the button still reloads.
    h.app.active = 0;
    let title = h.app.hitmap.panes[1].title;
    h.click(title.x + title.width - 6, title.y);
    assert_eq!(h.app.active, 1);
    assert!(matches!(&h.app.dialog, Some(Dialog::Input { text, .. }) if text == "nas:backup"));
    let line = bar_line(&h);
    assert!(line.contains("│ smb:// ││ [Revising] ") && line.ends_with("│ ↻ │"), "{}", h.screen);
    assert!(edit_line(&h).ends_with("││ nas:backup                           │"), "{}", h.screen);
    // An arrow leads from the address bar down to the window that edits it.
    let lines: Vec<&str> = h.screen.lines().collect();
    assert!(lines[2].ends_with("╰─┬─────────────────────╯╰───╯"), "{}", h.screen);
    assert!(lines[3].ends_with("╭───────────▼──────────────────────────╮"), "{}", h.screen);
    let listed = h.calls("operations/list").len();
    h.press(KeyCode::Enter);
    assert_eq!(h.app.dialog, None);
    assert_eq!(h.calls("operations/list").len(), listed + 1);

    // Edited, it becomes a go button.
    h.press(KeyCode::Char(':'));
    h.type_text("/photos");
    assert!(bar_line(&h).ends_with("│ > │"), "{}", h.screen);
    assert!(edit_line(&h).contains("││ nas:backup/photos "), "{}", h.screen);
    let button = h.app.hitmap.buttons[0];
    h.click(button.x, button.y);
    assert_eq!(h.app.panes[1].loc, Location::new("nas:", "backup/photos"));

    // A long address scrolls, with `<` / `>` where text is cut off.
    h.press(KeyCode::Char(':'));
    h.press_mod(KeyCode::Char('u'), KeyModifiers::CONTROL);
    h.type_text(&format!("nas:{}", "x".repeat(50)));
    let end = format!("││ <{}  │", "x".repeat(34));
    assert!(edit_line(&h).ends_with(&end), "{}", h.screen);
    h.press(KeyCode::Home);
    let start = format!("││ nas:{}> │", "x".repeat(31));
    assert!(edit_line(&h).ends_with(&start), "{}", h.screen);
    h.press(KeyCode::Esc);

    // A click elsewhere cancels editing.
    h.press(KeyCode::Char(':'));
    h.type_text("x");
    h.click(2, 5);
    assert_eq!(h.app.dialog, None);
    assert_eq!(h.app.panes[1].loc, Location::new("nas:", "backup/photos"));
}

fn ssh_asks(h: &mut Harness, prompt: &str) -> crossbeam_channel::Receiver<Option<SshAnswer>> {
    ssh_asks_remember(h, prompt, false, None)
}

fn ssh_asks_remember(
    h: &mut Harness,
    prompt: &str,
    can_remember: bool,
    notice: Option<&str>,
) -> crossbeam_channel::Receiver<Option<SshAnswer>> {
    let (reply, answer) = crossbeam_channel::bounded(1);
    let question = SshQuestion {
        prompt: prompt.to_owned(),
        can_remember,
        notice: notice.map(str::to_owned),
        confirm: false,
    };
    h.app.handle(Event::SshPrompt { question, reply }, Instant::now());
    h.render();
    answer
}

fn typed(text: &str, remember: bool) -> SshAnswer {
    SshAnswer { text: text.to_owned(), remember }
}

/// A server never seen before: its whole question, fingerprint included, is shown, and No
/// is the default.
#[test]
fn a_new_host_key_is_trusted_only_by_choice() {
    let mut h = standard();
    let prompt = "The authenticity of host 'box (10.0.0.2)' can't be established.\n\
                  ED25519 key fingerprint is SHA256:7J9rpvDFH53wfi5g1MXVyZTrTaofAmCfqmhkhgwAans.\n\
                  Are you sure you want to continue connecting (yes/no/[fingerprint])?";
    let ask = |h: &mut Harness| {
        let (reply, answer) = crossbeam_channel::bounded(1);
        let question = SshQuestion {
            prompt: prompt.to_owned(),
            can_remember: false,
            notice: None,
            confirm: true,
        };
        h.app.handle(Event::SshPrompt { question, reply }, Instant::now());
        h.render();
        answer
    };
    let answer = ask(&mut h);
    assert!(
        h.screen.contains("SHA256:7J9rpvDFH53wfi5g1MXVyZTrTaofAmCfqmhkhgwAans"),
        "{}",
        h.screen
    );
    h.press(KeyCode::Enter);
    assert_eq!(answer.try_recv(), Ok(None), "Enter alone does not trust it");
    let answer = ask(&mut h);
    h.press(KeyCode::Left);
    h.press(KeyCode::Enter);
    assert_eq!(answer.try_recv(), Ok(Some(typed("yes", false))));
    assert_eq!(h.app.dialog, None);
}

#[test]
fn ssh_questions_are_asked_in_a_dialog() {
    let mut h = standard();
    let answer = ssh_asks(&mut h, "me@box's password:");
    assert!(
        h.screen.contains("SSH login") && h.screen.contains("me@box's password:"),
        "{}",
        h.screen
    );
    h.type_text("hunter2");
    assert!(h.screen.contains("•••••••") && !h.screen.contains("hunter2"), "{}", h.screen);
    assert!(!h.screen.contains("Remember password"), "{}", h.screen);
    h.press(KeyCode::Enter);
    assert_eq!(answer.try_recv(), Ok(Some(typed("hunter2", false))));
    assert_eq!(h.app.dialog, None);

    // Esc cancels.
    let answer = ssh_asks(&mut h, "Verification code:");
    h.press(KeyCode::Esc);
    assert_eq!(answer.try_recv(), Ok(None));
    assert_eq!(h.app.dialog, None);

    // A question arriving while another dialog is open waits for it to close.
    h.press(KeyCode::Char('?'));
    let answer = ssh_asks(&mut h, "Password:");
    assert!(matches!(h.app.dialog, Some(Dialog::Help { .. })));
    h.press(KeyCode::Esc);
    assert!(h.screen.contains("Password:"), "{}", h.screen);
    h.type_text("x");
    h.press(KeyCode::Enter);
    assert_eq!(answer.try_recv(), Ok(Some(typed("x", false))));
}

#[test]
fn ssh_passwords_can_be_remembered() {
    let mut h = standard();
    let answer = ssh_asks_remember(&mut h, "me@box's password:", true, None);
    assert!(h.screen.contains("[ ] Remember password  (Tab)"), "{}", h.screen);
    assert!(!h.screen.contains("Why you are asked"), "{}", h.screen);
    h.type_text("pw");
    h.press(KeyCode::Tab);
    assert!(h.screen.contains("[x] Remember password"), "{}", h.screen);
    h.press(KeyCode::Enter);
    assert_eq!(answer.try_recv(), Ok(Some(typed("pw", true))));

    // Clicking the box toggles it; the typed text stays.
    let answer = ssh_asks_remember(&mut h, "me@box's password:", true, None);
    h.type_text("pw2");
    let (x, y) = h.find("[ ] Remember");
    h.click(x + 1, y);
    assert!(h.screen.contains("[x] Remember password"), "{}", h.screen);
    h.click(x + 5, y);
    assert!(h.screen.contains("[ ] Remember password"), "{}", h.screen);
    h.press(KeyCode::Enter);
    assert_eq!(answer.try_recv(), Ok(Some(typed("pw2", false))));

    // Why a remembered password was not used shows in a window under the dialog, and
    // stays while the box is toggled.
    let reason = "The remembered password cannot be used: /home/me/.ssh/rse-passwords can \
                  be read by other users, so rse does not use it; run chmod 600 it";
    let answer = ssh_asks_remember(&mut h, "me@box's password:", true, Some(reason));
    let lines: Vec<&str> = h.screen.lines().collect();
    let dialog = lines.iter().position(|l| l.contains("SSH login")).unwrap();
    let notice = lines.iter().position(|l| l.contains("Why you are asked")).unwrap();
    assert_eq!(notice, dialog + 7, "right under the dialog\n{}", h.screen);
    assert!(lines[notice + 1].contains("The remembered password cannot be used"), "{}", h.screen);
    assert!(h.screen.contains("chmod 600"), "{}", h.screen);
    h.press(KeyCode::Tab);
    assert!(h.screen.contains("Why you are asked"), "{}", h.screen);
    h.press(KeyCode::Esc);
    assert_eq!(answer.try_recv(), Ok(None));
    assert!(!h.screen.contains("Why you are asked"), "{}", h.screen);
}

#[test]
fn number_keys_focus_the_windows_named_in_their_corners() {
    let mut h = standard();
    let lines: Vec<&str> = h.screen.lines().collect();
    assert!(lines[0].contains("[1] Address─╮") && lines[0].contains("[3] Address─╮"));
    assert!(lines[3].contains("[2] Local─╮") && lines[3].ends_with("[4] Remote─╮"));
    assert!(h.screen.contains("[5] Transfers─╮"), "{}", h.screen);
    h.press(KeyCode::Char('4'));
    assert_eq!((h.app.active, h.app.focus), (1, Focus::Panes));
    h.press(KeyCode::Char('5'));
    assert_eq!(h.app.focus, Focus::Queue);
    h.press(KeyCode::Char('2'));
    assert_eq!((h.app.active, h.app.focus), (0, Focus::Panes));
    // An address window's key focuses that pane's address bar, which lights up with the
    // boxes beside it. The entries' keys wait; Enter edits it, Esc goes back.
    h.press(KeyCode::Char('3'));
    assert_eq!((h.app.dialog.is_none(), h.app.active, h.app.focus), (true, 1, Focus::Address));
    let (bar, button) = (h.app.hitmap.panes[1].bar, h.app.hitmap.panes[1].button);
    assert_eq!(
        [h.fg(bar.x - 1, bar.y), h.fg(bar.x, bar.y), h.fg(button.x, button.y)],
        [Color::Green; 3]
    );
    h.press(KeyCode::Down);
    assert_eq!(h.app.panes[1].cursor, 0);
    h.press(KeyCode::Enter);
    assert!(matches!(&h.app.dialog, Some(Dialog::Input { text, .. }) if text == "nas:backup"));
    h.press(KeyCode::Esc);
    assert_eq!((h.app.dialog.is_none(), h.app.focus), (true, Focus::Address));
    h.press(KeyCode::Esc);
    assert_eq!((h.app.active, h.app.focus), (1, Focus::Panes));
    // The protocol box is only information: clicking it does nothing.
    h.click(2, 1);
    assert_eq!((h.app.dialog.is_none(), h.app.active), (true, 1));
    // A click on a pane's border focuses that pane; on its address window's, edits it.
    let frame = h.app.hitmap.panes[0].frame;
    h.click(frame.x, frame.y + 3);
    assert_eq!(h.app.active, 0);
    assert!(h.app.dialog.is_none());
    let bar = h.app.hitmap.panes[1].bar;
    h.click(bar.x + 3, bar.y);
    assert_eq!(h.app.active, 1);
    assert!(matches!(&h.app.dialog, Some(Dialog::Input { text, .. }) if text == "nas:backup"));
}

#[test]
fn marks_show_in_the_corner_and_clear_with_esc_or_a_blank_click() {
    let mut h = standard();
    h.press(KeyCode::Down);
    h.press(KeyCode::Char(' '));
    h.press(KeyCode::Char(' '));
    let bottom = |h: &Harness| {
        let frame = h.app.hitmap.panes[0].frame;
        h.screen.lines().nth(usize::from(frame.y + frame.height - 1)).unwrap_or_default().to_owned()
    };
    assert!(bottom(&h).contains("─[2 marked]─╯"), "{}", h.screen);
    h.press(KeyCode::Esc);
    assert!(h.app.panes[0].marks.is_empty());
    assert!(!h.screen.contains("marked"), "{}", h.screen);

    h.press(KeyCode::Char(' '));
    let list = h.app.hitmap.panes[0].list;
    h.click(list.x + 3, list.y + list.height - 1);
    assert!(h.app.panes[0].marks.is_empty());
}

#[test]
fn remote_picker_filters_and_switches() {
    let mut h = standard();
    h.press(KeyCode::Char('g'));
    assert!(
        h.screen.contains("Local (~)") && h.screen.contains("nas:") && h.screen.contains("s3:"),
        "{}",
        h.screen
    );
    h.type_text("s3");
    h.press(KeyCode::Enter);
    assert_eq!(h.app.panes[0].loc, Location::new("s3:", ""));
    h.press(KeyCode::F(10));
    h.press(KeyCode::Enter);
    assert_eq!(h.app.panes[0].loc, home());
}

#[test]
fn swap_and_switch_panes() {
    let mut h = standard();
    h.press_mod(KeyCode::Char('u'), KeyModifiers::CONTROL);
    assert_eq!((h.app.panes[0].loc.clone(), h.app.panes[1].loc.clone()), (nas(), home()));
    h.press(KeyCode::Tab);
    assert_eq!(h.app.active, 1);
    h.press_mod(KeyCode::Char('r'), KeyModifiers::CONTROL);
    assert_eq!(h.app.panes[1].status, PaneStatus::Ready);
}

// ----- operations ----------------------------------------------------------------

#[test]
fn copy_file_asks_then_submits_copyfile() {
    let mut h = standard();
    select(&mut h, "b.txt");
    h.press(KeyCode::F(5));
    assert!(matches!(&h.app.dialog, Some(Dialog::Confirm { focus: 0, .. })));
    assert!(h.screen.contains("Copy 1 item (2.0 KB in 1 file)"), "{}", h.screen);
    assert!(h.screen.contains("to    nas:backup"), "{}", h.screen);
    h.press(KeyCode::Enter);
    h.wait_until(|a| a.queue.jobs.first().is_some_and(|j| j.state == JobState::Done));
    let call = last_call(&h, "rse/copy");
    assert_eq!(call["srcFs"], "/home/sky");
    assert_eq!(call["srcRemote"], "b.txt");
    assert_eq!(call["dstFs"], "nas:backup");
    assert_eq!(call["dstRemote"], "b.txt");
    assert_eq!(call["_async"], true);
    // The target pane is reloaded after the job.
    assert!(h.calls("operations/list").iter().filter(|c| c["fs"] == "nas:backup").count() >= 2);
}

#[test]
fn copy_dir_uses_rse_copy_and_merges_note() {
    let mut h = standard();
    select(&mut h, "projects");
    h.press(KeyCode::Char('c'));
    assert!(h.screen.contains("Directories are merged"), "{}", h.screen);
    h.press(KeyCode::Enter);
    h.wait_until(|a| a.queue.jobs.first().is_some_and(|j| j.state == JobState::Done));
    let call = last_call(&h, "rse/copy");
    assert_eq!(call["srcFs"], "/home/sky/projects");
    assert_eq!(call["dstFs"], "nas:backup/projects");
    assert_eq!(call["createEmptySrcDirs"], true);
}

#[test]
fn conflicts_ask_per_item() {
    let mut h = standard();
    select(&mut h, "a.pdf");
    h.press(KeyCode::Char(' '));
    h.press(KeyCode::Char(' '));
    h.press(KeyCode::F(5));
    h.press(KeyCode::Enter);
    assert!(
        matches!(&h.app.dialog, Some(Dialog::Conflict { conflicts, .. }) if conflicts.len() == 1)
    );
    assert!(h.screen.contains("a.pdf already exists in nas:backup"), "{}", h.screen);
    // Skip is the default: only b.txt is copied.
    assert!(matches!(&h.app.dialog, Some(Dialog::Conflict { focus: 1, .. })));
    h.press(KeyCode::Enter);
    h.wait_until(|a| a.queue.jobs.first().is_some_and(|j| j.state == JobState::Done));
    let copied: Vec<_> = h.calls("rse/copy").iter().map(|c| c["srcRemote"].clone()).collect();
    assert_eq!(copied, [json!("b.txt")]);
}

#[test]
fn overwrite_all_copies_everything() {
    let mut h = standard();
    select(&mut h, "a.pdf");
    h.press(KeyCode::F(5));
    h.press(KeyCode::Enter);
    assert!(matches!(h.app.dialog, Some(Dialog::Conflict { .. })));
    h.press(KeyCode::Right);
    h.press(KeyCode::Enter);
    h.wait_until(|a| !a.queue.jobs.is_empty());
    assert_eq!(h.calls("rse/copy").len(), 1);
}

/// The pane showing the target listed it before a.pdf appeared there: the target is listed
/// again, so a.pdf is not overwritten without asking.
#[test]
fn conflict_check_lists_the_target_again() {
    let mut h = standard();
    h.app.panes[1].entries.clear();
    h.app.panes[1].rebuild(false);
    select(&mut h, "a.pdf");
    h.press(KeyCode::F(5));
    h.press(KeyCode::Enter);
    h.wait_until(|a| a.dialog.is_some());
    assert!(
        matches!(&h.app.dialog, Some(Dialog::Conflict { conflicts, .. }) if conflicts.len() == 1)
    );
}

#[test]
fn conflict_check_lists_a_target_no_pane_shows() {
    let mut h = standard();
    // Drop target that is not shown: copy into nas:backup/photos via a drag later; here
    // simulate by pointing the other pane elsewhere and checking a probe happens.
    h.app.panes[1].status = PaneStatus::Loading;
    select(&mut h, "b.txt");
    h.press(KeyCode::F(5));
    h.press(KeyCode::Enter);
    h.wait_until(|a| !a.queue.jobs.is_empty());
    assert!(h.calls("operations/list").iter().filter(|c| c["fs"] == "nas:backup").count() >= 2);
}

#[test]
fn move_across_remotes_warns() {
    let mut h = standard();
    select(&mut h, "b.txt");
    h.press(KeyCode::F(6));
    assert!(h.screen.contains("already gone from the source"), "{}", h.screen);
    h.press(KeyCode::Enter);
    h.wait_until(|a| a.queue.jobs.first().is_some_and(|j| j.state == JobState::Done));
    assert_eq!(last_call(&h, "rse/copy")["dstFs"], "nas:backup");
}

#[test]
fn delete_defaults_to_cancel() {
    let mut h = standard();
    select(&mut h, "projects");
    h.press(KeyCode::F(8));
    assert!(h.screen.contains("including all subdirectories"), "{}", h.screen);
    h.press(KeyCode::Enter);
    assert_eq!(h.app.dialog, None);
    assert_eq!(h.app.queue.jobs.len(), 0);

    h.press(KeyCode::Delete);
    h.press(KeyCode::Left);
    h.press(KeyCode::Enter);
    h.wait_until(|a| !a.queue.jobs.is_empty());
    assert_eq!(
        last_call(&h, "operations/purge"),
        json!({"fs": "/home/sky", "remote": "projects", "_async": true})
    );
}

#[test]
fn copy_to_the_same_directory_is_refused() {
    let mut h = standard();
    h.app.panes[1].loc = home();
    select(&mut h, "b.txt");
    h.press(KeyCode::F(5));
    assert_eq!(h.app.dialog, None);
    assert_eq!(h.app.status.as_deref(), Some("source and target are the same directory"));
}

#[test]
fn copying_a_directory_into_itself_is_refused() {
    let mut h = standard();
    h.app.panes[1].loc = home().join("projects").join("rse");
    select(&mut h, "projects");
    h.press(KeyCode::F(5));
    assert_eq!(h.app.status.as_deref(), Some("cannot put a directory inside itself"));
}

#[test]
fn nothing_selected_on_parent_row() {
    let mut h = standard();
    h.press(KeyCode::Home);
    h.press(KeyCode::F(8));
    assert_eq!(h.app.status.as_deref(), Some("nothing selected"));
}

#[test]
fn mkdir_creates_and_selects() {
    let mut h = standard();
    h.press(KeyCode::F(7));
    h.type_text("new dir");
    h.press(KeyCode::Enter);
    h.wait_until(|a| a.panes[0].current().is_some_and(|e| e.name == "new dir"));
    assert_eq!(last_call(&h, "operations/mkdir"), json!({"fs": "/home/sky", "remote": "new dir"}));
}

#[test]
fn mkdir_rejects_existing_and_slash() {
    let mut h = standard();
    h.press(KeyCode::Char('n'));
    h.type_text("Docs");
    h.press(KeyCode::Enter);
    assert_eq!(h.app.status.as_deref(), Some("Docs already exists"));
    h.press(KeyCode::Char('n'));
    h.type_text("a/b");
    h.press(KeyCode::Enter);
    assert_eq!(h.app.status.as_deref(), Some("a name cannot contain /"));
    assert_eq!(h.calls("operations/mkdir").len(), 0);
}

#[test]
fn rename_file_and_directory() {
    let mut h = standard();
    select(&mut h, "b.txt");
    h.press(KeyCode::F(2));
    h.press(KeyCode::Left);
    h.press(KeyCode::Left);
    h.press(KeyCode::Left);
    h.press(KeyCode::Left);
    h.type_text("2");
    h.press(KeyCode::Enter);
    h.wait_until(|a| !a.has_pending_requests());
    assert_eq!(
        last_call(&h, "operations/movefile"),
        json!({"srcFs": "/home/sky", "srcRemote": "b.txt", "dstFs": "/home/sky", "dstRemote": "b2.txt"})
    );

    select(&mut h, "Docs");
    h.press(KeyCode::Char('r'));
    h.press_mod(KeyCode::Char('u'), KeyModifiers::CONTROL);
    h.type_text("Papers");
    h.press(KeyCode::Enter);
    h.wait_until(|a| !a.has_pending_requests());
    assert_eq!(last_call(&h, "sync/move")["dstFs"], "/home/sky/Papers");
}

#[test]
fn transfer_queue_cancel_retry_clear() {
    let stopped = Arc::new(AtomicBool::new(false));
    let s = stopped.clone();
    let backend = Arc::new(FakeBackend::new(move |m, input| match m {
        "operations/list" => Ok(
            json!({"list": if input["fs"] == "/home/sky" { vec![file("big.bin", 1 << 30)] } else { vec![] }}),
        ),
        "rse/copy" => Ok(json!({"jobid": 5})),
        "job/stop" => {
            s.store(true, Ordering::SeqCst);
            Ok(json!({}))
        }
        "job/status" => {
            let done = s.load(Ordering::SeqCst);
            Ok(
                json!({"id": input["jobid"], "finished": done, "success": false, "error": if done { "context canceled" } else { "" }}),
            )
        }
        "core/stats" => {
            Ok(json!({"bytes": 1 << 29, "totalBytes": 1 << 30, "speed": 52e6, "eta": 16}))
        }
        _ => Ok(json!({})),
    }));
    let mut h = Harness::new(backend, home(), nas());
    select(&mut h, "big.bin");
    h.press(KeyCode::F(5));
    h.press(KeyCode::Enter);
    h.wait_until(|a| a.queue.jobs.first().is_some_and(|j| j.stats.bytes > 0));
    assert!(h.screen.contains("copy big.bin → nas:backup"), "{}", h.screen);
    assert!(h.screen.contains("50%"), "{}", h.screen);

    h.press_mod(KeyCode::Char('t'), KeyModifiers::CONTROL);
    assert_eq!(h.app.focus, Focus::Queue);
    h.press(KeyCode::Char('x'));
    h.wait_until(|a| a.queue.jobs[0].state == JobState::Cancelled);
    assert_eq!(h.calls("job/stop"), vec![json!({"jobid": 5})]);
    assert!(h.app.errors.is_empty(), "a cancel is not an error");

    stopped.store(false, Ordering::SeqCst);
    h.press(KeyCode::Char('r'));
    h.wait_until(|a| a.queue.jobs[0].state == JobState::Running);
    h.press(KeyCode::Char('x'));
    h.wait_until(|a| a.queue.jobs[0].state == JobState::Cancelled);
    h.press(KeyCode::Char('c'));
    assert_eq!(h.app.queue.jobs.len(), 0);
    h.press(KeyCode::Esc);
    assert_eq!(h.app.focus, Focus::Panes);
}

/// A backend with big.bin in /home/sky whose copy runs until it is stopped; the flag is
/// set once `job/stop` has been called.
fn stoppable_copy() -> (Arc<FakeBackend>, Arc<AtomicBool>) {
    let stopped = Arc::new(AtomicBool::new(false));
    let s = stopped.clone();
    let backend = Arc::new(FakeBackend::new(move |m, input| match m {
        "operations/list" => Ok(
            json!({"list": if input["fs"] == "/home/sky" { vec![file("big.bin", 10)] } else { vec![] }}),
        ),
        "rse/copy" => Ok(json!({"jobid": 1})),
        "job/stop" => {
            s.store(true, Ordering::SeqCst);
            Ok(json!({}))
        }
        "job/status" => {
            Ok(json!({"id": 1, "finished": s.load(Ordering::SeqCst), "success": false}))
        }
        "core/stats" => Ok(json!({"bytes": 4, "totalBytes": 10,
            "transferring": [{"name": "big.bin", "bytes": 4, "size": 10}]})),
        _ => Ok(json!({})),
    }));
    (backend, stopped)
}

fn copy_running(backend: Arc<FakeBackend>) -> Harness {
    let mut h = Harness::new(backend, home(), nas());
    select(&mut h, "big.bin");
    h.press(KeyCode::F(5));
    h.press(KeyCode::Enter);
    h.wait_until(|a| a.queue.jobs.first().is_some_and(|j| j.state == JobState::Running));
    h
}

/// A job that would lose the file under way is named before quitting, as it is now; the
/// quit key again quits, pausing it.
#[test]
fn quit_with_a_job_that_cannot_resume_asks_and_q_again_pauses_it() {
    let (backend, stopped) = stoppable_copy();
    let mut h = copy_running(backend);
    h.wait_until(|a| a.queue.jobs[0].stats.bytes == 4);

    h.press(KeyCode::Char('q'));
    assert!(matches!(h.app.dialog, Some(Dialog::Quit { focus: 1, .. })));
    h.render();
    assert!(h.screen.contains("big.bin: 4 B of 10 B would be sent again"), "{}", h.screen);
    h.press(KeyCode::Enter); // "Back" is the default
    assert_eq!(h.app.quit, QuitState::Running);

    h.press(KeyCode::Char('q'));
    h.press(KeyCode::Char('q'));
    h.wait_until(|a| a.quit == QuitState::Done);
    assert!(stopped.load(Ordering::SeqCst));
    assert_eq!(h.app.queue.jobs[0].state, JobState::Queued, "kept for the next start");
}

/// A job that goes on from where it stops is paused without a question.
#[test]
fn quit_with_jobs_that_resume_is_immediate() {
    let (backend, stopped) = stoppable_copy();
    let mut h = copy_running(backend);
    h.app.queue.started(1, Engine::Rsync, true);
    h.press(KeyCode::Char('q'));
    assert!(h.app.dialog.is_none());
    h.wait_until(|a| a.quit == QuitState::Done);
    assert!(stopped.load(Ordering::SeqCst));
}

/// Leaving one of a job's endpoints pauses it, after asking when it would lose the file
/// under way; it is not shown while away, and goes on when the pane comes back.
#[test]
fn leaving_an_endpoint_pauses_its_jobs_until_it_is_shown_again() {
    let (backend, stopped) = stoppable_copy();
    let mut h = copy_running(backend.clone());
    h.app.active = 1;
    goto(&mut h, "s3:");
    assert!(matches!(h.app.dialog, Some(Dialog::Leave { .. })), "{:?}", h.app.dialog);
    h.press(KeyCode::Esc);
    assert_eq!(h.app.panes[1].loc, nas(), "Esc stays");

    goto(&mut h, "s3:");
    h.press(KeyCode::Left);
    h.press(KeyCode::Enter);
    assert_eq!(h.app.panes[1].loc, Location::new("s3:", ""));
    h.wait_until(|a| a.queue.jobs[0].state == JobState::Queued);
    assert!(stopped.load(Ordering::SeqCst));
    assert_eq!(h.app.queue.visible().len(), 0);

    stopped.store(false, Ordering::SeqCst);
    goto(&mut h, "nas:backup");
    assert!(h.app.dialog.is_none());
    h.wait_until(|a| a.queue.jobs[0].state == JobState::Running);
    assert_eq!(h.app.queue.visible().len(), 1);
    assert_eq!(backend.calls_to("rse/copy").len(), 2);
}

#[test]
fn quit_without_jobs_is_immediate() {
    let mut h = standard();
    h.press(KeyCode::Char('q'));
    assert_eq!(h.app.quit, QuitState::Done);
}

#[test]
fn quit_gives_up_waiting_after_the_grace_period() {
    let mut h = standard();
    // Running at once: chosen to overwrite, its target is not checked.
    h.app.queue.push(vec![Transfer {
        kind: OpKind::Copy,
        src_dir: home(),
        name: "x".into(),
        is_dir: false,
        dst_dir: Some(nas()),
        size: None,
        overwrite: true,
    }]);
    let now = Instant::now();
    h.app.stop_and_quit(now);
    assert!(matches!(h.app.quit, QuitState::Stopping { .. }));
    h.app.handle(Event::Tick, now + Duration::from_secs(6));
    assert_eq!(h.app.quit, QuitState::Done);
}

// ----- checks as jobs start, overwrite mode ----------------------------------------

/// `names` are queued for nas:backup, which has none of them when listed for the
/// conflict check; they arrive there while their jobs list it again before starting.
fn taken_after_queueing(names: &[&str]) -> (Harness, Tree) {
    let (backend, tree) = fake_fs(
        &[("/home/sky", names.iter().map(|n| file(n, 2048)).collect()), ("nas:backup", vec![])],
        &[("nas:backup", Duration::from_millis(200))],
    );
    let mut h = Harness::new(backend, home(), nas());
    h.wait_until(|a| a.panes[1].status == PaneStatus::Ready);
    h.press(KeyCode::Char('a'));
    h.press(KeyCode::F(5));
    h.press(KeyCode::Enter);
    h.wait_until(|a| {
        a.queue.jobs.len() == names.len()
            && a.queue.jobs.iter().all(|j| j.state == JobState::Checking)
    });
    let arrived = names.iter().map(|n| file(n, 1)).collect();
    tree.lock().unwrap().insert("nas:backup".to_owned(), arrived);
    (h, tree)
}

/// How many jobs the conflict dialog for jobs holds, 0 if it is not open.
fn job_conflicts(app: &App) -> usize {
    match &app.dialog {
        Some(Dialog::JobConflict { conflicts, .. }) => conflicts.len(),
        _ => 0,
    }
}

#[test]
fn a_file_that_arrives_before_the_job_starts_is_not_overwritten() {
    let (mut h, _) = taken_after_queueing(&["b.txt"]);
    h.wait_until(|a| a.dialog.is_some());
    assert!(matches!(h.app.dialog, Some(Dialog::JobConflict { focus: 1, .. })), "Skip");
    for text in [
        "copy b.txt → nas:backup did not start:",
        "b.txt already exists in nas:backup",
        "existing  1 B, modified 2026-10-20 08:15",
        "new       2.0 KB",
        "Cancel leaves the job failed in the queue;",
        "[ Overwrite ]  [ Skip ]  [ Overwrite all ]  [ Skip all ]  [ Cancel ]",
    ] {
        assert!(h.screen.contains(text), "{text}\n{}", h.screen);
    }
    // An error, in the queue and the log; nothing is copied.
    assert_eq!(h.app.queue.jobs[0].state, JobState::Failed("b.txt already exists there".into()));
    assert_eq!(h.app.errors[0].message, "copy b.txt → nas:backup: b.txt already exists there");
    assert_eq!(h.calls("rse/copy").len(), 0);

    h.press(KeyCode::Left);
    h.press(KeyCode::Enter);
    h.wait_until(|a| a.queue.jobs[0].state == JobState::Done);
    assert_eq!(last_call(&h, "rse/copy")["dstRemote"], "b.txt");
}

/// Cancel leaves the job failed. A retry lists the target again: still taken, it asks
/// again; free, it copies.
#[test]
fn a_retry_checks_the_target_again() {
    let (mut h, tree) = taken_after_queueing(&["b.txt"]);
    h.wait_until(|a| a.dialog.is_some());
    h.press(KeyCode::Esc);
    assert!(matches!(h.app.queue.jobs[0].state, JobState::Failed(_)));
    h.press(KeyCode::Char('5'));
    h.press(KeyCode::Char('r'));
    h.wait_until(|a| a.dialog.is_some());
    assert_eq!(job_conflicts(&h.app), 1);
    assert_eq!(h.calls("rse/copy").len(), 0);

    h.press(KeyCode::Esc);
    tree.lock().unwrap().insert("nas:backup".to_owned(), vec![]);
    h.press(KeyCode::Char('r'));
    h.wait_until(|a| a.queue.jobs[0].state == JobState::Done);
    assert_eq!(h.calls("rse/copy").len(), 1);
    assert!(h.app.dialog.is_none());
}

/// Two jobs find their target taken one after the other: the second joins the dialog the
/// first opened, whose "all" choices then take in both.
#[test]
fn jobs_in_conflict_together_are_decided_together() {
    let (mut h, _) = taken_after_queueing(&["a.txt", "b.txt"]);
    h.wait_until(|a| job_conflicts(a) == 2);
    assert!(h.screen.contains("conflict 1 of 2"), "{}", h.screen);
    assert!(matches!(h.app.dialog, Some(Dialog::JobConflict { focus: 1, .. })), "still Skip");
    h.press(KeyCode::Right);
    h.press(KeyCode::Enter);
    h.wait_until(|a| a.queue.jobs.iter().all(|j| j.state == JobState::Done));
    assert_eq!(h.calls("rse/copy").len(), 2);
    assert!(h.app.dialog.is_none());
}

/// Cancel at the first of two conflicts leaves both jobs failed, as the dialog says, and
/// the second is not asked about until a retry.
#[test]
fn cancel_leaves_every_job_not_yet_decided_failed() {
    let (mut h, _) = taken_after_queueing(&["a.txt", "b.txt"]);
    h.wait_until(|a| job_conflicts(a) == 2);
    let note = "Cancel leaves the 2 jobs not yet decided failed in the queue;";
    assert!(h.screen.contains(note), "{}", h.screen);
    h.press(KeyCode::Esc);
    assert_eq!(h.app.dialog, None);
    assert!(h.app.queue.jobs.iter().all(|j| matches!(j.state, JobState::Failed(_))));
}

/// A conflict that joins the open dialog makes "all" mean more than the user saw: a click
/// right after is ignored, as on a new dialog. The buttons stay where they were.
#[test]
fn a_click_right_after_a_conflict_joins_is_ignored() {
    let mut h = standard();
    let entry = Entry::file("a.pdf", 1);
    h.app.dialog =
        Some(Dialog::JobConflict { conflicts: vec![(1, entry.clone())], index: 0, focus: SKIP });
    h.render();
    let buttons = h.app.hitmap.buttons.clone();
    let t = Instant::now() + Duration::from_secs(5);
    h.app.job_conflicts.push((2, entry));
    h.app.handle(Event::Tick, t);
    h.render();
    assert_eq!(h.app.hitmap.buttons, buttons);
    let all = buttons[2];
    let click = |h: &mut Harness, at| {
        h.mouse_at(MouseEventKind::Down(MouseButton::Left), all.x, all.y, KeyModifiers::NONE, at);
    };
    click(&mut h, t + Duration::from_millis(100));
    assert_eq!(job_conflicts(&h.app), 2, "ignored");
    click(&mut h, t + Duration::from_millis(400));
    assert_eq!(h.app.dialog, None);
}

#[test]
fn a_conflict_waits_for_the_dialog_in_front_of_it() {
    let (mut h, _) = taken_after_queueing(&["b.txt"]);
    h.press(KeyCode::Char('?'));
    h.wait_until(|a| matches!(a.queue.jobs[0].state, JobState::Failed(_)));
    assert!(matches!(h.app.dialog, Some(Dialog::Help { .. })));
    h.press(KeyCode::Esc);
    assert_eq!(job_conflicts(&h.app), 1);
}

/// Two items of one name in any case, queued together for one directory: the second waits
/// for the first, then finds what it wrote and asks, instead of both checking an empty
/// target and the last to end winning.
#[test]
fn a_job_waits_for_an_earlier_one_writing_its_name() {
    let target: Arc<Mutex<Vec<Value>>> = Arc::default();
    let t = target.clone();
    let backend = Arc::new(FakeBackend::new(move |m, input| match m {
        "operations/list" if input["fs"] == "nas:backup" => {
            let list = t.lock().unwrap().clone();
            Ok(json!({ "list": list }))
        }
        "operations/list" => Ok(json!({"list": [file("IMG.JPG", 1), file("img.jpg", 2)]})),
        "rse/copy" => {
            t.lock().unwrap().push(file(input["dstRemote"].as_str().unwrap_or_default(), 1));
            Ok(json!({"jobid": 1}))
        }
        "job/status" => Ok(json!({"id": input["jobid"], "finished": true, "success": true})),
        _ => Ok(json!({})),
    }));
    let mut h = Harness::new(backend, home(), nas());
    h.press(KeyCode::Char('a'));
    h.press(KeyCode::F(5));
    h.press(KeyCode::Enter);
    h.wait_until(|a| a.dialog.is_some());
    assert_eq!(job_conflicts(&h.app), 1);
    assert!(h.screen.contains("copy img.jpg → nas:backup did not start:"), "{}", h.screen);
    assert!(h.screen.contains("IMG.JPG already exists in nas:backup"), "{}", h.screen);
    assert_eq!(h.calls("rse/copy").len(), 1);
    assert_eq!(h.app.queue.jobs[0].state, JobState::Done);
}

/// nas:backup answers the pane and the conflict check; the listings after those, the
/// jobs' checks, hang until `release` is set. /home/sky holds `names`, all queued.
fn hanging_checks(names: &[&str]) -> (Harness, Arc<AtomicBool>) {
    let listed = AtomicUsize::new(0);
    let release = Arc::new(AtomicBool::new(false));
    let r = release.clone();
    let files: Vec<Value> = names.iter().map(|n| file(n, 1)).collect();
    let backend = Arc::new(FakeBackend::new(move |m, input| match m {
        "operations/list" if input["fs"] == "nas:backup" => {
            if listed.fetch_add(1, Ordering::SeqCst) >= 2 {
                while !r.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
            Ok(json!({"list": []}))
        }
        "operations/list" => Ok(json!({ "list": files })),
        "job/status" => Ok(json!({"id": input["jobid"], "finished": true, "success": true})),
        _ if input.get("_async").is_some() => Ok(json!({"jobid": 1})),
        _ => Ok(json!({})),
    }));
    let mut h = Harness::new(backend, home(), nas());
    h.press(KeyCode::Char('a'));
    h.press(KeyCode::F(5));
    h.press(KeyCode::Enter);
    h.wait_until(|a| a.queue.jobs.len() == names.len());
    (h, release)
}

fn states(h: &Harness) -> Vec<JobState> {
    h.app.queue.jobs.iter().map(|j| j.state.clone()).collect()
}

/// Cancelling the jobs whose checks hang makes room for the next one at once; the answers
/// to those checks, when they come, change nothing.
#[test]
fn cancelling_checks_that_hang_starts_the_next_job() {
    let (mut h, release) = hanging_checks(&["a", "b", "c"]);
    assert_eq!(states(&h), [JobState::Checking, JobState::Checking, JobState::Queued]);
    h.press(KeyCode::Char('5'));
    h.press(KeyCode::Char('x'));
    h.press(KeyCode::Down);
    h.press(KeyCode::Char('x'));
    assert_eq!(states(&h), [JobState::Cancelled, JobState::Cancelled, JobState::Checking]);
    release.store(true, Ordering::SeqCst);
    h.wait_until(|a| a.queue.jobs[2].state == JobState::Done);
    assert_eq!(states(&h)[..2], [JobState::Cancelled, JobState::Cancelled]);
    assert_eq!(h.calls("rse/copy").len(), 1);
    assert_eq!(last_call(&h, "rse/copy")["srcRemote"], "c");
}

/// The answer to a check that was cancelled changes nothing, even with the job retried
/// and checking again: it may be older than what the target has now.
#[test]
fn the_answer_to_a_cancelled_check_is_ignored() {
    let (mut h, release) = hanging_checks(&["a"]);
    let cancelled = h.app.job_check(1, 1);
    h.press(KeyCode::Char('5'));
    h.press(KeyCode::Char('x'));
    h.press(KeyCode::Char('r'));
    assert_eq!(states(&h), [JobState::Checking]);
    let taken = Event::Probed { req: cancelled, result: Ok(vec![Entry::file("a", 1)]) };
    h.app.handle(taken, Instant::now());
    assert_eq!((states(&h), h.app.dialog.clone()), (vec![JobState::Checking], None));
    release.store(true, Ordering::SeqCst);
    h.wait_until(|a| a.queue.jobs[0].state == JobState::Done);
}

/// A dialog that comes up during a drag, such as a conflict, stays when the drag ends:
/// the drop menu does not take its place.
#[test]
fn a_drop_leaves_a_dialog_that_came_up_during_the_drag() {
    let mut h = standard();
    let from = h.find("b.txt");
    let t = Instant::now();
    h.mouse_at(MouseEventKind::Down(MouseButton::Left), from.0, from.1, KeyModifiers::NONE, t);
    h.mouse_at(MouseEventKind::Drag(MouseButton::Left), 50, 12, KeyModifiers::NONE, t);
    h.app.job_conflicts.push((1, Entry::file("a.pdf", 1)));
    h.app.handle(Event::Tick, t);
    h.mouse_at(MouseEventKind::Up(MouseButton::Left), 50, 12, KeyModifiers::NONE, t);
    assert_eq!(job_conflicts(&h.app), 1);
    let status = h.app.status.as_deref();
    assert_eq!(status, Some("nothing dropped: a dialog opened during the drag"));
}

/// Overwrite mode, turned on while a job lists its target, lets it replace what it finds.
#[test]
fn overwrite_mode_turned_on_during_a_check_applies_to_it() {
    let (mut h, _) = taken_after_queueing(&["b.txt"]);
    h.press(KeyCode::Char('O'));
    h.wait_until(|a| a.queue.jobs[0].state == JobState::Done);
    assert_eq!(h.app.dialog, None);
    assert_eq!(h.calls("rse/copy").len(), 1);
}

/// Overwrite mode: a row in white on red across the top says so, and a copy onto a file
/// the target has goes ahead with neither a question nor a listing of the target.
#[test]
fn overwrite_mode_copies_without_checking_and_says_so() {
    let mut h = standard();
    assert!(!h.screen.contains("Overwrite mode"), "{}", h.screen);
    h.press(KeyCode::Char('O'));
    let top = h.screen.lines().next().unwrap_or_default().to_owned();
    assert_eq!(
        top,
        " Overwrite mode  existing files are overwritten without asking; O turns it off"
    );
    assert_eq!((h.fg(1, 0), h.bg(1, 0), h.bg(79, 0)), (Color::White, Color::Red, Color::Red));
    let listed =
        |h: &Harness| h.calls("operations/list").iter().filter(|c| c["fs"] == "nas:backup").count();
    let before = listed(&h);

    select(&mut h, "a.pdf");
    h.press(KeyCode::F(5));
    assert!(h.screen.contains("Overwrite mode is on"), "{}", h.screen);
    assert!(h.screen.starts_with(" Overwrite mode "), "over the dialog too\n{}", h.screen);
    h.press(KeyCode::Enter);
    h.wait_until(|a| a.queue.jobs.first().is_some_and(|j| j.state == JobState::Done));
    h.wait_until(|a| a.panes[1].status == PaneStatus::Ready);
    assert_eq!(last_call(&h, "rse/copy")["dstRemote"], "a.pdf");
    assert_eq!(listed(&h), before + 1, "only the reload after the copy");

    h.press(KeyCode::Char('O'));
    assert!(!h.screen.contains("Overwrite mode"), "{}", h.screen);
}

fn rename(h: &mut Harness, from: &str, to: &str) {
    select(h, from);
    h.press(KeyCode::F(2));
    h.press_mod(KeyCode::Char('u'), KeyModifiers::CONTROL);
    h.type_text(to);
    h.press(KeyCode::Enter);
    h.wait_until(|a| !a.has_pending_requests() && a.panes[0].status == PaneStatus::Ready);
}

/// The pane listed /home/sky before B2.txt arrived. Renaming b.txt to b2.txt asks the
/// server and leaves B2.txt alone, as names differing only in case are one to many
/// targets; its own name in another case is no conflict. Overwrite mode renames onto it.
#[test]
fn a_rename_onto_a_name_only_the_server_has_is_refused() {
    let (backend, tree) =
        fake_fs(&[("/home/sky", vec![file("b.txt", 1)]), ("nas:backup", vec![])], &[]);
    let mut h = Harness::new(backend, home(), nas());
    tree.lock().unwrap().entry("/home/sky".to_owned()).or_default().push(file("B2.txt", 1));
    rename(&mut h, "b.txt", "b2.txt");
    assert_eq!(h.app.status.as_deref(), Some("B2.txt already exists"));
    assert_eq!(h.calls("operations/movefile").len(), 0);
    assert_eq!(names(&h, 0), ["..", "b.txt", "B2.txt"], "the pane shows it now");

    rename(&mut h, "b.txt", "B.txt");
    assert_eq!(last_call(&h, "operations/movefile")["dstRemote"], "B.txt");

    h.press(KeyCode::Char('O'));
    rename(&mut h, "b.txt", "B2.txt");
    assert_eq!(last_call(&h, "operations/movefile")["dstRemote"], "B2.txt");
}

// ----- mouse ---------------------------------------------------------------------

#[test]
fn click_moves_cursor_and_clears_marks() {
    let mut h = standard();
    h.app.panes[0].marks.insert(Entry::file("b.txt", 0).mark());
    let (x, y) = h.find("a.pdf");
    h.click(x, y);
    assert_eq!(cursor_name(&h, 0), "a.pdf");
    assert!(h.app.panes[0].marks.is_empty());
    // Clicking in the other pane focuses it.
    let (x, y) = h.find("photos/");
    h.click(x, y);
    assert_eq!(h.app.active, 1);
}

#[test]
fn ctrl_click_and_right_click_toggle_marks() {
    let mut h = standard();
    let (x, y) = h.find("a.pdf");
    h.click_mod(x, y, KeyModifiers::CONTROL);
    let (x2, y2) = h.find("b.txt");
    h.click_mod(x2, y2, KeyModifiers::CONTROL);
    assert_eq!(h.app.panes[0].marks.len(), 2);
    h.mouse_at(MouseEventKind::Down(MouseButton::Right), x, y, KeyModifiers::NONE, Instant::now());
    h.settle();
    assert_eq!(h.app.panes[0].marks.iter().map(|m| &m.name).collect::<Vec<_>>(), ["b.txt"]);
    assert_eq!(cursor_name(&h, 0), "a.pdf");
}

#[test]
fn right_click_takes_the_cursor_and_the_focus_to_the_row() {
    let mut h = standard();
    h.app.focus = Focus::Queue;
    let (x, y) = h.find("b.txt");
    h.mouse_at(MouseEventKind::Down(MouseButton::Right), x, y, KeyModifiers::NONE, Instant::now());
    h.settle();
    assert_eq!((h.app.focus, h.app.active), (Focus::Panes, 0));
    assert_eq!(cursor_name(&h, 0), "b.txt");
    // In the other pane too, which becomes the active one.
    let (x, y) = h.find("photos/");
    h.mouse_at(MouseEventKind::Down(MouseButton::Right), x, y, KeyModifiers::NONE, Instant::now());
    h.settle();
    assert_eq!(h.app.active, 1);
    assert_eq!(cursor_name(&h, 1), "photos");
    assert_eq!(h.app.panes[1].marks.iter().map(|m| &m.name).collect::<Vec<_>>(), ["photos"]);
}

#[test]
fn alt_modifier_can_replace_ctrl() {
    let (backend, _) =
        fake_fs(&[("/home/sky", vec![file("a", 1), file("b", 1)]), ("nas:backup", vec![])], &[]);
    let ui = UiConfig {
        multi_select_modifier: crate::config::MultiSelectModifier::Alt,
        ..UiConfig::default()
    };
    let mut h = Harness::with(backend, home(), nas(), ui, 80, 24);
    let (x, y) = h.find(" a ");
    h.click_mod(x + 1, y, KeyModifiers::ALT);
    assert_eq!(h.app.panes[0].marks.len(), 1);
    h.click_mod(x + 1, y, KeyModifiers::CONTROL);
    assert!(h.app.panes[0].marks.is_empty(), "ctrl+click is a plain click now");
}

#[test]
fn double_click_enters_directories_and_parent() {
    let mut h = standard();
    let (x, y) = h.find("projects/");
    let t = Instant::now();
    h.mouse_at(MouseEventKind::Down(MouseButton::Left), x, y, KeyModifiers::NONE, t);
    h.mouse_at(MouseEventKind::Up(MouseButton::Left), x, y, KeyModifiers::NONE, t);
    h.mouse_at(
        MouseEventKind::Down(MouseButton::Left),
        x,
        y,
        KeyModifiers::NONE,
        t + Duration::from_millis(200),
    );
    h.mouse_at(
        MouseEventKind::Up(MouseButton::Left),
        x,
        y,
        KeyModifiers::NONE,
        t + Duration::from_millis(200),
    );
    h.settle();
    assert_eq!(h.app.panes[0].loc, home().join("projects"));

    let (x, y) = h.find("..");
    let t = Instant::now();
    h.mouse_at(MouseEventKind::Down(MouseButton::Left), x, y, KeyModifiers::NONE, t);
    h.mouse_at(
        MouseEventKind::Down(MouseButton::Left),
        x,
        y,
        KeyModifiers::NONE,
        t + Duration::from_millis(100),
    );
    h.settle();
    assert_eq!(h.app.panes[0].loc, home());
}

#[test]
fn slow_second_click_is_not_a_double_click() {
    let mut h = standard();
    let (x, y) = h.find("projects/");
    let t = Instant::now();
    h.mouse_at(MouseEventKind::Down(MouseButton::Left), x, y, KeyModifiers::NONE, t);
    h.mouse_at(MouseEventKind::Up(MouseButton::Left), x, y, KeyModifiers::NONE, t);
    h.mouse_at(
        MouseEventKind::Down(MouseButton::Left),
        x,
        y,
        KeyModifiers::NONE,
        t + Duration::from_millis(600),
    );
    h.mouse_at(
        MouseEventKind::Up(MouseButton::Left),
        x,
        y,
        KeyModifiers::NONE,
        t + Duration::from_millis(600),
    );
    h.settle();
    assert_eq!(h.app.panes[0].loc, home());
}

#[test]
fn wheel_scrolls_the_pane_under_the_pointer_without_focus() {
    let entries: Vec<Value> = (0..100).map(|i| file(&format!("f{i:03}"), 1)).collect();
    let (backend, _) = fake_fs(&[("/home/sky", vec![]), ("nas:backup", entries)], &[]);
    let mut h = Harness::new(backend, home(), nas());
    let (x, y) = (60, 5);
    h.mouse_at(MouseEventKind::ScrollDown, x, y, KeyModifiers::NONE, Instant::now());
    h.mouse_at(MouseEventKind::ScrollDown, x, y, KeyModifiers::NONE, Instant::now());
    h.settle();
    assert_eq!(h.app.active, 0);
    assert_eq!(h.app.panes[1].offset, 6);
    assert!(h.screen.contains("f005") && !h.screen.contains("f004 "), "{}", h.screen);
    h.mouse_at(MouseEventKind::ScrollUp, x, y, KeyModifiers::NONE, Instant::now());
    h.settle();
    assert_eq!(h.app.panes[1].offset, 3);
}

/// The wheel moves the view, not the highlight: the cursor stays on its entry out of view,
/// and the view comes back to it when the cursor moves.
#[test]
fn wheel_leaves_the_cursor_on_its_entry() {
    let entries: Vec<Value> = (0..100).map(|i| file(&format!("f{i:03}"), 1)).collect();
    let (backend, _) = fake_fs(&[("/home/sky", vec![]), ("nas:backup", entries)], &[]);
    let mut h = Harness::new(backend, home(), nas());
    h.press(KeyCode::Tab);
    h.press(KeyCode::Down);
    assert_eq!(h.app.panes[1].cursor, 1);
    for _ in 0..4 {
        h.mouse_at(MouseEventKind::ScrollDown, 60, 5, KeyModifiers::NONE, Instant::now());
    }
    h.settle();
    assert_eq!(h.app.panes[1].offset, 12, "the view stays where it was scrolled to");
    assert_eq!(h.app.panes[1].cursor, 1);
    assert_eq!(h.app.panes[1].current().unwrap().name, "f000");
    h.press(KeyCode::Down);
    assert_eq!(h.app.panes[1].current().unwrap().name, "f001");
    assert_eq!(h.app.panes[1].offset, 2, "back to the cursor");
}

#[test]
fn clicking_a_column_header_sorts_by_it_and_again_the_other_way() {
    let mut h = standard();
    let (x, y) = h.find("Size");
    h.click(x, y);
    assert_eq!((h.app.panes[0].sort, h.app.panes[0].ascending), (SortKey::Size, false));
    assert_eq!(names(&h, 0)[3..], ["a.pdf", "b.txt"]);
    h.click(x, y);
    assert_eq!((h.app.panes[0].sort, h.app.panes[0].ascending), (SortKey::Size, true));
    assert_eq!(names(&h, 0)[3..], ["b.txt", "a.pdf"]);
    assert!(h.screen.contains("Size ▲"), "{}", h.screen);
    let (x, y) = h.find("Name");
    h.click(x, y);
    assert_eq!((h.app.panes[0].sort, h.app.panes[0].ascending), (SortKey::Name, true));
    h.click(x, y);
    assert!(!h.app.panes[0].ascending);
    assert_eq!(names(&h, 0), ["..", "projects", "Docs", "b.txt", "a.pdf"], "directories first");
}

#[test]
fn clicking_the_path_edits_it() {
    let mut h = standard();
    let start = h.app.panes[0].loc.clone();
    let (x, y) = h.find("home");
    h.click(x + 1, y);
    assert!(
        matches!(&h.app.dialog, Some(Dialog::Input { purpose: InputPurpose::Goto { .. }, .. })),
        "{}",
        h.screen
    );
    assert_eq!(h.app.panes[0].loc, start);
}

#[test]
fn clicking_a_key_hint_runs_it() {
    let mut h = standard();
    let (x, y) = h.find("F7 Mkdir");
    h.click(x, y);
    assert!(matches!(h.app.dialog, Some(Dialog::Input { purpose: InputPurpose::Mkdir, .. })));
    let (x, y) = h.find("[ Cancel ]");
    h.click(x + 2, y);
    assert_eq!(h.app.dialog, None);
}

fn drag(h: &mut Harness, from: (u16, u16), to: (u16, u16)) {
    let t = Instant::now();
    h.mouse_at(MouseEventKind::Down(MouseButton::Left), from.0, from.1, KeyModifiers::NONE, t);
    h.mouse_at(MouseEventKind::Drag(MouseButton::Left), from.0 + 1, from.1, KeyModifiers::NONE, t);
    h.mouse_at(MouseEventKind::Drag(MouseButton::Left), to.0, to.1, KeyModifiers::NONE, t);
    h.render();
    h.mouse_at(MouseEventKind::Up(MouseButton::Left), to.0, to.1, KeyModifiers::NONE, t);
    h.settle();
}

#[test]
fn drag_to_other_pane_offers_copy_or_move() {
    let mut h = standard();
    let from = h.find("b.txt");
    drag(&mut h, from, (50, 12));
    assert!(matches!(&h.app.dialog, Some(Dialog::Drop { focus: 0, .. })));
    assert!(h.screen.contains("Copy here") && h.screen.contains("Move here"), "{}", h.screen);
    h.press(KeyCode::Enter);
    h.wait_until(|a| a.queue.jobs.first().is_some_and(|j| j.state == JobState::Done));
    assert_eq!(last_call(&h, "rse/copy")["dstFs"], "nas:backup");
}

#[test]
fn drag_menu_move_still_confirms() {
    let mut h = standard();
    let from = h.find("b.txt");
    drag(&mut h, from, (50, 12));
    let (x, y) = h.find("Move here");
    h.click(x, y);
    assert!(
        matches!(&h.app.dialog, Some(Dialog::Confirm { plan, .. }) if plan.kind == OpKind::Move)
    );
    h.press(KeyCode::Enter);
    h.wait_until(|a| a.queue.jobs.first().is_some_and(|j| j.state == JobState::Done));
    assert_eq!(h.calls("rse/copy").len(), 1);
}

#[test]
fn dragging_a_marked_entry_drags_all_marked() {
    let mut h = standard();
    let a = h.find("a.pdf");
    let b = h.find("b.txt");
    h.click_mod(a.0, a.1, KeyModifiers::CONTROL);
    h.click_mod(b.0, b.1, KeyModifiers::CONTROL);
    let photos = h.find("photos/");
    drag(&mut h, b, photos);
    let Some(Dialog::Drop { plan, .. }) = &h.app.dialog else {
        panic!("no drop menu:\n{}", h.screen)
    };
    assert_eq!(plan.items.len(), 2);
    // Dropped on a directory: that directory is the target.
    assert_eq!(plan.dst_dir, Some(nas().join("photos")));
}

#[test]
fn dragging_an_unmarked_entry_drags_only_it() {
    let mut h = standard();
    let a = h.find("a.pdf");
    h.click_mod(a.0, a.1, KeyModifiers::CONTROL);
    let b = h.find("b.txt");
    drag(&mut h, b, (50, 12));
    let Some(Dialog::Drop { plan, .. }) = &h.app.dialog else { panic!() };
    assert_eq!(plan.items.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(), ["b.txt"]);
}

#[test]
fn drop_into_subdirectory_of_the_same_pane() {
    let mut h = standard();
    let from = h.find("b.txt");
    let to = h.find("Docs/");
    drag(&mut h, from, to);
    let Some(Dialog::Drop { plan, .. }) = &h.app.dialog else { panic!("{}", h.screen) };
    assert_eq!(plan.dst_dir, Some(home().join("Docs")));
}

#[test]
fn invalid_drops_do_nothing() {
    let mut h = standard();
    // Onto its own directory (a file in the same pane).
    let from = h.find("b.txt");
    let to = h.find("a.pdf");
    let t = Instant::now();
    h.mouse_at(MouseEventKind::Down(MouseButton::Left), from.0, from.1, KeyModifiers::NONE, t);
    h.mouse_at(MouseEventKind::Drag(MouseButton::Left), to.0, to.1, KeyModifiers::NONE, t);
    h.render();
    assert!(h.screen.contains("⊘ 1 item"), "{}", h.screen);
    h.mouse_at(MouseEventKind::Up(MouseButton::Left), to.0, to.1, KeyModifiers::NONE, t);
    h.settle();
    assert_eq!(h.app.dialog, None);
    // A directory onto itself.
    let p = h.find("projects/");
    drag(&mut h, p, (p.0 + 3, p.1));
    assert_eq!(h.app.dialog, None);
    assert_eq!(h.app.queue.jobs.len(), 0);
}

#[test]
fn esc_cancels_a_drag() {
    let mut h = standard();
    let from = h.find("b.txt");
    let t = Instant::now();
    h.mouse_at(MouseEventKind::Down(MouseButton::Left), from.0, from.1, KeyModifiers::NONE, t);
    h.mouse_at(MouseEventKind::Drag(MouseButton::Left), 50, 12, KeyModifiers::NONE, t);
    h.press(KeyCode::Esc);
    h.mouse_at(MouseEventKind::Up(MouseButton::Left), 50, 12, KeyModifiers::NONE, t);
    h.settle();
    assert_eq!(h.app.dialog, None);
    assert_eq!(h.app.drag, Drag::Idle);
}

#[test]
fn drop_action_copy_skips_the_menu() {
    let (backend, _) =
        fake_fs(&[("/home/sky", vec![file("b.txt", 1)]), ("nas:backup", vec![])], &[]);
    let ui = UiConfig { drop_action: DropAction::Copy, ..UiConfig::default() };
    let mut h = Harness::with(backend, home(), nas(), ui, 80, 24);
    let from = h.find("b.txt");
    drag(&mut h, from, (50, 5));
    h.wait_until(|a| a.queue.jobs.first().is_some_and(|j| j.state == JobState::Done));
    assert_eq!(h.calls("rse/copy").len(), 1);
}

#[test]
fn drag_rests_on_bottom_edge_autoscrolls() {
    let entries: Vec<Value> = (0..100).map(|i| file(&format!("f{i:03}"), 1)).collect();
    let (backend, _) = fake_fs(&[("/home/sky", vec![file("x", 1)]), ("nas:backup", entries)], &[]);
    let mut h = Harness::new(backend, home(), nas());
    let from = h.find(" x ");
    let bottom = h.app.hitmap.panes[1].list;
    let edge = (bottom.x + 3, bottom.y + bottom.height - 1);
    let t = Instant::now();
    h.mouse_at(MouseEventKind::Down(MouseButton::Left), from.0 + 1, from.1, KeyModifiers::NONE, t);
    h.mouse_at(MouseEventKind::Drag(MouseButton::Left), edge.0, edge.1, KeyModifiers::NONE, t);
    h.app.handle(Event::Tick, t);
    h.app.handle(Event::Tick, t);
    assert!(h.app.panes[1].offset >= 2, "offset {}", h.app.panes[1].offset);
}

#[test]
fn clicking_a_queue_cross_cancels() {
    let backend = Arc::new(FakeBackend::new(|m, input| match m {
        "operations/list" => Ok(
            json!({"list": if input["fs"] == "/home/sky" { vec![file("big.bin", 10)] } else { vec![] }}),
        ),
        "rse/copy" => Ok(json!({"jobid": 1})),
        "job/status" => Ok(json!({"id": 1, "finished": false, "success": false})),
        _ => Ok(json!({})),
    }));
    let mut h = Harness::new(backend, home(), nas());
    select(&mut h, "big.bin");
    h.press(KeyCode::F(5));
    h.press(KeyCode::Enter);
    h.wait_until(|a| a.queue.jobs.first().is_some_and(|j| j.state == JobState::Running));
    let (x, y) = h.find("✕");
    h.click(x, y);
    assert_eq!(h.app.queue.jobs[0].state, JobState::Cancelling);
    assert_eq!(h.calls("job/stop").len(), 1);
}

#[test]
fn help_scroll_stops_at_the_end() {
    let mut h = standard();
    h.press(KeyCode::Char('?'));
    for _ in 0..20 {
        h.press(KeyCode::PageDown);
    }
    let max = h.app.hitmap.scroll_max;
    assert!(max > 0);
    assert_eq!(h.app.dialog, Some(Dialog::Help { scroll: max }));
    h.press(KeyCode::Up);
    assert_eq!(h.app.dialog, Some(Dialog::Help { scroll: max - 1 }));
}

#[test]
fn help_and_error_log_open_and_close() {
    let mut h = standard();
    h.press(KeyCode::Char('?'));
    assert!(h.screen.contains("Switch between the panes"), "{}", h.screen);
    h.press(KeyCode::Esc);
    assert_eq!(h.app.dialog, None);
    h.press(KeyCode::Char('E'));
    assert!(h.screen.contains("No errors."), "{}", h.screen);
}

impl App {
    fn has_pending_requests(&self) -> bool {
        !self.pending.is_empty()
    }

    /// The request that lists the target for the job's check number `check`.
    fn job_check(&self, id: JobId, check: u64) -> ReqId {
        let found = self.pending.iter().find(
            |(_, p)| matches!(p, Pending::JobCheck { id: i, check: c } if (*i, *c) == (id, check)),
        );
        *found.expect("no such check").0
    }
}

// ----- regressions from review --------------------------------------------------

#[test]
fn double_click_on_copy_does_not_hit_the_conflict_dialog() {
    let mut h = standard();
    select(&mut h, "a.pdf");
    h.press(KeyCode::F(5));
    let (x, y) = h.find("[ Copy ]");
    let t = Instant::now() + Duration::from_secs(5);
    h.mouse_at(MouseEventKind::Down(MouseButton::Left), x + 2, y, KeyModifiers::NONE, t);
    // The target is listed again; the conflict dialog appears 50 ms after the click.
    h.wait_until_at(Some(t + Duration::from_millis(50)), |a| a.dialog.is_some());
    assert!(matches!(h.app.dialog, Some(Dialog::Conflict { .. })));
    // The second click of the double click lands on whatever the new dialog shows there.
    for (bx, by) in h.app.hitmap.buttons.clone().iter().map(|r| (r.x, r.y)) {
        h.mouse_at(
            MouseEventKind::Down(MouseButton::Left),
            bx,
            by,
            KeyModifiers::NONE,
            t + Duration::from_millis(150),
        );
    }
    h.settle();
    assert!(matches!(h.app.dialog, Some(Dialog::Conflict { .. })), "a button fired too early");
    assert_eq!(h.calls("rse/copy").len(), 0);
}

/// The second Enter comes while the target is being listed: nothing is overwritten, and
/// the conflict question waits with Skip as its default.
#[test]
fn double_enter_on_confirm_skips_instead_of_overwriting() {
    let mut h = standard();
    select(&mut h, "a.pdf");
    h.press(KeyCode::F(5));
    let now = Instant::now();
    h.app.handle(Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)), now);
    h.app.handle(Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)), now);
    h.wait_until(|a| a.dialog.is_some());
    assert!(matches!(h.app.dialog, Some(Dialog::Conflict { focus: 1, .. })));
    assert_eq!(h.calls("rse/copy").len(), 0);
    h.press(KeyCode::Enter);
    assert_eq!(h.app.status.as_deref(), Some("nothing to do: all items skipped"));
}

#[test]
fn late_target_check_does_not_replace_an_open_dialog() {
    let (backend, _) = fake_fs(
        &[("/home/sky", vec![file("b.txt", 1)]), ("nas:backup", vec![file("b.txt", 1)])],
        &[("nas:backup", Duration::from_millis(300))],
    );
    let mut h = Harness::new(backend, home(), nas());
    h.wait_until(|a| a.panes[1].status == PaneStatus::Ready);
    h.app.panes[1].status = PaneStatus::Loading; // forces a fresh listing of the target
    select(&mut h, "b.txt");
    let now = Instant::now();
    h.app.handle(Event::Key(KeyEvent::new(KeyCode::F(5), KeyModifiers::NONE)), now);
    h.app.handle(Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)), now);
    h.app.handle(Event::Key(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE)), now);
    std::thread::sleep(Duration::from_millis(400));
    h.settle();
    assert!(matches!(h.app.dialog, Some(Dialog::Help { .. })));
    assert!(h.app.errors.front().is_some_and(|e| e.message.contains("copy not started")));
    assert_eq!(h.calls("rse/copy").len(), 0);
}

#[test]
fn moved_directory_source_is_removed() {
    let mut h = standard();
    select(&mut h, "projects");
    h.press(KeyCode::F(6));
    h.press(KeyCode::Enter);
    h.wait_until(|a| a.queue.jobs.first().is_some_and(|j| j.state == JobState::Done));
    h.settle();
    assert_eq!(h.calls("operations/rmdir"), vec![json!({"fs": "/home/sky", "remote": "projects"})]);
}

#[test]
fn one_listing_per_pane_in_flight() {
    let (backend, _) = fake_fs(
        &[("/home/sky", vec![file("a", 1)]), ("nas:backup", vec![])],
        &[("/home/sky", Duration::from_millis(200))],
    );
    let mut h = Harness::new(backend, home(), nas());
    h.wait_until(|a| a.panes[0].status == PaneStatus::Ready);
    let before = h.calls("operations/list").iter().filter(|c| c["fs"] == "/home/sky").count();
    let now = Instant::now();
    for _ in 0..5 {
        h.app.handle(Event::Key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL)), now);
    }
    h.wait_until(|a| a.panes[0].status == PaneStatus::Ready);
    let after = h.calls("operations/list").iter().filter(|c| c["fs"] == "/home/sky").count();
    // At most one listing runs at a time and the other requests collapse into one
    // follow-up (or, if the first was still queued, into that one).
    assert!((1..=2).contains(&(after - before)), "{} listings", after - before);
}

#[test]
fn drag_follows_the_entry_not_the_row() {
    let mut h = standard();
    let (x, y) = h.find("b.txt");
    let t = Instant::now();
    h.mouse_at(MouseEventKind::Down(MouseButton::Left), x, y, KeyModifiers::NONE, t);
    // A reload adds an entry above b.txt before the pointer moves.
    h.app.panes[0].entries.push(Entry::file("aa.txt", 1));
    h.app.panes[0].rebuild(false);
    h.mouse_at(MouseEventKind::Drag(MouseButton::Left), 50, 12, KeyModifiers::NONE, t);
    let Drag::Dragging { items, .. } = &h.app.drag else { panic!("not dragging") };
    assert_eq!(items.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(), ["b.txt"]);
}

#[test]
fn names_differing_only_in_case_conflict() {
    let (backend, _) = fake_fs(
        &[("/home/sky", vec![file("Report.pdf", 1)]), ("nas:backup", vec![file("report.pdf", 2)])],
        &[],
    );
    let mut h = Harness::new(backend, home(), nas());
    select(&mut h, "Report.pdf");
    h.press(KeyCode::F(5));
    h.press(KeyCode::Enter);
    assert!(
        matches!(&h.app.dialog, Some(Dialog::Conflict { conflicts, .. }) if conflicts[0].name == "report.pdf")
    );
    h.press(KeyCode::Enter); // Skip
    assert_eq!(h.calls("rse/copy").len(), 0);
}

#[test]
fn wheel_in_remote_picker_stops_at_the_last_choice() {
    let mut h = standard();
    h.press(KeyCode::Char('g'));
    for _ in 0..10 {
        h.mouse_at(MouseEventKind::ScrollDown, 40, 12, KeyModifiers::NONE, Instant::now());
    }
    h.press(KeyCode::Enter);
    assert_eq!(h.app.panes[0].loc, Location::new("s3:", ""));
}

// ----- terminal ------------------------------------------------------------------

/// The terminal's screen as text.
fn term_text(h: &Harness) -> String {
    match &h.app.terminal {
        Some(crate::app::Terminal { state: crate::app::TermState::Running(s), .. }) => {
            s.screen().contents()
        }
        _ => String::new(),
    }
}

/// Whether a line of the screen ends with `text`: a command's output, which can share its line
/// with a prompt the shell printed only after the command was typed.
fn ends_a_line(screen: &str, text: &str) -> bool {
    screen.lines().any(|line| line.trim_end().ends_with(text))
}

/// Handles events and ticks until `done` holds of the app (or panics after 10 s).
fn tick_until(h: &mut Harness, what: &str, done: impl Fn(&App) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done(&h.app) {
        assert!(Instant::now() < deadline, "{what}:\n{}", term_text(h));
        h.app.handle(Event::Tick, Instant::now());
        h.settle();
    }
}

fn running(app: &App) -> Option<&crate::terminal::Session> {
    match &app.terminal {
        Some(crate::app::Terminal { state: crate::app::TermState::Running(s), .. }) => Some(s),
        _ => None,
    }
}

fn type_in(h: &mut Harness, text: &str) {
    for c in text.chars() {
        let code = if c == '\r' { KeyCode::Enter } else { KeyCode::Char(c) };
        h.app.handle(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)), Instant::now());
    }
    h.settle();
}

/// A shell where the pane is, which follows the pane once nothing runs in it. sh with a
/// known setup stands in for ssh to a server.
#[test]
fn the_terminal_opens_in_the_pane_directory_and_follows_it() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_string_lossy().into_owned();
    std::fs::create_dir(tmp.path().join("sub dir")).unwrap();
    let (backend, _) = fake_fs(
        &[("srv:", vec![dir("sub dir")]), ("srv:sub dir", vec![]), ("/home/sky", vec![])],
        &[],
    );
    let shell = crate::terminal::ShellTarget {
        open: ["/bin/sh", "-c"].map(str::to_owned).to_vec(),
        run: ["/bin/sh", "-c"].map(str::to_owned).to_vec(),
        root: root.clone(),
        env: [("SHELL", "/bin/sh"), ("HOME", root.as_str()), ("PS1", "$ ")]
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .to_vec(),
    };
    let mut h = Harness::with_shell(backend, Location::new("srv:", ""), home(), shell);

    h.press_mod(KeyCode::Char('o'), KeyModifiers::CONTROL);
    assert_eq!(h.app.focus, Focus::Terminal);
    tick_until(&mut h, "shell shown", |a| running(a).is_some_and(crate::terminal::Session::shown));
    type_in(&mut h, "pwd\r");
    tick_until(&mut h, "in the pane's directory", |a| {
        running(a).is_some_and(|s| ends_a_line(&s.screen().contents(), &root))
    });
    assert!(h.screen.contains("Terminal"), "{}", h.screen);

    // Back to the panes, into `sub dir`: the shell follows.
    h.press_mod(KeyCode::Char('o'), KeyModifiers::CONTROL);
    assert_eq!(h.app.focus, Focus::Panes);
    h.press(KeyCode::Enter);
    let sub = format!("{root}/sub dir");
    tick_until(&mut h, "cd typed", |a| {
        running(a).is_some_and(|s| s.screen().contents().contains("cd '"))
    });
    h.press_mod(KeyCode::Char('o'), KeyModifiers::CONTROL);
    type_in(&mut h, "pwd\r");
    tick_until(&mut h, "in sub dir", |a| {
        running(a).is_some_and(|s| ends_a_line(&s.screen().contents(), &sub))
    });

    // While a command runs, the cd waits for it.
    type_in(&mut h, "sleep 2\r");
    h.press_mod(KeyCode::Char('o'), KeyModifiers::CONTROL);
    h.press(KeyCode::Backspace);
    for _ in 0..3 {
        h.app.handle(Event::Tick, Instant::now());
        h.settle();
    }
    assert!(!term_text(&h).contains(&format!("cd '{root}'")), "{}", term_text(&h));
    tick_until(&mut h, "cd after the command", |a| {
        running(a).is_some_and(|s| s.screen().contents().contains(&format!("cd '{root}'")))
    });

    // exit closes it.
    h.press_mod(KeyCode::Char('o'), KeyModifiers::CONTROL);
    type_in(&mut h, "exit\r");
    tick_until(&mut h, "closed", |a| a.terminal.is_none());
    assert_eq!(h.app.focus, Focus::Panes);
}

/// On the local disk the terminal is the user's own shell, in the pane's directory.
#[test]
fn the_terminal_opens_on_the_local_disk() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().to_string_lossy().into_owned();
    let local = Location::new("/", path.trim_start_matches('/'));
    let (backend, _) = fake_fs(&[(path.as_str(), vec![]), ("nas:backup", vec![])], &[]);
    let mut h = Harness::new(backend, local, nas());
    h.press_mod(KeyCode::Char('o'), KeyModifiers::CONTROL);
    tick_until(&mut h, "shell shown", |a| running(a).is_some_and(crate::terminal::Session::shown));
    type_in(&mut h, "pwd\r");
    tick_until(&mut h, "in the pane's directory", |a| {
        running(a).is_some_and(|s| ends_a_line(&s.screen().contents(), &path))
    });
    // Ctrl+C reaches the command running in it, not rse.
    type_in(&mut h, "sleep 30\r");
    h.press_mod(KeyCode::Char('c'), KeyModifiers::CONTROL);
    assert_eq!(h.app.quit, QuitState::Running);
    type_in(&mut h, "echo stopped\r");
    tick_until(&mut h, "sleep interrupted", |a| {
        running(a).is_some_and(|s| s.screen().contents().contains("\nstopped\n"))
    });
    type_in(&mut h, "exit\r");
    tick_until(&mut h, "closed", |a| a.terminal.is_none());
}

/// `--no-terminal`: no terminal opens, and the server is not even asked how to reach it.
#[test]
fn a_terminal_turned_off_never_opens() {
    let mut h = standard();
    h.app.terminal_allowed = false;
    h.press(KeyCode::Tab);
    h.press_mod(KeyCode::Char('o'), KeyModifiers::CONTROL);
    assert!(h.app.terminal.is_none());
    assert_eq!(h.app.focus, Focus::Panes);
    assert!(h.screen.contains("turned off"), "{}", h.screen);
}
